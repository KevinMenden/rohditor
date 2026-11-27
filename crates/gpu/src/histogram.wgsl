// Counts the red, green, blue and luminance levels of a display image into 256 bins each, laid
// out one channel after another. Must count exactly like rohditor_core::Histogram::from_rgba8.

const BINS: u32 = 256u;
// Threads per workgroup along each axis.
const GROUP: u32 = 16u;
// Pixels per thread along each axis, so a workgroup counts 64x64 pixels.
const SPAN: u32 = 4u;

@group(0) @binding(0) var display: texture_2d<f32>;
@group(0) @binding(1) var<storage, read_write> bins: array<atomic<u32>, 1024>;

// The workgroup counts into shared memory first, so that only one global add per bin and
// workgroup is left. Large flat areas would otherwise serialize on a single global counter.
// Workgroup memory starts at zero.
var<workgroup> counts: array<atomic<u32>, 1024>;

@compute @workgroup_size(16, 16)
fn main(
    @builtin(workgroup_id) group: vec3<u32>,
    @builtin(local_invocation_id) local: vec3<u32>,
    @builtin(local_invocation_index) thread: u32,
) {
    let size = textureDimensions(display);
    let origin = group.xy * GROUP * SPAN + local.xy;
    for (var dy = 0u; dy < SPAN; dy++) {
        for (var dx = 0u; dx < SPAN; dx++) {
            // Neighbouring threads read neighbouring pixels.
            let pixel = origin + vec2(dx, dy) * GROUP;
            if all(pixel < size) {
                // Unorm values are exact multiples of 1/255, so this gives back the stored byte.
                let levels = vec3<u32>(round(textureLoad(display, pixel, 0).rgb * 255.0));
                // Rec. 709 in integers, rounded, as on the CPU.
                let luminance = (2126u * levels.r + 7152u * levels.g + 722u * levels.b + 5000u)
                    / 10000u;
                atomicAdd(&counts[levels.r], 1u);
                atomicAdd(&counts[BINS + levels.g], 1u);
                atomicAdd(&counts[2u * BINS + levels.b], 1u);
                atomicAdd(&counts[3u * BINS + luminance], 1u);
            }
        }
    }
    workgroupBarrier();
    for (var bin = thread; bin < 4u * BINS; bin += GROUP * GROUP) {
        let count = atomicLoad(&counts[bin]);
        if count != 0u {
            atomicAdd(&bins[bin], count);
        }
    }
}
