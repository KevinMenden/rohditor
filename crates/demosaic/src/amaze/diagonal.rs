//! Diagonal interpolation correction: red/blue at blue/red sites from the
//! plus and minus diagonals, their variance weighted combination, and a second
//! green pass that uses the resulting R+B estimate.

use rohditor_image::BayerPattern;

use super::math::{
    ARTHRESH, EPS, EPSSQ, GAUSSEVEN, M1, M2, P1, P2, TS, V1, V2, at, green_at_even, half, median,
    sqr,
};
use super::scratch::AmazeScratch;

/// Diagonal gradients and squared diagonal differences of green, stored per
/// horizontal pixel pair for the diagonal stage.
pub(super) fn diagonal_gradients(s: &mut AmazeScratch, pattern: BayerPattern) {
    for rr in 6..TS - 6 {
        let cfa = &s.cfa;
        if green_at_even(pattern, rr) == 0 {
            // Red/blue at even columns: green sits at the odd neighbour.
            for cc in (6..TS - 6).step_by(2) {
                let i = rr * TS + cc;
                s.delp[i >> 1] = (cfa[at(i, P1)] - cfa[at(i, -P1)]).abs();
                s.delm[i >> 1] = (cfa[at(i, M1)] - cfa[at(i, -M1)]).abs();
                let g = i + 1;
                s.dgrbsq1p[i >> 1] = sqr(cfa[g] - cfa[at(g, -P1)]) + sqr(cfa[g] - cfa[at(g, P1)]);
                s.dgrbsq1m[i >> 1] = sqr(cfa[g] - cfa[at(g, -M1)]) + sqr(cfa[g] - cfa[at(g, M1)]);
            }
        } else {
            for cc in (6..TS - 6).step_by(2) {
                let i = rr * TS + cc;
                s.dgrbsq1p[i >> 1] = sqr(cfa[i] - cfa[at(i, -P1)]) + sqr(cfa[i] - cfa[at(i, P1)]);
                s.dgrbsq1m[i >> 1] = sqr(cfa[i] - cfa[at(i, -M1)]) + sqr(cfa[i] - cfa[at(i, M1)]);
                let g = i + 1;
                s.delp[i >> 1] = (cfa[at(g, P1)] - cfa[at(g, -P1)]).abs();
                s.delm[i >> 1] = (cfa[at(g, M1)] - cfa[at(g, -M1)]).abs();
            }
        }
    }
}

/// Diagonal estimate of the opposite colour: adaptive ratio, falling back to
/// Hamilton-Adams where the ratio is implausible.
fn diagonal_estimate(cfa: &[f32], i: usize, step: isize) -> f32 {
    let (near, far) = (cfa[at(i, step)], cfa[at(i, 2 * step)]);
    let ratio = 2.0 * near / (EPS + cfa[i] + far);
    if (1.0 - ratio).abs() < ARTHRESH {
        cfa[i] * ratio
    } else {
        near + 0.5 * (cfa[i] - far)
    }
}

/// Bound a diagonal estimate in regions of high saturation. `step` selects
/// the diagonal; `clip_pt` is the saturation level.
fn bound_diagonal(estimate: f32, cfa: &[f32], i: usize, step: isize, clip_pt: f32) -> f32 {
    let (centre, a, b) = (cfa[i], cfa[at(i, -step)], cfa[at(i, step)]);
    let mut value = estimate;
    if value < centre {
        if 2.0 * value < centre {
            value = median(value, a, b);
        } else {
            let weight = 2.0 * (centre - value) / (EPS + value + centre);
            value = weight * value + (1.0 - weight) * median(value, a, b);
        }
    }
    if value > clip_pt {
        value = median(value, a, b);
    }
    value
}

/// Sum of a `GAUSSEVEN`-weighted neighbourhood of a diagonal-difference plane.
fn even_kernel(plane: &[f32], i: usize) -> f32 {
    GAUSSEVEN[0]
        * (plane[half(i, -V1)] + plane[(i - 1) >> 1] + plane[(i + 1) >> 1] + plane[half(i, V1)])
        + GAUSSEVEN[1]
            * (plane[half(i, -V2 - 1)]
                + plane[half(i, -V2 + 1)]
                + plane[(i - 2 - TS) >> 1]
                + plane[(i + 2 - TS) >> 1]
                + plane[(i - 2 + TS) >> 1]
                + plane[(i + 2 + TS) >> 1]
                + plane[half(i, V2 - 1)]
                + plane[half(i, V2 + 1)])
}

/// Estimate the opposite colour at every red/blue site along both diagonals
/// and the relative weight of the two.
pub(super) fn diagonal_estimates(s: &mut AmazeScratch, pattern: BayerPattern, clip_pt: f32) {
    for rr in 8..TS - 8 {
        let mut cc = 8 + green_at_even(pattern, rr);
        while cc < TS - 8 {
            let i = rr * TS + cc;
            let i1 = i >> 1;
            let cfa = &s.cfa;

            let rbse = diagonal_estimate(cfa, i, M1);
            let rbnw = diagonal_estimate(cfa, i, -M1);
            let rbne = diagonal_estimate(cfa, i, P1);
            let rbsw = diagonal_estimate(cfa, i, -P1);

            let wtse = EPS + s.delm[i1] + s.delm[half(i, M1)] + s.delm[half(i, M2)];
            let wtnw = EPS + s.delm[i1] + s.delm[half(i, -M1)] + s.delm[half(i, -M2)];
            let wtne = EPS + s.delp[i1] + s.delp[half(i, P1)] + s.delp[half(i, P2)];
            let wtsw = EPS + s.delp[i1] + s.delp[half(i, -P1)] + s.delp[half(i, -P2)];

            let rbm = (wtse * rbnw + wtnw * rbse) / (wtse + wtnw);
            let rbp = (wtne * rbsw + wtsw * rbne) / (wtne + wtsw);

            // Variance of R-B in the plus/minus directions.
            let rbvarm = EPSSQ + even_kernel(&s.dgrbsq1m, i);
            s.pmwt[i1] = rbvarm / ((EPSSQ + even_kernel(&s.dgrbsq1p, i)) + rbvarm);

            // Bound the interpolation in regions of high saturation.
            s.rbp[i1] = bound_diagonal(rbp, cfa, i, P1, clip_pt);
            s.rbm[i1] = bound_diagonal(rbm, cfa, i, M1, clip_pt);
            cc += 2;
        }
    }
}

/// Let the plus/minus weight follow its diagonal neighbours where they
/// discriminate better, and form R+B at each red/blue site.
pub(super) fn combine_diagonals(s: &mut AmazeScratch, pattern: BayerPattern) {
    for rr in 10..TS - 10 {
        let mut cc = 10 + green_at_even(pattern, rr);
        while cc < TS - 10 {
            let i = rr * TS + cc;
            let i1 = i >> 1;
            let alt = (s.pmwt[half(i, -M1)]
                + s.pmwt[half(i, P1)]
                + s.pmwt[half(i, -P1)]
                + s.pmwt[half(i, M1)])
                / 4.0;
            if (0.5 - s.pmwt[i1]).abs() < (0.5 - alt).abs() {
                // A better result was obtained from the neighbours.
                s.pmwt[i1] = alt;
            }
            // This is R+B, interpolated.
            s.rbint[i1] =
                0.5 * (s.cfa[i] + s.rbm[i1] * (1.0 - s.pmwt[i1]) + s.rbp[i1] * s.pmwt[i1]);
            cc += 2;
        }
    }
}

/// Second green pass at red/blue sites using R+B. Skipped where the diagonal
/// weight is more decisive than the vertical/horizontal weight.
pub(super) fn refine_green_with_rb(s: &mut AmazeScratch, pattern: BayerPattern, clip_pt: f32) {
    for rr in 12..TS - 12 {
        let mut cc = 12 + green_at_even(pattern, rr);
        while cc < TS - 12 {
            let i = rr * TS + cc;
            let i1 = i >> 1;
            cc += 2;
            if (0.5 - s.pmwt[i1]).abs() < (0.5 - s.hvwt[i1]).abs() {
                continue;
            }

            // Now interpolate G vertically/horizontally using R+B values.
            // Since G interpolation cannot be done diagonally this may lead
            // to colour shifts. The R+B plane is addressed two rows or two
            // columns away: the nearest sites that carry R+B.
            let cfa = &s.cfa;
            let rb = &s.rbint;
            let step = TS; // `v1` in the half-resolution plane: two image rows
            let candidate = |sample: f32, other: f32| {
                let ratio = sample * 2.0 / (EPS + rb[i1] + other);
                if (1.0 - ratio).abs() < ARTHRESH {
                    rb[i1] * ratio
                } else {
                    sample + 0.5 * (rb[i1] - other)
                }
            };
            let gu = candidate(cfa[i - TS], rb[i1 - step]);
            let gd = candidate(cfa[i + TS], rb[i1 + step]);
            let gl = candidate(cfa[i - 1], rb[i1 - 1]);
            let gr = candidate(cfa[i + 1], rb[i1 + 1]);

            // Interpolated G via adaptive weights of the cardinal evaluations.
            let mut gintv = (s.dirwts0[i - TS] * gd + s.dirwts0[i + TS] * gu)
                / (s.dirwts0[i + TS] + s.dirwts0[i - TS]);
            let mut ginth = (s.dirwts1[i - 1] * gr + s.dirwts1[i + 1] * gl)
                / (s.dirwts1[i - 1] + s.dirwts1[i + 1]);

            // Bound the interpolation in regions of high saturation.
            let bound = |value: f32, a: f32, b: f32| {
                let mut value = value;
                if value < rb[i1] {
                    if 2.0 * value < rb[i1] {
                        value = median(value, a, b);
                    } else {
                        let weight = 2.0 * (rb[i1] - value) / (EPS + value + rb[i1]);
                        value = weight * value + (1.0 - weight) * median(value, a, b);
                    }
                }
                if value > clip_pt {
                    value = median(value, a, b);
                }
                value
            };
            gintv = bound(gintv, cfa[i - TS], cfa[i + TS]);
            ginth = bound(ginth, cfa[i - 1], cfa[i + 1]);

            s.green[i] = ginth * (1.0 - s.hvwt[i1]) + gintv * s.hvwt[i1];
            s.dgrb[0][i1] = s.green[i] - cfa[i];
        }
    }
}
