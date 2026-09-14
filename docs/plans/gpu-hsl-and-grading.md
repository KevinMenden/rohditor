# GPU HSL and three-way grading

Status: implemented; hardware parity and corpus checks passed on 2026-09-14.
End-to-end interaction timing and skin-tone visual qualification remain open.
First milestone of the
[GPU strategy](gpu-strategy.md).

## Goal and scope

Keep existing HSL mixer and three-way color-grading edits on the resident GPU
preview path. Preserve current image semantics, controls, recipe representation,
and CPU export. No new tools, RAW stages, spatial passes, or graph framework.

Previously `GpuPreviewProcessor::supports_recipe()` in
`crates/gpu/src/preview/processor.rs` rejected non-neutral HSL or grading. Desktop
checks that gate in `apps/desktop/src/app/mod.rs`. The reference formulas live in
the adjustments module of `crates/core/src/cpu/stages.rs`, exposed through
`crates/core/src/cpu/adjustments.rs`.

## Implementation

1. **Freeze parity expectations.** Inspect the CPU formulas and existing tests.
   Preserve eight unequal hue-band centers, normalized overlap weights, hue
   wraparound, the low-chroma fade, signed/HDR scale-and-offset restoration, and
   zero-effect bypasses. Preserve grading's luminance-dependent weights,
   multiplicative tint, and guarded luminance restoration. State the supported
   finite input range and numerical error budget before enabling the GPU path;
   assess source/intermediate half-float quantization separately from shader math.
2. **Extend the existing fused color pass.** Add aligned HSL/grading parameters
   to Rust preparation and WGSL. Reuse shared Rust constants where practical;
   verify any shader-side constants against the CPU contract. Apply HSL after
   saturation/vibrance, then grading, before storing the working result and
   performing output conversion. Keep neutral operations bypassed. Split shader
   helpers or parameter code only where it makes this addition easier to maintain.
3. **Enable resident editing.** Remove HSL/grading as unsupported-recipe reasons
   once parity passes. Update rejection tests to assert support. Check desktop
   backend selection, adjusted-cache identity, and resident rerender behavior;
   HSL/grading changes must invalidate adjusted output only. Keep capability,
   resource-failure, and stale-source protections. Preserve CPU fallback when GPU
   execution is unavailable.
4. **Qualify the complete slice.** Test the formulas and actual GPU execution,
   then exercise desktop interaction with a retained source. Add only the counters
   or test seams needed to prove reuse. Record hardware results and visual review
   here before marking the milestone complete.

## Acceptance checklist

- [x] All eight bands and all three grading ranges work independently and together,
  including minimum/maximum controls and combined existing color edits.
- [x] Small asymmetric fixtures cover hue centers, overlap and wraparound, neutral
  and near-neutral pixels, black/near-black, signed values, and HDR saturation.
  Equal band adjustments do not amplify overlap; zero settings retain baseline
  behavior. Grading preserves the CPU luminance and near-zero guard behavior.
- [x] CPU/GPU comparisons check working-linear results and encoded output, with
  explicit absolute/relative tolerances and encoded code-value limits. Exercise
  Standard/Neutral rendering, Light/curves, WB, and both output-gamut policies.
  Do not widen tolerances merely to hide formula errors.
- [x] After initial preparation, changing only HSL/grading causes no RAW
  reconstruction, source upload, or readback required for presentation. Undo/reset and
  switching between neutral and active settings retain the GPU backend.
  Existing delayed histogram readbacks remain separate and unchanged.
- [ ] Hardware measurements on the RX 9070 XT record adapter/driver, preview size,
  recipes, warm-cache GPU duration, and slider-to-display latency. Measure neutral,
  HSL-only, grading-only, and combined edits; compare CPU and prior neutral GPU
  baselines. Check rapid edits for stale frames and responsiveness.
- [ ] Representative RAW images receive visual CPU/GPU comparison, including skin,
  foliage, saturated colors, shadows, and clipped highlights. Export continues to
  use the unchanged CPU reference; compare matched-resolution processing to avoid
  conflating resize differences with color parity.
- [x] `./scripts/check.sh` and
  `cargo test --release -p rohditor-gpu -- --ignored --nocapture` pass. Run the
  relevant private-corpus checks for visual qualification. Record unavailable
  corpus/hardware checks as outstanding; software Vulkan is not hardware evidence.

Completion means implementation and qualification are both recorded. GPU export,
capture sharpening, optics/resampling, and RAW development remain subsequent
milestones in the strategy document.

## Implementation and qualification record

The fused shader now calls `color_adjustments.wgsl` after saturation/vibrance,
with HSL followed by grading. The 400-byte uniform carries all controls, the
shared CPU hue centers and hue-shift constant, neutral flags, and f32 epsilon.
CPU algorithms, recipe semantics, and export processing are unchanged. Source
provenance checks and device fallback remain active.

### Precision decision

Hardware testing exposed a source-format limitation: `[0.5001, 0.5, 0.5]` becomes
neutral in RGBA16F. With the existing CPU HSL formula and a red luminance edit,
this produced a large visible mismatch despite correct shader math. The retained
source and worker upload now use RGBA32F with unfiltered texture loads, preserving
finite f32 samples exactly. Capability checks, checked upload byte arithmetic,
and resident texture estimates reflect the change. No per-edit upload is needed.

The working target stays RGBA16F; display conversion uses the f32 shader result
before that storage rounding. At 2560x1707, source + working + display textures
occupy about 116.7 MiB, versus about 83.4 MiB with the previous source format.
This excludes CPU caches, staging, and driver allocations. The source memory
increase applies to neutral edits too, so later HSL activation can reuse it.

The numerical fixture covers finite signed/HDR inputs from -2 to 16, near-zero
values, hue centers/midpoints/wraparound, and near-neutral values. Valid recipe
limits are exercised for every HSL and grading control. The acceptance budget is
`abs(GPU - CPU) <= 2e-5 + 0.0015 * abs(CPU)` in the stored working result and at
most two encoded sRGB8 codes per channel. Source upload adds no quantization;
the linear budget includes half-float output rounding. This is a tested domain,
not a claim that every finite f32 intermediate fits the half-float working target.
Non-finite source samples remain rejected.

### Results

- `./scripts/check.sh` passed, including desktop cache and undo/reset/revision
  tests. Each of the 33 color controls changes only the adjusted cache key.
- `cargo test --release -p rohditor-gpu -- --ignored --nocapture` passed all 14
  tests on AMD Radeon RX 9070 XT, Vulkan/RADV, Mesa 26.2.2. An existing crop test
  was corrected to pass its actual WB recipe when constructing upload provenance.
- The 13x7 fixture matrix passed under both gamut policies with zero encoded
  code differences. Maximum stored-linear absolute error was 0.01387 on HDR
  values, within the stated relative budget. One source is uploaded for the
  complete edit/reset sequence and compatible output textures are reused.
- The camera-native matrix covers 16 recipes across all eight orientations,
  including WB and combined Light/rendering/HSL/grading. Its synthetic output
  matched exactly; its private RAW output differed by at most one code.
- Six private Sony files (`DSC00851`, `DSC01166`, `DSC02382`, `DSC03270`,
  `DSC03687`, `DSC03821`) passed neutral, HSL, grading, and combined comparisons
  under both gamut policies, at a 2560-pixel preview edge, within one code.
- CPU-left/GPU-right combined-edit comparison sheets were visually inspected
  for both policies: foliage, wildlife, snow/highlights, equipment, saturated
  greens, and a backlit field showed no visible backend discrepancy. Existing
  magenta clipped-highlight behavior appears in both CPU and GPU Clip output;
  changing that rendering behavior is outside this migration.

The opt-in `private_hsl_grading_corpus_parity_and_timings` test emits lossless
side-by-side PPMs into a fresh process-specific temporary directory and prints
its path. The inspected run used `/tmp/rohditor-gpu-color-2634691`; these artifacts
are temporary and can be regenerated with the test.

Warm timings across those six files (10 GPU samples after two warmups; five CPU
samples) were:

| Recipe | GPU queue-completion median range | CPU render median range |
| --- | --- | --- |
| Neutral | 0.282–0.578 ms | 103–120 ms |
| HSL | 0.273–0.321 ms | 130–149 ms |
| Grading | 0.298–0.347 ms | 105–145 ms |
| Combined Light/color | 0.346–0.946 ms | 178–222 ms |

These are resident color-render measurements, excluding source preparation and
upload. GPU queue completion includes submission/callback overhead; it is neither
a pure shader timestamp nor slider-to-screen latency. The workspace check ran
during part of the measurement, so these numbers are qualification observations,
not isolated performance-regression baselines.

### Remaining qualification

- Measure actual slider-to-display latency, rapid-edit presentation, and a
  comparable pre-change neutral GPU baseline in an isolated desktop run.
- Review a representative skin-tone RAW image; the six available corpus scenes
  do not provide a suitable close portrait.
- Keep the two corresponding acceptance items open until these checks are
  recorded. Presentation itself adds no readback; the existing asynchronous
  histogram/Auto Tone path still reads the display texture after its debounce.
