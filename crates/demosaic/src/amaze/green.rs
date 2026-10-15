//! AMaZE green reconstruction: gradients, vertical/horizontal colour
//! differences, their bounding, and the directional weight `hvwt`.

use rohditor_image::BayerPattern;

use super::math::{
    ARTHRESH, EPS, EPSSQ, TS, V1, V2, V3, at, fc, green_at_even, median, sqr,
};
use super::scratch::AmazeScratch;

/// Direction weights and gradient energy from the raw CFA.
pub(super) fn gradients(s: &mut AmazeScratch) {
    for rr in 2..TS - 2 {
        for cc in 2..TS - 2 {
            let i = rr * TS + cc;
            let delh = (s.cfa[i + 1] - s.cfa[i - 1]).abs();
            let delv = (s.cfa[i + TS] - s.cfa[i - TS]).abs();
            s.dirwts0[i] = EPS
                + (s.cfa[i + 2 * TS] - s.cfa[i]).abs()
                + (s.cfa[i] - s.cfa[i - 2 * TS]).abs()
                + delv;
            s.dirwts1[i] = EPS
                + (s.cfa[i + 2] - s.cfa[i]).abs()
                + (s.cfa[i] - s.cfa[i - 2]).abs()
                + delh;
            s.delhvsqsum[i] = sqr(delh) + sqr(delv);
        }
    }
}

/// Vertical and horizontal colour differences (G - R or G - B) using adaptive
/// ratios, plus the Hamilton-Adams alternative. `clip_pt8` is 80% of the
/// level at which the first channel saturates.
pub(super) fn colour_differences(
    s: &mut AmazeScratch,
    pattern: BayerPattern,
    clip_pt8: f32,
) {
    for rr in 4..TS - 4 {
        let mut fcswitch = fc(pattern, rr, 4) & 1 == 1;
        for cc in 4..TS - 4 {
            let i = rr * TS + cc;
            let cfa = &s.cfa;
            let (w0, w1) = (&s.dirwts0, &s.dirwts1);

            // Colour ratios in each cardinal direction.
            let cru = cfa[i - TS] * (w0[i - 2 * TS] + w0[i])
                / (w0[i - 2 * TS] * (EPS + cfa[i]) + w0[i] * (EPS + cfa[i - 2 * TS]));
            let crd = cfa[i + TS] * (w0[i + 2 * TS] + w0[i])
                / (w0[i + 2 * TS] * (EPS + cfa[i]) + w0[i] * (EPS + cfa[i + 2 * TS]));
            let crl = cfa[i - 1] * (w1[i - 2] + w1[i])
                / (w1[i - 2] * (EPS + cfa[i]) + w1[i] * (EPS + cfa[i - 2]));
            let crr = cfa[i + 1] * (w1[i + 2] + w1[i])
                / (w1[i + 2] * (EPS + cfa[i]) + w1[i] * (EPS + cfa[i + 2]));

            // G interpolated in vertical/horizontal directions, Hamilton-Adams.
            let guha = cfa[i - TS] + 0.5 * (cfa[i] - cfa[i - 2 * TS]);
            let gdha = cfa[i + TS] + 0.5 * (cfa[i] - cfa[i + 2 * TS]);
            let glha = cfa[i - 1] + 0.5 * (cfa[i] - cfa[i - 2]);
            let grha = cfa[i + 1] + 0.5 * (cfa[i] - cfa[i + 2]);

            // G interpolated using adaptive ratios.
            let adaptive = |ratio: f32, fallback: f32| {
                if (1.0 - ratio).abs() < ARTHRESH {
                    cfa[i] * ratio
                } else {
                    fallback
                }
            };
            let mut guar = adaptive(cru, guha);
            let mut gdar = adaptive(crd, gdha);
            let mut glar = adaptive(crl, glha);
            let mut grar = adaptive(crr, grha);

            // Adaptive weights for vertical/horizontal directions.
            let hwt = w1[i - 1] / (w1[i - 1] + w1[i + 1]);
            let vwt = w0[i - TS] / (w0[i + TS] + w0[i - TS]);

            // Interpolated G via adaptive weights of the cardinal evaluations.
            let gintvha = vwt * gdha + (1.0 - vwt) * guha;
            let ginthha = hwt * grha + (1.0 - hwt) * glha;

            // Interpolated colour differences; sign flips between green and
            // red/blue sites so that every entry is G - (R or B).
            let (vcd, hcd, vcdalt, hcdalt);
            if fcswitch {
                vcd = cfa[i] - (vwt * gdar + (1.0 - vwt) * guar);
                hcd = cfa[i] - (hwt * grar + (1.0 - hwt) * glar);
                vcdalt = cfa[i] - gintvha;
                hcdalt = cfa[i] - ginthha;
            } else {
                vcd = (vwt * gdar + (1.0 - vwt) * guar) - cfa[i];
                hcd = (hwt * grar + (1.0 - hwt) * glar) - cfa[i];
                vcdalt = gintvha - cfa[i];
                hcdalt = ginthha - cfa[i];
            }
            fcswitch = !fcswitch;

            let (mut vcd, mut hcd) = (vcd, hcd);
            if cfa[i] > clip_pt8 || gintvha > clip_pt8 || ginthha > clip_pt8 {
                // Use Hamilton-Adams if highlights are (nearly) clipped.
                guar = guha;
                gdar = gdha;
                glar = glha;
                grar = grha;
                vcd = vcdalt;
                hcd = hcdalt;
            }

            // Differences of interpolations in opposite directions.
            s.dgintv[i] = sqr(guha - gdha).min(sqr(guar - gdar));
            s.dginth[i] = sqr(glha - grha).min(sqr(glar - grar));
            s.vcd[i] = vcd;
            s.hcd[i] = hcd;
            s.vcdalt[i] = vcdalt;
            s.hcdalt[i] = hcdalt;
        }
    }
}

/// Choose the smoother of each colour-difference pair, bound the result in
/// saturated regions, and record the squared V-H difference at red/blue sites.
pub(super) fn bound_differences(s: &mut AmazeScratch, pattern: BayerPattern, clip_pt: f32) {
    for rr in 4..TS - 4 {
        // True at green sites.
        let mut green = fc(pattern, rr, 4) & 1 == 1;
        for cc in 4..TS - 4 {
            let i = rr * TS + cc;
            let hcdvar = 3.0 * (sqr(s.hcd[i - 2]) + sqr(s.hcd[i]) + sqr(s.hcd[i + 2]))
                - sqr(s.hcd[i - 2] + s.hcd[i] + s.hcd[i + 2]);
            let hcdaltvar = 3.0
                * (sqr(s.hcdalt[i - 2]) + sqr(s.hcdalt[i]) + sqr(s.hcdalt[i + 2]))
                - sqr(s.hcdalt[i - 2] + s.hcdalt[i] + s.hcdalt[i + 2]);
            let vcdvar = 3.0
                * (sqr(s.vcd[i - 2 * TS]) + sqr(s.vcd[i]) + sqr(s.vcd[i + 2 * TS]))
                - sqr(s.vcd[i - 2 * TS] + s.vcd[i] + s.vcd[i + 2 * TS]);
            let vcdaltvar = 3.0
                * (sqr(s.vcdalt[i - 2 * TS]) + sqr(s.vcdalt[i]) + sqr(s.vcdalt[i + 2 * TS]))
                - sqr(s.vcdalt[i - 2 * TS] + s.vcdalt[i] + s.vcdalt[i + 2 * TS]);

            // Choose the smallest variance; this yields a smoother interpolation.
            if hcdaltvar < hcdvar {
                s.hcd[i] = s.hcdalt[i];
            }
            if vcdaltvar < vcdvar {
                s.vcd[i] = s.vcdalt[i];
            }

            let cfa = &s.cfa;
            let (cur, left, right) = (cfa[i], cfa[i - 1], cfa[i + 1]);
            let (up, down) = (cfa[i - TS], cfa[i + TS]);
            if green {
                // Bound the interpolation in regions of high saturation.
                let ginth = -s.hcd[i] + cur;
                let gintv = -s.vcd[i] + cur;
                if s.hcd[i] > 0.0 {
                    if 3.0 * s.hcd[i] > ginth + cur {
                        s.hcd[i] = -median(ginth, left, right) + cur;
                    } else {
                        let hwt = 1.0 - 3.0 * s.hcd[i] / (EPS + ginth + cur);
                        s.hcd[i] = hwt * s.hcd[i]
                            + (1.0 - hwt) * (-median(ginth, left, right) + cur);
                    }
                }
                if s.vcd[i] > 0.0 {
                    if 3.0 * s.vcd[i] > gintv + cur {
                        s.vcd[i] = -median(gintv, up, down) + cur;
                    } else {
                        let vwt = 1.0 - 3.0 * s.vcd[i] / (EPS + gintv + cur);
                        s.vcd[i] =
                            vwt * s.vcd[i] + (1.0 - vwt) * (-median(gintv, up, down) + cur);
                    }
                }
                if ginth > clip_pt {
                    s.hcd[i] = -median(ginth, left, right) + cur;
                }
                if gintv > clip_pt {
                    s.vcd[i] = -median(gintv, up, down) + cur;
                }
            } else {
                // Red or blue site: the stored differences are G - C.
                let ginth = s.hcd[i] + cur;
                let gintv = s.vcd[i] + cur;
                if s.hcd[i] < 0.0 {
                    if 3.0 * s.hcd[i] < -(ginth + cur) {
                        s.hcd[i] = median(ginth, left, right) - cur;
                    } else {
                        let hwt = 1.0 + 3.0 * s.hcd[i] / (EPS + ginth + cur);
                        s.hcd[i] =
                            hwt * s.hcd[i] + (1.0 - hwt) * (median(ginth, left, right) - cur);
                    }
                }
                if s.vcd[i] < 0.0 {
                    if 3.0 * s.vcd[i] < -(gintv + cur) {
                        s.vcd[i] = median(gintv, up, down) - cur;
                    } else {
                        let vwt = 1.0 + 3.0 * s.vcd[i] / (EPS + gintv + cur);
                        s.vcd[i] =
                            vwt * s.vcd[i] + (1.0 - vwt) * (median(gintv, up, down) - cur);
                    }
                }
                if ginth > clip_pt {
                    s.hcd[i] = median(ginth, left, right) - cur;
                }
                if gintv > clip_pt {
                    s.vcd[i] = median(gintv, up, down) - cur;
                }
                s.cddiffsq[i] = sqr(s.vcd[i] - s.hcd[i]);
            }
            green = !green;
        }
    }
}

/// Adaptive horizontal-versus-vertical weight at every red/blue site.
pub(super) fn direction_weights(s: &mut AmazeScratch, pattern: BayerPattern) {
    for rr in 6..TS - 6 {
        let mut cc = 6 + green_at_even(pattern, rr);
        while cc < TS - 6 {
            let i = rr * TS + cc;
            let (vcd, hcd) = (&s.vcd, &s.hcd);

            // Colour difference variances in the cardinal directions.
            let uave = vcd[i] + vcd[at(i, -V1)] + vcd[at(i, -V2)] + vcd[at(i, -V3)];
            let dave = vcd[i] + vcd[at(i, V1)] + vcd[at(i, V2)] + vcd[at(i, V3)];
            let lave = hcd[i] + hcd[i - 1] + hcd[i - 2] + hcd[i - 3];
            let rave = hcd[i] + hcd[i + 1] + hcd[i + 2] + hcd[i + 3];

            let mut dgrbvvaru = sqr(vcd[i] - uave)
                + sqr(vcd[at(i, -V1)] - uave)
                + sqr(vcd[at(i, -V2)] - uave)
                + sqr(vcd[at(i, -V3)] - uave);
            let mut dgrbvvard = sqr(vcd[i] - dave)
                + sqr(vcd[at(i, V1)] - dave)
                + sqr(vcd[at(i, V2)] - dave)
                + sqr(vcd[at(i, V3)] - dave);
            let mut dgrbhvarl = sqr(hcd[i] - lave)
                + sqr(hcd[i - 1] - lave)
                + sqr(hcd[i - 2] - lave)
                + sqr(hcd[i - 3] - lave);
            let mut dgrbhvarr = sqr(hcd[i] - rave)
                + sqr(hcd[i + 1] - rave)
                + sqr(hcd[i + 2] - rave)
                + sqr(hcd[i + 3] - rave);

            let hwt = s.dirwts1[i - 1] / (s.dirwts1[i - 1] + s.dirwts1[i + 1]);
            let vwt = s.dirwts0[i - TS] / (s.dirwts0[i + TS] + s.dirwts0[i - TS]);

            let vcdvar = EPSSQ + vwt * dgrbvvard + (1.0 - vwt) * dgrbvvaru;
            let hcdvar = EPSSQ + hwt * dgrbhvarr + (1.0 - hwt) * dgrbhvarl;

            // Fluctuations in up/down and left/right interpolations of colours.
            dgrbvvaru = s.dgintv[i] + s.dgintv[at(i, -V1)] + s.dgintv[at(i, -V2)];
            dgrbvvard = s.dgintv[i] + s.dgintv[at(i, V1)] + s.dgintv[at(i, V2)];
            dgrbhvarl = s.dginth[i] + s.dginth[i - 1] + s.dginth[i - 2];
            dgrbhvarr = s.dginth[i] + s.dginth[i + 1] + s.dginth[i + 2];

            let vcdvar1 = EPSSQ + vwt * dgrbvvard + (1.0 - vwt) * dgrbvvaru;
            let hcdvar1 = EPSSQ + hwt * dgrbhvarr + (1.0 - hwt) * dgrbhvarl;

            // Adaptive weights for the green interpolation.
            let varwt = hcdvar / (vcdvar + hcdvar);
            let diffwt = hcdvar1 / (vcdvar1 + hcdvar1);

            // If both agree on the direction use the one with the strongest
            // discrimination; otherwise use the fluctuation weight.
            s.hvwt[i >> 1] = if (0.5 - varwt) * (0.5 - diffwt) > 0.0
                && (0.5 - diffwt).abs() < (0.5 - varwt).abs()
            {
                varwt
            } else {
                diffwt
            };
            cc += 2;
        }
    }
}
