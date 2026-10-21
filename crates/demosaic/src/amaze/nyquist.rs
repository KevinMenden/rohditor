//! Nyquist texture detection, area interpolation of the direction weight in
//! textured regions, the final green at red/blue sites, and the curvature
//! based refinement of green inside Nyquist areas.

use rohditor_image::BayerPattern;

use super::math::{
    EPSSQ, GAUSSGRAD, GAUSSODD, GQUINC, M1, M2, P1, P2, Row, TS, TSH, V1, V2, at, green_at_even,
    half, half_row_mut, half_window_mut, intp, row, rows_around_mut, sqr,
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
        // Rows rr-2 ..= rr+2 of the two source planes.
        let c: [&Row; 5] = std::array::from_fn(|k| row(&s.cddiffsq, rr + k - 2));
        let d: [&Row; 5] = std::array::from_fn(|k| row(&s.delhvsqsum, rr + k - 2));
        let out = half_row_mut(&mut s.nyqutest, rr);
        for cc in (6 + green_at_even(pattern, rr)..TS - 6).step_by(2) {
            out[cc >> 1] = (GAUSSODD[0] * c[2][cc]
                + GAUSSODD[1] * (c[1][cc - 1] + c[1][cc + 1] + c[3][cc - 1] + c[3][cc + 1])
                + GAUSSODD[2] * (c[0][cc] + c[2][cc - 2] + c[2][cc + 2] + c[4][cc])
                + GAUSSODD[3] * (c[0][cc - 2] + c[0][cc + 2] + c[4][cc - 2] + c[4][cc + 2]))
                - (GAUSSGRAD[0] * d[2][cc]
                    + GAUSSGRAD[1] * (d[1][cc] + d[2][cc + 1] + d[2][cc - 1] + d[3][cc])
                    + GAUSSGRAD[2] * (d[1][cc - 1] + d[1][cc + 1] + d[3][cc - 1] + d[3][cc + 1])
                    + GAUSSGRAD[3] * (d[0][cc] + d[2][cc - 2] + d[2][cc + 2] + d[4][cc])
                    + GAUSSGRAD[4]
                        * (d[0][cc - 1]
                            + d[0][cc + 1]
                            + d[1][cc - 2]
                            + d[1][cc + 2]
                            + d[3][cc - 2]
                            + d[3][cc + 2]
                            + d[4][cc - 1]
                            + d[4][cc + 1])
                    + GAUSSGRAD[5] * (d[0][cc - 2] + d[0][cc + 2] + d[4][cc - 2] + d[4][cc + 2]));
        }
    }

    // Nyquist test: bounding box of flagged pixels.
    let (mut start_row, mut end_row) = (0, 0);
    let (mut start_col, mut end_col) = (TS + 1, 0);
    for rr in 6..TS - 6 {
        let first = 6 + green_at_even(pattern, rr);
        for cc in (first..TS - 6).step_by(2) {
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
        let (hv_window, hvwt) = half_window_mut(&mut s.hvwt, rr);
        let (green_up, green, green_down) = rows_around_mut::<1>(&mut s.green, rr);
        let (cfa, vcd, hcd) = (row(&s.cfa, rr), row(&s.vcd, rr), row(&s.hcd, rr));
        let nyquist = <&[u8; TSH]>::try_from(&s.nyquist2[rr * TSH..(rr + 1) * TSH])
            .expect("a half plane row holds TSH samples");
        let dgrb = half_row_mut(&mut s.dgrb[0], rr);
        let (curvature_h, curvature_v) = (
            half_row_mut(&mut s.dgrb2_h, rr),
            half_row_mut(&mut s.dgrb2_v, rr),
        );

        for cc in (8 + green_at_even(pattern, rr)..TS - 8).step_by(2) {
            let i1 = cc >> 1;
            // First ask if one gets more directional discrimination from the
            // nearby B/R sites (a weight further from 0.5 is more decisive).
            let alt = (hv_window.at(-1, cc, -1)
                + hv_window.at(-1, cc, 1)
                + hv_window.at(1, cc, -1)
                + hv_window.at(1, cc, 1))
                / 4.0;
            if (0.5 - hvwt[i1]).abs() < (0.5 - alt).abs() {
                hvwt[i1] = alt;
            }

            // Evaluate the colour difference and G (finally!).
            dgrb[i1] = intp(hvwt[i1], vcd[cc], hcd[cc]);
            green[cc] = cfa[cc] + dgrb[i1];

            // Local curvature in G (preparation for the Nyquist refinement).
            let flagged = nyquist[i1] != 0;
            curvature_h[i1] = if flagged {
                sqr(green[cc] - 0.5 * (green[cc - 1] + green[cc + 1]))
            } else {
                0.0
            };
            curvature_v[i1] = if flagged {
                sqr(green[cc] - 0.5 * (green_up[cc] + green_down[cc]))
            } else {
                0.0
            };
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
