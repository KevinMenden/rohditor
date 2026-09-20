# GPU optics and preview resampling

Status: implementation and software-adapter qualification complete; 2026-09-20.
Supported-hardware qualification on the RX 9070 XT remains open. This milestone
is not complete until that evidence is recorded.

This is milestone 4 of the
[GPU processing strategy](gpu-strategy.md). The existing Lensfun behavior and
profile contract remain owned by the optics implementation; this plan owns the
GPU execution boundary, resident camera source, preview reduction, integration,
and qualification.

## Goal

Keep camera-native RGB on the GPU after capture sharpening, apply optics there,
and produce either an antialiased fit-preview source or full-resolution output
without reading camera RGB back to the CPU. A successful GPU path must have no
full-resolution capture readback followed by CPU optics and a second upload.
Only final JPEG/PNG samples return to the CPU for encoding. Native preview and
Source 1:1 presentation remain GPU textures; small picker, histogram, test, and
diagnostic results may still be read back deliberately.

Preserve the current order and semantics:

```text
CPU normalization / reconstruction / demosaic
  -> GPU camera-native f32 source
  -> GPU capture sharpening, when active
  -> resident capture-completed camera RGB
  -> GPU vignetting then combined distortion/TCA cubic remap
  +-> exact area reduction -> existing GPU preview color pipeline
  +-> full-resolution color/output -> GPU display texture or export bands
```

Lensfun database loading, matching, interpolation, automatic scale selection,
and provenance remain CPU work in `rohditor-optics`. GPU work begins only after
an immutable, shot-specific execution plan has been resolved. The CPU optics
and area-reduction implementations remain the numerical reference and fallback.

## Scope and non-goals

This milestone includes:

- a backend-neutral optics execution contract shared by the CPU reference and
  GPU parameter preparation;
- a bounded, f32, camera-native GPU image representation that survives capture
  and can be reused by optics edits;
- vignetting, all current distortion and TCA models, Catmull-Rom sampling, and
  the current invalid-footprint behavior;
- exact separable pixel-area reduction at the existing preview dimensions;
- fit preview, Source 1:1, and headless JPEG/PNG export integration;
- cache identity, cancellation, stale-result rejection, memory accounting,
  backend recovery, numerical parity, visual review, and hardware measurement.

It does not add lens models, change profile matching, tune corrections, add UI
controls, alter recipe defaults, move sensor development or demosaic to GPU,
encode files on GPU, merge the preview and export devices, or introduce a
general graph scheduler. It does not close milestone 2 or 3 hardware items
without their own recorded evidence.

## Current boundary to replace

- `crates/core/src/pipeline/camera_source.rs` owns the typed
  `DemosaicedCameraSource` and `CapturedCameraSource` states. Its completion step
  currently runs CPU optics and `resize_area_cancellable`, returning a CPU
  `ReconstructedPreview`.
- `crates/gpu/src/capture/` uploads every capture tile and reads the result back
  into a full CPU `LinearRgbImage<f32>`. Capture scratch is bounded, but no
  camera image is retained on the device.
- `crates/optics` resolves a private `LensCorrectionPlan`. CPU execution applies
  vignetting in camera space, then combines distortion and per-channel TCA into
  one remap with a 4x4 Catmull-Rom footprint. Invalid or non-finite coordinates
  fail rather than clamp to an edge.
- `crates/core/src/resample.rs` applies a separable area filter. Destination
  samples are normalized source-pixel-cell overlap integrals; it is not a
  bilinear resize.
- Fit preview currently receives CPU optics/reduction output, packs RGB as
  RGBA32F, and uploads it to `GpuPreviewProcessor`. Source 1:1 remains an sRGB8
  CPU result even when GPU capture was used. GPU export repeats the same CPU
  completion before uploading a full-resolution RGBA32F source.
- `apps/desktop/src/preview_cache/spatial.rs` retains CPU pre-capture and
  captured images. Optics settings live in the later reconstructed cache key,
  but an optics edit still executes on the CPU and causes a new GPU upload.

## 1. Freeze and expose the mathematical contracts

Add a small public execution view in `rohditor-optics` without exposing Lensfun
database objects. It contains dimensions, center/normalization/automatic-scale
constants, tagged distortion/TCA/vignetting coefficients, applied components,
and the existing `OpticsProvenance`. `LensCorrectionPlan` resolution remains the
only way to create a valid contract. The CPU correction path must consume the
same execution view so its formulas cannot drift from the data packed for WGSL.

Preserve these algorithm-v1 rules:

- output and source coordinates are un-oriented, recommended-crop-local pixel
  centers; EXIF orientation and user crop remain downstream;
- vignetting multiplies source lattice samples before any geometry/TCA remap;
- distortion is evaluated once, then TCA produces independent red, green, and
  blue coordinates from that result;
- each channel uses the current Catmull-Rom cubic kernel with tension `-0.5`
  and a 4x4 footprint;
- automatic scale retains its current perimeter guard, while each cubic sample
  retains the exact floor-based footprint check (`floor - 1 >= 0` and
  `floor + 2 < length`); non-finite mapping, gain, or output is an error, not a
  clipped or substituted pixel;
- signed and HDR camera values remain f32 and are not clipped by optics.

Lensfun plan construction currently performs part of its coordinate setup in
f64 before invoking f32 model functions. Keep that CPU behavior authoritative.
Pack the minimal stable f32 constants required by WGSL and compare mapped
coordinates before comparing pixels. Do not require shader f64 support. If the
f32 port cannot meet the agreed image gates, treat any reference-formula change
as an optics algorithm change: review it separately, bump
`OPTICS_ALGORITHM_VERSION`, update fingerprints, and compare old/new real-image
output. Do not hide a coordinate error by widening pixel tolerances.

Extract the area-axis calculation into a backend-neutral `AreaReductionPlan`
owned below the GPU crate. It records each destination sample's first source
cell and flattened f32 overlap weights calculated by the current f64 setup. The
CPU reference and GPU pass consume the same tables. Record a resampling
algorithm version in cache/provenance if this extraction changes pixel identity.

In core, introduce a pixel-storage-independent spatial completion description
containing the resolved optics execution plan or explicit Off state, target
dimensions, area plans, calibration/highlight/capture provenance, orientation,
and accumulated preparation diagnostics. Do not put wgpu types in core and do
not fabricate a CPU `ReconstructedPreview` for a GPU-resident image.

Acceptance: extracting these contracts leaves CPU preview, Source 1:1, export,
optics upstream-parity tests, and cache fingerprints unchanged bit for bit.

## 2. Retain a bounded capture-completed GPU source

Evolve the concrete capture executor into a `GpuSpatialProcessor` under
`crates/gpu/src/spatial/`. Keep capture math in focused capture modules and add
source, optics, reduction, resource, and qualification modules. The processor
accepts a supplied device/queue and has no egui, desktop, codec, or filesystem
dependency. This is an ordered executor for the current stages, not a generic
pipeline graph.

Use three planar `R32Float` texture arrays for resident camera RGB. Planar f32
keeps the 12-byte-per-pixel CPU representation, preserves signed/HDR values,
avoids RGBA32F's 33 percent storage overhead, and avoids a single large storage
buffer binding. Split the logical image into equally sized array layers. A
shared global-coordinate loader maps `(x, y)` to layer/local coordinates, so
optics sampling can cross physical tile boundaries without treating them as
image edges. Include padded edge texels in allocation estimates.

Validate sampled and write-only storage support for `R32Float`, maximum 2D
extent, array layers, workgroup counts, bindings, copy layout, and total budget
before allocation. Select the tile edge from actual limits and allocation
overhead; do not hard-code support for only one sensor size. If the logical
image cannot fit a valid tiled backing plus the minimum work unit under the
budget, return a typed unsupported/resource error before partial execution.

For inactive capture, upload the CPU demosaiced RGB once into the resident
planes. For active capture, reuse the current bounded iterative tile algorithm:
upload each immutable source tile with its established capture halo, compute
the tile, and scatter only its valid core into the resident planes. Remove the
tile readback and CPU full-frame result assembly. Release capture scratch after
the final core is stored. The first slice does not retain a second full-size
unsharpened GPU image; capture edits reuse the existing CPU demosaic cache and
rebuild the resident captured source, while optics-only edits reuse that source.

Represent GPU states with distinct owning types, for example resident
demosaiced/capture-completed/spatial-preview states, and attach exact source,
capture, and calibration provenance. A disabled capture may transfer ownership
through an explicit bypass; an active source cannot be relabeled without the
matching capture contract. Never convert partial device output into a core CPU
state.

All allocations use checked arithmetic and `crates/gpu/src/memory.rs`
reservations. Count actual tiled extents, upload staging, capture scratch,
optics/reduction scratch, preview/export targets, and concurrent allocations on
the separate preview and export devices. Keep the current 768 MiB per-operation
ceiling and 2 GiB host working-set ceiling unless measured evidence supports a
separate strategy decision. Driver heaps remain outside estimates and must be
measured independently.

Acceptance: successful GPU capture publishes a complete resident source with
zero camera-RGB readback. Capture-disabled upload and active capture match the
CPU typed boundary, constrained budgets fail before publication, and device
loss permits recovery from the immutable CPU source.

## 3. Implement optics and exact preview reduction

Add explicit WGSL helpers for every current optics model and the cubic kernel.
For each logical optics-output pixel:

1. compute the distortion coordinate from the global output pixel center;
2. derive channel-specific TCA coordinates;
3. load each channel's 4x4 source footprint through the tiled global loader;
4. apply the vignetting multiplier to each source lattice sample before its
   cubic weight, matching the CPU stage order;
5. accumulate in f32 and set a shared failure flag on non-finite values or an
   invalid footprint.

Component bypasses must be explicit. Off is an identity sample; vignetting-only
does not invoke cubic interpolation; distortion and TCA share one interpolation
pass; unavailable requested components retain the resolved plan's existing
`requested` versus `applied` provenance. Do not use hardware linear filtering,
edge clamping, or normalized texture coordinates to approximate this contract.

For fit preview, preserve the materialized CPU order `optics -> horizontal area
filter -> vertical area filter` without allocating a full corrected image. Work
in bounded destination-row bands:

- determine the exact global source rows needed by the band's vertical area
  entries;
- horizontally reduce optics-corrected full-resolution pixels into an f32 RGB
  band scratch buffer using the shared axis weights;
- vertically reduce that scratch into the final preview-sized RGBA32F camera
  texture with alpha one;
- retain global coordinates and overlap rows across bands, and publish only
  after every band and the failure flag succeed.

This is a combined execution of warping and reduction, but not a combined
single-sample approximation. Every full-resolution optics-output pixel and
every area weight required by the CPU reference still contributes. A single
bilinear or bicubic lookup at the destination center is not an acceptable
downsampling path. If repeated cubic evaluation proves too costly, optimize
only after baseline parity, using algebraically equivalent reuse within a work
unit.

No-reduction requests skip the area passes. Export and Source 1:1 can evaluate
optics directly at the full-resolution coordinate consumed by their output
pass, avoiding a full optics-output texture. Split the shared color shader so
the authoritative WB/camera conversion and downstream color function accepts a
camera RGB value; the existing reduced-texture preview loader and the direct
full-resolution optics loader both call it. Preserve exposure, rendering, Light,
tone, saturation/vibrance, HSL, grading, gamut mapping, orientation, crop, and
dither order.

Keep one bounded work unit in flight per processor initially. Check cancellation
before packing plans, between bands/submissions, after queue completion, and
before publication. Submitted work may drain after cancellation, but its result
must not be installed. Tile/band boundaries use global pixel and dither
coordinates and must not create seams.

Acceptance: all optics components and reduction ratios match the CPU reference;
the successful fit path produces a reduced resident camera texture without a
full-resolution optics allocation, camera readback, or second CPU upload.

## 4. Integrate preview, Source 1:1, and export

### Fit preview

Run spatial preparation on the existing preview worker with the eframe device
and queue. Replace the worker's CPU `ReconstructedPreview -> GpuPreviewUpload`
handoff with a GPU-owned reduced camera source from the same device. The UI
continues to own texture registration and presentation; downstream color edits
continue to reuse the reduced source and existing display textures.

An optics edit reuses the resident capture-completed source and reruns only
optics/reduction plus downstream color. A target-size change reruns reduction,
not capture. A capture edit reuses CPU demosaic, rebuilds capture residency, and
invalidates optics/reduction. Downstream WB/Light/HSL/grading edits retain the
current fast source reuse, subject to Clip/WB reconstruction rules.

### Source 1:1

Add a GPU Source 1:1 result that writes the final oriented/cropped sRGB8 image
to an egui-compatible GPU texture and reports the GPU backend accurately. Do not
route successful GPU Source 1:1 through `DisplayRgbImage<u8>` or label it CPU.
Avoid a full-resolution RGBA16F working texture when it would break the budget;
the direct f32 shader value may proceed to output conversion in the same pass.
Keep the CPU Source 1:1 path unchanged for forced CPU and recovery.

Replace full-image CPU histogram construction for GPU Source 1:1 with a GPU
histogram reduction and a bounded bin-buffer readback, or explicitly defer the
histogram until a fit preview is available. Picker reads remain small explicit
operations. Tests and opt-in diagnostics may read pixels back, but ordinary
presentation must use the native display texture.

### Export

Have `GpuExportProcessor` prepare the CPU demosaiced source, run capture into
resident planes, resolve the same optics execution contract, and evaluate
optics plus shared color/output in the existing bounded export bands. Preserve
global orientation, crop, and ordered-dither coordinates. Read back only final
integer output bands and keep CPU encoding, metadata, cancellation, and
transactional file writes unchanged.

Preview and export still use separate devices and resident copies. Do not imply
cross-device sharing. Release capture scratch before optics/reduction/output
resources, and evict the export resident source after the immutable export
snapshot finishes unless a measured bounded cache is justified later.

Acceptance: fit preview, Source 1:1, and export all exercise the same optics
contract and GPU kernels. Instrumented successful runs show no full camera-RGB
readback and no post-optics full-resolution upload. Export retains only its
final bounded pixel readbacks.

## 5. Cache identity, recovery, and publication

Separate cache identities at the actual invalidation boundaries:

- pre-capture: RAW/source identity, RAW crop, highlight/reconstruction and its
  Clip/WB/profile dependencies, demosaic algorithm, and algorithm versions;
- capture-completed resident source: pre-capture identity plus capture settings,
  resolved ceilings, capture algorithm identity, and backend representation;
- optics execution/output: captured identity plus database and plan content
  fingerprints, profile/component intent, applied components, automatic scale,
  metadata fallbacks, and optics algorithm identity;
- reduced preview: optics identity plus target dimensions, exact area-plan
  identity, resampling algorithm identity, and resident format/layout version;
- downstream output: the existing color, geometry, and gamut keys.

The cache stores GPU owners, not bare views. Evict document resources on source
changes and all affected resources on device loss. Budget retention in this
order: currently displayed reduced source/frame, capture-completed source useful
for optics edits, then optional upstream data. CPU demosaic retention remains
bounded separately. Correct recomputation after eviction is part of acceptance;
residency is not permission to exceed either budget.

Preserve temporary WB drafts for Clip exactly as today: the old resident result
may render a marked draft during interaction, but settled fit, Source 1:1, and
export require exact reconstruction/capture provenance. Optics edits may keep
the old frame visible while new GPU work runs, but old optics provenance must
never be presented as the new revision.

CPU selection bypasses the spatial GPU path. Auto attempts GPU and records the
specific recovery reason before restarting from immutable CPU camera data.
Required-GPU CLI export fails without encoding a CPU replacement. Cancellation
and supersession are not fallback triggers. GPU allocation, validation,
non-finite/footprint flags, synchronization, and device-loss failures return
before publishing any partial image; recovery never continues from a partially
processed resident source.

Retain the serial export worker's one-active/one-queued snapshots and the
desktop document/export/revision checks. Coalesce rapid optics edits before
submission where possible. Already submitted bounded work may finish, but a
stale ticket cannot replace a newer frame or export result.

Acceptance: cache tests prove the intended reuse and invalidation matrix,
including Off/on component toggles, WB under every highlight mode, capture
changes, preview-size changes, eviction, cancellation, rapid document switches,
device loss, and auto versus required-GPU recovery.

## 6. Validation and qualification

Run the project gate for each implementation slice. Because this changes GPU,
full-resolution, optics, preview, and export behavior, final validation requires:

```sh
./scripts/check.sh
cargo test --release --workspace --tests -- --ignored --nocapture
cargo test --release -p rohditor-gpu -- --ignored --nocapture
```

Keep software Vulkan results explicitly separate from RX 9070 XT results.
Before enabling the new normal path, establish and record these baselines and
results.

### Deterministic fixtures

- Exercise distortion Poly3, Poly5, and PTLens; TCA Linear and Poly3;
  vignetting PA; every single component, every combination, Off, and requested
  but unavailable components.
- Compare CPU/GPU mapped coordinates at center, axes, corners, scale boundary,
  and asymmetric interior points. Include dimensions just above the four-pixel
  minimum, odd dimensions, padded CPU strides, tile crossings, and band
  crossings.
- Use constants, per-channel impulses, ramps, checkerboards, one-pixel lines,
  high-frequency diagonals, signed values, HDR values, and non-finite rejection.
  Verify vignetting-before-remap and channel-specific TCA rather than only final
  RGB similarity.
- Test exact and fractional area reductions, one unchanged axis, large ratios,
  source dimensions smaller than one work unit, several forced work-unit sizes,
  and output rows whose vertical footprints cross bands. Compare tiled and
  untiled execution where both fit.
- Preserve geometry/export coverage for all eight orientations, nontrivial user
  crop, 8/16-bit output, both dither modes, both gamut policies, Clip with
  non-neutral WB, and capture Off/on.

Use proposed pre-output gates of `2e-6 + 2e-6 * abs(cpu)` for reduction with
optics Off and `2e-4 + 2e-4 * abs(cpu)` for optics-processed camera RGB, plus a
mapped-coordinate error no greater than `2e-4` source pixel. These are initial
thresholds to validate, not observed results. Investigate model, precision, or
ordering errors before changing them. Preserve milestone 2's final export gates:
maximum 2 codes at 8-bit and 16 codes at 16-bit, with corpus mean errors at most
0.1 and 1 code respectively.

### Real-image review

Use representative Sony RAWs and save matched CPU/GPU comparisons for:

- straight architectural lines near edges for distortion and automatic scale;
- high-contrast radial edges and lens corners for red/blue TCA;
- flat walls or sky for vignetting smoothness and band/tile seams;
- foliage, fabric, roof tiles, and diagonals for downsampling aliasing and moire;
- skin, deep shadows, saturated colors, and reconstructed highlights to catch
  unintended color, clipping, or signed/HDR changes.

Compare fit preview, Source 1:1, and 8/16-bit export at matched dimensions.
Review each optics component independently and together. Numerical parity does
not by itself establish that the profile or interpolation looks correct.

### Hardware, memory, and interaction

On the RX 9070 XT, record adapter, driver, device limits, source dimensions,
recipe, resident tile layout, work-unit size, budget, and cold/warm state. Test
at least representative 24 MP and 48 MP inputs. Measure separately:

- CPU preparation and optics-plan resolution;
- source packing/upload, capture, optics, reduction, downstream color, and
  output/presentation or export readback;
- bytes uploaded and read back, proving removal of the old full-resolution
  capture readback and second upload;
- warm optics-only edit p50/p95 slider-to-display latency, capture edits,
  Source 1:1 entry, export throughput, cancellation latency, and preview latency
  during concurrent export;
- reservation peaks and actual peak host/device memory, including simultaneous
  preview/export devices and driver allocations;
- constrained-budget fallback, allocation failure, device loss, recovery, and
  rapid stale-result behavior.

Compare complete GPU paths with the existing CPU optics/reduction path on the
same inputs. Kernel-only speedups do not establish a better backend. If the
resident path regresses normal interaction or exceeds the budget, record the
cause and retain CPU/auto selection until the issue is resolved.

## Completion checklist

- [x] Backend-neutral optics and area-reduction contracts are shared with the
  unchanged CPU reference; cache versions reflect any intentional identity
  change.
- [x] Capture produces a bounded resident f32 camera source without camera-RGB
  readback; optics edits reuse it.
- [x] GPU vignetting, distortion, TCA, cubic boundaries, and exact area
  reduction pass deterministic and tiled/banded parity tests.
- [x] Fit preview and Source 1:1 remain GPU-native after spatial processing;
  ordinary presentation does not read back the full image.
- [x] GPU export reads back only final bounded integer pixels for CPU encoding;
  CPU, auto, and required-GPU recovery semantics remain correct.
- [x] Cache reuse, eviction, cancellation, stale-result rejection, device loss,
  checked limits, and memory-budget behavior are verified.
- [x] `./scripts/check.sh` and both ignored release suites pass.
- [ ] RX 9070 XT 24/48 MP parity, saved visual comparisons, transfer-inclusive
  performance, actual peak memory, interaction, and recovery are recorded.

Implementation is not complete until successful supported GPU runs have no
full-resolution CPU optics bridge, all CPU reference and recovery paths remain
maintained, and hardware/visual evidence is recorded. Append implementation and
qualification evidence here, then update the strategy's milestone status only
to the extent demonstrated.

### Implementation evidence (2026-09-20)

- CPU and GPU share the immutable optics execution view and exact area weight
  tables. The core spatial description carries an in-process immutable-source
  identity without retaining image pixels.
- Capture can scatter completed tile cores into resident planar R32Float arrays
  without camera-RGB readback. GPU optics implements the existing model families
  and cubic footprint; preview reduction uses bounded horizontal/vertical bands.
- Desktop fit preview adopts the reduced GPU texture. Source 1:1 develops the
  resident planes directly into a display texture on the preview worker, in
  128-row completed bands. The UI retains the prior frame until the current
  ticket is ready and only registers the completed texture. Normal GPU export
  evaluates spatial/color processing into final integer bands for CPU encoding.
- Deterministic software-adapter tests cover fractional reductions, forced tile
  and band boundaries, optics component combinations and model families,
  capture on/off, direct display, and integer export. These are numerical and
  structural tests, not hardware or real-image visual qualification.
- The global GPU reservation now covers resident planes, transient capture and
  reduction resources, worker display output, the outgoing UI display frame,
  and export bands. Allocations reserve before device creation; budget refusal
  is a recoverable GPU error that selects the maintained CPU path.
- Optics-only cache reuse, cancellation, cache eviction, stale-ticket rejection,
  and simulated GPU-error recovery have regression coverage. Cancellation does
  not evict the resident source; other GPU errors do.

### f32 numerical and real-image gates

`spatial::tests::private_full_resolution_optics_and_fractional_preview_parity`
uses `DSC00851.ARW` (6000×4000) with its bundled automatic Tamron profile. CPU
Lensfun resolves normalization and Newton intermediates in f64 before rounding
to f32; WGSL executes the same operations in f32. The retained coordinate gate
is therefore 0.001 source pixels, tested at corners and a sparse interior grid.
The exact fractional-reduction camera-linear output gate is 0.002 absolute plus
0.0002 relative error. The observed maximum was `1.4701486e-3` on llvmpipe.

`spatial::qualification::private_spatial_display_parity_and_visual_crops`
compares fit and Source 1:1 display output, and writes CPU/GPU corner, center,
and worst-difference crops when `ROHDITOR_GPU_SPATIAL_ARTIFACTS` is set. On the
same software adapter, fit differed by at most two sRGB codes. Source 1:1 had a
maximum 26-code difference at an isolated high-contrast cubic sample, with 144
channel samples above three codes out of 72 million. Its gate is at most 32
codes and 256 such samples, paired with saved worst-crop review; this preserves
the visible-risk signal instead of averaging it away. The saved CPU/GPU worst
crops showed no visible artifact in this run.

The current execution environment exposes llvmpipe and no `/dev/dri`; these are
software-adapter correctness and review results, not RX 9070 XT measurements.

## Remaining supported-hardware qualification

Run the ignored GPU and workspace suites plus the spatial artifact test on the
RX 9070 XT for representative corrected 24 MP and 48 MP RAWs. Record adapter
identity, fit and Source 1:1 transfer-inclusive timings, measured peak memory,
repeated optics edits, cancellation, device-loss recovery, export-readback
timing, and the saved crop review. Do not substitute llvmpipe results for this
evidence.
