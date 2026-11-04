//! Recipe edits of the Masks tool, as pure functions over `EditRecipe`.
//!
//! Adding to a recipe checks the limits `LocalAdjustments::validate` enforces first, so the
//! tool reports a limit instead of producing a recipe the edit session would silently reject.

use rohditor_edit::{
    BrushStroke, EditRecipe, LinearGradient, LocalSettings, MASK_OPACITY_RANGE, MASK_RADIUS_RANGE,
    MASK_UNIT_RANGE, MAX_BRUSH_POINTS, MAX_MASK_COMPONENTS, MAX_MASK_LAYERS,
    MIN_LINEAR_GRADIENT_LENGTH, MIN_RADIAL_RADIUS, MaskComponent, MaskLayer, MaskMode, MaskShape,
    RadialGradient,
};

use super::MaskLimit;
use super::handles::linear_length;
use super::view::{Space, Vec2, distance};

/// A component inside a recipe: the layer's id and the component's index in that layer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct Placement {
    pub(super) layer: u32,
    pub(super) component: usize,
}

/// The gradient a creation drag draws.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum GradientKind {
    Linear,
    Radial,
}

/// `shape` as a new component: appended to layer `layer` (as `Add`) when that layer exists,
/// otherwise as the only component of a new layer.
pub(super) fn add_component(
    recipe: &EditRecipe,
    layer: Option<u32>,
    shape: MaskShape,
) -> Result<(EditRecipe, Placement), MaskLimit> {
    let component = MaskComponent {
        mode: MaskMode::Add,
        invert: false,
        shape,
    };
    let existing = layer.and_then(|id| recipe.local.layer(id));
    if let Some(existing) = existing {
        if existing.components.len() >= MAX_MASK_COMPONENTS {
            return Err(MaskLimit::Components);
        }
    } else if recipe.local.layers.len() >= MAX_MASK_LAYERS {
        return Err(MaskLimit::Layers);
    }

    let mut next = recipe.clone();
    if let Some(id) = existing.map(|layer| layer.id)
        && let Some(layer) = next.local.layer_mut(id)
    {
        layer.components.push(component);
        let placement = Placement {
            layer: id,
            component: layer.components.len() - 1,
        };
        return Ok((next, placement));
    }
    let id = next.local.next_layer_id();
    next.local.layers.push(MaskLayer {
        id,
        name: format!("Mask {}", id + 1),
        enabled: true,
        opacity: MASK_OPACITY_RANGE.neutral,
        components: vec![component],
        settings: LocalSettings::default(),
    });
    Ok((
        next,
        Placement {
            layer: id,
            component: 0,
        },
    ))
}

/// The gradient drawn by dragging from `origin` to `pointer`, or `None` while it would be
/// degenerate.
///
/// Linear: `origin` is the start (full effect) and `pointer` the end. Radial: a circle around
/// `origin` that reaches `pointer`; the handles reshape it afterwards.
pub(super) fn gradient_shape(
    kind: GradientKind,
    space: Space,
    origin: Vec2,
    pointer: Vec2,
) -> Option<MaskShape> {
    match kind {
        GradientKind::Linear => {
            let gradient = LinearGradient {
                start: space.space_to_point(origin),
                end: space.space_to_point(pointer),
            };
            (linear_length(&gradient) >= MIN_LINEAR_GRADIENT_LENGTH)
                .then_some(MaskShape::Linear(gradient))
        }
        GradientKind::Radial => {
            let radius = distance(origin, pointer).clamp(
                f64::from(MIN_RADIAL_RADIUS),
                f64::from(MASK_RADIUS_RANGE.maximum),
            ) as f32;
            Some(MaskShape::Radial(RadialGradient {
                center: space.space_to_point(origin),
                radius_x: radius,
                radius_y: radius,
                angle_degrees: 0.0,
                feather: MASK_UNIT_RANGE.neutral,
            }))
        }
    }
}

pub(super) fn shape_mut(recipe: &mut EditRecipe, at: Placement) -> Option<&mut MaskShape> {
    recipe
        .local
        .layer_mut(at.layer)?
        .components
        .get_mut(at.component)
        .map(|component| &mut component.shape)
}

/// Stroke `stroke` of the brush component at `at`.
pub(super) fn stroke_mut(
    recipe: &mut EditRecipe,
    at: Placement,
    stroke: usize,
) -> Option<&mut BrushStroke> {
    match shape_mut(recipe, at)? {
        MaskShape::Brush(brush) => brush.strokes.get_mut(stroke),
        _ => None,
    }
}

/// Brush points in the whole recipe, which `LocalAdjustments::validate` caps.
pub(super) fn brush_point_total(recipe: &EditRecipe) -> usize {
    recipe
        .local
        .layers
        .iter()
        .flat_map(|layer| &layer.components)
        .filter_map(|component| match &component.shape {
            MaskShape::Brush(brush) => Some(brush),
            _ => None,
        })
        .flat_map(|brush| &brush.strokes)
        .map(|stroke| stroke.points.len())
        .sum()
}

/// Brush points still available before the recipe limit.
pub(super) fn brush_point_budget(recipe: &EditRecipe) -> usize {
    MAX_BRUSH_POINTS.saturating_sub(brush_point_total(recipe))
}

#[cfg(test)]
mod tests {
    use rohditor_edit::{BrushMask, MaskPoint};

    use super::*;

    fn linear() -> MaskShape {
        MaskShape::Linear(LinearGradient {
            start: MaskPoint { x: 0.1, y: 0.1 },
            end: MaskPoint { x: 0.9, y: 0.9 },
        })
    }

    #[test]
    fn a_new_layer_is_valid_named_and_numbered_past_the_existing_ids() {
        let (first, at) = add_component(&EditRecipe::default(), None, linear()).expect("room");
        assert_eq!(
            at,
            Placement {
                layer: 0,
                component: 0
            }
        );
        assert_eq!(first.local.layers[0].name, "Mask 1");
        assert!(first.validate().is_ok());

        // Not selecting the first layer adds a second one beside it.
        let (second, at) = add_component(&first, None, linear()).expect("room");
        assert_eq!(at.layer, 1);
        assert_eq!(second.local.layers.len(), 2);
    }

    #[test]
    fn a_component_joins_the_selected_layer_as_an_add() {
        let (first, _) = add_component(&EditRecipe::default(), None, linear()).expect("room");
        let (second, at) = add_component(&first, Some(0), linear()).expect("room");
        assert_eq!(
            at,
            Placement {
                layer: 0,
                component: 1
            }
        );
        assert_eq!(second.local.layers.len(), 1);
        assert_eq!(second.local.layers[0].components[1].mode, MaskMode::Add);
        // An id that no longer exists starts a new layer instead.
        let (third, at) = add_component(&second, Some(7), linear()).expect("room");
        assert_eq!(
            (at.layer, third.local.layers.len(), at.component),
            (1, 2, 0)
        );
    }

    #[test]
    fn layer_and_component_limits_are_reported_not_exceeded() {
        let mut full = EditRecipe::default();
        for _ in 0..MAX_MASK_LAYERS {
            full = add_component(&full, None, linear()).expect("room").0;
        }
        assert_eq!(
            add_component(&full, None, linear()).err(),
            Some(MaskLimit::Layers)
        );
        // A full recipe still takes components while the layer has room.
        assert!(add_component(&full, Some(0), linear()).is_ok());

        let mut crowded = add_component(&EditRecipe::default(), None, linear())
            .expect("room")
            .0;
        for _ in 1..MAX_MASK_COMPONENTS {
            crowded = add_component(&crowded, Some(0), linear()).expect("room").0;
        }
        assert_eq!(
            add_component(&crowded, Some(0), linear()).err(),
            Some(MaskLimit::Components)
        );
        assert!(crowded.validate().is_ok());
    }

    #[test]
    fn gradient_shapes_are_valid_and_degenerate_drags_are_refused() {
        let space = Space::new((600, 400));
        let origin = [0.2, 0.1];
        assert!(gradient_shape(GradientKind::Linear, space, origin, origin).is_none());
        let radial = gradient_shape(GradientKind::Radial, space, origin, origin)
            .expect("a radial clamps to its smallest radius");
        let linear =
            gradient_shape(GradientKind::Linear, space, origin, [0.6, 0.3]).expect("a real drag");
        for shape in [radial, linear] {
            let mut recipe = EditRecipe::default();
            recipe = add_component(&recipe, None, shape).expect("room").0;
            assert!(recipe.validate().is_ok(), "{recipe:?}");
        }
    }

    #[test]
    fn brush_points_are_counted_over_the_whole_recipe() {
        let stroke = |points: usize| BrushStroke {
            points: vec![MaskPoint { x: 0.5, y: 0.5 }; points],
            radius: 0.05,
            feather: 0.5,
            flow: 1.0,
            erase: false,
        };
        let brush = |strokes| MaskShape::Brush(BrushMask { strokes });
        let (recipe, _) = add_component(
            &EditRecipe::default(),
            None,
            brush(vec![stroke(3), stroke(4)]),
        )
        .expect("room");
        let (recipe, _) = add_component(&recipe, None, brush(vec![stroke(5)])).expect("room");
        let (recipe, _) = add_component(&recipe, None, linear()).expect("room");
        assert_eq!(brush_point_total(&recipe), 12);
        assert_eq!(brush_point_budget(&recipe), MAX_BRUSH_POINTS - 12);
    }
}
