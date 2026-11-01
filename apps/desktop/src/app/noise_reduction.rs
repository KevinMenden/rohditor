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

use eframe::egui;

use rohditor_core::{NoiseProfile, with_default_noise_reduction};
use rohditor_edit::NoiseReduction;

use super::{RohditorApp, default_recipe_for_document};

/// Why a noise estimate was requested.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum NoisePurpose {
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

/// Where a document stands with its noise estimate.
#[derive(Debug, Default)]
pub(super) struct NoiseState {
    stage: NoiseStage,
    /// The estimate belongs to this document's default recipe. Set when a
    /// photo without saved edits is opened with automatic noise reduction on.
    default_applies: bool,
}

impl NoiseState {
    pub(super) fn for_new_photo(automatic: bool) -> Self {
        Self {
            stage: NoiseStage::Idle,
            default_applies: automatic,
        }
    }

    /// The profile the default recipe is derived from, once known.
    pub(super) fn default_profile(&self) -> Option<&NoiseProfile> {
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

    /// What the Detail panel shows next to the Auto button.
    pub(super) fn suggestion(&self) -> crate::ui::adjustment_panel::NoiseSuggestion {
        use crate::ui::adjustment_panel::NoiseSuggestion;
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

impl RohditorApp {
    /// Profile the open document in the background, unless that is already
    /// done or under way.
    pub(super) fn request_noise_estimate(&mut self, document_id: u64, purpose: NoisePurpose) {
        let render_options = self.settings.render_options();
        let Some(document) = self.document.as_mut().filter(|doc| doc.id == document_id) else {
            return;
        };
        if !matches!(document.noise.stage, NoiseStage::Idle) {
            return;
        }
        let Some(frame) = document.frame.clone() else {
            return;
        };
        match self
            .coordinator
            .estimate_noise(document_id, frame, render_options)
        {
            Ok(()) => document.noise.stage = NoiseStage::Pending(purpose),
            Err(message) => {
                document.warning = Some(format!("Noise estimate: {message}"));
                document.noise.stage = NoiseStage::Ready(None);
            }
        }
    }

    /// The RAW frame arrived: start the default estimate for a new photo.
    pub(super) fn start_default_noise_estimate(&mut self, document_id: u64) {
        if self
            .document
            .as_ref()
            .is_some_and(|doc| doc.id == document_id && doc.noise.default_applies)
        {
            self.request_noise_estimate(document_id, NoisePurpose::Default);
        }
    }

    pub(super) fn noise_profile_ready(
        &mut self,
        context: &egui::Context,
        document_id: u64,
        result: Result<Option<NoiseProfile>, String>,
    ) {
        let Some(document) = self.document.as_mut().filter(|doc| doc.id == document_id) else {
            return;
        };
        let NoiseStage::Pending(purpose) = document.noise.stage else {
            return;
        };
        let profile = match result {
            Ok(profile) => profile,
            Err(message) => {
                tracing::info!(%message, "noise estimate failed; noise reduction stays manual");
                document.warning = Some(format!("Noise estimate: {message}"));
                None
            }
        };
        document.noise.stage = NoiseStage::Ready(profile);
        let changed = match purpose {
            NoisePurpose::Default => apply_default(document),
            NoisePurpose::Suggestion => apply_suggestion(document),
        };
        if changed {
            self.queue_preview(context, document_id);
        }
    }

    /// The Auto button: apply the suggestion, estimating first when needed.
    pub(super) fn apply_noise_suggestion(&mut self, context: &egui::Context) {
        let Some(document) = self.document.as_mut() else {
            return;
        };
        let document_id = document.id;
        match document.noise.stage {
            NoiseStage::Ready(_) => {
                if apply_suggestion(document) {
                    self.queue_preview(context, document_id);
                }
            }
            NoiseStage::Idle => self.request_noise_estimate(document_id, NoisePurpose::Suggestion),
            NoiseStage::Pending(_) => {}
        }
    }
}

/// Store the derived default in a document that still has the neutral setting,
/// as derived data (see `EditSession::apply_derived`). A user's own noise
/// reduction wins.
fn apply_default(document: &mut super::Document) -> bool {
    let target = default_recipe_for_document(document).noise_reduction;
    if document.edits.recipe().noise_reduction != NoiseReduction::default() {
        return false;
    }
    document
        .edits
        .apply_derived(|recipe| recipe.noise_reduction = target)
}

/// Set the colour strength to the suggestion as an undoable user edit; the
/// luminance settings stay as the user left them.
fn apply_suggestion(document: &mut super::Document) -> bool {
    let Some(profile) = document.noise.profile() else {
        return false;
    };
    let chroma = suggested_noise_reduction(profile).chroma;
    let mut next = document.edits.recipe().clone();
    next.noise_reduction.chroma = chroma;
    document.edits.set_discrete(next)
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use rohditor_edit::EditRecipe;

    use super::super::{Document, has_user_adjustments};
    use super::*;

    /// Noise at mid grey of roughly a corpus ISO 1000 photo (sigma 0.0063).
    fn noisy() -> NoiseProfile {
        NoiseProfile {
            a: 2.2e-4,
            b: 0.0,
            bins: 16,
        }
    }

    fn document(automatic: bool, stage: NoiseStage) -> Document {
        let mut document = Document::opening(
            1,
            PathBuf::from("fixture.arw"),
            super::super::default_recipe(None),
            None,
        );
        document.noise = NoiseState {
            stage,
            default_applies: automatic,
        };
        document
    }

    #[test]
    fn the_estimate_joins_the_default_only_for_photos_that_opted_in() {
        let on = document(true, NoiseStage::Ready(Some(noisy())));
        assert!(default_recipe_for_document(&on).noise_reduction.chroma > 0.0);
        // Still estimating, or setting off / saved edits: the default stays neutral.
        let pending = document(true, NoiseStage::Pending(NoisePurpose::Default));
        let off = document(false, NoiseStage::Ready(Some(noisy())));
        for document in [&pending, &off] {
            assert_eq!(
                default_recipe_for_document(document).noise_reduction,
                NoiseReduction::default()
            );
        }
    }

    #[test]
    fn derived_default_is_silent_undo_free_and_not_a_user_adjustment() {
        let mut document = document(true, NoiseStage::Ready(Some(noisy())));
        assert!(apply_default(&mut document));
        assert!(document.edits.recipe().noise_reduction.chroma_active());
        assert!(
            !document.edits.is_dirty(),
            "opening must not create a sidecar"
        );
        assert!(!document.edits.can_undo());
        assert!(!has_user_adjustments(&document));
        // Applying twice changes nothing.
        assert!(!apply_default(&mut document));
    }

    #[test]
    fn derived_default_never_overrides_the_users_noise_reduction() {
        let mut document = document(true, NoiseStage::Ready(Some(noisy())));
        let mut mine = document.edits.recipe().clone();
        mine.noise_reduction.luminance = 0.3;
        assert!(document.edits.set_discrete(mine));
        assert!(!apply_default(&mut document));
        assert_eq!(document.edits.recipe().noise_reduction.chroma, 0.0);
        assert_eq!(document.edits.recipe().noise_reduction.luminance, 0.3);
    }

    #[test]
    fn auto_button_is_one_undoable_edit_that_keeps_luminance() {
        let mut document = document(false, NoiseStage::Ready(Some(noisy())));
        let mut mine = document.edits.recipe().clone();
        mine.noise_reduction.luminance = 0.3;
        assert!(document.edits.set_discrete(mine));
        assert!(apply_suggestion(&mut document));
        let applied = document.edits.recipe().noise_reduction;
        assert!(applied.chroma > 0.0);
        assert_eq!(applied.luminance, 0.3);
        assert!(document.edits.undo());
        assert_eq!(document.edits.recipe().noise_reduction.chroma, 0.0);
        assert_eq!(document.edits.recipe().noise_reduction.luminance, 0.3);
        // No estimate (too small to profile): nothing to apply.
        let mut unknown = self::document(false, NoiseStage::Ready(None));
        assert!(!apply_suggestion(&mut unknown));
    }

    #[test]
    fn reset_all_returns_to_the_derived_default() {
        let mut document = document(true, NoiseStage::Ready(Some(noisy())));
        assert!(apply_default(&mut document));
        let derived = document.edits.recipe().noise_reduction;
        let mut mine = document.edits.recipe().clone();
        mine.noise_reduction = NoiseReduction::default();
        mine.light.exposure_ev = 0.5;
        assert!(document.edits.set_discrete(mine));
        assert!(has_user_adjustments(&document));
        let defaults: EditRecipe = default_recipe_for_document(&document);
        assert!(document.edits.reset_to(defaults));
        assert_eq!(document.edits.recipe().noise_reduction, derived);
        assert!(!has_user_adjustments(&document));
    }

    #[test]
    fn panel_suggestion_follows_the_estimate() {
        use crate::ui::adjustment_panel::NoiseSuggestion;
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
