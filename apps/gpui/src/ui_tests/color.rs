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

/// Press on the photo at a position 0 to 1 over the viewport.
fn press_photo(harness: &mut Harness, x: f32, y: f32) {
    let area = harness.find("viewport");
    let position = area.origin + point(area.size.width * x, area.size.height * y);
    harness.press(position, 1);
    harness.release(position);
}

#[test]
#[ignore = "requires a wgpu adapter and ROHDITOR_RAW"]
fn the_white_balance_is_picked_from_the_photo() {
    let Some(mut harness) = opened_tall("pick-balance") else {
        return;
    };
    let picking = |session: &Session| session.document().is_some_and(|d| d.is_picking());
    let original = recipe(&mut harness);
    harness.keys("w");
    assert!(harness.with_session(|session, _| session.picker().is_some()));
    harness.capture("picking");
    harness.keys("escape");
    assert!(harness.with_session(|session, _| session.picker().is_none()));
    press_photo(&mut harness, 0.5, 0.5);
    assert!(!harness.with_session(|session, _| picking(session)));

    harness.click("button:pick-white-balance");
    press_photo(&mut harness, 0.5, 0.5);
    assert!(harness.with_session(|session, _| session.picker().is_none()));
    // The worker rejects patches that are too varied, so look for one that is not.
    let deadline = Instant::now() + Duration::from_secs(60);
    let mut x = 0.3;
    while recipe(&mut harness) == original {
        assert!(Instant::now() < deadline, "no patch could be sampled");
        harness.with_session(|session, cx| session.tick(cx));
        harness.cx.run_until_parked();
        if !harness.with_session(|session, _| picking(session)) && recipe(&mut harness) == original
        {
            harness.keys("w");
            press_photo(&mut harness, x, 0.4);
            x = if x > 0.7 { 0.3 } else { x + 0.03 };
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    let picked = recipe(&mut harness);
    assert_ne!(picked.color.white_balance, original.color.white_balance);
    assert_eq!(picked.light, original.light);
    harness.wait_until("the picked balance", settled);
    harness.capture("picked-balance");
    harness.keys("ctrl-z");
    assert_eq!(recipe(&mut harness), original, "a pick is one undo step");
}

#[test]
#[ignore = "requires a wgpu adapter and ROHDITOR_RAW"]
fn a_mixer_color_is_picked_from_the_photo() {
    let Some(mut harness) = opened_tall("pick-color") else {
        return;
    };
    let original = recipe(&mut harness);
    // What would hide the result is reset.
    harness.keys("ctrl-f x y z");
    harness.with_session(|session, cx| {
        session.toggle_picker(rohditor_engine::tools::Picker::ColorMixer, cx);
    });
    press_photo(&mut harness, 0.25, 0.5);
    harness.wait_until("the sample", |session| {
        session.document().is_some_and(|d| !d.is_picking())
    });
    harness.draw();
    let channel = (0..8).find(|channel| {
        let row = format!("adjustment:HslHue({channel})");
        harness.lookup(&row).is_some()
    });
    let channel = channel.expect("a color is selected");
    println!("picked channel {channel}");
    harness.capture("picked-color");
    assert_eq!(recipe(&mut harness), original, "a pick only selects");
}
