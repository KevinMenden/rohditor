//! Nyquist texture detection, area interpolation of the direction weight in
//! textured regions, the final green at red/blue sites, and the curvature
//! based refinement of green inside Nyquist areas.

use rohditor_image::BayerPattern;

use super::math::{
    EPSSQ, GAUSSGRAD, GAUSSODD, GQUINC, M1, M2, P1, P2, TS, V1, V2, at, green_at_even, half, intp,
    sqr,
};
use super::scratch::AmazeScratch;

/// Pixel range of the Nyquist texture found in a tile, if any.
pub(super) struct NyquistArea {
    start_row: usize,
    end_row: usize,
    start_col: usize,
    end_col: usize,
}

/// Compare the smoothed colour-difference energy with the smoothed CFA
/// gradient energy at each red/blue site. Where the former wins, the pixel is
/// in a Nyquist texture region. Returns the (clamped) bounding box of such
/// pixels, or `None` when the tile has none worth refining.
pub(super) fn detect(s: &mut AmazeScratch, pattern: BayerPattern) -> Option<NyquistArea> {
    // Precompute the test statistic.
    for rr in 6..TS - 6 {
        let mut cc = 6 + green_at_even(pattern, rr);
        while cc < TS - 6 {
            let i = rr * TS + cc;
            let c = &s.cddiffsq;
            let d = &s.delhvsqsum;
            s.nyqutest[i >> 1] = (GAUSSODD[0] * c[i]
                + GAUSSODD[1] * (c[at(i, -M1)] + c[at(i, P1)] + c[at(i, -P1)] + c[at(i, M1)])
                + GAUSSODD[2] * (c[at(i, -V2)] + c[i - 2] + c[i + 2] + c[at(i, V2)])
                + GAUSSODD[3] * (c[at(i, -M2)] + c[at(i, P2)] + c[at(i, -P2)] + c[at(i, M2)]))
                - (GAUSSGRAD[0] * d[i]
                    + GAUSSGRAD[1] * (d[at(i, -V1)] + d[i + 1] + d[i - 1] + d[at(i, V1)])
                    + GAUSSGRAD[2] * (d[at(i, -M1)] + d[at(i, P1)] + d[at(i, -P1)] + d[at(i, M1)])
                    + GAUSSGRAD[3] * (d[at(i, -V2)] + d[i - 2] + d[i + 2] + d[at(i, V2)])
                    + GAUSSGRAD[4]
                        * (d[at(i, -V2 - 1)]
                            + d[at(i, -V2 + 1)]
                            + d[i - TS - 2]
                            + d[i - TS + 2]
                            + d[i + TS - 2]
                            + d[i + TS + 2]
                            + d[at(i, V2 - 1)]
                            + d[at(i, V2 + 1)])
                    + GAUSSGRAD[5] * (d[at(i, -M2)] + d[at(i, P2)] + d[at(i, -P2)] + d[at(i, M2)]));
            cc += 2;
        }
    }

    // Nyquist test: bounding box of flagged pixels.
    let (mut start_row, mut end_row) = (0, 0);
    let (mut start_col, mut end_col) = (TS + 1, 0);
    for rr in 6..TS - 6 {
        let mut cc = 6 + green_at_even(pattern, rr);
        while cc < TS - 6 {
            let i = rr * TS + cc;
            if s.nyqutest[i >> 1] > 0.0 {
                s.nyquist[i >> 1] = 1;
                if start_row == 0 {
                    start_row = rr;
                }
                end_row = rr;
                start_col = start_col.min(cc);
                end_col = end_col.max(cc);
            }
            cc += 2;
        }
    }

    if start_row == end_row || start_col == end_col {
        return None;
    }
    // Upstream: `<` loop bounds, even start column, kept clear of the border.
    end_row += 1;
    end_col += 1;
    start_col -= start_col & 1;
    Some(NyquistArea {
        start_row: start_row.max(8),
        end_row: end_row.min(TS - 8),
        start_col: start_col.max(8),
        end_col: end_col.min(TS - 8),
    })
}

impl NyquistArea {
    /// Red/blue site indices of row `rr` inside the area.
    fn sites(&self, pattern: BayerPattern, rr: usize) -> impl Iterator<Item = usize> {
        let first = rr * TS + self.start_col + green_at_even(pattern, rr);
        (first..rr * TS + self.end_col).step_by(2)
    }
}

/// Neighbourhood vote on the Nyquist flags, then area interpolation of the
/// direction weight `hvwt` for pixels that stay flagged.
pub(super) fn area_interpolation(s: &mut AmazeScratch, pattern: BayerPattern, area: &NyquistArea) {
    for rr in area.start_row..area.end_row {
        for i in area.sites(pattern, rr) {
            let n = &s.nyquist;
            let votes = u32::from(n[half(i, -V2)])
                + u32::from(n[half(i, -M1)])
                + u32::from(n[half(i, P1)])
                + u32::from(n[(i - 2) >> 1])
                + u32::from(n[(i + 2) >> 1])
                + u32::from(n[half(i, -P1)])
                + u32::from(n[half(i, M1)])
                + u32::from(n[half(i, V2)]);
            // If most neighbours are Nyquist, so is this pixel; on a tie keep it.
            s.nyquist2[i >> 1] = match votes {
                5.. => 1,
                4 => s.nyquist[i >> 1],
                _ => 0,
            };
        }
    }

    for rr in area.start_row..area.end_row {
        for i in area.sites(pattern, rr) {
            if s.nyquist2[i >> 1] == 0 {
                continue;
            }
            let (mut sumcfa, mut sumh, mut sumv) = (0.0_f32, 0.0_f32, 0.0_f32);
            let (mut sumsqh, mut sumsqv, mut areawt) = (0.0_f32, 0.0_f32, 0.0_f32);
            for di in (-6..7).step_by(2) {
                let mut i1 = at(i, di * V1 - 6);
                for _ in (-6..7).step_by(2) {
                    if s.nyquist2[i1 >> 1] != 0 {
                        let c = s.cfa[i1];
                        sumcfa += c;
                        sumh += s.cfa[i1 - 1] + s.cfa[i1 + 1];
                        sumv += s.cfa[at(i1, -V1)] + s.cfa[at(i1, V1)];
                        sumsqh += sqr(c - s.cfa[i1 - 1]) + sqr(c - s.cfa[i1 + 1]);
                        sumsqv += sqr(c - s.cfa[at(i1, -V1)]) + sqr(c - s.cfa[at(i1, V1)]);
                        areawt += 1.0;
                    }
                    i1 += 2;
                }
            }

            // Horizontal and vertical colour differences, and adaptive weight.
            sumh = sumcfa - 0.5 * sumh;
            sumv = sumcfa - 0.5 * sumv;
            areawt *= 0.5;
            let hcdvar = EPSSQ + (areawt * sumsqh - sumh * sumh).abs();
            let vcdvar = EPSSQ + (areawt * sumsqv - sumv * sumv).abs();
            s.hvwt[i >> 1] = hcdvar / (vcdvar + hcdvar);
        }
    }
}

/// Final green at red/blue sites from the colour differences and `hvwt`,
/// after letting `hvwt` follow the diagonal neighbours where they discriminate
/// better. Rows are processed in order and `hvwt` is updated in place, as
/// upstream does.
pub(super) fn populate_green(s: &mut AmazeScratch, pattern: BayerPattern) {
    for rr in 8..TS - 8 {
        let mut i = rr * TS + 8 + green_at_even(pattern, rr);
        while i < rr * TS + TS - 8 {
            // First ask if one gets more directional discrimination from the
            // nearby B/R sites (a weight further from 0.5 is more decisive).
            let alt = (s.hvwt[half(i, -M1)]
                + s.hvwt[half(i, P1)]
                + s.hvwt[half(i, -P1)]
                + s.hvwt[half(i, M1)])
                / 4.0;
            if (0.5 - s.hvwt[i >> 1]).abs() < (0.5 - alt).abs() {
                s.hvwt[i >> 1] = alt;
            }

            // Evaluate the colour difference and G (finally!).
            s.dgrb[0][i >> 1] = intp(s.hvwt[i >> 1], s.vcd[i], s.hcd[i]);
            s.green[i] = s.cfa[i] + s.dgrb[0][i >> 1];

            // Local curvature in G (preparation for the Nyquist refinement).
            let nyquist = s.nyquist2[i >> 1] != 0;
            s.dgrb2_h[i >> 1] = if nyquist {
                sqr(s.green[i] - 0.5 * (s.green[i - 1] + s.green[i + 1]))
            } else {
                0.0
            };
            s.dgrb2_v[i >> 1] = if nyquist {
                sqr(s.green[i] - 0.5 * (s.green[at(i, -V1)] + s.green[at(i, V1)]))
            } else {
                0.0
            };
            i += 2;
        }
    }
}

/// Refine green inside Nyquist areas using the local G curvature.
pub(super) fn refine_green(s: &mut AmazeScratch, pattern: BayerPattern, area: &NyquistArea) {
    for rr in area.start_row..area.end_row {
        for i in area.sites(pattern, rr) {
            if s.nyquist2[i >> 1] == 0 {
                continue;
            }
            // Local averages (over Nyquist pixels only) of G curvature squared.
            let average = |plane: &[f32]| {
                EPSSQ
                    + (GQUINC[0] * plane[i >> 1]
                        + GQUINC[1]
                            * (plane[half(i, -M1)]
                                + plane[half(i, P1)]
                                + plane[half(i, -P1)]
                                + plane[half(i, M1)])
                        + GQUINC[2]
                            * (plane[half(i, -V2)]
                                + plane[(i - 2) >> 1]
                                + plane[(i + 2) >> 1]
                                + plane[half(i, V2)])
                        + GQUINC[3]
                            * (plane[half(i, -M2)]
                                + plane[half(i, P2)]
                                + plane[half(i, -P2)]
                                + plane[half(i, M2)]))
            };
            let gvarh = average(&s.dgrb2_h);
            let gvarv = average(&s.dgrb2_v);
            // Use the results as weights for the refined G interpolation.
            s.dgrb[0][i >> 1] = (s.hcd[i] * gvarv + s.vcd[i] * gvarh) / (gvarv + gvarh);
            s.green[i] = s.cfa[i] + s.dgrb[0][i >> 1];
        }
    }
}
