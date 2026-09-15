# GPU capture sharpening

Status: implemented with software qualification; 2026-09-15. RX 9070 XT
qualification, interaction measurements, and hardware performance selection remain open.

This is milestone 3 of [GPU processing strategy](gpu-strategy.md).
[Capture sharpening](capture-sharpening.md) owns the algorithm and controls;
this plan owns its GPU execution, preparation boundaries, and qualification.

## Scope and decisions

Port the existing opt-in algorithm without changing defaults, recipe semantics,
iteration count, or processing order. Use full-resolution camera-native RGB
immediately after demosaic, before optics, reduction, WB, and camera conversion.
Radius remains Gaussian sigma in source pixels. Fit preview, Source 1:1, and
export must exercise the same capture implementation when GPU execution is
selected. Keep the CPU implementation as reference and recovery path.

Optics and area reduction remain CPU operations until milestone 4. Consequently,
this milestone deliberately includes a temporary full-resolution RGB readback:

```text
CPU normalize / reconstruct / demosaic
  → GPU full-resolution capture sharpening (bounded tiles if needed)
  → CPU RGB readback / optics / area reduction as appropriate
  → existing GPU color preview or export
```

Source 1:1 can consume the GPU-sharpened CPU image through its existing
full-resolution color path; migrating its presentation is not required here.
Do not substitute sharpening of the reduced preview or move optics to hide the
transfer. Measure the complete bridge; faster kernels alone do not establish a
faster application. Fully resident spatial processing belongs to milestone 4.

No new controls, algorithm tuning, general graph scheduler, generic filter
framework, or new crate is required. Prefer concrete modules in core and GPU.
Milestone 2's open hardware checks remain open and must be distinguished from
new capture-stage results.

## Current seams

- `crates/core/src/sharpening.rs` owns algorithm v1, eight iterations, Gaussian
  construction, mirrored borders, masking, and six f32 scratch planes.
- `crates/core/src/pipeline/capture.rs` resolves highlight ceilings and checks
  capture memory. `pipeline/orchestration.rs` currently performs demosaic,
  capture, optics, and reduction together. `prepare_export_source` reuses this
  preparation without reduction. Full-resolution CPU rendering also has a
  capture call and must stay consistent with the extracted boundary.
- `crates/gpu/src/preview/processor.rs` uploads an already sharpened source and
  rejects mismatched sharpening provenance. `export/mod.rs` calls CPU source
  preparation before running the shared GPU color pipeline.
- `apps/desktop/src/preview_cache/storage.rs` includes capture settings in
  `ReconstructedCameraRgbKey`. Preview and export run through separate workers
  and devices; those devices do not share resident buffers.

## 1. Extract a typed preparation boundary

Split CPU preparation into demosaiced camera-source preparation and completion
through optics/reduction. Add a concrete source type containing camera-native
`LinearRgbImage<f32>`, source dimensions/coordinates, resolved calibration/WB
context, reconstruction provenance, capture ceilings, and stage timings. Keep
unsharpened and capture-completed states distinguishable so neither double
sharpening nor omission can pass compatibility checks.

Expose only the operations needed by GPU orchestration. Core must not depend on
wgpu or call a GPU processor. Existing CPU entry points compose these same
operations with CPU capture; GPU/application orchestration inserts the GPU
capture result before requesting CPU completion. Split orchestration into
focused preparation modules instead of enlarging the existing file.

Use identity WB at the camera-native boundary. Preserve the CPU disabled-path
behavior, including the existing full-resolution demosaic/WB optimization where
applicable; qualify any numerical differences from routing GPU requests through
the camera-native path. Do not obtain the new source by silently disabling
capture in a copied recipe and relabeling a completed preview.

Move Gaussian weights, iteration count, floor, and ceiling preparation into a
small shared core contract used by CPU and GPU. Keep the CPU numerical formulas
authoritative. No recipe/schema or algorithm-version change is needed for an
equivalent port; any intentional mathematical change needs separate review and
appropriate algorithm/cache versioning.

Acceptance: CPU bypass and active capture regressions pass after extraction;
preview/full-resolution/export preserve ordering and exact Clip/WB provenance.

## 2. Implement the f32 capture passes

Create `crates/gpu/src/capture/` with processor, resource/budget, shader, and test
responsibilities. Accept a supplied device/queue without presentation types.
Preview and headless export use this same processor implementation.

Start with explicit passes and reusable scalar f32 storage buffers:

1. Validate finite source data while packing; build the positive-channel mean
   guide floored at 1e-6, initial estimate, and highlight mask.
2. Separable Gaussian blur of the highlight mask; combine the original mask
   with the fourth power of its blur using the CPU minimum rule.
3. Blur the guide and apply the existing absolute/relative contrast threshold
   and shadow fade to the mask.
4. Eight iterations: blur estimate, form guide/blur ratio with the floor, blur
   ratio, and multiply/clamp estimate to 0.5–2 times the original guide.
5. Apply `1 + amount * mask * (estimate / guide - 1)` as a common RGB gain,
   retaining the CPU finite-product guard and signed/HDR behavior.

Use the CPU-generated normalized Gaussian weights and half-sample mirrored
borders, including one-pixel axes. Keep source, estimates, convolution, and
output f32; do not pass capture data through RGBA16F or display textures.
Disabled and zero-amount requests bypass capture allocation, upload, and
dispatch exactly. Do not bake WB, optics, orientation, or output crop into these
passes. Fuse passes only after baseline parity and resource hazards are tested.

Acceptance: asymmetric fixtures compare intermediate guide/mask/iteration
results and final camera RGB against CPU before application integration.

## 3. Bound memory, tiles, and submitted work

Implement capture-specific tiles in this milestone. A 6000×4000 image already
requires 576,000,000 bytes for six scalar planes; one RGBA32F source adds
384,000,000 bytes before output or staging. This exceeds the existing 768 MiB
export allocation budget. Do not raise that budget to accommodate full-frame
scratch or make normal 24 MP capture silently CPU-only.

Calculate live allocations with checked arithmetic: source, output, six planes,
upload/readback staging, retained buffers, and other resources on the same
device. Validate individual storage bindings, buffer sizes, workgroup counts,
and device limits before allocation. Preserve the 2 GiB CPU working-set bound,
including decoded RAW, retained camera source, assembled result, optics,
staging, and export output at their actual overlapping lifetimes. Release
capture scratch before allocating the full-resolution export color source when
necessary. Account for preview/export device allocations together in reporting.

The dependency radius grows with every iteration. With Gaussian support radius
`r = ceil(3 * sigma)` and `I = 8`, the estimate depends on a square source halo
of `2 * I * r` per axis (at most 64 pixels); the mask needs only `r`.
Document and test this derivation. Upload each tile with that halo, compute its
extended domain, and copy back only the valid core. At true image edges use the
global half-sample mirror rule on every convolution pass; artificial tile edges
must never affect a retained core pixel. Test tiles touching actual corners and
dimensions smaller than the halo. Tile input always comes from the immutable
unsharpened source, never previously assembled sharpened output.

Select tile dimensions from the remaining budget and device limits, with a
bounded maximum work size even when the entire frame fits. Reuse one tile's
scratch/staging resources; allow at most one capture work unit in flight.
Check cancellation before packing, between submissions/iterations as needed,
during readback/assembly, and before publishing. Tune submission size against
measured responsiveness, without changing the eight-iteration algorithm.

Full-source GPU retention is opportunistic under the same budget; otherwise
retain the CPU source and upload tile halos on capture edits. Evict unused
scratch/source resources on document changes and pressure. Do not promise a
single full-frame resident upload on all supported devices.

Acceptance: forced-small-budget tiled/untiled equivalence, seams, edge handling,
limit rejection, cancellation, and recoverable allocation/readback failure pass.
If even a minimum valid tile or CPU result cannot fit, report a clear resource
failure and apply the existing backend policy; CPU fallback is also budgeted.

## 4. Integrate reuse, backend selection, and recovery

Separate the pre-capture source key from the completed reconstruction key.
The former includes RAW identity, raw crop, demosaic method, reconstruction
settings/algorithm, and any resolved WB dependency of reconstruction. Capture
results additionally include settings, algorithm identity, and resolved
highlight ceilings. Later keys add optics and reduction settings. Preserve
camera-profile/calibration provenance wherever it affects pixels or ceilings.

Capture edits reuse demosaic; optics edits reuse available capture results;
Light/HSL/grading edits reuse the completed uploaded color source. These are
budgeted caches, not a mandate to retain every full-resolution intermediate.
Test both cache hits and correct recomputation after eviction. Clip/WB changes
invalidate reconstruction and capture as required. Preserve the existing
temporary WB draft behavior and require exact reconstruction for settled
Source 1:1 and export results.

Connect preview worker preparation, Source 1:1 preparation, and
`GpuExportProcessor::render` to the shared capture processor. Keep existing
already-prepared APIs explicit about whether capture was completed and by which
compatible algorithm. Preserve CLI `auto|cpu|gpu` and desktop recovery semantics:
CPU forces CPU capture; auto attempts GPU with an observable reason on recovery;
required-GPU CLI failure must not encode a CPU replacement. Desktop retains its
existing reported CPU recovery policy. Cancellation is not a fallback trigger.

Assemble results privately and publish only complete matching document/revision
results. Never resume CPU capture on partially sharpened tile output; restart
from the unmodified camera source. Device failure invalidates affected GPU
caches. Preserve the serial export worker's active/queued snapshot bounds and
transactional encoding. Coalesce rapid capture edits and stop submitting stale
work; native display texture registration remains unchanged.

Acceptance: cache/provenance tests cover capture changes, Clip/WB, optics,
downstream edits, eviction, rapid document switches, cancellation, and recovery.

## 5. Qualification and completion gates

Run `./scripts/check.sh` for each implementation slice. Full-resolution/GPU
integration additionally requires both ignored release suites:

```sh
cargo test --release --workspace --tests -- --ignored --nocapture
cargo test --release -p rohditor-gpu -- --ignored --nocapture
```

Use asymmetric impulses/edges, constants, noisy patches, padded strides, tiny
axes, signed/HDR values, near-floor values, clipped channels, and invalid finite
input checks. Cover parameter extremes and every highlight method. Compare
multiple tile sizes and boundaries crossing edges/highlights at maximum radius.

Initial camera-RGB parity gate: per channel, absolute error no greater than
`2e-5 + 2e-5 * abs(cpu)`; exact bypass remains bit-exact. Treat this as a proposed
acceptance threshold to validate against the CPU fixtures, not an observed
result. Investigate failures before changing it. Retain existing known-blur MSE,
overshoot/undershoot (0.025), and flat-patch noise-gain (1.01) regression limits.
End-to-end export must retain milestone 2's limits: maximum 2 codes at 8-bit and
16 at 16-bit, corpus mean errors at most 0.1 and 1 code respectively. Exercise
both gamut policies, dither modes, crop/orientation, and exact Clip/WB.

Save CPU On/GPU On and Off/On Source 1:1 crops of representative Sony RAWs:
foliage, hair/fabric, skin, flat sky, high ISO, reconstructed highlights, and
strong TCA/lens corners. Include default settings and noise protection zero as a
diagnostic. Compare matched-resolution preview, Source 1:1, and 8/16-bit export.
Numerical parity does not establish visible usefulness or absence of artifacts;
do not tune the algorithm or enable it by default as part of this port.

Measure 24 MP and 48 MP stages on the RX 9070 XT, recording adapter/driver,
dimensions, recipe, tile/halo size, and cold/warm state. Report CPU preparation,
GPU capture, upload/readback bytes and latency, CPU optics/reduction, color
processing, and total time separately. Compare against CPU capture on identical
inputs. Record actual peak host/device memory separately from buffer estimates.
Measure resident color edits, capture edits, cancellation, and concurrent export
with preview p50/p95 latency; establish the baseline before implementation and
record any regression and its cause. An end-to-end slowdown requires an explicit
backend-selection decision before declaring GPU capture the normal path.

- [x] Shared typed preparation boundary and unchanged CPU reference verified.
- [x] Shared GPU capture runs before optics/reduction for preview, Source 1:1,
  and export; no duplicate CPU capture remains on successful GPU requests.
- [x] Bounded spatial scratch, iterative halos, resource eviction, cancellation,
  and stale-result rejection verified.
- [x] Numerical, tiled/untiled, full-resolution, and existing export gates pass
  on software Vulkan; hardware gates below remain open.
- [ ] RX 9070 XT corpus parity and saved visual comparisons reviewed.
- [ ] Transfer-inclusive performance, peak memory, and concurrent interaction
  measured; backend-selection decision recorded.
- [ ] Hardware failure/recovery and constrained-budget behavior qualified.

Record software Vulkan results separately and leave hardware items unchecked
when unavailable. Append actual implementation/qualification evidence here;
update the strategy's milestone status only to the extent demonstrated. CPU
optics/reduction and their temporary transfers remain explicit until milestone 4.

## Implementation and qualification evidence — 2026-09-15

### Implementation

- `core/src/pipeline/camera_source.rs` separates immutable camera-native
  demosaic output from capture-completed output and CPU optics/reduction.
  Active CPU full-resolution rendering uses this same boundary. The disabled
  full-resolution CPU path retains its demosaic/WB optimization. The shared
  contract owns normalized Gaussian weights, floor, iteration count, and
  resolved ceilings; algorithm v1, recipes, controls, and defaults are unchanged.
- `gpu/src/capture/` implements scalar f32 passes, six scratch planes, bounded
  RGB tile upload/readback, and private output assembly. A maximum 512-pixel
  core uses a halo of `2 * 8 * ceil(3 * sigma)`, at most 64 pixels. Domains are
  clipped to the actual image boundary; local mirroring at artificial edges
  cannot reach a retained core. Every tile reads the immutable source.
- Maximum-radius scratch and staging are estimated at 24,641,536 device bytes;
  default-radius tiles use 19,972,096. The six scalar planes share one storage
  binding; RGB input/output uses three f32 scalars per pixel. Budgets include
  staging and parameters and shrink tiles against remaining allocation and
  binding/dispatch limits. These are buffer estimates, not measured physical
  device memory. Only one bounded work unit is submitted at a time, with a
  cancellation checkpoint between iterations; cancellation drains that unit.
- Preview capture uses the existing supplied preview device/queue and reserves
  at most 64 MiB of the existing budget for its tiles. Export uses its separate
  serial worker device and releases capture scratch before color upload.
  Full-source GPU retention is not implemented: the opportunistic caches retain
  CPU camera RGB. CPU optics/reduction and their transfers remain explicit.
- Processing reservations track retained color sources/staging, preview
  textures, capture tiles, and export bands together across devices, with
  current and process-lifetime peak totals logged by both workers. Source
  staging is conservatively reserved for the source's lifetime. Driver pools,
  compiled shaders, and native UI allocations are excluded; this is not a
  physical GPU memory measurement.
- Desktop pre-capture and capture keys preserve reconstruction/WB/profile,
  algorithm/settings, and resolved ceilings. Capture edits reuse demosaic;
  optics/reduction edits reuse capture; downstream color edits keep the existing
  uploaded-source reuse. Full-resolution cache retention is limited to 768 MiB
  and conservatively reserves operation memory under the 2 GiB host limit.
  Oversized entries are recomputed. Document changes evict image resources.
- Fit preview, Source 1:1, and GPU export invoke the shared capture processor.
  CPU selection forces CPU capture. Recovery starts from the original source;
  desktop reports CPU capture recovery and logs its reason. Cancellation is
  propagated without CPU replacement. CLI required-GPU and transactional
  export behavior are preserved.

### Software validation

- `./scripts/check.sh` passed, including existing CPU bypass, capture-quality,
  source-scale/export, cache, and stale-ticket regressions. Both ignored release
  suites passed with `RUST_TEST_THREADS=1`; hardware-only tests explicitly
  skipped llvmpipe. Follow-up targeted checks cover the final device-loss fix.
- The adapter was **llvmpipe, LLVM 23.1.0, Vulkan CPU, Mesa 26.2.2**. This is not
  an RX 9070 XT result. Tests compare guide initialization, completed masking,
  and every RL iteration with the CPU reference. Fixtures cover asymmetric
  data, padded strides, tiny axes, signed/HDR and near-floor samples, invalid
  input, parameter extremes, multiple tile sizes, constrained budgets, and
  cancellation followed by recovery.
- Destroying an isolated test device initially exposed a panic in wgpu's
  mapped-at-creation initialization helper. Capture now uses unmapped buffers
  and queue uploads, with validation/allocation/internal error scopes. The
  device-loss test now returns an error and permits CPU recovery from the
  unchanged source. Physical hardware loss/OOM remains unqualified.
- All six private Sony RAWs passed full 6000×4000 camera-RGB comparisons with
  noise protection 0.5 and 0: maximum absolute error was **1.1920929e-7** in
  every run, below the proposed tolerance. Each image used 96 tiles, a
  512-pixel core, a 32-pixel halo, and 357,832,704 bytes in each transfer direction.
- Capture plus full-resolution color export on `DSC00851.ARW` passed both gamut
  policies at 8/16 bits with ordered dither: maximum error 1 code at 8 bits and
  12 at 16 bits; maximum means 0.00001 and 0.00420 codes respectively. Synthetic
  integration also exercises all highlight methods, exact non-neutral Clip/WB,
  reduction, orientation/crop, both dither modes, and both gamut policies.
- Fifty PNG crops were saved to `/tmp/rohditor-gpu-capture-crops`: ten manifest
  regions, each with Off, CPU On, and GPU On at both noise-protection settings.
  Eighteen of twenty CPU/GPU On PNG pairs are byte-identical; the other two
  differ by at most one 8-bit code. Inspection of moss, high-ISO fur, and rope
  crops found no obvious new GPU artifact; the sharpening remains subtle.
  This does not qualify the missing skin, flat-sky, or strong lens-corner cases.
  Reproduce with `ROHDITOR_GPU_CAPTURE_ARTIFACTS=<directory>` and the ignored
  `private_capture_camera_parity_and_visual_crops` test.

### Performance and remaining hardware work

- CPU capture baseline fixtures measured 703.3 ms at 6000×4000 and 1399.0 ms at
  8000×6000, with cancellation at 10.6/5.8 ms. These are CPU kernel fixtures,
  not desktop end-to-end baselines. A subsequent run without other agent checks
  measured 761.9/1436.5 ms and cancellation at 23.1/114.9 ms. These single-run
  increases are recorded rather than treated as proof of unchanged performance;
  scheduling/allocation effects have not been separated from code effects by
  controlled statistical runs. The performance gate remains open.
  The software corpus bridge took roughly
  16.6–31.0 seconds per 24 MP image, with later runs overlapping other checks.
  Concurrent software-suite timings are not hardware performance measurements.
- Backend decision for this environment: retain the existing rejection of CPU
  rasterizers for application GPU processing. GPU capture is available when
  hardware GPU execution is selected, but no speedup or hardware qualification
  is claimed. Capture remains opt-in. A decision that it should be the normal
  hardware path still requires the planned transfer-inclusive measurements.
- RX 9070 XT adapter/driver qualification, 24/48 MP cold/warm stage timing,
  actual peak host/device memory and validation of the combined reservations,
  interactive p50/p95 and cancellation under concurrent export, and physical
  failure recovery remain open. Native interactive UI behavior was not manually
  qualified in this session. Milestone 2's hardware items remain unchanged.
