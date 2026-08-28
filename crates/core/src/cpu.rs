use rayon::prelude::*;
use rohditor_raw::{
    ImageRect, LevelPattern, PhotometricInterpretation, RawFileInfo, RawFrame, RawOrientation,
};

use crate::color::{
    CameraColorTransform, LINEAR_REC2020_TO_XYZ_D65, XYZ_D65_TO_LINEAR_SRGB,
    clip_linear_srgb_for_output, linear_srgb_to_srgb,
};
use crate::image::{allocate_zeroed_f32, allocate_zeroed_u8};
use crate::{
    BayerPattern, CfaColor, CropPolicy, DemosaicAlgorithm, DisplayRgbImage, DisplayTransfer,
    EditRecipe, ImageRegion, LinearRgbImage, LinearRgbSpace, MosaicImage, OutputPolicy,
    PipelineError, WhiteBalance,
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
    validate_raw_layout(frame)?;
    let (pattern, crop) = development_geometry(&frame.info, crop_policy)?;
    validate_levels(&frame.info, pattern)?;

    let elements = crop.width.checked_mul(crop.height).ok_or_else(|| {
        invalid_dimensions(crop.width, crop.height, crop.width, "crop overflowed")
    })?;
    let mut normalized = allocate_zeroed_f32(elements)?;
    normalized
        .par_chunks_mut(crop.width)
        .enumerate()
        .for_each(|(output_y, output_row)| {
            let sensor_y = crop.y + output_y;
            let source_start = sensor_y * frame.row_stride + crop.x;
            let source_row = &frame.mosaic[source_start..source_start + crop.width];
            for (output_x, (sample, destination)) in
                source_row.iter().zip(output_row.iter_mut()).enumerate()
            {
                let sensor_x = crop.x + output_x;
                let black_index = level_index(&frame.info.black_levels, sensor_x, sensor_y, 0);
                let black = frame.info.black_levels.values[black_index];
                let color = pattern.color_at(sensor_x, sensor_y);
                let white = white_level(&frame.info, black_index, color);
                *destination = (f32::from(*sample) - black) / (white - black);
            }
        });

    MosaicImage::new(
        crop.width,
        crop.height,
        crop.width,
        pattern.shifted(crop.x, crop.y),
        normalized,
    )
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
        DemosaicAlgorithm::Bilinear => demosaic_bilinear(mosaic, gains),
    }
}

/// Transform a camera-native image into the linear Rec.2020/D65 working space.
pub(crate) fn apply_camera_color_transform(
    image: &mut LinearRgbImage<f32>,
    transform: &CameraColorTransform,
) -> Result<(), PipelineError> {
    require_space(image, LinearRgbSpace::CameraNative)?;
    let width_samples = image.width() * 3;
    let row_stride = image.row_stride();
    image.data_mut().par_chunks_mut(row_stride).for_each(|row| {
        for pixel in row[..width_samples].chunks_exact_mut(3) {
            let converted = transform
                .camera_to_linear_rec2020
                .transform([pixel[0], pixel[1], pixel[2]]);
            pixel.copy_from_slice(&converted);
        }
    });
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
    require_space(image, LinearRgbSpace::Rec2020D65)?;
    recipe.validate()?;
    let exposure_gain = recipe.exposure_ev.exp2();
    let contrast_gain = recipe.contrast.exp2();
    let width_samples = image.width() * 3;
    let row_stride = image.row_stride();
    image.data_mut().par_chunks_mut(row_stride).for_each(|row| {
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
    });
    Ok(())
}

/// Convert linear Rec.2020 to clipped, transfer-encoded sRGB8 while physically
/// applying the requested EXIF orientation. Quantization uses nearest code value.
pub fn render_display_srgb8(
    image: &LinearRgbImage<f32>,
    orientation: RawOrientation,
    output_policy: OutputPolicy,
) -> Result<DisplayRgbImage<u8>, PipelineError> {
    require_space(image, LinearRgbSpace::Rec2020D65)?;
    let (output_width, output_height) =
        oriented_dimensions(image.width(), image.height(), orientation);
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
    output
        .par_chunks_mut(row_stride)
        .enumerate()
        .for_each(|(output_y, output_row)| {
            for (output_x, destination) in output_row.chunks_exact_mut(3).enumerate() {
                let (source_x, source_y) = source_coordinate(
                    output_x,
                    output_y,
                    image.width(),
                    image.height(),
                    orientation,
                );
                let start = source_y * image.row_stride() + source_x * 3;
                let source = &image.data()[start..start + 3];
                let linear_srgb = rec2020_to_srgb.transform([source[0], source[1], source[2]]);
                let clipped = match output_policy {
                    OutputPolicy::ClipToSrgb => clip_linear_srgb_for_output(linear_srgb),
                };
                for (value, output) in clipped.into_iter().zip(destination) {
                    let encoded = linear_srgb_to_srgb(value);
                    *output = (encoded * 255.0).round().clamp(0.0, 255.0) as u8;
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

fn demosaic_bilinear(
    mosaic: &MosaicImage<f32>,
    gains: WhiteBalanceGains,
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
    output
        .par_chunks_mut(row_stride)
        .enumerate()
        .for_each(|(y, output_row)| {
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
        });
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

fn oriented_dimensions(width: usize, height: usize, orientation: RawOrientation) -> (usize, usize) {
    match orientation {
        RawOrientation::Transpose
        | RawOrientation::Rotate90
        | RawOrientation::Transverse
        | RawOrientation::Rotate270 => (height, width),
        _ => (width, height),
    }
}

fn source_coordinate(
    output_x: usize,
    output_y: usize,
    source_width: usize,
    source_height: usize,
    orientation: RawOrientation,
) -> (usize, usize) {
    match orientation {
        RawOrientation::Normal | RawOrientation::Unknown => (output_x, output_y),
        RawOrientation::HorizontalFlip => (source_width - 1 - output_x, output_y),
        RawOrientation::Rotate180 => (source_width - 1 - output_x, source_height - 1 - output_y),
        RawOrientation::VerticalFlip => (output_x, source_height - 1 - output_y),
        RawOrientation::Transpose => (output_y, output_x),
        RawOrientation::Rotate90 => (output_y, source_height - 1 - output_x),
        RawOrientation::Transverse => (source_width - 1 - output_y, source_height - 1 - output_x),
        RawOrientation::Rotate270 => (source_width - 1 - output_y, output_x),
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
            assert_eq!(oriented_dimensions(2, 3, orientation), dimensions);
            let actual = (0..dimensions.1)
                .flat_map(|y| {
                    (0..dimensions.0).map(move |x| source_coordinate(x, y, 2, 3, orientation))
                })
                .collect::<Vec<_>>();
            assert_eq!(actual, expected, "{orientation:?}");
        }
    }
}
