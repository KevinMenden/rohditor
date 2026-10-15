//! Fancy chrominance interpolation and the final red/blue planes.

use rohditor_image::{BayerPattern, CfaColor};

use super::math::{EPS, M1, M3, P1, P3, TS, V1, V2, fc, green_at_even, half};
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
        let mut cc = 14 + green_at_even(pattern, rr);
        // Plane to interpolate: G - B at red sites (1), G - R at blue sites (0).
        let plane = 1 - fc(pattern, rr, cc) / 2;
        while cc < TS - 14 {
            let i = rr * TS + cc;
            let d = &s.dgrb[plane];
            let at_half = |delta: isize| d[half(i, delta)];

            let wtnw = 1.0
                / (EPS
                    + (at_half(-M1) - at_half(M1)).abs()
                    + (at_half(-M1) - at_half(-M3)).abs()
                    + (at_half(M1) - at_half(-M3)).abs());
            let wtne = 1.0
                / (EPS
                    + (at_half(P1) - at_half(-P1)).abs()
                    + (at_half(P1) - at_half(P3)).abs()
                    + (at_half(-P1) - at_half(P3)).abs());
            let wtsw = 1.0
                / (EPS
                    + (at_half(-P1) - at_half(P1)).abs()
                    + (at_half(-P1) - at_half(M3)).abs()
                    + (at_half(P1) - at_half(-P3)).abs());
            let wtse = 1.0
                / (EPS
                    + (at_half(M1) - at_half(-M1)).abs()
                    + (at_half(M1) - at_half(-P3)).abs()
                    + (at_half(-M1) - at_half(M3)).abs());

            // Each diagonal contributes its nearest value, extrapolated from
            // the three further ones along it and across it.
            let diagonal = |near: isize, far: isize, along: isize, across: isize| {
                1.325 * at_half(near) - 0.175 * at_half(far)
                    - 0.075 * at_half(near + along)
                    - 0.075 * at_half(near + across)
            };
            s.dgrb[plane][i >> 1] = (wtnw * diagonal(-M1, -M3, -2, -V2)
                + wtne * diagonal(P1, P3, 2, V2)
                + wtsw * diagonal(-P1, -P3, -2, -V2)
                + wtse * diagonal(M1, M3, 2, V2))
                / (wtnw + wtne + wtsw + wtse);
            cc += 2;
        }
    }
}

/// Red and blue for the tile's valid interior. At red/blue sites they follow
/// directly from green and the colour differences; at green sites the four
/// neighbouring differences are averaged with the direction weight.
/// Like upstream, reconstructed values are clamped at zero.
pub(super) fn finish_planes(s: &mut AmazeScratch, pattern: BayerPattern) {
    for rr in 16..TS - 16 {
        for cc in 16..TS - 16 {
            let i = rr * TS + cc;
            let g = s.green[i];
            let (r, b) = if fc(pattern, rr, cc) == 1 {
                let w = &s.hvwt;
                let (up, down) = (w[half(i, -V1)], w[half(i, V1)]);
                let (left, right) = (w[(i - 1) >> 1], w[(i + 1) >> 1]);
                let temp = 1.0 / (up + 2.0 - right - left + down);
                let blend = |d: &[f32]| {
                    (up * d[half(i, -V1)]
                        + (1.0 - right) * d[(i + 1) >> 1]
                        + (1.0 - left) * d[(i - 1) >> 1]
                        + down * d[half(i, V1)])
                        * temp
                };
                (g - blend(&s.dgrb[0]), g - blend(&s.dgrb[1]))
            } else {
                (g - s.dgrb[0][i >> 1], g - s.dgrb[1][i >> 1])
            };
            s.red[i] = r.max(0.0);
            s.blue[i] = b.max(0.0);
        }
    }
}
