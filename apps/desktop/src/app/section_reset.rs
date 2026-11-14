//! Resetting one section of the Edit panel to its defaults.
//!
//! The panel only reports which section's reset button was clicked; this maps
//! a section to the part of the recipe it owns.

use rohditor_edit::EditRecipe;

use crate::ui::adjustment_panel::PanelSection;

/// `recipe` with the fields owned by `section` taken from `defaults`.
/// Everything outside the section, including other sections' edits, is kept.
pub(super) fn reset_section(
    recipe: &EditRecipe,
    defaults: &EditRecipe,
    section: PanelSection,
) -> EditRecipe {
    let mut next = recipe.clone();
    match section {
        PanelSection::Tone => {
            next.light.exposure_ev = defaults.light.exposure_ev;
            next.light.contrast = defaults.light.contrast;
            next.light.highlights = defaults.light.highlights;
            next.light.shadows = defaults.light.shadows;
            next.light.whites = defaults.light.whites;
            next.light.blacks = defaults.light.blacks;
        }
        PanelSection::WhiteBalance => next.color.white_balance = defaults.color.white_balance,
        PanelSection::Saturation => {
            next.color.saturation = defaults.color.saturation;
            next.color.vibrance = defaults.color.vibrance;
        }
        PanelSection::ToneCurve => next.light.tone_curve = defaults.light.tone_curve.clone(),
        PanelSection::HighlightReconstruction => next.raw.highlights = defaults.raw.highlights,
        PanelSection::ColorProfile => {
            next.color.camera_profile = defaults.color.camera_profile.clone();
            // Only the choice: the fitted camera look is derived data that
            // belongs to the photo and stays with it.
            next.rendering.profile = defaults.rendering.profile;
        }
        PanelSection::ColorMixer => next.color.hsl = defaults.color.hsl.clone(),
        PanelSection::ColorGrading => next.color.grading = defaults.color.grading,
        PanelSection::NoiseReduction => next.noise_reduction = defaults.noise_reduction,
        PanelSection::CaptureSharpening => next.capture_sharpening = defaults.capture_sharpening,
        PanelSection::FalseColor => next.false_color = defaults.false_color,
        PanelSection::Defringe => next.defringe = defaults.defringe,
        PanelSection::Optics => next.optics = defaults.optics.clone(),
    }
    next
}

#[cfg(test)]
mod tests {
    use rohditor_edit::{
        CameraProfileSelection, CameraTone, HighlightMethod, LensProfileSelection,
        RenderingProfileSelection, WhiteBalance,
    };

    use super::*;

    fn edited() -> EditRecipe {
        let mut recipe = EditRecipe::default();
        recipe.light.exposure_ev = 0.7;
        recipe.light.tone_curve.shadows = 0.2;
        recipe.color.saturation = 1.4;
        recipe.color.hsl.channels[2].hue = 0.3;
        recipe
    }

    #[test]
    fn resetting_a_section_leaves_the_others_alone() {
        let recipe = edited();
        let defaults = EditRecipe::default();

        let tone = reset_section(&recipe, &defaults, PanelSection::Tone);
        assert_eq!(tone.light.exposure_ev, 0.0);
        assert_eq!(tone.light.tone_curve.shadows, 0.2);
        assert_eq!(tone.color.saturation, 1.4);

        let curve = reset_section(&recipe, &defaults, PanelSection::ToneCurve);
        assert_eq!(curve.light.tone_curve, defaults.light.tone_curve);
        assert_eq!(curve.light.exposure_ev, 0.7);
    }

    #[test]
    fn resetting_a_section_takes_the_documents_defaults_not_neutral_values() {
        let recipe = edited();
        let mut defaults = EditRecipe::default();
        defaults.color.saturation = 1.1;

        let reset = reset_section(&recipe, &defaults, PanelSection::Saturation);
        assert_eq!(reset.color.saturation, 1.1);
        assert_eq!(reset.color.hsl, recipe.color.hsl);
    }

    const ALL_SECTIONS: [PanelSection; 13] = [
        PanelSection::Tone,
        PanelSection::WhiteBalance,
        PanelSection::Saturation,
        PanelSection::ToneCurve,
        PanelSection::HighlightReconstruction,
        PanelSection::ColorProfile,
        PanelSection::ColorMixer,
        PanelSection::ColorGrading,
        PanelSection::NoiseReduction,
        PanelSection::CaptureSharpening,
        PanelSection::FalseColor,
        PanelSection::Defringe,
        PanelSection::Optics,
    ];

    /// A recipe where every field a section owns differs from the defaults.
    fn fully_edited() -> EditRecipe {
        let mut recipe = EditRecipe::default();
        recipe.light.exposure_ev = 0.7;
        recipe.light.contrast = 0.1;
        recipe.light.highlights = -0.2;
        recipe.light.shadows = 0.3;
        recipe.light.whites = 0.1;
        recipe.light.blacks = -0.1;
        recipe.light.tone_curve.shadows = 0.2;
        recipe.color.white_balance = WhiteBalance::AsShot;
        recipe.color.saturation = 1.4;
        recipe.color.vibrance = 0.2;
        recipe.color.hsl.channels[2].hue = 0.3;
        recipe.color.grading.shadows[0] = 0.2;
        recipe.color.camera_profile = CameraProfileSelection::Automatic;
        recipe.rendering.profile = RenderingProfileSelection::MATCHED;
        recipe.raw.highlights.method = HighlightMethod::Off;
        recipe.noise_reduction.chroma = 0.5;
        recipe.capture_sharpening.enabled = true;
        recipe.false_color.steps = 2;
        recipe.defringe.purple_amount = 0.5;
        recipe.optics.profile = LensProfileSelection::Automatic;
        recipe
    }

    #[test]
    fn every_edited_field_is_owned_by_exactly_the_sections_that_reset_it() {
        let edited = fully_edited();
        let defaults = EditRecipe::default();

        // Together the sections cover every field the edits touched.
        let all = ALL_SECTIONS.iter().fold(edited.clone(), |recipe, section| {
            reset_section(&recipe, &defaults, *section)
        });
        assert_eq!(all, defaults);

        // Each section owns something: without it, the rest leave the recipe
        // different from the defaults. A field copied from the wrong place in
        // the mapping shows up here as a section that owns nothing.
        for section in ALL_SECTIONS {
            let others = ALL_SECTIONS
                .iter()
                .filter(|other| **other != section)
                .fold(edited.clone(), |recipe, other| {
                    reset_section(&recipe, &defaults, *other)
                });
            assert_ne!(others, defaults, "{section:?} resets nothing of its own");
            assert_eq!(
                reset_section(&others, &defaults, section),
                defaults,
                "{section:?} does not reset everything it owns"
            );
        }
    }

    #[test]
    fn colour_profile_reset_keeps_the_fitted_camera_look() {
        let mut recipe = fully_edited();
        recipe.rendering.profile = RenderingProfileSelection::default();
        recipe.rendering.camera_tone = Some(CameraTone::static_fallback());
        let mut defaults = EditRecipe::default();
        defaults.rendering.profile = RenderingProfileSelection::MATCHED;

        let reset = reset_section(&recipe, &defaults, PanelSection::ColorProfile);
        assert_eq!(reset.rendering.profile, RenderingProfileSelection::MATCHED);
        assert_eq!(reset.rendering.camera_tone, recipe.rendering.camera_tone);
    }
}
