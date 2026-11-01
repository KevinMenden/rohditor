// Export with resize and/or output sharpening, in bounded bands of output
// rows. Appended to the shared colour kernels and `spatial.wgsl`; one entry
// point, with a `mode` per pass (a band runs several passes in order):
//
//   resize:   REDUCE_HORIZONTAL (develop source pixels, area-reduce columns)
//             REDUCE_VERTICAL   (area-reduce rows, encode to sRGB floats)
//   no resize: ENCODE           (develop, encode)
//   sharpen:  LUMA, BLUR_HORIZONTAL, then SHARPEN (vertical blur, soft
//             unsharp mask limited to the 3x3 luma range, quantize)
//   otherwise: QUANTIZE
//
// Area reduction runs on developed linear Rec.2020 and sharpening on the
// transfer-encoded floats, as on the CPU (`export_resize`, `output_sharpen`).
// A band's buffers hold `band_rows` rows from real output row `band_first`;
// with sharpening they extend past the rows being written (`core_*`) by the
// blur radius so no pass needs data from another band.
//
// Few storage buffers fit one shader stage, so related data share buffers:
// `work` holds the reduction scratch (at 0), the encoded band and the two
// sharpening planes (luma, then its horizontal blur) at the stage's offsets;
// `area_samples` and `area_weights` hold the horizontal tables, then the
// vertical ones, and the Gaussian kernel after the weights.
struct ExportStage {
    mode: u32, target_width: u32, target_height: u32, band_first: u32,
    band_rows: u32, core_first: u32, core_rows: u32, radius: u32,
    source_first_row: u32, source_row_count: u32, maximum: u32, ordered_dither: u32,
    amount: f32, threshold: f32, overshoot: f32, vertical_samples: u32,
    encoded_offset: u32, planes_offset: u32, vertical_weights: u32, gaussian_offset: u32,
};

@group(1) @binding(7) var<storage, read_write> work: array<f32>;
@group(1) @binding(8) var<storage, read> area_samples: array<vec4<u32>>;
@group(1) @binding(9) var<uniform> stage: ExportStage;
@group(1) @binding(10) var<storage, read> area_weights: array<f32>;
@group(1) @binding(11) var<storage, read_write> output_samples: array<u32>;

const REDUCE_HORIZONTAL = 0u;
const REDUCE_VERTICAL = 1u;
const ENCODE = 2u;
const LUMA = 3u;
const BLUR_HORIZONTAL = 4u;
const SHARPEN = 5u;
const QUANTIZE = 6u;

const STAGED_BAYER_8X8 = array<u32, 64>(
    0,32,8,40,2,34,10,42, 48,16,56,24,50,18,58,26,
    12,44,4,36,14,46,6,38, 60,28,52,20,62,30,54,22,
    3,35,11,43,1,33,9,41, 51,19,59,27,49,17,57,25,
    15,47,7,39,13,45,5,37, 63,31,55,23,61,29,53,21,
);

fn mirror_index(i: i32, length: u32) -> u32 {
    let period = i32(2u * length);
    let folded = u32(((i % period) + period) % period);
    if folded < length { return folded; }
    return 2u * length - 1u - folded;
}

// Developed linear Rec.2020 pixel of the cropped, oriented image.
fn developed_pixel(cropped: vec2<u32>) -> vec3<f32> {
    let source = source_coordinate(cropped);
    let camera_native = vec3<f32>(
        corrected_channel(source, 0u),
        corrected_channel(source, 1u),
        corrected_channel(source, 2u),
    );
    return develop_camera_color(camera_native);
}

fn encoded_at(row_in_band: u32, x: u32) -> vec3<f32> {
    let i = stage.encoded_offset + (row_in_band * stage.target_width + x) * 3u;
    return vec3<f32>(work[i], work[i + 1u], work[i + 2u]);
}

fn luma_at(row_in_band: u32, x: u32) -> f32 {
    return work[stage.planes_offset + row_in_band * stage.target_width + x];
}

fn staged_smoothstep(edge0: f32, edge1: f32, value: f32) -> f32 {
    if edge1 <= edge0 { return select(0.0, 1.0, value >= edge0); }
    let t = clamp((value - edge0) / (edge1 - edge0), 0.0, 1.0);
    return t * t * (3.0 - 2.0 * t);
}

fn quantize(value: vec3<f32>, x: u32, y: u32) -> vec3<u32> {
    var dither = 0.0;
    if stage.ordered_dither != 0u {
        dither = (f32(STAGED_BAYER_8X8[(y & 7u) * 8u + (x & 7u)]) + 0.5) / 64.0 - 0.5;
    }
    let maximum = f32(stage.maximum);
    // CPU round() rounds nonnegative halves upward; WGSL round() uses ties
    // to even. Clamp first and use floor(x + .5) to preserve CPU semantics.
    return vec3<u32>(floor(clamp(value * maximum + vec3<f32>(dither), vec3<f32>(0.0), vec3<f32>(maximum)) + vec3<f32>(0.5)));
}

@compute @workgroup_size(16, 16, 1)
fn staged_export(@builtin(global_invocation_id) invocation: vec3<u32>) {
    let x = invocation.x;
    let y = invocation.y;
    if x >= stage.target_width { return; }
    switch stage.mode {
        case REDUCE_HORIZONTAL: {
            if y >= stage.source_row_count { return; }
            let sample = area_samples[x];
            var sum = vec3<f32>(0.0);
            for (var offset = 0u; offset < sample.z; offset += 1u) {
                sum += developed_pixel(vec2<u32>(sample.x + offset, stage.source_first_row + y))
                    * area_weights[sample.y + offset];
            }
            let i = (y * stage.target_width + x) * 3u;
            work[i] = sum.x;
            work[i + 1u] = sum.y;
            work[i + 2u] = sum.z;
        }
        case REDUCE_VERTICAL: {
            if y >= stage.band_rows { return; }
            let sample = area_samples[stage.vertical_samples + stage.band_first + y];
            var sum = vec3<f32>(0.0);
            for (var offset = 0u; offset < sample.z; offset += 1u) {
                let i = ((sample.x + offset - stage.source_first_row) * stage.target_width + x) * 3u;
                sum += vec3<f32>(work[i], work[i + 1u], work[i + 2u])
                    * area_weights[stage.vertical_weights + sample.y + offset];
            }
            let e = encode_output(sum);
            let i = stage.encoded_offset + (y * stage.target_width + x) * 3u;
            work[i] = e.x; work[i + 1u] = e.y; work[i + 2u] = e.z;
        }
        case ENCODE: {
            if y >= stage.band_rows { return; }
            let e = encode_output(developed_pixel(vec2<u32>(x, stage.band_first + y)));
            let i = stage.encoded_offset + (y * stage.target_width + x) * 3u;
            work[i] = e.x; work[i + 1u] = e.y; work[i + 2u] = e.z;
        }
        case LUMA: {
            if y >= stage.band_rows { return; }
            let e = encoded_at(y, x);
            work[stage.planes_offset + y * stage.target_width + x] =
                clamp(e.x * 0.2126 + e.y * 0.7152 + e.z * 0.0722, 0.0, 1.0);
        }
        case BLUR_HORIZONTAL: {
            if y >= stage.band_rows { return; }
            var sum = 0.0;
            for (var k = 0u; k < 2u * stage.radius + 1u; k += 1u) {
                let sx = mirror_index(i32(x) + i32(k) - i32(stage.radius), stage.target_width);
                sum += area_weights[stage.gaussian_offset + k] * luma_at(y, sx);
            }
            work[stage.planes_offset + (stage.band_rows + y) * stage.target_width + x] = sum;
        }
        case SHARPEN: {
            if y >= stage.core_rows { return; }
            let row = stage.core_first + y;
            let in_band = row - stage.band_first;
            let centre = luma_at(in_band, x);
            var blurred = 0.0;
            for (var k = 0u; k < 2u * stage.radius + 1u; k += 1u) {
                let sy = mirror_index(i32(row) + i32(k) - i32(stage.radius), stage.target_height);
                blurred += area_weights[stage.gaussian_offset + k]
                    * work[stage.planes_offset + (stage.band_rows + sy - stage.band_first) * stage.target_width + x];
            }
            let detail = centre - blurred;
            let gain = staged_smoothstep(stage.threshold, 2.0 * stage.threshold, abs(detail));
            let target_luma = centre + stage.amount * detail * gain;
            var low = centre;
            var high = centre;
            for (var dy = -1; dy <= 1; dy += 1) {
                let sy = mirror_index(i32(row) + dy, stage.target_height);
                for (var dx = -1; dx <= 1; dx += 1) {
                    let value = luma_at(sy - stage.band_first, mirror_index(i32(x) + dx, stage.target_width));
                    low = min(low, value);
                    high = max(high, value);
                }
            }
            let delta = clamp(target_luma, low - stage.overshoot, high + stage.overshoot) - centre;
            let sharpened = clamp(encoded_at(in_band, x) + vec3<f32>(delta), vec3<f32>(0.0), vec3<f32>(1.0));
            let samples = quantize(sharpened, x, row);
            let i = (y * stage.target_width + x) * 3u;
            output_samples[i] = samples.x; output_samples[i + 1u] = samples.y; output_samples[i + 2u] = samples.z;
        }
        case QUANTIZE: {
            if y >= stage.core_rows { return; }
            let row = stage.core_first + y;
            let samples = quantize(encoded_at(row - stage.band_first, x), x, row);
            let i = (y * stage.target_width + x) * 3u;
            output_samples[i] = samples.x; output_samples[i + 1u] = samples.y; output_samples[i + 2u] = samples.z;
        }
        default: {}
    }
}
