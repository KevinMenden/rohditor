//! Brush strokes: dabs spaced along a polyline, binned into row bands.

use rohditor_edit::{BrushMask, BrushStroke};

use crate::math::to_pixels;
use crate::{MaskError, MaskFrame};

/// Rows per band. `evaluate_row` only visits the dabs that touch the row's band.
const BAND_ROWS: usize = 64;
/// Dabs are spaced `radius / DAB_SPACING_DIVISOR` apart along the stroke.
const DAB_SPACING_DIVISOR: f32 = 4.0;
/// Closest dab spacing in pixels. Below this a dab covers less than a pixel anyway, and the
/// floor keeps tiny radii from generating millions of dabs.
const MIN_SPACING_PX: f32 = 0.25;
/// Narrowest dab falloff in pixels; a feather of 0 uses this instead of a true step.
const MIN_FALLOFF_PX: f32 = 1e-3;
/// Most (dab, band) pairs held by one prepared brush (8 bytes each). Rejects strokes that
/// would need an unreasonable amount of memory instead of allocating it.
pub(crate) const MAX_DAB_ENTRIES: usize = 4_000_000;

/// All strokes of one brush component, resolved to pixel space.
///
/// # Maths
///
/// * A stroke places **dabs** `radius / 4` apart along its polyline (first point included).
///   A dab has alpha `a = 1` within `radius * (1 - feather)` of its centre, falling to 0 at
///   `radius` with a smoothstep.
/// * Within one stroke coverage builds up per dab as `v = v + a * flow * (1 - v)`. That is
///   `v = 1 - prod(1 - a * flow)`, which does not depend on dab order, so dabs can be binned
///   and visited in any order.
/// * Strokes are applied in order: a normal stroke gives `max(acc, v)`, an erase stroke
///   `acc * (1 - v)`. An erase stroke therefore only removes what earlier strokes painted.
#[derive(Debug, Clone)]
pub(crate) struct PreparedBrush {
    strokes: Vec<PreparedStroke>,
}

#[derive(Debug, Clone)]
struct PreparedStroke {
    erase: bool,
    flow: f32,
    radius: f32,
    /// Distance from the dab centre up to which the dab is fully opaque.
    inner_radius: f32,
    /// Reciprocal of the falloff width (`radius - inner_radius`), floored so a feather of 0 is
    /// an (almost) hard edge instead of a division by zero.
    inv_falloff: f32,
    /// `band_offsets[b]..band_offsets[b + 1]` indexes `dabs` for row band `b`. A dab that
    /// spans several bands is stored once per band.
    band_offsets: Vec<u32>,
    dabs: Vec<[f32; 2]>,
}

impl PreparedBrush {
    pub(crate) fn prepare(brush: &BrushMask, frame: MaskFrame) -> Result<Self, MaskError> {
        let mut entries = 0usize;
        let strokes = brush
            .strokes
            .iter()
            .map(|stroke| {
                let prepared = PreparedStroke::prepare(stroke, frame, MAX_DAB_ENTRIES - entries)?;
                entries += prepared.dabs.len();
                Ok(prepared)
            })
            .collect::<Result<_, MaskError>>()?;
        Ok(Self { strokes })
    }

    /// Writes the brush alpha for pixels `x_range` of row `y` into `out`. `keep` is reusable
    /// scratch for the per-stroke "uncovered" product.
    pub(crate) fn evaluate_row(
        &self,
        y: usize,
        x_start: usize,
        out: &mut [f32],
        keep: &mut Vec<f32>,
    ) {
        out.fill(0.0);
        let band = y / BAND_ROWS;
        let row_y = y as f32 + 0.5;
        // `keep` is all 1 between strokes: each stroke resets the part it touched.
        keep.clear();
        keep.resize(out.len(), 1.0);
        for stroke in &self.strokes {
            // Columns (relative to `x_start`) any dab of this stroke reached on this row.
            let mut touched: Option<(usize, usize)> = None;
            for dab in stroke.band(band) {
                if let Some((first, last)) = stroke.splat(*dab, row_y, x_start, keep) {
                    touched = Some(match touched {
                        Some((lo, hi)) => (lo.min(first), hi.max(last)),
                        None => (first, last),
                    });
                }
            }
            let Some((first, last)) = touched else {
                continue;
            };
            // `keep` is the product of (1 - a * flow) over the stroke's dabs: the share of the
            // pixel the stroke leaves uncovered.
            let alphas = out[first..=last].iter_mut();
            let keeps = keep[first..=last].iter_mut();
            if stroke.erase {
                for (alpha, keep) in alphas.zip(keeps) {
                    *alpha *= *keep;
                    *keep = 1.0;
                }
            } else {
                for (alpha, keep) in alphas.zip(keeps) {
                    *alpha = alpha.max(1.0 - *keep);
                    *keep = 1.0;
                }
            }
        }
    }

    pub(crate) fn bytes(&self) -> usize {
        std::mem::size_of_val(self.strokes.as_slice())
            + self
                .strokes
                .iter()
                .map(|s| s.band_offsets.len() * 4 + s.dabs.len() * std::mem::size_of::<[f32; 2]>())
                .sum::<usize>()
    }
}

impl PreparedStroke {
    fn prepare(
        stroke: &BrushStroke,
        frame: MaskFrame,
        entry_budget: usize,
    ) -> Result<Self, MaskError> {
        let degenerate = |reason| MaskError::Degenerate {
            shape: "brush",
            reason,
        };
        let radius = stroke.radius * frame.long_edge();
        if !(radius.is_finite() && radius > 0.0) {
            return Err(degenerate("stroke radius must be positive and finite"));
        }
        if !(stroke.feather.is_finite() && stroke.flow.is_finite()) {
            return Err(degenerate("stroke feather and flow must be finite"));
        }
        if stroke
            .points
            .iter()
            .any(|p| !(p.x.is_finite() && p.y.is_finite()))
        {
            return Err(degenerate("stroke points must be finite"));
        }
        let points: Vec<[f32; 2]> = stroke
            .points
            .iter()
            .map(|p| to_pixels(*p, frame.width, frame.height))
            .collect();
        let spacing = (radius / DAB_SPACING_DIVISOR).max(MIN_SPACING_PX);
        let dabs = place_dabs(&points, spacing, entry_budget)?;
        let (band_offsets, dabs) = bin_into_bands(&dabs, radius, frame, entry_budget)?;
        let inner_radius = radius * (1.0 - stroke.feather.clamp(0.0, 1.0));
        Ok(Self {
            erase: stroke.erase,
            flow: stroke.flow.clamp(0.0, 1.0),
            radius,
            inner_radius,
            inv_falloff: 1.0 / (radius - inner_radius).max(MIN_FALLOFF_PX),
            band_offsets,
            dabs,
        })
    }

    fn band(&self, band: usize) -> &[[f32; 2]] {
        match self.band_offsets.get(band..band + 2) {
            Some(range) => &self.dabs[range[0] as usize..range[1] as usize],
            None => &[],
        }
    }

    /// Multiplies `keep` by `1 - a * flow` for the part of `dab` that lies on the row at
    /// `row_y` (pixel-centre y). `keep[i]` is pixel `x_start + i`. Returns the first and last
    /// `keep` index it may have changed, or `None` if the dab misses the row.
    fn splat(
        &self,
        dab: [f32; 2],
        row_y: f32,
        x_start: usize,
        keep: &mut [f32],
    ) -> Option<(usize, usize)> {
        let dy = row_y - dab[1];
        let dy_squared = dy * dy;
        let radius_squared = self.radius * self.radius;
        if dy_squared >= radius_squared {
            return None;
        }
        // Half-width of the dab's disc on this row; only pixels inside it can be covered.
        let half_width = (radius_squared - dy_squared).sqrt();
        let first = (dab[0] - half_width - 0.5).floor().max(x_start as f32);
        let last = (dab[0] + half_width - 0.5)
            .ceil()
            .min((x_start + keep.len()) as f32 - 1.0);
        if first > last {
            return None;
        }
        let (first, last) = (first as usize, last as usize);
        // Iterating a sub-slice (instead of indexing) lets the compiler vectorize the loop.
        let row = &mut keep[first - x_start..=last - x_start];
        let first_dx = first as f32 + 0.5 - dab[0];
        for (i, keep) in row.iter_mut().enumerate() {
            let dx = first_dx + i as f32;
            let distance = (dx * dx + dy_squared).sqrt();
            // Branch-free smoothstep(inner, radius, distance).
            let t = ((distance - self.inner_radius) * self.inv_falloff).clamp(0.0, 1.0);
            let alpha = 1.0 - t * t * (3.0 - 2.0 * t);
            *keep *= 1.0 - alpha * self.flow;
        }
        Some((first - x_start, last - x_start))
    }
}

/// Dab centres every `spacing` pixels of arc length along the polyline, starting at the first
/// point. A single-point stroke is one dab. `budget` caps the number of dabs.
fn place_dabs(
    points: &[[f32; 2]],
    spacing: f32,
    budget: usize,
) -> Result<Vec<[f32; 2]>, MaskError> {
    let too_dense = |entries| MaskError::BrushTooDense {
        entries,
        limit: MAX_DAB_ENTRIES,
    };
    let Some(first) = points.first() else {
        return Ok(Vec::new());
    };
    let mut dabs = vec![*first];
    // Distance travelled since the last dab, carried across segments so spacing stays even
    // around corners.
    let mut since_last = 0.0f32;
    for pair in points.windows(2) {
        let (from, to) = (pair[0], pair[1]);
        let length = (to[0] - from[0]).hypot(to[1] - from[1]);
        if length <= 0.0 {
            continue;
        }
        let mut position = spacing - since_last;
        while position <= length {
            if dabs.len() >= budget {
                return Err(too_dense(dabs.len() + 1));
            }
            let t = position / length;
            dabs.push([
                from[0] + (to[0] - from[0]) * t,
                from[1] + (to[1] - from[1]) * t,
            ]);
            position += spacing;
        }
        since_last = length - (position - spacing);
    }
    Ok(dabs)
}

/// Groups dabs by the row bands their disc touches (counting sort: count, prefix-sum, fill).
/// Dabs entirely outside the frame are dropped.
fn bin_into_bands(
    dabs: &[[f32; 2]],
    radius: f32,
    frame: MaskFrame,
    entry_budget: usize,
) -> Result<(Vec<u32>, Vec<[f32; 2]>), MaskError> {
    let band_count = frame.height.div_ceil(BAND_ROWS);
    let bands_of = |dab: &[f32; 2]| -> Option<(usize, usize)> {
        let (width, height) = (frame.width as f32, frame.height as f32);
        // Pixel centres are at +0.5, so a dab reaches rows in (y - r - 0.5, y + r - 0.5).
        let first_row = (dab[1] - radius - 0.5).floor().max(0.0);
        let last_row = (dab[1] + radius - 0.5).ceil().min(height - 1.0);
        let off_frame = dab[0] + radius < 0.0 || dab[0] - radius > width;
        if off_frame || first_row > last_row {
            return None;
        }
        Some((
            first_row as usize / BAND_ROWS,
            last_row as usize / BAND_ROWS,
        ))
    };

    let mut offsets = vec![0u32; band_count + 1];
    let mut entries = 0usize;
    for (first, last) in dabs.iter().filter_map(bands_of) {
        for band in first..=last {
            offsets[band + 1] += 1;
        }
        entries += last - first + 1;
        if entries > entry_budget {
            return Err(MaskError::BrushTooDense {
                entries,
                limit: MAX_DAB_ENTRIES,
            });
        }
    }
    for band in 0..band_count {
        offsets[band + 1] += offsets[band];
    }
    let mut cursor = offsets.clone();
    let mut binned = vec![[0.0f32; 2]; entries];
    for dab in dabs {
        let Some((first, last)) = bands_of(dab) else {
            continue;
        };
        for band in first..=last {
            binned[cursor[band] as usize] = *dab;
            cursor[band] += 1;
        }
    }
    Ok((offsets, binned))
}

#[cfg(test)]
mod tests {
    use rohditor_edit::MaskPoint;

    use super::*;
    use crate::math::smoothstep;
    use crate::test_support::{FRAME, plane_of};

    fn point(x: f32, y: f32) -> MaskPoint {
        MaskPoint { x, y }
    }

    fn stroke(points: &[(f32, f32)], radius: f32, feather: f32, flow: f32) -> BrushStroke {
        BrushStroke {
            points: points.iter().map(|p| point(p.0, p.1)).collect(),
            radius,
            feather,
            flow,
            erase: false,
        }
    }

    fn evaluate(strokes: Vec<BrushStroke>, frame: MaskFrame) -> Vec<f32> {
        let brush = PreparedBrush::prepare(&BrushMask { strokes }, frame).unwrap();
        let mut keep = Vec::new();
        plane_of(frame, |y, x, out| brush.evaluate_row(y, x, out, &mut keep))
    }

    /// Pixel (x, y) of the 37x23 test plane.
    fn at(plane: &[f32], x: usize, y: usize) -> f32 {
        plane[y * FRAME.width + x]
    }

    #[test]
    fn a_single_dab_is_solid_inside_and_smooth_outside() {
        // Radius 0.2 * 37 = 7.4 px, feather 0.5: solid within 3.7 px, zero from 7.4 px.
        let plane = evaluate(vec![stroke(&[(0.5, 0.5)], 0.2, 0.5, 1.0)], FRAME);
        let (cx, cy) = (18.5, 11.5);
        for y in 0..FRAME.height {
            for x in 0..FRAME.width {
                let d = (x as f32 + 0.5 - cx).hypot(y as f32 + 0.5 - cy);
                let expected = 1.0 - smoothstep(3.7, 7.4, d);
                assert!((at(&plane, x, y) - expected).abs() < 1e-5, "({x}, {y})");
            }
        }
    }

    #[test]
    fn flow_builds_up_over_repeated_dabs_and_caps_at_one() {
        let one = evaluate(vec![stroke(&[(0.5, 0.5)], 0.2, 0.0, 0.3)], FRAME);
        assert!((at(&one, 18, 11) - 0.3).abs() < 1e-5);

        // Radius 7.4 px, spacing 1.85 px: a 7.4 px move places several dabs, and every one of
        // them is within 7.4 px of the centre pixel, so each multiplies `keep` by 0.7.
        let many = evaluate(
            vec![stroke(&[(0.4, 0.5), (0.6, 0.5)], 0.2, 0.0, 0.3)],
            FRAME,
        );
        let dabs = place_dabs(&[[14.8, 11.5], [22.2, 11.5]], 1.85, 100)
            .unwrap()
            .len();
        assert!(dabs >= 4);
        let expected = 1.0 - 0.7f32.powi(dabs as i32);
        assert!((at(&many, 18, 11) - expected).abs() < 1e-5);
        assert!(many.iter().all(|a| *a <= 1.0));
    }

    #[test]
    fn strokes_combine_with_max() {
        let a = stroke(&[(0.3, 0.5)], 0.1, 0.0, 0.5);
        let b = stroke(&[(0.7, 0.5)], 0.1, 0.0, 0.8);
        let both = evaluate(vec![a.clone(), b.clone()], FRAME);
        let (only_a, only_b) = (evaluate(vec![a], FRAME), evaluate(vec![b], FRAME));
        for ((both, a), b) in both.iter().zip(&only_a).zip(&only_b) {
            assert_eq!(*both, a.max(*b));
        }
    }

    #[test]
    fn erase_strokes_only_remove_earlier_paint() {
        let paint = stroke(&[(0.5, 0.5)], 0.2, 0.0, 1.0);
        let mut erase = stroke(&[(0.5, 0.5)], 0.1, 0.0, 1.0);
        erase.erase = true;

        let erased = evaluate(vec![paint.clone(), erase.clone()], FRAME);
        assert_eq!(at(&erased, 18, 11), 0.0);
        // Outside the eraser but inside the paint stays solid.
        assert_eq!(at(&erased, 18 + 6, 11), 1.0);

        // Painting after erasing brings the paint back.
        let repainted = evaluate(vec![paint.clone(), erase, paint], FRAME);
        assert_eq!(at(&repainted, 18, 11), 1.0);

        // Erasing with nothing underneath changes nothing.
        let mut lone = stroke(&[(0.5, 0.5)], 0.1, 0.0, 1.0);
        lone.erase = true;
        assert!(evaluate(vec![lone], FRAME).iter().all(|a| *a == 0.0));
    }

    #[test]
    fn a_stroke_crossing_a_band_boundary_has_no_seam() {
        // The stroke runs down the middle of a 200-row frame, across the row-64 and row-128
        // band boundaries.
        let frame = MaskFrame {
            width: 61,
            height: 200,
        };
        let down = stroke(&[(0.5, 0.2), (0.5, 0.8)], 0.05, 0.4, 0.6);
        let banded = evaluate(vec![down.clone()], frame);

        // Reference: every row sees every dab, no binning.
        let brush = PreparedBrush::prepare(
            &BrushMask {
                strokes: vec![down],
            },
            frame,
        )
        .unwrap();
        let prepared = &brush.strokes[0];
        let long_edge = frame.long_edge();
        let spacing = 0.05 * long_edge / DAB_SPACING_DIVISOR;
        let all_dabs =
            place_dabs(&[[30.5, 40.0], [30.5, 160.0]], spacing, MAX_DAB_ENTRIES).unwrap();
        let mut reference = vec![0.0f32; frame.width * frame.height];
        for (y, row) in reference.chunks_exact_mut(frame.width).enumerate() {
            let mut keep = vec![1.0; frame.width];
            for dab in &all_dabs {
                prepared.splat(*dab, y as f32 + 0.5, 0, &mut keep);
            }
            for (alpha, keep) in row.iter_mut().zip(&keep) {
                *alpha = 1.0 - keep;
            }
        }
        for (a, b) in banded.iter().zip(&reference) {
            assert!((a - b).abs() < 1e-6);
        }
        // The stroke really crosses both boundaries.
        assert!(banded[63 * frame.width + 30] > 0.5 && banded[64 * frame.width + 30] > 0.5);
        assert!(banded[127 * frame.width + 30] > 0.5 && banded[128 * frame.width + 30] > 0.5);
    }

    #[test]
    fn dabs_are_spaced_evenly_around_corners() {
        let points = [[0.0, 0.0], [10.0, 0.0], [10.0, 10.0]];
        let dabs = place_dabs(&points, 4.0, 100).unwrap();
        // Arc length 20 at spacing 4: dabs at 0, 4, 8, 12, 16, 20.
        assert_eq!(dabs.len(), 6);
        assert_eq!(dabs[2], [8.0, 0.0]);
        assert_eq!(dabs[3], [10.0, 2.0]);
        assert_eq!(dabs[5], [10.0, 10.0]);
    }

    #[test]
    fn a_stroke_needing_too_many_dabs_is_rejected() {
        // The spacing floor is 0.25 px, so a 4000 px stroke needs 16000 dabs; budget it out.
        let err = place_dabs(&[[0.0, 0.0], [4000.0, 0.0]], 0.25, 100).unwrap_err();
        assert!(matches!(err, MaskError::BrushTooDense { .. }));
    }

    #[test]
    fn rejects_non_finite_input() {
        let mut bad = stroke(&[(0.5, 0.5)], 0.1, 0.0, 1.0);
        bad.points[0].x = f32::NAN;
        let brush = BrushMask { strokes: vec![bad] };
        assert!(matches!(
            PreparedBrush::prepare(&brush, FRAME),
            Err(MaskError::Degenerate { .. })
        ));
    }
}
