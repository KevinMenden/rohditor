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

/// Where `[x, y]` of the curve's axes is on the graph.
fn on_graph(harness: &mut Harness, [x, y]: [f32; 2]) -> Point<Pixels> {
    let graph = harness.find("tone-curve");
    graph.bottom_left() + point(graph.size.width * x, -graph.size.height * y)
}

fn curve_points(harness: &mut Harness) -> Vec<[f32; 2]> {
    recipe(harness).light.tone_curve.points
}

/// Press at `from`, drag by `by` and release.
fn drag(harness: &mut Harness, from: Point<Pixels>, by: Point<Pixels>) {
    harness.press(from, 1);
    harness.drag_to(from + by * 0.5);
    harness.drag_to(from + by);
    harness.release(from + by);
}

#[test]
#[ignore = "requires a wgpu adapter and ROHDITOR_RAW"]
fn the_tone_curve_gets_points_that_are_dragged_and_removed() {
    let Some((mut harness, ..)) = opened("curve") else {
        return;
    };
    harness.click("section:ToneCurve");
    assert_eq!(
        harness.lookup("adjustment:CurveOutput"),
        None,
        "nothing selected"
    );

    // A press on the diagonal adds a point there; the drag moves it, as one undo step.
    let toe = on_graph(&mut harness, [0.25, 0.25]);
    drag(&mut harness, toe, point(px(0.0), px(18.0)));
    let points = curve_points(&mut harness);
    assert_eq!(points.len(), 3, "{points:?}");
    let [x, y] = points[1];
    assert!((x - 0.25).abs() < 0.01 && y < 0.2, "{points:?}");
    harness.find("curve-point:1");
    harness.find("adjustment:CurveOutput");

    // The shoulder, for an S-curve.
    let shoulder = on_graph(&mut harness, [0.75, 0.75]);
    drag(&mut harness, shoulder, point(px(0.0), px(-18.0)));
    let s_curve = curve_points(&mut harness);
    assert_eq!(s_curve.len(), 4);
    // Added on the curve, which the toe pulled below the diagonal, then raised.
    assert!(s_curve[2][1] > 0.75, "{s_curve:?}");
    harness.find("changed:ToneCurve");
    harness.wait_until("the edited preview", settled);
    harness.capture("tone-curve");

    // Dragging the toe point again, from beside it: points are grabbed near them.
    let toe = on_graph(&mut harness, s_curve[1]) + point(px(3.0), px(2.0));
    drag(&mut harness, toe, point(px(0.0), px(-9.0)));
    let dragged = curve_points(&mut harness);
    assert_eq!(dragged.len(), 4, "no point was added");
    assert!(dragged[1][1] > s_curve[1][1]);

    // The graph has the focus: Page Up raises the selected point.
    harness.keys("pageup");
    assert!(curve_points(&mut harness)[1][1] > dragged[1][1]);
    harness.keys("ctrl-z");
    assert_eq!(curve_points(&mut harness), dragged);
    harness.keys("ctrl-z");
    assert_eq!(
        curve_points(&mut harness),
        s_curve,
        "a drag is one undo step"
    );
    harness.keys("ctrl-shift-z");
    assert_eq!(curve_points(&mut harness), dragged);

    // A double click removes a point, Delete the selected one.
    let shoulder = on_graph(&mut harness, dragged[2]);
    harness.press(shoulder, 1);
    harness.release(shoulder);
    harness.press(shoulder, 2);
    harness.release(shoulder);
    assert_eq!(curve_points(&mut harness).len(), 3);
    harness.click("curve-point:1");
    harness.keys("delete");
    assert_eq!(curve_points(&mut harness).len(), 2);
    harness.keys("ctrl-z ctrl-z");
    assert_eq!(curve_points(&mut harness), dragged);
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
    const CURVE: &str = "tone-curve";
    let before = recipe(&mut harness);
    const CURVE_SHADOWS: &str = "adjustment:Shadows";
    harness.find("group:Detail");
    assert_eq!(harness.lookup(CURVE), None, "it starts collapsed");
    harness.capture("groups");
    harness.keys("ctrl-f");
    type_text(&mut harness, "shadows");
    harness.capture("search");
    harness.find(CURVE_SHADOWS);
    assert_eq!(harness.lookup(EXPOSURE), None);
    assert_eq!(harness.lookup("group:Detail"), None);
    assert_eq!(harness.lookup(CURVE), None);
    harness.click("button:clear-search");
    harness.keys("ctrl-f");
    type_text(&mut harness, "curve");
    assert_eq!(harness.lookup(CURVE_SHADOWS), None);
    harness.find(CURVE);

    // Down goes to the results: the header, then the slider.
    harness.keys("down down delete");
    assert_eq!(
        recipe(&mut harness),
        before,
        "the curve has the focus, but no point to remove"
    );
    harness.keys("ctrl-f escape");
    harness.find(EXPOSURE);
    assert_eq!(harness.lookup(CURVE), None, "collapsed again");
    type_text(&mut harness, "xyz");
    assert_eq!(harness.lookup("group:Light"), None);
    harness.click("button:clear-search");
    harness.find("group:Light");
    harness.keys("ctrl-f escape right");
    let name = harness.with_session(|session, _| session.document().map(|d| d.file_name()));
    assert_eq!(name.as_deref(), Some("B.ARW"), "Escape gives the keys back");
}
