//! Parts of the frame: where a brush changed, and which strokes reach there. While painting, only
//! a small part changes from one preview to the next, however much was painted before.

use std::ops::Range;

use rohditor_edit::{BrushMask, BrushStroke, MaskPoint};

use crate::MaskFrame;
use crate::math::to_pixels;

/// Pixels added around a stroke, so that rounding never cuts its edge off.
const MARGIN_PX: f32 = 1.0;

/// A part of a frame that is not empty, in pixels.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MaskArea {
    pub columns: Range<usize>,
    pub rows: Range<usize>,
}

impl MaskArea {
    #[must_use]
    pub const fn whole(frame: MaskFrame) -> Self {
        Self {
            columns: 0..frame.width,
            rows: 0..frame.height,
        }
    }

    /// Where the coverage of the two brushes can differ; `None` if nowhere in the frame.
    #[must_use]
    pub fn changed(before: &BrushMask, after: &BrushMask, frame: MaskFrame) -> Option<Self> {
        let mut reach = Reach::default();
        for index in 0..before.strokes.len().max(after.strokes.len()) {
            match (before.strokes.get(index), after.strokes.get(index)) {
                (Some(before), Some(after)) => {
                    let from = first_difference(before, after);
                    reach.add(before, from, frame);
                    reach.add(after, from, frame);
                }
                (stroke, None) | (None, stroke) => {
                    if let Some(stroke) = stroke {
                        reach.add(stroke, 0, frame);
                    }
                }
            }
        }
        reach.area(frame)
    }

    /// Whether the stroke can cover any of it.
    pub(crate) fn is_reached_by(&self, stroke: &BrushStroke, frame: MaskFrame) -> bool {
        let mut reach = Reach::default();
        reach.add(stroke, 0, frame);
        reach.area(frame).is_some_and(|area| {
            let overlap = |a: &Range<usize>, b: &Range<usize>| a.start < b.end && b.start < a.end;
            overlap(&area.columns, &self.columns) && overlap(&area.rows, &self.rows)
        })
    }
}

/// The point from which on two strokes differ. A stroke places its dabs along its points in
/// order, so the dabs up to there are the same.
fn first_difference(before: &BrushStroke, after: &BrushStroke) -> usize {
    let alike = before.radius == after.radius
        && before.feather == after.feather
        && before.flow == after.flow
        && before.erase == after.erase;
    if !alike {
        return 0;
    }
    if before.points == after.points {
        return before.points.len();
    }
    let points = std::iter::zip(&before.points, &after.points);
    let common = points.take_while(|(a, b)| a == b).count();
    // With the point before it, as they differ in the line from there.
    common.saturating_sub(1)
}

/// The pixels within the radius of some strokes.
struct Reach {
    low: [f32; 2],
    high: [f32; 2],
}

impl Default for Reach {
    fn default() -> Self {
        Self {
            low: [f32::INFINITY; 2],
            high: [f32::NEG_INFINITY; 2],
        }
    }
}

impl Reach {
    /// The stroke from its point `from` on.
    fn add(&mut self, stroke: &BrushStroke, from: usize, frame: MaskFrame) {
        let radius = stroke.radius * frame.long_edge() + MARGIN_PX;
        let points: &[MaskPoint] = stroke.points.get(from..).unwrap_or_default();
        for point in points {
            let center = to_pixels(*point, frame.width, frame.height);
            for (low, center) in self.low.iter_mut().zip(center) {
                *low = low.min(center - radius);
            }
            for (high, center) in self.high.iter_mut().zip(center) {
                *high = high.max(center + radius);
            }
        }
    }

    fn area(&self, frame: MaskFrame) -> Option<MaskArea> {
        let clipped = |axis: usize, size: usize| {
            let low = self.low[axis].floor().max(0.0);
            let high = self.high[axis].ceil().min(size as f32);
            (low < high).then_some(low as usize..high as usize)
        };
        Some(MaskArea {
            columns: clipped(0, frame.width)?,
            rows: clipped(1, frame.height)?,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::BrushCoverage;

    const FRAME: MaskFrame = MaskFrame {
        width: 120,
        height: 80,
    };

    fn stroke(points: &[(f32, f32)], radius: f32) -> BrushStroke {
        BrushStroke {
            points: points.iter().map(|&(x, y)| MaskPoint { x, y }).collect(),
            radius,
            feather: 0.5,
            flow: 0.6,
            erase: false,
        }
    }

    fn brush(strokes: &[BrushStroke]) -> BrushMask {
        BrushMask {
            strokes: strokes.to_vec(),
        }
    }

    fn evaluate(brush: &BrushMask, area: &MaskArea) -> Vec<Vec<f32>> {
        let coverage = BrushCoverage::prepare_area(brush, FRAME, area).unwrap();
        let mut keep = Vec::new();
        area.rows
            .clone()
            .map(|y| {
                let mut row = vec![0.0; area.columns.len()];
                coverage.evaluate_row(y, area.columns.start, &mut row, &mut keep);
                row
            })
            .collect()
    }

    /// Updating only the changed area gives what evaluating everything does.
    #[track_caller]
    fn assert_update(before: &BrushMask, after: &BrushMask) -> Option<MaskArea> {
        let whole = MaskArea::whole(FRAME);
        let mut updated = evaluate(before, &whole);
        let area = MaskArea::changed(before, after, FRAME);
        if let Some(area) = &area {
            for (y, row) in area.rows.clone().zip(evaluate(after, area)) {
                updated[y][area.columns.clone()].copy_from_slice(&row);
            }
        }
        assert_eq!(updated, evaluate(after, &whole));
        area
    }

    #[test]
    fn a_growing_stroke_changes_only_its_end() {
        let old = stroke(&[(0.1, 0.8), (0.9, 0.2)], 0.1);
        let short = stroke(&[(0.1, 0.2), (0.4, 0.3), (0.6, 0.3)], 0.05);
        let long = stroke(&[(0.1, 0.2), (0.4, 0.3), (0.6, 0.3), (0.7, 0.5)], 0.05);
        let area = assert_update(&brush(&[old.clone(), short]), &brush(&[old.clone(), long]));
        // From the point (72, 24) to the new one at (84, 40), and a radius of 6.
        let expected = MaskArea {
            columns: 65..91,
            rows: 17..47,
        };
        assert_eq!(area, Some(expected));

        // A last point that moved.
        let moved = stroke(&[(0.1, 0.2), (0.4, 0.3), (0.5, 0.45)], 0.05);
        let short = stroke(&[(0.1, 0.2), (0.4, 0.3), (0.6, 0.3)], 0.05);
        assert_update(&brush(&[old.clone(), short]), &brush(&[old, moved]));
    }

    #[test]
    fn new_removed_erasing_and_changed_strokes_are_updated() {
        let one = stroke(&[(0.2, 0.3), (0.8, 0.6)], 0.1);
        let two = stroke(&[(0.5, 0.2), (0.5, 0.9)], 0.07);
        let mut erasing = two.clone();
        erasing.erase = true;
        let mut harder = two.clone();
        harder.feather = 0.1;
        let before = brush(&[one.clone(), two.clone()]);
        for after in [
            brush(std::slice::from_ref(&one)),
            brush(std::slice::from_ref(&two)),
            brush(&[one.clone(), two.clone(), erasing.clone()]),
            brush(&[one.clone(), erasing]),
            brush(&[one.clone(), harder]),
            BrushMask::default(),
        ] {
            assert!(assert_update(&before, &after).is_some());
            assert!(assert_update(&after, &before).is_some());
        }
        assert_eq!(MaskArea::changed(&before, &before, FRAME), None);
    }

    #[test]
    fn strokes_outside_of_the_frame_change_nothing() {
        let outside = stroke(&[(1.5, 0.5), (1.6, 0.5)], 0.1);
        let area = MaskArea::changed(&BrushMask::default(), &brush(&[outside]), FRAME);
        assert_eq!(area, None);
        let reaching = stroke(&[(1.0, 0.5)], 0.125);
        let area = MaskArea::changed(&BrushMask::default(), &brush(&[reaching]), FRAME);
        assert_eq!(area.map(|area| area.columns), Some(104..120));
    }
}
