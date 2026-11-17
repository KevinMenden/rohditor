//! Noise-reduction defaults derived from the frame's own noise estimate.
//!
//! Any active noise reduction keeps a document off the GPU (see
//! `cpu_only_stage_reason`), and low-ISO files gain almost nothing from it, so
//! a fixed default for every file would cost the common case for the sake of
//! the noisy one. The estimate is the same luma [`NoiseProfile`] the filters
//! use; it is judged at mid grey and mapped to a chroma strength. Luminance
//! reduction, false-colour suppression and defringe stay off: on the private
//! corpus their effect is a per-image taste call (see `detail-corpus`).

use rohditor_detail::{NeverCancelled, NoiseProfile, estimate_luma_noise};
use rohditor_edit::{EditRecipe, FalseColorSuppression, NoiseReduction, WhiteBalance};
use rohditor_raw::RawFrame;

use crate::{CancellationToken, CpuPipeline, PipelineError, RenderOptions};

/// Signal level the noise is judged at: mid grey of the balanced luma.
const REFERENCE_SIGNAL: f32 = 0.18;
/// Luma sigma at the reference signal below which chroma reduction stays off
/// (about the corpus' ISO 100–200 files: 0.0015–0.0027 on a 24 MP APS-C body).
const CHROMA_OFF_BELOW_SIGMA: f32 = 0.0025;
/// Sigma at which the default reaches [`CHROMA_MAX`] (corpus ISO 1000: 0.0063).
const CHROMA_FULL_SIGMA: f32 = 0.006;
/// Strength just above the threshold, so the step from off is not a rounding error.
const CHROMA_MIN: f32 = 0.15;
const CHROMA_MAX: f32 = 0.4;

/// Chroma strength for a luma noise `sigma` at [`REFERENCE_SIGNAL`].
fn chroma_for_sigma(sigma: f32) -> f32 {
    if sigma < CHROMA_OFF_BELOW_SIGMA {
        return 0.0;
    }
    let t = ((sigma - CHROMA_OFF_BELOW_SIGMA) / (CHROMA_FULL_SIGMA - CHROMA_OFF_BELOW_SIGMA))
        .clamp(0.0, 1.0);
    CHROMA_MIN + t * (CHROMA_MAX - CHROMA_MIN)
}

/// `recipe` with the noise reduction a frame with this noise profile starts
/// from. `None` (no estimate, e.g. a frame too small to profile) leaves it off.
/// Only the noise-reduction settings change.
#[must_use]
pub fn with_default_noise_reduction(
    mut recipe: EditRecipe,
    noise: Option<&NoiseProfile>,
) -> EditRecipe {
    recipe.noise_reduction = NoiseReduction {
        chroma: noise.map_or(0.0, |noise| chroma_for_sigma(noise.sigma(REFERENCE_SIGNAL))),
        ..NoiseReduction::default()
    };
    recipe
}

/// Luma noise profile of the as-shot-balanced demosaic of `frame`, measured
/// before any detail stage (so it does not depend on what `recipe` enables
/// there). Costs a full-resolution reconstruction. `None` when the frame is too
/// small to profile.
pub fn estimate_noise_profile(
    pipeline: &CpuPipeline,
    frame: &RawFrame,
    recipe: &EditRecipe,
    options: RenderOptions,
) -> Result<Option<NoiseProfile>, PipelineError> {
    let mut recipe = recipe.clone();
    recipe.noise_reduction = NoiseReduction::default();
    recipe.false_color = FalseColorSuppression::default();
    recipe.capture_sharpening.enabled = false;
    let source =
        pipeline.prepare_export_source(frame, &recipe, options, &CancellationToken::new())?;
    let gains = source.white_balance_gains(WhiteBalance::AsShot)?;
    estimate_luma_noise(
        source.image(),
        [gains.red, gains.green, gains.blue],
        &NeverCancelled,
    )
    .map_err(|error| PipelineError::InvalidRecipe {
        field: "noise_reduction",
        reason: error.to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A profile whose sigma at the reference signal is `sigma`.
    fn profile(sigma: f32) -> NoiseProfile {
        NoiseProfile {
            a: 0.0,
            b: sigma * sigma,
            bins: 16,
        }
    }

    fn chroma(noise: Option<&NoiseProfile>) -> f32 {
        with_default_noise_reduction(EditRecipe::default(), noise)
            .noise_reduction
            .chroma
    }

    #[test]
    fn clean_files_stay_off_and_noisy_files_ramp_up_to_the_cap() {
        assert_eq!(chroma(None), 0.0);
        assert_eq!(chroma(Some(&profile(0.0015))), 0.0);
        assert_eq!(chroma(Some(&profile(0.0024))), 0.0);
        let at_threshold = chroma(Some(&profile(0.0026)));
        assert!((CHROMA_MIN..CHROMA_MIN + 0.01).contains(&at_threshold));
        let mid = chroma(Some(&profile(0.0045)));
        assert!(at_threshold < mid && mid < CHROMA_MAX);
        assert_eq!(chroma(Some(&profile(0.006))), CHROMA_MAX);
        assert_eq!(chroma(Some(&profile(0.05))), CHROMA_MAX);
    }

    #[test]
    fn only_noise_reduction_changes_and_luminance_stays_off() {
        let mut recipe = EditRecipe::default();
        recipe.light.exposure_ev = 0.7;
        let derived = with_default_noise_reduction(recipe.clone(), Some(&profile(0.01)));
        assert!(derived.noise_reduction.chroma_active());
        assert!(!derived.noise_reduction.luminance_active());
        let mut expected = recipe;
        expected.noise_reduction = derived.noise_reduction;
        assert_eq!(derived, expected);
        // A stale value is replaced, not kept.
        let mut stale = EditRecipe::default();
        stale.noise_reduction.chroma = 0.9;
        assert_eq!(
            with_default_noise_reduction(stale, None).noise_reduction,
            NoiseReduction::default()
        );
    }
}
