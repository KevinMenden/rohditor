//! Applying presets to the open photo.

use rohditor_edit::{EditRecipe, LensProfileSelection, RecipeSection, changed_sections};

use super::OpenDocument;
use crate::presets::{ApplyOptions, Preset};

/// The preset applied last, whose amount can be changed until the recipe is.
#[derive(Debug)]
pub(super) struct AppliedPreset {
    preset: Preset,
    amount: f32,
    /// The recipe it was applied to and the one that made.
    base: EditRecipe,
    result: EditRecipe,
}

impl OpenDocument {
    /// Apply a preset as one undo step. Returns whether the recipe changed, and
    /// says so if not.
    pub fn apply_preset(&mut self, preset: &Preset, amount: f32) -> bool {
        let amount = amount.clamp(0.0, 1.0);
        let base = self.edits.recipe().clone();
        let (result, lens_left_out) = self.with_preset(preset, &base, amount);
        let problem = lens_left_out.then(|| {
            let problem = self.lens_profiles.problem();
            let problem =
                problem.unwrap_or_else(|| "The photo does not have its lens profile".to_owned());
            format!(
                "\"{}\" was applied without its optics. {problem}",
                preset.name()
            )
        });
        if !self.edits.set_discrete(result.clone()) {
            self.notice = Some(format!("\"{}\" changes nothing here", preset.name()));
            return false;
        }
        if problem.is_some() {
            self.notice = problem;
        }
        self.applied_preset = Some(AppliedPreset {
            preset: preset.clone(),
            amount,
            base,
            result,
        });
        true
    }

    /// The name and amount of the preset applied last, while nothing else was
    /// changed since.
    pub fn applied_preset(&self) -> Option<(&str, f32)> {
        let applied = self.applied_preset.as_ref()?;
        let same = same_edits(&applied.result, self.edits.recipe());
        same.then(|| (applied.preset.name(), applied.amount))
    }

    /// Change the amount of [`Self::applied_preset`], within the gesture in
    /// progress. Returns whether the recipe changed.
    pub fn set_preset_amount(&mut self, amount: f32) -> bool {
        if self.applied_preset().is_none() {
            return false;
        }
        let Some(mut applied) = self.applied_preset.take() else {
            return false;
        };
        applied.amount = amount.clamp(0.0, 1.0);
        (applied.result, _) = self.with_preset(&applied.preset, &applied.base, applied.amount);
        let changed = self.edits.set_during_gesture(applied.result.clone());
        self.applied_preset = Some(applied);
        changed
    }

    /// `base` with the preset, and whether its optics were left out.
    fn with_preset(&self, preset: &Preset, base: &EditRecipe, amount: f32) -> (EditRecipe, bool) {
        let mut options = ApplyOptions {
            amount,
            ..ApplyOptions::default()
        };
        let mut next = preset.apply(base, &options);
        let lens = &next.optics.profile;
        // A lens profile that the photo does not have could not be rendered.
        if *lens != base.optics.profile
            && *lens != LensProfileSelection::Off
            && self.lens_profiles.profile(lens).is_none()
        {
            options.unavailable.push(RecipeSection::Optics);
            next = preset.apply(base, &options);
        }
        let left_out = !options.unavailable.is_empty();
        (self.follow_camera_profile(base, next), left_out)
    }

    /// What a new preset should hold: the edited sections, except for those that
    /// describe this one photo (white balance, lens, crop, masks, ...), which
    /// rarely belong in a preset.
    pub fn suggested_preset_sections(&self) -> Vec<RecipeSection> {
        let mut sections = changed_sections(self.edits.recipe(), &self.default_recipe());
        sections.retain(|section| !section.is_photo_specific());
        sections
    }
}

/// Whether the recipes differ in no more than the fitted camera look, which is
/// derived from the photo.
fn same_edits(one: &EditRecipe, other: &EditRecipe) -> bool {
    let chosen = |recipe: &EditRecipe| {
        let mut recipe = recipe.clone();
        recipe.rendering.camera_tone = None;
        recipe.rendering.camera_chroma = None;
        recipe
    };
    chosen(one) == chosen(other)
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use rohditor_core::default_recipe;
    use rohditor_edit::RecipeSection as Section;

    use super::*;

    fn document() -> OpenDocument {
        OpenDocument::opening(1, PathBuf::from("fixture.arw"), default_recipe(None), None)
    }

    fn brighter() -> Preset {
        let mut recipe = EditRecipe::default();
        recipe.light.exposure_ev = 1.0;
        Preset::new("Brighter", &recipe, &[Section::Tone]).expect("valid preset")
    }

    fn exposure(document: &OpenDocument) -> f32 {
        document.edits.recipe().light.exposure_ev
    }

    #[test]
    fn the_amount_is_relative_to_the_recipe_before() {
        let mut document = document();
        assert!(document.apply_preset(&brighter(), 1.0));
        assert_eq!(exposure(&document), 1.0);
        document.edits.begin_gesture();
        for amount in [0.5, 0.0, 0.25] {
            assert!(document.set_preset_amount(amount));
            assert_eq!(exposure(&document), amount);
        }
        document.edits.finish_gesture();
        assert_eq!(document.applied_preset(), Some(("Brighter", 0.25)));
        assert!(document.edits.undo());
        assert_eq!(
            exposure(&document),
            1.0,
            "a change of the amount is one step"
        );
        assert!(document.edits.undo());
        assert_eq!(exposure(&document), 0.0);
    }

    #[test]
    fn the_amount_is_at_most_all_of_the_preset() {
        let mut document = document();
        assert!(document.apply_preset(&brighter(), 2.0));
        assert_eq!(document.applied_preset(), Some(("Brighter", 1.0)));
    }

    #[test]
    fn another_edit_ends_changing_the_amount() {
        let mut document = document();
        assert!(document.apply_preset(&brighter(), 1.0));
        let mut next = document.edits.recipe().clone();
        next.light.contrast = 0.2;
        assert!(document.edits.set_discrete(next));
        assert_eq!(document.applied_preset(), None);
        assert!(!document.set_preset_amount(0.5));
        assert_eq!(exposure(&document), 1.0);
    }

    #[test]
    fn a_preset_without_an_effect_says_so() {
        let mut document = document();
        assert!(document.apply_preset(&brighter(), 1.0));
        assert!(document.notice.is_none());
        assert!(!document.apply_preset(&brighter(), 1.0));
        assert!(document.notice.is_some());
    }

    #[test]
    fn a_lens_profile_that_the_photo_lacks_is_left_out() {
        let mut recipe = EditRecipe::default();
        recipe.light.exposure_ev = 1.0;
        recipe.optics.profile = LensProfileSelection::Automatic;
        let preset =
            Preset::new("Lens", &recipe, &[Section::Tone, Section::Optics]).expect("valid preset");
        let mut document = document();
        assert!(document.apply_preset(&preset, 1.0));
        assert_eq!(exposure(&document), 1.0);
        assert!(document.notice.is_some(), "it says so");
        assert_eq!(
            document.edits.recipe().optics.profile,
            LensProfileSelection::Off
        );
    }

    #[test]
    fn a_new_preset_holds_the_edits_that_are_not_photo_specific() {
        let mut document = document();
        let mut next = document.edits.recipe().clone();
        next.light.exposure_ev = 1.0;
        next.presence.clarity = 0.3;
        next.optics.profile = LensProfileSelection::Automatic;
        assert!(document.edits.set_discrete(next));
        assert_eq!(
            document.suggested_preset_sections(),
            [Section::Tone, Section::Presence]
        );
    }
}
