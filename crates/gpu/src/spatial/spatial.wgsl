struct OpticsParameters {
    source_width: u32,
    source_height: u32,
    tile_width: u32,
    tile_height: u32,
    tile_columns: u32,
    distortion_model: u32,
    tca_model: u32,
    vignetting_model: u32,
    norm_scale: f32,
    norm_unscale: f32,
    center_x: f32,
    center_y: f32,
    automatic_scale: f32,
    _padding0: u32,
    _padding1: u32,
    _padding2: u32,
    distortion: vec4<f32>,
    tca_red: vec4<f32>,
    tca_blue: vec4<f32>,
    vignetting: vec4<f32>,
};

struct BandParameters {
    source_first_row: u32,
    source_row_count: u32,
    target_first_row: u32,
    target_row_count: u32,
};

@group(2) @binding(0) var camera_red: texture_2d_array<f32>;
@group(2) @binding(1) var camera_green: texture_2d_array<f32>;
@group(2) @binding(2) var camera_blue: texture_2d_array<f32>;
@group(2) @binding(3) var<uniform> optics: OpticsParameters;
@group(2) @binding(4) var<storage, read_write> spatial_failure: atomic<u32>;

@group(1) @binding(0) var<storage, read> horizontal_samples: array<vec4<u32>>;
@group(1) @binding(1) var<storage, read> horizontal_weights: array<f32>;
@group(1) @binding(2) var<storage, read_write> horizontal_scratch: array<f32>;
@group(1) @binding(3) var<uniform> band: BandParameters;
@group(1) @binding(4) var<storage, read> vertical_samples: array<vec4<u32>>;
@group(1) @binding(5) var<storage, read> vertical_weights: array<f32>;
@group(1) @binding(6) var reduced_camera: texture_storage_2d<rgba32float, write>;

fn finite_scalar(value: f32) -> bool {
    return value == value && abs(value) <= 3.402823e38;
}

fn mark_spatial_failure() {
    atomicStore(&spatial_failure, 1u);
}

fn camera_plane_value(coordinate: vec2<u32>, channel: u32) -> f32 {
    let tile = coordinate / vec2<u32>(optics.tile_width, optics.tile_height);
    let local = coordinate % vec2<u32>(optics.tile_width, optics.tile_height);
    let layer = tile.y * optics.tile_columns + tile.x;
    switch channel {
        case 0u: { return textureLoad(camera_red, vec2<i32>(local), i32(layer), 0).r; }
        case 1u: { return textureLoad(camera_green, vec2<i32>(local), i32(layer), 0).r; }
        default: { return textureLoad(camera_blue, vec2<i32>(local), i32(layer), 0).r; }
    }
}

fn vignetting_multiplier(coordinate: vec2<u32>) -> f32 {
    if optics.vignetting_model == 0u {
        return 1.0;
    }
    let x = -optics.center_x + optics.norm_scale * f32(coordinate.x);
    let y = -optics.center_y + optics.norm_scale * f32(coordinate.y);
    let radius2 = x * x + y * y;
    let radius4 = radius2 * radius2;
    let radius6 = radius4 * radius2;
    let gain = 1.0 + optics.vignetting.x * radius2
        + optics.vignetting.y * radius4 + optics.vignetting.z * radius6;
    if !finite_scalar(gain) || gain == 0.0 {
        mark_spatial_failure();
        return 0.0;
    }
    return 1.0 / gain;
}

fn lattice_value(coordinate: vec2<u32>, channel: u32) -> f32 {
    let value = camera_plane_value(coordinate, channel) * vignetting_multiplier(coordinate);
    if !finite_scalar(value) {
        mark_spatial_failure();
        return 0.0;
    }
    return value;
}

fn inverse_poly3(point: vec2<f32>, k1: f32) -> vec2<f32> {
    let inverse_k1 = 1.0 / k1;
    let rd = length(point);
    if rd == 0.0 { return point; }
    let rd_div_k1 = rd * inverse_k1;
    var ru = rd;
    var converged = false;
    for (var step = 0u; step < 7u; step += 1u) {
        let value = ru * ru * ru + ru * inverse_k1 - rd_div_k1;
        if abs(value) < 0.00001 {
            converged = true;
            break;
        }
        ru -= value / (3.0 * ru * ru + inverse_k1);
    }
    if !converged || ru < 0.0 {
        return vec2<f32>(bitcast<f32>(0x7fc00000u));
    }
    return point * (ru / rd);
}

fn inverse_poly5(point: vec2<f32>, k1: f32, k2: f32) -> vec2<f32> {
    let rd = length(point);
    if rd == 0.0 { return point; }
    var ru = rd;
    var converged = false;
    for (var step = 0u; step < 7u; step += 1u) {
        let ru2 = ru * ru;
        let value = ru * (1.0 + k1 * ru2 + k2 * ru2 * ru2) - rd;
        if abs(value) < 0.00001 {
            converged = true;
            break;
        }
        ru -= value / (1.0 + 3.0 * k1 * ru2 + 5.0 * k2 * ru2 * ru2);
    }
    if !converged || ru < 0.0 { return point; }
    return point * (ru / rd);
}

fn inverse_ptlens(point: vec2<f32>, coefficients: vec3<f32>) -> vec2<f32> {
    let rd = length(point);
    if rd == 0.0 { return point; }
    let a = coefficients.x;
    let b = coefficients.y;
    let c = coefficients.z;
    var ru = rd;
    var converged = false;
    for (var step = 0u; step < 7u; step += 1u) {
        let value = ru * (a * ru * ru * ru + b * ru * ru + c * ru + 1.0) - rd;
        if abs(value) < 0.00001 {
            converged = true;
            break;
        }
        ru -= value / (4.0 * a * ru * ru * ru + 3.0 * b * ru * ru + 2.0 * c * ru + 1.0);
    }
    if !converged || ru < 0.0 { return point; }
    return point * (ru / rd);
}

fn apply_distortion(point: vec2<f32>) -> vec2<f32> {
    switch optics.distortion_model {
        case 1u: { return inverse_poly3(point, optics.distortion.x); }
        case 2u: { return inverse_poly5(point, optics.distortion.x, optics.distortion.y); }
        case 3u: { return inverse_ptlens(point, optics.distortion.xyz); }
        default: { return point; }
    }
}

fn inverse_tca_poly3(point: vec2<f32>, coefficients: vec3<f32>) -> vec2<f32> {
    let rd = length(point);
    if rd == 0.0 { return point; }
    let v = coefficients.x;
    let c = coefficients.y;
    let b = coefficients.z;
    var ru = rd;
    var converged = false;
    for (var step = 0u; step < 7u; step += 1u) {
        let ru2 = ru * ru;
        let value = b * ru2 * ru + c * ru2 + v * ru - rd;
        if abs(value) < 0.00001 {
            converged = true;
            break;
        }
        ru -= value / (3.0 * b * ru2 + 2.0 * c * ru + v);
    }
    if !converged || ru <= 0.0 { return point; }
    return point * (ru / rd);
}

fn channel_normalized(point: vec2<f32>, channel: u32) -> vec2<f32> {
    if optics.tca_model == 1u {
        if channel == 0u { return point * optics.tca_red.x; }
        if channel == 2u { return point * optics.tca_blue.x; }
    } else if optics.tca_model == 2u {
        if channel == 0u { return inverse_tca_poly3(point, optics.tca_red.xyz); }
        if channel == 2u { return inverse_tca_poly3(point, optics.tca_blue.xyz); }
    }
    return point;
}

fn mapped_coordinate(output: vec2<u32>, channel: u32) -> vec2<f32> {
    let center_pixel = vec2<f32>(optics.center_x, optics.center_y) * optics.norm_unscale;
    let mapped = center_pixel + (vec2<f32>(output) - center_pixel) / optics.automatic_scale;
    let normalized = mapped * optics.norm_scale - vec2<f32>(optics.center_x, optics.center_y);
    let geometry = apply_distortion(normalized);
    let channel_point = channel_normalized(geometry, channel);
    return (channel_point + vec2<f32>(optics.center_x, optics.center_y)) * optics.norm_unscale;
}

fn cubic_weight(distance: f32) -> f32 {
    let value = abs(distance);
    if value <= 1.0 {
        return 1.5 * value * value * value - 2.5 * value * value + 1.0;
    }
    if value < 2.0 {
        return -0.5 * value * value * value + 2.5 * value * value - 4.0 * value + 2.0;
    }
    return 0.0;
}

fn corrected_channel(output: vec2<u32>, channel: u32) -> f32 {
    if optics.distortion_model == 0u && optics.tca_model == 0u {
        return lattice_value(output, channel);
    }
    let coordinate = mapped_coordinate(output, channel);
    if !finite_scalar(coordinate.x) || !finite_scalar(coordinate.y) {
        mark_spatial_failure();
        return 0.0;
    }
    let base = vec2<i32>(floor(coordinate));
    if base.x - 1 < 0 || base.y - 1 < 0
        || base.x + 2 >= i32(optics.source_width)
        || base.y + 2 >= i32(optics.source_height) {
        mark_spatial_failure();
        return 0.0;
    }
    var value = 0.0;
    for (var oy = -1; oy <= 2; oy += 1) {
        let wy = cubic_weight(coordinate.y - f32(base.y + oy));
        for (var ox = -1; ox <= 2; ox += 1) {
            let source = vec2<u32>(base + vec2<i32>(ox, oy));
            value += lattice_value(source, channel)
                * wy * cubic_weight(coordinate.x - f32(base.x + ox));
        }
    }
    if !finite_scalar(value) {
        mark_spatial_failure();
        return 0.0;
    }
    return value;
}

@compute @workgroup_size(16, 16, 1)
fn materialize_optics(@builtin(global_invocation_id) invocation: vec3<u32>) {
    if invocation.x >= textureDimensions(reduced_camera).x
        || invocation.y >= textureDimensions(reduced_camera).y {
        return;
    }
    let camera = vec3<f32>(
        corrected_channel(invocation.xy, 0u),
        corrected_channel(invocation.xy, 1u),
        corrected_channel(invocation.xy, 2u),
    );
    textureStore(reduced_camera, vec2<i32>(invocation.xy), vec4<f32>(camera, 1.0));
}

@compute @workgroup_size(16, 8, 1)
fn reduce_horizontal(@builtin(global_invocation_id) invocation: vec3<u32>) {
    if invocation.x >= arrayLength(&horizontal_samples)
        || invocation.y >= band.source_row_count {
        return;
    }
    let output_x = invocation.x;
    let source_y = invocation.y + band.source_first_row;
    let sample = horizontal_samples[output_x];
    let scratch_pixel = (invocation.y * arrayLength(&horizontal_samples) + output_x) * 3u;
    for (var channel = 0u; channel < 3u; channel += 1u) {
        var value = 0.0;
        for (var offset = 0u; offset < sample.z; offset += 1u) {
            value += corrected_channel(vec2<u32>(sample.x + offset, source_y), channel)
                * horizontal_weights[sample.y + offset];
        }
        horizontal_scratch[scratch_pixel + channel] = value;
    }
}

@compute @workgroup_size(16, 8, 1)
fn reduce_vertical(@builtin(global_invocation_id) invocation: vec3<u32>) {
    if invocation.x >= textureDimensions(reduced_camera).x
        || invocation.y >= band.target_row_count {
        return;
    }
    let target_y = invocation.y + band.target_first_row;
    let sample = vertical_samples[target_y];
    var value = vec3<f32>(0.0);
    for (var offset = 0u; offset < sample.z; offset += 1u) {
        let source_y = sample.x + offset - band.source_first_row;
        let scratch = (source_y * textureDimensions(reduced_camera).x + invocation.x) * 3u;
        let weight = vertical_weights[sample.y + offset];
        value += vec3<f32>(
            horizontal_scratch[scratch],
            horizontal_scratch[scratch + 1u],
            horizontal_scratch[scratch + 2u],
        ) * weight;
    }
    if !finite_scalar(value.r) || !finite_scalar(value.g) || !finite_scalar(value.b) {
        mark_spatial_failure();
        value = vec3<f32>(0.0);
    }
    textureStore(reduced_camera, vec2<i32>(invocation.xy + vec2<u32>(0u, band.target_first_row)), vec4<f32>(value, 1.0));
}
