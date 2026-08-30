use rayon::prelude::*;
use rohditor_raw::{
    ImageRect, LevelPattern, PhotometricInterpretation, RawFileInfo, RawFrame, RawOrientation,
};

use crate::color::{
    CameraColorTransform, LINEAR_REC2020_TO_XYZ_D65, XYZ_D65_TO_LINEAR_SRGB,
    encode_rec2020_for_srgb_output,
};
use crate::image::{allocate_zeroed_f32, allocate_zeroed_u8, allocate_zeroed_u16};
use crate::{
    BayerPattern, CancellationToken, CfaColor, CropPolicy, DemosaicAlgorithm, DisplayRgbImage,
    DisplayTransfer, DitherMode, EditRecipe, ImageRegion, LinearRgbImage, LinearRgbSpace,
    MosaicImage, OrientationMap, OutputPolicy, PipelineError, WhiteBalance,
};

const CONTRAST_PIVOT: f32 = 0.18;
const REC2020_LUMINANCE: [f32; 3] = [0.2627, 0.6780, 0.0593];
const CROSS_OFFSETS: [(isize, isize); 4] = [(-1, 0), (1, 0), (0, -1), (0, 1)];
const DIAGONAL_OFFSETS: [(isize, isize); 4] = [(-1, -1), (1, -1), (-1, 1), (1, 1)];
const HORIZONTAL_OFFSETS: [(isize, isize); 2] = [(-1, 0), (1, 0)];
const VERTICAL_OFFSETS: [(isize, isize); 2] = [(0, -1), (0, 1)];

/// Effective multipliers applied to camera-native R, G, and B samples.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct WhiteBalanceGains {
    pub red: f32,
    pub green: f32,
    pub blue: f32,
}

impl WhiteBalanceGains {
    #[must_use]
    pub const fn identity() -> Self {
        Self {
            red: 1.0,
            green: 1.0,
            blue: 1.0,
        }
    }

    fn for_color(self, color: CfaColor) -> f32 {
        match color {
            CfaColor::Red => self.red,
            CfaColor::Green => self.green,
            CfaColor::Blue => self.blue,
        }
    }

    fn validate(self) -> Result<(), PipelineError> {
        if [self.red, self.green, self.blue]
            .into_iter()
            .all(|value| value.is_finite() && value > 0.0)
        {
            Ok(())
        } else {
            Err(PipelineError::InvalidMetadata {
                field: "as_shot_white_balance",
                reason: "effective R, G, and B gains must be finite and positive".to_owned(),
            })
        }
    }
}

/// Crop the decoded sensor frame and normalize samples as `(sample-black)/(white-black)`.
///
/// Negative values and values above one are intentionally retained for later
/// highlight handling. Level patterns remain indexed in original sensor coordinates.
pub fn normalize_raw(
    frame: &RawFrame,
    crop_policy: CropPolicy,
) -> Result<MosaicImage<f32>, PipelineError> {
    normalize_raw_impl(frame, crop_policy, None, &CancellationToken::new())
}

/// Normalize a resolution-limited Bayer mosaic for interactive development.
///
/// Samples are selected on their original color-filter phase, so reducing the
/// mosaic never turns a red, green, or blue sensor site into a different CFA
/// color. The output preserves the crop aspect ratio and never exceeds
/// `max_long_edge` on its longest side.
pub fn normalize_raw_preview(
    frame: &RawFrame,
    crop_policy: CropPolicy,
    max_long_edge: usize,
) -> Result<MosaicImage<f32>, PipelineError> {
    normalize_raw_impl(
        frame,
        crop_policy,
        Some(max_long_edge),
        &CancellationToken::new(),
    )
}

pub(crate) fn normalize_raw_preview_cancellable(
    frame: &RawFrame,
    crop_policy: CropPolicy,
    max_long_edge: usize,
    cancellation: &CancellationToken,
) -> Result<MosaicImage<f32>, PipelineError> {
    normalize_raw_impl(frame, crop_policy, Some(max_long_edge), cancellation)
}

fn normalize_raw_impl(
    frame: &RawFrame,
    crop_policy: CropPolicy,
    max_long_edge: Option<usize>,
    cancellation: &CancellationToken,
) -> Result<MosaicImage<f32>, PipelineError> {
    cancellation.checkpoint()?;
    validate_raw_layout(frame)?;
    let (pattern, crop) = development_geometry(&frame.info, crop_policy)?;
    validate_levels(&frame.info, pattern)?;

    let (output_width, output_height) = match max_long_edge {
        Some(max_long_edge) => preview_dimensions(crop.width, crop.height, max_long_edge)?,
        None => (crop.width, crop.height),
    };
    let span = tracing::info_span!(
        "cpu.normalize",
        source_width = frame.info.width,
        source_height = frame.info.height,
        output_width,
        output_height,
        preview = max_long_edge.is_some()
    );
    let _guard = span.enter();

    let elements = output_width.checked_mul(output_height).ok_or_else(|| {
        invalid_dimensions(
            output_width,
            output_height,
            output_width,
            "preview crop overflowed",
        )
    })?;
    let mut normalized = allocate_zeroed_f32(elements)?;
    normalized
        .par_chunks_mut(output_width)
        .enumerate()
        .try_for_each(|(output_y, output_row)| -> Result<(), PipelineError> {
            cancellation.checkpoint()?;
            let crop_y = phase_preserving_sample(output_y, output_height, crop.height);
            let sensor_y = crop.y + crop_y;
            for (output_x, destination) in output_row.iter_mut().enumerate() {
                let crop_x = phase_preserving_sample(output_x, output_width, crop.width);
                let sensor_x = crop.x + crop_x;
                let sample = frame.mosaic[sensor_y * frame.row_stride + sensor_x];
                let black_index = level_index(&frame.info.black_levels, sensor_x, sensor_y, 0);
                let black = frame.info.black_levels.values[black_index];
                let color = pattern.color_at(sensor_x, sensor_y);
                let white = white_level(&frame.info, black_index, color);
                *destination = (f32::from(sample) - black) / (white - black);
            }
            Ok(())
        })?;
    cancellation.checkpoint()?;

    MosaicImage::new(
        output_width,
        output_height,
        output_width,
        pattern.shifted(crop.x, crop.y),
        normalized,
    )
}

fn preview_dimensions(
    width: usize,
    height: usize,
    max_long_edge: usize,
) -> Result<(usize, usize), PipelineError> {
    if width < 2 || height < 2 {
        return Err(invalid_dimensions(
            width,
            height,
            width,
            "preview development requires a crop of at least 2x2 pixels",
        ));
    }
    if max_long_edge < 2 {
        return Err(invalid_dimensions(
            width,
            height,
            width,
            "preview long edge must be at least 2 pixels",
        ));
    }
    let long_edge = width.max(height);
    if long_edge <= max_long_edge {
        return Ok((width, height));
    }

    let scale = |dimension: usize| -> Result<usize, PipelineError> {
        let numerator = dimension
            .checked_mul(max_long_edge)
            .and_then(|value| value.checked_add(long_edge / 2))
            .ok_or_else(|| {
                invalid_dimensions(width, height, width, "preview scale calculation overflowed")
            })?;
        Ok((numerator / long_edge).clamp(2, dimension))
    };
    Ok((scale(width)?, scale(height)?))
}

fn phase_preserving_sample(
    output_index: usize,
    output_length: usize,
    source_length: usize,
) -> usize {
    if output_length == source_length {
        return output_index;
    }

    let phase = output_index & 1;
    let output_phase_count = (output_length + (1 - phase)) / 2;
    let source_phase_count = (source_length + (1 - phase)) / 2;
    let output_phase_index = output_index / 2;
    let source_phase_index = if output_phase_count <= 1 {
        0
    } else {
        (output_phase_index * (source_phase_count - 1) + (output_phase_count - 1) / 2)
            / (output_phase_count - 1)
    };
    phase + source_phase_index * 2
}

/// Parse and combine as-shot gains with optional relative manual multipliers.
pub fn white_balance_gains(
    info: &RawFileInfo,
    selection: WhiteBalance,
) -> Result<WhiteBalanceGains, PipelineError> {
    let [red, green, blue, _] = info.as_shot_white_balance;
    let values =
        [red, green, blue].map(|value| value.filter(|number| number.is_finite() && *number > 0.0));
    let [Some(red), Some(green), Some(blue)] = values else {
        return Err(PipelineError::InvalidMetadata {
            field: "as_shot_white_balance",
            reason: "finite positive R, G, and B multipliers are required".to_owned(),
        });
    };
    let mut gains = WhiteBalanceGains {
        red: red / green,
        green: 1.0,
        blue: blue / green,
    };
    if let WhiteBalance::ManualMultipliers { red, green, blue } = selection {
        gains.red *= red;
        gains.green *= green;
        gains.blue *= blue;
    }
    gains.validate()?;
    Ok(gains)
}

/// Bilinearly interpolate a normalized Bayer mosaic into camera-native linear RGB.
pub fn demosaic(
    mosaic: &MosaicImage<f32>,
    gains: WhiteBalanceGains,
    algorithm: DemosaicAlgorithm,
) -> Result<LinearRgbImage<f32>, PipelineError> {
    demosaic_cancellable(mosaic, gains, algorithm, &CancellationToken::new())
}

pub(crate) fn demosaic_cancellable(
    mosaic: &MosaicImage<f32>,
    gains: WhiteBalanceGains,
    algorithm: DemosaicAlgorithm,
    cancellation: &CancellationToken,
) -> Result<LinearRgbImage<f32>, PipelineError> {
    let span = tracing::info_span!(
        "cpu.demosaic",
        width = mosaic.width(),
        height = mosaic.height(),
        algorithm = ?algorithm
    );
    let _guard = span.enter();
    cancellation.checkpoint()?;
    gains.validate()?;
    if mosaic.width() < 2 || mosaic.height() < 2 {
        return Err(invalid_dimensions(
            mosaic.width(),
            mosaic.height(),
            mosaic.row_stride(),
            "bilinear demosaicing requires at least 2x2 samples",
        ));
    }
    match algorithm {
        DemosaicAlgorithm::Bilinear => demosaic_bilinear(mosaic, gains, cancellation),
    }
}

/// Transform a camera-native image into the linear Rec.2020/D65 working space.
pub(crate) fn apply_camera_color_transform(
    image: &mut LinearRgbImage<f32>,
    transform: &CameraColorTransform,
) -> Result<(), PipelineError> {
    apply_camera_color_transform_cancellable(image, transform, &CancellationToken::new())
}

pub(crate) fn apply_camera_color_transform_cancellable(
    image: &mut LinearRgbImage<f32>,
    transform: &CameraColorTransform,
    cancellation: &CancellationToken,
) -> Result<(), PipelineError> {
    let span = tracing::info_span!(
        "cpu.color_conversion",
        width = image.width(),
        height = image.height(),
        target = "linear Rec.2020/D65"
    );
    let _guard = span.enter();
    cancellation.checkpoint()?;
    require_space(image, LinearRgbSpace::CameraNative)?;
    let width_samples = image.width() * 3;
    let row_stride = image.row_stride();
    image.data_mut().par_chunks_mut(row_stride).try_for_each(
        |row| -> Result<(), PipelineError> {
            cancellation.checkpoint()?;
            for pixel in row[..width_samples].chunks_exact_mut(3) {
                let converted = transform
                    .camera_to_linear_rec2020
                    .transform([pixel[0], pixel[1], pixel[2]]);
                pixel.copy_from_slice(&converted);
            }
            Ok(())
        },
    )?;
    cancellation.checkpoint()?;
    image.set_space(LinearRgbSpace::Rec2020D65);
    Ok(())
}

/// Apply global scene-linear adjustments in their documented fixed order.
///
/// Exposure is `2^EV`. Contrast is a linear slope of `2^contrast` around 18%
/// gray. Saturation interpolates from Rec.2020 luminance using its Y coefficients.
pub fn apply_adjustments(
    image: &mut LinearRgbImage<f32>,
    recipe: &EditRecipe,
) -> Result<(), PipelineError> {
    apply_adjustments_cancellable(image, recipe, &CancellationToken::new())
}

pub(crate) fn apply_adjustments_cancellable(
    image: &mut LinearRgbImage<f32>,
    recipe: &EditRecipe,
    cancellation: &CancellationToken,
) -> Result<(), PipelineError> {
    let span = tracing::info_span!(
        "cpu.adjustments",
        width = image.width(),
        height = image.height(),
        exposure_ev = recipe.exposure_ev,
        contrast = recipe.contrast,
        saturation = recipe.saturation
    );
    let _guard = span.enter();
    cancellation.checkpoint()?;
    require_space(image, LinearRgbSpace::Rec2020D65)?;
    recipe.validate()?;
    let exposure_gain = recipe.exposure_ev.exp2();
    let contrast_gain = recipe.contrast.exp2();
    let width_samples = image.width() * 3;
    let row_stride = image.row_stride();
    image.data_mut().par_chunks_mut(row_stride).try_for_each(
        |row| -> Result<(), PipelineError> {
            cancellation.checkpoint()?;
            for pixel in row[..width_samples].chunks_exact_mut(3) {
                if recipe.exposure_ev != 0.0 {
                    for value in pixel.iter_mut() {
                        *value *= exposure_gain;
                    }
                }
                if recipe.contrast != 0.0 {
                    for value in pixel.iter_mut() {
                        *value = CONTRAST_PIVOT + (*value - CONTRAST_PIVOT) * contrast_gain;
                    }
                }
                if recipe.saturation != 1.0 {
                    let luminance = pixel[0] * REC2020_LUMINANCE[0]
                        + pixel[1] * REC2020_LUMINANCE[1]
                        + pixel[2] * REC2020_LUMINANCE[2];
                    for value in pixel.iter_mut() {
                        *value = luminance + recipe.saturation * (*value - luminance);
                    }
                }
            }
            Ok(())
        },
    )?;
    cancellation.checkpoint()?;
    Ok(())
}

/// Convert linear Rec.2020 to clipped, transfer-encoded sRGB8 while physically
/// applying the requested EXIF orientation. Quantization uses nearest code value.
pub fn render_display_srgb8(
    image: &LinearRgbImage<f32>,
    orientation: RawOrientation,
    output_policy: OutputPolicy,
) -> Result<DisplayRgbImage<u8>, PipelineError> {
    render_display_srgb8_dithered(image, orientation, output_policy, DitherMode::None)
}

/// Convert linear Rec.2020 to clipped, transfer-encoded sRGB8 with explicit
/// deterministic output dithering.
pub fn render_display_srgb8_dithered(
    image: &LinearRgbImage<f32>,
    orientation: RawOrientation,
    output_policy: OutputPolicy,
    dithering: DitherMode,
) -> Result<DisplayRgbImage<u8>, PipelineError> {
    render_display_srgb8_dithered_cancellable(
        image,
        orientation,
        output_policy,
        dithering,
        &CancellationToken::new(),
    )
}

pub(crate) fn render_display_srgb8_cancellable(
    image: &LinearRgbImage<f32>,
    orientation: RawOrientation,
    output_policy: OutputPolicy,
    cancellation: &CancellationToken,
) -> Result<DisplayRgbImage<u8>, PipelineError> {
    render_display_srgb8_dithered_cancellable(
        image,
        orientation,
        output_policy,
        DitherMode::None,
        cancellation,
    )
}

fn render_display_srgb8_dithered_cancellable(
    image: &LinearRgbImage<f32>,
    orientation: RawOrientation,
    output_policy: OutputPolicy,
    dithering: DitherMode,
    cancellation: &CancellationToken,
) -> Result<DisplayRgbImage<u8>, PipelineError> {
    let span = tracing::info_span!(
        "cpu.output_conversion",
        width = image.width(),
        height = image.height(),
        bit_depth = 8,
        orientation = %orientation
    );
    let _guard = span.enter();
    cancellation.checkpoint()?;
    require_space(image, LinearRgbSpace::Rec2020D65)?;
    let orientation_map = OrientationMap::new(image.width(), image.height(), orientation)?;
    let (output_width, output_height) = orientation_map.output_dimensions();
    let row_stride = output_width.checked_mul(3).ok_or_else(|| {
        invalid_dimensions(output_width, output_height, 0, "RGB stride overflowed")
    })?;
    let elements = row_stride.checked_mul(output_height).ok_or_else(|| {
        invalid_dimensions(
            output_width,
            output_height,
            row_stride,
            "RGB sample count overflowed",
        )
    })?;
    let mut output = allocate_zeroed_u8(elements)?;
    let rec2020_to_srgb = LINEAR_REC2020_TO_XYZ_D65.then(XYZ_D65_TO_LINEAR_SRGB);
    output.par_chunks_mut(row_stride).enumerate().try_for_each(
        |(output_y, output_row)| -> Result<(), PipelineError> {
            cancellation.checkpoint()?;
            for (output_x, destination) in output_row.chunks_exact_mut(3).enumerate() {
                let (source_x, source_y) =
                    orientation_map.source_coordinate_in_bounds(output_x, output_y);
                let start = source_y * image.row_stride() + source_x * 3;
                let source = &image.data()[start..start + 3];
                let encoded = match output_policy {
                    OutputPolicy::ClipToSrgb => encode_rec2020_for_srgb_output(
                        rec2020_to_srgb,
                        [source[0], source[1], source[2]],
                    ),
                };
                for (encoded, output) in encoded.into_iter().zip(destination) {
                    let dither = quantization_dither(dithering, output_x, output_y);
                    *output = (encoded * 255.0 + dither).round().clamp(0.0, 255.0) as u8;
                }
            }
            Ok(())
        },
    )?;
    cancellation.checkpoint()?;
    DisplayRgbImage::new(
        output_width,
        output_height,
        row_stride,
        DisplayTransfer::Srgb,
        output,
    )
}

/// Convert linear Rec.2020 directly to clipped, transfer-encoded sRGB16 while
/// physically applying the requested EXIF orientation.
pub fn render_display_srgb16(
    image: &LinearRgbImage<f32>,
    orientation: RawOrientation,
    output_policy: OutputPolicy,
    dithering: DitherMode,
) -> Result<DisplayRgbImage<u16>, PipelineError> {
    let span = tracing::info_span!(
        "cpu.output_conversion",
        width = image.width(),
        height = image.height(),
        bit_depth = 16,
        orientation = %orientation
    );
    let _guard = span.enter();
    require_space(image, LinearRgbSpace::Rec2020D65)?;
    let orientation_map = OrientationMap::new(image.width(), image.height(), orientation)?;
    let (output_width, output_height) = orientation_map.output_dimensions();
    let row_stride = output_width.checked_mul(3).ok_or_else(|| {
        invalid_dimensions(output_width, output_height, 0, "RGB stride overflowed")
    })?;
    let elements = row_stride.checked_mul(output_height).ok_or_else(|| {
        invalid_dimensions(
            output_width,
            output_height,
            row_stride,
            "RGB sample count overflowed",
        )
    })?;
    let mut output = allocate_zeroed_u16(elements)?;
    let rec2020_to_srgb = LINEAR_REC2020_TO_XYZ_D65.then(XYZ_D65_TO_LINEAR_SRGB);
    output
        .par_chunks_mut(row_stride)
        .enumerate()
        .for_each(|(output_y, output_row)| {
            for (output_x, destination) in output_row.chunks_exact_mut(3).enumerate() {
                let (source_x, source_y) =
                    orientation_map.source_coordinate_in_bounds(output_x, output_y);
                let start = source_y * image.row_stride() + source_x * 3;
                let source = &image.data()[start..start + 3];
                let encoded = match output_policy {
                    OutputPolicy::ClipToSrgb => encode_rec2020_for_srgb_output(
                        rec2020_to_srgb,
                        [source[0], source[1], source[2]],
                    ),
                };
                for (encoded, output) in encoded.into_iter().zip(destination) {
                    let dither = quantization_dither(dithering, output_x, output_y);
                    *output = (encoded * 65_535.0 + dither).round().clamp(0.0, 65_535.0) as u16;
                }
            }
        });
    DisplayRgbImage::new(
        output_width,
        output_height,
        row_stride,
        DisplayTransfer::Srgb,
        output,
    )
}

fn quantization_dither(mode: DitherMode, x: usize, y: usize) -> f32 {
    const BAYER_8X8: [[u8; 8]; 8] = [
        [0, 32, 8, 40, 2, 34, 10, 42],
        [48, 16, 56, 24, 50, 18, 58, 26],
        [12, 44, 4, 36, 14, 46, 6, 38],
        [60, 28, 52, 20, 62, 30, 54, 22],
        [3, 35, 11, 43, 1, 33, 9, 41],
        [51, 19, 59, 27, 49, 17, 57, 25],
        [15, 47, 7, 39, 13, 45, 5, 37],
        [63, 31, 55, 23, 61, 29, 53, 21],
    ];
    match mode {
        DitherMode::None => 0.0,
        DitherMode::Ordered8x8 => (f32::from(BAYER_8X8[y & 7][x & 7]) + 0.5) / 64.0 - 0.5,
    }
}

fn demosaic_bilinear(
    mosaic: &MosaicImage<f32>,
    gains: WhiteBalanceGains,
    cancellation: &CancellationToken,
) -> Result<LinearRgbImage<f32>, PipelineError> {
    let row_stride = mosaic.width().checked_mul(3).ok_or_else(|| {
        invalid_dimensions(mosaic.width(), mosaic.height(), 0, "RGB stride overflowed")
    })?;
    let elements = row_stride.checked_mul(mosaic.height()).ok_or_else(|| {
        invalid_dimensions(
            mosaic.width(),
            mosaic.height(),
            row_stride,
            "RGB sample count overflowed",
        )
    })?;
    let mut output = allocate_zeroed_f32(elements)?;
    output.par_chunks_mut(row_stride).enumerate().try_for_each(
        |(y, output_row)| -> Result<(), PipelineError> {
            cancellation.checkpoint()?;
            for (x, pixel) in output_row.chunks_exact_mut(3).enumerate() {
                let site = mosaic.pattern().color_at(x, y);
                let mut rgb = match site {
                    CfaColor::Red => [
                        *mosaic.sample(x, y),
                        average_offsets(mosaic, x, y, &CROSS_OFFSETS),
                        average_offsets(mosaic, x, y, &DIAGONAL_OFFSETS),
                    ],
                    CfaColor::Blue => [
                        average_offsets(mosaic, x, y, &DIAGONAL_OFFSETS),
                        average_offsets(mosaic, x, y, &CROSS_OFFSETS),
                        *mosaic.sample(x, y),
                    ],
                    CfaColor::Green => {
                        let red_horizontal =
                            mosaic.pattern().color_at(x.wrapping_add(1), y) == CfaColor::Red;
                        let (red_offsets, blue_offsets) = if red_horizontal {
                            (&HORIZONTAL_OFFSETS[..], &VERTICAL_OFFSETS[..])
                        } else {
                            (&VERTICAL_OFFSETS[..], &HORIZONTAL_OFFSETS[..])
                        };
                        [
                            average_offsets(mosaic, x, y, red_offsets),
                            *mosaic.sample(x, y),
                            average_offsets(mosaic, x, y, blue_offsets),
                        ]
                    }
                };
                for color in [CfaColor::Red, CfaColor::Green, CfaColor::Blue] {
                    rgb[color.channel_index()] *= gains.for_color(color);
                }
                pixel.copy_from_slice(&rgb);
            }
            Ok(())
        },
    )?;
    cancellation.checkpoint()?;
    LinearRgbImage::new(
        mosaic.width(),
        mosaic.height(),
        row_stride,
        LinearRgbSpace::CameraNative,
        output,
    )
}

fn average_offsets(
    mosaic: &MosaicImage<f32>,
    x: usize,
    y: usize,
    offsets: &[(isize, isize)],
) -> f32 {
    let mut sum = 0.0;
    let mut count = 0_u8;
    for &(offset_x, offset_y) in offsets {
        let Some(neighbor_x) = x.checked_add_signed(offset_x) else {
            continue;
        };
        let Some(neighbor_y) = y.checked_add_signed(offset_y) else {
            continue;
        };
        if neighbor_x < mosaic.width() && neighbor_y < mosaic.height() {
            sum += *mosaic.sample(neighbor_x, neighbor_y);
            count += 1;
        }
    }
    if count == 0 {
        *mosaic.sample(x, y)
    } else {
        sum / f32::from(count)
    }
}

fn validate_raw_layout(frame: &RawFrame) -> Result<(), PipelineError> {
    if frame.info.components_per_pixel != 1 {
        return Err(PipelineError::InvalidMetadata {
            field: "components_per_pixel",
            reason: format!(
                "Bayer normalization requires one component, received {}",
                frame.info.components_per_pixel
            ),
        });
    }
    if frame.row_stride < frame.info.width {
        return Err(invalid_dimensions(
            frame.info.width,
            frame.info.height,
            frame.row_stride,
            "decoded row stride is shorter than the sensor width",
        ));
    }
    let expected = frame
        .row_stride
        .checked_mul(frame.info.height)
        .ok_or_else(|| {
            invalid_dimensions(
                frame.info.width,
                frame.info.height,
                frame.row_stride,
                "decoded sample count overflowed",
            )
        })?;
    if frame.mosaic.len() != expected {
        return Err(invalid_dimensions(
            frame.info.width,
            frame.info.height,
            frame.row_stride,
            &format!(
                "decoded buffer has {} samples, expected {expected}",
                frame.mosaic.len()
            ),
        ));
    }
    Ok(())
}

fn development_geometry(
    info: &RawFileInfo,
    policy: CropPolicy,
) -> Result<(BayerPattern, ImageRegion), PipelineError> {
    let pattern = match &info.photometric_interpretation {
        PhotometricInterpretation::Cfa { pattern } => {
            BayerPattern::parse(&pattern.name, pattern.width, pattern.height)?
        }
        other => {
            return Err(PipelineError::InvalidMetadata {
                field: "photometric_interpretation",
                reason: format!("CPU Bayer pipeline cannot process {other:?}"),
            });
        }
    };
    let full = ImageRegion {
        x: 0,
        y: 0,
        width: info.width,
        height: info.height,
    };
    validate_region(full, full, "sensor dimensions")?;
    let active = info.active_area.map_or(full, image_region);
    validate_region(active, full, "active_area")?;
    let crop = match policy {
        CropPolicy::ActiveArea => active,
        CropPolicy::Recommended => info.crop_area.map_or(active, image_region),
    };
    validate_region(crop, active, "crop_area")?;
    Ok((pattern, crop))
}

fn validate_levels(info: &RawFileInfo, pattern: BayerPattern) -> Result<(), PipelineError> {
    let levels = &info.black_levels;
    if levels.repeat_width == 0 || levels.repeat_height == 0 || levels.components_per_pixel != 1 {
        return Err(PipelineError::InvalidMetadata {
            field: "black_levels",
            reason: "a non-empty one-component repeat pattern is required".to_owned(),
        });
    }
    let expected = levels
        .repeat_width
        .checked_mul(levels.repeat_height)
        .and_then(|count| count.checked_mul(levels.components_per_pixel))
        .ok_or_else(|| PipelineError::InvalidMetadata {
            field: "black_levels",
            reason: "repeat-pattern dimensions overflowed".to_owned(),
        })?;
    if levels.values.len() != expected || levels.values.iter().any(|value| !value.is_finite()) {
        return Err(PipelineError::InvalidMetadata {
            field: "black_levels",
            reason: format!(
                "expected {expected} finite values, found {}",
                levels.values.len()
            ),
        });
    }
    let white_count = info.white_levels.len();
    if !matches!(white_count, 1 | 3) && white_count != expected {
        return Err(PipelineError::InvalidMetadata {
            field: "white_levels",
            reason: format!("expected 1, 3, or {expected} values, found {white_count}"),
        });
    }
    if info.white_levels.iter().any(|value| !value.is_finite()) {
        return Err(PipelineError::InvalidMetadata {
            field: "white_levels",
            reason: "all white levels must be finite".to_owned(),
        });
    }
    for y in 0..levels.repeat_height {
        for x in 0..levels.repeat_width {
            let index = level_index(levels, x, y, 0);
            let black = levels.values[index];
            let white = white_level(info, index, pattern.color_at(x, y));
            if white <= black {
                return Err(PipelineError::InvalidMetadata {
                    field: "white_levels",
                    reason: format!("white level {white} must exceed black level {black}"),
                });
            }
        }
    }
    Ok(())
}

fn level_index(levels: &LevelPattern, x: usize, y: usize, component: usize) -> usize {
    ((y % levels.repeat_height) * levels.repeat_width + (x % levels.repeat_width))
        * levels.components_per_pixel
        + component
}

fn white_level(info: &RawFileInfo, black_index: usize, color: CfaColor) -> f32 {
    match info.white_levels.as_slice() {
        [global] => *global,
        [red, green, blue] => [*red, *green, *blue][color.channel_index()],
        values => values[black_index],
    }
}

fn image_region(rect: ImageRect) -> ImageRegion {
    ImageRegion {
        x: rect.x,
        y: rect.y,
        width: rect.width,
        height: rect.height,
    }
}

fn validate_region(
    region: ImageRegion,
    bounds: ImageRegion,
    field: &'static str,
) -> Result<(), PipelineError> {
    let region_end_x = region.x.checked_add(region.width);
    let region_end_y = region.y.checked_add(region.height);
    let bounds_end_x = bounds.x.checked_add(bounds.width);
    let bounds_end_y = bounds.y.checked_add(bounds.height);
    let valid = region.width > 0
        && region.height > 0
        && region.x >= bounds.x
        && region.y >= bounds.y
        && region_end_x.is_some_and(|end| bounds_end_x.is_some_and(|bound| end <= bound))
        && region_end_y.is_some_and(|end| bounds_end_y.is_some_and(|bound| end <= bound));
    if valid {
        Ok(())
    } else {
        Err(PipelineError::InvalidMetadata {
            field,
            reason: format!("region {region:?} is outside {bounds:?}"),
        })
    }
}

fn require_space(
    image: &LinearRgbImage<f32>,
    expected: LinearRgbSpace,
) -> Result<(), PipelineError> {
    if image.space() == expected {
        Ok(())
    } else {
        Err(PipelineError::WrongImageState {
            expected: expected.description(),
            actual: image.space().description(),
        })
    }
}

fn invalid_dimensions(
    width: usize,
    height: usize,
    row_stride: usize,
    reason: &str,
) -> PipelineError {
    PipelineError::InvalidDimensions {
        width,
        height,
        row_stride,
        reason: reason.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use rohditor_raw::{CameraColorMatrix, CaptureMetadata, CfaPattern, LevelPattern, RawFileInfo};

    use super::*;

    fn test_info(width: usize, height: usize, pattern: &str) -> RawFileInfo {
        RawFileInfo {
            format: "synthetic".to_owned(),
            make: "Rohditor".to_owned(),
            model: "Fixture".to_owned(),
            clean_make: "Rohditor".to_owned(),
            clean_model: "Fixture".to_owned(),
            source_size_bytes: 0,
            source_identity: None,
            width,
            height,
            components_per_pixel: 1,
            source_bits_per_sample: Some(16),
            decoded_bits_per_sample: 16,
            compression: None,
            active_area: Some(ImageRect {
                x: 0,
                y: 0,
                width,
                height,
            }),
            crop_area: None,
            photometric_interpretation: PhotometricInterpretation::Cfa {
                pattern: CfaPattern {
                    name: pattern.to_owned(),
                    width: 2,
                    height: 2,
                },
            },
            black_levels: LevelPattern {
                values: vec![0.0],
                repeat_width: 1,
                repeat_height: 1,
                components_per_pixel: 1,
            },
            white_levels: vec![100.0],
            as_shot_white_balance: [Some(1.0), Some(1.0), Some(1.0), None],
            xyz_to_camera: [[0.0; 3]; 4],
            color_matrices: vec![CameraColorMatrix {
                illuminant: "D65".to_owned(),
                values: vec![1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0],
            }],
            orientation: RawOrientation::Normal,
            capture: CaptureMetadata::default(),
            embedded_preview: None,
        }
    }

    #[test]
    fn normalization_uses_sensor_phase_levels_and_preserves_highlights() {
        let mut info = test_info(4, 4, "RGGB");
        info.crop_area = Some(ImageRect {
            x: 1,
            y: 1,
            width: 2,
            height: 2,
        });
        info.black_levels = LevelPattern {
            values: vec![0.0, 10.0, 20.0, 30.0],
            repeat_width: 2,
            repeat_height: 2,
            components_per_pixel: 1,
        };
        info.white_levels = vec![100.0, 110.0, 120.0, 130.0];
        let mut samples = vec![0_u16; 16];
        for y in 0..4 {
            for x in 0..4 {
                let index = (y & 1) * 2 + (x & 1);
                samples[y * 4 + x] = [50, 60, 70, 80][index];
            }
        }
        samples[5] = 160;
        let frame = RawFrame {
            info,
            row_stride: 4,
            mosaic: Arc::from(samples),
        };

        let normalized = normalize_raw(&frame, CropPolicy::Recommended).expect("valid fixture");
        assert_eq!((normalized.width(), normalized.height()), (2, 2));
        assert_eq!(normalized.pattern(), BayerPattern::Bggr);
        assert_eq!(normalized.get(0, 0), Some(&1.3));
        assert_eq!(normalized.get(1, 0), Some(&0.5));
        assert_eq!(normalized.get(0, 1), Some(&0.5));
        assert_eq!(normalized.get(1, 1), Some(&0.5));

        let active = normalize_raw(&frame, CropPolicy::ActiveArea).expect("valid active area");
        assert_eq!((active.width(), active.height()), (4, 4));
        assert_eq!(active.pattern(), BayerPattern::Rggb);
    }

    #[test]
    fn normalization_supports_per_color_white_levels() {
        let mut info = test_info(2, 2, "RGGB");
        info.black_levels.values = vec![10.0];
        info.white_levels = vec![110.0, 210.0, 410.0];
        let frame = RawFrame {
            info,
            row_stride: 2,
            mosaic: Arc::from(vec![60, 110, 110, 210]),
        };
        let normalized = normalize_raw(&frame, CropPolicy::ActiveArea).expect("valid levels");
        assert_eq!(normalized.data(), [0.5; 4]);
    }

    #[test]
    fn preview_normalization_limits_long_edge_and_preserves_cfa_phase() {
        let width = 12;
        let height = 8;
        let info = test_info(width, height, "RGGB");
        let pattern = BayerPattern::Rggb;
        let samples = (0..height)
            .flat_map(|y| {
                (0..width).map(move |x| match pattern.color_at(x, y) {
                    CfaColor::Red => 20,
                    CfaColor::Green => 40,
                    CfaColor::Blue => 80,
                })
            })
            .collect::<Vec<_>>();
        let frame = RawFrame {
            info,
            row_stride: width,
            mosaic: Arc::from(samples),
        };

        let preview =
            normalize_raw_preview(&frame, CropPolicy::ActiveArea, 6).expect("valid scaled preview");
        assert_eq!((preview.width(), preview.height()), (6, 4));
        assert_eq!(preview.pattern(), pattern);
        for y in 0..preview.height() {
            for x in 0..preview.width() {
                let expected = match pattern.color_at(x, y) {
                    CfaColor::Red => 0.2,
                    CfaColor::Green => 0.4,
                    CfaColor::Blue => 0.8,
                };
                assert_eq!(preview.get(x, y), Some(&expected));
            }
        }
    }

    #[test]
    fn preview_normalization_rejects_an_unusable_target() {
        let frame = RawFrame {
            info: test_info(4, 4, "RGGB"),
            row_stride: 4,
            mosaic: Arc::from(vec![50; 16]),
        };

        let error = normalize_raw_preview(&frame, CropPolicy::ActiveArea, 1)
            .expect_err("one-pixel preview target must fail");
        assert!(error.to_string().contains("at least 2 pixels"));
    }

    #[test]
    fn preview_normalization_rejects_a_one_pixel_crop_without_panicking() {
        let frame = RawFrame {
            info: test_info(1, 4, "RGGB"),
            row_stride: 1,
            mosaic: Arc::from(vec![50; 4]),
        };

        let error = normalize_raw_preview(&frame, CropPolicy::ActiveArea, 2_560)
            .expect_err("one-pixel crop must fail");
        assert!(error.to_string().contains("at least 2x2"));
    }

    #[test]
    fn bilinear_demosaic_reconstructs_constant_rgb_at_every_border() {
        for pattern in [
            BayerPattern::Rggb,
            BayerPattern::Bggr,
            BayerPattern::Grbg,
            BayerPattern::Gbrg,
        ] {
            let data = (0..16)
                .map(|index| {
                    let x = index % 4;
                    let y = index / 4;
                    match pattern.color_at(x, y) {
                        CfaColor::Red => 0.2,
                        CfaColor::Green => 0.4,
                        CfaColor::Blue => 0.8,
                    }
                })
                .collect();
            let mosaic = MosaicImage::new(4, 4, 4, pattern, data).expect("valid fixture");
            let rgb = demosaic(
                &mosaic,
                WhiteBalanceGains::identity(),
                DemosaicAlgorithm::Bilinear,
            )
            .expect("demosaic succeeds");
            for pixel in rgb.data().chunks_exact(3) {
                assert_eq!(pixel, [0.2, 0.4, 0.8]);
            }
        }
    }

    #[test]
    fn demosaic_applies_white_balance_without_clipping() {
        let mosaic =
            MosaicImage::new(2, 2, 2, BayerPattern::Rggb, vec![0.5; 4]).expect("valid fixture");
        let rgb = demosaic(
            &mosaic,
            WhiteBalanceGains {
                red: 3.0,
                green: 1.0,
                blue: 2.0,
            },
            DemosaicAlgorithm::Bilinear,
        )
        .expect("demosaic succeeds");
        assert_eq!(rgb.pixel(0, 0), Some(&[1.5, 0.5, 1.0][..]));
    }

    #[test]
    fn manual_white_balance_is_relative_to_as_shot() {
        let info = test_info(2, 2, "RGGB");
        let gains = white_balance_gains(
            &info,
            WhiteBalance::ManualMultipliers {
                red: 0.5,
                green: 1.0,
                blue: 2.0,
            },
        )
        .expect("valid white balance");
        assert_eq!(
            gains,
            WhiteBalanceGains {
                red: 0.5,
                green: 1.0,
                blue: 2.0,
            }
        );
    }

    #[test]
    fn exposure_neutral_is_identity_and_known_ev_is_power_of_two() {
        let source = vec![0.1, 0.2, 0.3, 0.5, 0.6, 0.7];
        let mut neutral = LinearRgbImage::new(2, 1, 6, LinearRgbSpace::Rec2020D65, source.clone())
            .expect("valid image");
        apply_adjustments(&mut neutral, &EditRecipe::default()).expect("neutral adjustment");
        assert_eq!(neutral.data(), source);

        let mut raised = neutral;
        let recipe = EditRecipe {
            exposure_ev: 2.0,
            ..EditRecipe::default()
        };
        apply_adjustments(&mut raised, &recipe).expect("exposure adjustment");
        for (actual, original) in raised.data().iter().zip(source) {
            assert_eq!(*actual, original * 4.0);
        }
    }

    #[test]
    fn contrast_pivot_and_saturation_grayscale_are_stable() {
        let mut image = LinearRgbImage::new(
            2,
            1,
            6,
            LinearRgbSpace::Rec2020D65,
            vec![CONTRAST_PIVOT; 3]
                .into_iter()
                .chain([0.4; 3])
                .collect(),
        )
        .expect("valid image");
        let recipe = EditRecipe {
            contrast: 1.0,
            saturation: 2.0,
            ..EditRecipe::default()
        };
        apply_adjustments(&mut image, &recipe).expect("valid adjustments");
        assert_eq!(image.pixel(0, 0), Some(&[CONTRAST_PIVOT; 3][..]));
        let gray = image.pixel(1, 0).expect("second pixel");
        assert!((gray[0] - gray[1]).abs() < 1.0e-6);
        assert!((gray[1] - gray[2]).abs() < 1.0e-6);
    }

    #[test]
    fn rotate_ninety_maps_pixels_and_swaps_dimensions() {
        let values = [0.0, 0.1, 0.2, 0.3, 0.4, 0.5];
        let data = values.into_iter().flat_map(|value| [value; 3]).collect();
        let image =
            LinearRgbImage::new(2, 3, 6, LinearRgbSpace::Rec2020D65, data).expect("valid image");
        let normal = render_display_srgb8(&image, RawOrientation::Normal, OutputPolicy::ClipToSrgb)
            .expect("normal output");
        let rotated =
            render_display_srgb8(&image, RawOrientation::Rotate90, OutputPolicy::ClipToSrgb)
                .expect("rotated output");
        assert_eq!((rotated.width(), rotated.height()), (3, 2));
        assert_eq!(rotated.pixel(0, 0), normal.pixel(0, 2));
        assert_eq!(rotated.pixel(2, 0), normal.pixel(0, 0));
        assert_eq!(rotated.pixel(0, 1), normal.pixel(1, 2));
    }

    #[test]
    fn sixteen_bit_output_is_quantized_directly_from_float() {
        let width = 1_024;
        let data = (0..width)
            .flat_map(|index| {
                let value = index as f32 / (width - 1) as f32;
                [value; 3]
            })
            .collect();
        let image = LinearRgbImage::new(width, 1, width * 3, LinearRgbSpace::Rec2020D65, data)
            .expect("valid gradient");
        let eight = render_display_srgb8(&image, RawOrientation::Normal, OutputPolicy::ClipToSrgb)
            .expect("8-bit output");
        let sixteen = render_display_srgb16(
            &image,
            RawOrientation::Normal,
            OutputPolicy::ClipToSrgb,
            DitherMode::None,
        )
        .expect("16-bit output");

        assert!(
            sixteen
                .data()
                .iter()
                .zip(eight.data())
                .any(|(wide, narrow)| *wide != u16::from(*narrow) * 257)
        );
        let distinct = sixteen
            .data()
            .iter()
            .step_by(3)
            .copied()
            .collect::<std::collections::BTreeSet<_>>();
        assert!(distinct.len() > 256);
    }

    #[test]
    fn ordered_dithering_is_optional_and_deterministic() {
        let width = 64;
        let height = 64;
        let data = (0..width * height)
            .flat_map(|index| {
                let encoded = ((index % 31) as f32 + 0.5) / 255.0;
                let linear = crate::srgb_to_linear_srgb(encoded);
                [linear; 3]
            })
            .collect();
        let image = LinearRgbImage::new(width, height, width * 3, LinearRgbSpace::Rec2020D65, data)
            .expect("valid dither fixture");
        let without = render_display_srgb8_dithered(
            &image,
            RawOrientation::Normal,
            OutputPolicy::ClipToSrgb,
            DitherMode::None,
        )
        .expect("undithered output");
        let with = render_display_srgb8_dithered(
            &image,
            RawOrientation::Normal,
            OutputPolicy::ClipToSrgb,
            DitherMode::Ordered8x8,
        )
        .expect("dithered output");
        let repeated = render_display_srgb8_dithered(
            &image,
            RawOrientation::Normal,
            OutputPolicy::ClipToSrgb,
            DitherMode::Ordered8x8,
        )
        .expect("repeated dithered output");

        assert_ne!(with.data(), without.data());
        assert_eq!(with.data(), repeated.data());
        assert!(
            with.data()
                .iter()
                .zip(without.data())
                .all(|(dithered, plain)| dithered.abs_diff(*plain) <= 1)
        );
    }

    #[test]
    fn every_exif_orientation_has_the_expected_coordinate_map() {
        type OrientationCase = (RawOrientation, (usize, usize), &'static [(usize, usize)]);
        let cases: [OrientationCase; 8] = [
            (
                RawOrientation::Normal,
                (2, 3),
                &[(0, 0), (1, 0), (0, 1), (1, 1), (0, 2), (1, 2)],
            ),
            (
                RawOrientation::HorizontalFlip,
                (2, 3),
                &[(1, 0), (0, 0), (1, 1), (0, 1), (1, 2), (0, 2)],
            ),
            (
                RawOrientation::Rotate180,
                (2, 3),
                &[(1, 2), (0, 2), (1, 1), (0, 1), (1, 0), (0, 0)],
            ),
            (
                RawOrientation::VerticalFlip,
                (2, 3),
                &[(0, 2), (1, 2), (0, 1), (1, 1), (0, 0), (1, 0)],
            ),
            (
                RawOrientation::Transpose,
                (3, 2),
                &[(0, 0), (0, 1), (0, 2), (1, 0), (1, 1), (1, 2)],
            ),
            (
                RawOrientation::Rotate90,
                (3, 2),
                &[(0, 2), (0, 1), (0, 0), (1, 2), (1, 1), (1, 0)],
            ),
            (
                RawOrientation::Transverse,
                (3, 2),
                &[(1, 2), (1, 1), (1, 0), (0, 2), (0, 1), (0, 0)],
            ),
            (
                RawOrientation::Rotate270,
                (3, 2),
                &[(1, 0), (1, 1), (1, 2), (0, 0), (0, 1), (0, 2)],
            ),
        ];

        for (orientation, dimensions, expected) in cases {
            let map = OrientationMap::new(2, 3, orientation).expect("valid orientation map");
            assert_eq!(map.output_dimensions(), dimensions);
            let actual = (0..dimensions.1)
                .flat_map(|y| {
                    (0..dimensions.0)
                        .map(move |x| map.source_coordinate(x, y).expect("in-range coordinate"))
                })
                .collect::<Vec<_>>();
            assert_eq!(actual, expected, "{orientation:?}");
        }
    }
}
