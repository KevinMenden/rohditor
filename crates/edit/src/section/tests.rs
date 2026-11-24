use std::collections::BTreeMap;

use super::*;
use crate::test_support::{changed_paths, fully_edited, leaves};
use crate::{EditRecipe, HighlightMethod, RenderingProfileSelection, WhiteBalance};

#[test]
fn every_recipe_field_belongs_to_exactly_one_section() {
    let defaults = EditRecipe::default();
    let edited = fully_edited();
    let everything = changed_paths(&defaults, &edited);

    // The fixture must touch every field, or a new field could slip through
    // unnoticed. Add new fields to `fully_edited`, then to a section.
    // (Two values cannot differ in a valid recipe: the schema version and the
    // current process version.)
    let fixed = [
        "recipe.schema_version",
        "recipe.rendering.profile.process_version",
    ];
    for path in leaves(&defaults).keys() {
        assert!(
            fixed.contains(&path.as_str()) || everything.contains(path),
            "fully_edited() leaves {path} at its default"
        );
    }

    let mut owners: BTreeMap<String, RecipeSection> = BTreeMap::new();
    for section in RecipeSection::ALL {
        let paths = changed_paths(&defaults, &copy_sections(&defaults, &edited, &[section]));
        assert!(!paths.is_empty(), "{section:?} owns no field");
        for path in paths {
            if let Some(other) = owners.insert(path.clone(), section) {
                panic!("{path} is owned by both {other:?} and {section:?}");
            }
        }
    }
    let unowned: Vec<_> = everything
        .iter()
        .filter(|path| !owners.contains_key(*path))
        .collect();
    assert!(unowned.is_empty(), "fields in no section: {unowned:?}");
}

#[test]
fn only_per_photo_sections_are_photo_specific() {
    let photo_specific: Vec<_> = RecipeSection::ALL
        .into_iter()
        .filter(|section| section.is_photo_specific())
        .collect();
    assert_eq!(
        photo_specific,
        [
            RecipeSection::WhiteBalance,
            RecipeSection::ColorProfile,
            RecipeSection::CameraLook,
            RecipeSection::Optics,
            RecipeSection::Geometry,
            RecipeSection::LocalMasks,
        ]
    );
}

#[test]
fn copying_a_section_leaves_the_others_alone() {
    let mut recipe = EditRecipe::default();
    recipe.light.exposure_ev = 0.7;
    recipe.light.tone_curve.shadows = 0.2;
    recipe.color.saturation = 1.4;
    recipe.color.hsl.channels[2].hue = 0.3;
    let defaults = EditRecipe::default();

    let tone = copy_sections(&recipe, &defaults, &[RecipeSection::Tone]);
    assert_eq!(tone.light.exposure_ev, 0.0);
    assert_eq!(tone.light.tone_curve.shadows, 0.2);
    assert_eq!(tone.color.saturation, 1.4);

    let curve = copy_sections(&recipe, &defaults, &[RecipeSection::ToneCurve]);
    assert_eq!(curve.light.tone_curve, defaults.light.tone_curve);
    assert_eq!(curve.light.exposure_ev, 0.7);

    let saturation = copy_sections(&recipe, &defaults, &[RecipeSection::Saturation]);
    assert_eq!(saturation.color.saturation, defaults.color.saturation);
    assert_eq!(saturation.color.hsl, recipe.color.hsl);
}

#[test]
fn copying_takes_the_sources_values_not_neutral_ones() {
    let recipe = fully_edited();
    let mut source = EditRecipe::default();
    source.color.saturation = 1.1;

    let copied = copy_sections(&recipe, &source, &[RecipeSection::Saturation]);
    assert_eq!(copied.color.saturation, 1.1);
    assert_eq!(copied.color.vibrance, source.color.vibrance);
    assert_eq!(copied.color.hsl, recipe.color.hsl);
}

#[test]
fn copying_all_sections_gives_the_source_but_keeps_the_schema_version() {
    let defaults = EditRecipe::default();
    let mut base = fully_edited();
    base.schema_version += 1;

    let copied = copy_sections(&base, &defaults, &RecipeSection::ALL);
    assert_eq!(
        copied,
        EditRecipe {
            schema_version: base.schema_version,
            ..defaults
        }
    );
}

#[test]
fn colour_profile_choice_and_fitted_look_are_separate_sections() {
    let mut recipe = fully_edited();
    recipe.rendering.profile = RenderingProfileSelection::default();
    let mut source = EditRecipe::default();
    source.rendering.profile = RenderingProfileSelection::MATCHED;

    // Resetting the profile choice keeps the fitted look of the photo ...
    let profile = copy_sections(&recipe, &source, &[RecipeSection::ColorProfile]);
    assert_eq!(
        profile.rendering.profile,
        RenderingProfileSelection::MATCHED
    );
    assert_eq!(profile.rendering.camera_tone, recipe.rendering.camera_tone);
    assert_eq!(
        profile.rendering.camera_chroma,
        recipe.rendering.camera_chroma
    );
    assert_eq!(profile.color.camera_profile, source.color.camera_profile);

    // ... and the look only moves when asked for.
    let look = copy_sections(&recipe, &source, &[RecipeSection::CameraLook]);
    assert_eq!(look.rendering.camera_tone, None);
    assert_eq!(look.rendering.profile, recipe.rendering.profile);
}

#[test]
fn listing_a_section_twice_does_not_blend_it_twice() {
    let base = EditRecipe::default();
    let preset = fully_edited();
    let once = blend_sections(&base, &preset, &[RecipeSection::Tone], 0.25);
    let twice = blend_sections(
        &base,
        &preset,
        &[RecipeSection::Tone, RecipeSection::Tone],
        0.25,
    );
    assert_eq!(once, twice);
}

#[test]
fn blend_ends_are_exactly_the_base_and_the_preset() {
    let base = fully_edited();
    let mut preset = EditRecipe::default();
    preset.light.exposure_ev = 0.123_456_79;
    preset.light.shadows = -0.37;

    let none = blend_sections(&base, &preset, &RecipeSection::ALL, 0.0);
    assert_eq!(none, base);

    let full = blend_sections(&base, &preset, &RecipeSection::ALL, 1.0);
    assert_eq!(full, preset);
    assert_eq!(full.light.exposure_ev, 0.123_456_79);

    // Out-of-range amounts are clamped; NaN blends nothing.
    assert_eq!(
        blend_sections(&base, &preset, &RecipeSection::ALL, 3.0),
        full
    );
    assert_eq!(
        blend_sections(&base, &preset, &RecipeSection::ALL, -1.0),
        none
    );
    assert_eq!(
        blend_sections(&base, &preset, &RecipeSection::ALL, f32::NAN),
        none
    );
}

#[test]
fn blend_interpolates_numbers_in_the_chosen_sections_only() {
    let base = EditRecipe::default();
    let mut preset = EditRecipe::default();
    preset.light.exposure_ev = 1.0;
    preset.light.shadows = -0.8;
    preset.light.tone_curve.lights = 0.4;
    preset.color.hsl.channels[2].hue = 0.6;
    preset.color.grading.midtones = [0.2, 0.0, -0.4];
    preset.defringe.purple_hue = [260.0, 300.0];

    let blended = blend_sections(
        &base,
        &preset,
        &[
            RecipeSection::Tone,
            RecipeSection::ColorMixer,
            RecipeSection::ColorGrading,
            RecipeSection::Defringe,
        ],
        0.25,
    );
    assert!((blended.light.exposure_ev - 0.25).abs() < 1.0e-6);
    assert!((blended.light.shadows + 0.2).abs() < 1.0e-6);
    assert!((blended.color.hsl.channels[2].hue - 0.15).abs() < 1.0e-6);
    assert_eq!(blended.color.hsl.channels[1], base.color.hsl.channels[1]);
    assert!((blended.color.grading.midtones[0] - 0.05).abs() < 1.0e-6);
    assert!((blended.color.grading.midtones[2] + 0.1).abs() < 1.0e-6);
    let hue = base.defringe.purple_hue;
    let expected = [
        hue[0] + (260.0 - hue[0]) * 0.25,
        hue[1] + (300.0 - hue[1]) * 0.25,
    ];
    assert!(
        blended
            .defringe
            .purple_hue
            .iter()
            .zip(expected)
            .all(|(got, want)| (got - want).abs() < 1.0e-4)
    );
    // Not listed: untouched even though the preset differs.
    assert_eq!(blended.light.tone_curve, base.light.tone_curve);
}

#[test]
fn blend_switches_non_numeric_fields_at_half_strength() {
    let base = EditRecipe::default();
    let preset = fully_edited();
    let sections = [
        RecipeSection::Optics,
        RecipeSection::Geometry,
        RecipeSection::LocalMasks,
        RecipeSection::ColorProfile,
        RecipeSection::CameraLook,
    ];

    let below = blend_sections(&base, &preset, &sections, 0.49);
    assert_eq!(below, base);

    let at = blend_sections(&base, &preset, &sections, 0.5);
    assert_eq!(at.optics, preset.optics);
    assert_eq!(at.geometry, preset.geometry);
    assert_eq!(at.local, preset.local);
    assert_eq!(at.color.camera_profile, preset.color.camera_profile);
    assert_eq!(at.rendering, preset.rendering);
}

#[test]
fn blend_mixes_numbers_and_switches_flags_inside_one_section() {
    let base = EditRecipe::default();
    let preset = fully_edited();

    // Sharpening: the amount is part way while the switch has not flipped yet.
    let early = blend_sections(&base, &preset, &[RecipeSection::CaptureSharpening], 0.4);
    assert!(!early.capture_sharpening.enabled);
    assert!(
        (early.capture_sharpening.amount
            - (base.capture_sharpening.amount
                + (preset.capture_sharpening.amount - base.capture_sharpening.amount) * 0.4))
            .abs()
            < 1.0e-6
    );
    let late = blend_sections(&base, &preset, &[RecipeSection::CaptureSharpening], 0.6);
    assert!(late.capture_sharpening.enabled);

    // Highlight reconstruction: the method switches, the thresholds mix.
    let below = blend_sections(
        &base,
        &preset,
        &[RecipeSection::HighlightReconstruction],
        0.4,
    );
    assert_eq!(below.raw.highlights.method, base.raw.highlights.method);
    assert_ne!(
        below.raw.highlights.clip.threshold,
        base.raw.highlights.clip.threshold
    );
    let above = blend_sections(
        &base,
        &preset,
        &[RecipeSection::HighlightReconstruction],
        0.6,
    );
    assert_eq!(above.raw.highlights.method, HighlightMethod::Opposed);
}

#[test]
fn blend_rounds_counts_to_the_nearest_whole_number() {
    let mut base = EditRecipe::default();
    base.false_color.steps = 1;
    let mut preset = EditRecipe::default();
    preset.false_color.steps = 4;
    for (amount, steps) in [(0.1, 1), (0.2, 2), (0.5, 3), (0.9, 4)] {
        let blended = blend_sections(&base, &preset, &[RecipeSection::FalseColor], amount);
        assert_eq!(blended.false_color.steps, steps, "at amount {amount}");
    }
}

#[test]
fn blend_white_balance_mixes_within_a_mode_and_switches_between_modes() {
    let mut base = EditRecipe::default();
    base.color.white_balance = WhiteBalance::TemperatureTint {
        temperature: 5000.0,
        tint: 10.0,
    };
    let mut preset = base.clone();
    preset.color.white_balance = WhiteBalance::TemperatureTint {
        temperature: 6000.0,
        tint: -10.0,
    };
    let section = [RecipeSection::WhiteBalance];

    let mixed = blend_sections(&base, &preset, &section, 0.25);
    assert_eq!(
        mixed.color.white_balance,
        WhiteBalance::TemperatureTint {
            temperature: 5250.0,
            tint: 5.0
        }
    );

    preset.color.white_balance = WhiteBalance::AsShot;
    assert_eq!(
        blend_sections(&base, &preset, &section, 0.4)
            .color
            .white_balance,
        base.color.white_balance
    );
    assert_eq!(
        blend_sections(&base, &preset, &section, 0.5)
            .color
            .white_balance,
        WhiteBalance::AsShot
    );
}

#[test]
fn a_blend_of_valid_recipes_is_valid() {
    let base = EditRecipe::default();
    let preset = fully_edited();
    preset.validate().expect("fixture is valid");
    for amount in [0.0, 0.3, 0.5, 0.7, 1.0] {
        blend_sections(&base, &preset, &RecipeSection::ALL, amount)
            .validate()
            .unwrap_or_else(|error| panic!("invalid at {amount}: {error:?}"));
    }
}

#[test]
fn changed_sections_lists_exactly_the_edited_sections() {
    let defaults = EditRecipe::default();
    assert!(changed_sections(&defaults, &defaults).is_empty());

    let mut recipe = defaults.clone();
    recipe.light.exposure_ev = 0.5;
    recipe.presence.clarity = 20.0;
    assert_eq!(
        changed_sections(&recipe, &defaults),
        [RecipeSection::Tone, RecipeSection::Presence]
    );

    // Every section of a fully edited recipe differs from the defaults.
    assert_eq!(
        changed_sections(&fully_edited(), &defaults),
        RecipeSection::ALL
    );
}

#[test]
fn section_names_round_trip_through_serde_and_have_labels() {
    for section in RecipeSection::ALL {
        let json = serde_json::to_string(&section).expect("serialize section");
        assert_eq!(
            serde_json::from_str::<RecipeSection>(&json).expect("deserialize section"),
            section
        );
        assert!(!section.label().is_empty());
    }
    assert_eq!(
        serde_json::to_string(&RecipeSection::HighlightReconstruction).expect("serialize section"),
        "\"highlight_reconstruction\""
    );
}
