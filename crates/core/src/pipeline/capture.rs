//! Source-space capture stage shared by fit previews and full-resolution renders.
use std::time::{Duration, Instant};

use rohditor_demosaic::WhiteBalanceGains;
use rohditor_edit::{EditRecipe, HighlightMethod};
use rohditor_image::LinearRgbImage;
use rohditor_raw::RawFrame;

use crate::{CancellationToken, CaptureSharpeningProvenance, PipelineError, sharpening};

pub(super) struct CaptureStage {
    pub provenance: Option<CaptureSharpeningProvenance>,
    pub scratch_bytes: usize,
    pub elapsed: Duration,
}

pub(super) fn validate_working_set(
    frame: &RawFrame,
    recipe: &EditRecipe,
) -> Result<(), PipelineError> {
    recipe.capture_sharpening.validate()?;
    if recipe.capture_sharpening.is_active() {
        let scratch = sharpening::scratch_bytes(frame.info.width, frame.info.height)?;
        let bytes = frame
            .info
            .width
            .checked_mul(frame.info.height)
            .and_then(|n| n.checked_mul(3 * size_of::<f32>()))
            .and_then(|n| n.checked_add(scratch))
            .and_then(|n| {
                frame
                    .mosaic
                    .len()
                    .checked_mul(size_of::<u16>())
                    .and_then(|raw| n.checked_add(raw))
            })
            .ok_or_else(|| {
                super::orchestration::dimension_overflow(frame.info.width, frame.info.height)
            })?;
        super::orchestration::validate_working_set(bytes)?;
    }
    Ok(())
}

pub(super) fn apply(
    image: &mut LinearRgbImage<f32>,
    recipe: &EditRecipe,
    gains: WhiteBalanceGains,
    cancellation: &CancellationToken,
) -> Result<CaptureStage, PipelineError> {
    let provenance = CaptureSharpeningProvenance::for_settings(recipe.capture_sharpening);
    if provenance.is_none() {
        cancellation.checkpoint()?;
        return Ok(CaptureStage {
            provenance,
            scratch_bytes: 0,
            elapsed: Duration::ZERO,
        });
    }
    let started = Instant::now();
    let levels = match recipe.raw.highlights.method {
        HighlightMethod::Clip => {
            let ceiling =
                recipe.raw.highlights.clip.threshold * gains.red.min(gains.green).min(gains.blue);
            [
                ceiling / gains.red,
                ceiling / gains.green,
                ceiling / gains.blue,
            ]
        }
        HighlightMethod::LocalRatios => [recipe.raw.highlights.local_ratios.detection_threshold; 3],
        HighlightMethod::Opposed => [recipe.raw.highlights.opposed.detection_threshold; 3],
        HighlightMethod::Off => [1.0; 3],
    };
    let scratch_bytes = sharpening::scratch_bytes(image.width(), image.height())?;
    sharpening::apply_cancellable(image, recipe.capture_sharpening, levels, cancellation)?;
    Ok(CaptureStage {
        provenance,
        scratch_bytes,
        elapsed: started.elapsed(),
    })
}
