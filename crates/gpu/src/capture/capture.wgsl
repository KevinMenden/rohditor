// Six scalar f32 planes: guide, mask, estimate, temporary, blurred, ratio.
struct Parameters {
    width: u32, height: u32, mode: u32, radius: u32,
    input_plane: u32, output_plane: u32, axis: u32, unused: u32,
    amount: f32, noise: f32, floor_value: f32, pad: f32,
    ceilings: vec4<f32>,
};
@group(0) @binding(0) var<storage, read_write> rgb: array<f32>;
@group(0) @binding(1) var<storage, read_write> planes: array<f32>;
@group(0) @binding(2) var<storage, read> weights: array<f32>;
@group(0) @binding(3) var<uniform> p: Parameters;

fn mirror(i: i32, length: u32) -> u32 {
    let period = i32(2u * length);
    let folded = u32(((i % period) + period) % period);
    if folded < length { return folded; }
    return 2u * length - 1u - folded;
}
fn fade(low: f32, high: f32, v: f32) -> f32 {
    let t = clamp((v - low) / (high - low), 0.0, 1.0);
    return t * t * (3.0 - 2.0 * t);
}
@compute @workgroup_size(8, 8)
fn capture(@builtin(global_invocation_id) id: vec3<u32>) {
    if id.x >= p.width || id.y >= p.height { return; }
    let n = p.width * p.height;
    let i = id.y * p.width + id.x;
    if p.mode == 0u {
        let r = rgb[3u*i]; let g = rgb[3u*i+1u]; let b = rgb[3u*i+2u];
        let guide = max(max(r, 0.0)/3.0 + max(g, 0.0)/3.0 + max(b, 0.0)/3.0, p.floor_value);
        planes[i] = guide;
        planes[2u*n+i] = guide;
        let peak = max(0.0, max(r/p.ceilings.x, max(g/p.ceilings.y, b/p.ceilings.z)));
        planes[n+i] = 1.0 - fade(0.85, 0.98, peak);
    } else if p.mode == 1u {
        var sum = 0.0;
        for(var k = 0u; k < 2u*p.radius+1u; k++) {
            var x = id.x; var y = id.y;
            if p.axis == 0u { x = mirror(i32(x)+i32(k)-i32(p.radius), p.width); }
            else { y = mirror(i32(y)+i32(k)-i32(p.radius), p.height); }
            sum += planes[p.input_plane*n+y*p.width+x] * weights[k];
        }
        planes[p.output_plane*n+i] = sum;
    } else if p.mode == 2u {
        let b = planes[4u*n+i];
        planes[n+i] = min(planes[n+i], (b*b)*(b*b));
    } else if p.mode == 3u {
        let guide = planes[i]; let b = planes[4u*n+i];
        let threshold = 0.0005 + p.noise*0.008 + b*p.noise*0.02;
        planes[n+i] *= fade(threshold, threshold*3.0, abs(guide-b)) * fade(0.001, 0.01, guide);
    } else if p.mode == 4u {
        planes[5u*n+i] = planes[i] / max(planes[4u*n+i], p.floor_value);
    } else if p.mode == 5u {
        planes[2u*n+i] = clamp(planes[2u*n+i]*planes[4u*n+i], planes[i]*0.5, planes[i]*2.0);
    } else {
        let gain = 1.0 + p.amount*planes[n+i]*(planes[2u*n+i]/planes[i]-1.0);
        let value = vec3<f32>(rgb[3u*i], rgb[3u*i+1u], rgb[3u*i+2u]);
        let result = value * gain;
        // Match CPU's common finite-product guard; do not clip signed/HDR RGB.
        let bits = bitcast<vec3<u32>>(result) & vec3<u32>(0x7f800000u);
        if gain != 1.0 && all(bits != vec3<u32>(0x7f800000u)) {
            rgb[3u*i] = result.x; rgb[3u*i+1u] = result.y; rgb[3u*i+2u] = result.z;
        }
    }
}
