use super::*;
use crate::test_support::{changed_paths, fully_edited};

/// A photo whose defaults are not neutral.
fn photo_defaults() -> EditRecipe {
    let mut defaults = EditRecipe::default();
    defaults.color.white_balance = WhiteBalance::TemperatureTint {
        temperature: 5_200.0,
        tint: -0.2,
    };
    defaults.noise_reduction.chroma = 0.3;
    defaults
}

#[test]
fn every_parameter_changes_only_itself_and_keeps_the_recipe_valid() {
    let defaults = photo_defaults();
    for parameter in Parameter::all() {
        let range = parameter.range();
        let current = parameter.get(&defaults, &defaults);
        // Small enough not to pull the other end of a hue range along.
        let target = if matches!(parameter, Parameter::FalseColorSteps) {
            2.0
        } else {
            current + (range.maximum - current) * 0.02
        };
        let after = parameter.with_value(&defaults, &defaults, target);
        assert_eq!(parameter.get(&after, &defaults), target, "{parameter:?}");
        assert!(parameter.is_changed(&after, &defaults), "{parameter:?}");
        after
            .validate()
            .unwrap_or_else(|error| panic!("{parameter:?}: {error}"));
        for other in Parameter::all().filter(|&other| other != parameter) {
            assert!(
                !other.is_changed(&after, &defaults),
                "{parameter:?} changed {other:?}"
            );
        }
        // It belongs to the section it claims.
        assert_eq!(
            crate::changed_sections(&after, &defaults),
            [parameter.section()],
            "{parameter:?}"
        );
        let back = parameter.with_value(&after, &defaults, current);
        assert_eq!(back, defaults, "{parameter:?}");
    }
}

#[test]
fn every_number_of_a_recipe_is_a_parameter() {
    let defaults = EditRecipe::default();
    let edited = fully_edited();
    let copied = Parameter::all().fold(defaults.clone(), |recipe, parameter| {
        parameter.with_value(&recipe, &defaults, parameter.get(&edited, &defaults))
    });
    // Choices, switches and what has an editor of its own.
    let not_parameters = [
        "recipe.color.camera_profile",
        "recipe.color.white_balance",
        "recipe.raw.highlights.method",
        "recipe.rendering",
        "recipe.optics",
        "recipe.geometry",
        "recipe.local",
        "recipe.light.tone_curve",
    ];
    for path in changed_paths(&copied, &edited) {
        assert!(
            not_parameters.iter().any(|other| path.starts_with(other)),
            "{path} has no parameter"
        );
    }
}

#[test]
fn white_balance_edits_start_from_the_photos_own_balance() {
    let defaults = photo_defaults();
    let mut recipe = defaults.clone();
    recipe.color.white_balance = WhiteBalance::AsShot;

    let warmer = Parameter::Temperature.with_value(&recipe, &defaults, 7_200.0);
    assert_eq!(
        warmer.color.white_balance,
        WhiteBalance::TemperatureTint {
            temperature: 7_200.0,
            tint: -0.2,
        }
    );
    let greener = Parameter::Tint.with_value(&recipe, &defaults, 0.25);
    assert_eq!(
        greener.color.white_balance,
        WhiteBalance::TemperatureTint {
            temperature: 5_200.0,
            tint: 0.25,
        }
    );
}

#[test]
fn a_balance_without_coordinates_is_changed() {
    let defaults = photo_defaults();
    let multipliers = WhiteBalance::ManualMultipliers {
        red: 1.2,
        green: 1.0,
        blue: 0.9,
    };
    for balance in [WhiteBalance::AsShot, multipliers] {
        let mut recipe = defaults.clone();
        recipe.color.white_balance = balance;
        assert!(Parameter::Temperature.is_changed(&recipe, &defaults));
        assert_eq!(
            crate::changed_sections(&recipe, &defaults),
            [RecipeSection::WhiteBalance]
        );
    }
}

#[test]
fn a_value_is_judged_against_the_photos_default() {
    let defaults = photo_defaults();
    let noise = Parameter::ChromaNoiseReduction;
    assert!(!noise.is_changed(&defaults, &defaults));
    let off = noise.with_value(&defaults, &defaults, 0.0);
    assert!(noise.is_changed(&off, &defaults), "off is an edit here");
}

#[test]
fn rounding_left_by_a_drag_is_not_a_change() {
    let defaults = photo_defaults();
    let almost = Parameter::Exposure.with_value(&defaults, &defaults, 1.0e-6);
    assert_ne!(almost, defaults);
    assert!(!Parameter::Exposure.is_changed(&almost, &defaults));
    assert!(crate::changed_sections(&almost, &defaults).is_empty());
    // One displayed step is.
    let warmer = Parameter::Temperature.with_value(&defaults, &defaults, 5_201.0);
    assert!(Parameter::Temperature.is_changed(&warmer, &defaults));
}

#[test]
fn a_sharpening_amount_of_zero_switches_it_off() {
    let defaults = EditRecipe::default();
    let amount = Parameter::CaptureAmount;
    assert_eq!(amount.get(&defaults, &defaults), 0.0);
    let on = amount.with_value(&defaults, &defaults, 0.3);
    assert!(on.capture_sharpening.enabled);
    assert_eq!(amount.with_value(&on, &defaults, 0.0), defaults);
}

#[test]
fn false_color_steps_are_whole_and_in_range() {
    let defaults = EditRecipe::default();
    let steps = |value| {
        Parameter::FalseColorSteps
            .with_value(&defaults, &defaults, value)
            .false_color
            .steps
    };
    assert_eq!(steps(2.4), 2);
    assert_eq!(steps(99.0), FALSE_COLOR_MAX_STEPS);
    assert_eq!(steps(-1.0), 0);
}
