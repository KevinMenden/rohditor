//! Automatic noise reduction: the per-photo default and the Detail panel's
//! Auto button.
//!
//! A newly opened photo (no saved edits, setting on) is profiled in the
//! background. When the profile arrives the derived colour noise reduction
//! becomes part of the document's *default* recipe: it is applied without
//! entering undo history or dirtying the document, "Reset all" returns to it,
//! and it does not count as a user adjustment. The same estimate backs the
//! Auto button, which applies it as an ordinary, undoable edit for photos that
//! did not get it on open (saved edits, setting off).

use rohditor_core::{NoiseProfile, RenderOptions, with_default_noise_reduction};
use rohditor_edit::NoiseReduction;

use super::OpenDocument;
use crate::coordinator::RenderCoordinator;

/// Why a noise estimate was requested.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NoisePurpose {
    /// The photo's default recipe, applied silently when the estimate arrives.
    Default,
    /// The Auto button: applied as a user edit when the estimate arrives.
    Suggestion,
}

#[derive(Debug, Default)]
enum NoiseStage {
    #[default]
    Idle,
    Pending(NoisePurpose),
    /// `None`: too small to profile, or the estimate failed.
    Ready(Option<NoiseProfile>),
}

/// The noise-reduction strength Auto would set, as far as it is known.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum NoiseSuggestion {
    /// Not estimated yet; clicking Auto estimates first.
    Unknown,
    Estimating,
    /// The frame is too small to profile, or the estimate failed.
    Unavailable,
    Suggested {
        chroma: f32,
    },
}

/// Where a document stands with its noise estimate.
#[derive(Debug, Default)]
pub struct NoiseState {
    stage: NoiseStage,
    /// The estimate belongs to this document's default recipe. Set when a
    /// photo without saved edits is opened with automatic noise reduction on.
    default_applies: bool,
}

impl NoiseState {
    pub fn for_new_photo(automatic: bool) -> Self {
        Self {
            stage: NoiseStage::Idle,
            default_applies: automatic,
        }
    }

    /// The profile the default recipe is derived from, once known.
    pub fn default_profile(&self) -> Option<&NoiseProfile> {
        match (&self.stage, self.default_applies) {
            (NoiseStage::Ready(profile), true) => profile.as_ref(),
            _ => None,
        }
    }

    fn profile(&self) -> Option<&NoiseProfile> {
        match &self.stage {
            NoiseStage::Ready(profile) => profile.as_ref(),
            _ => None,
        }
    }

    pub fn suggestion(&self) -> NoiseSuggestion {
        match &self.stage {
            NoiseStage::Idle => NoiseSuggestion::Unknown,
            NoiseStage::Pending(_) => NoiseSuggestion::Estimating,
            NoiseStage::Ready(None) => NoiseSuggestion::Unavailable,
            NoiseStage::Ready(Some(profile)) => NoiseSuggestion::Suggested {
                chroma: suggested_noise_reduction(profile).chroma,
            },
        }
    }
}

fn suggested_noise_reduction(profile: &NoiseProfile) -> NoiseReduction {
    with_default_noise_reduction(rohditor_edit::EditRecipe::default(), Some(profile))
        .noise_reduction
}

impl OpenDocument {
    /// Profile the photo in the background, unless that is already done or
    /// under way.
    pub fn request_noise_estimate(
        &mut self,
        coordinator: &RenderCoordinator,
        render_options: RenderOptions,
        purpose: NoisePurpose,
    ) {
        if !matches!(self.noise.stage, NoiseStage::Idle) {
            return;
        }
        let Some(frame) = self.frame.clone() else {
            return;
        };
        match coordinator.estimate_noise(self.id, frame, render_options) {
            Ok(()) => self.noise.stage = NoiseStage::Pending(purpose),
            Err(message) => {
                self.warning = Some(format!("Noise estimate: {message}"));
                self.noise.stage = NoiseStage::Ready(None);
            }
        }
    }

    /// The RAW frame arrived: start the default estimate for a new photo.
    pub fn start_default_noise_estimate(
        &mut self,
        coordinator: &RenderCoordinator,
        render_options: RenderOptions,
    ) {
        if self.noise.default_applies {
            self.request_noise_estimate(coordinator, render_options, NoisePurpose::Default);
        }
    }

    /// Record a finished estimate and apply it as its purpose asks. Returns
    /// whether the recipe changed, so the caller must queue a preview.
    pub fn noise_profile_ready(&mut self, result: Result<Option<NoiseProfile>, String>) -> bool {
        let NoiseStage::Pending(purpose) = self.noise.stage else {
            return false;
        };
        let profile = match result {
            Ok(profile) => profile,
            Err(message) => {
                tracing::info!(%message, "noise estimate failed; noise reduction stays manual");
                self.warning = Some(format!("Noise estimate: {message}"));
                None
            }
        };
        self.noise.stage = NoiseStage::Ready(profile);
        match purpose {
            NoisePurpose::Default => self.apply_default_noise_reduction(),
            NoisePurpose::Suggestion => self.apply_noise_suggestion_now(),
        }
    }

    /// The Auto button: apply the suggestion, estimating first when needed.
    /// Returns whether the recipe changed, so the caller must queue a preview.
    pub fn apply_noise_suggestion(
        &mut self,
        coordinator: &RenderCoordinator,
        render_options: RenderOptions,
    ) -> bool {
        match self.noise.stage {
            NoiseStage::Ready(_) => self.apply_noise_suggestion_now(),
            NoiseStage::Idle => {
                self.request_noise_estimate(coordinator, render_options, NoisePurpose::Suggestion);
                false
            }
            NoiseStage::Pending(_) => false,
        }
    }

    /// Store the derived default in a document that still has the neutral
    /// setting, as derived data (see `EditSession::apply_derived`). A user's own
    /// noise reduction wins.
    fn apply_default_noise_reduction(&mut self) -> bool {
        let target = self.default_recipe().noise_reduction;
        if self.edits.recipe().noise_reduction != NoiseReduction::default() {
            return false;
        }
        self.edits
            .apply_derived(|recipe| recipe.noise_reduction = target)
    }

    /// Set the colour strength to the suggestion as an undoable user edit; the
    /// luminance settings stay as the user left them.
    fn apply_noise_suggestion_now(&mut self) -> bool {
        let Some(profile) = self.noise.profile() else {
            return false;
        };
        let chroma = suggested_noise_reduction(profile).chroma;
        let mut next = self.edits.recipe().clone();
        next.noise_reduction.chroma = chroma;
        self.edits.set_discrete(next)
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use rohditor_core::default_recipe;
    use rohditor_edit::EditRecipe;

    use super::*;

    /// Noise at mid grey of roughly a corpus ISO 1000 photo (sigma 0.0063).
    fn noisy() -> NoiseProfile {
        NoiseProfile {
            a: 2.2e-4,
            b: 0.0,
            bins: 16,
        }
    }

    fn document(automatic: bool, stage: NoiseStage) -> OpenDocument {
        let mut document =
            OpenDocument::opening(1, PathBuf::from("fixture.arw"), default_recipe(None), None);
        document.noise = NoiseState {
            stage,
            default_applies: automatic,
        };
        document
    }

    #[test]
    fn the_estimate_joins_the_default_only_for_photos_that_opted_in() {
        let on = document(true, NoiseStage::Ready(Some(noisy())));
        assert!(on.default_recipe().noise_reduction.chroma > 0.0);
        // Still estimating, or setting off / saved edits: the default stays neutral.
        let pending = document(true, NoiseStage::Pending(NoisePurpose::Default));
        let off = document(false, NoiseStage::Ready(Some(noisy())));
        for document in [&pending, &off] {
            assert_eq!(
                document.default_recipe().noise_reduction,
                NoiseReduction::default()
            );
        }
    }

    #[test]
    fn derived_default_is_silent_undo_free_and_not_a_user_adjustment() {
        let mut document = document(true, NoiseStage::Ready(Some(noisy())));
        assert!(document.apply_default_noise_reduction());
        assert!(document.edits.recipe().noise_reduction.chroma_active());
        assert!(
            !document.edits.is_dirty(),
            "opening must not create a sidecar"
        );
        assert!(!document.edits.can_undo());
        assert!(!document.has_user_adjustments());
        // Applying twice changes nothing.
        assert!(!document.apply_default_noise_reduction());
    }

    #[test]
    fn derived_default_never_overrides_the_users_noise_reduction() {
        let mut document = document(true, NoiseStage::Ready(Some(noisy())));
        let mut mine = document.edits.recipe().clone();
        mine.noise_reduction.luminance = 0.3;
        assert!(document.edits.set_discrete(mine));
        assert!(!document.apply_default_noise_reduction());
        assert_eq!(document.edits.recipe().noise_reduction.chroma, 0.0);
        assert_eq!(document.edits.recipe().noise_reduction.luminance, 0.3);
    }

    #[test]
    fn auto_button_is_one_undoable_edit_that_keeps_luminance() {
        let mut document = document(false, NoiseStage::Ready(Some(noisy())));
        let mut mine = document.edits.recipe().clone();
        mine.noise_reduction.luminance = 0.3;
        assert!(document.edits.set_discrete(mine));
        assert!(document.apply_noise_suggestion_now());
        let applied = document.edits.recipe().noise_reduction;
        assert!(applied.chroma > 0.0);
        assert_eq!(applied.luminance, 0.3);
        assert!(document.edits.undo());
        assert_eq!(document.edits.recipe().noise_reduction.chroma, 0.0);
        assert_eq!(document.edits.recipe().noise_reduction.luminance, 0.3);
        // No estimate (too small to profile): nothing to apply.
        let mut unknown = self::document(false, NoiseStage::Ready(None));
        assert!(!unknown.apply_noise_suggestion_now());
    }

    #[test]
    fn reset_all_returns_to_the_derived_default() {
        let mut document = document(true, NoiseStage::Ready(Some(noisy())));
        assert!(document.apply_default_noise_reduction());
        let derived = document.edits.recipe().noise_reduction;
        let mut mine = document.edits.recipe().clone();
        mine.noise_reduction = NoiseReduction::default();
        mine.light.exposure_ev = 0.5;
        assert!(document.edits.set_discrete(mine));
        assert!(document.has_user_adjustments());
        let defaults: EditRecipe = document.default_recipe();
        assert!(document.edits.reset_to(defaults));
        assert_eq!(document.edits.recipe().noise_reduction, derived);
        assert!(!document.has_user_adjustments());
    }

    #[test]
    fn a_stale_result_changes_nothing_and_a_ready_one_applies_by_purpose() {
        let mut document = document(true, NoiseStage::Idle);
        // Nothing was requested: a stray result is ignored.
        assert!(!document.noise_profile_ready(Ok(Some(noisy()))));
        document.noise.stage = NoiseStage::Pending(NoisePurpose::Default);
        assert!(document.noise_profile_ready(Ok(Some(noisy()))));
        assert!(document.edits.recipe().noise_reduction.chroma_active());
        // A failed estimate leaves noise reduction manual and warns.
        let mut failed = self::document(true, NoiseStage::Pending(NoisePurpose::Default));
        assert!(!failed.noise_profile_ready(Err("boom".to_owned())));
        assert_eq!(failed.noise_suggestion(), NoiseSuggestion::Unavailable);
        assert!(failed.warning.is_some());
    }

    #[test]
    fn panel_suggestion_follows_the_estimate() {
        assert_eq!(NoiseState::default().suggestion(), NoiseSuggestion::Unknown);
        assert_eq!(
            document(true, NoiseStage::Pending(NoisePurpose::Suggestion))
                .noise
                .suggestion(),
            NoiseSuggestion::Estimating
        );
        assert_eq!(
            document(true, NoiseStage::Ready(None)).noise.suggestion(),
            NoiseSuggestion::Unavailable
        );
        assert!(matches!(
            document(true, NoiseStage::Ready(Some(noisy()))).noise.suggestion(),
            NoiseSuggestion::Suggested { chroma } if chroma > 0.0
        ));
    }
}
