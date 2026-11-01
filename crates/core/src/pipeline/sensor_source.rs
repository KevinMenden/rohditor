//! Pixel-free identity and completion metadata for a sensor backend.
use super::*;

/// Holds immutable decoded RAW identity, but no normalized mosaic or RGB image.
/// Recipe variants retain identity only while every sensor dependency matches.
#[derive(Debug, Clone)]
pub struct SensorCameraMetadata {
    identity: CameraSourceIdentity,
    frame: RawFrame,
    sensor: SensorDevelopmentDescription,
    recipe: EditRecipe,
    options: PreviewOptions,
}

impl SensorCameraMetadata {
    pub fn new(
        frame: &RawFrame,
        recipe: &EditRecipe,
        options: PreviewOptions,
    ) -> Result<Self, PipelineError> {
        validate_optics_crop(options.render.raw_crop_policy, recipe)?;
        Ok(Self {
            identity: CameraSourceIdentity::default(),
            frame: frame.clone(),
            sensor: SensorDevelopmentDescription::from_frame(frame, recipe, options.render)?,
            recipe: recipe.clone(),
            options,
        })
    }

    pub fn sensor(&self) -> &SensorDevelopmentDescription {
        &self.sensor
    }

    pub fn detail_contract(&self) -> Result<crate::DetailContract, PipelineError> {
        crate::DetailContract::new(
            &self.recipe,
            self.sensor.demosaic_balance(),
            self.sensor.white_balance_gains(),
        )
    }

    pub fn with_recipe(
        &self,
        frame: &RawFrame,
        recipe: &EditRecipe,
        options: PreviewOptions,
    ) -> Result<Self, PipelineError> {
        let mut next = Self::new(frame, recipe, options)?;
        if !Arc::ptr_eq(&self.frame.mosaic, &frame.mosaic)
            || self.frame.info != frame.info
            || self.frame.row_stride != frame.row_stride
            || self.sensor.normalization() != next.sensor.normalization()
            || self.sensor.highlight_execution() != next.sensor.highlight_execution()
            || self.sensor.demosaic() != next.sensor.demosaic()
            || crate::DetailProvenance::for_recipe(&self.recipe)
                != crate::DetailProvenance::for_recipe(&next.recipe)
            || self.detail_contract()?.ceilings() != next.detail_contract()?.ceilings()
        {
            return Err(PipelineError::InvalidRecipe {
                field: "raw",
                reason: "resident sensor source dependencies changed".into(),
            });
        }
        next.identity = self.identity.clone();
        Ok(next)
    }
}

impl CpuPipeline {
    /// Resolve optics/reduction for an independently executed sensor backend.
    /// Diagnostics and timings must describe that completed sensor operation.
    pub fn describe_sensor_completion(
        &self,
        source: &SensorCameraMetadata,
        diagnostics: HighlightDiagnostics,
        timings: StageTimings,
        cancellation: &CancellationToken,
    ) -> Result<SpatialCompletionDescription, PipelineError> {
        cancellation.checkpoint()?;
        let source_dimensions = source.sensor.normalization().crop().dimensions();
        let target_dimensions = preview_dimensions(
            source_dimensions.0,
            source_dimensions.1,
            source.options.max_long_edge,
        )?;
        let execution = crate::optics::resolve_execution(
            self.optics_service().map(AsRef::as_ref),
            &source.frame.info,
            &source.recipe.optics,
            source_dimensions.0,
            source_dimensions.1,
        )?;
        let area_reduction = crate::AreaReductionPlan::new(
            source_dimensions.0,
            source_dimensions.1,
            target_dimensions.0,
            target_dimensions.1,
        )?;
        cancellation.checkpoint()?;
        Ok(SpatialCompletionDescription {
            source_identity: source.identity.clone(),
            source_dimensions,
            target_dimensions,
            optics: execution.map_or(SpatialOptics::Off, |e| SpatialOptics::Enabled(Box::new(e))),
            area_reduction,
            calibration: source.sensor.calibration().clone(),
            source_orientation: source.sensor.source_orientation(),
            profile_selection: source.recipe.color.camera_profile.clone(),
            camera_profile: source.sensor.camera_profile_key().clone(),
            highlight_adjustments: source.sensor.highlight_adjustments(),
            highlight_white_balance: source.sensor.white_balance(),
            highlight_diagnostics: diagnostics,
            detail: crate::DetailProvenance::for_recipe(&source.recipe),
            defringe: crate::DefringeProvenance::for_recipe(&source.recipe),
            preparation_timings: timings,
            decoded_raw_bytes: source
                .frame
                .mosaic
                .len()
                .checked_mul(size_of::<u16>())
                .ok_or_else(|| dimension_overflow(source_dimensions.0, source_dimensions.1))?,
            normalized_mosaic_bytes: 0,
            highlight_scratch_bytes: 0,
        })
    }
}
