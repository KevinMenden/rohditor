//! Source-resolution detail stage shared by fit previews and full-resolution
//! renders. It runs on demosaiced camera RGB, before optics, in a fixed order:
//! false-colour suppression (a demosaic clean-up, so it sees the raw demosaic
//! output), chroma noise reduction, luminance noise reduction, then capture
//! sharpening. Noise is profiled once, on the stage input.
use std::time::{Duration, Instant};

use rohditor_demosaic::WhiteBalanceGains;
use rohditor_detail::{
    ChromaDenoise, DetailError, FalseColor, LumaDenoise, NoiseProfiles, chroma_scratch_bytes,
    denoise_chroma, denoise_luma, estimate_noise_profiles, false_color_scratch_bytes,
    luma_scratch_bytes, suppress_false_color,
};
use rohditor_edit::{
    CaptureSharpening, EditRecipe, FalseColorSuppression, HighlightMethod, NoiseReduction,
};
use rohditor_image::LinearRgbImage;
use rohditor_raw::RawFrame;

use super::detail_contract::DetailContract;
use crate::{CancellationToken, PipelineError, sharpening};

/// Bump when the noise-reduction output changes for the same settings.
pub const NOISE_REDUCTION_ALGORITHM_VERSION: u16 = 1;
/// Bump when the false-colour suppression output changes for the same settings.
pub const FALSE_COLOR_ALGORITHM_VERSION: u16 = 1;

/// Settings and implementation identity of an already processed base: every
/// detail step that ran, so a cached source is reused only for the same work.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct DetailProvenance {
    pub false_color: Option<FalseColorSuppression>,
    pub noise_reduction: Option<NoiseReduction>,
    pub capture_sharpening: Option<CaptureSharpening>,
    pub false_color_version: u16,
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
        let false_color = recipe.false_color.is_active().then_some(recipe.false_color);
        (false_color.is_some() || noise_reduction.is_some() || capture_sharpening.is_some())
            .then_some(Self {
                false_color,
                noise_reduction,
                capture_sharpening,
                false_color_version: FALSE_COLOR_ALGORITHM_VERSION,
                noise_reduction_version: NOISE_REDUCTION_ALGORITHM_VERSION,
                capture_sharpening_version: sharpening::CAPTURE_SHARPENING_ALGORITHM_VERSION,
            })
    }
}

impl DetailProvenance {
    /// Copy the recorded detail settings into `recipe` (inactive when `None`).
    pub fn write_settings(provenance: Option<Self>, recipe: &mut EditRecipe) {
        recipe.false_color = provenance.and_then(|p| p.false_color).unwrap_or_default();
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
    recipe.false_color.is_active()
        || recipe.noise_reduction.is_active()
        || recipe.capture_sharpening.is_active()
}

#[derive(Debug, Clone)]
pub(super) struct DetailStage {
    pub provenance: Option<DetailProvenance>,
    /// Peak scratch of any step (steps run one after another).
    pub scratch_bytes: usize,
    pub false_color: Duration,
    pub chroma_noise_reduction: Duration,
    pub luminance_noise_reduction: Duration,
    pub capture_sharpening: Duration,
}

impl DetailStage {
    pub(super) const fn skipped() -> Self {
        Self {
            provenance: None,
            scratch_bytes: 0,
            false_color: Duration::ZERO,
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
    if recipe.false_color.is_active() {
        peak = peak.max(false_color_scratch_bytes(width, height).ok_or_else(overflow)?);
    }
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
    recipe.false_color.validate()?;
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
    let mut stage = DetailStage {
        provenance,
        scratch_bytes: scratch_bytes(recipe, image.width(), image.height())?,
        ..DetailStage::skipped()
    };
    let contract = DetailContract::new(recipe, balance, gains)?;
    run_steps(image, &contract, cancellation, &mut stage)?;
    Ok(stage)
}

/// Run the steps of `contract` in order, booking their times to `stage`.
pub(super) fn run_steps(
    image: &mut LinearRgbImage<f32>,
    contract: &DetailContract,
    cancellation: &CancellationToken,
    stage: &mut DetailStage,
) -> Result<(), PipelineError> {
    let (balance, levels) = (contract.balance(), contract.ceilings());
    let settings = contract.noise_reduction();
    // Profile before any step edits the image, so executors that work on
    // tiles can profile once up front too. Images too small to profile skip
    // noise reduction.
    let noise = if settings.is_active() {
        let started = Instant::now();
        let noise = estimate_noise_profiles(image, balance, &|| cancellation.is_cancelled())
            .map_err(noise_reduction_error)?;
        // The profile serves both steps; its time is booked to the first one.
        let first = if settings.chroma_active() {
            &mut stage.chroma_noise_reduction
        } else {
            &mut stage.luminance_noise_reduction
        };
        *first += started.elapsed();
        noise
    } else {
        None
    };
    if contract.false_color().is_active() {
        let started = Instant::now();
        suppress_false_color(
            image,
            FalseColor {
                steps: contract.false_color().steps,
                balance,
                ceilings: levels,
            },
            &|| cancellation.is_cancelled(),
        )
        .map_err(|error| detail_error("false_color", error))?;
        stage.false_color = started.elapsed();
    }
    if let Some(noise) = noise {
        reduce_noise(image, settings, balance, levels, noise, cancellation, stage)?;
    }
    let capture = contract.capture();
    if capture.settings().is_active() {
        let started = Instant::now();
        capture.apply_cpu(image, cancellation)?;
        stage.capture_sharpening = started.elapsed();
    }
    cancellation.checkpoint()?;
    Ok(())
}

/// Chroma, then luminance noise reduction, both driven by the profiles of
/// the stage input.
fn reduce_noise(
    image: &mut LinearRgbImage<f32>,
    settings: NoiseReduction,
    balance: [f32; 3],
    ceilings: [f32; 3],
    noise: NoiseProfiles,
    cancellation: &CancellationToken,
    stage: &mut DetailStage,
) -> Result<(), PipelineError> {
    let is_cancelled = || cancellation.is_cancelled();
    if settings.chroma_active() {
        let started = Instant::now();
        denoise_chroma(
            image,
            ChromaDenoise {
                strength: settings.chroma,
                balance,
                ceilings,
                noise,
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
                noise: noise.luma,
            },
            &is_cancelled,
        )
        .map_err(noise_reduction_error)?;
        stage.luminance_noise_reduction += started.elapsed();
    }
    Ok(())
}

fn noise_reduction_error(error: DetailError) -> PipelineError {
    detail_error("noise_reduction", error)
}

fn detail_error(field: &'static str, error: DetailError) -> PipelineError {
    match error {
        DetailError::Cancelled => PipelineError::Cancelled,
        other => PipelineError::InvalidRecipe {
            field,
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
    fn false_color_runs_alone_and_leaves_green_and_inactive_recipes_untouched() {
        use rohditor_image::LinearRgbSpace;
        // Neutral ramp with one magenta speck (red and blue up, green alone).
        let (w, h) = (9, 7);
        let mut data: Vec<f32> = (0..w * h)
            .flat_map(|i| [0.2 + 0.01 * (i % w) as f32; 3])
            .collect();
        let speck = (3 * w + 4) * 3;
        data[speck] += 0.4;
        data[speck + 2] += 0.4;
        let input =
            LinearRgbImage::new(w, h, w * 3, LinearRgbSpace::CameraNative, data).expect("image");
        let balance = WhiteBalanceGains::identity();
        let token = CancellationToken::new();

        // Steps = 0 does no work and leaves the image bit-identical.
        let mut image = input.clone();
        let stage =
            apply(&mut image, &EditRecipe::default(), balance, balance, &token).expect("apply");
        assert!(stage.provenance.is_none());
        assert_eq!(image, input);

        let mut recipe = EditRecipe::default();
        recipe.false_color.steps = 1;
        let stage = apply(&mut image, &recipe, balance, balance, &token).expect("apply");
        assert_eq!(
            stage.provenance.and_then(|p| p.false_color),
            Some(recipe.false_color)
        );
        assert!(
            stage
                .provenance
                .is_some_and(|p| p.noise_reduction.is_none())
        );
        assert!(stage.scratch_bytes > 0);
        assert!(stage.chroma_noise_reduction.is_zero() && stage.capture_sharpening.is_zero());
        assert_ne!(image, input);
        for y in 0..h {
            for x in 0..w {
                let (a, b) = (
                    image.pixel(x, y).expect("pixel"),
                    input.pixel(x, y).expect("pixel"),
                );
                assert_eq!(a[1], b[1], "green is kept at ({x},{y})");
            }
        }
        // The speck is gone: red and blue are back on the green ramp.
        let fixed = image.pixel(4, 3).expect("pixel");
        assert!((fixed[0] - fixed[1]).abs() < 1e-5 && (fixed[2] - fixed[1]).abs() < 1e-5);
    }

    #[test]
    fn provenance_ignores_the_parked_texture_setting() {
        let mut recipe = EditRecipe::default();
        assert_eq!(DetailProvenance::for_recipe(&recipe), None);
        // False colour is detail work of its own.
        recipe.false_color.steps = 2;
        let false_color = DetailProvenance::for_recipe(&recipe).expect("active");
        assert_eq!(false_color.false_color, Some(recipe.false_color));
        assert!(false_color.noise_reduction.is_none());
        recipe.false_color.steps = 3;
        assert_ne!(DetailProvenance::for_recipe(&recipe), Some(false_color));
        recipe.false_color.steps = 0;
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
