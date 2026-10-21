//! AMaZE green reconstruction: gradients, vertical/horizontal colour
//! differences, their bounding, and the directional weight `hvwt`.

use rohditor_image::BayerPattern;

use super::math::{
    ARTHRESH, EPS, EPSSQ, Row, TS, fc, fmin, green_at_even, half_row_mut, median, row, row_mut,
    rows_around_mut, sqr,
};
use super::scratch::AmazeScratch;

/// Direction weights and gradient energy from the raw CFA.
pub(super) fn gradients(s: &mut AmazeScratch) {
    for rr in 2..TS - 2 {
        let cfa: [&Row; 5] = std::array::from_fn(|k| row(&s.cfa, rr + k - 2));
        let (w0, w1) = (row_mut(&mut s.dirwts0, rr), row_mut(&mut s.dirwts1, rr));
        let sqsum = row_mut(&mut s.delhvsqsum, rr);
        for cc in 2..TS - 2 {
            let delh = (cfa[2][cc + 1] - cfa[2][cc - 1]).abs();
            let delv = (cfa[3][cc] - cfa[1][cc]).abs();
            w0[cc] = EPS + (cfa[4][cc] - cfa[2][cc]).abs() + (cfa[2][cc] - cfa[0][cc]).abs() + delv;
            w1[cc] = EPS
                + (cfa[2][cc + 2] - cfa[2][cc]).abs()
                + (cfa[2][cc] - cfa[2][cc - 2]).abs()
                + delh;
            sqsum[cc] = sqr(delh) + sqr(delv);
        }
    }
}

/// Green estimate from the adaptive ratio when it is plausible, else the
/// Hamilton-Adams fallback.
#[inline(always)]
fn adaptive_or(ratio: f32, centre: f32, fallback: f32) -> f32 {
    if (1.0 - ratio).abs() < ARTHRESH {
        centre * ratio
    } else {
        fallback
    }
}

/// Vertical and horizontal colour differences (G - R or G - B) using adaptive
/// ratios, plus the Hamilton-Adams alternative. `clip_pt8` is 80% of the
/// level at which the first channel saturates.
pub(super) fn colour_differences(s: &mut AmazeScratch, pattern: BayerPattern, clip_pt8: f32) {
    for rr in 4..TS - 4 {
        // Rows rr-2 ..= rr+2 of the CFA and of the vertical direction weights.
        let cfa: [&Row; 5] = std::array::from_fn(|k| row(&s.cfa, rr + k - 2));
        let w0: [&Row; 5] = std::array::from_fn(|k| row(&s.dirwts0, rr + k - 2));
        let w1 = row(&s.dirwts1, rr);
        let (vcd_out, hcd_out) = (row_mut(&mut s.vcd, rr), row_mut(&mut s.hcd, rr));
        let (vcdalt_out, hcdalt_out) = (row_mut(&mut s.vcdalt, rr), row_mut(&mut s.hcdalt, rr));
        let (dgintv_out, dginth_out) = (row_mut(&mut s.dgintv, rr), row_mut(&mut s.dginth, rr));

        // Whether column 4 is a green site; sites alternate along the row.
        let green_first = fc(pattern, rr, 4) & 1 == 1;
        for cc in 4..TS - 4 {
            let fcswitch = green_first == (cc & 1 == 0);
            let cur = cfa[2][cc];
            let (up, up2, down, down2) = (cfa[1][cc], cfa[0][cc], cfa[3][cc], cfa[4][cc]);
            let (left, left2, right, right2) = (
                cfa[2][cc - 1],
                cfa[2][cc - 2],
                cfa[2][cc + 1],
                cfa[2][cc + 2],
            );

            // Colour ratios in each cardinal direction.
            let cru =
                up * (w0[0][cc] + w0[2][cc]) / (w0[0][cc] * (EPS + cur) + w0[2][cc] * (EPS + up2));
            let crd = down * (w0[4][cc] + w0[2][cc])
                / (w0[4][cc] * (EPS + cur) + w0[2][cc] * (EPS + down2));
            let crl =
                left * (w1[cc - 2] + w1[cc]) / (w1[cc - 2] * (EPS + cur) + w1[cc] * (EPS + left2));
            let crr = right * (w1[cc + 2] + w1[cc])
                / (w1[cc + 2] * (EPS + cur) + w1[cc] * (EPS + right2));

            // G interpolated in vertical/horizontal directions, Hamilton-Adams.
            let guha = up + 0.5 * (cur - up2);
            let gdha = down + 0.5 * (cur - down2);
            let glha = left + 0.5 * (cur - left2);
            let grha = right + 0.5 * (cur - right2);

            // G interpolated using adaptive ratios.
            let guar = adaptive_or(cru, cur, guha);
            let gdar = adaptive_or(crd, cur, gdha);
            let glar = adaptive_or(crl, cur, glha);
            let grar = adaptive_or(crr, cur, grha);

            // Adaptive weights for vertical/horizontal directions.
            let hwt = w1[cc - 1] / (w1[cc - 1] + w1[cc + 1]);
            let vwt = w0[1][cc] / (w0[3][cc] + w0[1][cc]);

            // Interpolated G via adaptive weights of the cardinal evaluations.
            let gintvha = vwt * gdha + (1.0 - vwt) * guha;
            let ginthha = hwt * grha + (1.0 - hwt) * glha;

            // Interpolated colour differences; sign flips between green and
            // red/blue sites so that every entry is G - (R or B).
            let (vcd, hcd, vcdalt, hcdalt) = if fcswitch {
                (
                    cur - (vwt * gdar + (1.0 - vwt) * guar),
                    cur - (hwt * grar + (1.0 - hwt) * glar),
                    cur - gintvha,
                    cur - ginthha,
                )
            } else {
                (
                    (vwt * gdar + (1.0 - vwt) * guar) - cur,
                    (hwt * grar + (1.0 - hwt) * glar) - cur,
                    gintvha - cur,
                    ginthha - cur,
                )
            };

            // Use Hamilton-Adams if highlights are (nearly) clipped. Evaluated
            // without short-circuiting so the loop stays branch free.
            let clipped = (cur > clip_pt8) | (gintvha > clip_pt8) | (ginthha > clip_pt8);
            let (guar, gdar, glar, grar) = if clipped {
                (guha, gdha, glha, grha)
            } else {
                (guar, gdar, glar, grar)
            };
            let (vcd, hcd) = if clipped {
                (vcdalt, hcdalt)
            } else {
                (vcd, hcd)
            };

            // Differences of interpolations in opposite directions.
            dgintv_out[cc] = fmin(sqr(guha - gdha), sqr(guar - gdar));
            dginth_out[cc] = fmin(sqr(glha - grha), sqr(glar - grar));
            vcd_out[cc] = vcd;
            hcd_out[cc] = hcd;
            vcdalt_out[cc] = vcdalt;
            hcdalt_out[cc] = hcdalt;
        }
    }
}

/// Bound a colour difference `d` (G - C) at a green site, where `cur` is the
/// green sample and `a`, `b` its two neighbours along the direction. The
/// interpolated value is the other channel's `g = cur - d`.
#[inline(always)]
fn bound_at_green(d: f32, cur: f32, a: f32, b: f32, clip_pt: f32) -> f32 {
    let g = -d + cur;
    let alt = -median(g, a, b) + cur;
    let mut bounded = d;
    if d > 0.0 {
        bounded = if 3.0 * d > g + cur {
            alt
        } else {
            let weight = 1.0 - 3.0 * d / (EPS + g + cur);
            weight * d + (1.0 - weight) * alt
        };
    }
    if g > clip_pt { alt } else { bounded }
}

/// Bound a colour difference `d` (G - C) at a red/blue site, where `cur` is
/// the red or blue sample and the interpolated green is `g = d + cur`.
#[inline(always)]
fn bound_at_red_blue(d: f32, cur: f32, a: f32, b: f32, clip_pt: f32) -> f32 {
    let g = d + cur;
    let alt = median(g, a, b) - cur;
    let mut bounded = d;
    if d < 0.0 {
        bounded = if 3.0 * d < -(g + cur) {
            alt
        } else {
            let weight = 1.0 + 3.0 * d / (EPS + g + cur);
            weight * d + (1.0 - weight) * alt
        };
    }
    if g > clip_pt { alt } else { bounded }
}

/// Variance-like measure of three samples: `3 * sum(x^2) - (sum x)^2`.
#[inline(always)]
fn spread(a: f32, b: f32, c: f32) -> f32 {
    3.0 * (sqr(a) + sqr(b) + sqr(c)) - sqr(a + b + c)
}

/// Choose the smoother of each colour-difference pair, bound the result in
/// saturated regions, and record the squared V-H difference at red/blue sites.
pub(super) fn bound_differences(s: &mut AmazeScratch, pattern: BayerPattern, clip_pt: f32) {
    for rr in 4..TS - 4 {
        let (vcd_up, vcd, vcd_down) = rows_around_mut::<2>(&mut s.vcd, rr);
        let (alt_up, alt_down) = (row(&s.vcdalt, rr - 2), row(&s.vcdalt, rr + 2));
        let (vcdalt, hcdalt) = (row(&s.vcdalt, rr), row(&s.hcdalt, rr));
        let hcd = row_mut(&mut s.hcd, rr);
        let (cfa, cfa_up, cfa_down) = (row(&s.cfa, rr), row(&s.cfa, rr - 1), row(&s.cfa, rr + 1));
        let cddiffsq = row_mut(&mut s.cddiffsq, rr);

        // True at green sites.
        let mut green = fc(pattern, rr, 4) & 1 == 1;
        for cc in 4..TS - 4 {
            // Choose the smallest variance; this yields a smoother interpolation.
            let hcdvar = spread(hcd[cc - 2], hcd[cc], hcd[cc + 2]);
            let hcdaltvar = spread(hcdalt[cc - 2], hcdalt[cc], hcdalt[cc + 2]);
            let vcdvar = spread(vcd_up[cc], vcd[cc], vcd_down[cc]);
            let vcdaltvar = spread(alt_up[cc], vcdalt[cc], alt_down[cc]);
            let h = if hcdaltvar < hcdvar {
                hcdalt[cc]
            } else {
                hcd[cc]
            };
            let v = if vcdaltvar < vcdvar {
                vcdalt[cc]
            } else {
                vcd[cc]
            };

            let cur = cfa[cc];
            if green {
                hcd[cc] = bound_at_green(h, cur, cfa[cc - 1], cfa[cc + 1], clip_pt);
                vcd[cc] = bound_at_green(v, cur, cfa_up[cc], cfa_down[cc], clip_pt);
            } else {
                hcd[cc] = bound_at_red_blue(h, cur, cfa[cc - 1], cfa[cc + 1], clip_pt);
                vcd[cc] = bound_at_red_blue(v, cur, cfa_up[cc], cfa_down[cc], clip_pt);
                cddiffsq[cc] = sqr(vcd[cc] - hcd[cc]);
            }
            green = !green;
        }
    }
}

/// Adaptive horizontal-versus-vertical weight at every red/blue site.
pub(super) fn direction_weights(s: &mut AmazeScratch, pattern: BayerPattern) {
    for rr in 6..TS - 6 {
        // vcd rows rr-3..=rr+3 and dgintv rows rr-2..=rr+2, vertically around the site.
        let vcd: [&Row; 7] = std::array::from_fn(|k| row(&s.vcd, rr + k - 3));
        let dgintv: [&Row; 5] = std::array::from_fn(|k| row(&s.dgintv, rr + k - 2));
        let (hcd, dginth) = (row(&s.hcd, rr), row(&s.dginth, rr));
        let (w0_up, w0_down) = (row(&s.dirwts0, rr - 1), row(&s.dirwts0, rr + 1));
        let w1 = row(&s.dirwts1, rr);
        let hvwt = half_row_mut(&mut s.hvwt, rr);

        for cc in (6 + green_at_even(pattern, rr)..TS - 6).step_by(2) {
            // Colour difference variances in the cardinal directions.
            let uave = vcd[3][cc] + vcd[2][cc] + vcd[1][cc] + vcd[0][cc];
            let dave = vcd[3][cc] + vcd[4][cc] + vcd[5][cc] + vcd[6][cc];
            let lave = hcd[cc] + hcd[cc - 1] + hcd[cc - 2] + hcd[cc - 3];
            let rave = hcd[cc] + hcd[cc + 1] + hcd[cc + 2] + hcd[cc + 3];

            let dgrbvvaru = sqr(vcd[3][cc] - uave)
                + sqr(vcd[2][cc] - uave)
                + sqr(vcd[1][cc] - uave)
                + sqr(vcd[0][cc] - uave);
            let dgrbvvard = sqr(vcd[3][cc] - dave)
                + sqr(vcd[4][cc] - dave)
                + sqr(vcd[5][cc] - dave)
                + sqr(vcd[6][cc] - dave);
            let dgrbhvarl = sqr(hcd[cc] - lave)
                + sqr(hcd[cc - 1] - lave)
                + sqr(hcd[cc - 2] - lave)
                + sqr(hcd[cc - 3] - lave);
            let dgrbhvarr = sqr(hcd[cc] - rave)
                + sqr(hcd[cc + 1] - rave)
                + sqr(hcd[cc + 2] - rave)
                + sqr(hcd[cc + 3] - rave);

            let hwt = w1[cc - 1] / (w1[cc - 1] + w1[cc + 1]);
            let vwt = w0_up[cc] / (w0_down[cc] + w0_up[cc]);

            let vcdvar = EPSSQ + vwt * dgrbvvard + (1.0 - vwt) * dgrbvvaru;
            let hcdvar = EPSSQ + hwt * dgrbhvarr + (1.0 - hwt) * dgrbhvarl;

            // Fluctuations in up/down and left/right interpolations of colours.
            let fluct_u = dgintv[2][cc] + dgintv[1][cc] + dgintv[0][cc];
            let fluct_d = dgintv[2][cc] + dgintv[3][cc] + dgintv[4][cc];
            let fluct_l = dginth[cc] + dginth[cc - 1] + dginth[cc - 2];
            let fluct_r = dginth[cc] + dginth[cc + 1] + dginth[cc + 2];

            let vcdvar1 = EPSSQ + vwt * fluct_d + (1.0 - vwt) * fluct_u;
            let hcdvar1 = EPSSQ + hwt * fluct_r + (1.0 - hwt) * fluct_l;

            // Adaptive weights for the green interpolation.
            let varwt = hcdvar / (vcdvar + hcdvar);
            let diffwt = hcdvar1 / (vcdvar1 + hcdvar1);

            // If both agree on the direction use the one with the strongest
            // discrimination; otherwise use the fluctuation weight.
            hvwt[cc >> 1] = if (0.5 - varwt) * (0.5 - diffwt) > 0.0
                && (0.5 - diffwt).abs() < (0.5 - varwt).abs()
            {
                varwt
            } else {
                diffwt
            };
        }
    }
}
