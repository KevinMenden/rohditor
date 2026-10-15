//! Constants, small numeric helpers, and tile index arithmetic shared by the
//! AMaZE stages. Names follow the RawTherapee source so stages can be compared
//! line by line.

use rohditor_image::BayerPattern;

pub(super) use super::TILE_SIZE as TS;

/// Half the tile width: the pitch of the half-resolution planes.
pub(super) const TSH: usize = TS / 2;

// Offsets between a pixel and its vertical (`V`), plus-diagonal (`P`, towards
// north-east) and minus-diagonal (`M`, towards south-east) neighbours.
pub(super) const V1: isize = TS as isize;
pub(super) const V2: isize = 2 * V1;
pub(super) const V3: isize = 3 * V1;
pub(super) const P1: isize = -V1 + 1;
pub(super) const P2: isize = -2 * V1 + 2;
pub(super) const P3: isize = -3 * V1 + 3;
pub(super) const M1: isize = V1 + 1;
pub(super) const M2: isize = 2 * V1 + 2;
pub(super) const M3: isize = 3 * V1 + 3;

/// Tolerance to avoid dividing by zero.
pub(super) const EPS: f32 = 1.0e-5;
pub(super) const EPSSQ: f32 = 1.0e-10;
/// Adaptive-ratio threshold.
pub(super) const ARTHRESH: f32 = 0.75;

// The kernels keep the full decimal expansions of the upstream source so each
// rounds to the same f32.
/// Gaussian on a 5x5 quincunx, sigma = 1.2.
#[allow(clippy::excessive_precision)]
pub(super) const GAUSSODD: [f32; 4] = [
    0.14659727707323927,
    0.103592713382435,
    0.0732036125103057,
    0.0365543548389495,
];
/// Nyquist texture test threshold.
const NYQTHRESH: f32 = 0.5;
/// Gaussian on 5x5 pre-multiplied with `NYQTHRESH`.
#[allow(clippy::excessive_precision)]
pub(super) const GAUSSGRAD: [f32; 6] = [
    NYQTHRESH * 0.07384411893421103,
    NYQTHRESH * 0.06207511968171489,
    NYQTHRESH * 0.0521818194747806,
    NYQTHRESH * 0.03687419286733595,
    NYQTHRESH * 0.03099732204057846,
    NYQTHRESH * 0.018413194161458882,
];
/// Gaussian on a 5x5 alternate quincunx, sigma = 1.5.
#[allow(clippy::excessive_precision)]
pub(super) const GAUSSEVEN: [f32; 2] = [0.13719494435797422, 0.05640252782101291];
/// Gaussian on a quincunx grid.
pub(super) const GQUINC: [f32; 4] = [0.169917, 0.108947, 0.069855, 0.0287182];

/// Index of the pixel `delta` away from `index`; stages guarantee it is in the tile.
#[inline]
pub(super) fn at(index: usize, delta: isize) -> usize {
    index.wrapping_add_signed(delta)
}

/// Index into a half-resolution plane for the pixel `delta` away from `index`.
/// Each entry covers a horizontal pixel pair (one green, one red/blue site).
#[inline]
pub(super) fn half(index: usize, delta: isize) -> usize {
    at(index, delta) >> 1
}

/// Colour channel (0 = R, 1 = G, 2 = B) at tile row `row`, column `col`.
#[inline]
pub(super) fn fc(pattern: BayerPattern, row: usize, col: usize) -> usize {
    pattern.color_at(col, row).channel_index()
}

/// 1 when column 2 of `row` is green, else 0. Selects the start column of the
/// loops over red/blue sites, which are every other pixel in a row.
#[inline]
pub(super) fn green_at_even(pattern: BayerPattern, row: usize) -> usize {
    fc(pattern, row, 2) & 1
}

#[inline]
pub(super) fn sqr(x: f32) -> f32 {
    x * x
}

#[inline]
pub(super) fn median(a: f32, b: f32, c: f32) -> f32 {
    a.min(b).max(b.max(a).min(c))
}

/// `a * b + (1 - a) * c`, evaluated as upstream does.
#[inline]
pub(super) fn intp(a: f32, b: f32, c: f32) -> f32 {
    a * (b - c) + c
}
