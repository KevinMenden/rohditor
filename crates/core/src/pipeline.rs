use std::mem::size_of;
use std::time::{Duration, Instant};

use rohditor_raw::{RawFileInfo, RawFrame, RawOrientation};

use crate::analysis::Histogram;
use crate::color::{CameraColorTransform, camera_color_transform};
use crate::cpu::{
    apply_adjustments_cancellable, apply_camera_color_transform_cancellable,
    apply_white_balance_cancellable, normalize_raw_cancellable, preview_dimensions,
    render_display_srgb8_cancellable,
};
use crate::demosaic::demosaic_cancellable;
use crate::resample::resize_area_cancellable;
use crate::{
    CancellationToken, DemosaicAlgorithm, DisplayRgbImage, DitherMode, EditRecipe, ExportImage,
    LinearRgbImage, OutputBitDepth, PipelineError, WhiteBalance, WhiteBalanceGains,
    apply_adjustments, render_display_srgb8, render_display_srgb8_dithered, render_display_srgb16,
    white_balance_gains,
};

/// Default longest edge of an interactively developed preview.
pub const DEFAULT_PREVIEW_LONG_EDGE: usize = 2_560;

/// Maximum estimated live CPU image buffers for one render operation.
pub const CPU_WORKING_SET_LIMIT_BYTES: usize = 2 * 1_024 * 1_024 * 1_024;

/// Sensor crop selected before normalization.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum CropPolicy {
    ActiveArea,
    #[default]
    Recommended,
}

/// Explicit output clipping/gamut policy.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum OutputPolicy {
    #[default]
    ClipToSrgb,
}

/// Stable options that are not edits to the image itself.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct RenderOptions {
    pub crop_policy: CropPolicy,
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
    pub demosaic: Duration,
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
    info: RawFileInfo,
    camera_transform: CameraColorTransform,
    timings: StageTimings,
    decoded_raw_bytes: usize,
    normalized_mosaic_bytes: usize,
    resample_intermediate_bytes: usize,
    preparation_peak_bytes: usize,
}

impl ReconstructedPreview {
    /// Reduced unbalanced camera-native RGB samples.
    #[must_use]
    pub const fn image(&self) -> &LinearRgbImage<f32> {
        &self.image
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
    pub memory: MemoryEstimate,
}

/// A full-resolution export render and its processing diagnostics.
#[derive(Debug)]
pub struct ExportRenderResult {
    pub image: ExportImage,
    pub timings: StageTimings,
    pub memory: MemoryEstimate,
}

/// A linear Rec.2020 preview after normalization, white balance, demosaic, and
/// camera color conversion, but before interactive adjustments.
///
/// This is the cache/upload boundary shared by the CPU reference path and the
/// Phase 5 GPU backend. Downstream light/color edits, orientation, and output
/// conversion can change without rebuilding this base.
#[derive(Debug, Clone)]
pub struct DemosaicedBase {
    image: LinearRgbImage<f32>,
    source_orientation: RawOrientation,
    white_balance: WhiteBalance,
    timings: StageTimings,
    decoded_raw_bytes: usize,
    normalized_mosaic_bytes: usize,
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
    pub const fn source_orientation(&self) -> RawOrientation {
        self.source_orientation
    }

    #[must_use]
    pub const fn white_balance(&self) -> WhiteBalance {
        self.white_balance
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
#[derive(Debug, Default, Clone, Copy)]
pub struct CpuPipeline;

impl CpuPipeline {
    pub fn render(
        &self,
        frame: &RawFrame,
        recipe: &EditRecipe,
        options: RenderOptions,
    ) -> Result<RenderResult, PipelineError> {
        let total_started = Instant::now();
        let base = prepare_base(frame, recipe, options)?;
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
            self.prepare_preview_reconstruction_cancellable(frame, options, cancellation)?;
        let mut base = self.prepare_preview_base_from_reconstruction_cancellable(
            &reconstructed,
            recipe,
            cancellation,
        )?;
        base.timings.metadata += reconstructed.timings.metadata;
        base.timings.normalization = reconstructed.timings.normalization;
        base.timings.demosaic = reconstructed.timings.demosaic;
        base.timings.resampling = reconstructed.timings.resampling;
        base.timings.total += reconstructed.timings.total;
        Ok(base)
    }

    /// Reconstruct and antialias a camera-native preview base for reuse across
    /// white-balance and downstream edit changes.
    pub fn prepare_preview_reconstruction(
        &self,
        frame: &RawFrame,
        options: PreviewOptions,
    ) -> Result<ReconstructedPreview, PipelineError> {
        self.prepare_preview_reconstruction_cancellable(frame, options, &CancellationToken::new())
    }

    /// Cancellable form of [`Self::prepare_preview_reconstruction`].
    pub fn prepare_preview_reconstruction_cancellable(
        &self,
        frame: &RawFrame,
        options: PreviewOptions,
        cancellation: &CancellationToken,
    ) -> Result<ReconstructedPreview, PipelineError> {
        prepare_reconstructed_preview(frame, options, cancellation)
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
        let mut memory = memory_estimate(base, size_of::<u8>())?;
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

        let orientation = recipe
            .geometry
            .orientation_override
            .unwrap_or(base.source_orientation);
        let output_started = Instant::now();
        let image =
            render_display_srgb8_cancellable(working, orientation, output_policy, cancellation)?;
        let histogram = Histogram::from_display_rgb8(&image);
        timings.output_conversion = output_started.elapsed();
        timings.total = total_started.elapsed();

        Ok(RenderResult {
            image,
            histogram,
            timings,
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
        let mut base = prepare_base_cancellable(frame, recipe, options, cancellation)?;
        let memory = memory_estimate(&base, size_of::<u8>())?;
        let adjustments_started = Instant::now();
        apply_adjustments_cancellable(&mut base.image, recipe, cancellation)?;
        base.timings.adjustments = adjustments_started.elapsed();
        let orientation = recipe
            .geometry
            .orientation_override
            .unwrap_or(base.source_orientation);
        let output_started = Instant::now();
        let image = render_display_srgb8_cancellable(
            &base.image,
            orientation,
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
        let mut base = prepare_base(frame, recipe, options)?;
        let memory = memory_estimate(&base, bit_depth.bytes_per_sample())?;
        let adjustments_started = Instant::now();
        apply_adjustments(&mut base.image, recipe)?;
        base.timings.adjustments = adjustments_started.elapsed();
        let orientation = recipe
            .geometry
            .orientation_override
            .unwrap_or(base.source_orientation);

        let output_started = Instant::now();
        let image = match bit_depth {
            OutputBitDepth::Eight => ExportImage::Rgb8(render_display_srgb8_dithered(
                &base.image,
                orientation,
                options.output_policy,
                dithering,
            )?),
            OutputBitDepth::Sixteen => ExportImage::Rgb16(render_display_srgb16(
                &base.image,
                orientation,
                options.output_policy,
                dithering,
            )?),
        };
        base.timings.output_conversion = output_started.elapsed();
        base.timings.total = total_started.elapsed();

        Ok(ExportRenderResult {
            image,
            timings: base.timings,
            memory,
        })
    }
}

fn prepare_reconstructed_preview(
    frame: &RawFrame,
    options: PreviewOptions,
    cancellation: &CancellationToken,
) -> Result<ReconstructedPreview, PipelineError> {
    let total_started = Instant::now();
    cancellation.checkpoint()?;
    validate_preview_working_set(frame, options.max_long_edge)?;

    let metadata_started = Instant::now();
    let metadata_span = tracing::info_span!(
        "cpu.metadata",
        width = frame.info.width,
        height = frame.info.height,
        purpose = "preview reconstruction"
    );
    let metadata_guard = metadata_span.enter();
    let camera_transform = camera_color_transform(&frame.info)?;
    let metadata = metadata_started.elapsed();
    drop(metadata_guard);

    let normalization_started = Instant::now();
    let mosaic = normalize_raw_cancellable(frame, options.render.crop_policy, cancellation)?;
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

    let demosaic_started = Instant::now();
    let full_linear = demosaic_cancellable(
        &mosaic,
        WhiteBalanceGains::identity(),
        options.render.demosaic,
        cancellation,
    )?;
    let demosaic = demosaic_started.elapsed();
    drop(mosaic);

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
    let preparation_peak_bytes = preview_preparation_peak(
        decoded_raw_bytes,
        normalized_mosaic_bytes,
        full_linear_bytes,
        resample_intermediate_bytes,
        reduced_linear_bytes,
        unchanged_dimensions,
    )?;
    validate_working_set(preparation_peak_bytes)?;

    let resampling_started = Instant::now();
    let image = resize_area_cancellable(full_linear, target_width, target_height, cancellation)?;
    let resampling = resampling_started.elapsed();
    let timings = StageTimings {
        metadata,
        normalization,
        demosaic,
        resampling,
        total: total_started.elapsed(),
        ..StageTimings::default()
    };

    Ok(ReconstructedPreview {
        image,
        info: frame.info.clone(),
        camera_transform,
        timings,
        decoded_raw_bytes,
        normalized_mosaic_bytes,
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
    let gains = white_balance_gains(&reconstructed.info, recipe.color.white_balance)?;
    let metadata = metadata_started.elapsed();
    drop(metadata_guard);

    let color_started = Instant::now();
    let mut image = reconstructed.image.clone();
    apply_white_balance_cancellable(&mut image, gains, cancellation)?;
    apply_camera_color_transform_cancellable(
        &mut image,
        &reconstructed.camera_transform,
        cancellation,
    )?;
    let color_conversion = color_started.elapsed();
    let timings = StageTimings {
        metadata,
        color_conversion,
        total: total_started.elapsed(),
        ..StageTimings::default()
    };

    Ok(DemosaicedBase {
        image,
        source_orientation: reconstructed.info.orientation,
        white_balance: recipe.color.white_balance,
        timings,
        decoded_raw_bytes: reconstructed.decoded_raw_bytes,
        normalized_mosaic_bytes: reconstructed.normalized_mosaic_bytes,
        resample_intermediate_bytes: reconstructed.resample_intermediate_bytes,
        preparation_peak_bytes: reconstructed.preparation_peak_bytes,
    })
}

fn prepare_base(
    frame: &RawFrame,
    recipe: &EditRecipe,
    options: RenderOptions,
) -> Result<DemosaicedBase, PipelineError> {
    prepare_base_cancellable(frame, recipe, options, &CancellationToken::new())
}

fn prepare_base_cancellable(
    frame: &RawFrame,
    recipe: &EditRecipe,
    options: RenderOptions,
    cancellation: &CancellationToken,
) -> Result<DemosaicedBase, PipelineError> {
    let total_started = Instant::now();
    cancellation.checkpoint()?;
    validate_base_working_set(frame)?;
    let metadata_started = Instant::now();
    let metadata_span = tracing::info_span!(
        "cpu.metadata",
        width = frame.info.width,
        height = frame.info.height,
        purpose = "full pipeline base"
    );
    let metadata_guard = metadata_span.enter();
    recipe.validate()?;
    let gains = white_balance_gains(&frame.info, recipe.color.white_balance)?;
    let camera_transform = camera_color_transform(&frame.info)?;
    let metadata = metadata_started.elapsed();
    drop(metadata_guard);

    let normalization_started = Instant::now();
    let normalized = normalize_raw_cancellable(frame, options.crop_policy, cancellation)?;
    let normalization = normalization_started.elapsed();
    let normalized_mosaic_bytes = normalized
        .data()
        .len()
        .checked_mul(size_of::<f32>())
        .ok_or_else(|| dimension_overflow(normalized.width(), normalized.height()))?;

    let demosaic_started = Instant::now();
    let mut linear = demosaic_cancellable(&normalized, gains, options.demosaic, cancellation)?;
    let demosaic = demosaic_started.elapsed();
    drop(normalized);

    let color_started = Instant::now();
    apply_camera_color_transform_cancellable(&mut linear, &camera_transform, cancellation)?;
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
    let preparation_peak_bytes = decoded_raw_bytes
        .checked_add(normalized_mosaic_bytes)
        .and_then(|bytes| bytes.checked_add(linear_rgb_bytes))
        .ok_or_else(|| dimension_overflow(linear.width(), linear.height()))?;

    let mut timings = StageTimings {
        metadata,
        normalization,
        demosaic,
        color_conversion,
        ..StageTimings::default()
    };
    timings.total = total_started.elapsed();

    Ok(DemosaicedBase {
        image: linear,
        source_orientation: frame.info.orientation,
        white_balance: recipe.color.white_balance,
        timings,
        decoded_raw_bytes,
        normalized_mosaic_bytes,
        resample_intermediate_bytes: 0,
        preparation_peak_bytes,
    })
}

fn validate_base_recipe(base: &DemosaicedBase, recipe: &EditRecipe) -> Result<(), PipelineError> {
    recipe.validate()?;
    if recipe.color.white_balance == base.white_balance {
        Ok(())
    } else {
        Err(PipelineError::InvalidRecipe {
            field: "white_balance",
            reason: "the recipe does not match the white balance used to build the demosaiced base"
                .to_owned(),
        })
    }
}

fn render_base(
    mut base: DemosaicedBase,
    recipe: &EditRecipe,
    output_policy: OutputPolicy,
) -> Result<RenderResult, PipelineError> {
    validate_base_recipe(&base, recipe)?;
    let memory = memory_estimate(&base, size_of::<u8>())?;

    let adjustments_started = Instant::now();
    apply_adjustments(&mut base.image, recipe)?;
    base.timings.adjustments = adjustments_started.elapsed();

    let output_started = Instant::now();
    let orientation = recipe
        .geometry
        .orientation_override
        .unwrap_or(base.source_orientation);
    let image = render_display_srgb8(&base.image, orientation, output_policy)?;
    base.timings.output_conversion = output_started.elapsed();
    base.timings.total = base.timings.metadata
        + base.timings.normalization
        + base.timings.demosaic
        + base.timings.resampling
        + base.timings.color_conversion
        + base.timings.adjustments
        + base.timings.output_conversion;

    Ok(RenderResult {
        histogram: Histogram::from_display_rgb8(&image),
        image,
        timings: base.timings,
        memory,
    })
}

fn memory_estimate(
    base: &DemosaicedBase,
    display_sample_bytes: usize,
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
    let display_rgb_bytes = pixels
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
        resample_intermediate_bytes,
        linear_rgb_bytes,
        display_rgb_bytes,
        estimated_peak_bytes: base.preparation_peak_bytes.max(output_peak),
    };
    validate_working_set(estimate.estimated_peak_bytes)?;
    Ok(estimate)
}

fn validate_base_working_set(frame: &RawFrame) -> Result<(), PipelineError> {
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
    let working_bytes = full_pixels
        .checked_mul(size_of::<f32>() * 4)
        .and_then(|bytes| bytes.checked_add(decoded_raw_bytes))
        .ok_or_else(|| dimension_overflow(frame.info.width, frame.info.height))?;
    validate_working_set(working_bytes)
}

fn validate_preview_working_set(
    frame: &RawFrame,
    max_long_edge: usize,
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
    let conservative_peak = decoded_raw_bytes
        .checked_add(full_linear_bytes)
        .and_then(|bytes| bytes.checked_add(normalized_bytes.max(intermediate_bytes)))
        .ok_or_else(|| dimension_overflow(full_width, full_height))?;
    validate_working_set(conservative_peak)
}

fn preview_preparation_peak(
    decoded_raw_bytes: usize,
    normalized_mosaic_bytes: usize,
    full_linear_bytes: usize,
    resample_intermediate_bytes: usize,
    reduced_linear_bytes: usize,
    unchanged_dimensions: bool,
) -> Result<usize, PipelineError> {
    let demosaic_peak = decoded_raw_bytes
        .checked_add(normalized_mosaic_bytes)
        .and_then(|bytes| bytes.checked_add(full_linear_bytes));
    let horizontal_peak = decoded_raw_bytes
        .checked_add(full_linear_bytes)
        .and_then(|bytes| bytes.checked_add(resample_intermediate_bytes));
    let vertical_peak = decoded_raw_bytes
        .checked_add(resample_intermediate_bytes)
        .and_then(|bytes| bytes.checked_add(reduced_linear_bytes));
    let demosaic_peak = demosaic_peak.ok_or_else(|| dimension_overflow(0, 0))?;
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

fn dimension_overflow(width: usize, height: usize) -> PipelineError {
    PipelineError::InvalidDimensions {
        width,
        height,
        row_stride: width,
        reason: "memory-size calculation overflowed".to_owned(),
    }
}
