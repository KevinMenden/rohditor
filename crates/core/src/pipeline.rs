use std::mem::size_of;
use std::time::{Duration, Instant};

use rohditor_raw::{RawFileInfo, RawFrame, RawOrientation};

use crate::color::{CameraColorTransform, camera_color_transform};
use crate::cpu::{
    apply_adjustments_cancellable, apply_camera_color_transform,
    apply_camera_color_transform_cancellable, demosaic_cancellable,
    normalize_raw_preview_cancellable, render_display_srgb8_cancellable,
};
use crate::{
    CancellationToken, DisplayRgbImage, DitherMode, EditRecipe, ExportImage, LinearRgbImage,
    MosaicImage, OutputBitDepth, PipelineError, WhiteBalance, apply_adjustments, demosaic,
    normalize_raw, normalize_raw_preview, render_display_srgb8, render_display_srgb8_dithered,
    render_display_srgb16, white_balance_gains,
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

/// Selectable CPU demosaic algorithms. The first reference is deliberately simple.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum DemosaicAlgorithm {
    #[default]
    Bilinear,
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
    pub linear_rgb_bytes: usize,
    pub display_rgb_bytes: usize,
    pub estimated_peak_bytes: usize,
}

/// Resolution-limited, normalized sensor mosaic retained between preview base
/// rebuilds. White balance and demosaic selection have not been applied yet.
#[derive(Debug, Clone)]
pub struct NormalizedPreview {
    mosaic: MosaicImage<f32>,
    info: RawFileInfo,
    camera_transform: CameraColorTransform,
    timings: StageTimings,
    decoded_raw_bytes: usize,
}

impl NormalizedPreview {
    /// Normalized CFA samples, mainly exposed for diagnostics and benchmarks.
    #[must_use]
    pub const fn image(&self) -> &MosaicImage<f32> {
        &self.mosaic
    }

    #[must_use]
    pub const fn timings(&self) -> StageTimings {
        self.timings
    }

    /// Bytes held by the normalized image buffer itself.
    #[must_use]
    pub fn buffer_bytes(&self) -> usize {
        self.mosaic.data().len().saturating_mul(size_of::<f32>())
    }
}

/// A completed CPU render and its diagnostics.
#[derive(Debug)]
pub struct RenderResult {
    pub image: DisplayRgbImage<u8>,
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
/// Phase 5 GPU backend. Exposure, contrast, saturation, orientation, and output
/// conversion can change without rebuilding this base.
#[derive(Debug, Clone)]
pub struct DemosaicedBase {
    image: LinearRgbImage<f32>,
    source_orientation: RawOrientation,
    white_balance: WhiteBalance,
    timings: StageTimings,
    decoded_raw_bytes: usize,
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
        let base = prepare_base(frame, recipe, options, BaseResolution::Full)?;
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
        let normalized =
            self.prepare_preview_normalized_cancellable(frame, options, cancellation)?;
        let mut base = self.prepare_preview_base_from_normalized_cancellable(
            &normalized,
            recipe,
            options.render.demosaic,
            cancellation,
        )?;
        base.timings.metadata += normalized.timings.metadata;
        base.timings.normalization = normalized.timings.normalization;
        base.timings.total += normalized.timings.total;
        Ok(base)
    }

    /// Normalize a resolution-limited sensor mosaic for reuse across white
    /// balance and demosaic changes.
    pub fn prepare_preview_normalized(
        &self,
        frame: &RawFrame,
        options: PreviewOptions,
    ) -> Result<NormalizedPreview, PipelineError> {
        self.prepare_preview_normalized_cancellable(frame, options, &CancellationToken::new())
    }

    /// Cancellable form of [`Self::prepare_preview_normalized`].
    pub fn prepare_preview_normalized_cancellable(
        &self,
        frame: &RawFrame,
        options: PreviewOptions,
        cancellation: &CancellationToken,
    ) -> Result<NormalizedPreview, PipelineError> {
        prepare_normalized_preview(frame, options, cancellation)
    }

    /// Demosaic and color-convert a retained normalized preview.
    pub fn prepare_preview_base_from_normalized(
        &self,
        normalized: &NormalizedPreview,
        recipe: &EditRecipe,
        algorithm: DemosaicAlgorithm,
    ) -> Result<DemosaicedBase, PipelineError> {
        self.prepare_preview_base_from_normalized_cancellable(
            normalized,
            recipe,
            algorithm,
            &CancellationToken::new(),
        )
    }

    /// Cancellable form of [`Self::prepare_preview_base_from_normalized`].
    pub fn prepare_preview_base_from_normalized_cancellable(
        &self,
        normalized: &NormalizedPreview,
        recipe: &EditRecipe,
        algorithm: DemosaicAlgorithm,
        cancellation: &CancellationToken,
    ) -> Result<DemosaicedBase, PipelineError> {
        prepare_demosaiced_preview(normalized, recipe, algorithm, cancellation)
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
        let retained_peak = memory_estimate(base, size_of::<u8>())?
            .estimated_peak_bytes
            .checked_add(retained_base_bytes)
            .ok_or_else(|| dimension_overflow(base.image.width(), base.image.height()))?;
        validate_working_set(retained_peak)?;
        let mut memory = memory_estimate(base, size_of::<u8>())?;
        memory.estimated_peak_bytes = memory
            .estimated_peak_bytes
            .checked_add(retained_base_bytes)
            .ok_or_else(|| dimension_overflow(base.image.width(), base.image.height()))?;

        let working = workspace.reset_from(base);
        cancellation.checkpoint()?;
        let mut timings = StageTimings::default();
        let adjustments_started = Instant::now();
        apply_adjustments_cancellable(working, recipe, cancellation)?;
        timings.adjustments = adjustments_started.elapsed();

        let orientation = recipe
            .orientation_override
            .unwrap_or(base.source_orientation);
        let output_started = Instant::now();
        let image =
            render_display_srgb8_cancellable(working, orientation, output_policy, cancellation)?;
        timings.output_conversion = output_started.elapsed();
        timings.total = total_started.elapsed();

        Ok(RenderResult {
            image,
            timings,
            memory,
        })
    }

    /// Render an sRGB8 preview from a CFA-phase-preserving, resolution-limited
    /// sensor mosaic.
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
        let mut base = prepare_base(frame, recipe, options, BaseResolution::Full)?;
        let memory = memory_estimate(&base, bit_depth.bytes_per_sample())?;
        let adjustments_started = Instant::now();
        apply_adjustments(&mut base.image, recipe)?;
        base.timings.adjustments = adjustments_started.elapsed();
        let orientation = recipe
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

fn prepare_normalized_preview(
    frame: &RawFrame,
    options: PreviewOptions,
    cancellation: &CancellationToken,
) -> Result<NormalizedPreview, PipelineError> {
    let total_started = Instant::now();
    cancellation.checkpoint()?;
    validate_base_working_set(frame, BaseResolution::Preview(options.max_long_edge))?;

    let metadata_started = Instant::now();
    let metadata_span = tracing::info_span!(
        "cpu.metadata",
        width = frame.info.width,
        height = frame.info.height,
        purpose = "preview normalization"
    );
    let metadata_guard = metadata_span.enter();
    let camera_transform = camera_color_transform(&frame.info)?;
    let metadata = metadata_started.elapsed();
    drop(metadata_guard);

    let normalization_started = Instant::now();
    let mosaic = normalize_raw_preview_cancellable(
        frame,
        options.render.crop_policy,
        options.max_long_edge,
        cancellation,
    )?;
    let normalization = normalization_started.elapsed();
    let decoded_raw_bytes = frame
        .mosaic
        .len()
        .checked_mul(size_of::<u16>())
        .ok_or_else(|| dimension_overflow(frame.info.width, frame.info.height))?;
    let timings = StageTimings {
        metadata,
        normalization,
        total: total_started.elapsed(),
        ..StageTimings::default()
    };

    Ok(NormalizedPreview {
        mosaic,
        info: frame.info.clone(),
        camera_transform,
        timings,
        decoded_raw_bytes,
    })
}

fn prepare_demosaiced_preview(
    normalized: &NormalizedPreview,
    recipe: &EditRecipe,
    algorithm: DemosaicAlgorithm,
    cancellation: &CancellationToken,
) -> Result<DemosaicedBase, PipelineError> {
    let total_started = Instant::now();
    cancellation.checkpoint()?;
    let metadata_started = Instant::now();
    let metadata_span = tracing::info_span!(
        "cpu.metadata",
        width = normalized.mosaic.width(),
        height = normalized.mosaic.height(),
        purpose = "preview white balance"
    );
    let metadata_guard = metadata_span.enter();
    recipe.validate()?;
    let gains = white_balance_gains(&normalized.info, recipe.white_balance)?;
    let metadata = metadata_started.elapsed();
    drop(metadata_guard);

    let demosaic_started = Instant::now();
    let mut image = demosaic_cancellable(&normalized.mosaic, gains, algorithm, cancellation)?;
    let demosaic = demosaic_started.elapsed();

    let color_started = Instant::now();
    apply_camera_color_transform_cancellable(
        &mut image,
        &normalized.camera_transform,
        cancellation,
    )?;
    let color_conversion = color_started.elapsed();
    let timings = StageTimings {
        metadata,
        demosaic,
        color_conversion,
        total: total_started.elapsed(),
        ..StageTimings::default()
    };

    Ok(DemosaicedBase {
        image,
        source_orientation: normalized.info.orientation,
        white_balance: recipe.white_balance,
        timings,
        decoded_raw_bytes: normalized.decoded_raw_bytes,
    })
}

#[derive(Debug, Clone, Copy)]
enum BaseResolution {
    Full,
    Preview(usize),
}

fn prepare_base(
    frame: &RawFrame,
    recipe: &EditRecipe,
    options: RenderOptions,
    resolution: BaseResolution,
) -> Result<DemosaicedBase, PipelineError> {
    let total_started = Instant::now();
    validate_base_working_set(frame, resolution)?;
    let metadata_started = Instant::now();
    let metadata_span = tracing::info_span!(
        "cpu.metadata",
        width = frame.info.width,
        height = frame.info.height,
        purpose = "full pipeline base"
    );
    let metadata_guard = metadata_span.enter();
    recipe.validate()?;
    let gains = white_balance_gains(&frame.info, recipe.white_balance)?;
    let camera_transform = camera_color_transform(&frame.info)?;
    let metadata = metadata_started.elapsed();
    drop(metadata_guard);

    let normalization_started = Instant::now();
    let normalized = match resolution {
        BaseResolution::Full => normalize_raw(frame, options.crop_policy)?,
        BaseResolution::Preview(max_long_edge) => {
            normalize_raw_preview(frame, options.crop_policy, max_long_edge)?
        }
    };
    let normalization = normalization_started.elapsed();

    let demosaic_started = Instant::now();
    let mut linear = demosaic(&normalized, gains, options.demosaic)?;
    let demosaic = demosaic_started.elapsed();
    drop(normalized);

    let color_started = Instant::now();
    apply_camera_color_transform(&mut linear, &camera_transform)?;
    let color_conversion = color_started.elapsed();

    let decoded_raw_bytes = frame
        .mosaic
        .len()
        .checked_mul(size_of::<u16>())
        .ok_or_else(|| dimension_overflow(frame.info.width, frame.info.height))?;

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
        white_balance: recipe.white_balance,
        timings,
        decoded_raw_bytes,
    })
}

fn validate_base_recipe(base: &DemosaicedBase, recipe: &EditRecipe) -> Result<(), PipelineError> {
    recipe.validate()?;
    if recipe.white_balance == base.white_balance {
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
        .orientation_override
        .unwrap_or(base.source_orientation);
    let image = render_display_srgb8(&base.image, orientation, output_policy)?;
    base.timings.output_conversion = output_started.elapsed();
    base.timings.total = base.timings.metadata
        + base.timings.normalization
        + base.timings.demosaic
        + base.timings.color_conversion
        + base.timings.adjustments
        + base.timings.output_conversion;

    Ok(RenderResult {
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
    let normalized_mosaic_bytes = pixels
        .checked_mul(size_of::<f32>())
        .ok_or_else(|| dimension_overflow(width, height))?;
    let linear_rgb_bytes = pixels
        .checked_mul(3)
        .and_then(|elements| elements.checked_mul(size_of::<f32>()))
        .ok_or_else(|| dimension_overflow(width, height))?;
    let display_rgb_bytes = pixels
        .checked_mul(3)
        .and_then(|elements| elements.checked_mul(display_sample_bytes))
        .ok_or_else(|| dimension_overflow(width, height))?;
    let demosaic_peak = decoded_raw_bytes
        .checked_add(normalized_mosaic_bytes)
        .and_then(|bytes| bytes.checked_add(linear_rgb_bytes))
        .ok_or_else(|| dimension_overflow(width, height))?;
    let output_peak = decoded_raw_bytes
        .checked_add(linear_rgb_bytes)
        .and_then(|bytes| bytes.checked_add(display_rgb_bytes))
        .ok_or_else(|| dimension_overflow(width, height))?;

    let estimate = MemoryEstimate {
        decoded_raw_bytes,
        normalized_mosaic_bytes,
        linear_rgb_bytes,
        display_rgb_bytes,
        estimated_peak_bytes: demosaic_peak.max(output_peak),
    };
    validate_working_set(estimate.estimated_peak_bytes)?;
    Ok(estimate)
}

fn validate_base_working_set(
    frame: &RawFrame,
    resolution: BaseResolution,
) -> Result<(), PipelineError> {
    let full_pixels = frame
        .info
        .width
        .checked_mul(frame.info.height)
        .ok_or_else(|| dimension_overflow(frame.info.width, frame.info.height))?;
    let pixels = match resolution {
        BaseResolution::Full => full_pixels,
        BaseResolution::Preview(max_long_edge)
            if max_long_edge < frame.info.width.max(frame.info.height) =>
        {
            full_pixels.min(
                max_long_edge
                    .checked_mul(max_long_edge)
                    .ok_or_else(|| dimension_overflow(max_long_edge, max_long_edge))?,
            )
        }
        BaseResolution::Preview(_) => full_pixels,
    };
    let decoded_raw_bytes = frame
        .mosaic
        .len()
        .checked_mul(size_of::<u16>())
        .ok_or_else(|| dimension_overflow(frame.info.width, frame.info.height))?;
    let working_bytes = pixels
        .checked_mul(size_of::<f32>() * 4)
        .and_then(|bytes| bytes.checked_add(decoded_raw_bytes))
        .ok_or_else(|| dimension_overflow(frame.info.width, frame.info.height))?;
    validate_working_set(working_bytes)
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
