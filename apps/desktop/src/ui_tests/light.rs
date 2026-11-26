//! The tone curve, the highlight reconstruction and the search of the develop panel.

use rohditor_edit::{EditRecipe, HighlightMethod};

use super::*;

fn recipe(harness: &mut Harness) -> EditRecipe {
    harness.with_session(|session, _| {
        let document = session.document().expect("a photo is open");
        document.edits.recipe().clone()
    })
}

fn type_text(harness: &mut Harness, text: &str) {
    for letter in text.chars() {
        let key = letter.to_string();
        harness.keys(if letter == ' ' { "space" } else { &key });
    }
    harness.draw();
}

#[test]
#[ignore = "requires a wgpu adapter and ROHDITOR_RAW"]
fn the_tone_curve_is_dragged_by_its_handles() {
    let Some((mut harness, ..)) = opened("curve") else {
        return;
    };
    const DARKS: &str = "handle:ToneCurveDarks";
    harness.click("section:ToneCurve");
    let before = harness.find(DARKS).center();
    // Not exactly on the handle: the drag is relative.
    let start = before + point(px(6.0), px(20.0));
    harness.press(start, 1);
    harness.drag_to(start - point(px(0.0), px(4.0)));
    harness.drag_to(start - point(px(0.0), px(10.0)));
    harness.release(start - point(px(0.0), px(10.0)));
    let after = harness.find(DARKS).center();
    let moved = before - after;
    assert!(
        (moved.y - px(10.0)).abs() < px(0.5) && moved.x == px(0.0),
        "{moved:?}"
    );
    let curve = recipe(&mut harness).light.tone_curve;
    assert!(curve.darks > 0.02 && curve.shadows == 0.0 && curve.lights == 0.0);
    harness.find("changed:ToneCurve");
    harness.wait_until("the edited preview", settled);
    harness.capture("tone-curve");

    // The handle's row has the focus now.
    harness.keys("right");
    assert!(recipe(&mut harness).light.tone_curve.darks > curve.darks);
    harness.keys("ctrl-z");
    assert_eq!(recipe(&mut harness).light.tone_curve, curve);
    harness.keys("ctrl-z");
    assert_eq!(
        recipe(&mut harness).light.tone_curve.darks,
        0.0,
        "a drag is one undo step"
    );
    harness.keys("ctrl-shift-z");
    assert_eq!(recipe(&mut harness).light.tone_curve, curve);
    let handle = harness.find(DARKS).center();
    harness.press(handle, 2);
    harness.release(handle);
    assert_eq!(recipe(&mut harness).light.tone_curve.darks, 0.0);
}

#[test]
#[ignore = "requires a wgpu adapter and ROHDITOR_RAW"]
fn the_highlight_method_is_chosen_and_has_its_own_threshold() {
    let Some((mut harness, ..)) = opened("highlights") else {
        return;
    };
    const CLIP: &str = "adjustment:HighlightClipThreshold";
    const DETECTION: &str = "adjustment:HighlightDetectionThreshold";
    let method = |harness: &mut Harness| recipe(harness).raw.highlights.method;
    harness.click("section:HighlightReconstruction");
    assert_eq!(method(&mut harness), HighlightMethod::Clip);
    harness.find(CLIP);
    assert_eq!(harness.lookup(DETECTION), None);

    harness.click("dropdown:highlight-method");
    harness.click("menu:Reconstruct");
    assert_eq!(method(&mut harness), HighlightMethod::Opposed);
    assert_eq!(harness.lookup(CLIP), None);
    let start = harness.find(DETECTION).center();
    harness.press(start, 1);
    harness.drag_to(start - point(px(30.0), px(0.0)));
    harness.release(start - point(px(30.0), px(0.0)));
    let highlights = recipe(&mut harness).raw.highlights;
    assert!(highlights.opposed.detection_threshold < 1.0);
    assert_eq!(highlights.clip.threshold, 1.0);
    harness.wait_until("the edited preview", settled);
    harness.capture("highlights");
    harness.keys("ctrl-z ctrl-z");
    assert_eq!(method(&mut harness), HighlightMethod::Clip);

    // By palette, and by keyboard in the panel: it stops at the dropdown.
    harness.keys("ctrl-shift-p");
    type_text(&mut harness, "reconstruction off");
    harness.keys("enter");
    harness.draw();
    assert_eq!(method(&mut harness), HighlightMethod::Off);
    assert_eq!(harness.lookup(CLIP), None);
    harness.keys("ctrl-f");
    type_text(&mut harness, "method");
    harness.keys("down down enter down enter");
    harness.draw();
    assert_eq!(method(&mut harness), HighlightMethod::Clip);

    // A changed method passes "Edited only".
    harness.keys("ctrl-z escape");
    harness.keys("ctrl-f escape");
    harness.click("toggle:edited-only");
    harness.click("button:reset-HighlightReconstruction");
    assert_eq!(method(&mut harness), HighlightMethod::Clip);
}

#[test]
#[ignore = "requires a wgpu adapter and ROHDITOR_RAW"]
fn adjustments_are_searched() {
    let Some((mut harness, ..)) = opened("search") else {
        return;
    };
    const CURVE_SHADOWS: &str = "adjustment:ToneCurveShadows";
    harness.find("group:Detail");
    assert_eq!(harness.lookup(CURVE_SHADOWS), None, "it starts collapsed");
    harness.capture("groups");
    harness.keys("ctrl-f");
    type_text(&mut harness, "shadows");
    harness.capture("search");
    harness.find("adjustment:Shadows");
    harness.find(CURVE_SHADOWS);
    assert_eq!(harness.lookup(EXPOSURE), None);
    assert_eq!(harness.lookup("group:Detail"), None);
    type_text(&mut harness, " curve");
    assert_eq!(harness.lookup("adjustment:Shadows"), None);

    // Down goes to the results: the header, then the slider.
    harness.keys("down down right");
    assert_eq!(recipe(&mut harness).light.tone_curve.shadows, 0.01);
    harness.keys("ctrl-z ctrl-f escape");
    harness.find(EXPOSURE);
    assert_eq!(harness.lookup(CURVE_SHADOWS), None, "collapsed again");
    type_text(&mut harness, "xyz");
    assert_eq!(harness.lookup("group:Light"), None);
    harness.click("button:clear-search");
    harness.find("group:Light");
    harness.keys("ctrl-f escape right");
    let name = harness.with_session(|session, _| session.document().map(|d| d.file_name()));
    assert_eq!(name.as_deref(), Some("B.ARW"), "Escape gives the keys back");
}
