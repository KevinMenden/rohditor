//! Backend-neutral contracts for the RAW sensor-development stages.
//!
//! The contracts describe validated metadata and algorithm choices without
//! retaining pixel storage. CPU preparation consumes the same normalization
//! contract today; a later GPU executor can consume it without reinterpreting
//! decoder metadata or recipe fields.

use rayon::prelude::*;
use rohditor_demosaic::{DemosaicContract, WhiteBalanceGains};
use rohditor_edit::{
    CameraProfileSelection, EditRecipe, HighlightAdjustments, HighlightMethod, WhiteBalance,
};
use rohditor_highlight::{
    ChannelClipLevels, ChannelDetectionLevels, HighlightExecution, LocalRatioOptions,
    OpposedOptions,
};
use rohditor_image::{
    BayerPattern, CfaColor, ImageRegion, MosaicImage, Orientation, allocate_zeroed_f32,
};
use rohditor_raw::{ImageRect, LevelPattern, PhotometricInterpretation, RawFileInfo, RawFrame};

use super::{RawCropPolicy, RenderOptions};
use crate::{
    CameraCalibration, CameraColorTransform, CameraProfileKey, CancellationToken, PipelineError,
    camera_profile_key, resolve_camera_colour,
};

/// Resolved crop and Bayer phase for one sensor-development request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SensorCrop {
    origin_x: usize,
    origin_y: usize,
    width: usize,
    height: usize,
    pattern: BayerPattern,
}

impl SensorCrop {
    #[must_use]
    pub const fn origin(self) -> (usize, usize) {
        (self.origin_x, self.origin_y)
    }

    #[must_use]
    pub const fn dimensions(self) -> (usize, usize) {
        (self.width, self.height)
    }

    /// Bayer phase at crop-local coordinates.
    #[must_use]
    pub const fn pattern(self) -> BayerPattern {
        self.pattern
    }
}

/// Pixel-storage-independent normalization metadata.
#[derive(Debug, Clone, PartialEq)]
pub struct NormalizationContract {
    sensor_dimensions: (usize, usize),
    row_stride: usize,
    sensor_pattern: BayerPattern,
    crop: SensorCrop,
    active_area: Option<ImageRect>,
    recommended_crop: Option<ImageRect>,
    black_levels: LevelPattern,
    white_levels: Vec<f32>,
}

impl NormalizationContract {
    /// Resolve and validate the normalization metadata for one decoded frame.
    pub fn from_frame(frame: &RawFrame, crop_policy: RawCropPolicy) -> Result<Self, PipelineError> {
        validate_raw_layout(frame)?;
        Self::from_raw_info(&frame.info, frame.row_stride, crop_policy)
    }

    /// Resolve metadata without retaining or inspecting RAW pixels.
    pub fn from_raw_info(
        info: &RawFileInfo,
        row_stride: usize,
        crop_policy: RawCropPolicy,
    ) -> Result<Self, PipelineError> {
        if row_stride < info.width {
            return Err(invalid_dimensions(
                info.width,
                info.height,
                row_stride,
                "decoded row stride is shorter than the sensor width",
            ));
        }
        let (sensor_pattern, crop_region) = development_geometry(info, crop_policy)?;
        validate_levels(info, sensor_pattern)?;
        Ok(Self {
            sensor_dimensions: (info.width, info.height),
            row_stride,
            sensor_pattern,
            crop: SensorCrop {
                origin_x: crop_region.x,
                origin_y: crop_region.y,
                width: crop_region.width,
                height: crop_region.height,
                pattern: sensor_pattern.shifted(crop_region.x, crop_region.y),
            },
            active_area: info.active_area,
            recommended_crop: info.crop_area,
            black_levels: info.black_levels.clone(),
            white_levels: info.white_levels.clone(),
        })
    }

    #[must_use]
    pub const fn sensor_dimensions(&self) -> (usize, usize) {
        self.sensor_dimensions
    }

    #[must_use]
    pub const fn row_stride(&self) -> usize {
        self.row_stride
    }

    #[must_use]
    pub const fn crop(&self) -> SensorCrop {
        self.crop
    }

    #[must_use]
    pub const fn black_levels(&self) -> &LevelPattern {
        &self.black_levels
    }

    #[must_use]
    pub fn white_levels(&self) -> &[f32] {
        &self.white_levels
    }

    /// Resolve the black/white pair at absolute sensor coordinates.
    pub fn levels_at_sensor(
        &self,
        sensor_x: usize,
        sensor_y: usize,
    ) -> Result<(f32, f32), PipelineError> {
        if sensor_x >= self.sensor_dimensions.0 || sensor_y >= self.sensor_dimensions.1 {
            return Err(PipelineError::InvalidDimensions {
                width: self.sensor_dimensions.0,
                height: self.sensor_dimensions.1,
                row_stride: self.row_stride,
                reason: format!("sensor coordinate ({sensor_x}, {sensor_y}) is outside the frame"),
            });
        }
        let black_index = level_index(&self.black_levels, sensor_x, sensor_y, 0);
        let black = self.black_levels.values[black_index];
        let white = white_level(
            &self.white_levels,
            black_index,
            self.sensor_pattern.color_at(sensor_x, sensor_y),
        );
        Ok((black, white))
    }

    pub fn normalized_mosaic_bytes(&self) -> Result<usize, PipelineError> {
        self.crop
            .width
            .checked_mul(self.crop.height)
            .and_then(|pixels| pixels.checked_mul(std::mem::size_of::<f32>()))
            .ok_or_else(|| {
                invalid_dimensions(
                    self.crop.width,
                    self.crop.height,
                    self.crop.width,
                    "normalized mosaic byte count overflowed",
                )
            })
    }

    /// Verify that an immutable decoded frame is still compatible with this
    /// metadata-only contract.
    ///
    /// Executors use this at their storage boundary. It intentionally checks
    /// the RAW layout and every normalization input without inspecting or
    /// copying pixel samples.
    pub fn validate_frame(&self, frame: &RawFrame) -> Result<(), PipelineError> {
        validate_raw_layout(frame)?;
        if (frame.info.width, frame.info.height) != self.sensor_dimensions
            || frame.row_stride != self.row_stride
        {
            return Err(PipelineError::InvalidMetadata {
                field: "raw_frame",
                reason: "decoded frame does not match its normalization contract".to_owned(),
            });
        }
        if sensor_pattern(&frame.info)? != self.sensor_pattern
            || frame.info.active_area != self.active_area
            || frame.info.crop_area != self.recommended_crop
            || frame.info.black_levels != self.black_levels
            || frame.info.white_levels != self.white_levels
        {
            return Err(PipelineError::InvalidMetadata {
                field: "raw_frame",
                reason: "normalization metadata does not match its contract".to_owned(),
            });
        }
        Ok(())
    }

    pub(crate) fn normalize_full(
        &self,
        frame: &RawFrame,
        cancellation: &CancellationToken,
    ) -> Result<MosaicImage<f32>, PipelineError> {
        self.normalize(frame, self.crop.dimensions(), cancellation)
    }

    pub(crate) fn normalize_preview(
        &self,
        frame: &RawFrame,
        output_dimensions: (usize, usize),
        cancellation: &CancellationToken,
    ) -> Result<MosaicImage<f32>, PipelineError> {
        self.normalize(frame, output_dimensions, cancellation)
    }

    fn normalize(
        &self,
        frame: &RawFrame,
        (output_width, output_height): (usize, usize),
        cancellation: &CancellationToken,
    ) -> Result<MosaicImage<f32>, PipelineError> {
        cancellation.checkpoint()?;
        self.validate_frame(frame)?;
        if output_width < 2
            || output_height < 2
            || output_width > self.crop.width
            || output_height > self.crop.height
        {
            return Err(invalid_dimensions(
                output_width,
                output_height,
                output_width,
                "normalization output must be at least 2x2 and no larger than the resolved crop",
            ));
        }

        let elements = output_width.checked_mul(output_height).ok_or_else(|| {
            invalid_dimensions(
                output_width,
                output_height,
                output_width,
                "normalized crop overflowed",
            )
        })?;
        let mut normalized = allocate_zeroed_f32(elements)?;
        normalized
            .par_chunks_mut(output_width)
            .enumerate()
            .try_for_each(|(output_y, output_row)| -> Result<(), PipelineError> {
                cancellation.checkpoint()?;
                let crop_y = phase_preserving_sample(output_y, output_height, self.crop.height);
                let sensor_y = self.crop.origin_y + crop_y;
                for (output_x, destination) in output_row.iter_mut().enumerate() {
                    let crop_x = phase_preserving_sample(output_x, output_width, self.crop.width);
                    let sensor_x = self.crop.origin_x + crop_x;
                    let sample = frame.mosaic[sensor_y * self.row_stride + sensor_x];
                    let (black, white) = self.levels_at_sensor(sensor_x, sensor_y)?;
                    *destination = (f32::from(sample) - black) / (white - black);
                }
                Ok(())
            })?;
        cancellation.checkpoint()?;

        MosaicImage::new(
            output_width,
            output_height,
            output_width,
            self.crop.pattern,
            normalized,
        )
        .map_err(Into::into)
    }
}

/// Complete validated description for the RAW stages before capture sharpening.
#[derive(Debug, Clone, PartialEq)]
pub struct SensorDevelopmentDescription {
    normalization: NormalizationContract,
    calibration: CameraCalibration,
    profile_selection: CameraProfileSelection,
    camera_profile: CameraProfileKey,
    camera_color_transform: CameraColorTransform,
    white_balance: WhiteBalance,
    white_balance_gains: WhiteBalanceGains,
    highlight_adjustments: HighlightAdjustments,
    highlight_execution: HighlightExecution,
    demosaic: DemosaicContract,
    source_orientation: Orientation,
}

impl SensorDevelopmentDescription {
    /// Build the validated metadata and algorithm choices needed to develop a
    /// decoded Bayer frame. This does not allocate output pixel storage.
    pub fn from_frame(
        frame: &RawFrame,
        recipe: &EditRecipe,
        options: RenderOptions,
    ) -> Result<Self, PipelineError> {
        recipe.validate()?;
        let normalization = NormalizationContract::from_frame(frame, options.raw_crop_policy)?;
        let calibration = CameraCalibration::from_raw_info(&frame.info);
        let profile_selection = recipe.color.camera_profile.clone();
        let white_balance = recipe.color.white_balance;
        let resolved = resolve_camera_colour(&calibration, &profile_selection, white_balance)?;
        let highlight_adjustments = recipe.raw.highlights;
        Ok(Self {
            normalization,
            calibration,
            camera_profile: camera_profile_key(&profile_selection),
            camera_color_transform: resolved.camera_color_transform(),
            profile_selection,
            white_balance,
            white_balance_gains: resolved.white_balance_gains,
            highlight_execution: highlight_execution(
                highlight_adjustments,
                resolved.white_balance_gains,
            ),
            highlight_adjustments,
            demosaic: options.demosaic.contract(),
            source_orientation: frame.info.orientation,
        })
    }

    #[must_use]
    pub const fn normalization(&self) -> &NormalizationContract {
        &self.normalization
    }

    #[must_use]
    pub const fn calibration(&self) -> &CameraCalibration {
        &self.calibration
    }

    #[must_use]
    pub const fn profile_selection(&self) -> &CameraProfileSelection {
        &self.profile_selection
    }

    #[must_use]
    pub fn camera_profile_key(&self) -> &CameraProfileKey {
        &self.camera_profile
    }

    #[must_use]
    pub const fn camera_color_transform(&self) -> &CameraColorTransform {
        &self.camera_color_transform
    }

    #[must_use]
    pub const fn white_balance(&self) -> WhiteBalance {
        self.white_balance
    }

    #[must_use]
    pub const fn white_balance_gains(&self) -> WhiteBalanceGains {
        self.white_balance_gains
    }

    #[must_use]
    pub const fn highlight_adjustments(&self) -> HighlightAdjustments {
        self.highlight_adjustments
    }

    #[must_use]
    pub const fn highlight_execution(&self) -> HighlightExecution {
        self.highlight_execution
    }

    #[must_use]
    pub const fn highlight_algorithm_version(&self) -> Option<u8> {
        self.highlight_execution.algorithm_version()
    }

    #[must_use]
    pub const fn demosaic(&self) -> DemosaicContract {
        self.demosaic
    }

    #[must_use]
    pub const fn source_orientation(&self) -> Orientation {
        self.source_orientation
    }
}

fn highlight_execution(
    adjustments: HighlightAdjustments,
    gains: WhiteBalanceGains,
) -> HighlightExecution {
    match adjustments.method {
        HighlightMethod::Off => HighlightExecution::Off,
        HighlightMethod::Clip => {
            let common_ceiling =
                adjustments.clip.threshold * gains.red.min(gains.green).min(gains.blue);
            HighlightExecution::Clip(ChannelClipLevels {
                red: common_ceiling / gains.red,
                green: common_ceiling / gains.green,
                blue: common_ceiling / gains.blue,
            })
        }
        HighlightMethod::LocalRatios => {
            let level = adjustments.local_ratios.detection_threshold;
            HighlightExecution::LocalRatios(LocalRatioOptions {
                detection_levels: ChannelDetectionLevels {
                    red: level,
                    green: level,
                    blue: level,
                },
            })
        }
        HighlightMethod::Opposed => {
            let level = adjustments.opposed.detection_threshold;
            HighlightExecution::Opposed(OpposedOptions {
                detection_levels: ChannelDetectionLevels {
                    red: level,
                    green: level,
                    blue: level,
                },
            })
        }
    }
}

pub(crate) fn raw_crop_dimensions(
    info: &RawFileInfo,
    policy: RawCropPolicy,
) -> Result<(usize, usize), PipelineError> {
    let contract = NormalizationContract::from_raw_info(info, info.width, policy)?;
    Ok(contract.crop.dimensions())
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
    policy: RawCropPolicy,
) -> Result<(BayerPattern, ImageRegion), PipelineError> {
    let pattern = sensor_pattern(info)?;
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
        RawCropPolicy::ActiveArea => active,
        RawCropPolicy::Recommended => info.crop_area.map_or(active, image_region),
    };
    validate_region(crop, active, "crop_area")?;
    Ok((pattern, crop))
}

fn sensor_pattern(info: &RawFileInfo) -> Result<BayerPattern, PipelineError> {
    match &info.photometric_interpretation {
        PhotometricInterpretation::Cfa { pattern } => Ok(BayerPattern::parse(
            &pattern.name,
            pattern.width,
            pattern.height,
        )?),
        other => Err(PipelineError::InvalidMetadata {
            field: "photometric_interpretation",
            reason: format!("CPU Bayer pipeline cannot process {other:?}"),
        }),
    }
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
            let white = white_level(&info.white_levels, index, pattern.color_at(x, y));
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

fn white_level(white_levels: &[f32], black_index: usize, color: CfaColor) -> f32 {
    match white_levels {
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

    use rohditor_demosaic::DemosaicAlgorithm;
    use rohditor_edit::{EditRecipe, HighlightMethod};
    use rohditor_image::{BayerPattern, Orientation};
    use rohditor_raw::{
        CameraColorMatrix, CameraMatrixOrigin, CaptureMetadata, CfaPattern, ImageRect, RawFileInfo,
    };

    use super::*;

    fn info(pattern: &str) -> RawFileInfo {
        RawFileInfo {
            format: "synthetic".to_owned(),
            make: "Rohditor".to_owned(),
            model: "Sensor contract fixture".to_owned(),
            clean_make: "Rohditor".to_owned(),
            clean_model: "Sensor contract fixture".to_owned(),
            source_size_bytes: 0,
            source_identity: None,
            width: 6,
            height: 5,
            components_per_pixel: 1,
            source_bits_per_sample: Some(16),
            decoded_bits_per_sample: 16,
            compression: None,
            active_area: Some(ImageRect {
                x: 0,
                y: 0,
                width: 6,
                height: 5,
            }),
            crop_area: Some(ImageRect {
                x: 1,
                y: 1,
                width: 4,
                height: 4,
            }),
            photometric_interpretation: PhotometricInterpretation::Cfa {
                pattern: CfaPattern {
                    name: pattern.to_owned(),
                    width: 2,
                    height: 2,
                },
            },
            black_levels: LevelPattern {
                values: vec![0.0, 10.0, 20.0, 30.0],
                repeat_width: 2,
                repeat_height: 2,
                components_per_pixel: 1,
            },
            white_levels: vec![100.0, 110.0, 120.0, 130.0],
            as_shot_white_balance: [Some(1.0), Some(1.0), Some(1.0), None],
            xyz_to_camera: [[0.0; 3]; 4],
            color_matrices: vec![CameraColorMatrix {
                illuminant: "D65".to_owned(),
                values: vec![1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0],
                origin: CameraMatrixOrigin::DecoderDatabase,
            }],
            orientation: Orientation::Rotate90,
            capture: CaptureMetadata::default(),
            embedded_preview: None,
        }
    }

    fn frame(pattern: &str) -> RawFrame {
        let row_stride = 8;
        let mut samples = vec![u16::MAX; row_stride * 5];
        for y in 0..5 {
            for x in 0..6 {
                let index = (y & 1) * 2 + (x & 1);
                samples[y * row_stride + x] = [50, 60, 70, 80][index];
            }
        }
        // This is the crop's top-left site: its black/white pair is 30/130.
        samples[row_stride + 1] = 160;
        RawFrame {
            info: info(pattern),
            row_stride,
            mosaic: Arc::from(samples),
        }
    }

    #[test]
    fn contracts_keep_sensor_coordinates_levels_and_all_crop_shifted_phases() {
        for (name, pattern) in [
            ("RGGB", BayerPattern::Rggb),
            ("BGGR", BayerPattern::Bggr),
            ("GRBG", BayerPattern::Grbg),
            ("GBRG", BayerPattern::Gbrg),
        ] {
            let frame = frame(name);
            let contract = NormalizationContract::from_frame(&frame, RawCropPolicy::Recommended)
                .expect("fixture metadata is valid");
            assert_eq!(contract.sensor_dimensions(), (6, 5));
            assert_eq!(contract.row_stride(), 8);
            assert_eq!(contract.crop().origin(), (1, 1));
            assert_eq!(contract.crop().dimensions(), (4, 4));
            assert_eq!(contract.crop().pattern(), pattern.shifted(1, 1));
            assert_eq!(
                contract
                    .levels_at_sensor(1, 1)
                    .expect("coordinate is valid"),
                (30.0, 130.0)
            );
            assert_eq!(
                contract
                    .normalized_mosaic_bytes()
                    .expect("small crop has a valid byte count"),
                64
            );

            let mosaic = contract
                .normalize_full(&frame, &CancellationToken::new())
                .expect("normalization succeeds");
            assert_eq!(mosaic.pattern(), pattern.shifted(1, 1));
            assert_eq!(mosaic.get(0, 0), Some(&1.3));
            assert!(mosaic.data().iter().skip(1).all(|sample| *sample == 0.5));
        }
    }

    #[test]
    fn normalization_contract_rejects_a_frame_with_changed_level_metadata() {
        let frame = frame("RGGB");
        let contract = NormalizationContract::from_frame(&frame, RawCropPolicy::Recommended)
            .expect("fixture metadata is valid");
        let mut changed = frame.clone();
        changed.info.white_levels[0] = 131.0;
        assert!(contract.validate_frame(&changed).is_err());
    }

    #[test]
    fn development_description_resolves_highlight_and_demosaic_without_pixels() {
        let frame = frame("RGGB");
        let mut recipe = EditRecipe::default();
        recipe.raw.highlights.method = HighlightMethod::Clip;
        let options = RenderOptions {
            raw_crop_policy: RawCropPolicy::Recommended,
            demosaic: DemosaicAlgorithm::MalvarHeCutler,
            ..RenderOptions::default()
        };

        let description = SensorDevelopmentDescription::from_frame(&frame, &recipe, options)
            .expect("valid description");
        assert_eq!(description.source_orientation(), Orientation::Rotate90);
        assert_eq!(description.normalization().crop().dimensions(), (4, 4));
        assert_eq!(
            description.demosaic().algorithm(),
            DemosaicAlgorithm::MalvarHeCutler
        );
        assert!(matches!(
            description.highlight_execution(),
            HighlightExecution::Clip(_)
        ));
        assert_eq!(description.highlight_algorithm_version(), Some(1));
    }
}
