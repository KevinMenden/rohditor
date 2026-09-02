use rayon::prelude::*;
use rohditor_raw::{
    ImageRect, LevelPattern, PhotometricInterpretation, RawFileInfo, RawFrame, RawOrientation,
};

use crate::color::{
    CameraColorTransform, LINEAR_REC2020_TO_XYZ_D65, XYZ_D65_TO_LINEAR_SRGB,
    camera_color_transform, encode_rec2020_for_srgb_output,
};
use crate::{
    BayerPattern, CancellationToken, CfaColor, CropPolicy, DisplayRgbImage, DisplayTransfer,
    DitherMode, EditRecipe, ImageRegion, LightToneLut, LinearRgbImage, LinearRgbSpace, MosaicImage,
    OrientationMap, OutputPolicy, PipelineError, TEMPERATURE_RANGE, TINT_RANGE,
    WHITE_BALANCE_MULTIPLIER_RANGE, WhiteBalance, WhiteBalanceGains,
};
use rohditor_image::{allocate_zeroed_f32, allocate_zeroed_u8, allocate_zeroed_u16};

const REC2020_LUMINANCE: [f32; 3] = [0.2627, 0.6780, 0.0593];
/// Conventional Red, Orange, Yellow, Green, Aqua, Blue, Purple, and Magenta
/// centers on a normalized hue circle. The unequal spacing is intentional:
/// it matches the named color slices users see in mainstream RAW editors.
pub const HSL_CHANNEL_CENTERS: [f32; crate::HSL_CHANNEL_COUNT] = [
    0.0,
    30.0 / 360.0,
    60.0 / 360.0,
    120.0 / 360.0,
    180.0 / 360.0,
    240.0 / 360.0,
    270.0 / 360.0,
    300.0 / 360.0,
];
// Blend into additive luminance changes near black instead of allowing a
// ratio to magnify tiny numerical differences. The continuous transition also
// keeps half-float source quantization from changing the visible result.
const LUMINANCE_RATIO_TRANSITION: f32 = 0.02;
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

pub(crate) fn normalize_raw_cancellable(
    frame: &RawFrame,
    crop_policy: CropPolicy,
    cancellation: &CancellationToken,
) -> Result<MosaicImage<f32>, PipelineError> {
    normalize_raw_impl(frame, crop_policy, None, cancellation)
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
    .map_err(Into::into)
}

pub(crate) fn preview_dimensions(
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
    let camera_to_xyz_d65 = if matches!(selection, WhiteBalance::TemperatureTint { .. }) {
        camera_color_transform(info)?.camera_to_xyz_d65
    } else {
        // As-shot and manual relative multipliers do not require a camera
        // matrix. Keep this public helper useful for demosaic-only callers;
        // full pipeline entry points validate the transform separately.
        crate::Matrix3::identity()
    };
    white_balance_gains_from_calibration(info.as_shot_white_balance, camera_to_xyz_d65, selection)
}

/// Resolve white balance using as-shot gains and the selected camera
/// calibration. The compact calibration form is suitable for preview/GPU
/// boundaries that need to evaluate many recipes without retaining RAW
/// metadata or rebuilding image pixels.
pub fn white_balance_gains_from_calibration(
    as_shot_white_balance: [Option<f32>; 4],
    camera_to_xyz_d65: crate::Matrix3,
    selection: WhiteBalance,
) -> Result<WhiteBalanceGains, PipelineError> {
    validate_white_balance_selection(selection)?;
    let [red, green, blue, _] = as_shot_white_balance;
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
    match selection {
        WhiteBalance::AsShot => {}
        WhiteBalance::ManualMultipliers { red, green, blue } => {
            gains.red *= red;
            gains.green *= green;
            gains.blue *= blue;
        }
        WhiteBalance::TemperatureTint { temperature, tint } => {
            let [red, green, blue] = temperature_tint_gains(camera_to_xyz_d65, temperature, tint)?;
            gains.red *= red;
            gains.green *= green;
            gains.blue *= blue;
        }
    }
    gains.validate()?;
    Ok(gains)
}

fn validate_white_balance_selection(selection: WhiteBalance) -> Result<(), PipelineError> {
    let valid = match selection {
        WhiteBalance::AsShot => true,
        WhiteBalance::ManualMultipliers { red, green, blue } => [red, green, blue]
            .into_iter()
            .all(|value| WHITE_BALANCE_MULTIPLIER_RANGE.contains(value)),
        WhiteBalance::TemperatureTint { temperature, tint } => {
            TEMPERATURE_RANGE.contains(temperature) && TINT_RANGE.contains(tint)
        }
    };
    if valid {
        Ok(())
    } else {
        Err(PipelineError::InvalidRecipe {
            field: "color.white_balance",
            reason: "white-balance values are outside their declared finite ranges".to_owned(),
        })
    }
}

pub(crate) fn white_balance_gains_with_transform(
    info: &RawFileInfo,
    transform: &CameraColorTransform,
    selection: WhiteBalance,
) -> Result<WhiteBalanceGains, PipelineError> {
    white_balance_gains_from_calibration(
        info.as_shot_white_balance,
        transform.camera_to_xyz_d65,
        selection,
    )
}

fn temperature_tint_gains(
    camera_to_xyz_d65: crate::Matrix3,
    temperature: f32,
    tint: f32,
) -> Result<[f32; 3], PipelineError> {
    // Estimate the requested white point in XYZ, then solve the correction in
    // the actual camera-native basis. This avoids treating sensor channels as
    // if they were display RGB (the former implementation did exactly that).
    let tint_gain = 1.0 + tint * 0.15;
    if (temperature - 6_500.0).abs() <= 0.5 {
        // 6500 K is the neutral point of this control. Keeping it exact avoids
        // introducing a small color change merely by switching modes from
        // AsShot to Temperature/Tint.
        return Ok([1.0, 1.0 / tint_gain, 1.0]);
    }
    let daylight_xyz = approximate_daylight_xyz(temperature);
    let camera_white = camera_to_xyz_d65.inverse()?.transform(daylight_xyz);
    if camera_white
        .iter()
        .any(|value| !value.is_finite() || *value <= 1.0e-8)
    {
        return Err(PipelineError::InvalidMetadata {
            field: "temperature_tint",
            reason: "the requested white point is outside the calibrated camera gamut".to_owned(),
        });
    }
    let green = camera_white[1];
    Ok([
        green / camera_white[0],
        1.0 / tint_gain,
        green / camera_white[2],
    ])
}

/// Approximate a daylight white point for the editor's temperature control.
///
/// This is a daylight-locus approximation expressed directly as a normalized
/// XYZ white point (Y = 1). The published locus is most reliable above 4000 K;
/// the same smooth polynomial is deliberately extended to the control's
/// 2000 K lower bound. It is still only an illuminant model: camera
/// calibration and as-shot gains remain the source of sensor-specific
/// behavior. Keeping the intermediate in XYZ also avoids applying a
/// display-transfer RGB approximation as though it were linear light.
fn approximate_daylight_xyz(temperature: f32) -> [f32; 3] {
    let temperature = temperature.clamp(2_000.0, 12_000.0);
    let x = if temperature <= 7_000.0 {
        -4_607_000_000.0 / temperature.powi(3)
            + 2_967_800.0 / temperature.powi(2)
            + 99.11 / temperature
            + 0.244_063
    } else {
        -2_006_400_000.0 / temperature.powi(3)
            + 1_901_800.0 / temperature.powi(2)
            + 247.48 / temperature
            + 0.237_040
    };
    let y = -3.0 * x.powi(2) + 2.87 * x - 0.275;
    [x / y, 1.0, (1.0 - x - y) / y]
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

pub(crate) fn apply_white_balance_cancellable(
    image: &mut LinearRgbImage<f32>,
    gains: WhiteBalanceGains,
    cancellation: &CancellationToken,
) -> Result<(), PipelineError> {
    cancellation.checkpoint()?;
    gains.validate()?;
    require_space(image, LinearRgbSpace::CameraNative)?;
    let width_samples = image.width() * 3;
    let row_stride = image.row_stride();
    image.data_mut().par_chunks_mut(row_stride).try_for_each(
        |row| -> Result<(), PipelineError> {
            cancellation.checkpoint()?;
            for pixel in row[..width_samples].chunks_exact_mut(3) {
                pixel[0] *= gains.red;
                pixel[1] *= gains.green;
                pixel[2] *= gains.blue;
            }
            Ok(())
        },
    )?;
    cancellation.checkpoint()
}

/// Apply global scene-linear adjustments in their documented fixed order.
///
/// Exposure is `2^EV`. The remaining Light controls share one bounded,
/// monotonic luminance LUT with a protected toe and shoulder. Tonal changes
/// scale the RGB triplet where safe to preserve chromaticity. Saturation and
/// vibrance then operate around Rec.2020 luminance. Negative and HDR working
/// samples remain available through the tonal LUT's identity extension.
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
        exposure_ev = recipe.light.exposure_ev,
        contrast = recipe.light.contrast,
        highlights = recipe.light.highlights,
        shadows = recipe.light.shadows,
        whites = recipe.light.whites,
        blacks = recipe.light.blacks,
        saturation = recipe.color.saturation,
        vibrance = recipe.color.vibrance
    );
    let _guard = span.enter();
    cancellation.checkpoint()?;
    require_space(image, LinearRgbSpace::Rec2020D65)?;
    recipe.validate()?;
    let exposure_gain = recipe.light.exposure_ev.exp2();
    let width_samples = image.width() * 3;
    let row_stride = image.row_stride();
    let light = recipe.light.clone();
    let color = recipe.color.clone();
    // Resolve stage participation once per recipe instead of repeatedly
    // checking all neutral controls for every pixel. This is especially
    // useful for the common global-adjustment path, where HSL and grading are
    // normally neutral but the image still contains millions of pixels.
    let has_light_tone = light.contrast != 0.0
        || light.highlights != 0.0
        || light.shadows != 0.0
        || light.whites != 0.0
        || light.blacks != 0.0;
    let light_tone_lut = has_light_tone.then(|| LightToneLut::new(&light));
    let has_tone_curve = light.tone_curve.shadows != 0.0
        || light.tone_curve.darks != 0.0
        || light.tone_curve.lights != 0.0
        || light.tone_curve.highlights != 0.0;
    let has_saturation = (color.saturation - 1.0).abs() > f32::EPSILON || color.vibrance != 0.0;
    let has_hsl =
        color.hsl.channels.iter().any(|channel| {
            channel.hue != 0.0 || channel.saturation != 0.0 || channel.luminance != 0.0
        });
    let has_grading = color.grading.shadows != [0.0; 3]
        || color.grading.midtones != [0.0; 3]
        || color.grading.highlights != [0.0; 3];
    image.data_mut().par_chunks_mut(row_stride).try_for_each(
        |row| -> Result<(), PipelineError> {
            cancellation.checkpoint()?;
            for pixel in row[..width_samples].chunks_exact_mut(3) {
                if light.exposure_ev != 0.0 {
                    for value in pixel.iter_mut() {
                        *value *= exposure_gain;
                    }
                }
                if let Some(light_tone_lut) = &light_tone_lut {
                    apply_light_tone(pixel, light_tone_lut);
                }
                if has_tone_curve {
                    apply_tone_curve(pixel, &light.tone_curve);
                }
                if has_saturation {
                    let luminance = luminance(pixel);
                    let saturation = color.saturation
                        * (1.0 + color.vibrance * (1.0 - color_saturation(pixel, luminance)));
                    if (saturation - 1.0).abs() > f32::EPSILON {
                        for value in pixel.iter_mut() {
                            *value = luminance + saturation * (*value - luminance);
                        }
                    }
                }
                if has_hsl {
                    apply_hsl_adjustments(pixel, &color.hsl);
                }
                if has_grading {
                    apply_color_grading(pixel, &color.grading);
                }
            }
            Ok(())
        },
    )?;
    cancellation.checkpoint()?;
    Ok(())
}

fn apply_light_tone(pixel: &mut [f32], light_tone_lut: &LightToneLut) {
    let current = luminance(pixel);
    let target = light_tone_lut.sample(current);
    if !target.is_finite() || (target - current).abs() <= f32::EPSILON {
        return;
    }
    apply_luminance_delta(pixel, current, target);
}

fn apply_tone_curve(pixel: &mut [f32], curve: &crate::ToneCurve) {
    if curve.shadows == 0.0 && curve.darks == 0.0 && curve.lights == 0.0 && curve.highlights == 0.0
    {
        return;
    }
    let current = luminance(pixel);
    let target = evaluate_tone_curve(curve, current);
    if !target.is_finite() || (target - current).abs() <= f32::EPSILON {
        return;
    }
    apply_luminance_delta(pixel, current, target);
}

/// Evaluate the monotonic scene-linear tone curve used by both the CPU and
/// the editor graph. The four recipe values are offsets at fixed inputs;
/// output points are projected into a non-decreasing curve so crossing
/// controls cannot invert tonal order. Values outside [0, 1] are left on the
/// identity extension to preserve HDR and negative working samples.
pub fn evaluate_tone_curve(curve: &crate::ToneCurve, input: f32) -> f32 {
    if !input.is_finite() || !(0.0..=1.0).contains(&input) {
        return input;
    }
    const INPUTS: [f32; 6] = [0.0, 0.12, 0.35, 0.65, 0.88, 1.0];
    let mut outputs = [
        0.0,
        INPUTS[1] + curve.shadows,
        INPUTS[2] + curve.darks,
        INPUTS[3] + curve.lights,
        INPUTS[4] + curve.highlights,
        1.0,
    ];
    for output in &mut outputs {
        *output = output.clamp(0.0, 1.0);
    }
    for index in 1..outputs.len() {
        outputs[index] = outputs[index].max(outputs[index - 1]);
    }
    let Some(index) = INPUTS.windows(2).position(|pair| input <= pair[1]) else {
        return 1.0;
    };
    let span = INPUTS[index + 1] - INPUTS[index];
    let fraction = (input - INPUTS[index]) / span;
    outputs[index] + (outputs[index + 1] - outputs[index]) * fraction
}

#[inline(always)]
fn apply_luminance_delta(pixel: &mut [f32], current: f32, target: f32) {
    if !current.is_finite() || !target.is_finite() {
        return;
    }
    if (current > 1.0e-6 && target >= LUMINANCE_RATIO_TRANSITION)
        || (current < -1.0e-6 && target <= -LUMINANCE_RATIO_TRANSITION)
    {
        let scale = target / current;
        let updated = [pixel[0] * scale, pixel[1] * scale, pixel[2] * scale];
        if updated.iter().all(|value| value.is_finite()) {
            pixel.copy_from_slice(&updated);
            return;
        }
    }
    let delta = target - current;
    if !delta.is_finite() {
        return;
    }
    // Crossing zero with a multiplicative scale would flip the sign of every
    // channel, so additive output is the stable endpoint of the transition.
    let additive = [pixel[0] + delta, pixel[1] + delta, pixel[2] + delta];
    if additive.iter().any(|value| !value.is_finite()) {
        return;
    }
    if current.abs() <= 1.0e-6 || current.signum() != target.signum() {
        pixel.copy_from_slice(&additive);
        return;
    }

    let ratio_weight = smoothstep(0.0, LUMINANCE_RATIO_TRANSITION, target.abs());
    if ratio_weight <= f32::EPSILON {
        pixel.copy_from_slice(&additive);
        return;
    }
    let scale = target / current;
    let scaled = [pixel[0] * scale, pixel[1] * scale, pixel[2] * scale];
    let updated = [
        additive[0] + (scaled[0] - additive[0]) * ratio_weight,
        additive[1] + (scaled[1] - additive[1]) * ratio_weight,
        additive[2] + (scaled[2] - additive[2]) * ratio_weight,
    ];
    if updated.iter().all(|value| value.is_finite()) {
        pixel.copy_from_slice(&updated);
    }
}

fn apply_hsl_adjustments(pixel: &mut [f32], adjustments: &crate::HslAdjustments) {
    let [red, green, blue] = [pixel[0], pixel[1], pixel[2]];
    if [red, green, blue]
        .into_iter()
        .any(|value| !value.is_finite())
    {
        return;
    }
    // HSL itself is bounded, but the working image is not. Normalize around
    // the pixel's signed range, apply HSL there, and restore the original
    // scale/offset so HDR and negative values are not silently clipped.
    let offset = (-red.min(green).min(blue)).max(0.0);
    let shifted = [red + offset, green + offset, blue + offset];
    if shifted.iter().any(|value| !value.is_finite()) {
        return;
    }
    let scale = shifted.into_iter().fold(0.0, f32::max);
    if !scale.is_finite() || scale <= f32::EPSILON {
        return;
    }
    let [mut hue, mut saturation, mut lightness] =
        rgb_to_hsl([shifted[0] / scale, shifted[1] / scale, shifted[2] / scale]);
    // Hue is undefined for a neutral pixel. Fade the mixer in over the first
    // small amount of chroma so luminance edits cannot accidentally target
    // gray pixels through the arbitrary red/zero hue returned by rgb_to_hsl.
    let chroma_weight = smoothstep(0.0, 0.05, saturation);
    if chroma_weight <= f32::EPSILON {
        return;
    }
    let channel_weights = hsl_channel_weights(hue);
    let mut hue_shift = 0.0;
    let mut saturation_shift = 0.0;
    let mut lightness_shift = 0.0;
    for (channel, weight) in adjustments.channels.iter().zip(channel_weights) {
        let weight = weight * chroma_weight;
        hue_shift += channel.hue * 0.125 * weight;
        saturation_shift += channel.saturation * 0.5 * weight;
        lightness_shift += channel.luminance * 0.25 * weight;
    }
    if hue_shift == 0.0 && saturation_shift == 0.0 && lightness_shift == 0.0 {
        return;
    }
    hue = (hue + hue_shift).rem_euclid(1.0);
    saturation = (saturation + saturation_shift).clamp(0.0, 1.0);
    lightness = (lightness + lightness_shift).clamp(0.0, 1.0);
    let converted = hsl_to_rgb([hue, saturation, lightness]);
    let restored = converted.map(|value| value * scale - offset);
    if restored.iter().all(|value| value.is_finite()) {
        pixel.copy_from_slice(&restored);
    }
}

/// Interpolate between the two named HSL color centers surrounding `hue`.
///
/// Exact centers receive one full band. Between centers the two feathered
/// weights always sum to one, including across the Magenta/Red wraparound, so
/// applying the same value to every band has exactly one adjustment's effect.
#[must_use]
pub fn hsl_channel_weights(hue: f32) -> [f32; crate::HSL_CHANNEL_COUNT] {
    let mut weights = [0.0; crate::HSL_CHANNEL_COUNT];
    if !hue.is_finite() {
        return weights;
    }
    let hue = hue.rem_euclid(1.0);
    let last = HSL_CHANNEL_CENTERS.len() - 1;
    let (left, right, start, end) = HSL_CHANNEL_CENTERS
        .windows(2)
        .enumerate()
        .find_map(|(index, centers)| {
            (hue >= centers[0] && hue < centers[1]).then_some((
                index,
                index + 1,
                centers[0],
                centers[1],
            ))
        })
        .unwrap_or((last, 0, HSL_CHANNEL_CENTERS[last], 1.0));
    let fraction = ((hue - start) / (end - start)).clamp(0.0, 1.0);
    weights[left] = 1.0 - fraction;
    weights[right] = fraction;
    weights
}

/// Find Color Mixer band weights for one display-encoded sRGB sample.
///
/// The sample is transformed back into Rohditor's linear Rec.2020 working
/// space before its hue is classified. Nearly neutral samples have no stable
/// hue and deliberately return `None`.
#[must_use]
pub fn hsl_channel_weights_from_display_rgb(
    display_rgb: [u8; 3],
) -> Option<[f32; crate::HSL_CHANNEL_COUNT]> {
    let linear_srgb = display_rgb.map(|value| crate::srgb_to_linear_srgb(f32::from(value) / 255.0));
    let linear_srgb_to_rec2020 = crate::XYZ_D65_TO_LINEAR_SRGB
        .inverse()
        .ok()?
        .then(crate::XYZ_D65_TO_LINEAR_REC2020);
    let working = linear_srgb_to_rec2020.transform(linear_srgb);
    if working.iter().any(|value| !value.is_finite()) {
        return None;
    }
    let minimum = working.into_iter().fold(f32::INFINITY, f32::min);
    let offset = (-minimum).max(0.0);
    let shifted = working.map(|value| value + offset);
    let scale = shifted.into_iter().fold(0.0, f32::max);
    if !scale.is_finite() || scale <= f32::EPSILON {
        return None;
    }
    let [hue, saturation, _] = rgb_to_hsl(shifted.map(|value| value / scale));
    (saturation >= 0.02).then(|| hsl_channel_weights(hue))
}

fn apply_color_grading(pixel: &mut [f32], grading: &crate::ColorGradingAdjustments) {
    if grading.shadows == [0.0; 3] && grading.midtones == [0.0; 3] && grading.highlights == [0.0; 3]
    {
        return;
    }
    let value = luminance(pixel).clamp(0.0, 1.0);
    let shadows = 1.0 - smoothstep(0.0, 0.5, value);
    let midtones = smoothstep(0.15, 0.45, value) * (1.0 - smoothstep(0.55, 0.85, value));
    let highlights = smoothstep(0.5, 1.0, value);
    let grade = [0, 1, 2].map(|index| {
        grading.shadows[index] * shadows
            + grading.midtones[index] * midtones
            + grading.highlights[index] * highlights
    });
    // Treat the RGB controls as a tint rather than an additive lift. Positive
    // multipliers preserve the sign of HDR/filter-lobe values, while the
    // luminance renormalization keeps a grade from silently changing exposure.
    let target = [0, 1, 2].map(|index| pixel[index] * (1.0 + 0.25 * grade[index]));
    if target.iter().any(|value| !value.is_finite()) {
        return;
    }
    let source_luminance = luminance(pixel);
    let target_luminance = luminance(&target);
    if source_luminance.abs() > 1.0e-6
        && source_luminance.is_finite()
        && target_luminance.abs() > 1.0e-6
        && target_luminance.is_finite()
        && source_luminance.signum() == target_luminance.signum()
    {
        let scale = source_luminance / target_luminance;
        let graded = target.map(|value| value * scale);
        if graded.iter().all(|value| value.is_finite()) {
            pixel.copy_from_slice(&graded);
        }
    } else {
        pixel.copy_from_slice(&target);
    }
}

fn rgb_to_hsl([red, green, blue]: [f32; 3]) -> [f32; 3] {
    let maximum = red.max(green).max(blue);
    let minimum = red.min(green).min(blue);
    let lightness = (maximum + minimum) * 0.5;
    let chroma = maximum - minimum;
    if chroma <= f32::EPSILON {
        return [0.0, 0.0, lightness];
    }
    let saturation = chroma / (1.0 - (2.0 * lightness - 1.0).abs());
    let hue = if maximum == red {
        ((green - blue) / chroma).rem_euclid(6.0) / 6.0
    } else if maximum == green {
        ((blue - red) / chroma + 2.0) / 6.0
    } else {
        ((red - green) / chroma + 4.0) / 6.0
    };
    [hue, saturation, lightness]
}

fn hsl_to_rgb([hue, saturation, lightness]: [f32; 3]) -> [f32; 3] {
    if saturation <= f32::EPSILON {
        return [lightness; 3];
    }
    let chroma = (1.0 - (2.0 * lightness - 1.0).abs()) * saturation;
    let hue_prime = hue * 6.0;
    let secondary = chroma * (1.0 - ((hue_prime.rem_euclid(2.0)) - 1.0).abs());
    let (red, green, blue) = match hue_prime {
        value if value < 1.0 => (chroma, secondary, 0.0),
        value if value < 2.0 => (secondary, chroma, 0.0),
        value if value < 3.0 => (0.0, chroma, secondary),
        value if value < 4.0 => (0.0, secondary, chroma),
        value if value < 5.0 => (secondary, 0.0, chroma),
        _ => (chroma, 0.0, secondary),
    };
    let match_value = lightness - chroma * 0.5;
    [red + match_value, green + match_value, blue + match_value]
}

fn luminance(pixel: &[f32]) -> f32 {
    pixel[0] * REC2020_LUMINANCE[0]
        + pixel[1] * REC2020_LUMINANCE[1]
        + pixel[2] * REC2020_LUMINANCE[2]
}

fn color_saturation(pixel: &[f32], luminance: f32) -> f32 {
    let chroma = pixel
        .iter()
        .map(|value| (value - luminance).abs())
        .fold(0.0, f32::max);
    (chroma / luminance.abs().max(1.0e-6)).clamp(0.0, 1.0)
}

fn smoothstep(edge0: f32, edge1: f32, value: f32) -> f32 {
    let normalized = ((value - edge0) / (edge1 - edge0)).clamp(0.0, 1.0);
    normalized * normalized * (3.0 - 2.0 * normalized)
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
    .map_err(Into::into)
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
    .map_err(Into::into)
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
    use crate::{DemosaicAlgorithm, ToneCurve, demosaic};

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
    fn neutral_temperature_tint_preserves_as_shot_gains() {
        let info = test_info(2, 2, "RGGB");
        let as_shot = white_balance_gains(&info, WhiteBalance::AsShot).expect("as-shot balance");
        let neutral = white_balance_gains(
            &info,
            WhiteBalance::TemperatureTint {
                temperature: 6_500.0,
                tint: 0.0,
            },
        )
        .expect("neutral temperature balance");
        assert_eq!(as_shot, neutral);
    }

    #[test]
    fn temperature_tint_uses_the_calibrated_camera_basis() {
        let camera_to_xyz =
            crate::Matrix3::new([[0.90, 0.08, 0.02], [0.03, 0.94, 0.03], [0.01, 0.08, 0.91]]);
        let selection = WhiteBalance::TemperatureTint {
            temperature: 3_200.0,
            tint: 0.0,
        };
        let display_basis = white_balance_gains_from_calibration(
            [Some(1.0), Some(1.0), Some(1.0), None],
            crate::Matrix3::identity(),
            selection,
        )
        .expect("identity basis should resolve");
        let camera_basis = white_balance_gains_from_calibration(
            [Some(1.0), Some(1.0), Some(1.0), None],
            camera_to_xyz,
            selection,
        )
        .expect("calibrated basis should resolve");
        assert!(
            [camera_basis.red, camera_basis.green, camera_basis.blue]
                .into_iter()
                .all(|value| value.is_finite() && value > 0.0)
        );
        assert!(
            (camera_basis.red - display_basis.red).abs() > 1.0e-3
                || (camera_basis.blue - display_basis.blue).abs() > 1.0e-3
        );
    }

    #[test]
    fn daylight_temperature_model_is_a_positive_xyz_white_point() {
        let d65 = approximate_daylight_xyz(6_500.0);
        assert!((d65[0] - 0.9505).abs() < 0.01);
        assert_eq!(d65[1], 1.0);
        assert!((d65[2] - 1.089).abs() < 0.01);
        for temperature in [2_000.0, 3_200.0, 6_500.0, 12_000.0] {
            assert!(
                approximate_daylight_xyz(temperature)
                    .into_iter()
                    .all(|value| value.is_finite() && value > 0.0)
            );
        }
    }

    #[test]
    fn exposure_neutral_is_identity_and_known_ev_is_power_of_two() {
        let source = vec![0.1, 0.2, 0.3, 0.5, 0.6, 0.7];
        let mut neutral = LinearRgbImage::new(2, 1, 6, LinearRgbSpace::Rec2020D65, source.clone())
            .expect("valid image");
        apply_adjustments(&mut neutral, &EditRecipe::default()).expect("neutral adjustment");
        assert_eq!(neutral.data(), source);

        let mut raised = neutral;
        let mut recipe = EditRecipe::default();
        recipe.light.exposure_ev = 2.0;
        apply_adjustments(&mut raised, &recipe).expect("exposure adjustment");
        for (actual, original) in raised.data().iter().zip(source) {
            assert_eq!(*actual, original * 4.0);
        }
    }

    #[test]
    fn contrast_pivot_and_saturation_grayscale_are_stable() {
        const CONTRAST_PIVOT: f32 = 0.18;
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
        let mut recipe = EditRecipe::default();
        recipe.light.contrast = 1.0;
        recipe.color.saturation = 2.0;
        apply_adjustments(&mut image, &recipe).expect("valid adjustments");
        for value in image.pixel(0, 0).expect("pivot pixel") {
            assert!((*value - CONTRAST_PIVOT).abs() < 1.0e-6);
        }
        let gray = image.pixel(1, 0).expect("second pixel");
        assert!((gray[0] - gray[1]).abs() < 1.0e-6);
        assert!((gray[1] - gray[2]).abs() < 1.0e-6);
    }

    #[test]
    fn tonal_controls_target_shadow_and_highlight_ranges() {
        let source = vec![0.1, 0.1, 0.1, 0.9, 0.9, 0.9];
        let mut shadows = LinearRgbImage::new(2, 1, 6, LinearRgbSpace::Rec2020D65, source.clone())
            .expect("valid image");
        let mut shadows_recipe = EditRecipe::default();
        shadows_recipe.light.shadows = 1.0;
        apply_adjustments(&mut shadows, &shadows_recipe).expect("shadow adjustment");
        assert!(shadows.pixel(0, 0).expect("shadow pixel")[0] > source[0]);
        assert!(shadows.pixel(1, 0).expect("highlight pixel")[0] < source[3] + 0.001);

        let mut highlights =
            LinearRgbImage::new(2, 1, 6, LinearRgbSpace::Rec2020D65, source).expect("valid image");
        let mut highlights_recipe = EditRecipe::default();
        highlights_recipe.light.highlights = -1.0;
        apply_adjustments(&mut highlights, &highlights_recipe).expect("highlight adjustment");
        assert!(highlights.pixel(1, 0).expect("highlight pixel")[0] < 0.9);
        assert!(highlights.pixel(0, 0).expect("shadow pixel")[0] > 0.099);
    }

    #[test]
    fn tone_curve_regions_adjust_luminance_without_changing_neutral_color() {
        let mut image = LinearRgbImage::new(
            2,
            1,
            6,
            LinearRgbSpace::Rec2020D65,
            vec![0.2, 0.2, 0.2, 0.8, 0.8, 0.8],
        )
        .expect("valid image");
        let mut recipe = EditRecipe::default();
        recipe.light.tone_curve.darks = 0.1;
        recipe.light.tone_curve.highlights = -0.1;
        apply_adjustments(&mut image, &recipe).expect("tone curve adjustment");
        for pixel in image.data().chunks_exact(3) {
            assert!((pixel[0] - pixel[1]).abs() < 1.0e-6);
            assert!((pixel[1] - pixel[2]).abs() < 1.0e-6);
        }
        assert!(image.pixel(0, 0).expect("dark pixel")[0] > 0.2);
        assert!(image.pixel(1, 0).expect("light pixel")[0] < 0.8);
    }

    #[test]
    fn hsl_mixer_and_color_grading_change_only_the_requested_color_regions() {
        let mut image = LinearRgbImage::new(
            2,
            1,
            6,
            LinearRgbSpace::Rec2020D65,
            vec![0.9, 0.2, 0.2, 0.1, 0.1, 0.1],
        )
        .expect("valid image");
        let mut recipe = EditRecipe::default();
        recipe.color.hsl.channels[0].hue = 0.5;
        recipe.color.grading.shadows[2] = 1.0;
        apply_adjustments(&mut image, &recipe).expect("color adjustment");
        let red = image.pixel(0, 0).expect("red pixel");
        let shadow = image.pixel(1, 0).expect("shadow pixel");
        assert!(red[1] > 0.2);
        assert!(shadow[2] > 0.1);
    }

    #[test]
    fn hsl_keeps_scene_linear_hdr_and_negative_range() {
        let source = [-0.25, 1.5, 2.25];
        let mut image = LinearRgbImage::new(1, 1, 3, LinearRgbSpace::Rec2020D65, source.to_vec())
            .expect("valid HDR image");
        let mut recipe = EditRecipe::default();
        recipe.color.hsl.channels[4].hue = 0.5;

        apply_adjustments(&mut image, &recipe).expect("valid HSL adjustment");
        let pixel = image.pixel(0, 0).expect("pixel remains present");
        assert!(pixel.iter().all(|value| value.is_finite()));
        assert!(pixel.iter().any(|value| *value > 1.0));
        assert!(pixel.iter().any(|value| *value < 0.0));
    }

    #[test]
    fn hsl_channel_weights_match_centers_midpoints_and_wraparound() {
        for (index, center) in HSL_CHANNEL_CENTERS.into_iter().enumerate() {
            let weights = hsl_channel_weights(center);
            for (channel, weight) in weights.into_iter().enumerate() {
                let expected = if channel == index { 1.0 } else { 0.0 };
                assert!((weight - expected).abs() < 1.0e-6);
            }
        }

        for index in 0..HSL_CHANNEL_CENTERS.len() {
            let next = (index + 1) % HSL_CHANNEL_CENTERS.len();
            let start = HSL_CHANNEL_CENTERS[index];
            let end = if next == 0 {
                1.0
            } else {
                HSL_CHANNEL_CENTERS[next]
            };
            let weights = hsl_channel_weights((start + end) * 0.5);
            assert!((weights[index] - 0.5).abs() < 1.0e-6);
            assert!((weights[next] - 0.5).abs() < 1.0e-6);
            assert!((weights.into_iter().sum::<f32>() - 1.0).abs() < 1.0e-6);
        }
    }

    #[test]
    fn equal_hsl_band_values_are_not_amplified_in_overlap_regions() {
        let source_hsl = [45.0 / 360.0, 0.5, 0.5];
        let source = hsl_to_rgb(source_hsl);
        let mut pixel = source;
        let mut adjustments = crate::HslAdjustments::default();
        for channel in &mut adjustments.channels {
            channel.hue = 0.4;
        }

        apply_hsl_adjustments(&mut pixel, &adjustments);

        let adjusted = rgb_to_hsl(pixel);
        assert!((adjusted[0] - (source_hsl[0] + 0.05)).abs() < 1.0e-6);
    }

    #[test]
    fn hsl_mixer_does_not_assign_a_hue_to_neutral_pixels() {
        let mut pixel = [0.4; 3];
        let mut adjustments = crate::HslAdjustments::default();
        for channel in &mut adjustments.channels {
            channel.hue = 1.0;
            channel.saturation = 1.0;
            channel.luminance = 1.0;
        }

        apply_hsl_adjustments(&mut pixel, &adjustments);

        assert_eq!(pixel, [0.4; 3]);
        assert!(hsl_channel_weights_from_display_rgb([128; 3]).is_none());
    }

    #[test]
    fn display_rgb_picker_maps_primaries_to_the_expected_mixer_bands() {
        for (sample, expected) in [([255, 0, 0], 0), ([0, 255, 0], 3), ([0, 0, 255], 5)] {
            let weights = hsl_channel_weights_from_display_rgb(sample)
                .expect("a saturated display primary has a stable hue");
            let selected = weights
                .into_iter()
                .enumerate()
                .max_by(|(_, left), (_, right)| left.total_cmp(right))
                .map(|(index, _)| index);
            assert_eq!(selected, Some(expected));
        }
    }

    #[test]
    fn every_adjustment_stage_keeps_asymmetric_working_values_finite() {
        let mut image = LinearRgbImage::new(
            2,
            2,
            6,
            LinearRgbSpace::Rec2020D65,
            vec![
                -0.35, 0.08, 1.75, 0.02, 0.65, 1.20, 0.9, -0.1, 0.4, 1.4, 0.3, -0.2,
            ],
        )
        .expect("valid asymmetric HDR image");
        let mut recipe = EditRecipe::default();
        recipe.light.exposure_ev = 1.25;
        recipe.light.contrast = -0.4;
        recipe.light.highlights = -0.7;
        recipe.light.shadows = 0.65;
        recipe.light.whites = 0.55;
        recipe.light.blacks = -0.6;
        recipe.light.tone_curve = ToneCurve {
            shadows: 0.2,
            darks: -0.15,
            lights: 0.18,
            highlights: -0.2,
        };
        recipe.color.saturation = 1.45;
        recipe.color.vibrance = -0.6;
        for (index, channel) in recipe.color.hsl.channels.iter_mut().enumerate() {
            channel.hue = if index % 2 == 0 { 0.4 } else { -0.35 };
            channel.saturation = if index % 3 == 0 { 0.3 } else { -0.2 };
            channel.luminance = if index % 2 == 0 { -0.25 } else { 0.2 };
        }
        recipe.color.grading.shadows = [0.8, -0.6, 0.4];
        recipe.color.grading.midtones = [-0.5, 0.7, -0.3];
        recipe.color.grading.highlights = [0.35, -0.45, 0.65];

        apply_adjustments(&mut image, &recipe).expect("all adjustment values are valid");
        assert!(
            image.data().iter().all(|value| value.is_finite()),
            "adjustments produced a non-finite sample"
        );
    }

    #[test]
    fn tone_curve_extremes_remain_monotonic_and_keep_endpoints() {
        let curve = ToneCurve {
            shadows: 0.25,
            darks: -0.25,
            lights: 0.25,
            highlights: -0.25,
        };
        let mut previous = evaluate_tone_curve(&curve, 0.0);
        assert_eq!(previous, 0.0);
        for index in 1..=1_000 {
            let input = index as f32 / 1_000.0;
            let output = evaluate_tone_curve(&curve, input);
            assert!(output >= previous, "curve decreased at input {input}");
            previous = output;
        }
        assert_eq!(previous, 1.0);
        assert_eq!(evaluate_tone_curve(&curve, -0.5), -0.5);
        assert_eq!(evaluate_tone_curve(&curve, 1.5), 1.5);
    }

    #[test]
    fn light_lut_preserves_negative_working_values_without_sign_flip() {
        let source = vec![-0.4, -0.2, -0.1];
        let mut image = LinearRgbImage::new(1, 1, 3, LinearRgbSpace::Rec2020D65, source.clone())
            .expect("valid image");
        let mut recipe = EditRecipe::default();
        recipe.light.shadows = 1.0;

        apply_adjustments(&mut image, &recipe).expect("valid tonal adjustment");
        let pixel = image.pixel(0, 0).expect("pixel remains present");
        assert!(pixel.iter().all(|value| value.is_finite()));
        assert_eq!(pixel, source);
    }

    #[test]
    fn tonal_luminance_transition_is_continuous_near_black() {
        let mut below_transition = [0.15, 0.1, 0.05];
        let mut above_transition = below_transition;
        apply_luminance_delta(&mut below_transition, 0.12, 0.0099);
        apply_luminance_delta(&mut above_transition, 0.12, 0.0101);
        for (below, above) in below_transition.iter().zip(above_transition) {
            assert!((below - above).abs() < 0.01);
        }

        let mut negative_crossing = [0.15, 0.1, 0.05];
        let mut positive_crossing = negative_crossing;
        apply_luminance_delta(&mut negative_crossing, 0.12, -0.0001);
        apply_luminance_delta(&mut positive_crossing, 0.12, 0.0001);
        for (negative, positive) in negative_crossing.iter().zip(positive_crossing) {
            assert!((negative - positive).abs() < 0.001);
        }
    }

    #[test]
    fn three_way_rgb_tint_preserves_neutral_luminance() {
        let mut image =
            LinearRgbImage::new(1, 1, 3, LinearRgbSpace::Rec2020D65, vec![0.4, 0.4, 0.4])
                .expect("valid image");
        let mut recipe = EditRecipe::default();
        recipe.color.grading.shadows[0] = 1.0;

        apply_adjustments(&mut image, &recipe).expect("valid grading adjustment");
        let pixel = image.pixel(0, 0).expect("pixel remains present");
        assert!((luminance(pixel) - 0.4).abs() < 1.0e-6);
        assert!(pixel[0] > pixel[1]);
        assert!(pixel[1] > pixel[2] - 1.0e-6);
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
