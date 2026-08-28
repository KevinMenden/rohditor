use std::mem::size_of;
use std::time::{Duration, Instant};

use rohditor_raw::RawFrame;

use crate::color::camera_color_transform;
use crate::cpu::apply_camera_color_transform;
use crate::{
    DisplayRgbImage, EditRecipe, PipelineError, apply_adjustments, demosaic, normalize_raw,
    render_display_srgb8, white_balance_gains,
};

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

/// A completed CPU render and its diagnostics.
#[derive(Debug)]
pub struct RenderResult {
    pub image: DisplayRgbImage<u8>,
    pub timings: StageTimings,
    pub memory: MemoryEstimate,
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

        let metadata_started = Instant::now();
        recipe.validate()?;
        let gains = white_balance_gains(&frame.info, recipe.white_balance)?;
        let camera_transform = camera_color_transform(&frame.info)?;
        let metadata = metadata_started.elapsed();

        let normalization_started = Instant::now();
        let normalized = normalize_raw(frame, options.crop_policy)?;
        let normalization = normalization_started.elapsed();

        let memory = memory_estimate(frame, normalized.width(), normalized.height())?;

        let demosaic_started = Instant::now();
        let mut linear = demosaic(&normalized, gains, options.demosaic)?;
        let demosaic = demosaic_started.elapsed();
        drop(normalized);

        let color_started = Instant::now();
        apply_camera_color_transform(&mut linear, &camera_transform)?;
        let color_conversion = color_started.elapsed();

        let adjustments_started = Instant::now();
        apply_adjustments(&mut linear, recipe)?;
        let adjustments = adjustments_started.elapsed();

        let output_started = Instant::now();
        let orientation = recipe
            .orientation_override
            .unwrap_or(frame.info.orientation);
        let image = render_display_srgb8(&linear, orientation, options.output_policy)?;
        let output_conversion = output_started.elapsed();

        Ok(RenderResult {
            image,
            timings: StageTimings {
                metadata,
                normalization,
                demosaic,
                color_conversion,
                adjustments,
                output_conversion,
                total: total_started.elapsed(),
            },
            memory,
        })
    }
}

fn memory_estimate(
    frame: &RawFrame,
    width: usize,
    height: usize,
) -> Result<MemoryEstimate, PipelineError> {
    let pixels = width
        .checked_mul(height)
        .ok_or_else(|| dimension_overflow(width, height))?;
    let decoded_raw_bytes = frame
        .mosaic
        .len()
        .checked_mul(size_of::<u16>())
        .ok_or_else(|| dimension_overflow(width, height))?;
    let normalized_mosaic_bytes = pixels
        .checked_mul(size_of::<f32>())
        .ok_or_else(|| dimension_overflow(width, height))?;
    let linear_rgb_bytes = pixels
        .checked_mul(3)
        .and_then(|elements| elements.checked_mul(size_of::<f32>()))
        .ok_or_else(|| dimension_overflow(width, height))?;
    let display_rgb_bytes = pixels
        .checked_mul(3)
        .and_then(|elements| elements.checked_mul(size_of::<u8>()))
        .ok_or_else(|| dimension_overflow(width, height))?;
    let demosaic_peak = decoded_raw_bytes
        .checked_add(normalized_mosaic_bytes)
        .and_then(|bytes| bytes.checked_add(linear_rgb_bytes))
        .ok_or_else(|| dimension_overflow(width, height))?;
    let output_peak = decoded_raw_bytes
        .checked_add(linear_rgb_bytes)
        .and_then(|bytes| bytes.checked_add(display_rgb_bytes))
        .ok_or_else(|| dimension_overflow(width, height))?;

    Ok(MemoryEstimate {
        decoded_raw_bytes,
        normalized_mosaic_bytes,
        linear_rgb_bytes,
        display_rgb_bytes,
        estimated_peak_bytes: demosaic_peak.max(output_peak),
    })
}

fn dimension_overflow(width: usize, height: usize) -> PipelineError {
    PipelineError::InvalidDimensions {
        width,
        height,
        row_stride: width,
        reason: "memory-size calculation overflowed".to_owned(),
    }
}
