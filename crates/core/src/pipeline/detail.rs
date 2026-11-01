//! Source-resolution detail stage shared by fit previews and full-resolution
//! renders. It runs on demosaiced camera RGB, before optics, in a fixed order:
//! chroma noise reduction, luminance noise reduction, then capture sharpening.
//! (False-colour suppression will join in front of noise reduction.)
use std::time::{Duration, Instant};

use rohditor_demosaic::WhiteBalanceGains;
use rohditor_detail::{
    ChromaDenoise, DetailError, LumaDenoise, chroma_scratch_bytes, denoise_chroma, denoise_luma,
    estimate_luma_noise, luma_scratch_bytes,
};
use rohditor_edit::{CaptureSharpening, EditRecipe, HighlightMethod, NoiseReduction};
use rohditor_image::LinearRgbImage;
use rohditor_raw::RawFrame;

use crate::{CancellationToken, PipelineError, sharpening};

/// Bump when the noise-reduction output changes for the same settings.
pub const NOISE_REDUCTION_ALGORITHM_VERSION: u16 = 1;

/// Settings and implementation identity of an already processed base: every
/// detail step that ran, so a cached source is reused only for the same work.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct DetailProvenance {
    pub noise_reduction: Option<NoiseReduction>,
    pub capture_sharpening: Option<CaptureSharpening>,
    pub noise_reduction_version: u16,
    pub capture_sharpening_version: u16,
}

impl DetailProvenance {
    /// `None` when the recipe has no active detail step.
    #[must_use]
    pub fn for_recipe(recipe: &EditRecipe) -> Option<Self> {
        // The texture setting is parked while luminance reduction is off, so
        // it must not make otherwise identical work look different.
        let noise_reduction = recipe.noise_reduction.is_active().then(|| NoiseReduction {
            luminance_detail: if recipe.noise_reduction.luminance_active() {
                recipe.noise_reduction.luminance_detail
            } else {
                NoiseReduction::default().luminance_detail
            },
            ..recipe.noise_reduction
        });
        let capture_sharpening = recipe
            .capture_sharpening
            .is_active()
            .then_some(recipe.capture_sharpening);
        (noise_reduction.is_some() || capture_sharpening.is_some()).then_some(Self {
            noise_reduction,
            capture_sharpening,
            noise_reduction_version: NOISE_REDUCTION_ALGORITHM_VERSION,
            capture_sharpening_version: sharpening::CAPTURE_SHARPENING_ALGORITHM_VERSION,
        })
    }
}

impl DetailProvenance {
    /// Copy the recorded detail settings into `recipe` (inactive when `None`).
    pub fn write_settings(provenance: Option<Self>, recipe: &mut EditRecipe) {
        recipe.noise_reduction = provenance
            .and_then(|p| p.noise_reduction)
            .unwrap_or_default();
        recipe.capture_sharpening = provenance
            .and_then(|p| p.capture_sharpening)
            .unwrap_or_default();
    }
}

/// Whether any step of the detail stage has work to do for `recipe`.
#[must_use]
pub fn detail_is_active(recipe: &EditRecipe) -> bool {
    recipe.noise_reduction.is_active() || recipe.capture_sharpening.is_active()
}

#[derive(Debug, Clone)]
pub(super) struct DetailStage {
    pub provenance: Option<DetailProvenance>,
    /// Peak scratch of any step (steps run one after another).
    pub scratch_bytes: usize,
    pub chroma_noise_reduction: Duration,
    pub luminance_noise_reduction: Duration,
    pub capture_sharpening: Duration,
}

impl DetailStage {
    pub(super) const fn skipped() -> Self {
        Self {
            provenance: None,
            scratch_bytes: 0,
            chroma_noise_reduction: Duration::ZERO,
            luminance_noise_reduction: Duration::ZERO,
            capture_sharpening: Duration::ZERO,
        }
    }
}

/// Peak scratch bytes of the active steps for a `width` x `height` source.
pub(crate) fn scratch_bytes(
    recipe: &EditRecipe,
    width: usize,
    height: usize,
) -> Result<usize, PipelineError> {
    let overflow = || super::orchestration::dimension_overflow(width, height);
    let mut peak = 0;
    if recipe.noise_reduction.chroma_active() {
        peak = peak.max(chroma_scratch_bytes(width, height).ok_or_else(overflow)?);
    }
    if recipe.noise_reduction.luminance_active() {
        peak = peak.max(luma_scratch_bytes(width, height).ok_or_else(overflow)?);
    }
    if recipe.capture_sharpening.is_active() {
        peak = peak.max(sharpening::scratch_bytes(width, height)?);
    }
    Ok(peak)
}

pub(super) fn validate_working_set(
    frame: &RawFrame,
    recipe: &EditRecipe,
) -> Result<(), PipelineError> {
    recipe.noise_reduction.validate()?;
    recipe.capture_sharpening.validate()?;
    if detail_is_active(recipe) {
        let scratch = scratch_bytes(recipe, frame.info.width, frame.info.height)?;
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

/// Run the active detail steps in place on camera-native RGB.
///
/// `balance` is the as-shot demosaic balance (noise reduction splits colour
/// against it, so white-balance edits never invalidate the result) and
/// `gains` the user's white balance (it only shapes the highlight ceilings).
pub(super) fn apply(
    image: &mut LinearRgbImage<f32>,
    recipe: &EditRecipe,
    balance: WhiteBalanceGains,
    gains: WhiteBalanceGains,
    cancellation: &CancellationToken,
) -> Result<DetailStage, PipelineError> {
    let provenance = DetailProvenance::for_recipe(recipe);
    if provenance.is_none() {
        cancellation.checkpoint()?;
        return Ok(DetailStage::skipped());
    }
    let levels = ceilings(recipe, gains);
    let mut stage = DetailStage {
        provenance,
        scratch_bytes: scratch_bytes(recipe, image.width(), image.height())?,
        ..DetailStage::skipped()
    };
    if recipe.noise_reduction.is_active() {
        reduce_noise(image, recipe, balance, levels, cancellation, &mut stage)?;
    }
    if recipe.capture_sharpening.is_active() {
        let started = Instant::now();
        sharpening::apply_cancellable(image, recipe.capture_sharpening, levels, cancellation)?;
        stage.capture_sharpening = started.elapsed();
    }
    Ok(stage)
}

/// Chroma, then luminance noise reduction, both driven by one luma noise
/// profile (chroma leaves luma unchanged, so it stays valid for the second
/// step). Images too small to profile are left alone.
fn reduce_noise(
    image: &mut LinearRgbImage<f32>,
    recipe: &EditRecipe,
    balance: WhiteBalanceGains,
    ceilings: [f32; 3],
    cancellation: &CancellationToken,
    stage: &mut DetailStage,
) -> Result<(), PipelineError> {
    let settings = recipe.noise_reduction;
    let balance = [balance.red, balance.green, balance.blue];
    let is_cancelled = || cancellation.is_cancelled();
    let started = Instant::now();
    let noise =
        estimate_luma_noise(image, balance, &is_cancelled).map_err(noise_reduction_error)?;
    // The profile serves both steps; its time is booked to the first one.
    let profiling = started.elapsed();
    let (first, second) = (
        &mut stage.chroma_noise_reduction,
        &mut stage.luminance_noise_reduction,
    );
    if settings.chroma_active() {
        *first += profiling;
    } else {
        *second += profiling;
    }
    // Too small to profile: nothing to do.
    let Some(noise) = noise else {
        return Ok(());
    };
    if settings.chroma_active() {
        let started = Instant::now();
        denoise_chroma(
            image,
            ChromaDenoise {
                strength: settings.chroma,
                balance,
                ceilings,
                luma_noise: noise,
            },
            &is_cancelled,
        )
        .map_err(noise_reduction_error)?;
        stage.chroma_noise_reduction += started.elapsed();
    }
    if settings.luminance_active() {
        let started = Instant::now();
        denoise_luma(
            image,
            LumaDenoise {
                strength: settings.luminance,
                detail: settings.luminance_detail,
                balance,
                ceilings,
                noise,
            },
            &is_cancelled,
        )
        .map_err(noise_reduction_error)?;
        stage.luminance_noise_reduction += started.elapsed();
    }
    Ok(())
}

fn noise_reduction_error(error: DetailError) -> PipelineError {
    match error {
        DetailError::Cancelled => PipelineError::Cancelled,
        other => PipelineError::InvalidRecipe {
            field: "noise_reduction",
            reason: other.to_string(),
        },
    }
}

pub(super) fn ceilings(recipe: &EditRecipe, gains: WhiteBalanceGains) -> [f32; 3] {
    match recipe.raw.highlights.method {
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
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn provenance_ignores_the_parked_texture_setting() {
        let mut recipe = EditRecipe::default();
        assert_eq!(DetailProvenance::for_recipe(&recipe), None);
        // Texture preservation alone is not work.
        recipe.noise_reduction.luminance_detail = 0.9;
        assert_eq!(DetailProvenance::for_recipe(&recipe), None);
        recipe.noise_reduction.chroma = 0.4;
        let chroma_only = DetailProvenance::for_recipe(&recipe);
        recipe.noise_reduction.luminance_detail = 0.1;
        assert_eq!(DetailProvenance::for_recipe(&recipe), chroma_only);
        // Once luminance reduction runs, the setting is part of the work.
        recipe.noise_reduction.luminance = 0.5;
        let low = DetailProvenance::for_recipe(&recipe);
        recipe.noise_reduction.luminance_detail = 0.9;
        assert_ne!(DetailProvenance::for_recipe(&recipe), low);
    }
}
