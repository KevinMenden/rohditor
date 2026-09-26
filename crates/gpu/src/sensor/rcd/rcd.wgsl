// Port of crates/demosaic/src/rcd.rs and rcd_stages.rs. RCD follows the
// RawTherapee/darktable tiled formulation attributed there to Luis Sanz
// Rodríguez, Ingo Weyrich, and Hanno Schwalm. Keep stage domains and arithmetic
// aligned with the CPU reference when changing this file.
const EDGE: u32 = 194u;
const FULL: u32 = EDGE * EDGE;
const HALF: u32 = FULL / 2u;
const CFA: u32 = 0u;
const RED: u32 = FULL;
const GREEN: u32 = 2u * FULL;
const BLUE: u32 = 3u * FULL;
const VH: u32 = 4u * FULL;
const PQ: u32 = 5u * FULL;
const P_DIFF: u32 = PQ + HALF;
const Q_DIFF: u32 = P_DIFF + HALF;
const EPS: f32 = 1e-5;
const EPS2: f32 = 1e-10;

struct Params {
    image: vec4<u32>, // width, height, mosaic tile width, mosaic tile height
    source: vec4<u32>, // mosaic columns, Bayer phase, unused, unused
    destination: vec4<u32>, // camera tile width, camera tile height, columns, unused
    tile: vec4<u32>, // global origin x/y, local width/height
};
@group(0) @binding(0) var mosaic: texture_2d_array<f32>;
@group(0) @binding(1) var<uniform> p: Params;
@group(0) @binding(2) var<storage, read_write> scratch: array<f32>;
@group(0) @binding(3) var<storage, read_write> invalid: atomic<u32>;
@group(0) @binding(4) var red: texture_storage_2d_array<r32float, write>;
@group(0) @binding(5) var green: texture_storage_2d_array<r32float, write>;
@group(0) @binding(6) var blue: texture_storage_2d_array<r32float, write>;

fn read(base: u32, index: i32) -> f32 { return scratch[base + u32(index)]; }
fn put(base: u32, index: i32, value: f32) { scratch[base + u32(index)] = value; }
fn color(pos: vec2<u32>) -> u32 {
    let index = (pos.y & 1u) * 2u + (pos.x & 1u);
    switch p.source.y {
        case 0u: { return array<u32, 4>(0u, 1u, 1u, 2u)[index]; }
        case 1u: { return array<u32, 4>(2u, 1u, 1u, 0u)[index]; }
        case 2u: { return array<u32, 4>(1u, 0u, 2u, 1u)[index]; }
        default: { return array<u32, 4>(1u, 2u, 0u, 1u)[index]; }
    }
}
fn sample(pos: vec2<u32>) -> f32 {
    let layer = (pos.y / p.image.w) * p.source.x + pos.x / p.image.z;
    return textureLoad(mosaic, vec2<i32>(pos % p.image.zw), i32(layer), 0).x;
}
fn rgb_base(channel: u32) -> u32 {
    if channel == 0u { return RED; }
    if channel == 1u { return GREEN; }
    return BLUE;
}
fn square(value: f32) -> f32 { return value * value; }
fn denominator(value: f32) -> f32 {
    if abs(value) >= EPS { return value; }
    if (bitcast<u32>(value) & 0x80000000u) != 0u { return -EPS; }
    return EPS;
}
fn weighted(a: f32, x: f32, b: f32, y: f32) -> f32 {
    return (a * x + b * y) / denominator(a + b);
}
fn refined(center: f32, nw: f32, ne: f32, sw: f32, se: f32) -> f32 {
    let neighborhood = 0.25 * (nw + ne + sw + se);
    var selected = center;
    if abs(0.5 - center) < abs(0.5 - neighborhood) { selected = neighborhood; }
    return clamp(selected, 0.0, 1.0);
}
fn interpolate(direction: f32, horizontal: f32, vertical: f32) -> f32 {
    return fma(direction, horizontal, (1.0 - direction) * vertical);
}
fn ratio(sample_value: f32, low: f32, neighbor: f32) -> f32 {
    return sample_value * (2.0 * low) / denominator(EPS + low + neighbor);
}
fn position(id: u32) -> vec2<u32> { return vec2(id % EDGE, id / EDGE); }
fn interior(pos: vec2<u32>, border: u32) -> bool {
    return pos.x >= border && pos.y >= border &&
        pos.x + border < p.tile.z && pos.y + border < p.tile.w;
}
fn rb_site(pos: vec2<u32>) -> bool {
    return (color(p.tile.xy + vec2(0u, pos.y)) & 1u) == (pos.x & 1u);
}

@compute @workgroup_size(64)
fn clear_populate(@builtin(global_invocation_id) id: vec3<u32>) {
    let i = id.x;
    if i >= FULL { return; }
    scratch[CFA + i] = 0.0;
    scratch[RED + i] = 0.0;
    scratch[GREEN + i] = 0.0;
    scratch[BLUE + i] = 0.0;
    scratch[VH + i] = 0.0;
    if i < HALF {
        scratch[PQ + i] = 0.0;
        scratch[P_DIFF + i] = 0.0;
        scratch[Q_DIFF + i] = 0.0;
    }
    let pos = position(i);
    if any(pos >= p.tile.zw) { return; }
    let value = sample(p.tile.xy + pos);
    let first = color(p.tile.xy + vec2(0u, pos.y));
    let second = color(p.tile.xy + vec2(1u, pos.y));
    scratch[CFA + i] = value;
    scratch[rgb_base(first) + i] = value;
    scratch[rgb_base(second) + i] = value;
}

fn vertical_hp(i: i32) -> f32 {
    let s = i32(EDGE);
    return (read(CFA, i - 3*s) - read(CFA, i - s) - read(CFA, i + s)
        + read(CFA, i + 3*s)) - 3.0 * (read(CFA, i - 2*s) + read(CFA, i + 2*s))
        + 6.0 * read(CFA, i);
}
fn horizontal_hp(i: i32) -> f32 {
    return (read(CFA, i - 3) - read(CFA, i - 1) - read(CFA, i + 1)
        + read(CFA, i + 3)) - 3.0 * (read(CFA, i - 2) + read(CFA, i + 2))
        + 6.0 * read(CFA, i);
}
@compute @workgroup_size(64)
fn directions(@builtin(global_invocation_id) id: vec3<u32>) {
    if id.x >= FULL { return; }
    let pos = position(id.x);
    if !interior(pos, 4u) { return; }
    let i = i32(id.x);
    let s = i32(EDGE);
    let v = (square(vertical_hp(i - s)) + square(vertical_hp(i))
        + square(vertical_hp(i + s)));
    let h = (square(horizontal_hp(i - 1)) + square(horizontal_hp(i))
        + square(horizontal_hp(i + 1)));
    let vertical = max(v, EPS2);
    let horizontal = max(h, EPS2);
    put(VH, i, vertical / (vertical + horizontal));
}

@compute @workgroup_size(64)
fn low_pass(@builtin(global_invocation_id) id: vec3<u32>) {
    if id.x >= FULL { return; }
    let pos = position(id.x);
    if !interior(pos, 2u) || !rb_site(pos) { return; }
    let i = i32(id.x);
    let s = i32(EDGE);
    let value = read(CFA, i)
        + 0.5 * (read(CFA, i-s) + read(CFA, i+s) + read(CFA, i-1) + read(CFA, i+1))
        + 0.25 * (read(CFA, i-s-1) + read(CFA, i-s+1)
            + read(CFA, i+s-1) + read(CFA, i+s+1));
    put(PQ, i / 2, value);
}

fn cardinal_gradient(i: i32, offset: i32) -> f32 {
    return EPS + abs(read(CFA, i+offset) - read(CFA, i-offset))
        + abs(read(CFA, i) - read(CFA, i+2*offset))
        + abs(read(CFA, i+offset) - read(CFA, i+3*offset))
        + abs(read(CFA, i+2*offset) - read(CFA, i+4*offset));
}
fn vh_refined(i: i32) -> f32 {
    let s = i32(EDGE);
    return refined(read(VH,i), read(VH,i-s-1), read(VH,i-s+1),
        read(VH,i+s-1), read(VH,i+s+1));
}
@compute @workgroup_size(64)
fn green_pass(@builtin(global_invocation_id) id: vec3<u32>) {
    if id.x >= FULL { return; }
    let pos = position(id.x);
    if !interior(pos, 4u) || !rb_site(pos) { return; }
    let i = i32(id.x);
    let s = i32(EDGE);
    let low = read(PQ, i/2);
    let north = ratio(read(CFA,i-s), low, read(PQ,i/2-s/2));
    let south = ratio(read(CFA,i+s), low, read(PQ,i/2+s/2));
    let west = ratio(read(CFA,i-1), low, read(PQ,i/2-1));
    let east = ratio(read(CFA,i+1), low, read(PQ,i/2+1));
    let vertical = weighted(cardinal_gradient(i,s), north, cardinal_gradient(i,-s), south);
    let horizontal = weighted(cardinal_gradient(i,-1), east, cardinal_gradient(i,1), west);
    put(GREEN, i, interpolate(vh_refined(i), horizontal, vertical));
}

@compute @workgroup_size(64)
fn diagonal_hp(@builtin(global_invocation_id) id: vec3<u32>) {
    if id.x >= FULL { return; }
    let pos = position(id.x);
    if pos.y < 3u || pos.y + 3u >= p.tile.w || pos.x < 3u ||
       pos.x + 3u >= p.tile.z || (pos.x & 1u) == 0u { return; }
    let i = i32(id.x);
    let s = i32(EDGE);
    let pv = (read(CFA,i-3*s-3) - read(CFA,i-s-1) - read(CFA,i+s+1)
        + read(CFA,i+3*s+3)) - 3.0 * (read(CFA,i-2*s-2) + read(CFA,i+2*s+2))
        + 6.0 * read(CFA,i);
    let qv = (read(CFA,i-3*s+3) - read(CFA,i-s+1) - read(CFA,i+s-1)
        + read(CFA,i+3*s-3)) - 3.0 * (read(CFA,i-2*s+2) + read(CFA,i+2*s-2))
        + 6.0 * read(CFA,i);
    put(P_DIFF, i/2, square(pv));
    put(Q_DIFF, i/2, square(qv));
}

@compute @workgroup_size(64)
fn diagonal_directions(@builtin(global_invocation_id) id: vec3<u32>) {
    if id.x >= FULL { return; }
    let pos = position(id.x);
    if !interior(pos, 4u) || !rb_site(pos) { return; }
    let i = i32(id.x);
    let s = i32(EDGE);
    let previous = (i-s-1)/2;
    let next = (i+s-1)/2;
    let pv = max(read(P_DIFF,previous) + read(P_DIFF,i/2) + read(P_DIFF,next+1), EPS2);
    let qv = max(read(Q_DIFF,previous+1) + read(Q_DIFF,i/2) + read(Q_DIFF,next), EPS2);
    put(PQ, i/2, pv / (pv + qv));
}

fn diagonal_gradient(base: u32, i: i32, offset: i32) -> f32 {
    return EPS + abs(read(base,i+offset) - read(base,i-offset))
        + abs(read(base,i+offset) - read(base,i+3*offset))
        + abs(read(GREEN,i) - read(GREEN,i+2*offset));
}
fn difference(base: u32, i: i32) -> f32 { return read(base,i) - read(GREEN,i); }
@compute @workgroup_size(64)
fn opposite_color(@builtin(global_invocation_id) id: vec3<u32>) {
    if id.x >= FULL { return; }
    let pos = position(id.x);
    if !interior(pos, 4u) || !rb_site(pos) { return; }
    let i = i32(id.x);
    let s = i32(EDGE);
    let previous = (i-s-1)/2;
    let next = (i+s-1)/2;
    let direction = refined(read(PQ,i/2), read(PQ,previous), read(PQ,previous+1),
        read(PQ,next), read(PQ,next+1));
    let base = rgb_base(2u - color(p.tile.xy + pos));
    let nw = -s-1;
    let ne = -s+1;
    let sw = s-1;
    let se = s+1;
    let dp = weighted(diagonal_gradient(base,i,nw), difference(base,i+se),
        diagonal_gradient(base,i,se), difference(base,i+nw));
    let dq = weighted(diagonal_gradient(base,i,ne), difference(base,i+sw),
        diagonal_gradient(base,i,sw), difference(base,i+ne));
    put(base, i, read(GREEN,i) + interpolate(direction,dq,dp));
}

@compute @workgroup_size(64)
fn at_green(@builtin(global_invocation_id) id: vec3<u32>) {
    if id.x >= FULL { return; }
    let pos = position(id.x);
    if !interior(pos, 4u) || rb_site(pos) { return; }
    let i = i32(id.x);
    let s = i32(EDGE);
    let direction = vh_refined(i);
    let g = read(GREEN,i);
    let n1 = EPS + abs(g - read(GREEN,i-2*s));
    let s1 = EPS + abs(g - read(GREEN,i+2*s));
    let w1 = EPS + abs(g - read(GREEN,i-2));
    let e1 = EPS + abs(g - read(GREEN,i+2));
    for (var color_index = 0u; color_index <= 2u; color_index += 2u) {
        let base = rgb_base(color_index);
        let ns = abs(read(base,i-s) - read(base,i+s));
        let ew = abs(read(base,i-1) - read(base,i+1));
        let ng = n1 + ns + abs(read(base,i-s) - read(base,i-3*s));
        let sg = s1 + ns + abs(read(base,i+s) - read(base,i+3*s));
        let wg = w1 + ew + abs(read(base,i-1) - read(base,i-3));
        let eg = e1 + ew + abs(read(base,i+1) - read(base,i+3));
        let north = read(base,i-s) - read(GREEN,i-s);
        let south = read(base,i+s) - read(GREEN,i+s);
        let west = read(base,i-1) - read(GREEN,i-1);
        let east = read(base,i+1) - read(GREEN,i+1);
        let vertical = weighted(ng,south,sg,north);
        let horizontal = weighted(eg,west,wg,east);
        put(base,i,g + interpolate(direction,horizontal,vertical));
    }
}

@compute @workgroup_size(64)
fn scatter_core(@builtin(global_invocation_id) id: vec3<u32>) {
    if id.x >= FULL { return; }
    let pos = position(id.x);
    if !interior(pos, 10u) { return; }
    let global = p.tile.xy + pos;
    let measured = color(global);
    let original = sample(global);
    let i = i32(id.x);
    var values = vec3(read(RED,i), read(GREEN,i), read(BLUE,i));
    values[measured] = original;
    if any((bitcast<vec3<u32>>(values) & vec3(0x7f800000u)) == vec3(0x7f800000u)) {
        atomicOr(&invalid, 1u);
    }
    let local = vec2<i32>(global % p.destination.xy);
    let layer = i32((global.y / p.destination.y) * p.destination.z + global.x / p.destination.x);
    textureStore(red,local,layer,vec4(values.r,0.0,0.0,1.0));
    textureStore(green,local,layer,vec4(values.g,0.0,0.0,1.0));
    textureStore(blue,local,layer,vec4(values.b,0.0,0.0,1.0));
}
