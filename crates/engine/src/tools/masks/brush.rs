//! Brush authoring: the settings copied into each stroke, the painting modes of the session,
//! and stroke simplification.

use rohditor_edit::{
    BrushStroke, EditRecipe, MASK_RADIUS_RANGE, MASK_UNIT_RANGE, MaskPoint, MaskShape,
    ParameterRange,
};

use super::view::{Vec2, distance_to_segment};
use super::{MaskEditMode, MaskToolSession};

/// Brush sizes the tool offers, as radii in long-edge units. The recipe accepts far more (up to
/// `MASK_RADIUS_RANGE`), but a brush beyond this is a gradient in practice.
pub const BRUSH_RADIUS_RANGE: ParameterRange = ParameterRange {
    minimum: 0.002,
    maximum: 0.3,
    neutral: 0.05,
};

/// One size step (a `[` or `]` press) scales the radius by this factor.
const RADIUS_STEP_FACTOR: f32 = 1.15;

/// One feather step (a `Shift+[` or `Shift+]` press).
const FEATHER_STEP: f32 = 0.05;

/// While recording, a point is added once the pointer is this far (as a fraction of the brush
/// radius) from the last one. The mask crate places its dabs a quarter radius apart, so
/// finer sampling would not change the painted result.
pub(super) const POINT_SPACING_PER_RADIUS: f64 = 0.25;

/// A finished stroke is simplified within this distance (as a fraction of the brush radius),
/// so sidecars stay small without visibly changing the stroke.
pub(super) const SIMPLIFY_TOLERANCE_PER_RADIUS: f64 = 0.1;

/// Smallest brush radius: the recipe needs a positive one.
const MIN_BRUSH_RADIUS: f32 = 1e-3;

/// Authoring state of the brush. Each stroke copies it when it starts, so changing it later
/// never alters strokes already painted.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BrushSettings {
    /// Long-edge units.
    pub radius: f32,
    /// `0..=1`.
    pub feather: f32,
    /// `0..=1`.
    pub flow: f32,
}

impl Default for BrushSettings {
    fn default() -> Self {
        Self {
            radius: BRUSH_RADIUS_RANGE.neutral,
            feather: MASK_UNIT_RANGE.neutral,
            flow: 1.0,
        }
    }
}

impl BrushSettings {
    /// Grows (`steps > 0`) or shrinks the brush by whole size steps, within the offered sizes.
    /// Steps are relative, so they feel the same at any size.
    pub fn step_radius(&mut self, steps: i32) {
        self.radius = (self.radius * RADIUS_STEP_FACTOR.powi(steps))
            .clamp(BRUSH_RADIUS_RANGE.minimum, BRUSH_RADIUS_RANGE.maximum);
    }

    /// Softens (`steps > 0`) or hardens the brush edge by whole feather steps.
    pub fn step_feather(&mut self, steps: i32) {
        self.feather = (self.feather + FEATHER_STEP * steps as f32).clamp(0.0, 1.0);
    }

    /// A new stroke with its first point, the settings clamped to what the recipe accepts.
    pub(super) fn begin_stroke(self, erase: bool, first: MaskPoint) -> BrushStroke {
        BrushStroke {
            points: vec![first],
            radius: self
                .radius
                .clamp(MIN_BRUSH_RADIUS, MASK_RADIUS_RANGE.maximum),
            feather: self.feather.clamp(0.0, 1.0),
            flow: self.flow.clamp(0.0, 1.0),
            erase,
        }
    }
}

impl MaskToolSession {
    pub const fn is_painting(&self) -> bool {
        matches!(self.mode, MaskEditMode::Paint { .. })
    }

    /// Whether a stroke that started now would erase: the painting mode, inverted while the
    /// erase modifier is held.
    pub const fn is_erasing(&self) -> bool {
        matches!(self.mode, MaskEditMode::Paint { erase } if erase != self.erase_held)
    }

    /// Switches between painting and erasing. Does nothing outside painting.
    pub fn toggle_erase(&mut self) {
        if let MaskEditMode::Paint { erase } = &mut self.mode {
            *erase = !*erase;
        }
    }

    /// Paints more strokes into the selected component. False, and nothing changes, when the
    /// selected component is not a brush.
    pub fn paint_selected(&mut self, recipe: &EditRecipe) -> bool {
        let is_brush = matches!(self.selected_shape(recipe), Some((_, MaskShape::Brush(_))));
        if is_brush {
            self.mode = MaskEditMode::Paint { erase: false };
            self.new_brush_component = false;
        }
        is_brush
    }
}

/// Indices of the points to keep (Ramer–Douglas–Peucker): the ends and every point that
/// sticks out more than `tolerance` from the chord of the points kept around it.
pub(super) fn simplify(points: &[Vec2], tolerance: f64) -> Vec<usize> {
    let count = points.len();
    if count < 3 {
        return (0..count).collect();
    }
    let mut keep = vec![false; count];
    keep[0] = true;
    keep[count - 1] = true;
    // An explicit stack: a stroke can have up to 200,000 points, too deep to recurse.
    let mut pending = vec![(0, count - 1)];
    while let Some((first, last)) = pending.pop() {
        let mut farthest = first;
        let mut farthest_distance = 0.0;
        for index in first + 1..last {
            let distance = distance_to_segment(points[index], points[first], points[last]);
            if distance > farthest_distance {
                farthest = index;
                farthest_distance = distance;
            }
        }
        if farthest_distance > tolerance {
            keep[farthest] = true;
            pending.push((first, farthest));
            pending.push((farthest, last));
        }
    }
    (0..count).filter(|index| keep[*index]).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_straight_run_collapses_to_its_ends() {
        let points: Vec<Vec2> = (0..50).map(|i| [f64::from(i) * 0.01, 0.2]).collect();
        assert_eq!(simplify(&points, 0.005), vec![0, 49]);
    }

    #[test]
    fn corners_beyond_the_tolerance_survive() {
        // An L: right along y = 0, then down along x = 0.5.
        let mut points: Vec<Vec2> = (0..=10).map(|i| [f64::from(i) * 0.05, 0.0]).collect();
        points.extend((1..=10).map(|i| [0.5, f64::from(i) * 0.05]));
        assert_eq!(simplify(&points, 0.01), vec![0, 10, 20]);
    }

    #[test]
    fn wobble_inside_the_tolerance_is_dropped() {
        let points: Vec<Vec2> = (0..20)
            .map(|i| [f64::from(i) * 0.02, if i % 2 == 0 { 0.0 } else { 0.004 }])
            .collect();
        assert_eq!(simplify(&points, 0.01), vec![0, 19]);
    }

    #[test]
    fn short_strokes_keep_every_point() {
        assert_eq!(simplify(&[], 0.1), Vec::<usize>::new());
        assert_eq!(simplify(&[[0.1, 0.1]], 0.1), vec![0]);
        assert_eq!(simplify(&[[0.1, 0.1], [0.1, 0.1]], 0.1), vec![0, 1]);
    }

    #[test]
    fn a_closed_loop_keeps_its_shape() {
        // First and last point coincide, so the chord is degenerate.
        let points: Vec<Vec2> = (0..=16)
            .map(|i| {
                let angle = f64::from(i) / 16.0 * std::f64::consts::TAU;
                [0.5 + 0.2 * angle.cos(), 0.5 + 0.2 * angle.sin()]
            })
            .collect();
        let kept = simplify(&points, 0.01);
        assert!(kept.len() > 8, "a loop must not collapse: {kept:?}");
        assert_eq!((kept[0], *kept.last().expect("non-empty")), (0, 16));
    }

    #[test]
    fn size_steps_are_relative_and_stay_within_the_offered_sizes() {
        let mut brush = BrushSettings::default();
        brush.step_radius(1);
        assert!((brush.radius - 0.05 * RADIUS_STEP_FACTOR).abs() < 1e-6);
        brush.step_radius(-1);
        assert!((brush.radius - 0.05).abs() < 1e-6);
        brush.step_radius(1000);
        assert_eq!(brush.radius, BRUSH_RADIUS_RANGE.maximum);
        brush.step_radius(-1000);
        assert_eq!(brush.radius, BRUSH_RADIUS_RANGE.minimum);
    }

    #[test]
    fn feather_steps_stay_within_zero_and_one() {
        let mut brush = BrushSettings::default();
        brush.step_feather(2);
        assert!((brush.feather - 0.6).abs() < 1e-6);
        brush.step_feather(100);
        assert_eq!(brush.feather, 1.0);
        brush.step_feather(-100);
        assert_eq!(brush.feather, 0.0);
    }

    #[test]
    fn settings_are_clamped_into_the_recipe_range_when_a_stroke_starts() {
        let stroke = BrushSettings {
            radius: -1.0,
            feather: 3.0,
            flow: -0.5,
        }
        .begin_stroke(true, MaskPoint { x: 0.1, y: 0.2 });
        assert_eq!(stroke.radius, MIN_BRUSH_RADIUS);
        assert_eq!((stroke.feather, stroke.flow), (1.0, 0.0));
        assert!(stroke.erase);
        assert_eq!(stroke.points, vec![MaskPoint { x: 0.1, y: 0.2 }]);
    }
}
