//! Full-resolution extended-range (HDR) rendition of a recipe.
//!
//! A second render of the same recipe, next to [`CpuPipeline::render_export`]: the same base,
//! geometry and resize, with the adjustment stage developing [`Rendition::Extended`]. The result
//! stays linear and unquantized, relative to diffuse white (1.0) and at most `headroom`.

use rohditor_image::{ExtendedLinearRgbImage, RgbPrimaries};

use super::*;
use crate::cpu::render_extended_linear_with_geometry;

/// An extended-range rendition and its processing diagnostics.
#[derive(Debug)]
pub struct ExtendedRenderResult {
    pub image: ExtendedLinearRgbImage<f32>,
    pub timings: StageTimings,
    pub highlight_diagnostics: HighlightDiagnostics,
    pub detail: Option<crate::DetailProvenance>,
    pub output_gamut_diagnostics: GamutMappingDiagnostics,
    pub optics_provenance: Option<OpticsProvenance>,
    pub memory: MemoryEstimate,
}

impl CpuPipeline {
    /// Render the extended-range rendition of `recipe` at full resolution, in linear `primaries`
    /// up to `headroom`.
    ///
    /// Values below [`crate::HDR_SHOULDER_KNEE`] after the base rendering match the SDR
    /// [`Self::render_export`] of the same recipe; highlights above it rise toward `headroom`
    /// instead of 1.0. Output sharpening is not applied: it works on SDR-encoded values.
    ///
    /// # Errors
    ///
    /// Fails like [`Self::render_export`], and when `headroom` is not finite or below 1.0.
    pub fn render_extended_export(
        &self,
        frame: &RawFrame,
        recipe: &EditRecipe,
        options: RenderOptions,
        primaries: RgbPrimaries,
        headroom: f32,
        size: OutputSize,
    ) -> Result<ExtendedRenderResult, PipelineError> {
        // Rejected before the expensive base preparation.
        crate::rendering::validate_headroom(headroom)?;
        let total_started = Instant::now();
        let cancellation = CancellationToken::new();
        let mut base = prepare_base(self, frame, recipe, options)?;
        let mut geometry = output_geometry(&base.image, base.source_orientation, recipe)?;
        let (output_width, output_height) = geometry.output_dimensions();
        let target = size.resolve(output_width, output_height);
        let local = LocalWork::prepare(&base, recipe)?;
        let memory = memory_estimate(
            &base,
            size_of::<f32>(),
            geometry,
            target,
            false,
            local.bytes(),
        )?;
        let adjustments_started = Instant::now();
        apply_adjustments_with_local(
            &mut base.image,
            recipe,
            local.stage.as_ref(),
            Rendition::Extended { headroom },
            &cancellation,
        )?;
        base.timings.adjustments = adjustments_started.elapsed();
        drop(local);
        base.local_detail = Vec::new();

        let output_started = Instant::now();
        let resized = if target == (output_width, output_height) {
            None
        } else {
            let resized = crop_and_resize(&base.image, geometry, target, &cancellation)?;
            geometry = OutputGeometry::new(target.0, target.1, Orientation::Normal, None)?;
            Some(resized)
        };
        let source = resized.as_ref().unwrap_or(&base.image);
        let (image, output_gamut_diagnostics) = render_extended_linear_with_geometry(
            source,
            geometry,
            primaries,
            headroom,
            options.output_policy,
            &cancellation,
        )?;
        base.timings.output_conversion = output_started.elapsed();
        base.timings.total = total_started.elapsed();

        Ok(ExtendedRenderResult {
            image,
            timings: base.timings,
            highlight_diagnostics: base.highlight_diagnostics,
            detail: base.detail,
            output_gamut_diagnostics,
            optics_provenance: base.optics_provenance,
            memory,
        })
    }
}
