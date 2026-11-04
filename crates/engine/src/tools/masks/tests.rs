//! Behaviour of the Masks tool through its pointer API, driven the way a frontend drives it.
//!
//! The developed frame is 600x400 and shown 1:1, so a screen pixel is `1/600` of a long-edge
//! unit and the 12 px grab radius is `0.02`. Display positions are normalized over the
//! 600x400 image; in long-edge units a step of `d` in display y is `d * 2/3`.

use rohditor_core::OutputGeometry;
use rohditor_edit::{
    BrushMask, BrushStroke, EditRecipe, LinearGradient, MAX_BRUSH_POINTS, MAX_BRUSH_STROKES,
    MAX_MASK_LAYERS, MaskMode, MaskPoint, MaskShape, NormalizedCropRect, RadialGradient,
};
use rohditor_image::Orientation;

use super::edit;
use super::{MaskEditMode, MaskHandle, MaskLimit, MaskToolSession, MaskView};
use crate::document::EditSession;

fn view(orientation: Orientation, crop: Option<NormalizedCropRect>) -> MaskView {
    MaskView {
        geometry: OutputGeometry::new(600, 400, orientation, crop).expect("valid geometry"),
        screen_scale: 1.0,
    }
}

/// A session, its edit history and its view, with the plumbing of one pointer gesture.
struct Harness {
    edits: EditSession,
    session: MaskToolSession,
    view: MaskView,
}

impl Harness {
    fn new(recipe: EditRecipe) -> Self {
        Self::with_view(recipe, view(Orientation::Normal, None))
    }

    fn with_view(recipe: EditRecipe, view: MaskView) -> Self {
        Self {
            edits: EditSession::from_saved(recipe),
            session: MaskToolSession::default(),
            view,
        }
    }

    /// A harness whose recipe holds one layer with `shape`, selected.
    fn selecting(shape: MaskShape) -> Self {
        let (recipe, at) = edit::add_component(&EditRecipe::default(), None, shape).expect("room");
        let mut harness = Self::new(recipe);
        harness.session.selected_layer = Some(at.layer);
        harness.session.selected_component = Some(at.component);
        harness
    }

    /// Installs a recipe the tool returned. The tool must only ever return valid recipes.
    fn apply(&mut self, next: Option<EditRecipe>) {
        if let Some(next) = next {
            assert!(next.validate().is_ok(), "the tool returned {next:?}");
            self.edits.set_during_gesture(next);
        }
    }

    fn press(&mut self, position: (f64, f64)) {
        self.edits.begin_gesture();
        let next = self
            .session
            .press(self.edits.recipe(), &self.view, position);
        self.apply(next);
    }

    fn drag_to(&mut self, position: (f64, f64)) {
        let next = self
            .session
            .drag_to(self.edits.recipe(), &self.view, position);
        self.apply(next);
    }

    fn release(&mut self) {
        let next = self.session.release(self.edits.recipe());
        self.apply(next);
        self.edits.finish_gesture();
    }

    /// Press, move along `path` in steps, release.
    fn gesture(&mut self, from: (f64, f64), path: &[(f64, f64)]) {
        self.press(from);
        for position in path {
            self.drag_to(*position);
        }
        self.release();
    }

    fn recipe(&self) -> &EditRecipe {
        self.edits.recipe()
    }

    fn shape(&self, layer: u32, component: usize) -> &MaskShape {
        &self.recipe().local.layer(layer).expect("layer").components[component].shape
    }

    fn linear(&self) -> LinearGradient {
        match self.shape(0, 0) {
            MaskShape::Linear(gradient) => *gradient,
            other => panic!("expected a linear gradient, got {other:?}"),
        }
    }

    fn radial(&self) -> RadialGradient {
        match self.shape(0, 0) {
            MaskShape::Radial(gradient) => *gradient,
            other => panic!("expected a radial gradient, got {other:?}"),
        }
    }

    fn strokes(&self, layer: u32, component: usize) -> &[BrushStroke] {
        match self.shape(layer, component) {
            MaskShape::Brush(brush) => &brush.strokes,
            other => panic!("expected a brush, got {other:?}"),
        }
    }
}

fn point(x: f32, y: f32) -> MaskPoint {
    MaskPoint { x, y }
}

fn assert_close(actual: f32, expected: f32) {
    assert!(
        (actual - expected).abs() < 1e-5,
        "got {actual}, want {expected}"
    );
}

fn assert_point(actual: MaskPoint, expected: (f32, f32)) {
    assert_close(actual.x, expected.0);
    assert_close(actual.y, expected.1);
}

/// Horizontal gradient across the middle: in long-edge space start (0.2, 1/3), end (0.8, 1/3).
fn linear_shape() -> MaskShape {
    MaskShape::Linear(LinearGradient {
        start: point(0.2, 0.5),
        end: point(0.8, 0.5),
    })
}

/// Wide ellipse in the middle: radii 0.3 and 0.15 long-edge units, feather 0.5.
fn radial_shape() -> MaskShape {
    MaskShape::Radial(RadialGradient {
        center: point(0.5, 0.5),
        radius_x: 0.3,
        radius_y: 0.15,
        angle_degrees: 0.0,
        feather: 0.5,
    })
}

fn brush_shape(strokes: Vec<BrushStroke>) -> MaskShape {
    MaskShape::Brush(BrushMask { strokes })
}

fn stroke_with(points: usize) -> BrushStroke {
    BrushStroke {
        points: vec![point(0.5, 0.5); points],
        radius: 0.05,
        feather: 0.5,
        flow: 1.0,
        erase: false,
    }
}

// --- creating gradients -----------------------------------------------------------------

#[test]
fn dragging_in_create_linear_adds_a_selected_gradient_in_one_undo_step() {
    let mut harness = Harness::new(EditRecipe::default());
    harness.session.mode = MaskEditMode::CreateLinear;
    harness.gesture(
        (0.2, 0.3),
        &[(0.3, 0.3), (0.45, 0.35), (0.5, 0.4), (0.6, 0.5)],
    );

    let local = &harness.recipe().local;
    assert_eq!(local.layers.len(), 1);
    assert_eq!(local.layers[0].components.len(), 1);
    let gradient = harness.linear();
    assert_point(gradient.start, (0.2, 0.3));
    assert_point(gradient.end, (0.6, 0.5));
    assert_eq!(harness.session.selected_layer, Some(0));
    assert_eq!(harness.session.selected_component, Some(0));
    // Back to grabbing handles, so the next drag edits what was just drawn.
    assert_eq!(harness.session.mode, MaskEditMode::Select);

    assert!(harness.edits.undo());
    assert_eq!(*harness.recipe(), EditRecipe::default());
    assert!(!harness.edits.can_undo());
}

#[test]
fn a_radial_is_a_circle_whose_radius_is_the_same_in_every_direction() {
    for release in [(0.6, 0.5), (0.5, 0.65), (0.4, 0.5)] {
        // 0.1 across is 0.1 long-edge units; 0.15 down is 0.15 * 2/3 = 0.1 as well.
        let mut harness = Harness::new(EditRecipe::default());
        harness.session.mode = MaskEditMode::CreateRadial;
        harness.gesture((0.5, 0.5), &[(0.55, 0.52), release]);

        let radial = harness.radial();
        assert_point(radial.center, (0.5, 0.5));
        assert_close(radial.radius_x, 0.1);
        assert_close(radial.radius_y, 0.1);
        assert_eq!(radial.angle_degrees, 0.0);
        assert_eq!(harness.session.selected_component, Some(0));
    }
}

#[test]
fn a_click_or_a_tiny_drag_creates_nothing() {
    let mut harness = Harness::new(EditRecipe::default());
    harness.session.mode = MaskEditMode::CreateLinear;
    harness.gesture((0.2, 0.3), &[]);
    harness.gesture((0.2, 0.3), &[(0.2 + 3.0 / 600.0, 0.3)]);
    assert_eq!(*harness.recipe(), EditRecipe::default());
    assert!(!harness.edits.can_undo());
    // Still waiting for a real drag.
    assert_eq!(harness.session.mode, MaskEditMode::CreateLinear);
    assert_eq!(harness.session.selected_layer, None);
}

#[test]
fn creating_with_a_layer_selected_adds_a_component_to_it() {
    let mut harness = Harness::selecting(linear_shape());
    harness.session.mode = MaskEditMode::CreateRadial;
    harness.gesture((0.5, 0.5), &[(0.6, 0.5)]);
    let layer = harness.recipe().local.layer(0).expect("layer");
    assert_eq!(harness.recipe().local.layers.len(), 1);
    assert_eq!(layer.components.len(), 2);
    assert!(matches!(layer.components[1].shape, MaskShape::Radial(_)));
    assert_eq!(harness.session.selected_component, Some(1));

    // With nothing selected it starts a new layer.
    harness.session.selected_layer = None;
    harness.session.mode = MaskEditMode::CreateLinear;
    harness.gesture((0.1, 0.1), &[(0.4, 0.4)]);
    assert_eq!(harness.recipe().local.layers.len(), 2);
    assert_eq!(harness.session.selected_layer, Some(1));
}

#[test]
fn a_gradient_drawn_into_a_layer_takes_the_requested_mode_but_a_new_layer_stays_add() {
    let mut harness = Harness::selecting(linear_shape());
    harness
        .session
        .create_component(MaskEditMode::CreateRadial, MaskMode::Subtract);
    harness.gesture((0.5, 0.5), &[(0.6, 0.5)]);
    let components = &harness.recipe().local.layer(0).expect("layer").components;
    assert_eq!(components[0].mode, MaskMode::Add);
    assert_eq!(components[1].mode, MaskMode::Subtract);
    assert!(harness.recipe().validate().is_ok());

    // Even with Subtract pending, the first component of a new layer has to be Add.
    harness.session.selected_layer = None;
    harness
        .session
        .create_component(MaskEditMode::CreateLinear, MaskMode::Subtract);
    harness.gesture((0.1, 0.1), &[(0.4, 0.4)]);
    let new_layer = harness.recipe().local.layer(1).expect("new layer");
    assert_eq!(new_layer.components[0].mode, MaskMode::Add);
    assert!(harness.recipe().validate().is_ok());
}

#[test]
fn creation_follows_the_pointer_through_orientation_and_crop() {
    let crop = NormalizedCropRect {
        left: 0.1,
        top: 0.2,
        right: 0.9,
        bottom: 0.7,
    };
    for orientation in [
        Orientation::Normal,
        Orientation::Rotate90,
        Orientation::Rotate180,
        Orientation::Transverse,
    ] {
        let view = view(orientation, Some(crop));
        let mut harness = Harness::with_view(EditRecipe::default(), view);
        harness.session.mode = MaskEditMode::CreateLinear;
        harness.gesture((0.25, 0.2), &[(0.7, 0.8)]);

        let gradient = harness.linear();
        let start = view.geometry.display_to_developed((0.25, 0.2));
        let end = view.geometry.display_to_developed((0.7, 0.8));
        assert_point(gradient.start, (start.0 as f32, start.1 as f32));
        assert_point(gradient.end, (end.0 as f32, end.1 as f32));
    }
}

#[test]
fn a_full_recipe_reports_the_layer_limit_instead_of_growing() {
    let mut recipe = EditRecipe::default();
    for _ in 0..MAX_MASK_LAYERS {
        recipe = edit::add_component(&recipe, None, linear_shape())
            .expect("room")
            .0;
    }
    let mut harness = Harness::new(recipe.clone());
    harness.session.mode = MaskEditMode::CreateRadial;
    harness.gesture((0.5, 0.5), &[(0.6, 0.5)]);
    assert_eq!(*harness.recipe(), recipe);
    assert_eq!(harness.session.limit_reached(), Some(MaskLimit::Layers));
    // The next press starts clean.
    harness.press((0.5, 0.5));
    assert_eq!(harness.session.limit_reached(), None);
    harness.release();
}

// --- handles ----------------------------------------------------------------------------

#[test]
fn linear_start_and_end_handles_move_only_their_own_end() {
    let mut harness = Harness::selecting(linear_shape());
    harness.gesture((0.2, 0.5), &[(0.25, 0.55), (0.3, 0.6)]);
    let gradient = harness.linear();
    assert_point(gradient.start, (0.3, 0.6));
    assert_point(gradient.end, (0.8, 0.5));

    harness.gesture((0.8, 0.5), &[(0.7, 0.3)]);
    let gradient = harness.linear();
    assert_point(gradient.start, (0.3, 0.6));
    assert_point(gradient.end, (0.7, 0.3));
}

#[test]
fn the_linear_centre_handle_moves_both_ends_and_keeps_the_length() {
    let mut harness = Harness::selecting(linear_shape());
    harness.gesture((0.5, 0.5), &[(0.5, 0.6), (0.55, 0.7)]);
    let gradient = harness.linear();
    assert_point(gradient.start, (0.25, 0.7));
    assert_point(gradient.end, (0.85, 0.7));
}

#[test]
fn grabbing_the_linear_line_rotates_it_around_its_centre_without_changing_its_length() {
    let mut harness = Harness::selecting(linear_shape());
    // The line is 0.6 long-edge units, centred at (0.5, 1/3). Grab it 0.15 right of the centre
    // and swing that point round to 0.15 below it: a quarter turn clockwise on screen.
    harness.gesture((0.65, 0.5), &[(0.6, 0.6), (0.5, 0.725)]);
    let gradient = harness.linear();
    // West end to the north, east end to the south, 0.3 each from the centre; in normalized
    // y that is 1/3 -/+ 0.3 over the frame's 2/3.
    assert_point(gradient.start, (0.5, 0.05));
    assert_point(gradient.end, (0.5, 0.95));
}

#[test]
fn rotating_a_diagonal_line_turns_both_coordinates() {
    // The axis-aligned case above never exercises the cross terms of the rotation.
    // In long-edge units the line runs (0.3, 0.2) -> (0.7, 0.4667) around the centre
    // (0.5, 1/3). Grab it a quarter of the way to the end, at (0.6, 0.4), and bring that point
    // a quarter turn clockwise round the centre, to (0.4333, 0.4333): the offset (0.1, 0.0667)
    // becomes (-0.0667, 0.1). Every point of the line turns with it, (x, y) -> (-y, x).
    let mut harness = Harness::selecting(MaskShape::Linear(LinearGradient {
        start: point(0.3, 0.3),
        end: point(0.7, 0.7),
    }));
    harness.gesture((0.6, 0.6), &[(0.55, 0.65), (0.4333333, 0.65)]);
    let gradient = harness.linear();
    // start offset (-0.2, -0.1333) -> (0.1333, -0.2); end offset is the opposite.
    assert_point(gradient.start, (0.633_333_3, 0.2));
    assert_point(gradient.end, (0.366_666_7, 0.8));
}

#[test]
fn a_linear_handle_cannot_collapse_the_gradient_onto_a_point() {
    let mut harness = Harness::selecting(linear_shape());
    let before = harness.recipe().clone();
    // End dragged onto the start: the gradient would have no length, so the move is refused
    // and the recipe stays at its last valid value.
    harness.gesture((0.8, 0.5), &[(0.2, 0.5)]);
    assert_eq!(*harness.recipe(), before);
}

#[test]
fn dragging_a_handle_away_and_back_restores_the_original_shape() {
    // Moves are computed from the grabbed shape, so there is no drift to undo.
    let mut harness = Harness::selecting(radial_shape());
    let before = harness.recipe().clone();
    harness.press((0.8, 0.5));
    for position in [(0.9, 0.5), (0.99, 0.5), (0.5, 0.5), (0.95, 0.8), (0.8, 0.5)] {
        harness.drag_to(position);
    }
    harness.release();
    assert_eq!(*harness.recipe(), before);
}

#[test]
fn radius_handles_change_only_their_own_radius_and_keep_the_centre() {
    let mut harness = Harness::selecting(radial_shape());
    // x marker at centre + 0.3 -> display (0.8, 0.5); drag 0.1 further out.
    harness.gesture((0.8, 0.5), &[(0.9, 0.5)]);
    let radial = harness.radial();
    assert_close(radial.radius_x, 0.4);
    assert_close(radial.radius_y, 0.15);
    assert_point(radial.center, (0.5, 0.5));
    assert_eq!(radial.angle_degrees, 0.0);
    assert_eq!(radial.feather, 0.5);

    // y marker at centre + 0.15 -> display y = 0.5 + 0.15 / (2/3); drag 0.05 further out.
    harness.gesture((0.5, 0.725), &[(0.5, 0.8)]);
    let radial = harness.radial();
    assert_close(radial.radius_x, 0.4);
    assert_close(radial.radius_y, 0.2);
    assert_point(radial.center, (0.5, 0.5));
}

#[test]
fn a_radius_handle_ignores_movement_across_its_axis() {
    let mut harness = Harness::selecting(radial_shape());
    let before = harness.recipe().clone();
    harness.gesture((0.8, 0.5), &[(0.8, 0.9)]);
    assert_eq!(*harness.recipe(), before);
}

#[test]
fn a_radius_handle_stops_at_the_smallest_radius_instead_of_inverting() {
    let mut harness = Harness::selecting(radial_shape());
    harness.gesture((0.8, 0.5), &[(0.1, 0.5)]);
    assert_close(harness.radial().radius_x, 1e-3);
    assert_point(harness.radial().center, (0.5, 0.5));
}

#[test]
fn grabbing_the_outline_rotates_the_ellipse_around_its_centre() {
    let mut harness = Harness::selecting(radial_shape());
    // The far left of the outline, swung over the top: a quarter turn clockwise on screen.
    harness.gesture((0.2, 0.5), &[(0.3, 0.2), (0.5, 0.05)]);
    let radial = harness.radial();
    assert_close(radial.angle_degrees, 90.0);
    assert_close(radial.radius_x, 0.3);
    assert_close(radial.radius_y, 0.15);
    assert_point(radial.center, (0.5, 0.5));
}

#[test]
fn the_feather_handle_runs_from_the_outline_to_the_centre() {
    let mut harness = Harness::selecting(radial_shape());
    // Feather 0.5: halfway out along the 45 degree diagonal of the ellipse.
    let marker = harness
        .session
        .handle_markers(harness.recipe(), &harness.view)
        .into_iter()
        .find(|marker| marker.handle == MaskHandle::RadialFeather)
        .expect("a radial has a feather marker")
        .position;
    let on_outline = (
        0.5 + 0.3 * 0.5_f64.sqrt(),
        0.5 + 0.15 * 0.5_f64.sqrt() / (2.0 / 3.0),
    );

    harness.gesture(marker, &[on_outline]);
    assert_close(harness.radial().feather, 0.0);
    harness.gesture(on_outline, &[(0.5, 0.5)]);
    assert_close(harness.radial().feather, 1.0);
    assert_close(harness.radial().radius_x, 0.3);
}

#[test]
fn pressing_inside_the_ellipse_moves_it_and_pressing_outside_grabs_nothing() {
    let mut harness = Harness::selecting(radial_shape());
    harness.gesture((0.55, 0.5), &[(0.6, 0.55), (0.65, 0.6)]);
    let radial = harness.radial();
    assert_point(radial.center, (0.6, 0.6));
    assert_close(radial.radius_x, 0.3);
    assert_close(radial.radius_y, 0.15);

    let before = harness.recipe().clone();
    harness.press((0.05, 0.05));
    assert!(!harness.session.is_dragging());
    harness.drag_to((0.2, 0.2));
    harness.release();
    assert_eq!(*harness.recipe(), before);
}

#[test]
fn handle_drags_are_one_undo_step_each() {
    let mut harness = Harness::selecting(linear_shape());
    let original = harness.recipe().clone();
    harness.gesture((0.2, 0.5), &[(0.3, 0.5), (0.4, 0.5)]);
    harness.gesture((0.8, 0.5), &[(0.7, 0.5), (0.6, 0.5)]);
    assert!(harness.edits.undo());
    assert_point(harness.linear().start, (0.4, 0.5));
    assert_point(harness.linear().end, (0.8, 0.5));
    assert!(harness.edits.undo());
    assert_eq!(*harness.recipe(), original);
}

#[test]
fn without_a_valid_selection_a_press_in_select_mode_grabs_nothing() {
    let mut harness = Harness::selecting(linear_shape());
    harness.session.selected_component = None;
    harness.press((0.2, 0.5));
    assert!(!harness.session.is_dragging());
    harness.release();

    harness.session.selected_component = Some(0);
    harness.session.selected_layer = Some(99);
    harness.press((0.2, 0.5));
    assert!(!harness.session.is_dragging());
    harness.release();
    assert!(!harness.edits.can_undo());
}

#[test]
fn handles_are_only_offered_while_they_can_be_grabbed() {
    let mut harness = Harness::selecting(linear_shape());
    assert_eq!(
        harness
            .session
            .hit_test(harness.recipe(), &harness.view, (0.2, 0.5)),
        Some(MaskHandle::LinearStart)
    );
    assert_eq!(
        harness
            .session
            .handle_markers(harness.recipe(), &harness.view)
            .len(),
        3
    );
    harness.session.mode = MaskEditMode::CreateLinear;
    assert!(
        harness
            .session
            .handle_markers(harness.recipe(), &harness.view)
            .is_empty()
    );
    assert_eq!(
        harness
            .session
            .hit_test(harness.recipe(), &harness.view, (0.2, 0.5)),
        None
    );
}

#[test]
fn markers_and_drags_follow_the_display_orientation() {
    let view = view(Orientation::Rotate90, None);
    let (recipe, at) =
        edit::add_component(&EditRecipe::default(), None, linear_shape()).expect("room");
    let mut harness = Harness::with_view(recipe, view);
    harness.session.selected_layer = Some(at.layer);
    harness.session.selected_component = Some(at.component);

    // Each marker is drawn where its recipe point shows on screen.
    let start = harness.linear().start;
    let markers = harness
        .session
        .handle_markers(harness.recipe(), &harness.view);
    let start_marker = markers
        .iter()
        .find(|marker| marker.handle == MaskHandle::LinearStart)
        .expect("start marker");
    let developed = view.geometry.display_to_developed(start_marker.position);
    assert_close(developed.0 as f32, start.x);
    assert_close(developed.1 as f32, start.y);

    // Rotate90 shows developed y flipped along the display's x axis: moving the centre 0.1 to
    // the right on screen moves the gradient 0.1 up the developed frame, not sideways.
    let centre = markers
        .iter()
        .find(|marker| marker.handle == MaskHandle::LinearCenter)
        .expect("centre marker")
        .position;
    harness.gesture(centre, &[(centre.0 + 0.1, centre.1)]);
    let moved = harness.linear();
    assert_close(moved.start.x, 0.2);
    assert_close(moved.start.y, 0.4);
    assert_close(moved.end.x, 0.8);
    assert_close(moved.end.y, 0.4);
}

// --- brush ------------------------------------------------------------------------------

#[test]
fn a_stroke_is_one_undo_step_and_records_a_bounded_number_of_points() {
    let mut harness = Harness::new(EditRecipe::default());
    harness.session.mode = MaskEditMode::Paint { erase: false };
    // Radius 0.05: points every 0.0125 long-edge units, tolerance 0.005.
    let steps = 600;
    let path: Vec<(f64, f64)> = (1..=steps)
        .map(|i| (0.2 + 0.6 * f64::from(i) / f64::from(steps), 0.5))
        .collect();
    harness.press((0.2, 0.5));
    for position in &path {
        harness.drag_to(*position);
    }
    // While painting: first point + one per spacing + the tentative tip. 0.6 / 0.0125 = 48.
    let recorded = harness.strokes(0, 0)[0].points.len();
    assert!((48..=51).contains(&recorded), "recorded {recorded} points");
    harness.release();

    // A straight run simplifies to its ends, and the end is where the pointer was released.
    let stroke = &harness.strokes(0, 0)[0];
    assert_eq!(stroke.points.len(), 2);
    assert_point(stroke.points[0], (0.2, 0.5));
    assert_point(stroke.points[1], (0.8, 0.5));
    assert_eq!(
        (stroke.radius, stroke.feather, stroke.flow, stroke.erase),
        (0.05, 0.5, 1.0, false)
    );
    assert_eq!(harness.session.selected_layer, Some(0));
    assert_eq!(harness.session.selected_component, Some(0));
    // Still painting: the mode stays.
    assert_eq!(harness.session.mode, MaskEditMode::Paint { erase: false });

    assert!(harness.edits.undo());
    assert_eq!(*harness.recipe(), EditRecipe::default());
    assert!(!harness.edits.can_undo());
}

#[test]
fn a_curved_stroke_keeps_enough_points_to_follow_the_curve() {
    let mut harness = Harness::new(EditRecipe::default());
    harness.session.mode = MaskEditMode::Paint { erase: false };
    // A semicircle of radius 0.3 long-edge units: 0.3 across, 0.3 * 3/2 = 0.45 up in display y.
    let arc = |t: f64| {
        let angle = std::f64::consts::PI * t;
        (0.5 - 0.3 * angle.cos(), 0.7 - 0.45 * angle.sin())
    };
    harness.press(arc(0.0));
    for step in 1..=400 {
        harness.drag_to(arc(f64::from(step) / 400.0));
    }
    harness.release();
    let points = harness.strokes(0, 0)[0].points.len();
    assert!((4..=40).contains(&points), "kept {points} points");
}

#[test]
fn moving_less_than_the_spacing_only_moves_the_tentative_end() {
    let mut harness = Harness::new(EditRecipe::default());
    harness.session.mode = MaskEditMode::Paint { erase: false };
    harness.press((0.5, 0.5));
    // 0.0125 long-edge units is 7.5 px; wander 2 px at a time within it.
    for step in 1..=3 {
        harness.drag_to((0.5 + f64::from(step) * 2.0 / 600.0, 0.5));
    }
    let stroke = &harness.strokes(0, 0)[0];
    assert_eq!(stroke.points.len(), 2, "first point plus one tentative end");
    assert_point(stroke.points[0], (0.5, 0.5));
    assert_point(stroke.points[1], (0.5 + 6.0 / 600.0, 0.5));
    harness.release();
}

#[test]
fn a_click_paints_a_single_dab() {
    let mut harness = Harness::new(EditRecipe::default());
    harness.session.mode = MaskEditMode::Paint { erase: false };
    harness.gesture((0.3, 0.4), &[]);
    let stroke = &harness.strokes(0, 0)[0];
    assert_eq!(stroke.points.len(), 1);
    assert_point(stroke.points[0], (0.3, 0.4));
}

#[test]
fn later_strokes_join_the_selected_brush_and_keep_their_own_settings() {
    let mut harness = Harness::new(EditRecipe::default());
    harness.session.mode = MaskEditMode::Paint { erase: false };
    harness.gesture((0.2, 0.2), &[(0.3, 0.2)]);
    harness.session.brush.radius = 0.1;
    harness.session.brush.flow = 0.4;
    harness.gesture((0.2, 0.6), &[(0.3, 0.6)]);
    harness.session.mode = MaskEditMode::Paint { erase: true };
    harness.gesture((0.25, 0.6), &[(0.28, 0.6)]);

    assert_eq!(harness.recipe().local.layers.len(), 1);
    let strokes = harness.strokes(0, 0);
    assert_eq!(strokes.len(), 3);
    assert_eq!((strokes[0].radius, strokes[0].flow), (0.05, 1.0));
    assert_eq!((strokes[1].radius, strokes[1].flow), (0.1, 0.4));
    assert!(strokes[2].erase && !strokes[1].erase);

    // Each stroke undoes on its own.
    assert!(harness.edits.undo());
    assert_eq!(harness.strokes(0, 0).len(), 2);
    assert!(harness.edits.undo());
    assert_eq!(harness.strokes(0, 0).len(), 1);
}

#[test]
fn erasing_without_a_brush_to_erase_from_does_nothing() {
    let mut harness = Harness::selecting(linear_shape());
    harness.session.mode = MaskEditMode::Paint { erase: true };
    let before = harness.recipe().clone();
    harness.gesture((0.3, 0.4), &[(0.4, 0.4)]);
    assert_eq!(*harness.recipe(), before);
    assert!(!harness.edits.can_undo());
}

#[test]
fn painting_over_a_gradient_adds_a_brush_component_to_its_layer() {
    let mut harness = Harness::selecting(linear_shape());
    harness.session.mode = MaskEditMode::Paint { erase: false };
    harness.gesture((0.3, 0.4), &[(0.4, 0.4)]);
    let layer = harness.recipe().local.layer(0).expect("layer");
    assert_eq!(layer.components.len(), 2);
    assert!(matches!(layer.components[0].shape, MaskShape::Linear(_)));
    assert!(matches!(layer.components[1].shape, MaskShape::Brush(_)));
    assert_eq!(harness.session.selected_component, Some(1));
}

#[test]
fn a_stroke_stops_recording_at_the_brush_point_limit_and_says_so() {
    // 199,998 points elsewhere leave room for two more.
    let (recipe, _) = edit::add_component(
        &EditRecipe::default(),
        None,
        brush_shape(vec![stroke_with(MAX_BRUSH_POINTS - 2)]),
    )
    .expect("room");
    let mut harness = Harness::new(recipe);
    harness.session.mode = MaskEditMode::Paint { erase: false };

    harness.press((0.2, 0.5));
    assert_eq!(harness.session.limit_reached(), None);
    for step in 1..=20 {
        harness.drag_to((0.2 + f64::from(step) * 0.005, 0.5));
    }
    assert_eq!(
        harness.session.limit_reached(),
        Some(MaskLimit::BrushPoints)
    );
    // The gesture survives, so the release still finishes the stroke.
    assert!(harness.session.is_dragging());
    harness.release();

    let total = edit::brush_point_total(harness.recipe());
    assert_eq!(total, MAX_BRUSH_POINTS);
    assert!(harness.recipe().validate().is_ok());
}

#[test]
fn painting_is_refused_when_no_brush_point_is_left_or_the_brush_has_all_its_strokes() {
    let (recipe, _) = edit::add_component(
        &EditRecipe::default(),
        None,
        brush_shape(vec![stroke_with(MAX_BRUSH_POINTS)]),
    )
    .expect("room");
    let mut harness = Harness::new(recipe.clone());
    harness.session.mode = MaskEditMode::Paint { erase: false };
    harness.gesture((0.2, 0.5), &[(0.3, 0.5)]);
    assert_eq!(
        harness.session.limit_reached(),
        Some(MaskLimit::BrushPoints)
    );
    assert_eq!(*harness.recipe(), recipe);

    let strokes = (0..MAX_BRUSH_STROKES).map(|_| stroke_with(1)).collect();
    let (recipe, at) =
        edit::add_component(&EditRecipe::default(), None, brush_shape(strokes)).expect("room");
    let mut harness = Harness::new(recipe.clone());
    harness.session.selected_layer = Some(at.layer);
    harness.session.selected_component = Some(at.component);
    harness.session.mode = MaskEditMode::Paint { erase: false };
    harness.gesture((0.2, 0.5), &[(0.3, 0.5)]);
    assert_eq!(harness.session.limit_reached(), Some(MaskLimit::Strokes));
    assert_eq!(*harness.recipe(), recipe);
}

// --- overlay and selection --------------------------------------------------------------

#[test]
fn the_overlay_request_follows_the_selection_and_the_toggle() {
    let mut session = MaskToolSession::default();
    assert_eq!(session.overlay_layer(), None);
    session.selected_layer = Some(3);
    assert_eq!(session.overlay_layer(), Some(3));
    session.show_overlay = false;
    assert_eq!(session.overlay_layer(), None);
}

#[test]
fn a_gesture_whose_target_disappeared_changes_nothing() {
    // Undo in the middle of a drag removes the component the drag was editing.
    let mut harness = Harness::new(EditRecipe::default());
    harness.session.mode = MaskEditMode::CreateLinear;
    harness.press((0.2, 0.3));
    harness.drag_to((0.5, 0.3));
    assert!(harness.edits.undo());
    assert_eq!(*harness.recipe(), EditRecipe::default());

    let next = harness
        .session
        .drag_to(harness.edits.recipe(), &harness.view, (0.6, 0.4));
    assert_eq!(next, None);
    assert_eq!(harness.session.release(harness.edits.recipe()), None);
}
