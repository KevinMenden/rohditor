// Source-resolution detail passes on one halo-expanded camera RGB tile:
// false-colour suppression, chroma and luminance noise reduction. They port
// `rohditor-detail` pass for pass; the scratch `planes` buffer holds `n`
// scalar planes of tile size, laid out per step as described at each mode.
// Every dispatch writes only its own pixel's outputs, so passes in one
// compute pass need no extra synchronisation.
struct Parameters {
    width: u32, height: u32, mode: u32, step: u32,
    plane_a: u32, plane_b: u32, plane_c: u32, plane_d: u32,
    scale: f32, noise_gain_squared: f32, range_sigmas: f32, signal_floor: f32,
    floor_value: f32, max_gain: f32, pad0: f32, pad1: f32,
    balance: vec4<f32>,
    ceilings: vec4<f32>,
    // Noise variance `a * x + b` of luma and of the two colour differences.
    luma_noise: vec4<f32>,
    chroma_noise_1: vec4<f32>,
    chroma_noise_2: vec4<f32>,
};
@group(0) @binding(0) var<storage, read_write> rgb: array<f32>;
@group(0) @binding(1) var<storage, read_write> planes: array<f32>;
@group(0) @binding(2) var<uniform> p: Parameters;

const FALSE_COLOR_SPLIT = 0u;
const FALSE_COLOR_MEDIAN = 1u;
const FALSE_COLOR_WRITE = 2u;
const CHROMA_SPLIT = 3u;
const CHROMA_LEVEL = 4u;
const CHROMA_WRITE = 5u;
const LUMA_INIT = 6u;
const LUMA_SMOOTH_HORIZONTAL = 7u;
const LUMA_SMOOTH_VERTICAL = 8u;
const LUMA_ENERGY_HORIZONTAL = 9u;
const LUMA_ENERGY_VERTICAL = 10u;
const LUMA_SHRINK = 11u;
const LUMA_WRITE = 12u;

// Chroma plane layout: luma, two ping-pong colour pairs, accumulated pair.
const CHROMA_LUMA = 0u;
const CHROMA_ACCUMULATED = 5u;
// Luma plane layout: two ping-pong planes, accumulated, energy, temporary.
const LUMA_ACCUMULATED = 2u;
const LUMA_ENERGY = 3u;
const LUMA_TEMPORARY = 4u;

// B3 spline [1, 4, 6, 4, 1] / 16 and the chroma binomial [1, 2, 1] / 4.
const B3 = array<f32, 5>(0.0625, 0.25, 0.375, 0.25, 0.0625);
const BINOMIAL = array<f32, 3>(0.25, 0.5, 0.25);
const ENERGY_RADIUS = 3;

fn mirror(i: i32, length: u32) -> u32 {
    let period = i32(2u * length);
    let folded = u32(((i % period) + period) % period);
    if folded < length { return folded; }
    return 2u * length - 1u - folded;
}
fn finite3(v: vec3<f32>) -> bool {
    return all((bitcast<vec3<u32>>(v) & vec3<u32>(0x7f800000u)) != vec3<u32>(0x7f800000u));
}
fn soft_threshold(value: f32, threshold: f32) -> f32 {
    return sign(value) * max(abs(value) - threshold, 0.0);
}
fn pixel(i: u32) -> vec3<f32> {
    return vec3<f32>(rgb[3u * i], rgb[3u * i + 1u], rgb[3u * i + 2u]);
}
fn store_pixel(i: u32, v: vec3<f32>) {
    rgb[3u * i] = v.x; rgb[3u * i + 1u] = v.y; rgb[3u * i + 2u] = v.z;
}
// Pixels the steps must not touch: non-finite or at/above the ceilings.
fn untouched(v: vec3<f32>) -> bool {
    return !finite3(v) || any(v >= p.ceilings.xyz);
}
fn plane_at(plane: u32, i: u32) -> f32 {
    return planes[plane * p.width * p.height + i];
}
fn set_plane(plane: u32, i: u32, v: f32) {
    planes[plane * p.width * p.height + i] = v;
}

// Median of nine, as the CPU does it: sort columns, then combine ranks.
fn min3(a: f32, b: f32, c: f32) -> f32 { return min(min(a, b), c); }
fn max3(a: f32, b: f32, c: f32) -> f32 { return max(max(a, b), c); }
fn median3(a: f32, b: f32, c: f32) -> f32 { return max(min(a, b), min(max(a, b), c)); }
fn median9(plane: u32, x: u32, y: u32) -> f32 {
    var low = vec3<f32>(0.0);
    var mid = vec3<f32>(0.0);
    var high = vec3<f32>(0.0);
    for (var k = 0; k < 3; k++) {
        let sx = mirror(i32(x) + k - 1, p.width);
        let a = plane_at(plane, mirror(i32(y) - 1, p.height) * p.width + sx);
        let b = plane_at(plane, y * p.width + sx);
        let c = plane_at(plane, mirror(i32(y) + 1, p.height) * p.width + sx);
        low[k] = min3(a, b, c);
        mid[k] = median3(a, b, c);
        high[k] = max3(a, b, c);
    }
    return median3(
        max3(low.x, low.y, low.z),
        median3(mid.x, mid.y, mid.z),
        min3(high.x, high.y, high.z));
}

// `exp(-t)` as (1 - t/8)^8 clamped to zero, as on the CPU.
fn fast_exp_neg(t: f32) -> f32 {
    var x = max(1.0 - t * 0.125, 0.0);
    x = x * x;
    x = x * x;
    return x * x;
}
fn noise_sigma(noise: vec4<f32>, brightness: f32) -> f32 {
    return sqrt(noise.x * max(brightness, 0.0) + noise.y);
}

fn chroma_level(x: u32, y: u32, i: u32) {
    let current = 1u + 2u * p.plane_a;
    let smoothed = 1u + 2u * (1u - p.plane_a);
    let centre = plane_at(CHROMA_LUMA, i);
    let range = max(noise_sigma(p.luma_noise, centre), 1.0e-6) * p.range_sigmas;
    let inv = 1.0 / (range * range);
    var sum1 = 0.0; var sum2 = 0.0; var weight_sum = 0.0;
    for (var j = 0; j < 3; j++) {
        let sy = mirror(i32(y) + (j - 1) * i32(p.step), p.height);
        for (var k = 0; k < 3; k++) {
            let sx = mirror(i32(x) + (k - 1) * i32(p.step), p.width);
            let q = sy * p.width + sx;
            let d = plane_at(CHROMA_LUMA, q) - centre;
            let weight = BINOMIAL[j] * BINOMIAL[k] * fast_exp_neg(d * d * inv);
            sum1 += weight * plane_at(current, q);
            sum2 += weight * plane_at(current + 1u, q);
            weight_sum += weight;
        }
    }
    let s1 = sum1 / weight_sum;
    let s2 = sum2 / weight_sum;
    set_plane(smoothed, i, s1);
    set_plane(smoothed + 1u, i, s2);
    let t1 = p.scale * noise_sigma(p.chroma_noise_1, centre);
    let t2 = p.scale * noise_sigma(p.chroma_noise_2, centre);
    set_plane(CHROMA_ACCUMULATED, i,
        plane_at(CHROMA_ACCUMULATED, i) + soft_threshold(plane_at(current, i) - s1, t1));
    set_plane(CHROMA_ACCUMULATED + 1u, i,
        plane_at(CHROMA_ACCUMULATED + 1u, i) + soft_threshold(plane_at(current + 1u, i) - s2, t2));
}

// Horizontal or vertical pass of the dilated B3 smoothing of `input`.
fn smooth_axis(input: u32, x: u32, y: u32, horizontal: bool) -> f32 {
    var sum = 0.0;
    for (var k = 0; k < 5; k++) {
        let offset = (k - 2) * i32(p.step);
        var sx = x; var sy = y;
        if horizontal { sx = mirror(i32(x) + offset, p.width); }
        else { sy = mirror(i32(y) + offset, p.height); }
        sum += B3[k] * plane_at(input, sy * p.width + sx);
    }
    return sum;
}

// Sum of squares of `input` over a window of 2 * ENERGY_RADIUS + 1 along one axis.
fn energy_axis(input: u32, x: u32, y: u32, horizontal: bool, square: bool) -> f32 {
    var sum = 0.0;
    for (var k = -ENERGY_RADIUS; k <= ENERGY_RADIUS; k++) {
        var sx = x; var sy = y;
        if horizontal { sx = mirror(i32(x) + k, p.width); }
        else { sy = mirror(i32(y) + k, p.height); }
        let v = plane_at(input, sy * p.width + sx);
        if square { sum += v * v; } else { sum += v; }
    }
    return sum;
}

// BayesShrink-style threshold of a band pixel (see `LevelShrinkage`).
fn luma_threshold(brightness: f32, energy: f32) -> f32 {
    let noise_variance = (p.luma_noise.x * max(brightness, 0.0) + p.luma_noise.y) * p.noise_gain_squared;
    let floor = p.signal_floor * p.signal_floor * noise_variance;
    let sigma_signal = sqrt(max(energy - noise_variance, floor));
    if sigma_signal > 0.0 { return p.scale * noise_variance / sigma_signal; }
    return 0.0;
}

@compute @workgroup_size(8, 8)
fn detail(@builtin(global_invocation_id) id: vec3<u32>) {
    if id.x >= p.width || id.y >= p.height { return; }
    let i = id.y * p.width + id.x;
    switch p.mode {
        // Colour differences r - g, b - g of the balanced pixel into planes a, b.
        case FALSE_COLOR_SPLIT: {
            let v = pixel(i) * p.balance.xyz;
            set_plane(p.plane_a, i, v.x - v.y);
            set_plane(p.plane_b, i, v.z - v.y);
        }
        // 3x3 median of planes a, b into planes c, d.
        case FALSE_COLOR_MEDIAN: {
            set_plane(p.plane_c, i, median9(p.plane_a, id.x, id.y));
            set_plane(p.plane_d, i, median9(p.plane_b, id.x, id.y));
        }
        // Rebuild red and blue from planes a, b; green is kept.
        case FALSE_COLOR_WRITE: {
            let v = pixel(i);
            if !untouched(v) {
                let g = v.y * p.balance.y;
                let r = (g + plane_at(p.plane_a, i)) / p.balance.x;
                let b = (g + plane_at(p.plane_b, i)) / p.balance.z;
                if finite3(vec3<f32>(r, b, 0.0)) {
                    rgb[3u * i] = r; rgb[3u * i + 2u] = b;
                }
            }
        }
        // Luma and colour differences into planes 0..3; clear the accumulators.
        case CHROMA_SPLIT: {
            let v = pixel(i) * p.balance.xyz;
            set_plane(CHROMA_LUMA, i, (v.x + 2.0 * v.y + v.z) * 0.25);
            set_plane(1u, i, v.x - v.y);
            set_plane(2u, i, v.z - v.y);
            set_plane(CHROMA_ACCUMULATED, i, 0.0);
            set_plane(CHROMA_ACCUMULATED + 1u, i, 0.0);
        }
        case CHROMA_LEVEL: { chroma_level(id.x, id.y, i); }
        // Residual pair `plane_a` joins the accumulated details.
        case CHROMA_WRITE: {
            let v = pixel(i);
            if !untouched(v) {
                let residual = 1u + 2u * p.plane_a;
                let c1 = plane_at(CHROMA_ACCUMULATED, i) + plane_at(residual, i);
                let c2 = plane_at(CHROMA_ACCUMULATED + 1u, i) + plane_at(residual + 1u, i);
                let g = plane_at(CHROMA_LUMA, i) - (c1 + c2) * 0.25;
                let out = vec3<f32>((g + c1) / p.balance.x, g / p.balance.y, (g + c2) / p.balance.z);
                if finite3(out) { store_pixel(i, out); }
            }
        }
        case LUMA_INIT: {
            let v = pixel(i) * p.balance.xyz;
            var luma = (v.x + 2.0 * v.y + v.z) * 0.25;
            if !finite3(vec3<f32>(luma, 0.0, 0.0)) { luma = 0.0; }
            set_plane(0u, i, luma);
            set_plane(LUMA_ACCUMULATED, i, 0.0);
        }
        // Plane `plane_a` is the level input; the other ping-pong plane the output.
        case LUMA_SMOOTH_HORIZONTAL: {
            set_plane(LUMA_TEMPORARY, i, smooth_axis(p.plane_a, id.x, id.y, true));
        }
        // Smoothed result into the other plane; the input becomes the band.
        case LUMA_SMOOTH_VERTICAL: {
            let smoothed = smooth_axis(LUMA_TEMPORARY, id.x, id.y, false);
            set_plane(1u - p.plane_a, i, smoothed);
            set_plane(p.plane_a, i, plane_at(p.plane_a, i) - smoothed);
        }
        case LUMA_ENERGY_HORIZONTAL: {
            set_plane(LUMA_TEMPORARY, i, energy_axis(p.plane_a, id.x, id.y, true, true));
        }
        case LUMA_ENERGY_VERTICAL: {
            let window = f32(2 * ENERGY_RADIUS + 1);
            set_plane(LUMA_ENERGY, i,
                energy_axis(LUMA_TEMPORARY, id.x, id.y, false, false) / (window * window));
        }
        case LUMA_SHRINK: {
            let threshold = luma_threshold(plane_at(1u - p.plane_a, i), plane_at(LUMA_ENERGY, i));
            set_plane(LUMA_ACCUMULATED, i,
                plane_at(LUMA_ACCUMULATED, i) + soft_threshold(plane_at(p.plane_a, i), threshold));
        }
        // Plane `plane_a` holds the residual; scale RGB to the filtered luma.
        case LUMA_WRITE: {
            let v = pixel(i);
            if !untouched(v) {
                let balanced = v * p.balance.xyz;
                let old = (balanced.x + 2.0 * balanced.y + balanced.z) * 0.25;
                if old > p.floor_value {
                    let filtered = plane_at(LUMA_ACCUMULATED, i) + plane_at(p.plane_a, i);
                    let gain = clamp(filtered / old, 0.0, p.max_gain);
                    let out = v * gain;
                    if finite3(out) { store_pixel(i, out); }
                }
            }
        }
        default: {}
    }
}
