//! The Masks tool: its list, its sliders and the gradients on the photo.

use rohditor_edit::{
    BrushMask, BrushStroke, LinearGradient, LocalSettings, MaskComponent, MaskLayer, MaskMode,
    MaskPoint, MaskShape, RadialGradient,
};
use rohditor_engine::tools::{MaskAction, MaskEditMode, ToolKind};

use super::*;

fn layers(harness: &mut Harness) -> Vec<MaskLayer> {
    harness.with_session(|session, _| {
        let document = session.document().expect("a photo is open");
        document.edits.recipe().local.layers.clone()
    })
}

fn names(harness: &mut Harness) -> Vec<String> {
    layers(harness)
        .into_iter()
        .map(|layer| layer.name)
        .collect()
}

/// The selected mask and component.
fn selection(harness: &mut Harness) -> (Option<u32>, Option<usize>) {
    harness.with_session(|session, _| {
        let masks = session.masks().expect("the Masks tool is active");
        (masks.selected_layer, masks.selected_component)
    })
}

fn mode(harness: &mut Harness) -> MaskEditMode {
    harness.with_session(|session, _| session.masks().expect("the Masks tool is active").mode)
}

fn pan(harness: &mut Harness) -> Option<[f32; 2]> {
    harness.with_session(|session, _| session.document().map(|d| d.preview.view.pan()))
}

/// Choose a new mask's shape and draw it.
fn draw(harness: &mut Harness, entry: &str, from: (f32, f32), to: (f32, f32)) {
    harness.click("dropdown:new-mask");
    harness.click(&format!("menu:{entry}"));
    drag(harness, from, to, px(0.0));
}

fn linear(layer: &MaskLayer, component: usize) -> LinearGradient {
    match layer.components[component].shape {
        MaskShape::Linear(gradient) => gradient,
        ref other => panic!("not a linear gradient: {other:?}"),
    }
}

fn radial(layer: &MaskLayer) -> RadialGradient {
    match layer.components[0].shape {
        MaskShape::Radial(gradient) => gradient,
        ref other => panic!("not a radial gradient: {other:?}"),
    }
}

/// Of a photo that is shown as it was developed, uncropped and not turned.
#[track_caller]
fn assert_at(point: MaskPoint, x: f32, y: f32) {
    let near = (point.x - x).abs() < 2.0e-3 && (point.y - y).abs() < 2.0e-3;
    assert!(near, "{point:?} is not at {x}, {y}");
}

#[test]
#[ignore = "requires a wgpu adapter and ROHDITOR_RAW"]
fn a_gradient_is_drawn_and_its_sliders_change_only_a_part_of_the_photo() {
    let Some((mut harness, ..)) = opened("masks-linear") else {
        return;
    };
    harness.keys("m");
    harness.find("masks-panel");
    harness.capture("masks-empty");
    harness.click("dropdown:new-mask");
    harness.capture("masks-new");
    harness.click("menu:New linear gradient");
    assert_eq!(mode(&mut harness), MaskEditMode::CreateLinear);
    harness.find("mask-creation");
    // Escape only ends the drawing.
    harness.keys("escape");
    assert_eq!(mode(&mut harness), MaskEditMode::Select);
    assert_eq!(harness.lookup("mask-creation"), None);

    draw(&mut harness, "New linear gradient", (0.3, 0.2), (0.6, 0.4));
    let drawn = layers(&mut harness);
    assert_eq!(drawn.len(), 1);
    assert_at(linear(&drawn[0], 0).start, 0.3, 0.2);
    assert_at(linear(&drawn[0], 0).end, 0.6, 0.4);
    assert_eq!(mode(&mut harness), MaskEditMode::Select);
    assert_eq!(selection(&mut harness), (Some(drawn[0].id), Some(0)));
    assert_eq!(pan(&mut harness), Some([0.0, 0.0]));

    // Inside and outside of the mask.
    let shown = photo(&mut harness);
    let at = |x: f32, y: f32| shown.origin + point(shown.size.width * x, shown.size.height * y);
    harness.wait_until("the masked preview", settled);
    let before = harness.capture("masks-linear");
    let start = harness.find("adjustment:local-Exposure").center();
    harness.press(start, 1);
    harness.drag_to(start - point(px(20.0), px(0.0)));
    harness.drag_to(start - point(px(60.0), px(0.0)));
    harness.release(start - point(px(60.0), px(0.0)));
    let exposure = layers(&mut harness)[0].settings.exposure_ev;
    assert!(exposure < -1.0, "{exposure}");
    harness.wait_until("the darkened preview", settled);
    let after = harness.capture("masks-darkened");
    let change = |x, y| brightness(&after, at(x, y)) - brightness(&before, at(x, y));
    assert!(change(0.15, 0.2) < -5.0, "{}", change(0.15, 0.2));
    assert!(change(0.85, 0.8).abs() < 1.0, "{}", change(0.85, 0.8));

    harness.keys("ctrl-z");
    assert_eq!(layers(&mut harness), drawn, "the slider is one undo step");
    harness.keys("ctrl-z");
    assert!(layers(&mut harness).is_empty(), "so is the gradient");
    harness.keys("ctrl-shift-z");
    assert_eq!(layers(&mut harness), drawn);
    assert_eq!(selection(&mut harness), (Some(drawn[0].id), Some(0)));
}

#[test]
#[ignore = "requires a wgpu adapter and ROHDITOR_RAW"]
fn gradients_are_changed_by_their_handles_and_selected_by_their_pins() {
    let Some((mut harness, ..)) = opened("masks-handles") else {
        return;
    };
    harness.keys("m");
    draw(&mut harness, "New linear gradient", (0.2, 0.2), (0.2, 0.6));
    drag(&mut harness, (0.2, 0.6), (0.3, 0.7), px(3.0));
    let line = linear(&layers(&mut harness)[0], 0);
    assert_at(line.start, 0.2, 0.2);
    assert_at(line.end, 0.3, 0.7);
    // By its center.
    drag(&mut harness, (0.25, 0.45), (0.3, 0.45), px(0.0));
    let line = linear(&layers(&mut harness)[0], 0);
    assert_at(line.start, 0.25, 0.2);
    assert_at(line.end, 0.35, 0.7);

    draw(&mut harness, "New radial gradient", (0.7, 0.5), (0.8, 0.5));
    let circle = radial(&layers(&mut harness)[1]);
    assert_at(circle.center, 0.7, 0.5);
    assert!((circle.radius_x - 0.1).abs() < 2.0e-3, "{circle:?}");
    drag(&mut harness, (0.8, 0.5), (0.9, 0.5), px(0.0));
    // Anywhere inside moves it.
    drag(&mut harness, (0.6, 0.5), (0.5, 0.5), px(0.0));
    let wide = radial(&layers(&mut harness)[1]);
    assert_at(wide.center, 0.6, 0.5);
    assert!((wide.radius_x - 0.2).abs() < 2.0e-3, "{wide:?}");
    assert!((wide.radius_y - 0.1).abs() < 2.0e-3, "{wide:?}");
    harness.capture("masks-radial");

    // The pin of the line, at its center.
    let both = layers(&mut harness);
    assert_eq!(selection(&mut harness).0, Some(both[1].id));
    drag(&mut harness, (0.3, 0.45), (0.3, 0.45), px(0.0));
    assert_eq!(selection(&mut harness).0, Some(both[0].id));
    harness.capture("masks-pin");
    assert_eq!(layers(&mut harness), both);
    assert_eq!(pan(&mut harness), Some([0.0, 0.0]));

    // Elsewhere the photo pans, and a double click fits it again, also inside an ellipse.
    drag(&mut harness, (0.1, 0.9), (0.2, 0.9), px(0.0));
    assert_ne!(pan(&mut harness), Some([0.0, 0.0]));
    drag(&mut harness, (0.1, 0.9), (0.0, 0.9), px(0.0));
    harness.click(&format!("mask:{}", both[1].id));
    let area = photo(&mut harness);
    let inside = area.origin + point(area.size.width * 0.65, area.size.height * 0.5);
    harness.press(inside, 2);
    harness.release(inside);
    assert_eq!(pan(&mut harness), Some([0.0, 0.0]));
    assert_eq!(layers(&mut harness), both);

    // Drags are one undo step each.
    harness.keys("ctrl-z ctrl-z ctrl-z");
    assert_eq!(layers(&mut harness).len(), 1);

    // Leaving the tool ends a drag.
    harness.click(&format!("mask:{}", both[0].id));
    let area = photo(&mut harness);
    let center = area.origin + point(area.size.width * 0.3, area.size.height * 0.45);
    harness.press(center, 1);
    harness.drag_to(center + point(px(40.0), px(0.0)));
    let dragged = layers(&mut harness);
    harness.with_session(|session, cx| session.select_tool(ToolKind::Edit, cx));
    harness.drag_to(center + point(px(80.0), px(0.0)));
    harness.release(center);
    assert_eq!(layers(&mut harness), dragged);
    harness.keys("ctrl-z");
    assert_eq!(layers(&mut harness)[..], both[..1], "it is one undo step");
}

#[test]
#[ignore = "requires a wgpu adapter and ROHDITOR_RAW"]
fn the_list_renames_orders_disables_and_deletes_masks() {
    let Some((mut harness, ..)) = opened("masks-list") else {
        return;
    };
    harness.keys("m");
    draw(&mut harness, "New linear gradient", (0.2, 0.2), (0.2, 0.6));
    draw(&mut harness, "New radial gradient", (0.7, 0.5), (0.8, 0.5));
    assert_eq!(names(&mut harness), ["Mask 1", "Mask 2"]);
    let [first, second] = [0, 1].map(|index| layers(&mut harness)[index].id);

    let row = harness.find(&format!("mask:{first}")).center();
    harness.press(row, 1);
    harness.release(row);
    assert_eq!(selection(&mut harness).0, Some(first));
    harness.press(row, 2);
    harness.cx.run_until_parked();
    harness.send(PlatformInput::MouseUp(MouseUpEvent {
        button: MouseButton::Left,
        position: row,
        click_count: 2,
        ..Default::default()
    }));
    harness.draw();
    for letter in ["s", "k", "y"] {
        harness.keys(letter);
    }
    harness.capture("masks-rename");
    harness.keys("enter");
    harness.draw();
    assert_eq!(names(&mut harness), ["sky", "Mask 2"]);
    // Escape keeps the name.
    harness.keys("f2 x escape");
    harness.draw();
    assert_eq!(names(&mut harness), ["sky", "Mask 2"]);
    assert_eq!(selection(&mut harness).0, Some(first));

    harness.keys("alt-down");
    assert_eq!(names(&mut harness), ["Mask 2", "sky"]);
    harness.keys("alt-up");
    assert_eq!(names(&mut harness), ["sky", "Mask 2"]);

    harness.click(&format!("button:toggle-mask-{second}"));
    assert!(!layers(&mut harness)[1].enabled);
    assert_eq!(selection(&mut harness).0, Some(first), "only toggled");
    harness.capture("masks-list");

    harness.right_click(&format!("mask:{second}"));
    harness.capture("masks-menu");
    harness.click("menu:Delete");
    assert_eq!(names(&mut harness), ["sky"]);
    harness.keys("ctrl-z");
    assert_eq!(names(&mut harness), ["sky", "Mask 2"]);
    harness.click(&format!("mask:{second}"));
    harness.click(&format!("button:delete-mask-{second}"));
    assert_eq!(names(&mut harness), ["sky"]);
}

#[test]
#[ignore = "requires a wgpu adapter and ROHDITOR_RAW"]
fn shapes_are_added_combined_inverted_and_deleted() {
    let Some((mut harness, ..)) = opened("masks-components") else {
        return;
    };
    harness.keys("m");
    draw(&mut harness, "New linear gradient", (0.2, 0.2), (0.2, 0.6));
    harness.click("dropdown:new-component");
    harness.capture("masks-add");
    harness.click("menu:Subtract radial gradient");
    drag(&mut harness, (0.6, 0.5), (0.7, 0.5), px(0.0));
    let mask = layers(&mut harness).remove(0);
    assert_eq!(layers(&mut harness).len(), 1);
    assert_eq!(mask.components[1].mode, MaskMode::Subtract);
    assert_eq!(selection(&mut harness), (Some(mask.id), Some(1)));
    harness.capture("masks-components");

    let component = |harness: &mut Harness| layers(harness)[0].components[1].clone();
    harness.click("dropdown:component-mode-1");
    harness.click("menu:Intersect");
    assert_eq!(component(&mut harness).mode, MaskMode::Intersect);
    harness.click("button:invert-component-1");
    assert!(component(&mut harness).invert);

    harness.click("component:0");
    assert_eq!(selection(&mut harness), (Some(mask.id), Some(0)));
    // The first shape goes, and what is left has nothing to combine with.
    harness.keys("delete");
    let left = layers(&mut harness).remove(0);
    assert_eq!(left.components.len(), 1);
    assert_eq!(left.components[0].mode, MaskMode::Add);
    harness.keys("delete");
    assert!(layers(&mut harness).is_empty());
    harness.keys("ctrl-z ctrl-z");
    assert_eq!(layers(&mut harness)[0].components.len(), 2);
}

#[test]
#[ignore = "requires a wgpu adapter and ROHDITOR_RAW"]
fn sliders_belong_to_the_mask_they_were_changed_for() {
    let Some((mut harness, ..)) = opened("masks-sliders") else {
        return;
    };
    harness.keys("m");
    draw(&mut harness, "New linear gradient", (0.2, 0.2), (0.2, 0.6));
    draw(&mut harness, "New radial gradient", (0.7, 0.5), (0.8, 0.5));
    let [first, second] = [0, 1].map(|index| layers(&mut harness)[index].id);
    let contrast = |harness: &mut Harness| -> Vec<f32> {
        let layers = layers(harness);
        layers.iter().map(|layer| layer.settings.contrast).collect()
    };

    // Typed, and applied when the other mask is selected.
    let row = harness.find("adjustment:local-Contrast");
    harness.press(row.center(), 1);
    harness.release(row.center());
    harness.keys("enter 2 5");
    harness.click(&format!("mask:{first}"));
    assert_eq!(contrast(&mut harness), [0.0, 0.25]);
    // Arrow keys, until a pin selects the other mask.
    let row = harness.find("adjustment:local-Contrast");
    harness.press(row.center(), 1);
    harness.release(row.center());
    harness.keys("right right");
    let nudged = contrast(&mut harness)[0];
    assert!(nudged > 0.0);
    drag(&mut harness, (0.7, 0.5), (0.7, 0.5), px(0.0));
    assert_eq!(selection(&mut harness).0, Some(second));
    assert_eq!(contrast(&mut harness), [nudged, 0.25]);
    harness.keys("ctrl-z");
    assert_eq!(contrast(&mut harness), [0.0, 0.25], "the keys are one step");

    // Sections show what is changed, and reset it.
    harness.find("changed:local-Light");
    assert_eq!(harness.lookup("changed:local-Color"), None);
    assert_eq!(harness.lookup("adjustment:local-Sharpening"), None);
    harness.click("section:local-Detail");
    harness.find("adjustment:local-Sharpening");
    harness.capture("masks-sliders");
    harness.click("button:reset-local-Light");
    assert_eq!(contrast(&mut harness), [0.0, 0.0]);
    harness.keys("ctrl-z");
    assert_eq!(contrast(&mut harness), [0.0, 0.25]);
}

#[test]
#[ignore = "requires a wgpu adapter and ROHDITOR_RAW"]
fn masks_are_used_by_keyboard_and_follow_a_crop() {
    let Some((mut harness, ..)) = opened("masks-keys") else {
        return;
    };
    // The command changes to the tool.
    harness.keys("ctrl-shift-p");
    for key in "n e w space r a d i a l".split(' ') {
        harness.keys(key);
    }
    harness.keys("enter");
    harness.draw();
    assert_eq!(mode(&mut harness), MaskEditMode::CreateRadial);
    drag(&mut harness, (0.5, 0.5), (0.6, 0.5), px(0.0));
    let before = radial(&layers(&mut harness)[0]);
    let mask = layers(&mut harness)[0].id;

    // The masks, their shapes and the sliders are one list for the keyboard.
    harness.keys("ctrl-d enter");
    assert_eq!(selection(&mut harness).0, Some(mask));
    harness.keys("down down down down left left");
    assert!((layers(&mut harness)[0].opacity - 0.98).abs() < 1.0e-6);
    harness.keys("ctrl-d delete");
    assert!(layers(&mut harness).is_empty());
    harness.keys("ctrl-z ctrl-z");
    assert_eq!(layers(&mut harness)[0].opacity, 1.0);

    // On a cropped photo the mask stays where it was on the picture.
    harness.keys("escape c");
    harness.wait_until("the photo without its crop", |session| {
        session.crop_draft().is_some()
    });
    drag(&mut harness, (0.0, 0.0), (0.4, 0.4), px(0.0));
    harness.keys("m");
    harness.wait_until("the cropped preview", settled);
    harness.wait_until("the photo with its crop", |session| {
        session.mask_geometry().is_some()
    });
    harness.capture("masks-cropped");
    // (0.5, 0.5) of the photo is a sixth into the crop.
    drag(&mut harness, (1.0 / 6.0, 1.0 / 6.0), (0.5, 0.5), px(0.0));
    let moved = radial(&layers(&mut harness)[0]);
    assert_at(moved.center, 0.7, 0.7);
    assert_eq!(moved.radius_x, before.radius_x);
}

/// The strokes of a mask's first shape.
fn strokes(layer: &MaskLayer) -> &[BrushStroke] {
    match &layer.components[0].shape {
        MaskShape::Brush(brush) => &brush.strokes,
        other => panic!("not a brush: {other:?}"),
    }
}

/// Press with these modifiers at an image position, and release.
fn click_with(harness: &mut Harness, (x, y): (f32, f32), modifiers: Modifiers) {
    let area = photo(harness);
    let position = area.origin + point(area.size.width * x, area.size.height * y);
    harness.send(PlatformInput::MouseDown(MouseDownEvent {
        button: MouseButton::Left,
        position,
        click_count: 1,
        modifiers,
        ..Default::default()
    }));
    harness.release(position);
}

/// How much redder the photo is at an image position than it is green and blue.
fn redness(harness: &mut Harness, shot: &image::RgbaImage, (x, y): (f32, f32)) -> f64 {
    let area = photo(harness);
    let position = area.origin + point(area.size.width * x, area.size.height * y);
    let scale = shot.width() as f32 / WINDOW.width.as_f32();
    let pixel = shot.get_pixel(
        (position.x.as_f32() * scale) as u32,
        (position.y.as_f32() * scale) as u32,
    );
    f64::from(pixel[0]) - (f64::from(pixel[1]) + f64::from(pixel[2])) / 2.0
}

const ALT: Modifiers = Modifiers {
    alt: true,
    control: false,
    shift: false,
    platform: false,
    function: false,
};

#[test]
#[ignore = "requires a wgpu adapter and ROHDITOR_RAW"]
fn a_brush_paints_and_erases() {
    let Some((mut harness, ..)) = opened("masks-brush") else {
        return;
    };
    harness.keys("m");
    let before = harness.capture("masks-unpainted");
    harness.click("dropdown:new-mask");
    harness.click("menu:New brush");
    harness.find("mask-creation");
    assert_eq!(mode(&mut harness), MaskEditMode::Paint { erase: false });

    // All positions of one frame are followed, around a corner.
    let area = photo(&mut harness);
    let at = |x: f32, y: f32| area.origin + point(area.size.width * x, area.size.height * y);
    harness.press(at(0.2, 0.3), 1);
    for step in 1..=10_u8 {
        harness.move_held(at(0.2 + f32::from(step) * 0.04, 0.3));
    }
    for step in 1..=10_u8 {
        harness.move_held(at(0.6, 0.3 + f32::from(step) * 0.04));
    }
    assert_eq!(strokes(&layers(&mut harness)[0])[0].points.len(), 1);
    harness.begin_frame();
    let drawn = strokes(&layers(&mut harness)[0])[0].points.clone();
    assert!(drawn.len() > 10, "{drawn:?}");
    // It is tinted while it is painted.
    harness.draw();
    let during = harness.capture("masks-painting");
    for place in [(0.4, 0.3), (0.6, 0.5)] {
        let change = redness(&mut harness, &during, place) - redness(&mut harness, &before, place);
        assert!(change > 40.0, "{change} at {place:?}");
    }
    harness.release(at(0.6, 0.7));
    let stroke = strokes(&layers(&mut harness)[0])[0].clone();
    assert_eq!(stroke.points.len(), 3, "simplified: {:?}", stroke.points);
    assert_at(stroke.points[1], 0.6, 0.3);
    assert_at(stroke.points[2], 0.6, 0.7);

    // Beside the photo nothing is painted, and the photo does not move.
    let view = harness.find("viewport");
    harness.press(view.origin + point(px(5.0), px(5.0)), 1);
    harness.drag_to(view.origin + point(px(60.0), px(60.0)));
    harness.release(view.origin + point(px(60.0), px(60.0)));
    assert_eq!(strokes(&layers(&mut harness)[0]).len(), 1);
    assert_eq!(pan(&mut harness), Some([0.0, 0.0]));

    // A click is a dab. Alt erases.
    drag(&mut harness, (0.2, 0.8), (0.2, 0.8), px(0.0));
    click_with(&mut harness, (0.4, 0.3), ALT);
    let mask = layers(&mut harness).remove(0);
    let erases: Vec<_> = strokes(&mask).iter().map(|stroke| stroke.erase).collect();
    assert_eq!(erases, [false, false, true]);
    let erased = harness.capture("masks-erased");
    let left = redness(&mut harness, &erased, (0.4, 0.3));
    assert!(left < redness(&mut harness, &during, (0.4, 0.3)) - 40.0);
    harness.keys("ctrl-z");
    assert_eq!(strokes(&layers(&mut harness)[0]).len(), 2);
    harness.keys("ctrl-z ctrl-z");
    assert!(layers(&mut harness).is_empty(), "one step per stroke");
    harness.keys("ctrl-shift-z ctrl-shift-z ctrl-shift-z");
    assert_eq!(strokes(&layers(&mut harness)[0]).len(), 3);
}

#[test]
#[ignore = "requires a wgpu adapter and ROHDITOR_RAW"]
fn the_brush_is_set_by_keys_and_sliders() {
    let Some((mut harness, ..)) = opened("masks-brush-keys") else {
        return;
    };
    let brush = |harness: &mut Harness| {
        harness.with_session(|session, _| session.masks().expect("the tool is active").brush)
    };
    harness.keys("m");
    harness.click("dropdown:new-mask");
    harness.click("menu:New brush");
    let first = brush(&mut harness);
    harness.keys("]");
    assert!(brush(&mut harness).radius > first.radius);
    harness.keys("[ [");
    assert!(brush(&mut harness).radius < first.radius);
    // Those that need no AltGr on a German keyboard.
    let small = brush(&mut harness).radius;
    harness.keys("+");
    let larger = brush(&mut harness).radius;
    assert!(larger > small);
    harness.keys("-");
    assert!(brush(&mut harness).radius < larger);

    // Ctrl and the wheel, one step per notch of 60 pixels. The wheel alone zooms.
    let fits = |harness: &mut Harness| {
        harness.with_session(|session, _| session.document().map(|d| d.preview.view.is_fit()))
    };
    let over = harness.find("viewport").center();
    let turn = |harness: &mut Harness, pixels: f32, modifiers: Modifiers| {
        harness.send(PlatformInput::ScrollWheel(ScrollWheelEvent {
            position: over,
            delta: ScrollDelta::Pixels(point(px(0.0), px(pixels))),
            modifiers,
            touch_phase: TouchPhase::Moved,
        }));
    };
    let unturned = brush(&mut harness).radius;
    for _ in 0..2 {
        turn(&mut harness, 30.0, Modifiers::control());
    }
    assert!((brush(&mut harness).radius / unturned - 1.15).abs() < 1.0e-3);
    turn(&mut harness, -60.0, Modifiers::control());
    assert!((brush(&mut harness).radius - unturned).abs() < 1.0e-6);
    assert_eq!(fits(&mut harness), Some(true));
    turn(&mut harness, 60.0, Modifiers::default());
    assert_eq!(fits(&mut harness), Some(false));
    assert!((brush(&mut harness).radius - unturned).abs() < 1.0e-6);
    harness.keys("ctrl-0");

    harness.keys("}");
    assert!(brush(&mut harness).feather > first.feather);
    harness.keys("{ {");
    assert!(brush(&mut harness).feather < first.feather);

    let size = harness.find("adjustment:brush-Size").center();
    harness.press(size, 1);
    harness.drag_to(size + point(px(60.0), px(0.0)));
    harness.release(size + point(px(60.0), px(0.0)));
    let wide = brush(&mut harness);
    assert!(wide.radius > first.radius * 1.5, "{wide:?}");
    // Typed, where E is no shortcut.
    harness.keys("enter 8 e backspace enter");
    assert!((brush(&mut harness).radius - 0.08).abs() < 1.0e-6);
    harness.capture("masks-brush");

    // E erases while painting, and is the Edit tool otherwise.
    harness.keys("e");
    assert_eq!(mode(&mut harness), MaskEditMode::Paint { erase: true });
    harness.click("toggle:erase");
    assert_eq!(mode(&mut harness), MaskEditMode::Paint { erase: false });
    drag(&mut harness, (0.3, 0.3), (0.5, 0.5), px(0.0));
    let radius = strokes(&layers(&mut harness)[0])[0].radius;
    assert!((radius - 0.08).abs() < 1.0e-6);
    // The size is no edit of the photo.
    harness.keys("ctrl-z ctrl-shift-z");
    assert!((brush(&mut harness).radius - 0.08).abs() < 1.0e-6);
    harness.keys("escape");
    assert_eq!(mode(&mut harness), MaskEditMode::Select);
    // The size can be set before painting on.
    assert_eq!(harness.lookup("toggle:erase"), None);
    let size = harness.find("adjustment:brush-Size").center();
    harness.press(size, 1);
    harness.drag_to(size - point(px(20.0), px(0.0)));
    harness.release(size - point(px(20.0), px(0.0)));
    assert!(brush(&mut harness).radius < 0.08);
    harness.capture("masks-brush-selected");

    // More strokes are painted into the same shape.
    harness.keys("b");
    drag(&mut harness, (0.6, 0.3), (0.7, 0.5), px(0.0));
    harness.keys("escape");
    harness.click("button:paint-component-0");
    drag(&mut harness, (0.6, 0.6), (0.7, 0.7), px(0.0));
    let mask = layers(&mut harness).remove(0);
    assert_eq!(mask.components.len(), 1);
    assert_eq!(strokes(&mask).len(), 3);
    harness.keys("escape e");
    let tool = harness.with_session(|session, _| session.tool().kind());
    assert_eq!(tool, ToolKind::Edit);
}

#[test]
#[ignore = "requires a wgpu adapter and ROHDITOR_RAW"]
fn luminance_ranges_are_set_on_their_bar_and_by_their_rows() {
    let Some((mut harness, ..)) = opened("masks-luminance") else {
        return;
    };
    let range = |harness: &mut Harness| match layers(harness)[0].components.last() {
        Some(MaskComponent {
            shape: MaskShape::Luminance(range),
            ..
        }) => *range,
        other => panic!("not a luminance range: {other:?}"),
    };
    harness.keys("m");
    harness.click("dropdown:new-mask");
    harness.click("menu:New luminance range");
    assert_eq!(layers(&mut harness).len(), 1);
    harness.wait_until("the measured mask", |session| session.mask_tint().is_some());

    let width = harness.find("luminance").size.width;
    let handle = harness.find("handle:LuminanceLow").center();
    harness.press(handle, 1);
    harness.drag_to(handle + point(width * 0.25, px(0.0)));
    harness.drag_to(handle + point(width * 0.5, px(0.0)));
    harness.release(handle + point(width * 0.5, px(0.0)));
    let dragged = range(&mut harness);
    assert!((dragged.low - 0.5).abs() < 0.01, "{dragged:?}");
    assert_eq!((dragged.high, dragged.feather_low), (1.0, 0.1));
    // Its row has the keyboard.
    harness.keys("right");
    assert!((range(&mut harness).low - 0.51).abs() < 0.011);
    let feather = harness.find("handle:LuminanceFeatherLow").center();
    harness.press(feather, 1);
    harness.drag_to(feather - point(width * 0.1, px(0.0)));
    harness.release(feather - point(width * 0.1, px(0.0)));
    assert!((range(&mut harness).feather_low - 0.2).abs() < 0.02);
    let row = harness.find("adjustment:range-LuminanceHigh").center();
    harness.press(row, 1);
    harness.release(row);
    harness.keys("enter 9 0 enter");
    assert_eq!(range(&mut harness).high, 0.9);
    harness.wait_until("the measured mask", |session| session.mask_tint().is_some());
    harness.wait_until("the preview", settled);
    harness.capture("masks-luminance");

    harness.keys("ctrl-z ctrl-z ctrl-z ctrl-z");
    assert_eq!(range(&mut harness).low, 0.0, "each change is one step");
    harness.keys("ctrl-z");
    assert!(layers(&mut harness).is_empty());

    // It refines another shape.
    draw(&mut harness, "New radial gradient", (0.5, 0.5), (0.8, 0.5));
    harness.click("dropdown:new-component");
    harness.capture("masks-refine");
    harness.click("menu:Intersect luminance range");
    let mask = layers(&mut harness).remove(0);
    assert_eq!(mask.components[1].mode, MaskMode::Intersect);
    harness.find("luminance");
    harness.click("component:0");
    assert_eq!(harness.lookup("luminance"), None);
}

#[test]
#[ignore = "requires a wgpu adapter and ROHDITOR_RAW"]
fn color_ranges_are_picked_on_the_photo() {
    let Some((mut harness, ..)) = opened("masks-color") else {
        return;
    };
    let samples = |session: &Session| {
        let layers = &session
            .document()
            .expect("open")
            .edits
            .recipe()
            .local
            .layers;
        match layers.first().map(|layer| &layer.components[0].shape) {
            Some(MaskShape::Color(range)) => range.samples.len(),
            _ => 0,
        }
    };
    harness.keys("m");
    harness.click("dropdown:new-mask");
    harness.click("menu:New color range");
    assert_eq!(mode(&mut harness), MaskEditMode::PickColor);
    harness.find("mask-creation");
    click_with(&mut harness, (0.3, 0.3), Modifiers::default());
    harness.wait_until("the first color", |session| samples(session) == 1);
    harness.draw();
    harness.find("swatch:0");
    assert_eq!(pan(&mut harness), Some([0.0, 0.0]));
    click_with(&mut harness, (0.7, 0.6), Modifiers::shift());
    harness.wait_until("another color", |session| samples(session) == 2);
    harness.wait_until("the measured mask", |session| session.mask_tint().is_some());
    harness.capture("masks-color");
    click_with(&mut harness, (0.7, 0.6), ALT);
    harness.wait_until("one color less", |session| samples(session) == 1);
    harness.keys("ctrl-z ctrl-z ctrl-z");
    assert!(layers(&mut harness).is_empty(), "a step per click");
    harness.keys("ctrl-shift-z");

    harness.keys("escape");
    assert_eq!(mode(&mut harness), MaskEditMode::Select);
    let mask = layers(&mut harness)[0].id;
    harness.click(&format!("mask:{mask}"));
    harness.click("button:pick-mask-colors");
    assert_eq!(mode(&mut harness), MaskEditMode::PickColor);
    harness.keys("escape");
    let row = harness.find("adjustment:range-ColorTolerance").center();
    harness.press(row, 1);
    harness.drag_to(row + point(px(30.0), px(0.0)));
    harness.release(row + point(px(30.0), px(0.0)));
    harness.keys("ctrl-z");
    assert_eq!(layers(&mut harness).len(), 1);
}

#[test]
#[ignore = "requires a wgpu adapter and ROHDITOR_RAW"]
fn the_selected_mask_is_tinted_unless_hidden() {
    let Some((mut harness, ..)) = opened("masks-tint") else {
        return;
    };
    let tinted =
        |harness: &mut Harness| harness.with_session(|session, _| session.mask_tint().is_some());
    harness.keys("m");
    let before = harness.capture("masks-no-tint");
    draw(&mut harness, "New linear gradient", (0.5, 0.2), (0.5, 0.6));
    harness.draw();
    let after = harness.capture("masks-tint");
    let change = |harness: &mut Harness, shot, place| {
        redness(harness, shot, place) - redness(harness, &before, place)
    };
    assert!(change(&mut harness, &after, (0.3, 0.1)) > 40.0);
    assert!(change(&mut harness, &after, (0.3, 0.9)).abs() < 3.0);

    harness.keys("o");
    assert!(!tinted(&mut harness));
    harness.click("toggle:mask-overlay");
    assert!(tinted(&mut harness));
    harness.keys("o");
    // Painting is never blind.
    harness.click("dropdown:new-component");
    harness.click("menu:Add brush");
    assert!(tinted(&mut harness));
    harness.keys("escape");
    assert!(!tinted(&mut harness));
    harness.keys("o");

    // Not without its mask, and not in the other tools.
    harness.keys("ctrl-z");
    assert!(!tinted(&mut harness));
    harness.keys("ctrl-shift-z");
    assert!(tinted(&mut harness));
    harness.keys("e");
    assert!(!tinted(&mut harness));
    harness.keys("m");
    let mask = layers(&mut harness)[0].id;
    harness.click(&format!("mask:{mask}"));
    assert!(tinted(&mut harness));
}

/// Prints what painting costs on a photo that already has a lot of strokes, with a mouse that
/// reports eight positions per frame. The time is what the interface is busy for per frame.
#[test]
#[ignore = "requires a wgpu adapter and ROHDITOR_RAW"]
fn painting_cost() {
    let Some((mut harness, ..)) = opened("masks-cost") else {
        return;
    };
    harness.keys("m");
    for (name, strokes) in [("no", 0), ("150", 150)] {
        harness.with_session(|session, cx| {
            let document = session.document_mut().expect("a photo is open");
            let mut next = document.edits.recipe().clone();
            next.local.layers = vec![painted(strokes)];
            document.edits.set_discrete(next);
            session.edit_masks(MaskAction::SelectLayer(0), cx);
            session.edit_masks(MaskAction::PaintSelected, cx);
        });
        let area = photo(&mut harness);
        let at = |step: u16| {
            let turn = f32::from(step) / 480.0 * std::f32::consts::TAU;
            let (x, y) = (0.5 + 0.3 * turn.cos(), 0.5 + 0.3 * turn.sin());
            area.origin + point(area.size.width * x, area.size.height * y)
        };
        harness.press(at(0), 1);
        let mut frames = Vec::new();
        for frame in 0..60 {
            let begun = Instant::now();
            for step in 0..8 {
                harness.move_held(at(frame * 8 + step));
            }
            harness.begin_frame();
            harness.draw();
            frames.push(begun.elapsed());
            // Leave the GPU some time, as the display would.
            std::thread::sleep(Duration::from_millis(5));
        }
        harness.release(at(479));
        frames.sort_unstable();
        println!(
            "painting over {name} strokes: median {:?}, 95% {:?}, max {:?}",
            frames[30], frames[57], frames[59]
        );
        harness.wait_until("the painted preview", settled);
    }
}

/// A brush with this many strokes of 40 points.
fn painted(strokes: u16) -> MaskLayer {
    let stroke = |index: u16| BrushStroke {
        points: (0..40_u16)
            .map(|step| MaskPoint {
                x: 0.1 + f32::from(step) / 50.0,
                y: 0.1 + f32::from(index % 50) / 60.0,
            })
            .collect(),
        radius: 0.02,
        feather: 0.5,
        flow: 1.0,
        erase: false,
    };
    MaskLayer {
        id: 0,
        name: "Painted".to_owned(),
        enabled: true,
        opacity: 1.0,
        components: vec![MaskComponent {
            mode: MaskMode::Add,
            invert: false,
            shape: MaskShape::Brush(BrushMask {
                strokes: (0..strokes.max(1)).map(stroke).collect(),
            }),
        }],
        settings: LocalSettings {
            exposure_ev: -1.0,
            ..LocalSettings::default()
        },
    }
}
