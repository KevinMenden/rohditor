//! Pointer gestures of the Masks tool: grabbing a handle, drawing a gradient, painting a stroke.
//!
//! A gesture starts at [`MaskToolSession::press`], follows the pointer with
//! [`MaskToolSession::drag_to`] and ends at [`MaskToolSession::release`]. The state of the
//! gesture in progress lives in [`MaskDrag`]. Each step returns the recipe to install, if
//! the gesture changed it.

use rohditor_edit::{BrushMask, EditRecipe, MAX_BRUSH_STROKES, MaskShape};

use super::brush::{self, POINT_SPACING_PER_RADIUS, SIMPLIFY_TOLERANCE_PER_RADIUS};
use super::edit::{self, GradientKind, Placement};
use super::handles;
use super::view::{Space, Vec2, distance};
use super::{HandleMarker, MaskEditMode, MaskHandle, MaskLimit, MaskToolSession, MaskView};

/// How close, in screen pixels, a press must be to a handle to grab it.
const GRAB_RADIUS_PX: f64 = 12.0;

/// How far, in screen pixels, a creation drag must travel before the gradient exists. A click
/// without a drag creates nothing.
const CREATE_MIN_DRAG_PX: f64 = 4.0;

/// The gesture in progress.
#[derive(Debug, Clone)]
pub(super) enum MaskDrag {
    Handle(HandleDrag),
    Create(CreateDrag),
    Stroke(StrokeDrag),
}

/// Dragging a handle of an existing component.
#[derive(Debug, Clone)]
pub(super) struct HandleDrag {
    target: Placement,
    handle: MaskHandle,
    /// Where the pointer grabbed the handle, in long-edge space.
    grab: Vec2,
    /// The shape when it was grabbed; every move is computed from it.
    original: MaskShape,
}

/// Drawing a new gradient from `origin`.
#[derive(Debug, Clone)]
pub(super) struct CreateDrag {
    kind: GradientKind,
    origin: Vec2,
    /// Where the gradient went once the drag was long enough to create it.
    placed: Option<Placement>,
}

/// Recording a brush stroke.
///
/// The stroke's points are the recorded ones plus, between recordings, a tentative last point
/// (`tip`) that follows the pointer, so the stroke always reaches the pointer.
#[derive(Debug, Clone)]
pub(super) struct StrokeDrag {
    target: Placement,
    /// Index of the stroke being painted in its brush component.
    stroke: usize,
    /// The last recorded point, in long-edge space.
    anchor: Vec2,
    /// Whether the stroke's last point is tentative.
    tip: bool,
    /// Points the recipe limit still allows.
    budget: usize,
    /// Distance between recorded points, in long-edge units.
    spacing: f64,
    /// Simplification tolerance, in long-edge units.
    tolerance: f64,
    space: Space,
}

impl MaskToolSession {
    /// The point handles of the selected component, at display positions, for drawing. Empty
    /// unless the mode is `Select`, the only mode in which they can be grabbed.
    pub fn handle_markers(&self, recipe: &EditRecipe, view: &MaskView) -> Vec<HandleMarker> {
        if self.mode != MaskEditMode::Select {
            return Vec::new();
        }
        let Some((_, shape)) = self.selected_shape(recipe) else {
            return Vec::new();
        };
        handles::markers(shape, view.space())
            .into_iter()
            .map(|(handle, position)| HandleMarker {
                handle,
                position: view.space_to_display(position),
            })
            .collect()
    }

    /// The handle a press at the display position `position` would grab, for hover feedback.
    pub fn hit_test(
        &self,
        recipe: &EditRecipe,
        view: &MaskView,
        position: (f64, f64),
    ) -> Option<MaskHandle> {
        if self.mode != MaskEditMode::Select {
            return None;
        }
        let (_, shape) = self.selected_shape(recipe)?;
        handles::hit_test(
            shape,
            view.space(),
            view.display_to_space(position),
            view.screen_to_units(GRAB_RADIUS_PX),
        )
    }

    /// Pointer pressed at a display position. Starts a gesture unless nothing was grabbed
    /// (`is_dragging` tells). Painting changes the recipe at once (a click makes a dab), so
    /// the press can return it.
    pub fn press(
        &mut self,
        recipe: &EditRecipe,
        view: &MaskView,
        position: (f64, f64),
    ) -> Option<EditRecipe> {
        self.drag = None;
        self.limit = None;
        let at = view.display_to_space(position);
        match self.mode {
            MaskEditMode::Select => {
                self.press_select(recipe, view, position, at);
                None
            }
            MaskEditMode::CreateLinear => self.press_create(GradientKind::Linear, at),
            MaskEditMode::CreateRadial => self.press_create(GradientKind::Radial, at),
            MaskEditMode::Paint { erase } => self.press_paint(recipe, view.space(), at, erase),
        }
    }

    /// Pointer moved to a display position during a gesture.
    pub fn drag_to(
        &mut self,
        recipe: &EditRecipe,
        view: &MaskView,
        position: (f64, f64),
    ) -> Option<EditRecipe> {
        let mut drag = self.drag.take()?;
        let at = view.display_to_space(position);
        let next = match &mut drag {
            MaskDrag::Handle(handle) => handle.update(recipe, view.space(), at),
            MaskDrag::Create(create) => self.update_create(create, recipe, view, at),
            MaskDrag::Stroke(stroke) => match stroke.extend(recipe, at) {
                Ok(next) => next,
                // Stop recording but keep the gesture, so release still simplifies the stroke.
                Err(limit) => {
                    self.limit = Some(limit);
                    None
                }
            },
        };
        self.drag = Some(drag);
        next
    }

    /// Pointer released: ends the gesture. A stroke is simplified here, so this can return a
    /// recipe too; install it before finishing the edit gesture.
    pub fn release(&mut self, recipe: &EditRecipe) -> Option<EditRecipe> {
        match self.drag.take()? {
            MaskDrag::Handle(_) => None,
            MaskDrag::Create(create) => {
                if create.placed.is_some() {
                    self.return_to_select();
                }
                None
            }
            MaskDrag::Stroke(stroke) => stroke.finish(recipe),
        }
    }

    /// The selected component, if the selection still points at one.
    pub(super) fn selected_shape<'a>(
        &self,
        recipe: &'a EditRecipe,
    ) -> Option<(Placement, &'a MaskShape)> {
        let layer = recipe.local.layer(self.selected_layer?)?;
        let component = self.selected_component?;
        let shape = &layer.components.get(component)?.shape;
        Some((
            Placement {
                layer: layer.id,
                component,
            },
            shape,
        ))
    }

    fn select(&mut self, target: Placement) {
        self.selected_layer = Some(target.layer);
        self.selected_component = Some(target.component);
    }

    fn press_select(
        &mut self,
        recipe: &EditRecipe,
        view: &MaskView,
        position: (f64, f64),
        at: Vec2,
    ) {
        let Some(handle) = self.hit_test(recipe, view, position) else {
            return;
        };
        let Some((target, shape)) = self.selected_shape(recipe) else {
            return;
        };
        self.drag = Some(MaskDrag::Handle(HandleDrag {
            target,
            handle,
            grab: at,
            original: shape.clone(),
        }));
    }

    /// Nothing exists until the drag is long enough, so a press only remembers where it was.
    fn press_create(&mut self, kind: GradientKind, at: Vec2) -> Option<EditRecipe> {
        self.drag = Some(MaskDrag::Create(CreateDrag {
            kind,
            origin: at,
            placed: None,
        }));
        None
    }

    fn update_create(
        &mut self,
        create: &mut CreateDrag,
        recipe: &EditRecipe,
        view: &MaskView,
        at: Vec2,
    ) -> Option<EditRecipe> {
        let space = view.space();
        if create.placed.is_none()
            && distance(at, create.origin) < view.screen_to_units(CREATE_MIN_DRAG_PX)
        {
            return None;
        }
        let shape = edit::gradient_shape(create.kind, space, create.origin, at)?;
        if let Some(placed) = create.placed {
            let mut next = recipe.clone();
            *edit::shape_mut(&mut next, placed)? = shape;
            return Some(next);
        }
        match edit::add_component(recipe, self.selected_layer, shape) {
            Ok((mut next, placed)) => {
                if placed.component > 0 {
                    edit::set_component_mode(&mut next, placed, self.component_mode);
                }
                create.placed = Some(placed);
                self.select(placed);
                Some(next)
            }
            Err(limit) => {
                self.limit = Some(limit);
                None
            }
        }
    }

    fn press_paint(
        &mut self,
        recipe: &EditRecipe,
        space: Space,
        at: Vec2,
        erase: bool,
    ) -> Option<EditRecipe> {
        // The erase modifier turns painting into erasing and the other way round.
        let erase = erase != self.erase_held;
        let budget = edit::brush_point_budget(recipe);
        if budget == 0 {
            self.limit = Some(MaskLimit::BrushPoints);
            return None;
        }
        let stroke = self.brush.begin_stroke(erase, space.space_to_point(at));
        let radius = f64::from(stroke.radius);

        // Paint into the selected brush component, unless a new one was asked for. Without a
        // brush to join, a new brush component takes the stroke; erasing has nothing to erase
        // from then.
        let selected_brush = self
            .selected_shape(recipe)
            .filter(|_| !self.new_brush_component)
            .and_then(|(target, shape)| match shape {
                MaskShape::Brush(brush) => Some((target, brush.strokes.len())),
                _ => None,
            });
        let (next, target, stroke_index) = if let Some((target, stroke_count)) = selected_brush {
            if stroke_count >= MAX_BRUSH_STROKES {
                self.limit = Some(MaskLimit::Strokes);
                return None;
            }
            let mut next = recipe.clone();
            let MaskShape::Brush(brush) = edit::shape_mut(&mut next, target)? else {
                return None;
            };
            brush.strokes.push(stroke);
            (next, target, stroke_count)
        } else if erase {
            return None;
        } else {
            let shape = MaskShape::Brush(BrushMask {
                strokes: vec![stroke],
            });
            match edit::add_component(recipe, self.selected_layer, shape) {
                Ok((mut next, target)) => {
                    // As for a gradient: only a component joining a layer combines with it.
                    if target.component > 0 {
                        edit::set_component_mode(&mut next, target, self.component_mode);
                    }
                    (next, target, 0)
                }
                Err(limit) => {
                    self.limit = Some(limit);
                    return None;
                }
            }
        };

        self.new_brush_component = false;
        self.select(target);
        self.drag = Some(MaskDrag::Stroke(StrokeDrag {
            target,
            stroke: stroke_index,
            anchor: at,
            tip: false,
            budget: budget - 1,
            spacing: radius * POINT_SPACING_PER_RADIUS,
            tolerance: radius * SIMPLIFY_TOLERANCE_PER_RADIUS,
            space,
        }));
        Some(next)
    }
}

impl HandleDrag {
    fn update(&self, recipe: &EditRecipe, space: Space, pointer: Vec2) -> Option<EditRecipe> {
        let shape = handles::drag(&self.original, space, self.handle, self.grab, pointer)?;
        let mut next = recipe.clone();
        *edit::shape_mut(&mut next, self.target)? = shape;
        Some(next)
    }
}

impl StrokeDrag {
    /// Follows the pointer to `at`. `Err` when the recipe's brush point limit stops the stroke.
    fn extend(&mut self, recipe: &EditRecipe, at: Vec2) -> Result<Option<EditRecipe>, MaskLimit> {
        let moved = distance(at, self.anchor);
        if moved == 0.0 {
            return Ok(None);
        }
        let mut next = recipe.clone();
        let Some(stroke) = edit::stroke_mut(&mut next, self.target, self.stroke) else {
            return Ok(None);
        };
        let point = self.space.space_to_point(at);
        if self.tip {
            if let Some(last) = stroke.points.last_mut() {
                *last = point;
            }
        } else if self.budget == 0 {
            return Err(MaskLimit::BrushPoints);
        } else {
            stroke.points.push(point);
            self.budget -= 1;
        }
        // Past the spacing the point is final and the next move starts a new one. Before it,
        // the point stays tentative and keeps following the pointer.
        self.tip = moved < self.spacing;
        if !self.tip {
            self.anchor = at;
        }
        Ok(Some(next))
    }

    /// The stroke with its polyline simplified, if that removes points.
    fn finish(&self, recipe: &EditRecipe) -> Option<EditRecipe> {
        let mut next = recipe.clone();
        let stroke = edit::stroke_mut(&mut next, self.target, self.stroke)?;
        let points: Vec<Vec2> = stroke
            .points
            .iter()
            .map(|point| self.space.point_to_space(*point))
            .collect();
        let kept = brush::simplify(&points, self.tolerance);
        if kept.len() == points.len() {
            return None;
        }
        let simplified = kept.into_iter().map(|index| stroke.points[index]).collect();
        stroke.points = simplified;
        Some(next)
    }
}
