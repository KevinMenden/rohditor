// Defringe on one halo-expanded tile of optics-corrected camera RGB; ports
// `rohditor_detail::remove_fringes` pass for pass (`defringe/` there
// documents the algorithm). Planes: 0 luma, 1 gradient / edge mask / weights,
// 2 and 3 scratch, later the replacement colour differences. Every dispatch
// writes only its own pixel, so the passes need no extra synchronisation.
struct Parameters {
    width: u32, height: u32, mode: u32, radius: u32,
    plane_a: u32, plane_b: u32, noise_enabled: u32, pad0: u32,
    purple_amount: f32, green_amount: f32, edge_fraction: f32, noise_sigmas: f32,
    purple_hue: vec2<f32>, green_hue: vec2<f32>,
    neutral_saturation: f32, trusted_saturation: f32, hue_rolloff: f32, pad1: f32,
    balance: vec4<f32>,
    ceilings: vec4<f32>,
    // Luma noise variance `a * x + b`.
    noise: vec4<f32>,
    rec2020_row0: vec4<f32>,
    rec2020_row1: vec4<f32>,
    rec2020_row2: vec4<f32>,
};
@group(0) @binding(0) var<storage, read_write> rgb: array<f32>;
@group(0) @binding(1) var<storage, read_write> planes: array<f32>;
@group(0) @binding(2) var<uniform> p: Parameters;

const LUMA = 0u;
const WEIGHT = 1u;
const FIRST_CHROMA = 2u;
const NEIGHBOURHOOD = 2;
const EPSILON = 1.1920929e-7;
const PI = 3.1415927;

const MODE_LUMA = 0u;
const MODE_GRADIENT = 1u;
const MODE_MAX_HORIZONTAL = 2u;
const MODE_MAX_VERTICAL = 3u;
const MODE_EDGE = 4u;
const MODE_WEIGHT = 5u;
const MODE_REPLACEMENT = 6u;
const MODE_BLEND = 7u;

fn finite3(v: vec3<f32>) -> bool {
    return all((bitcast<vec3<u32>>(v) & vec3<u32>(0x7f800000u)) != vec3<u32>(0x7f800000u));
}
fn pixel(i: u32) -> vec3<f32> {
    return vec3<f32>(rgb[3u * i], rgb[3u * i + 1u], rgb[3u * i + 2u]);
}
fn plane_at(plane: u32, i: u32) -> f32 {
    return planes[plane * p.width * p.height + i];
}
fn set_plane(plane: u32, i: u32, v: f32) {
    planes[plane * p.width * p.height + i] = v;
}
// Borders repeat the edge pixel.
fn clamped(i: i32, length: u32) -> u32 {
    return u32(clamp(i, 0, i32(length) - 1));
}
fn is_blown(v: vec3<f32>) -> bool {
    return any(v >= p.ceilings.xyz);
}

fn gradient(x: u32, y: u32) -> f32 {
    let l = clamped(i32(x) - 1, p.width);
    let r = clamped(i32(x) + 1, p.width);
    let up = clamped(i32(y) - 1, p.height) * p.width;
    let mid = y * p.width;
    let down = clamped(i32(y) + 1, p.height) * p.width;
    let gx = (plane_at(LUMA, down + r) - plane_at(LUMA, down + l))
        + 2.0 * (plane_at(LUMA, mid + r) - plane_at(LUMA, mid + l))
        + (plane_at(LUMA, up + r) - plane_at(LUMA, up + l));
    let gy = (plane_at(LUMA, down + l) + 2.0 * plane_at(LUMA, down + x) + plane_at(LUMA, down + r))
        - (plane_at(LUMA, up + l) + 2.0 * plane_at(LUMA, up + x) + plane_at(LUMA, up + r));
    return sqrt(gx * gx + gy * gy) * 0.25;
}

fn hue_window(hue: f32, range: vec2<f32>) -> f32 {
    if hue < range.x || hue > range.y { return 0.0; }
    let t = (hue - range.x) / (range.y - range.x);
    let edge = min(t, 1.0 - t) / p.hue_rolloff;
    if edge >= 1.0 { return 1.0; }
    return 0.5 * (1.0 - cos(PI * edge));
}
fn saturation_ramp(saturation: f32) -> f32 {
    let t = clamp((saturation - p.neutral_saturation) / (p.trusted_saturation - p.neutral_saturation), 0.0, 1.0);
    return t * t * (3.0 - 2.0 * t);
}

// Fringe weight of one pixel, 0 for blown and non-finite pixels.
fn pixel_weight(px: vec3<f32>) -> f32 {
    if !finite3(px) || is_blown(px) { return 0.0; }
    let balanced = px * p.balance.xyz;
    let wide = vec3<f32>(
        dot(p.rec2020_row0.xyz, balanced),
        dot(p.rec2020_row1.xyz, balanced),
        dot(p.rec2020_row2.xyz, balanced));
    if !finite3(wide) { return 0.0; }
    let offset = max(-min(wide.x, min(wide.y, wide.z)), 0.0);
    let c = wide + vec3<f32>(offset);
    let top = max(c.x, max(c.y, c.z));
    if top <= EPSILON { return 0.0; }
    let chroma = top - min(c.x, min(c.y, c.z));
    var hue = 0.0;
    var saturation = 0.0;
    if chroma > EPSILON {
        var sector = 0.0;
        if top == c.x {
            sector = (c.y - c.z) / chroma;
            sector = sector % 6.0;
            if sector < 0.0 { sector += 6.0; }
        } else if top == c.y {
            sector = (c.z - c.x) / chroma + 2.0;
        } else {
            sector = (c.x - c.y) / chroma + 4.0;
        }
        hue = sector * 60.0;
        saturation = chroma / top;
    }
    let membership = max(
        p.purple_amount * hue_window(hue, p.purple_hue),
        p.green_amount * hue_window(hue, p.green_hue));
    return membership * saturation_ramp(saturation);
}

// Balanced colour differences (r - g, b - g).
fn differences(px: vec3<f32>) -> vec2<f32> {
    let g = px.y * p.balance.y;
    return vec2<f32>(px.x * p.balance.x - g, px.z * p.balance.z - g);
}

fn replacement(x: u32, y: u32) -> vec2<f32> {
    var best = vec2<f32>(0.0);
    var least = 3.4e38;
    var found = false;
    for (var dy = -NEIGHBOURHOOD; dy <= NEIGHBOURHOOD; dy++) {
        let ny = clamped(i32(y) + dy, p.height);
        // Rows and columns past the border repeat; the CPU visits each
        // in-range neighbour once, so skip repeats.
        if i32(y) + dy != i32(ny) { continue; }
        for (var dx = -NEIGHBOURHOOD; dx <= NEIGHBOURHOOD; dx++) {
            let nx = clamped(i32(x) + dx, p.width);
            if i32(x) + dx != i32(nx) { continue; }
            let q = ny * p.width + nx;
            if plane_at(WEIGHT, q) > 0.0 { continue; }
            let px = pixel(q);
            if is_blown(px) { continue; }
            let d = differences(px);
            if !finite3(vec3<f32>(d, 0.0)) { continue; }
            let energy = d.x * d.x + d.y * d.y;
            if !found || energy < least {
                found = true;
                least = energy;
                best = d;
            }
        }
    }
    return best;
}

@compute @workgroup_size(8, 8)
fn defringe(@builtin(global_invocation_id) id: vec3<u32>) {
    if id.x >= p.width || id.y >= p.height { return; }
    let i = id.y * p.width + id.x;
    switch p.mode {
        case MODE_LUMA: {
            let v = pixel(i) * p.balance.xyz;
            var luma = (v.x + 2.0 * v.y + v.z) * 0.25;
            if !finite3(vec3<f32>(luma, 0.0, 0.0)) { luma = 0.0; }
            set_plane(LUMA, i, luma);
        }
        case MODE_GRADIENT: { set_plane(WEIGHT, i, gradient(id.x, id.y)); }
        // Maximum over a (2r+1) window along one axis, plane_a into plane_b.
        case MODE_MAX_HORIZONTAL: {
            var best = plane_at(p.plane_a, i);
            for (var k = 1; k <= i32(p.radius); k++) {
                best = max(best, plane_at(p.plane_a, id.y * p.width + clamped(i32(id.x) - k, p.width)));
                best = max(best, plane_at(p.plane_a, id.y * p.width + clamped(i32(id.x) + k, p.width)));
            }
            set_plane(p.plane_b, i, best);
        }
        case MODE_MAX_VERTICAL: {
            var best = plane_at(p.plane_a, i);
            for (var k = 1; k <= i32(p.radius); k++) {
                best = max(best, plane_at(p.plane_a, clamped(i32(id.y) - k, p.height) * p.width + id.x));
                best = max(best, plane_at(p.plane_a, clamped(i32(id.y) + k, p.height) * p.width + id.x));
            }
            set_plane(p.plane_b, i, best);
        }
        // Gradient (plane 1) against the local maximum (plane 3).
        case MODE_EDGE: {
            let g = plane_at(WEIGHT, i);
            var floor_value = 0.0;
            if p.noise_enabled != 0u {
                let variance = p.noise.x * max(plane_at(LUMA, i), 0.0) + p.noise.y;
                floor_value = p.noise_sigmas * p.noise_sigmas * variance;
            }
            var edge = 0.0;
            if g > 0.0 && g >= p.edge_fraction * plane_at(3u, i) && g * g >= floor_value { edge = 1.0; }
            set_plane(WEIGHT, i, edge);
        }
        // Dilated mask (plane 3) selects where the hue test applies.
        case MODE_WEIGHT: {
            var weight = 0.0;
            if plane_at(3u, i) > 0.0 { weight = pixel_weight(pixel(i)); }
            set_plane(WEIGHT, i, weight);
        }
        case MODE_REPLACEMENT: {
            if plane_at(WEIGHT, i) > 0.0 {
                let d = replacement(id.x, id.y);
                set_plane(FIRST_CHROMA, i, d.x);
                set_plane(FIRST_CHROMA + 1u, i, d.y);
            }
        }
        case MODE_BLEND: {
            let weight = plane_at(WEIGHT, i);
            if weight > 0.0 {
                let px = pixel(i);
                let old = differences(px);
                if finite3(vec3<f32>(old, 0.0)) {
                    let b = px * p.balance.xyz;
                    let luma = (b.x + 2.0 * b.y + b.z) * 0.25;
                    let d = old + weight * (vec2<f32>(plane_at(FIRST_CHROMA, i), plane_at(FIRST_CHROMA + 1u, i)) - old);
                    let g = luma - (d.x + d.y) * 0.25;
                    let rebuilt = vec3<f32>((g + d.x) / p.balance.x, g / p.balance.y, (g + d.y) / p.balance.z);
                    if finite3(rebuilt) {
                        rgb[3u * i] = rebuilt.x; rgb[3u * i + 1u] = rebuilt.y; rgb[3u * i + 2u] = rebuilt.z;
                    }
                }
            }
        }
        default: {}
    }
}
