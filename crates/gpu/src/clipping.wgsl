// Colours the pixels of a display image that reached either end of the range.
// The rule of rohditor_core::ClippingMask: one channel at exactly 0 or 255 is enough.

struct Tints {
    highlight: vec4<f32>,
    shadow: vec4<f32>,
}

@group(0) @binding(0) var display: texture_2d<f32>;
@group(0) @binding(1) var tint: texture_storage_2d<rgba8unorm, write>;
@group(0) @binding(2) var<uniform> tints: Tints;

@compute @workgroup_size(8, 8)
fn main(@builtin(global_invocation_id) id: vec3<u32>) {
    if any(id.xy >= textureDimensions(display)) {
        return;
    }
    let levels = round(textureLoad(display, id.xy, 0).rgb * 255.0);
    var color = vec4<f32>(0.0);
    if any(levels >= vec3<f32>(255.0)) {
        color = tints.highlight;
    } else if any(levels <= vec3<f32>(0.0)) {
        color = tints.shadow;
    }
    textureStore(tint, id.xy, color);
}
