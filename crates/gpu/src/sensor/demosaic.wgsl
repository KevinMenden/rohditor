// Coordinates are crop-local, including across mosaic and RGB texture layers.
struct Parameters {
    image: vec4<u32>, // width, height, source tile width, source tile height
    source: vec4<u32>, // source columns, Bayer phase, algorithm (0 bilinear, 1 MHC), unused
    region: vec4<u32>, // left, top, width, height
    destination: vec4<u32>, // plane tile width, plane tile height, columns, unused
};
@group(0) @binding(0) var mosaic: texture_2d_array<f32>;
@group(0) @binding(1) var<uniform> p: Parameters;
@group(0) @binding(2) var<storage, read_write> invalid: atomic<u32>;
@group(0) @binding(3) var red: texture_storage_2d_array<r32float, write>;
@group(0) @binding(4) var green: texture_storage_2d_array<r32float, write>;
@group(0) @binding(5) var blue: texture_storage_2d_array<r32float, write>;
@group(0) @binding(6) var<storage, read_write> capture_rgb: array<f32>;

fn channel(pos: vec2<i32>) -> u32 {
    let index = u32((pos.y & 1) * 2 + (pos.x & 1));
    switch p.source.y {
        case 0u: { return array<u32, 4>(0, 1, 1, 2)[index]; }
        case 1u: { return array<u32, 4>(2, 1, 1, 0)[index]; }
        case 2u: { return array<u32, 4>(1, 0, 2, 1)[index]; }
        default: { return array<u32, 4>(1, 2, 0, 1)[index]; }
    }
}

fn sample_at(pos: vec2<i32>) -> f32 {
    let tile = vec2<i32>(p.image.zw);
    let layer = (pos.y / tile.y) * i32(p.source.x) + pos.x / tile.x;
    return textureLoad(mosaic, pos % tile, layer, 0).x;
}

// Offset order matches the CPU reference's floating-point accumulation.
const CROSS = array<vec2<i32>, 4>(vec2(-1, 0), vec2(1, 0), vec2(0, -1), vec2(0, 1));
const DIAGONAL = array<vec2<i32>, 4>(vec2(-1, -1), vec2(1, -1), vec2(-1, 1), vec2(1, 1));
fn average(pos: vec2<i32>, kind: u32) -> f32 {
    var sum = 0.0;
    var count = 0u;
    for (var i = 0u; i < 4u; i++) {
        if (kind == 2u && i >= 2u) || (kind == 3u && i < 2u) { continue; }
        var offset = CROSS[i];
        if kind == 1u { offset = DIAGONAL[i]; }
        let neighbor = pos + offset;
        if all(neighbor >= vec2(0)) && all(neighbor < vec2<i32>(p.image.xy)) {
            sum += sample_at(neighbor);
            count++;
        }
    }
    if count == 0u { return sample_at(pos); }
    return sum / f32(count);
}

fn bilinear(pos: vec2<i32>) -> vec3<f32> {
    let measured = sample_at(pos);
    switch channel(pos) {
        case 0u: { return vec3(measured, average(pos, 0u), average(pos, 1u)); }
        case 2u: { return vec3(average(pos, 1u), average(pos, 0u), measured); }
        default: {
            let horizontal_red = channel(pos + vec2(1, 0)) == 0u;
            return vec3(average(pos, select(3u, 2u, horizontal_red)), measured,
                        average(pos, select(2u, 3u, horizontal_red)));
        }
    }
}

// Published MHC integer-over-16 kernels, identical to the CPU contract.
const GREEN = array<i32, 25>(0,0,-2,0,0, 0,0,4,0,0, -2,4,8,4,-2, 0,0,4,0,0, 0,0,-2,0,0);
const SAME_ROW = array<i32, 25>(0,0,1,0,0, 0,-2,0,-2,0, -2,8,10,8,-2, 0,-2,0,-2,0, 0,0,1,0,0);
const OPPOSITE = array<i32, 25>(0,0,-3,0,0, 0,4,0,4,0, -3,0,12,0,-3, 0,4,0,4,0, 0,0,-3,0,0);
fn kernel(pos: vec2<i32>, kind: u32, transpose: bool) -> f32 {
    var sum = 0.0;
    for (var y = 0u; y < 5u; y++) {
        for (var x = 0u; x < 5u; x++) {
            let index = select(y * 5u + x, x * 5u + y, transpose);
            var coefficient = GREEN[index];
            if kind == 1u { coefficient = SAME_ROW[index]; }
            if kind == 2u { coefficient = OPPOSITE[index]; }
            if coefficient != 0 {
                sum += f32(coefficient) * sample_at(pos + vec2<i32>(i32(x) - 2, i32(y) - 2));
            }
        }
    }
    return sum * (1.0 / 16.0);
}

fn reconstruct(pos: vec2<i32>) -> vec3<f32> {
    if p.source.z == 0u || any(pos < vec2(2)) || any(pos >= vec2<i32>(p.image.xy) - vec2(2)) {
        return bilinear(pos);
    }
    let measured = sample_at(pos);
    switch channel(pos) {
        case 0u: { return vec3(measured, kernel(pos, 0u, false), kernel(pos, 2u, false)); }
        case 2u: { return vec3(kernel(pos, 2u, false), kernel(pos, 0u, false), measured); }
        default: {
            let horizontal_red = channel(pos + vec2(1, 0)) == 0u;
            return vec3(kernel(pos, 1u, !horizontal_red), measured, kernel(pos, 1u, horizontal_red));
        }
    }
}

fn validate(rgb: vec3<f32>) {
    // Bit tests also catch NaNs on drivers that optimize floating comparisons.
    if any((bitcast<vec3<u32>>(rgb) & vec3(0x7f800000u)) == vec3(0x7f800000u)) {
        atomicOr(&invalid, 1u);
    }
}

@compute @workgroup_size(8, 8)
fn demosaic_planes(@builtin(global_invocation_id) id: vec3<u32>) {
    if any(id.xy >= p.region.zw) { return; }
    let pos = p.region.xy + id.xy;
    let rgb = reconstruct(vec2<i32>(pos));
    validate(rgb);
    let local = vec2<i32>(pos % p.destination.xy);
    let layer = i32((pos.y / p.destination.y) * p.destination.z + pos.x / p.destination.x);
    textureStore(red, local, layer, vec4(rgb.r, 0.0, 0.0, 1.0));
    textureStore(green, local, layer, vec4(rgb.g, 0.0, 0.0, 1.0));
    textureStore(blue, local, layer, vec4(rgb.b, 0.0, 0.0, 1.0));
}

@compute @workgroup_size(8, 8)
fn demosaic_tile(@builtin(global_invocation_id) id: vec3<u32>) {
    if any(id.xy >= p.region.zw) { return; }
    let rgb = reconstruct(vec2<i32>(p.region.xy + id.xy));
    validate(rgb);
    let index = (id.y * p.region.z + id.x) * 3u;
    capture_rgb[index] = rgb.r;
    capture_rgb[index + 1u] = rgb.g;
    capture_rgb[index + 2u] = rgb.b;
}
