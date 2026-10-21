//! Diagonal interpolation correction: red/blue at blue/red sites from the
//! plus and minus diagonals, their variance weighted combination, and a second
//! green pass that uses the resulting R+B estimate.

use rohditor_image::BayerPattern;

use super::math::{
    ARTHRESH, EPS, EPSSQ, GAUSSEVEN, HalfRow, Row, TS, green_at_even, half_row, half_row_mut,
    half_window_mut, median, row, row_mut, sqr,
};
use super::scratch::AmazeScratch;

/// Diagonal gradients and squared diagonal differences of green, stored per
/// horizontal pixel pair for the diagonal stage.
pub(super) fn diagonal_gradients(s: &mut AmazeScratch, pattern: BayerPattern) {
    for rr in 6..TS - 6 {
        let (up, mid, down) = (row(&s.cfa, rr - 1), row(&s.cfa, rr), row(&s.cfa, rr + 1));
        let (delp, delm) = (half_row_mut(&mut s.delp, rr), half_row_mut(&mut s.delm, rr));
        let (sqp, sqm) = (
            half_row_mut(&mut s.dgrbsq1p, rr),
            half_row_mut(&mut s.dgrbsq1m, rr),
        );
        if green_at_even(pattern, rr) == 0 {
            // Red/blue at even columns: green sits at the odd neighbour.
            for cc in (6..TS - 6).step_by(2) {
                let i1 = cc >> 1;
                delp[i1] = (up[cc + 1] - down[cc - 1]).abs();
                delm[i1] = (down[cc + 1] - up[cc - 1]).abs();
                let g = mid[cc + 1];
                sqp[i1] = sqr(g - down[cc]) + sqr(g - up[cc + 2]);
                sqm[i1] = sqr(g - up[cc]) + sqr(g - down[cc + 2]);
            }
        } else {
            for cc in (6..TS - 6).step_by(2) {
                let i1 = cc >> 1;
                let g = mid[cc];
                sqp[i1] = sqr(g - down[cc - 1]) + sqr(g - up[cc + 1]);
                sqm[i1] = sqr(g - up[cc - 1]) + sqr(g - down[cc + 1]);
                delp[i1] = (up[cc + 2] - down[cc]).abs();
                delm[i1] = (down[cc + 2] - up[cc]).abs();
            }
        }
    }
}

/// `GAUSSEVEN`-weighted neighbourhood of a diagonal-difference plane around the
/// pair `cc >> 1` of the middle row of `rows` (rows `rr - 2 ..= rr + 2`).
#[inline(always)]
fn even_kernel(rows: &[&HalfRow; 5], cc: usize) -> f32 {
    let (up2, up1, mid, down1, down2) = (rows[0], rows[1], rows[2], rows[3], rows[4]);
    GAUSSEVEN[0] * (up1[cc >> 1] + mid[(cc - 1) >> 1] + mid[(cc + 1) >> 1] + down1[cc >> 1])
        + GAUSSEVEN[1]
            * (up2[(cc - 1) >> 1]
                + up2[(cc + 1) >> 1]
                + up1[(cc - 2) >> 1]
                + up1[(cc + 2) >> 1]
                + down1[(cc - 2) >> 1]
                + down1[(cc + 2) >> 1]
                + down2[(cc - 1) >> 1]
                + down2[(cc + 1) >> 1])
}

/// Diagonal estimate of the opposite colour from the diagonal neighbour `near`
/// and the one beyond it, `far`: adaptive ratio, falling back to Hamilton-Adams
/// where the ratio is implausible.
#[inline(always)]
fn diagonal_estimate(centre: f32, near: f32, far: f32) -> f32 {
    let ratio = 2.0 * near / (EPS + centre + far);
    if (1.0 - ratio).abs() < ARTHRESH {
        centre * ratio
    } else {
        near + 0.5 * (centre - far)
    }
}

/// Bound a diagonal estimate in regions of high saturation. `a` and `b` are
/// the two neighbours on this diagonal; `clip_pt` is the saturation level.
#[inline(always)]
fn bound_diagonal(estimate: f32, centre: f32, a: f32, b: f32, clip_pt: f32) -> f32 {
    let mut value = estimate;
    if value < centre {
        value = if 2.0 * value < centre {
            median(value, a, b)
        } else {
            let weight = 2.0 * (centre - value) / (EPS + value + centre);
            weight * value + (1.0 - weight) * median(value, a, b)
        };
    }
    if value > clip_pt {
        median(value, a, b)
    } else {
        value
    }
}

/// Estimate the opposite colour at every red/blue site along both diagonals
/// and the relative weight of the two.
pub(super) fn diagonal_estimates(s: &mut AmazeScratch, pattern: BayerPattern, clip_pt: f32) {
    for rr in 8..TS - 8 {
        // Rows rr-2 ..= rr+2 of the CFA and of the half-resolution planes.
        let cfa: [&Row; 5] = std::array::from_fn(|k| row(&s.cfa, rr + k - 2));
        let delm: [&HalfRow; 5] = std::array::from_fn(|k| half_row(&s.delm, rr + k - 2));
        let delp: [&HalfRow; 5] = std::array::from_fn(|k| half_row(&s.delp, rr + k - 2));
        let sqm: [&HalfRow; 5] = std::array::from_fn(|k| half_row(&s.dgrbsq1m, rr + k - 2));
        let sqp: [&HalfRow; 5] = std::array::from_fn(|k| half_row(&s.dgrbsq1p, rr + k - 2));
        let pmwt = half_row_mut(&mut s.pmwt, rr);
        let rbp = half_row_mut(&mut s.rbp, rr);
        let rbm = half_row_mut(&mut s.rbm, rr);

        for cc in (8 + green_at_even(pattern, rr)..TS - 8).step_by(2) {
            let i1 = cc >> 1;
            let centre = cfa[2][cc];
            // Neighbours on the south-east, north-west, north-east, south-west
            // diagonals, one and two steps out.
            let (se1, se2) = (cfa[3][cc + 1], cfa[4][cc + 2]);
            let (nw1, nw2) = (cfa[1][cc - 1], cfa[0][cc - 2]);
            let (ne1, ne2) = (cfa[1][cc + 1], cfa[0][cc + 2]);
            let (sw1, sw2) = (cfa[3][cc - 1], cfa[4][cc - 2]);

            let rbse = diagonal_estimate(centre, se1, se2);
            let rbnw = diagonal_estimate(centre, nw1, nw2);
            let rbne = diagonal_estimate(centre, ne1, ne2);
            let rbsw = diagonal_estimate(centre, sw1, sw2);

            let wtse = EPS + delm[2][i1] + delm[3][(cc + 1) >> 1] + delm[4][(cc + 2) >> 1];
            let wtnw = EPS + delm[2][i1] + delm[1][(cc - 1) >> 1] + delm[0][(cc - 2) >> 1];
            let wtne = EPS + delp[2][i1] + delp[1][(cc + 1) >> 1] + delp[0][(cc + 2) >> 1];
            let wtsw = EPS + delp[2][i1] + delp[3][(cc - 1) >> 1] + delp[4][(cc - 2) >> 1];

            let rbm_estimate = (wtse * rbnw + wtnw * rbse) / (wtse + wtnw);
            let rbp_estimate = (wtne * rbsw + wtsw * rbne) / (wtne + wtsw);

            // Variance of R-B in the plus/minus directions.
            let rbvarm = EPSSQ + even_kernel(&sqm, cc);
            pmwt[i1] = rbvarm / ((EPSSQ + even_kernel(&sqp, cc)) + rbvarm);

            // Bound the interpolation in regions of high saturation.
            rbp[i1] = bound_diagonal(rbp_estimate, centre, sw1, ne1, clip_pt);
            rbm[i1] = bound_diagonal(rbm_estimate, centre, nw1, se1, clip_pt);
        }
    }
}

/// Let the plus/minus weight follow its diagonal neighbours where they
/// discriminate better, and form R+B at each red/blue site. Rows are processed
/// in order and `pmwt` is updated in place, as upstream does.
pub(super) fn combine_diagonals(s: &mut AmazeScratch, pattern: BayerPattern) {
    for rr in 10..TS - 10 {
        let (window, pmwt) = half_window_mut(&mut s.pmwt, rr);
        let cfa = row(&s.cfa, rr);
        let (rbm, rbp) = (half_row(&s.rbm, rr), half_row(&s.rbp, rr));
        let rbint = half_row_mut(&mut s.rbint, rr);
        for cc in (10 + green_at_even(pattern, rr)..TS - 10).step_by(2) {
            let i1 = cc >> 1;
            let alt = (window.at(-1, cc, -1)
                + window.at(-1, cc, 1)
                + window.at(1, cc, -1)
                + window.at(1, cc, 1))
                / 4.0;
            if (0.5 - pmwt[i1]).abs() < (0.5 - alt).abs() {
                // A better result was obtained from the neighbours.
                pmwt[i1] = alt;
            }
            // This is R+B, interpolated.
            rbint[i1] = 0.5 * (cfa[cc] + rbm[i1] * (1.0 - pmwt[i1]) + rbp[i1] * pmwt[i1]);
        }
    }
}

/// Green estimate from `sample`, the cardinal neighbour of a red/blue site
/// whose R+B is `centre`, and `other`, the R+B of the next such site in that
/// direction: adaptive ratio, falling back to Hamilton-Adams.
#[inline(always)]
fn green_from_rb(sample: f32, centre: f32, other: f32) -> f32 {
    let ratio = sample * 2.0 / (EPS + centre + other);
    if (1.0 - ratio).abs() < ARTHRESH {
        centre * ratio
    } else {
        sample + 0.5 * (centre - other)
    }
}

/// Bound a green estimate against the R+B `centre` and the two cardinal
/// neighbours `a`, `b` in its direction.
#[inline(always)]
fn bound_green(value: f32, centre: f32, a: f32, b: f32, clip_pt: f32) -> f32 {
    let mut value = value;
    if value < centre {
        value = if 2.0 * value < centre {
            median(value, a, b)
        } else {
            let weight = 2.0 * (centre - value) / (EPS + value + centre);
            weight * value + (1.0 - weight) * median(value, a, b)
        };
    }
    if value > clip_pt {
        median(value, a, b)
    } else {
        value
    }
}

/// Second green pass at red/blue sites using R+B. Skipped where the diagonal
/// weight is more decisive than the vertical/horizontal weight.
pub(super) fn refine_green_with_rb(s: &mut AmazeScratch, pattern: BayerPattern, clip_pt: f32) {
    for rr in 12..TS - 12 {
        let (cfa_up, cfa, cfa_down) = (row(&s.cfa, rr - 1), row(&s.cfa, rr), row(&s.cfa, rr + 1));
        let (w0_up, w0_down) = (row(&s.dirwts0, rr - 1), row(&s.dirwts0, rr + 1));
        let w1 = row(&s.dirwts1, rr);
        // R+B at this row and at the nearest rows that carry it, two above and below.
        let (rb_up, rb, rb_down) = (
            half_row(&s.rbint, rr - 2),
            half_row(&s.rbint, rr),
            half_row(&s.rbint, rr + 2),
        );
        let (pmwt, hvwt) = (half_row(&s.pmwt, rr), half_row(&s.hvwt, rr));
        let green = row_mut(&mut s.green, rr);
        let dgrb = half_row_mut(&mut s.dgrb[0], rr);

        for cc in (12 + green_at_even(pattern, rr)..TS - 12).step_by(2) {
            let i1 = cc >> 1;
            if (0.5 - pmwt[i1]).abs() < (0.5 - hvwt[i1]).abs() {
                continue;
            }

            // Now interpolate G vertically/horizontally using R+B values.
            // Since G interpolation cannot be done diagonally this may lead
            // to colour shifts. R+B exists only at red/blue sites, so the
            // vertical neighbour's value comes from two rows away.
            let centre = rb[i1];
            let gu = green_from_rb(cfa_up[cc], centre, rb_up[i1]);
            let gd = green_from_rb(cfa_down[cc], centre, rb_down[i1]);
            let gl = green_from_rb(cfa[cc - 1], centre, rb[i1 - 1]);
            let gr = green_from_rb(cfa[cc + 1], centre, rb[i1 + 1]);

            // Interpolated G via adaptive weights of the cardinal evaluations.
            let gintv = (w0_up[cc] * gd + w0_down[cc] * gu) / (w0_down[cc] + w0_up[cc]);
            let ginth = (w1[cc - 1] * gr + w1[cc + 1] * gl) / (w1[cc - 1] + w1[cc + 1]);

            // Bound the interpolation in regions of high saturation.
            let gintv = bound_green(gintv, centre, cfa_up[cc], cfa_down[cc], clip_pt);
            let ginth = bound_green(ginth, centre, cfa[cc - 1], cfa[cc + 1], clip_pt);

            green[cc] = ginth * (1.0 - hvwt[i1]) + gintv * hvwt[i1];
            dgrb[i1] = green[cc] - cfa[cc];
        }
    }
}
