//! White balance presets, the colour mixer, the grading wheels and the profiles.

use rohditor_edit::{
    CameraProfileSelection, EditRecipe, RecipeSection, RenderingProfileSelection, WhiteBalance,
};

use super::*;

fn recipe(harness: &mut Harness) -> EditRecipe {
    harness.with_session(|session, _| {
        let document = session.document().expect("a photo is open");
        document.edits.recipe().clone()
    })
}

fn defaults(harness: &mut Harness) -> EditRecipe {
    harness.with_session(|session, _| {
        let document = session.document().expect("a photo is open");
        document.default_recipe()
    })
}

/// High enough to show all sections.
fn opened_tall(name: &str) -> Option<Harness> {
    opened_in(size(px(1440.0), px(1700.0)), name).map(|(harness, ..)| harness)
}

fn drag(harness: &mut Harness, name: &str, x: f32, y: f32) {
    let start = harness.find(name).center();
    harness.press(start, 1);
    harness.drag_to(start + point(px(x / 2.0), px(y / 2.0)));
    harness.drag_to(start + point(px(x), px(y)));
    harness.release(start + point(px(x), px(y)));
    harness.draw();
}

#[test]
#[ignore = "requires a wgpu adapter and ROHDITOR_RAW"]
fn a_white_balance_preset_is_chosen() {
    let Some(mut harness) = opened_tall("presets") else {
        return;
    };
    let original = recipe(&mut harness);
    harness.click("dropdown:white-balance");
    harness.capture("white-balance");
    harness.click("menu:Shade");
    assert_eq!(
        recipe(&mut harness).color.white_balance,
        WhiteBalance::TemperatureTint {
            temperature: 7_500.0,
            tint: 0.0
        }
    );
    harness.find("changed:WhiteBalance");
    harness.keys("ctrl-z");
    assert_eq!(recipe(&mut harness), original, "it is one undo step");
    drag(&mut harness, "adjustment:Tint", 20.0, 0.0);
    harness.click("dropdown:white-balance");
    harness.click("menu:As shot");
    assert_eq!(recipe(&mut harness), original);
}

#[test]
#[ignore = "requires a wgpu adapter and ROHDITOR_RAW"]
fn the_mixer_lists_the_sliders_of_one_color() {
    let Some(mut harness) = opened_tall("mixer") else {
        return;
    };
    const GREEN: &str = "adjustment:HslSaturation(3)";
    harness.click("section:ColorMixer");
    harness.find("adjustment:HslSaturation(0)");
    assert_eq!(harness.lookup(GREEN), None);
    harness.click("mixer:Green");
    drag(&mut harness, GREEN, 60.0, 0.0);
    harness.wait_until("the edited preview", settled);
    harness.capture("mixer");
    let channels = recipe(&mut harness).color.hsl.channels;
    assert!(channels[3].saturation > 0.1);
    assert_eq!(channels[0].saturation, 0.0);

    // The filter and the search list every colour.
    harness.click("mixer:Red");
    assert_eq!(harness.lookup(GREEN), None);
    harness.click("toggle:edited-only");
    harness.find(GREEN);
    assert_eq!(harness.lookup("adjustment:HslHue(3)"), None);
    harness.click("toggle:edited-only");
    harness.keys("ctrl-f b l u e");
    harness.find("adjustment:HslHue(5)");
    assert_eq!(harness.lookup(GREEN), None);
    harness.keys("escape");

    // A press gives the colours the focus for the arrow keys.
    harness.click("mixer:Magenta");
    harness.keys("right");
    harness.find("adjustment:HslHue(0)");
    harness.keys("left");
    harness.find("adjustment:HslHue(7)");
}

#[test]
#[ignore = "requires a wgpu adapter and ROHDITOR_RAW"]
fn a_wheel_tints_its_tones() {
    let Some(mut harness) = opened_tall("wheels") else {
        return;
    };
    harness.click("section:ColorGrading");
    let grading = |harness: &mut Harness| recipe(harness).color.grading;
    // Up and to the left is green.
    let before = harness.find("tint:Shadows").center();
    drag(&mut harness, "wheel:Shadows", -12.0, -18.0);
    let moved = harness.find("tint:Shadows").center() - before;
    assert!((moved.x + px(12.0)).abs() < px(0.5) && (moved.y + px(18.0)).abs() < px(0.5));
    let shadows = grading(&mut harness).shadows;
    assert!(
        shadows[1] > 0.3 && shadows[0] < 0.0 && shadows[2] < 0.0,
        "{shadows:?}"
    );
    assert_eq!(grading(&mut harness).midtones, [0.0; 3]);
    drag(&mut harness, "wheel:Highlights", 15.0, -10.0);
    harness.wait_until("the edited preview", settled);
    harness.capture("wheels");
    harness.keys("ctrl-z");
    assert_eq!(grading(&mut harness).highlights, [0.0; 3], "one undo step");
    assert_eq!(grading(&mut harness).shadows, shadows);

    // A press gives the wheel the focus for the arrow keys.
    drag(&mut harness, "wheel:Midtones", 0.0, 0.0);
    harness.keys("right right");
    let midtones = grading(&mut harness).midtones;
    assert!((midtones[0] - 0.04).abs() < 1e-5, "{midtones:?}");
    harness.keys("delete");
    assert_eq!(grading(&mut harness).midtones, [0.0; 3]);

    let shadows = harness.find("wheel:Shadows").center();
    harness.press(shadows, 2);
    harness.release(shadows);
    assert_eq!(grading(&mut harness).shadows, [0.0; 3]);
}

#[test]
#[ignore = "requires a wgpu adapter, ROHDITOR_RAW and ROHDITOR_DCP, a profile for its camera"]
fn profiles_are_chosen_and_imported() {
    let Some(dcp) = std::env::var_os("ROHDITOR_DCP").map(PathBuf::from) else {
        return;
    };
    let Some(mut harness) = opened_tall("profiles") else {
        return;
    };
    harness.click("section:ColorProfile");
    harness.click("dropdown:rendering");
    harness.click("menu:Neutral");
    assert_eq!(
        recipe(&mut harness).rendering.profile,
        RenderingProfileSelection::NEUTRAL
    );
    harness.keys("ctrl-z");
    let original = recipe(&mut harness).color;

    let import = |harness: &mut Harness, path: PathBuf| {
        harness.with_session(|session, cx| {
            let id = session.document().expect("a photo is open").id;
            session.import_camera_profile(id, path, cx);
        });
    };
    import(&mut harness, PathBuf::from("/nonexistent.dcp"));
    harness.find("toast");
    harness.click("button:dismiss-toast");
    assert_eq!(recipe(&mut harness).color, original);

    import(&mut harness, dcp);
    let edited = recipe(&mut harness);
    assert!(matches!(
        edited.color.camera_profile,
        CameraProfileSelection::Matrix(_)
    ));
    // The balance follows the profile's as-shot coordinates instead of counting as edited.
    let now = defaults(&mut harness);
    assert_eq!(edited.color.white_balance, now.color.white_balance);
    assert!(!RecipeSection::WhiteBalance.is_changed(&edited, &now));
    harness.wait_until("the photo with the profile", settled);
    harness.click("dropdown:camera-profile");
    harness.capture("profiles");
    harness.click("menu:Automatic");
    assert_eq!(recipe(&mut harness).color, original);
    harness.keys("ctrl-z");
    harness.click("button:reset-ColorProfile");
    assert_eq!(recipe(&mut harness).color, original);
}
