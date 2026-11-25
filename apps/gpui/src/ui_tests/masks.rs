//! The Masks tool: its list, its sliders and the gradients on the photo.

use rohditor_edit::{LinearGradient, MaskLayer, MaskMode, MaskPoint, MaskShape, RadialGradient};
use rohditor_engine::tools::{MaskEditMode, ToolKind};

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
    harness.keys("down down down left left");
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
