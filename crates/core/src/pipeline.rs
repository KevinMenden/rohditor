use std::mem::size_of;
use std::sync::Arc;
use std::time::{Duration, Instant};

use rohditor_demosaic::{DemosaicAlgorithm, WhiteBalanceGains};
use rohditor_edit::{
    CameraProfileSelection, EditRecipe, HighlightAdjustments, HighlightMethod, WhiteBalance,
};
use rohditor_image::{DisplayRgbImage, LinearRgbImage, Orientation};
use rohditor_optics::{OpticsProvenance, OpticsService};
use rohditor_raw::RawFrame;

use crate::analysis::Histogram;
use crate::color::{
    CameraCalibration, CameraProfileKey, camera_profile_key, resolve_camera_colour,
};
use crate::cpu::{
    apply_adjustments_cancellable, apply_camera_color_transform_cancellable,
    apply_white_balance_cancellable, normalize_raw_cancellable, preview_dimensions,
    render_display_srgb8_cancellable_with_geometry,
    render_display_srgb8_dithered_with_geometry_and_diagnostics,
    render_display_srgb16_with_geometry_and_diagnostics,
};
use crate::demosaic::demosaic_cancellable;
use crate::highlight::{HighlightDiagnostics, apply_cancellable as apply_highlight_cancellable};
use crate::resample::resize_area_cancellable;
use crate::{
    CancellationToken, DitherMode, ExportImage, GamutMappingDiagnostics, OutputBitDepth,
    OutputGeometry, PipelineError, apply_adjustments,
};

/// Default longest edge of an interactively developed preview.
pub const DEFAULT_PREVIEW_LONG_EDGE: usize = 2_560;

/// Maximum estimated live CPU image buffers for one render operation.
pub const CPU_WORKING_SET_LIMIT_BYTES: usize = 2 * 1_024 * 1_024 * 1_024;

/// Sensor crop selected before normalization.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum RawCropPolicy {
    ActiveArea,
    #[default]
    Recommended,
}

/// Explicit output clipping/gamut policy.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum OutputPolicy {
    #[default]
    ClipToSrgb,
    ChromaCompressToSrgb,
}

impl OutputPolicy {
    #[must_use]
    pub const fn display_name(self) -> &'static str {
        match self {
            Self::ClipToSrgb => "Clip to sRGB",
            Self::ChromaCompressToSrgb => "Chroma compress to sRGB",
        }
    }

    #[must_use]
    pub const fn algorithm_version(self) -> Option<u16> {
        match self {
            Self::ClipToSrgb => None,
            Self::ChromaCompressToSrgb => Some(rohditor_color::CHROMA_COMPRESS_ALGORITHM_VERSION),
        }
    }
}

/// Stable options that are not edits to the image itself.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct RenderOptions {
    pub raw_crop_policy: RawCropPolicy,
    pub demosaic: DemosaicAlgorithm,
    pub output_policy: OutputPolicy,
}

/// Resolution and processing choices for an interactive CPU preview.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PreviewOptions {
    pub render: RenderOptions,
    pub max_long_edge: usize,
}

impl Default for PreviewOptions {
    fn default() -> Self {
        Self {
            render: RenderOptions::default(),
            max_long_edge: DEFAULT_PREVIEW_LONG_EDGE,
        }
    }
}

/// Wall-clock timings for each full-frame CPU stage.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct StageTimings {
    pub metadata: Duration,
    pub normalization: Duration,
    pub highlight_processing: Duration,
    /// Compatibility alias for diagnostics consumers from the Clip-only
    /// pipeline. New code should use [`Self::highlight_processing`].
    pub highlight_clipping: Duration,
    pub demosaic: Duration,
    pub optics: Duration,
    pub resampling: Duration,
    pub color_conversion: Duration,
    pub adjustments: Duration,
    pub output_conversion: Duration,
    pub total: Duration,
}

/// Deterministic buffer-size estimate; this is not an operating-system RSS reading.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct MemoryEstimate {
    pub decoded_raw_bytes: usize,
    pub normalized_mosaic_bytes: usize,
    pub highlight_scratch_bytes: usize,
    pub optics_output_bytes: usize,
    pub optics_scratch_bytes: usize,
    pub resample_intermediate_bytes: usize,
    pub linear_rgb_bytes: usize,
    pub display_rgb_bytes: usize,
    pub estimated_peak_bytes: usize,
}

/// Reduced, unbalanced camera-native RGB retained between preview base rebuilds.
///
/// The full crop has already been normalized, demosaiced, and antialiased down
/// to the fixed preview dimensions. White balance, camera color conversion, and
/// downstream edits have not been applied yet.
#[derive(Debug, Clone)]
pub struct ReconstructedPreview {
    image: LinearRgbImage<f32>,
    calibration: CameraCalibration,
    source_orientation: Orientation,
    profile_selection: CameraProfileSelection,
    camera_profile: CameraProfileKey,
    highlight_adjustments: HighlightAdjustments,
    highlight_white_balance: WhiteBalance,
    highlight_diagnostics: HighlightDiagnostics,
    optics_provenance: Option<OpticsProvenance>,
    optics_output_bytes: usize,
    optics_scratch_bytes: usize,
    timings: StageTimings,
    decoded_raw_bytes: usize,
    normalized_mosaic_bytes: usize,
    highlight_scratch_bytes: usize,
    resample_intermediate_bytes: usize,
    preparation_peak_bytes: usize,
}

impl ReconstructedPreview {
    /// Reduced unbalanced camera-native RGB samples.
    #[must_use]
    pub const fn image(&self) -> &LinearRgbImage<f32> {
        &self.image
    }

    /// Source EXIF orientation for the reconstructed camera-native image.
    #[must_use]
    pub const fn source_orientation(&self) -> Orientation {
        self.source_orientation
    }

    /// Camera calibration retained for shared CPU/GPU color resolution.
    #[must_use]
    pub const fn calibration(&self) -> &CameraCalibration {
        &self.calibration
    }

    /// Exact profile identity used while building the RAW-stage result.
    #[must_use]
    pub fn camera_profile_key(&self) -> &CameraProfileKey {
        &self.camera_profile
    }

    /// Profile selection used when this reconstruction was produced.
    #[must_use]
    pub const fn profile_selection(&self) -> &CameraProfileSelection {
        &self.profile_selection
    }

    /// Decoder as-shot multipliers used as the relative WB baseline.
    #[must_use]
    pub const fn as_shot_white_balance(&self) -> [Option<f32>; 4] {
        self.calibration.as_shot_white_balance
    }

    /// Whether changing recipe white balance can reuse this camera-native
    /// reconstruction without rebuilding the RAW-stage highlight result.
    #[must_use]
    pub fn supports_dynamic_white_balance(&self) -> bool {
        self.highlight_adjustments.method != HighlightMethod::Clip
    }

    /// White balance used to derive Clip's pre-WB channel ceilings.
    #[must_use]
    pub const fn highlight_white_balance(&self) -> WhiteBalance {
        self.highlight_white_balance
    }

    /// RAW highlight operation that produced this retained camera-native
    /// source. This provenance is part of the source identity at the GPU
    /// boundary; it must not be inferred from the current downstream recipe.
    #[must_use]
    pub const fn highlight_adjustments(&self) -> HighlightAdjustments {
        self.highlight_adjustments
    }

    #[must_use]
    pub const fn highlight_diagnostics(&self) -> HighlightDiagnostics {
        self.highlight_diagnostics
    }

    /// Profile identity and database snapshot that produced this source, when enabled.
    #[must_use]
    pub fn optics_provenance(&self) -> Option<&OpticsProvenance> {
        self.optics_provenance.as_ref()
    }

    #[must_use]
    pub const fn highlight_stats(&self) -> crate::ClipStats {
        self.highlight_diagnostics.legacy_clip_stats()
    }

    pub fn matches_highlight_recipe(&self, recipe: &EditRecipe) -> bool {
        if !highlight_adjustments_match(self.highlight_adjustments, recipe.raw.highlights) {
            return false;
        }
        if self.supports_dynamic_white_balance() {
            return true;
        }
        if self.highlight_white_balance != recipe.color.white_balance {
            return false;
        }
        !matches!(
            recipe.color.white_balance,
            WhiteBalance::TemperatureTint { .. }
        ) || self.camera_profile == camera_profile_key(&recipe.color.camera_profile)
    }

    /// Resolve a recipe white balance against this reconstruction's camera
    /// calibration without changing the immutable camera-native pixels.
    pub fn white_balance_gains(
        &self,
        selection: WhiteBalance,
    ) -> Result<WhiteBalanceGains, PipelineError> {
        resolve_camera_colour(&self.calibration, &self.profile_selection, selection)
            .map(|resolved| resolved.white_balance_gains)
    }

    #[must_use]
    pub const fn timings(&self) -> StageTimings {
        self.timings
    }

    #[must_use]
    pub const fn normalized_mosaic_bytes(&self) -> usize {
        self.normalized_mosaic_bytes
    }

    #[must_use]
    pub const fn resample_intermediate_bytes(&self) -> usize {
        self.resample_intermediate_bytes
    }

    #[must_use]
    pub const fn highlight_scratch_bytes(&self) -> usize {
        self.highlight_scratch_bytes
    }

    #[must_use]
    pub const fn optics_output_bytes(&self) -> usize {
        self.optics_output_bytes
    }

    #[must_use]
    pub const fn optics_scratch_bytes(&self) -> usize {
        self.optics_scratch_bytes
    }

    #[must_use]
    pub const fn preparation_peak_bytes(&self) -> usize {
        self.preparation_peak_bytes
    }

    /// Bytes held by the reduced camera-RGB buffer itself.
    #[must_use]
    pub fn buffer_bytes(&self) -> usize {
        self.image.data().len().saturating_mul(size_of::<f32>())
    }
}

/// A completed CPU render and its diagnostics.
#[derive(Debug)]
pub struct RenderResult {
    pub image: DisplayRgbImage<u8>,
    pub histogram: Histogram,
    pub timings: StageTimings,
    pub highlight_diagnostics: HighlightDiagnostics,
    pub output_gamut_diagnostics: GamutMappingDiagnostics,
    /// Compatibility projection for Clip-only callers.
    pub highlight_stats: crate::ClipStats,
    pub optics_provenance: Option<OpticsProvenance>,
    pub memory: MemoryEstimate,
}

/// A full-resolution export render and its processing diagnostics.
#[derive(Debug)]
pub struct ExportRenderResult {
    pub image: ExportImage,
    pub timings: StageTimings,
    pub highlight_diagnostics: HighlightDiagnostics,
    pub output_gamut_diagnostics: GamutMappingDiagnostics,
    /// Compatibility projection for Clip-only callers.
    pub highlight_stats: crate::ClipStats,
    pub optics_provenance: Option<OpticsProvenance>,
    pub memory: MemoryEstimate,
}

/// A linear Rec.2020 preview after normalization, white balance, demosaic, and
/// camera color conversion, but before base rendering and interactive adjustments.
///
/// This is the cache/upload boundary shared by the CPU reference path and the
/// Phase 5 GPU backend. Standard and Neutral consume this same scene-linear
/// base; rendering, downstream edits, orientation, and output conversion can
/// change without rebuilding it.
#[derive(Debug, Clone)]
pub struct DemosaicedBase {
    image: LinearRgbImage<f32>,
    source_orientation: Orientation,
    white_balance: WhiteBalance,
    camera_profile: CameraProfileKey,
    highlight_adjustments: HighlightAdjustments,
    highlight_white_balance: WhiteBalance,
    highlight_diagnostics: HighlightDiagnostics,
    optics_provenance: Option<OpticsProvenance>,
    optics_output_bytes: usize,
    optics_scratch_bytes: usize,
    timings: StageTimings,
    decoded_raw_bytes: usize,
    normalized_mosaic_bytes: usize,
    highlight_scratch_bytes: usize,
    resample_intermediate_bytes: usize,
    preparation_peak_bytes: usize,
}

impl DemosaicedBase {
    /// Scene-linear Rec.2020/D65 samples suitable for upload to a processor.
    #[must_use]
    pub const fn image(&self) -> &LinearRgbImage<f32> {
        &self.image
    }

    #[must_use]
    pub const fn source_orientation(&self) -> Orientation {
        self.source_orientation
    }

    #[must_use]
    pub const fn white_balance(&self) -> WhiteBalance {
        self.white_balance
    }

    #[must_use]
    pub fn camera_profile_key(&self) -> &CameraProfileKey {
        &self.camera_profile
    }

    #[must_use]
    pub const fn highlight_stats(&self) -> crate::ClipStats {
        self.highlight_diagnostics.legacy_clip_stats()
    }

    #[must_use]
    pub const fn highlight_diagnostics(&self) -> HighlightDiagnostics {
        self.highlight_diagnostics
    }

    /// Profile identity and database snapshot that produced this base, when enabled.
    #[must_use]
    pub fn optics_provenance(&self) -> Option<&OpticsProvenance> {
        self.optics_provenance.as_ref()
    }

    /// RAW highlight operation that produced this linear preview base.
    #[must_use]
    pub const fn highlight_adjustments(&self) -> HighlightAdjustments {
        self.highlight_adjustments
    }

    #[must_use]
    pub const fn timings(&self) -> StageTimings {
        self.timings
    }

    #[must_use]
    pub const fn normalized_mosaic_bytes(&self) -> usize {
        self.normalized_mosaic_bytes
    }

    #[must_use]
    pub const fn resample_intermediate_bytes(&self) -> usize {
        self.resample_intermediate_bytes
    }

    #[must_use]
    pub const fn highlight_scratch_bytes(&self) -> usize {
        self.highlight_scratch_bytes
    }

    #[must_use]
    pub const fn optics_output_bytes(&self) -> usize {
        self.optics_output_bytes
    }

    #[must_use]
    pub const fn optics_scratch_bytes(&self) -> usize {
        self.optics_scratch_bytes
    }

    #[must_use]
    pub const fn preparation_peak_bytes(&self) -> usize {
        self.preparation_peak_bytes
    }

    /// Bytes held by the scene-linear RGB image buffer itself.
    #[must_use]
    pub fn buffer_bytes(&self) -> usize {
        self.image.data().len().saturating_mul(size_of::<f32>())
    }
}

/// Reusable scene-linear working buffer for cached CPU preview adjustments.
///
/// Each edit still copies the immutable base pixels into this buffer, but it no
/// longer allocates another full `f32` RGB image for every slider revision.
#[derive(Debug, Default)]
pub struct CpuPreviewWorkspace {
    image: Option<LinearRgbImage<f32>>,
}

impl CpuPreviewWorkspace {
    /// Whether the current allocation can be overwritten for this base.
    #[must_use]
    pub fn can_reuse(&self, base: &DemosaicedBase) -> bool {
        self.image.as_ref().is_some_and(|image| {
            image.width() == base.image.width()
                && image.height() == base.image.height()
                && image.row_stride() == base.image.row_stride()
                && image.data().len() == base.image.data().len()
        })
    }

    /// Bytes held by the reusable scene-linear allocation.
    #[must_use]
    pub fn buffer_bytes(&self) -> usize {
        self.image.as_ref().map_or(0, |image| {
            image.data().len().saturating_mul(size_of::<f32>())
        })
    }

    fn reset_from(&mut self, base: &DemosaicedBase) -> &mut LinearRgbImage<f32> {
        if self.can_reuse(base) {
            if let Some(image) = self.image.as_mut() {
                image.data_mut().copy_from_slice(base.image.data());
                image.set_space(base.image.space());
            }
        } else {
            self.image = Some(base.image.clone());
        }
        match self.image.as_mut() {
            Some(image) => image,
            None => unreachable!("the workspace always contains an image after reset"),
        }
    }
}

/// Deterministic, headless CPU implementation of the Phase 2 reference pipeline.
#[derive(Debug, Clone)]
pub struct CpuPipeline {
    optics: Option<Arc<OpticsService>>,
}

impl Default for CpuPipeline {
    fn default() -> Self {
        Self::without_optics()
    }
}

impl CpuPipeline {
    /// Construct a pipeline with one immutable optics database snapshot.
    #[must_use]
    pub fn new(optics: Arc<OpticsService>) -> Self {
        Self {
            optics: Some(optics),
        }
    }

    /// Construct a pipeline for recipes whose optics profile is off.
    #[must_use]
    pub const fn without_optics() -> Self {
        Self { optics: None }
    }

    /// The shared optics service, when this processor has one.
    #[must_use]
    pub fn optics_service(&self) -> Option<&Arc<OpticsService>> {
        self.optics.as_ref()
    }

    /// Resolve the plan identity used by a reconstructed camera-native cache
    /// entry without allocating or processing image pixels.
    #[must_use]
    pub fn optics_cache_provenance(
        &self,
        frame: &RawFrame,
        recipe: &EditRecipe,
        options: RenderOptions,
    ) -> Option<OpticsProvenance> {
        crate::optics::cache_provenance(self.optics.as_deref(), frame, recipe, options)
    }

    pub fn render(
        &self,
        frame: &RawFrame,
        recipe: &EditRecipe,
        options: RenderOptions,
    ) -> Result<RenderResult, PipelineError> {
        let total_started = Instant::now();
        let base = prepare_base(self.optics.as_deref(), frame, recipe, options)?;
        let mut result = render_base(base, recipe, options.output_policy)?;
        result.timings.total = total_started.elapsed();
        Ok(result)
    }

    /// Build the stable linear base for an interactive preview.
    ///
    /// Only the recipe's white balance participates in the resulting pixels;
    /// downstream edits are validated but deliberately left for
    /// [`Self::render_preview_from_base`].
    ///
    /// # Errors
    ///
    /// Returns [`PipelineError`] for invalid metadata, recipes, dimensions,
    /// color transforms, allocation failures, or working-set limits.
    pub fn prepare_preview_base(
        &self,
        frame: &RawFrame,
        recipe: &EditRecipe,
        options: PreviewOptions,
    ) -> Result<DemosaicedBase, PipelineError> {
        self.prepare_preview_base_cancellable(frame, recipe, options, &CancellationToken::new())
    }

    /// Build the preview base while observing a cooperative cancellation token.
    pub fn prepare_preview_base_cancellable(
        &self,
        frame: &RawFrame,
        recipe: &EditRecipe,
        options: PreviewOptions,
        cancellation: &CancellationToken,
    ) -> Result<DemosaicedBase, PipelineError> {
        let reconstructed =
            self.prepare_preview_reconstruction_cancellable(frame, recipe, options, cancellation)?;
        let mut base = self.prepare_preview_base_from_reconstruction_cancellable(
            &reconstructed,
            recipe,
            cancellation,
        )?;
        base.timings.metadata += reconstructed.timings.metadata;
        base.timings.normalization = reconstructed.timings.normalization;
        base.timings.highlight_processing = reconstructed.timings.highlight_processing;
        base.timings.highlight_clipping = reconstructed.timings.highlight_clipping;
        base.timings.demosaic = reconstructed.timings.demosaic;
        base.timings.optics = reconstructed.timings.optics;
        base.timings.resampling = reconstructed.timings.resampling;
        base.timings.total += reconstructed.timings.total;
        Ok(base)
    }

    /// Reconstruct and antialias a camera-native preview base for reuse across
    /// white-balance and downstream edit changes.
    pub fn prepare_preview_reconstruction(
        &self,
        frame: &RawFrame,
        recipe: &EditRecipe,
        options: PreviewOptions,
    ) -> Result<ReconstructedPreview, PipelineError> {
        self.prepare_preview_reconstruction_cancellable(
            frame,
            recipe,
            options,
            &CancellationToken::new(),
        )
    }

    /// Cancellable form of [`Self::prepare_preview_reconstruction`].
    pub fn prepare_preview_reconstruction_cancellable(
        &self,
        frame: &RawFrame,
        recipe: &EditRecipe,
        options: PreviewOptions,
        cancellation: &CancellationToken,
    ) -> Result<ReconstructedPreview, PipelineError> {
        prepare_reconstructed_preview(self.optics.as_deref(), frame, recipe, options, cancellation)
    }

    /// Apply white balance and camera color conversion to a retained
    /// reconstructed preview.
    pub fn prepare_preview_base_from_reconstruction(
        &self,
        reconstructed: &ReconstructedPreview,
        recipe: &EditRecipe,
    ) -> Result<DemosaicedBase, PipelineError> {
        self.prepare_preview_base_from_reconstruction_cancellable(
            reconstructed,
            recipe,
            &CancellationToken::new(),
        )
    }

    /// Cancellable form of [`Self::prepare_preview_base_from_reconstruction`].
    pub fn prepare_preview_base_from_reconstruction_cancellable(
        &self,
        reconstructed: &ReconstructedPreview,
        recipe: &EditRecipe,
        cancellation: &CancellationToken,
    ) -> Result<DemosaicedBase, PipelineError> {
        prepare_demosaiced_preview(reconstructed, recipe, cancellation)
    }

    /// Apply downstream edits and output conversion to a reusable preview base.
    ///
    /// # Errors
    ///
    /// Returns [`PipelineError`] when the recipe is invalid, its white balance
    /// does not match the base, output conversion fails, or the retained-base
    /// working set exceeds the configured limit.
    pub fn render_preview_from_base(
        &self,
        base: &DemosaicedBase,
        recipe: &EditRecipe,
        output_policy: OutputPolicy,
    ) -> Result<RenderResult, PipelineError> {
        self.render_preview_from_base_reusing(
            base,
            recipe,
            output_policy,
            &mut CpuPreviewWorkspace::default(),
        )
    }

    /// Apply downstream edits using a retained scene-linear working allocation.
    pub fn render_preview_from_base_reusing(
        &self,
        base: &DemosaicedBase,
        recipe: &EditRecipe,
        output_policy: OutputPolicy,
        workspace: &mut CpuPreviewWorkspace,
    ) -> Result<RenderResult, PipelineError> {
        self.render_preview_from_base_reusing_cancellable(
            base,
            recipe,
            output_policy,
            workspace,
            &CancellationToken::new(),
        )
    }

    /// Cancellable form of [`Self::render_preview_from_base_reusing`].
    pub fn render_preview_from_base_reusing_cancellable(
        &self,
        base: &DemosaicedBase,
        recipe: &EditRecipe,
        output_policy: OutputPolicy,
        workspace: &mut CpuPreviewWorkspace,
        cancellation: &CancellationToken,
    ) -> Result<RenderResult, PipelineError> {
        let total_started = Instant::now();
        cancellation.checkpoint()?;
        validate_base_recipe(base, recipe)?;
        let retained_base_bytes = base
            .image
            .data()
            .len()
            .checked_mul(size_of::<f32>())
            .ok_or_else(|| dimension_overflow(base.image.width(), base.image.height()))?;
        let geometry = output_geometry(base.image(), base.source_orientation, recipe)?;
        let mut memory = memory_estimate(base, size_of::<u8>(), geometry)?;
        let retained_peak = base
            .decoded_raw_bytes
            .checked_add(retained_base_bytes)
            .and_then(|bytes| bytes.checked_add(retained_base_bytes))
            .and_then(|bytes| bytes.checked_add(memory.display_rgb_bytes))
            .ok_or_else(|| dimension_overflow(base.image.width(), base.image.height()))?;
        memory.estimated_peak_bytes = memory.estimated_peak_bytes.max(retained_peak);
        validate_working_set(memory.estimated_peak_bytes)?;

        let working = workspace.reset_from(base);
        cancellation.checkpoint()?;
        let mut timings = StageTimings::default();
        let adjustments_started = Instant::now();
        apply_adjustments_cancellable(working, recipe, cancellation)?;
        timings.adjustments = adjustments_started.elapsed();

        let output_started = Instant::now();
        let (image, output_gamut_diagnostics) = render_display_srgb8_cancellable_with_geometry(
            working,
            geometry,
            output_policy,
            cancellation,
        )?;
        let histogram = Histogram::from_display_rgb8(&image);
        timings.output_conversion = output_started.elapsed();
        timings.total = total_started.elapsed();

        Ok(RenderResult {
            image,
            histogram,
            timings,
            highlight_diagnostics: base.highlight_diagnostics,
            output_gamut_diagnostics,
            highlight_stats: base.highlight_stats(),
            optics_provenance: base.optics_provenance.clone(),
            memory,
        })
    }

    /// Render an sRGB8 preview after full-crop demosaic and antialiased linear
    /// reduction.
    pub fn render_preview(
        &self,
        frame: &RawFrame,
        recipe: &EditRecipe,
        options: PreviewOptions,
    ) -> Result<RenderResult, PipelineError> {
        let total_started = Instant::now();
        let base = self.prepare_preview_base(frame, recipe, options)?;
        let mut result = render_base(base, recipe, options.render.output_policy)?;
        result.timings.total = total_started.elapsed();
        Ok(result)
    }

    /// Render a cancellable full-resolution 8-bit display image for temporary
    /// one-source-pixel inspection in the desktop viewport.
    ///
    /// Unlike the retained preview-base path, this mutates one full-resolution
    /// linear buffer in place and releases it after output conversion. This
    /// keeps source-scale inspection within the Phase 9 transient-memory
    /// budget without making a 24 MP linear cache resident.
    pub fn render_source_scale_preview_cancellable(
        &self,
        frame: &RawFrame,
        recipe: &EditRecipe,
        options: RenderOptions,
        cancellation: &CancellationToken,
    ) -> Result<RenderResult, PipelineError> {
        let total_started = Instant::now();
        let mut base =
            prepare_base_cancellable(self.optics.as_deref(), frame, recipe, options, cancellation)?;
        let geometry = output_geometry(&base.image, base.source_orientation, recipe)?;
        let memory = memory_estimate(&base, size_of::<u8>(), geometry)?;
        let adjustments_started = Instant::now();
        apply_adjustments_cancellable(&mut base.image, recipe, cancellation)?;
        base.timings.adjustments = adjustments_started.elapsed();
        let output_started = Instant::now();
        let (image, output_gamut_diagnostics) = render_display_srgb8_cancellable_with_geometry(
            &base.image,
            geometry,
            options.output_policy,
            cancellation,
        )?;
        let histogram = Histogram::from_display_rgb8(&image);
        base.timings.output_conversion = output_started.elapsed();
        base.timings.total = total_started.elapsed();
        Ok(RenderResult {
            image,
            histogram,
            timings: base.timings,
            highlight_diagnostics: base.highlight_diagnostics,
            output_gamut_diagnostics,
            highlight_stats: base.highlight_stats(),
            optics_provenance: base.optics_provenance.clone(),
            memory,
        })
    }

    /// Render full-resolution output samples for a subsequent file export.
    pub fn render_export(
        &self,
        frame: &RawFrame,
        recipe: &EditRecipe,
        options: RenderOptions,
        bit_depth: OutputBitDepth,
        dithering: DitherMode,
    ) -> Result<ExportRenderResult, PipelineError> {
        let total_started = Instant::now();
        let mut base = prepare_base(self.optics.as_deref(), frame, recipe, options)?;
        let geometry = output_geometry(&base.image, base.source_orientation, recipe)?;
        let memory = memory_estimate(&base, bit_depth.bytes_per_sample(), geometry)?;
        let adjustments_started = Instant::now();
        apply_adjustments(&mut base.image, recipe)?;
        base.timings.adjustments = adjustments_started.elapsed();
        let output_started = Instant::now();
        let (image, output_gamut_diagnostics) = match bit_depth {
            OutputBitDepth::Eight => {
                let (image, diagnostics) =
                    render_display_srgb8_dithered_with_geometry_and_diagnostics(
                        &base.image,
                        geometry,
                        options.output_policy,
                        dithering,
                    )?;
                (ExportImage::Rgb8(image), diagnostics)
            }
            OutputBitDepth::Sixteen => {
                let (image, diagnostics) = render_display_srgb16_with_geometry_and_diagnostics(
                    &base.image,
                    geometry,
                    options.output_policy,
                    dithering,
                )?;
                (ExportImage::Rgb16(image), diagnostics)
            }
        };
        base.timings.output_conversion = output_started.elapsed();
        base.timings.total = total_started.elapsed();

        Ok(ExportRenderResult {
            image,
            timings: base.timings,
            highlight_diagnostics: base.highlight_diagnostics,
            output_gamut_diagnostics,
            highlight_stats: base.highlight_stats(),
            optics_provenance: base.optics_provenance,
            memory,
        })
    }
}

fn prepare_reconstructed_preview(
    optics: Option<&OpticsService>,
    frame: &RawFrame,
    recipe: &EditRecipe,
    options: PreviewOptions,
    cancellation: &CancellationToken,
) -> Result<ReconstructedPreview, PipelineError> {
    let total_started = Instant::now();
    cancellation.checkpoint()?;
    validate_preview_working_set(
        frame,
        options.max_long_edge,
        recipe.raw.highlights.method,
        crate::optics::optics_enabled(&recipe.optics),
    )?;

    let metadata_started = Instant::now();
    let metadata_span = tracing::info_span!(
        "cpu.metadata",
        width = frame.info.width,
        height = frame.info.height,
        purpose = "preview reconstruction"
    );
    let metadata_guard = metadata_span.enter();
    recipe.validate()?;
    validate_optics_crop(options.render.raw_crop_policy, recipe)?;
    let calibration = CameraCalibration::from_raw_info(&frame.info);
    let resolved = resolve_camera_colour(
        &calibration,
        &recipe.color.camera_profile,
        recipe.color.white_balance,
    )?;
    let highlight_gains = (recipe.raw.highlights.method == HighlightMethod::Clip)
        .then_some(resolved.white_balance_gains);
    let metadata = metadata_started.elapsed();
    drop(metadata_guard);

    let normalization_started = Instant::now();
    let mosaic = normalize_raw_cancellable(frame, options.render.raw_crop_policy, cancellation)?;
    let normalization = normalization_started.elapsed();
    let decoded_raw_bytes = frame
        .mosaic
        .len()
        .checked_mul(size_of::<u16>())
        .ok_or_else(|| dimension_overflow(frame.info.width, frame.info.height))?;
    let source_width = mosaic.width();
    let source_height = mosaic.height();
    let normalized_mosaic_bytes = mosaic
        .data()
        .len()
        .checked_mul(size_of::<f32>())
        .ok_or_else(|| dimension_overflow(source_width, source_height))?;

    let highlight_started = Instant::now();
    let highlighted = apply_highlight_cancellable(
        mosaic,
        recipe.raw.highlights,
        highlight_gains.unwrap_or(WhiteBalanceGains::identity()),
        cancellation,
    )?;
    let highlight_processing = highlight_started.elapsed();
    let highlight_diagnostics = highlighted.diagnostics;
    let highlight_scratch_bytes = estimated_highlight_scratch_bytes(
        recipe.raw.highlights.method,
        highlight_diagnostics,
        source_width,
        source_height,
    )?;
    let mosaic = highlighted.mosaic;

    let demosaic_started = Instant::now();
    let full_linear = demosaic_cancellable(
        &mosaic,
        WhiteBalanceGains::identity(),
        options.render.demosaic,
        cancellation,
    )?;
    let demosaic = demosaic_started.elapsed();
    drop(mosaic);

    let optics_started = Instant::now();
    let optics_result = crate::optics::apply_cancellable(
        optics,
        &frame.info,
        &recipe.optics,
        full_linear,
        cancellation,
    )?;
    let optics = optics_result.image;
    let optics_provenance = optics_result.provenance;
    let optics_output_bytes = optics_result.output_bytes;
    let optics_scratch_bytes = optics_result.scratch_bytes;
    let optics_timing = optics_started.elapsed();

    let (target_width, target_height) =
        preview_dimensions(source_width, source_height, options.max_long_edge)?;
    let unchanged_dimensions = source_width == target_width && source_height == target_height;
    let resample_intermediate_bytes = if unchanged_dimensions {
        0
    } else {
        target_width
            .checked_mul(source_height)
            .and_then(|pixels| pixels.checked_mul(3 * size_of::<f32>()))
            .ok_or_else(|| dimension_overflow(target_width, source_height))?
    };
    let reduced_linear_bytes = target_width
        .checked_mul(target_height)
        .and_then(|pixels| pixels.checked_mul(3 * size_of::<f32>()))
        .ok_or_else(|| dimension_overflow(target_width, target_height))?;
    let full_linear_bytes = source_width
        .checked_mul(source_height)
        .and_then(|pixels| pixels.checked_mul(3 * size_of::<f32>()))
        .ok_or_else(|| dimension_overflow(source_width, source_height))?;
    let preparation_peak_bytes = preview_preparation_peak(PreviewPreparationInputs {
        decoded_raw_bytes,
        normalized_mosaic_bytes,
        full_linear_bytes,
        highlight_scratch_bytes,
        optics_output_bytes,
        optics_scratch_bytes,
        resample_intermediate_bytes,
        reduced_linear_bytes,
        unchanged_dimensions,
    })?;
    validate_working_set(preparation_peak_bytes)?;

    let resampling_started = Instant::now();
    let image = resize_area_cancellable(optics, target_width, target_height, cancellation)?;
    let resampling = resampling_started.elapsed();
    let timings = StageTimings {
        metadata,
        normalization,
        highlight_processing,
        highlight_clipping: highlight_processing,
        demosaic,
        optics: optics_timing,
        resampling,
        total: total_started.elapsed(),
        ..StageTimings::default()
    };

    Ok(ReconstructedPreview {
        image,
        calibration,
        source_orientation: frame.info.orientation,
        profile_selection: recipe.color.camera_profile.clone(),
        camera_profile: camera_profile_key(&recipe.color.camera_profile),
        highlight_adjustments: recipe.raw.highlights,
        highlight_white_balance: recipe.color.white_balance,
        highlight_diagnostics,
        optics_provenance,
        optics_output_bytes,
        optics_scratch_bytes,
        timings,
        decoded_raw_bytes,
        normalized_mosaic_bytes,
        highlight_scratch_bytes,
        resample_intermediate_bytes,
        preparation_peak_bytes,
    })
}

fn prepare_demosaiced_preview(
    reconstructed: &ReconstructedPreview,
    recipe: &EditRecipe,
    cancellation: &CancellationToken,
) -> Result<DemosaicedBase, PipelineError> {
    let total_started = Instant::now();
    cancellation.checkpoint()?;
    let metadata_started = Instant::now();
    let metadata_span = tracing::info_span!(
        "cpu.metadata",
        width = reconstructed.image.width(),
        height = reconstructed.image.height(),
        purpose = "preview white balance"
    );
    let metadata_guard = metadata_span.enter();
    recipe.validate()?;
    if !reconstructed.matches_highlight_recipe(recipe) {
        return Err(PipelineError::InvalidRecipe {
            field: "raw.highlights",
            reason: "the recipe does not match the RAW highlight result used to build the reconstruction"
                .to_owned(),
        });
    }
    if !crate::optics::matches_recipe(reconstructed.optics_provenance(), &recipe.optics) {
        return Err(PipelineError::InvalidRecipe {
            field: "optics",
            reason: "the recipe does not match the optics result used to build the reconstruction"
                .to_owned(),
        });
    }
    let resolved = resolve_camera_colour(
        &reconstructed.calibration,
        &recipe.color.camera_profile,
        recipe.color.white_balance,
    )?;
    let metadata = metadata_started.elapsed();
    drop(metadata_guard);

    let color_started = Instant::now();
    let mut image = reconstructed.image.clone();
    apply_white_balance_cancellable(&mut image, resolved.white_balance_gains, cancellation)?;
    apply_camera_color_transform_cancellable(
        &mut image,
        &resolved.camera_color_transform(),
        cancellation,
    )?;
    let color_conversion = color_started.elapsed();
    let timings = StageTimings {
        metadata,
        optics: reconstructed.timings.optics,
        color_conversion,
        total: total_started.elapsed(),
        ..StageTimings::default()
    };

    Ok(DemosaicedBase {
        image,
        source_orientation: reconstructed.source_orientation,
        white_balance: recipe.color.white_balance,
        camera_profile: camera_profile_key(&recipe.color.camera_profile),
        highlight_adjustments: reconstructed.highlight_adjustments,
        highlight_white_balance: reconstructed.highlight_white_balance,
        highlight_diagnostics: reconstructed.highlight_diagnostics,
        optics_provenance: reconstructed.optics_provenance.clone(),
        optics_output_bytes: reconstructed.optics_output_bytes,
        optics_scratch_bytes: reconstructed.optics_scratch_bytes,
        timings,
        decoded_raw_bytes: reconstructed.decoded_raw_bytes,
        normalized_mosaic_bytes: reconstructed.normalized_mosaic_bytes,
        highlight_scratch_bytes: reconstructed.highlight_scratch_bytes,
        resample_intermediate_bytes: reconstructed.resample_intermediate_bytes,
        preparation_peak_bytes: reconstructed.preparation_peak_bytes,
    })
}

fn prepare_base(
    optics: Option<&OpticsService>,
    frame: &RawFrame,
    recipe: &EditRecipe,
    options: RenderOptions,
) -> Result<DemosaicedBase, PipelineError> {
    prepare_base_cancellable(optics, frame, recipe, options, &CancellationToken::new())
}

fn prepare_base_cancellable(
    optics: Option<&OpticsService>,
    frame: &RawFrame,
    recipe: &EditRecipe,
    options: RenderOptions,
    cancellation: &CancellationToken,
) -> Result<DemosaicedBase, PipelineError> {
    let total_started = Instant::now();
    cancellation.checkpoint()?;
    validate_base_working_set(
        frame,
        recipe.raw.highlights.method,
        crate::optics::optics_enabled(&recipe.optics),
    )?;
    let metadata_started = Instant::now();
    let metadata_span = tracing::info_span!(
        "cpu.metadata",
        width = frame.info.width,
        height = frame.info.height,
        purpose = "full pipeline base"
    );
    let metadata_guard = metadata_span.enter();
    recipe.validate()?;
    validate_optics_crop(options.raw_crop_policy, recipe)?;
    let calibration = CameraCalibration::from_raw_info(&frame.info);
    let resolved = resolve_camera_colour(
        &calibration,
        &recipe.color.camera_profile,
        recipe.color.white_balance,
    )?;
    let gains = resolved.white_balance_gains;
    let metadata = metadata_started.elapsed();
    drop(metadata_guard);

    let normalization_started = Instant::now();
    let normalized = normalize_raw_cancellable(frame, options.raw_crop_policy, cancellation)?;
    let normalization = normalization_started.elapsed();
    let normalized_mosaic_bytes = normalized
        .data()
        .len()
        .checked_mul(size_of::<f32>())
        .ok_or_else(|| dimension_overflow(normalized.width(), normalized.height()))?;
    let normalized_width = normalized.width();
    let normalized_height = normalized.height();

    let highlight_started = Instant::now();
    let highlighted =
        apply_highlight_cancellable(normalized, recipe.raw.highlights, gains, cancellation)?;
    let highlight_processing = highlight_started.elapsed();
    let highlight_diagnostics = highlighted.diagnostics;
    let highlight_scratch_bytes = estimated_highlight_scratch_bytes(
        recipe.raw.highlights.method,
        highlight_diagnostics,
        normalized_width,
        normalized_height,
    )?;
    let normalized = highlighted.mosaic;
    let optics_enabled = crate::optics::optics_enabled(&recipe.optics);

    let demosaic_started = Instant::now();
    let mut linear = demosaic_cancellable(
        &normalized,
        if optics_enabled {
            WhiteBalanceGains::identity()
        } else {
            gains
        },
        options.demosaic,
        cancellation,
    )?;
    let demosaic = demosaic_started.elapsed();
    drop(normalized);

    let optics_started = Instant::now();
    let optics_result = crate::optics::apply_cancellable(
        optics,
        &frame.info,
        &recipe.optics,
        linear,
        cancellation,
    )?;
    linear = optics_result.image;
    let optics_provenance = optics_result.provenance;
    let optics_output_bytes = optics_result.output_bytes;
    let optics_scratch_bytes = optics_result.scratch_bytes;
    let optics_timing = optics_started.elapsed();

    let color_started = Instant::now();
    if optics_enabled {
        apply_white_balance_cancellable(&mut linear, gains, cancellation)?;
    }
    apply_camera_color_transform_cancellable(
        &mut linear,
        &resolved.camera_color_transform(),
        cancellation,
    )?;
    let color_conversion = color_started.elapsed();

    let decoded_raw_bytes = frame
        .mosaic
        .len()
        .checked_mul(size_of::<u16>())
        .ok_or_else(|| dimension_overflow(frame.info.width, frame.info.height))?;
    let linear_rgb_bytes = linear
        .data()
        .len()
        .checked_mul(size_of::<f32>())
        .ok_or_else(|| dimension_overflow(linear.width(), linear.height()))?;
    let highlight_peak_bytes = decoded_raw_bytes
        .checked_add(normalized_mosaic_bytes)
        .and_then(|bytes| bytes.checked_add(highlight_scratch_bytes))
        .ok_or_else(|| dimension_overflow(linear.width(), linear.height()))?;
    let preparation_peak_bytes = decoded_raw_bytes
        .checked_add(normalized_mosaic_bytes)
        .and_then(|bytes| bytes.checked_add(linear_rgb_bytes))
        .and_then(|bytes| bytes.checked_add(optics_output_bytes))
        .and_then(|bytes| bytes.checked_add(optics_scratch_bytes))
        .ok_or_else(|| dimension_overflow(linear.width(), linear.height()))?
        .max(highlight_peak_bytes);

    let mut timings = StageTimings {
        metadata,
        normalization,
        highlight_processing,
        highlight_clipping: highlight_processing,
        demosaic,
        optics: optics_timing,
        color_conversion,
        ..StageTimings::default()
    };
    timings.total = total_started.elapsed();

    Ok(DemosaicedBase {
        image: linear,
        source_orientation: frame.info.orientation,
        white_balance: recipe.color.white_balance,
        camera_profile: camera_profile_key(&recipe.color.camera_profile),
        highlight_adjustments: recipe.raw.highlights,
        highlight_white_balance: recipe.color.white_balance,
        highlight_diagnostics,
        optics_provenance,
        optics_output_bytes,
        optics_scratch_bytes,
        timings,
        decoded_raw_bytes,
        normalized_mosaic_bytes,
        highlight_scratch_bytes,
        resample_intermediate_bytes: 0,
        preparation_peak_bytes,
    })
}

fn validate_base_recipe(base: &DemosaicedBase, recipe: &EditRecipe) -> Result<(), PipelineError> {
    recipe.validate()?;
    if !highlight_adjustments_match(base.highlight_adjustments, recipe.raw.highlights) {
        Err(PipelineError::InvalidRecipe {
            field: "raw.highlights",
            reason: "the recipe does not match the RAW highlight result used to build the demosaiced base"
                .to_owned(),
        })
    } else if !crate::optics::matches_recipe(base.optics_provenance(), &recipe.optics) {
        Err(PipelineError::InvalidRecipe {
            field: "optics",
            reason: "the recipe does not match the optics result used to build the base".to_owned(),
        })
    } else if base.camera_profile != camera_profile_key(&recipe.color.camera_profile) {
        Err(PipelineError::InvalidRecipe {
            field: "color.camera_profile",
            reason:
                "the recipe does not match the camera profile used to build the demosaiced base"
                    .to_owned(),
        })
    } else if recipe.color.white_balance == base.white_balance
        && (base.highlight_adjustments.method != HighlightMethod::Clip
            || recipe.color.white_balance == base.highlight_white_balance)
    {
        Ok(())
    } else {
        Err(PipelineError::InvalidRecipe {
            field: "white_balance",
            reason: "the recipe does not match the white balance used to build the demosaiced base"
                .to_owned(),
        })
    }
}

fn highlight_adjustments_match(
    retained: HighlightAdjustments,
    requested: HighlightAdjustments,
) -> bool {
    if retained.method != requested.method {
        return false;
    }
    match requested.method {
        HighlightMethod::Off => true,
        HighlightMethod::Clip => {
            retained.clip.threshold.to_bits() == requested.clip.threshold.to_bits()
        }
        HighlightMethod::LocalRatios => {
            retained.local_ratios.detection_threshold.to_bits()
                == requested.local_ratios.detection_threshold.to_bits()
        }
        HighlightMethod::Opposed => {
            retained.opposed.detection_threshold.to_bits()
                == requested.opposed.detection_threshold.to_bits()
        }
    }
}

fn estimated_highlight_scratch_bytes(
    method: HighlightMethod,
    diagnostics: HighlightDiagnostics,
    width: usize,
    height: usize,
) -> Result<usize, PipelineError> {
    let needs_scratch = match diagnostics {
        HighlightDiagnostics::LocalRatios(stats) if method == HighlightMethod::LocalRatios => {
            stats.suspected_clipped_sites > 0
        }
        HighlightDiagnostics::Opposed(stats) if method == HighlightMethod::Opposed => {
            stats.suspected_clipped_sites > 0
        }
        _ => false,
    };
    if !needs_scratch {
        return Ok(0);
    }
    let scratch = match method {
        HighlightMethod::LocalRatios => rohditor_highlight::local_ratio_scratch_bytes,
        HighlightMethod::Opposed => rohditor_highlight::opposed_scratch_bytes,
        HighlightMethod::Off | HighlightMethod::Clip => unreachable!("scratch is not needed"),
    };
    scratch(width, height).ok_or_else(|| dimension_overflow(width, height))
}

fn render_base(
    mut base: DemosaicedBase,
    recipe: &EditRecipe,
    output_policy: OutputPolicy,
) -> Result<RenderResult, PipelineError> {
    validate_base_recipe(&base, recipe)?;
    let geometry = output_geometry(&base.image, base.source_orientation, recipe)?;
    let memory = memory_estimate(&base, size_of::<u8>(), geometry)?;

    let adjustments_started = Instant::now();
    apply_adjustments(&mut base.image, recipe)?;
    base.timings.adjustments = adjustments_started.elapsed();

    let output_started = Instant::now();
    let (image, output_gamut_diagnostics) =
        render_display_srgb8_dithered_with_geometry_and_diagnostics(
            &base.image,
            geometry,
            output_policy,
            DitherMode::None,
        )?;
    base.timings.output_conversion = output_started.elapsed();
    base.timings.total = base.timings.metadata
        + base.timings.normalization
        + base.timings.highlight_processing
        + base.timings.demosaic
        + base.timings.optics
        + base.timings.resampling
        + base.timings.color_conversion
        + base.timings.adjustments
        + base.timings.output_conversion;

    Ok(RenderResult {
        histogram: Histogram::from_display_rgb8(&image),
        image,
        timings: base.timings,
        highlight_diagnostics: base.highlight_diagnostics,
        output_gamut_diagnostics,
        highlight_stats: base.highlight_stats(),
        optics_provenance: base.optics_provenance.clone(),
        memory,
    })
}

fn memory_estimate(
    base: &DemosaicedBase,
    display_sample_bytes: usize,
    output_geometry: OutputGeometry,
) -> Result<MemoryEstimate, PipelineError> {
    let width = base.image.width();
    let height = base.image.height();
    let pixels = width
        .checked_mul(height)
        .ok_or_else(|| dimension_overflow(width, height))?;
    let decoded_raw_bytes = base.decoded_raw_bytes;
    let normalized_mosaic_bytes = base.normalized_mosaic_bytes;
    let resample_intermediate_bytes = base.resample_intermediate_bytes;
    let linear_rgb_bytes = pixels
        .checked_mul(3)
        .and_then(|elements| elements.checked_mul(size_of::<f32>()))
        .ok_or_else(|| dimension_overflow(width, height))?;
    let (output_width, output_height) = output_geometry.output_dimensions();
    let output_pixels = output_width
        .checked_mul(output_height)
        .ok_or_else(|| dimension_overflow(output_width, output_height))?;
    let display_rgb_bytes = output_pixels
        .checked_mul(3)
        .and_then(|elements| elements.checked_mul(display_sample_bytes))
        .ok_or_else(|| dimension_overflow(width, height))?;
    let output_peak = decoded_raw_bytes
        .checked_add(linear_rgb_bytes)
        .and_then(|bytes| bytes.checked_add(display_rgb_bytes))
        .ok_or_else(|| dimension_overflow(width, height))?;

    let estimate = MemoryEstimate {
        decoded_raw_bytes,
        normalized_mosaic_bytes,
        highlight_scratch_bytes: base.highlight_scratch_bytes,
        optics_output_bytes: base.optics_output_bytes,
        optics_scratch_bytes: base.optics_scratch_bytes,
        resample_intermediate_bytes,
        linear_rgb_bytes,
        display_rgb_bytes,
        estimated_peak_bytes: base.preparation_peak_bytes.max(output_peak),
    };
    validate_working_set(estimate.estimated_peak_bytes)?;
    Ok(estimate)
}

fn output_geometry(
    image: &LinearRgbImage<f32>,
    source_orientation: Orientation,
    recipe: &EditRecipe,
) -> Result<OutputGeometry, PipelineError> {
    let orientation = recipe
        .geometry
        .orientation_override
        .unwrap_or(source_orientation);
    OutputGeometry::new(
        image.width(),
        image.height(),
        orientation,
        recipe.geometry.crop,
    )
}

fn validate_base_working_set(
    frame: &RawFrame,
    highlight_method: HighlightMethod,
    optics_enabled: bool,
) -> Result<(), PipelineError> {
    let full_pixels = frame
        .info
        .width
        .checked_mul(frame.info.height)
        .ok_or_else(|| dimension_overflow(frame.info.width, frame.info.height))?;
    let decoded_raw_bytes = frame
        .mosaic
        .len()
        .checked_mul(size_of::<u16>())
        .ok_or_else(|| dimension_overflow(frame.info.width, frame.info.height))?;
    let normalized_bytes = full_pixels
        .checked_mul(size_of::<f32>())
        .ok_or_else(|| dimension_overflow(frame.info.width, frame.info.height))?;
    let linear_bytes = full_pixels
        .checked_mul(3 * size_of::<f32>())
        .ok_or_else(|| dimension_overflow(frame.info.width, frame.info.height))?;
    let highlight_scratch_bytes = match highlight_method {
        HighlightMethod::LocalRatios => {
            rohditor_highlight::local_ratio_scratch_bytes(frame.info.width, frame.info.height)
                .ok_or_else(|| dimension_overflow(frame.info.width, frame.info.height))?
        }
        HighlightMethod::Opposed => {
            rohditor_highlight::opposed_scratch_bytes(frame.info.width, frame.info.height)
                .ok_or_else(|| dimension_overflow(frame.info.width, frame.info.height))?
        }
        HighlightMethod::Off | HighlightMethod::Clip => 0,
    };
    let highlight_peak = normalized_bytes
        .checked_add(highlight_scratch_bytes)
        .ok_or_else(|| dimension_overflow(frame.info.width, frame.info.height))?;
    let image_peak = normalized_bytes
        .checked_add(linear_bytes)
        .ok_or_else(|| dimension_overflow(frame.info.width, frame.info.height))?;
    let optics_scratch_bytes = if optics_enabled {
        frame
            .info
            .width
            .checked_mul(6)
            .and_then(|elements| elements.checked_mul(size_of::<f32>()))
            .ok_or_else(|| dimension_overflow(frame.info.width, frame.info.height))?
    } else {
        0
    };
    let optics_peak = linear_bytes
        .checked_add(linear_bytes)
        .and_then(|bytes| bytes.checked_add(optics_scratch_bytes))
        .ok_or_else(|| dimension_overflow(frame.info.width, frame.info.height))?;
    let working_bytes = decoded_raw_bytes
        .checked_add(image_peak.max(highlight_peak).max(optics_peak))
        .ok_or_else(|| dimension_overflow(frame.info.width, frame.info.height))?;
    validate_working_set(working_bytes)
}

fn validate_preview_working_set(
    frame: &RawFrame,
    max_long_edge: usize,
    highlight_method: HighlightMethod,
    optics_enabled: bool,
) -> Result<(), PipelineError> {
    let full_width = frame.info.width;
    let full_height = frame.info.height;
    let full_pixels = full_width
        .checked_mul(full_height)
        .ok_or_else(|| dimension_overflow(full_width, full_height))?;
    let decoded_raw_bytes = frame
        .mosaic
        .len()
        .checked_mul(size_of::<u16>())
        .ok_or_else(|| dimension_overflow(full_width, full_height))?;
    let normalized_bytes = full_pixels
        .checked_mul(size_of::<f32>())
        .ok_or_else(|| dimension_overflow(full_width, full_height))?;
    let full_linear_bytes = full_pixels
        .checked_mul(3 * size_of::<f32>())
        .ok_or_else(|| dimension_overflow(full_width, full_height))?;
    let (target_width, _) = preview_dimensions(full_width, full_height, max_long_edge)?;
    let intermediate_bytes = target_width
        .checked_mul(full_height)
        .and_then(|pixels| pixels.checked_mul(3 * size_of::<f32>()))
        .ok_or_else(|| dimension_overflow(target_width, full_height))?;
    let highlight_scratch_bytes = match highlight_method {
        HighlightMethod::LocalRatios => {
            rohditor_highlight::local_ratio_scratch_bytes(full_width, full_height)
                .ok_or_else(|| dimension_overflow(full_width, full_height))?
        }
        HighlightMethod::Opposed => {
            rohditor_highlight::opposed_scratch_bytes(full_width, full_height)
                .ok_or_else(|| dimension_overflow(full_width, full_height))?
        }
        HighlightMethod::Off | HighlightMethod::Clip => 0,
    };
    let demosaic_peak = normalized_bytes
        .checked_add(full_linear_bytes)
        .ok_or_else(|| dimension_overflow(full_width, full_height))?;
    let highlight_peak = normalized_bytes
        .checked_add(highlight_scratch_bytes)
        .ok_or_else(|| dimension_overflow(full_width, full_height))?;
    let optics_scratch_bytes = if optics_enabled {
        full_width
            .checked_mul(6)
            .and_then(|elements| elements.checked_mul(size_of::<f32>()))
            .ok_or_else(|| dimension_overflow(full_width, full_height))?
    } else {
        0
    };
    let optics_peak = full_linear_bytes
        .checked_add(full_linear_bytes)
        .and_then(|bytes| bytes.checked_add(optics_scratch_bytes))
        .ok_or_else(|| dimension_overflow(full_width, full_height))?;
    let horizontal_peak = full_linear_bytes
        .checked_add(intermediate_bytes)
        .ok_or_else(|| dimension_overflow(full_width, full_height))?;
    let conservative_peak = decoded_raw_bytes
        .checked_add(
            demosaic_peak
                .max(highlight_peak)
                .max(optics_peak)
                .max(horizontal_peak),
        )
        .ok_or_else(|| dimension_overflow(full_width, full_height))?;
    validate_working_set(conservative_peak)
}

struct PreviewPreparationInputs {
    decoded_raw_bytes: usize,
    normalized_mosaic_bytes: usize,
    full_linear_bytes: usize,
    highlight_scratch_bytes: usize,
    optics_output_bytes: usize,
    optics_scratch_bytes: usize,
    resample_intermediate_bytes: usize,
    reduced_linear_bytes: usize,
    unchanged_dimensions: bool,
}

fn preview_preparation_peak(inputs: PreviewPreparationInputs) -> Result<usize, PipelineError> {
    let PreviewPreparationInputs {
        decoded_raw_bytes,
        normalized_mosaic_bytes,
        full_linear_bytes,
        highlight_scratch_bytes,
        optics_output_bytes,
        optics_scratch_bytes,
        resample_intermediate_bytes,
        reduced_linear_bytes,
        unchanged_dimensions,
    } = inputs;
    let demosaic_peak = decoded_raw_bytes
        .checked_add(normalized_mosaic_bytes)
        .and_then(|bytes| bytes.checked_add(full_linear_bytes));
    let highlight_peak = decoded_raw_bytes
        .checked_add(normalized_mosaic_bytes)
        .and_then(|bytes| bytes.checked_add(highlight_scratch_bytes));
    let horizontal_peak = decoded_raw_bytes
        .checked_add(full_linear_bytes)
        .and_then(|bytes| bytes.checked_add(optics_output_bytes))
        .and_then(|bytes| bytes.checked_add(optics_scratch_bytes))
        .and_then(|bytes| bytes.checked_add(resample_intermediate_bytes));
    let optics_peak = decoded_raw_bytes
        .checked_add(full_linear_bytes)
        .and_then(|bytes| bytes.checked_add(optics_output_bytes))
        .and_then(|bytes| bytes.checked_add(optics_scratch_bytes));
    let vertical_peak = decoded_raw_bytes
        .checked_add(resample_intermediate_bytes)
        .and_then(|bytes| bytes.checked_add(reduced_linear_bytes));
    let demosaic_peak = demosaic_peak
        .ok_or_else(|| dimension_overflow(0, 0))?
        .max(highlight_peak.ok_or_else(|| dimension_overflow(0, 0))?)
        .max(optics_peak.ok_or_else(|| dimension_overflow(0, 0))?);
    let peak = if unchanged_dimensions {
        demosaic_peak
    } else {
        let horizontal_peak = horizontal_peak.ok_or_else(|| dimension_overflow(0, 0))?;
        let vertical_peak = vertical_peak.ok_or_else(|| dimension_overflow(0, 0))?;
        demosaic_peak.max(horizontal_peak).max(vertical_peak)
    };
    Ok(peak)
}

fn validate_working_set(estimated_bytes: usize) -> Result<(), PipelineError> {
    if estimated_bytes <= CPU_WORKING_SET_LIMIT_BYTES {
        Ok(())
    } else {
        Err(PipelineError::WorkingSetLimit {
            estimated_bytes,
            max_bytes: CPU_WORKING_SET_LIMIT_BYTES,
        })
    }
}

fn validate_optics_crop(
    crop_policy: RawCropPolicy,
    recipe: &EditRecipe,
) -> Result<(), PipelineError> {
    if crop_policy == RawCropPolicy::ActiveArea && crate::optics::optics_enabled(&recipe.optics) {
        Err(PipelineError::Optics {
            reason: "lens-profile correction requires the recommended RAW crop, not ActiveArea"
                .to_owned(),
        })
    } else {
        Ok(())
    }
}

fn dimension_overflow(width: usize, height: usize) -> PipelineError {
    PipelineError::InvalidDimensions {
        width,
        height,
        row_stride: width,
        reason: "memory-size calculation overflowed".to_owned(),
    }
}
