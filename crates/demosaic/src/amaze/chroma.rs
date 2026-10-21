//! Fancy chrominance interpolation and the final red/blue planes.

use rohditor_image::{BayerPattern, CfaColor};

use super::math::{
    EPS, HalfRow, TS, fc, fmax, green_at_even, half_row, half_window_mut, row, row_mut,
};
use super::scratch::AmazeScratch;

/// Offset of the red site within a Bayer quartet: `(ey, ex)` are the row and
/// column offsets of the red subarray.
fn red_offset(pattern: BayerPattern) -> (usize, usize) {
    match (pattern.color_at(0, 0), pattern.color_at(1, 0)) {
        (CfaColor::Green, CfaColor::Red) => (0, 1),
        (CfaColor::Green, _) => (1, 0),
        (CfaColor::Red, _) => (0, 0),
        (CfaColor::Blue, _) => (1, 1),
    }
}

/// Interpolate G - B at red sites and G - R at blue sites from the diagonal
/// neighbours, weighting each diagonal by its local smoothness.
pub(super) fn interpolate_chroma(s: &mut AmazeScratch, pattern: BayerPattern) {
    // Split G - B out of the combined colour difference at the B coset.
    let (ey, ex) = red_offset(pattern);
    for rr in (13 - ey..TS - 12).step_by(2) {
        for i1 in ((rr * TS + 13 - ex) >> 1)..((rr * TS + TS - 12) >> 1) {
            s.dgrb[1][i1] = s.dgrb[0][i1];
            s.dgrb[0][i1] = 0.0;
        }
    }

    for rr in 14..TS - 14 {
        let start = 14 + green_at_even(pattern, rr);
        // Plane to interpolate: G - B at red sites (1), G - R at blue sites (0).
        let plane = 1 - fc(pattern, rr, start) / 2;
        let (window, out) = half_window_mut(&mut s.dgrb[plane], rr);
        // Value `dr` rows and `dc` columns away from the site.
        let d = |dr: isize, cc: usize, dc: isize| window.at(dr, cc, dc);

        for cc in (start..TS - 14).step_by(2) {
            let wtnw = 1.0
                / (EPS
                    + (d(-1, cc, -1) - d(1, cc, 1)).abs()
                    + (d(-1, cc, -1) - d(-3, cc, -3)).abs()
                    + (d(1, cc, 1) - d(-3, cc, -3)).abs());
            let wtne = 1.0
                / (EPS
                    + (d(-1, cc, 1) - d(1, cc, -1)).abs()
                    + (d(-1, cc, 1) - d(-3, cc, 3)).abs()
                    + (d(1, cc, -1) - d(-3, cc, 3)).abs());
            let wtsw = 1.0
                / (EPS
                    + (d(1, cc, -1) - d(-1, cc, 1)).abs()
                    + (d(1, cc, -1) - d(3, cc, 3)).abs()
                    + (d(-1, cc, 1) - d(3, cc, -3)).abs());
            let wtse = 1.0
                / (EPS
                    + (d(1, cc, 1) - d(-1, cc, -1)).abs()
                    + (d(1, cc, 1) - d(3, cc, -3)).abs()
                    + (d(-1, cc, -1) - d(3, cc, 3)).abs());

            // Each diagonal contributes its nearest value, extrapolated from
            // the three further ones along it and across it.
            out[cc >> 1] = (wtnw
                * (1.325 * d(-1, cc, -1)
                    - 0.175 * d(-3, cc, -3)
                    - 0.075 * d(-1, cc, -3)
                    - 0.075 * d(-3, cc, -1))
                + wtne
                    * (1.325 * d(-1, cc, 1)
                        - 0.175 * d(-3, cc, 3)
                        - 0.075 * d(-1, cc, 3)
                        - 0.075 * d(1, cc, 1))
                + wtsw
                    * (1.325 * d(1, cc, -1)
                        - 0.175 * d(3, cc, -3)
                        - 0.075 * d(1, cc, -3)
                        - 0.075 * d(-1, cc, -1))
                + wtse
                    * (1.325 * d(1, cc, 1)
                        - 0.175 * d(3, cc, 3)
                        - 0.075 * d(1, cc, 3)
                        - 0.075 * d(3, cc, 1)))
                / (wtnw + wtne + wtsw + wtse);
        }
    }
}

/// Red and blue for the tile's valid interior. At red/blue sites they follow
/// directly from green and the colour differences; at green sites the four
/// neighbouring differences are averaged with the direction weight.
/// Like upstream, reconstructed values are clamped at zero.
pub(super) fn finish_planes(s: &mut AmazeScratch, pattern: BayerPattern) {
    for rr in 16..TS - 16 {
        let green = row(&s.green, rr);
        let (hv_up, hv, hv_down) = (
            half_row(&s.hvwt, rr - 1),
            half_row(&s.hvwt, rr),
            half_row(&s.hvwt, rr + 1),
        );
        let diff = |plane: usize| {
            (
                half_row(&s.dgrb[plane], rr - 1),
                half_row(&s.dgrb[plane], rr),
                half_row(&s.dgrb[plane], rr + 1),
            )
        };
        let (g_r_up, g_r, g_r_down) = diff(0);
        let (g_b_up, g_b, g_b_down) = diff(1);
        let red = row_mut(&mut s.red, rr);
        let blue = row_mut(&mut s.blue, rr);

        // Reconstructed (red, blue) at a green site: the four neighbouring
        // colour differences, weighted by the direction weight.
        let at_green = |cc: usize| {
            let (up, down) = (hv_up[cc >> 1], hv_down[cc >> 1]);
            let (left, right) = (hv[(cc - 1) >> 1], hv[(cc + 1) >> 1]);
            let temp = 1.0 / (up + 2.0 - right - left + down);
            let blend = |d_up: &HalfRow, d: &HalfRow, d_down: &HalfRow| {
                (up * d_up[cc >> 1]
                    + (1.0 - right) * d[(cc + 1) >> 1]
                    + (1.0 - left) * d[(cc - 1) >> 1]
                    + down * d_down[cc >> 1])
                    * temp
            };
            (
                green[cc] - blend(g_r_up, g_r, g_r_down),
                green[cc] - blend(g_b_up, g_b, g_b_down),
            )
        };
        let at_red_blue = |cc: usize| (green[cc] - g_r[cc >> 1], green[cc] - g_b[cc >> 1]);

        // Sites alternate along the row: handle them as pairs, green first or last.
        let green_first = fc(pattern, rr, 16) == 1;
        for cc in (16..TS - 16).step_by(2) {
            let ((r0, b0), (r1, b1)) = if green_first {
                (at_green(cc), at_red_blue(cc + 1))
            } else {
                (at_red_blue(cc), at_green(cc + 1))
            };
            red[cc] = fmax(r0, 0.0);
            blue[cc] = fmax(b0, 0.0);
            red[cc + 1] = fmax(r1, 0.0);
            blue[cc + 1] = fmax(b1, 0.0);
        }
    }
}
