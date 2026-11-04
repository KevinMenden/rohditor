//! Brush authoring: the settings copied into each stroke, and stroke simplification.

use rohditor_edit::{BrushStroke, MASK_RADIUS_RANGE, MASK_UNIT_RANGE, MaskPoint};

use super::view::{Vec2, distance_to_segment};

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
            radius: 0.05,
            feather: MASK_UNIT_RANGE.neutral,
            flow: 1.0,
        }
    }
}

impl BrushSettings {
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
