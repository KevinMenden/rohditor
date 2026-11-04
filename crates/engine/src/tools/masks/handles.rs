//! Handles of a gradient: where they are, which one a press grabs, and how dragging one
//! changes the shape.
//!
//! Everything runs in long-edge space (see `view`), where distances and angles are true, and
//! is pure over the shape. A drag is computed from the shape as it was when the handle was
//! grabbed plus the pointer movement since, so the result never accumulates rounding or
//! clamping error over many pointer events.

use std::f64::consts::FRAC_1_SQRT_2;

use rohditor_edit::{
    LinearGradient, MASK_RADIUS_RANGE, MIN_LINEAR_GRADIENT_LENGTH, MIN_RADIAL_RADIUS, MaskShape,
    RadialGradient,
};

use super::view::{Space, Vec2, add, distance, distance_to_segment, dot, scale, sub};

/// What a press on a gradient grabs. Brush components have no handles.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MaskHandle {
    /// Linear: the end with full effect. Moves only that end.
    LinearStart,
    /// Linear: the end where the effect is gone. Moves only that end.
    LinearEnd,
    /// Linear: the midpoint. Moves the whole gradient.
    LinearCenter,
    /// Linear: the gradient line away from its markers. Turns it around its midpoint.
    LinearRotate,
    /// Radial: the centre marker and the inside of the ellipse. Moves the whole ellipse.
    RadialCenter,
    /// Radial: the marker on the ellipse's own x axis. Changes only `radius_x`.
    RadialRadiusX,
    /// Radial: the marker on the ellipse's own y axis. Changes only `radius_y`.
    RadialRadiusY,
    /// Radial: the ellipse outline away from the markers. Turns the ellipse around its centre.
    RadialRotate,
    /// Radial: the marker on the inner ellipse, where the falloff starts.
    RadialFeather,
}

/// A radial gradient resolved to long-edge space.
struct Ellipse {
    center: Vec2,
    x_axis: Vec2,
    y_axis: Vec2,
    radius_x: f64,
    radius_y: f64,
}

impl Ellipse {
    fn new(radial: &RadialGradient, space: Space) -> Self {
        // Same convention as the mask crate: a positive angle turns the ellipse clockwise on
        // screen (y points down).
        let (sin, cos) = f64::from(radial.angle_degrees).to_radians().sin_cos();
        Self {
            center: space.point_to_space(radial.center),
            x_axis: [cos, sin],
            y_axis: [-sin, cos],
            radius_x: f64::from(radial.radius_x),
            radius_y: f64::from(radial.radius_y),
        }
    }

    /// `point` in the ellipse's own axes, in units of its radii: the outline is at length 1.
    fn local(&self, point: Vec2) -> Vec2 {
        let offset = sub(point, self.center);
        [
            dot(offset, self.x_axis) / self.radius_x,
            dot(offset, self.y_axis) / self.radius_y,
        ]
    }

    /// The feather marker sits on the diagonal at 45 degrees: away from the radius markers on
    /// the axes, and on the outline when the feather is 0. `feather` 1 puts it on the centre.
    fn feather_marker(&self, feather: f32) -> Vec2 {
        let diagonal = add(
            scale(self.x_axis, self.radius_x),
            scale(self.y_axis, self.radius_y),
        );
        add(
            self.center,
            scale(diagonal, FRAC_1_SQRT_2 * (1.0 - f64::from(feather))),
        )
    }

    /// First-order distance from `point` to the outline: exact for a circle, accurate within
    /// a grab radius for an ellipse.
    fn outline_distance(&self, point: Vec2) -> f64 {
        let local = self.local(point);
        let r = local[0].hypot(local[1]);
        if r == 0.0 {
            return self.radius_x.min(self.radius_y);
        }
        let offset = sub(point, self.center);
        let gradient = (dot(offset, self.x_axis) / (self.radius_x * self.radius_x))
            .hypot(dot(offset, self.y_axis) / (self.radius_y * self.radius_y))
            / r;
        (r - 1.0).abs() / gradient
    }
}

/// Length of a linear gradient as the recipe validation measures it, in normalized
/// coordinates.
pub(super) fn linear_length(gradient: &LinearGradient) -> f32 {
    (gradient.end.x - gradient.start.x).hypot(gradient.end.y - gradient.start.y)
}

/// The point handles of `shape` in long-edge space, for drawing and hit-testing.
///
/// The order is the tie-break for coincident markers: at feather 1 the feather marker sits on
/// the centre and has to stay reachable, so it comes first.
pub(super) fn markers(shape: &MaskShape, space: Space) -> Vec<(MaskHandle, Vec2)> {
    match shape {
        MaskShape::Linear(gradient) => {
            let start = space.point_to_space(gradient.start);
            let end = space.point_to_space(gradient.end);
            vec![
                (MaskHandle::LinearStart, start),
                (MaskHandle::LinearEnd, end),
                (MaskHandle::LinearCenter, midpoint(start, end)),
            ]
        }
        MaskShape::Radial(radial) => {
            let ellipse = Ellipse::new(radial, space);
            vec![
                (
                    MaskHandle::RadialFeather,
                    ellipse.feather_marker(radial.feather),
                ),
                (
                    MaskHandle::RadialRadiusX,
                    add(ellipse.center, scale(ellipse.x_axis, ellipse.radius_x)),
                ),
                (
                    MaskHandle::RadialRadiusY,
                    add(ellipse.center, scale(ellipse.y_axis, ellipse.radius_y)),
                ),
                (MaskHandle::RadialCenter, ellipse.center),
            ]
        }
        MaskShape::Brush(_) => Vec::new(),
    }
}

/// The handle a press at `point` grabs, if any; `grab_radius` is in long-edge units.
///
/// The nearest marker within the grab radius wins. Otherwise the line (linear) or outline
/// (radial) rotates, and the inside of an ellipse moves it.
pub(super) fn hit_test(
    shape: &MaskShape,
    space: Space,
    point: Vec2,
    grab_radius: f64,
) -> Option<MaskHandle> {
    let nearest_marker = markers(shape, space)
        .into_iter()
        .map(|(handle, position)| (handle, distance(position, point)))
        .filter(|(_, distance)| *distance <= grab_radius)
        // `min_by` keeps the first of equal distances, so `markers` order breaks ties.
        .min_by(|a, b| a.1.total_cmp(&b.1))
        .map(|(handle, _)| handle);
    if nearest_marker.is_some() {
        return nearest_marker;
    }
    match shape {
        MaskShape::Linear(gradient) => {
            let start = space.point_to_space(gradient.start);
            let end = space.point_to_space(gradient.end);
            (distance_to_segment(point, start, end) <= grab_radius)
                .then_some(MaskHandle::LinearRotate)
        }
        MaskShape::Radial(radial) => {
            let ellipse = Ellipse::new(radial, space);
            let local = ellipse.local(point);
            if ellipse.outline_distance(point) <= grab_radius {
                Some(MaskHandle::RadialRotate)
            } else if local[0].hypot(local[1]) < 1.0 {
                Some(MaskHandle::RadialCenter)
            } else {
                None
            }
        }
        MaskShape::Brush(_) => None,
    }
}

/// `original` with `handle` dragged from where the pointer grabbed it (`grab`) to `pointer`.
///
/// Continuous quantities (radii, feather) clamp at their limits. Anything that cannot be
/// clamped without changing the shape's meaning (a gradient collapsing to nothing, a rigid
/// move or turn leaving the range the recipe accepts) gives `None`, and the caller keeps the
/// previous value.
pub(super) fn drag(
    original: &MaskShape,
    space: Space,
    handle: MaskHandle,
    grab: Vec2,
    pointer: Vec2,
) -> Option<MaskShape> {
    match original {
        MaskShape::Linear(gradient) => {
            drag_linear(gradient, space, handle, grab, pointer).map(MaskShape::Linear)
        }
        MaskShape::Radial(radial) => {
            drag_radial(radial, space, handle, grab, pointer).map(MaskShape::Radial)
        }
        MaskShape::Brush(_) => None,
    }
}

fn drag_linear(
    gradient: &LinearGradient,
    space: Space,
    handle: MaskHandle,
    grab: Vec2,
    pointer: Vec2,
) -> Option<LinearGradient> {
    let start = space.point_to_space(gradient.start);
    let end = space.point_to_space(gradient.end);
    let delta = sub(pointer, grab);
    let (start, end) = match handle {
        MaskHandle::LinearStart => (add(start, delta), end),
        MaskHandle::LinearEnd => (start, add(end, delta)),
        MaskHandle::LinearCenter => (add(start, delta), add(end, delta)),
        MaskHandle::LinearRotate => {
            let center = midpoint(start, end);
            let turn = angle_of(sub(pointer, center)) - angle_of(sub(grab, center));
            (
                rotate_about(start, center, turn),
                rotate_about(end, center, turn),
            )
        }
        _ => return None,
    };
    // Moving or turning both ends is rigid; clamping one end would bend the gradient.
    let rigid = matches!(handle, MaskHandle::LinearCenter | MaskHandle::LinearRotate);
    if rigid && !(space.in_range(start) && space.in_range(end)) {
        return None;
    }
    let next = LinearGradient {
        start: space.space_to_point(start),
        end: space.space_to_point(end),
    };
    (linear_length(&next) >= MIN_LINEAR_GRADIENT_LENGTH).then_some(next)
}

fn drag_radial(
    radial: &RadialGradient,
    space: Space,
    handle: MaskHandle,
    grab: Vec2,
    pointer: Vec2,
) -> Option<RadialGradient> {
    let ellipse = Ellipse::new(radial, space);
    let delta = sub(pointer, grab);
    let mut next = *radial;
    match handle {
        MaskHandle::RadialCenter => next.center = space.space_to_point(add(ellipse.center, delta)),
        // A radius handle slides along its own axis; movement across the axis is ignored.
        MaskHandle::RadialRadiusX => {
            next.radius_x = clamp_radius(ellipse.radius_x + dot(delta, ellipse.x_axis));
        }
        MaskHandle::RadialRadiusY => {
            next.radius_y = clamp_radius(ellipse.radius_y + dot(delta, ellipse.y_axis));
        }
        MaskHandle::RadialRotate => {
            let turn = angle_of(sub(pointer, ellipse.center)) - angle_of(sub(grab, ellipse.center));
            next.angle_degrees = wrap_degrees(f64::from(radial.angle_degrees) + turn.to_degrees());
        }
        MaskHandle::RadialFeather => {
            // Where the marker would be after the move, as a fraction of the way out to the
            // outline. The marker snaps back to its diagonal on the next draw.
            let target = add(ellipse.feather_marker(radial.feather), delta);
            let local = ellipse.local(target);
            next.feather = (1.0 - local[0].hypot(local[1])).clamp(0.0, 1.0) as f32;
        }
        _ => return None,
    }
    Some(next)
}

fn midpoint(a: Vec2, b: Vec2) -> Vec2 {
    [(a[0] + b[0]) * 0.5, (a[1] + b[1]) * 0.5]
}

/// Direction of `vector` in radians; increases clockwise on screen (y points down).
fn angle_of(vector: Vec2) -> f64 {
    vector[1].atan2(vector[0])
}

fn rotate_about(point: Vec2, center: Vec2, radians: f64) -> Vec2 {
    let (sin, cos) = radians.sin_cos();
    let offset = sub(point, center);
    [
        center[0] + offset[0] * cos - offset[1] * sin,
        center[1] + offset[0] * sin + offset[1] * cos,
    ]
}

fn clamp_radius(radius: f64) -> f32 {
    radius.clamp(
        f64::from(MIN_RADIAL_RADIUS),
        f64::from(MASK_RADIUS_RANGE.maximum),
    ) as f32
}

/// `degrees` wrapped to `-180..=180`, well inside the recipe's accepted range.
fn wrap_degrees(degrees: f64) -> f32 {
    let wrapped = degrees.rem_euclid(360.0);
    (if wrapped > 180.0 {
        wrapped - 360.0
    } else {
        wrapped
    }) as f32
}

#[cfg(test)]
mod tests {
    use rohditor_edit::MaskPoint;

    use super::*;

    /// 3:2 frame, so a normalized unit differs between x (1.0) and y (2/3) in long-edge units.
    fn space() -> Space {
        Space::new((600, 400))
    }

    /// Radii and points are `f32` in the recipe, so a value read back as `f64` is off by ~3e-9.
    const F32_TOLERANCE: f64 = 1e-7;

    fn point(x: f32, y: f32) -> MaskPoint {
        MaskPoint { x, y }
    }

    fn circle(radius: f32) -> RadialGradient {
        RadialGradient {
            center: point(0.5, 0.5),
            radius_x: radius,
            radius_y: radius,
            angle_degrees: 0.0,
            feather: 0.5,
        }
    }

    #[test]
    fn the_outline_distance_of_a_circle_is_exact() {
        let ellipse = Ellipse::new(&circle(0.2), space());
        // The centre is (0.5, 1/3); 0.15 and 0.3 long-edge units to its right.
        let near = add(ellipse.center, [0.15, 0.0]);
        let far = add(ellipse.center, [0.3, 0.0]);
        assert!((ellipse.outline_distance(near) - 0.05).abs() < F32_TOLERANCE);
        assert!((ellipse.outline_distance(far) - 0.1).abs() < F32_TOLERANCE);
        assert!((ellipse.outline_distance(ellipse.center) - 0.2).abs() < F32_TOLERANCE);
    }

    #[test]
    fn the_feather_marker_runs_from_the_outline_to_the_centre() {
        let ellipse = Ellipse::new(&circle(0.2), space());
        let hard = ellipse.feather_marker(0.0);
        assert!((distance(hard, ellipse.center) - 0.2).abs() < F32_TOLERANCE);
        assert_eq!(ellipse.feather_marker(1.0), ellipse.center);
    }

    #[test]
    fn a_marker_wins_over_the_line_and_the_nearest_marker_wins_among_markers() {
        let shape = MaskShape::Linear(LinearGradient {
            start: point(0.2, 0.5),
            end: point(0.8, 0.5),
        });
        let near_start = [0.21, 2.0 / 6.0];
        assert_eq!(
            hit_test(&shape, space(), near_start, 0.05),
            Some(MaskHandle::LinearStart)
        );
        // Between the markers the line itself rotates.
        let on_line = [0.35, 2.0 / 6.0 + 0.01];
        assert_eq!(
            hit_test(&shape, space(), on_line, 0.05),
            Some(MaskHandle::LinearRotate)
        );
        assert_eq!(hit_test(&shape, space(), [0.35, 0.0], 0.05), None);
    }

    #[test]
    fn a_full_feather_marker_on_the_centre_stays_grabbable() {
        let shape = MaskShape::Radial(RadialGradient {
            feather: 1.0,
            ..circle(0.2)
        });
        let centre = space().point_to_space(point(0.5, 0.5));
        assert_eq!(
            hit_test(&shape, space(), centre, 0.02),
            Some(MaskHandle::RadialFeather)
        );
        // The inside of the ellipse still moves it.
        let inside = add(centre, [0.1, 0.0]);
        assert_eq!(
            hit_test(&shape, space(), inside, 0.02),
            Some(MaskHandle::RadialCenter)
        );
    }

    #[test]
    fn dragging_a_radius_past_the_centre_clamps_instead_of_flipping() {
        let radial = circle(0.2);
        let ellipse = Ellipse::new(&radial, space());
        let grab = add(ellipse.center, [0.2, 0.0]);
        let shape = drag_radial(
            &radial,
            space(),
            MaskHandle::RadialRadiusX,
            grab,
            add(ellipse.center, [-0.5, 0.0]),
        )
        .expect("radius handles always produce a shape");
        assert_eq!(shape.radius_x, MIN_RADIAL_RADIUS);
        assert_eq!(shape.radius_y, radial.radius_y);
        assert_eq!(shape.center, radial.center);
    }

    #[test]
    fn wrapped_angles_stay_inside_the_recipe_range() {
        assert_eq!(wrap_degrees(190.0), -170.0);
        assert_eq!(wrap_degrees(-190.0), 170.0);
        assert_eq!(wrap_degrees(720.0 + 30.0), 30.0);
        assert_eq!(wrap_degrees(180.0), 180.0);
    }
}
