// Port of crates/demosaic/src/amaze/*. AMaZE is Emil Martinec's algorithm as
// implemented in RawTherapee's amaze_demosaic_RT.cc (GPLv3-or-later, see the
// CPU module for attribution). Keep stage domains, accumulation order and
// arithmetic aligned with the CPU reference when changing these files; the
// stage functions carry the names of their CPU counterparts.
//
// Tiles are 256x256 samples (16 px halo) and the scratch holds every working
// plane of `BATCH` tiles; the dispatch's z coordinate selects the tile slot.
// Full planes hold one value per pixel (row pitch 256). "Pair" planes hold one
// value per horizontal pixel pair (row pitch 128, column index `c >> 1`): each
// pair has exactly one red/blue site per row, so these planes store the values
// of red/blue sites only.
//
// Stages that update a plane in place in the CPU port are not parallel per
// pixel: `bound_vcd`/`bound_hcd` chain along columns/rows, and `populate_green`
// and `combine_diagonals` chain from row to row. They run as "ordered kernels",
// one invocation per column/row/pair that loops in CPU order.
const TS: u32 = 256u;
const TSI: i32 = 256;
const FULL: u32 = 65536u;
const HALF: u32 = 32768u;

// Full planes.
const CFA: u32 = 0u;
const GREEN: u32 = 1u * FULL;
const SQSUM: u32 = 2u * FULL;     // delhvsqsum
const W0: u32 = 3u * FULL;        // dirwts0, vertical
const W1: u32 = 4u * FULL;        // dirwts1, horizontal
const VCD: u32 = 5u * FULL;
const HCD: u32 = 6u * FULL;
const VCDALT: u32 = 7u * FULL;
const HCDALT: u32 = 8u * FULL;
const CDDIFFSQ: u32 = 9u * FULL;
const DGINTV: u32 = 10u * FULL;
const DGINTH: u32 = 11u * FULL;
// Pair planes follow the full planes. Nyquist flags are stored as 0.0 / 1.0.
const PAIRS: u32 = 12u * FULL;
const HVWT: u32 = PAIRS;
const DGRB0: u32 = PAIRS + 1u * HALF;   // G - R
const DGRB1: u32 = PAIRS + 2u * HALF;   // G - B
const DGRB2H: u32 = PAIRS + 3u * HALF;
const DGRB2V: u32 = PAIRS + 4u * HALF;
const DELP: u32 = PAIRS + 5u * HALF;
const DELM: u32 = PAIRS + 6u * HALF;
const SQ1M: u32 = PAIRS + 7u * HALF;    // dgrbsq1m
const SQ1P: u32 = PAIRS + 8u * HALF;    // dgrbsq1p
const NYQTEST: u32 = PAIRS + 9u * HALF;
const PMWT: u32 = PAIRS + 10u * HALF;
const RBM: u32 = PAIRS + 11u * HALF;
const RBP: u32 = PAIRS + 12u * HALF;
const RBINT: u32 = PAIRS + 13u * HALF;
const NYQ: u32 = PAIRS + 14u * HALF;
const NYQ2: u32 = PAIRS + 15u * HALF;
const TILE_WORDS: u32 = PAIRS + 16u * HALF;

const EPS: f32 = 1e-5;
const EPSSQ: f32 = 1e-10;
const ARTHRESH: f32 = 0.75;
const GAUSSODD = array<f32, 4>(0.14659727707323927, 0.103592713382435, 0.0732036125103057, 0.0365543548389495);
// Pre-multiplied with the Nyquist threshold 0.5, as on the CPU.
const GAUSSGRAD = array<f32, 6>(
    0.5 * 0.07384411893421103, 0.5 * 0.06207511968171489, 0.5 * 0.0521818194747806,
    0.5 * 0.03687419286733595, 0.5 * 0.03099732204057846, 0.5 * 0.018413194161458882);
const GAUSSEVEN = array<f32, 2>(0.13719494435797422, 0.05640252782101291);
const GQUINC = array<f32, 4>(0.169917, 0.108947, 0.069855, 0.0287182);

struct Params {
    image: vec4<u32>,        // width, height, mosaic tile width, mosaic tile height
    source: vec4<u32>,       // mosaic columns, Bayer phase, unused, unused
    destination: vec4<u32>,  // camera tile width, camera tile height, columns, unused
    forward: vec4<f32>,      // channel balance applied to the mosaic before reconstruction
    inverse: vec4<f32>,      // undoes `forward` on the reconstructed planes
    clip: vec4<f32>,         // clip point, 80% of the clip point, unused, unused
    tiles: array<vec4<u32>, BATCH>, // per slot: core origin x, y, unused, unused
};
@group(0) @binding(0) var mosaic: texture_2d_array<f32>;
@group(0) @binding(1) var<uniform> params: Params;
@group(0) @binding(2) var<storage, read_write> scratch: array<f32>;
@group(0) @binding(3) var<storage, read_write> invalid: atomic<u32>;
@group(0) @binding(4) var red: texture_storage_2d_array<r32float, write>;
@group(0) @binding(5) var green: texture_storage_2d_array<r32float, write>;
@group(0) @binding(6) var blue: texture_storage_2d_array<r32float, write>;
// Per slot: first/last flagged Nyquist row, first/last flagged column.
@group(0) @binding(7) var<storage, read_write> bounds: array<atomic<u32>>;

// Offset of the invocation's tile slot in the scratch.
var<private> base: u32;
var<workgroup> chain: array<f32, 256>;

fn at(plane: u32, r: i32, c: i32) -> f32 { return scratch[base + plane + u32(r) * TS + u32(c)]; }
fn put(plane: u32, r: i32, c: i32, v: f32) { scratch[base + plane + u32(r) * TS + u32(c)] = v; }
// Pair planes take the full-resolution column of any pixel of the pair.
fn pat(plane: u32, r: i32, c: i32) -> f32 { return scratch[base + plane + u32(r) * 128u + u32(c >> 1u)]; }
fn pput(plane: u32, r: i32, c: i32, v: f32) { scratch[base + plane + u32(r) * 128u + u32(c >> 1u)] = v; }

// Tile origins are even and so is the 16 px halo: local parity is image parity.
fn fc(r: i32, c: i32) -> u32 {
    let index = (u32(r) & 1u) * 2u + (u32(c) & 1u);
    switch params.source.y {
        case 0u: { return array<u32, 4>(0u, 1u, 1u, 2u)[index]; }
        case 1u: { return array<u32, 4>(2u, 1u, 1u, 0u)[index]; }
        case 2u: { return array<u32, 4>(1u, 0u, 2u, 1u)[index]; }
        default: { return array<u32, 4>(1u, 2u, 0u, 1u)[index]; }
    }
}
// 1 when column 2 of the row is green, so red/blue sites sit at odd columns.
fn green_at_even(r: i32) -> i32 { return i32(fc(r, 2) & 1u); }

fn sqr(x: f32) -> f32 { return x * x; }
// Compare-select like the CPU's fmin/fmax; WGSL min/max leave NaN unspecified.
fn fmin(a: f32, b: f32) -> f32 { return select(b, a, a < b); }
fn fmax(a: f32, b: f32) -> f32 { return select(b, a, a > b); }
fn median(a: f32, b: f32, c: f32) -> f32 { return fmax(fmin(a, b), fmin(fmax(b, a), c)); }
fn intp(a: f32, b: f32, c: f32) -> f32 { return a * (b - c) + c; }

fn sample(pos: vec2<u32>) -> f32 {
    let layer = (pos.y / params.image.w) * params.source.x + pos.x / params.image.z;
    return textureLoad(mosaic, vec2<i32>(pos % params.image.zw), i32(layer), 0).x;
}
// Reflect about the first and last sample; preserves parity.
fn mirror(index: i32, length: i32) -> u32 {
    let period = 2 * (length - 1);
    let wrapped = ((index % period) + period) % period;
    return u32(min(wrapped, period - wrapped));
}

// Pixel-grid threads: id.x is the tile pixel, id.z the slot.
fn begin_pixels(id: vec3<u32>) -> vec2<i32> {
    base = id.z * TILE_WORDS;
    return vec2<i32>(i32(id.x % TS), i32(id.x / TS));
}
// Pair-grid threads: id.x = row * 128 + pair. Returns (row, pair) or (-1, -1)
// outside rows `lo..256-lo` and pairs `lo/2..(256-lo)/2`, which are exactly
// the red/blue site loops `cc in (lo + g..256 - lo).step_by(2)` of the CPU.
fn begin_pairs(id: vec3<u32>, lo: i32) -> vec2<i32> {
    base = id.z * TILE_WORDS;
    let r = i32(id.x / 128u);
    let q = i32(id.x % 128u);
    if r < lo || r >= TSI - lo || q < lo / 2 || q >= (TSI - lo) / 2 { return vec2<i32>(-1, -1); }
    return vec2<i32>(r, q);
}

// Exclusive end of the pixels a tile writes (`AmazeTileGeometry::write_window`):
// the core ends 224 px after its origin, or where the image's 16 px bilinear
// ring starts. The start is `max(origin, 16)`.
fn write_limit(origin: vec2<i32>) -> vec2<i32> {
    return min(origin + vec2<i32>(224), vec2<i32>(params.image.xy) - vec2<i32>(16));
}

// Bounding box of the Nyquist texture of this tile, clamped like the CPU
// (`nyquist::detect`). `valid` is false when no area is worth refining.
struct Area { valid: bool, row0: i32, row1: i32, col0: i32, col1: i32 }
fn nyquist_area() -> Area {
    let slot = (base / TILE_WORDS) * 4u;
    let min_row = atomicLoad(&bounds[slot]);
    let max_row = atomicLoad(&bounds[slot + 1u]);
    let min_col = atomicLoad(&bounds[slot + 2u]);
    let max_col = atomicLoad(&bounds[slot + 3u]);
    // No flagged site leaves the sentinels, which the CPU reads as an empty box.
    let start_row = select(0u, min_row, max_row != 0u);
    if start_row == max_row || min_col == max_col || max_row == 0u {
        return Area(false, 0, 0, 0, 0);
    }
    // `<` loop bounds, an even start column, and a margin from the tile edge.
    let start_col = min_col - (min_col & 1u);
    return Area(
        true,
        max(i32(start_row), 8), min(i32(max_row) + 1, TSI - 8),
        max(i32(start_col), 8), min(i32(max_col) + 1, TSI - 8));
}
fn in_area(area: Area, r: i32, c: i32) -> bool {
    return area.valid && r >= area.row0 && r < area.row1 && c >= area.col0 && c < area.col1;
}
