//! The pointer driver and panel actions against a real edit history: what is one undo step,
//! and which gestures must not stay open.

use rohditor_core::OutputGeometry;
use rohditor_edit::{
    BrushMask, BrushStroke, EditRecipe, LinearGradient, LocalSettings, LuminanceRange,
    MAX_BRUSH_POINTS, MaskComponent, MaskLayer, MaskMode, MaskPoint, MaskShape,
};
use rohditor_image::Orientation;

use super::*;
use crate::document::EditSession;

fn recipe_with_layer() -> EditRecipe {
    let mut recipe = EditRecipe::default();
    recipe.local.layers.push(MaskLayer {
        id: 0,
        name: "Mask 1".to_owned(),
        enabled: true,
        opacity: 1.0,
        components: vec![MaskComponent {
            mode: MaskMode::Add,
            invert: false,
            shape: MaskShape::Linear(LinearGradient {
                start: MaskPoint { x: 0.1, y: 0.1 },
                end: MaskPoint { x: 0.9, y: 0.9 },
            }),
        }],
        settings: LocalSettings::default(),
    });
    recipe
}

fn drag(started: bool, stopped: bool) -> SliderPhase {
    SliderPhase {
        changed: true,
        drag_started: started,
        dragged: !stopped,
        drag_stopped: stopped,
        reset: false,
    }
}

fn stop() -> SliderPhase {
    SliderPhase {
        drag_stopped: true,
        ..SliderPhase::default()
    }
}

fn slider(
    session: &mut MaskToolSession,
    edits: &mut EditSession,
    slider: LocalSlider,
    value: f32,
    phase: SliderPhase,
) -> bool {
    session.apply_action(edits, MaskAction::Slider(0, slider, value, phase))
}

#[test]
fn dragging_a_luminance_handle_is_one_undo_step() {
    let mut recipe = recipe_with_layer();
    recipe.local.layers[0].components.push(MaskComponent {
        mode: MaskMode::Intersect,
        invert: false,
        shape: MaskShape::Luminance(LuminanceRange::default()),
    });
    let mut edits = EditSession::from_saved(recipe);
    let mut session = MaskToolSession::default();
    let mut step = |edits: &mut EditSession, value, phase| {
        session.apply_action(
            edits,
            MaskAction::RangeSlider {
                layer: 0,
                component: 1,
                slider: RangeSlider::LuminanceLow,
                value,
                phase,
            },
        )
    };
    let low = |edits: &EditSession| match &edits.recipe().local.layers[0].components[1].shape {
        MaskShape::Luminance(range) => range.low,
        other => panic!("not a luminance range: {other:?}"),
    };
    assert!(step(&mut edits, 0.2, drag(true, false)));
    assert!(step(&mut edits, 0.4, drag(false, false)));
    assert!(!step(&mut edits, 0.4, stop()));
    assert_eq!(low(&edits), 0.4);
    assert!(edits.undo());
    assert_eq!(low(&edits), 0.0);
    assert!(!edits.can_undo());
}

#[test]
fn a_slider_drag_is_one_undo_step_and_undo_restores_the_value() {
    let mut edits = EditSession::from_saved(recipe_with_layer());
    let mut session = MaskToolSession::default();
    let exposure = LocalSlider::Exposure;
    assert!(slider(
        &mut session,
        &mut edits,
        exposure,
        0.2,
        drag(true, false)
    ));
    assert!(slider(
        &mut session,
        &mut edits,
        exposure,
        0.5,
        drag(false, false)
    ));
    // Releasing without moving ends the gesture without a further change.
    assert!(!slider(&mut session, &mut edits, exposure, 0.5, stop()));
    assert_eq!(edits.recipe().local.layers[0].settings.exposure_ev, 0.5);
    assert!(edits.undo());
    assert_eq!(edits.recipe().local.layers[0].settings.exposure_ev, 0.0);
    assert!(!edits.can_undo());
}

#[test]
fn a_typed_value_is_a_single_discrete_edit() {
    let mut edits = EditSession::from_saved(recipe_with_layer());
    let mut session = MaskToolSession::default();
    let typed = SliderPhase {
        changed: true,
        ..SliderPhase::default()
    };
    assert!(slider(
        &mut session,
        &mut edits,
        LocalSlider::Opacity,
        0.4,
        typed
    ));
    assert!(edits.undo());
    assert_eq!(edits.recipe().local.layers[0].opacity, 1.0);
}

#[test]
fn panel_actions_report_whether_the_recipe_changed() {
    let mut edits = EditSession::from_saved(recipe_with_layer());
    let mut session = MaskToolSession::default();
    // Selection and overlay changes leave the recipe alone, so no preview is needed.
    assert!(!session.apply_action(&mut edits, MaskAction::SelectLayer(0)));
    assert_eq!(session.selected_layer, Some(0));
    assert!(!session.apply_action(&mut edits, MaskAction::ToggleOverlay));
    assert!(!session.show_overlay);
    assert!(!edits.can_undo());
    // Deleting goes into the history as one step.
    assert!(session.apply_action(&mut edits, MaskAction::DeleteSelected));
    assert!(edits.recipe().local.layers.is_empty());
    assert!(edits.undo());
    assert_eq!(edits.recipe().local.layers.len(), 1);
}

#[test]
fn bracket_keys_resize_and_shifted_brackets_change_the_feather() {
    let mut steps = BrushKeySteps::default();
    assert!(steps.is_empty());
    steps.press(Bracket::Close, false);
    steps.press(Bracket::Close, false);
    steps.press(Bracket::Open, false);
    assert_eq!(
        steps,
        BrushKeySteps {
            size: 1,
            feather: 0
        }
    );
    steps.press(Bracket::Open, true);
    assert_eq!(
        steps,
        BrushKeySteps {
            size: 1,
            feather: -1
        }
    );

    let mut edits = EditSession::from_saved(EditRecipe::default());
    let mut session = MaskToolSession::default();
    let before = session.brush;
    let changed = session.apply_action(&mut edits, MaskAction::BrushKeys(steps));
    assert!(!changed, "brush settings are not part of the recipe");
    assert_ne!(session.brush.radius, before.radius);
    assert_ne!(session.brush.feather, before.feather);
}

fn view() -> MaskView {
    MaskView {
        geometry: OutputGeometry::new(600, 400, Orientation::Normal, None).expect("valid geometry"),
        screen_scale: 1.0,
    }
}

fn drive(
    session: &mut MaskToolSession,
    edits: &mut EditSession,
    events: &MaskPointerEvents,
) -> bool {
    session.drive_pointer(edits, events, &view())
}

#[test]
fn a_press_that_grabs_nothing_leaves_no_gesture_open() {
    let mut edits = EditSession::from_saved(recipe_with_layer());
    let mut session = MaskToolSession::default();
    // Select mode with nothing selected: nothing to grab, and the drag that follows moves
    // nothing.
    let events = MaskPointerEvents {
        press: Some((0.5, 0.5)),
        drag_to: vec![(0.6, 0.6)],
        ..MaskPointerEvents::default()
    };
    assert!(!drive(&mut session, &mut edits, &events));
    assert!(!edits.gesture_active());
    assert!(!session.is_dragging());
    // A release without a press is harmless too.
    let release = MaskPointerEvents {
        release: true,
        ..MaskPointerEvents::default()
    };
    assert!(!drive(&mut session, &mut edits, &release));
    assert!(!edits.can_undo());
}

#[test]
fn drawing_a_gradient_is_one_undo_step() {
    let mut edits = EditSession::from_saved(EditRecipe::default());
    let mut session = MaskToolSession::default();
    session.create_layer(MaskEditMode::CreateLinear);
    let press = MaskPointerEvents {
        press: Some((0.2, 0.5)),
        drag_to: vec![(0.4, 0.5)],
        ..MaskPointerEvents::default()
    };
    assert!(drive(&mut session, &mut edits, &press));
    assert!(edits.gesture_active());
    let drag = MaskPointerEvents {
        drag_to: vec![(0.8, 0.5)],
        ..MaskPointerEvents::default()
    };
    assert!(drive(&mut session, &mut edits, &drag));
    let release = MaskPointerEvents {
        release: true,
        ..MaskPointerEvents::default()
    };
    drive(&mut session, &mut edits, &release);
    assert!(!edits.gesture_active());
    assert_eq!(edits.recipe().local.layers.len(), 1);
    assert!(edits.undo());
    assert!(edits.recipe().local.layers.is_empty());
    assert!(!edits.can_undo());
}

fn paint_session() -> MaskToolSession {
    let mut session = MaskToolSession::default();
    session.create_layer(MaskEditMode::Paint { erase: false });
    session
}

#[test]
fn a_stroke_is_one_undo_step_through_the_pointer() {
    let mut edits = EditSession::from_saved(EditRecipe::default());
    let mut session = paint_session();
    let press = MaskPointerEvents {
        press: Some((0.2, 0.5)),
        drag_to: vec![(0.3, 0.5), (0.4, 0.5)],
        ..MaskPointerEvents::default()
    };
    assert!(drive(&mut session, &mut edits, &press));
    assert!(edits.gesture_active());
    let release = MaskPointerEvents {
        drag_to: vec![(0.6, 0.5)],
        release: true,
        ..MaskPointerEvents::default()
    };
    assert!(drive(&mut session, &mut edits, &release));
    assert!(!edits.gesture_active());
    assert_eq!(edits.recipe().local.layers.len(), 1);
    // The tool stays in painting for the next stroke.
    assert!(session.is_painting());

    assert!(edits.undo());
    assert_eq!(*edits.recipe(), EditRecipe::default());
    assert!(!edits.can_undo());
}

#[test]
fn a_click_that_is_pressed_and_released_in_one_frame_paints_a_dab() {
    let mut edits = EditSession::from_saved(EditRecipe::default());
    let mut session = paint_session();
    let click = MaskPointerEvents {
        press: Some((0.5, 0.5)),
        release: true,
        ..MaskPointerEvents::default()
    };
    assert!(drive(&mut session, &mut edits, &click));
    assert!(!edits.gesture_active());
    assert!(!session.is_dragging());
    let MaskShape::Brush(brush) = &edits.recipe().local.layers[0].components[0].shape else {
        panic!("a brush layer");
    };
    assert_eq!(brush.strokes[0].points.len(), 1);
}

#[test]
fn holding_alt_at_the_press_erases_from_the_selected_brush() {
    let mut edits = EditSession::from_saved(EditRecipe::default());
    let mut session = paint_session();
    let paint = MaskPointerEvents {
        press: Some((0.2, 0.5)),
        drag_to: vec![(0.6, 0.5)],
        release: true,
        ..MaskPointerEvents::default()
    };
    drive(&mut session, &mut edits, &paint);
    let erase = MaskPointerEvents {
        press: Some((0.3, 0.5)),
        drag_to: vec![(0.4, 0.5)],
        release: true,
        erase_held: true,
        ..MaskPointerEvents::default()
    };
    assert!(drive(&mut session, &mut edits, &erase));
    let MaskShape::Brush(brush) = &edits.recipe().local.layers[0].components[0].shape else {
        panic!("a brush layer");
    };
    let erases: Vec<bool> = brush.strokes.iter().map(|stroke| stroke.erase).collect();
    assert_eq!(erases, [false, true]);
    // Letting go of Alt is back to painting; the mode was never changed.
    assert_eq!(session.mode, MaskEditMode::Paint { erase: false });
}

#[test]
fn a_brush_press_that_hits_a_limit_leaves_no_gesture_open() {
    let mut recipe = EditRecipe::default();
    recipe.local.layers.push(MaskLayer {
        components: vec![MaskComponent {
            mode: MaskMode::Add,
            invert: false,
            shape: MaskShape::Brush(BrushMask {
                strokes: vec![BrushStroke {
                    points: vec![MaskPoint { x: 0.5, y: 0.5 }; MAX_BRUSH_POINTS],
                    radius: 0.05,
                    feather: 0.5,
                    flow: 1.0,
                    erase: false,
                }],
            }),
        }],
        ..recipe_with_layer().local.layers.remove(0)
    });
    let mut edits = EditSession::from_saved(recipe);
    let mut session = paint_session();
    let press = MaskPointerEvents {
        press: Some((0.5, 0.5)),
        drag_to: vec![(0.6, 0.5)],
        ..MaskPointerEvents::default()
    };
    assert!(!drive(&mut session, &mut edits, &press));
    assert!(!edits.gesture_active());
    assert!(session.limit_reached().is_some());
    assert!(!edits.can_undo());
}
