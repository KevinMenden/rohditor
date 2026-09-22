# GPU processing strategy

Status: active roadmap; 2026-09-21. Milestones 1 through 4 are implemented.
Milestone 4 has software-adapter parity and lifecycle coverage, but its RX 9070
XT, real-image, physical-memory, and interaction qualification remains open.
The remaining feature implementation is milestone 5: move normalization, RAW
highlight handling, and demosaic to the resident GPU path.

## Destination

Make the GPU the normal execution backend for image development and editing,
with a GPU-resident processing engine shared by preview and export. Keep CPU
RAW decoding, metadata handling, file encoding, and transactional output writes.
Retain a maintained CPU reference and fallback for correctness, unsupported
devices, unsupported algorithms, resource limits, and recovery.

The objective is predictable interaction, consistent output, and bounded memory,
not GPU utilization by itself. Avoid unnecessary transfers during editing;
bounded RAW uploads and final export readbacks are acceptable when required.

Conceptual ownership at the destination:

```text
CPU RAW decode / metadata
        |
        v
GPU sensor development: normalization / highlight handling / demosaic
        |
        v
GPU camera RGB processing: capture sharpening / optics / calibration
        |
        v
GPU working-space processing: rendering / tone / color / detail / local edits
        |
        +--> display transform --> native GPU display texture
        |
        +--> export transform --> final pixel readback --> CPU encoding
```

This is an ownership diagram, not a new algorithm order. Preserve current
processing semantics during each migration. New detail and local operations
need an explicit processing domain and position when designed.

## Current implemented boundary

As inspected on 2026-09-21, the successful GPU path is:

```text
CPU decode / metadata / normalization / RAW highlight handling / demosaic
  -> full-resolution camera-native GPU upload or bounded capture-tile uploads
  -> optional bounded GPU capture sharpening
  -> resident GPU optics and exact fit-preview area reduction
  -> shared GPU color and output processing
  +-> native fit or Source 1:1 display texture
  +-> bounded final integer export readback -> CPU encoding
```

- `crates/gpu` owns resident camera RGB after demosaic. Capture sharpening,
  optics, exact area reduction, fit preview, Source 1:1, and integer-band export
  can complete without a full-resolution camera-RGB readback or a second upload.
- Fit preview and Source 1:1 presentation use native GPU textures. Ordinary
  presentation has no image readback; picker, histogram, tests, and diagnostics
  may use explicit bounded readbacks.
- Preview and export share the spatial, color, gamut, and output contracts but
  still use separate devices and resident copies. CPU owns JPEG/PNG encoding,
  metadata, cancellation-aware orchestration, and transactional writes.
- The remaining CPU pixel stages before GPU residency are normalization, the
  selected RAW highlight method, and the selected demosaic algorithm. Their
  materialized f32 mosaic and RGB result are the boundary milestone 5 replaces.

## Architectural rules

1. **Keep processing independent of presentation.** Recipe validation and shared
   mathematical contracts stay below the desktop. `crates/gpu` owns GPU execution
   and resource management; desktop owns presentation and interaction. Headless
   export supplies a device without depending on egui. Core must not depend on
   the GPU crate.
2. **Retain results at meaningful boundaries.** Cache keys include every
   pixel-producing input and algorithm identity. Rerun from the earliest affected
   stage. An HSL edit reuses sensor development, optics, and the resident source.
   Preserve immutable RAW data, typed image states, checked dimension/byte
   arithmetic, cancellation, and rejection of stale document/revision results.
3. **Share algorithms between preview and export.** Use the same stage ordering,
   parameter preparation, and GPU kernels. Make preview resolution and any
   approximate quality mode explicit. Export uses full quality. Define spatial
   radii in source coordinates so zoom and output size do not silently change an
   effect. Share output operations while keeping display and export targets
   independently specified.
4. **Use an explicit ordered pipeline.** Fuse compatible per-pixel work; give
   neighborhood operations separate passes and reusable scratch resources.
   Introduce a general graph scheduler only when actual dependencies require it.
   Split modules by concrete responsibility; do not build a speculative framework.
5. **Budget memory and work.** Account for source, retained results, scratch,
   staging, and in-flight output against device limits and the global reservation.
   Tile only where the operation and supported workload require it, with the
   correct halo. Prioritize recent previews and keep export submissions bounded.
   Superseded submitted work may finish, but must not replace newer results.
6. **Make precision and backend behavior explicit.** Sensor, camera-native, and
   fused color work remain f32 unless a separately qualified contract says
   otherwise. Define CPU/GPU tolerances and visually qualify results. Preserve an
   observable CPU reference and fallback for unavailable or failed GPU execution.
   New editing features should include GPU support before normal release, with a
   deliberate documented exception if that cannot be achieved.

Camera calibration, highlight reconstruction, base rendering, and output gamut
remain independent concerns. Keep the existing rendering order: exposure, base
rendering, Light, tone curve, saturation/vibrance, HSL, then grading. Output gamut
mapping follows the working-space edits; rendering profile selection must not
implicitly choose an output gamut policy.

WB is not always downstream-only: Clip derives RAW ceilings from WB, while the
current preview may display a temporary WB draft from the previous exact source.
Preserve that dependency, capture-sharpening provenance, and settled-result rules
when moving the upstream cache boundaries. GPU residency does not make upstream
dependencies disappear.

## Milestones

| Order | Deliverable | Status and remaining evidence |
| --- | --- | --- |
| 1 | Complete GPU HSL and three-way grading | Implemented with RX 9070 XT and corpus parity. Slider-to-display latency and a representative skin-tone review remain open; see the [implementation record](gpu-hsl-and-grading.md). |
| 2 | Share the GPU color pipeline with export | Implemented with headless execution, shared kernels, direct 8/16-bit quantization, bounded final readback, CPU encoding, and fallback. RX 9070 XT export parity, visual review, throughput, physical memory, concurrent-preview behavior, and failure recovery remain open. |
| 3 | Move capture sharpening to GPU | Implemented as bounded f32 tiles. Milestone 4 removed the temporary camera-RGB readback from the normal resident path. Capture On parity, memory, interaction, and recovery remain part of the combined milestone 4 hardware qualification; the algorithm remains owned by [capture sharpening](capture-sharpening.md). |
| 4 | Move optics and preview resampling to GPU | Implementation and software-adapter qualification are complete: capture residency, optics, exact area reduction, fit preview, Source 1:1, and integer-band export are connected. RX 9070 XT 24/48 MP evidence remains open; see the [implementation and evidence record](gpu-optics-and-preview-resampling.md). |
| 5 | Move sensor development to GPU | Planned below. Implement normalization, all current RAW highlight methods, MHC, then RCD and AMaZE, while preserving sensor coordinates, CFA phase, signed/HDR data, diagnostics, cache dependencies, and explicit fallback. |

The old milestone 3 transfer-inclusive capture timing described a temporary
`GPU capture -> CPU optics -> GPU color` bridge. It remains useful historical
evidence but is not the path milestone 5 should extend. New measurements must use
the milestone 4 resident spatial path.

## Remaining work in implementation order

The next work is intentionally split into independently reviewable vertical
slices. Do not enable a slice as the normal backend merely because its shader
executes. Each slice must preserve the CPU reference, provenance, resource
failure behavior, and the relevant parity gates.

### 0. Record the current resident-path baseline

Before milestone 5 changes the upload boundary, run and record the current
milestone 1 through 4 path on the RX 9070 XT. This supplies the comparison point
for source preparation, transfers, memory, and interaction.

- Run both ignored release suites and the milestone 4 artifact-producing parity
  test `spatial::qualification::private_spatial_display_parity_and_visual_crops`
  on corrected 24 MP and 48 MP RAWs, with capture Off and On. Set
  `ROHDITOR_GPU_SPATIAL_ARTIFACTS` to retain the reviewed crops.
- Record adapter, driver, device limits, resident layout, work-unit sizes, cold
  and warm timings, bytes uploaded/read back, reservation peaks, and measured
  host/device memory.
- Measure fit preview, Source 1:1, 8/16-bit export, repeated optics edits,
  cancellation, constrained-budget recovery, device-loss recovery, rapid edits,
  and preview p50/p95 latency during concurrent export.
- Save and review matched CPU/GPU crops for optics corners, high-frequency detail,
  skin, saturated colors, deep shadows, and reconstructed highlights. This pass
  may also close milestone 1's skin-tone review and milestone 2's export gates,
  but each acceptance item must be reported explicitly.

This baseline is required before enabling GPU sensor development. Isolated
contract extraction and shader fixture work may proceed without claiming a new
normal path.

## Milestone 5: GPU sensor development

### Goal, scope, and non-goals

Start from the immutable CPU-decoded `RawFrame`, upload bounded u16 RAW tiles,
and produce the existing resident camera-native f32 planes without materializing
a full normalized mosaic or demosaiced RGB image on the CPU. Preview, Source 1:1,
and export must invoke the same GPU sensor kernels and then continue through the
implemented capture/optics/color path.

Preserve the current CPU order and semantics:

```text
immutable decoded RAW u16 mosaic
  -> recommended/full crop and black/white normalization
  -> selected RAW highlight method
  -> selected Bayer demosaic
  -> optional capture sharpening
  -> resident optics / reduction / color / output
```

This milestone does not move RAW decoding or metadata parsing to GPU, change RAW
crop defaults, change highlight or demosaic algorithms, add a new demosaic mode,
change recipe defaults, merge preview and export devices, encode files on GPU,
or introduce a graph scheduler. The existing CPU implementations remain the
authoritative reference and recovery path.

Planning assumptions:

- The decoded u16 mosaic remains immutable and available on the CPU for recovery.
- The current 768 MiB global GPU reservation and 2 GiB CPU working-set limit stay
  unchanged until measured evidence justifies a separate policy decision.
- The first normal GPU sensor path targets the default MHC demosaic. Bilinear is
  implemented because MHC uses it at borders and because it is a useful reference.
  RCD follows, then AMaZE. Until a selected method is qualified, that request uses
  the existing CPU-sensor/GPU-spatial path under `auto` and reports the fallback.
- Intermediate sensor and camera data use R32Float/f32. No RAW or demosaic stage
  may pass through RGBA16F.

### 1. Extract backend-neutral sensor contracts

Add a pixel-storage-independent sensor-development description in a focused core
pipeline module such as `crates/core/src/pipeline/sensor.rs`. It is built from
`RawFrame`, `EditRecipe`, and `PreviewOptions` and must contain only validated
data needed to execute the current algorithms:

- decoded dimensions and visible row stride;
- resolved RAW crop origin/dimensions and the shifted Bayer phase;
- black-level repeat dimensions, values, and sensor-coordinate indexing;
- per-channel/repeating white-level interpretation;
- camera calibration, resolved WB context, source orientation, and metadata
  fallbacks needed by downstream provenance;
- the selected highlight method with resolved Clip ceilings or method-specific
  detection levels and algorithm versions;
- selected demosaic algorithm, its border policy, required halo, and algorithm
  identity; and
- the identities and timing fields needed to construct the existing spatial
  completion description after demosaic.

The CPU normalization adapter must consume the same crop and level-resolution
contract before GPU work is integrated. Highlight execution contracts should be
owned by `rohditor-highlight`, and demosaic constants/halos by
`rohditor-demosaic`; do not copy recipe interpretation or pixel-producing
constants into the GPU crate. Core continues to own orchestration and must not
gain a dependency on wgpu.

Introduce explicit GPU-only typed states under `crates/gpu/src/sensor/`, for
example normalized mosaic, highlight-completed mosaic, and demosaiced resident
camera source. A state can advance only with matching source and algorithm
provenance. Partial device results are never converted into CPU typed states or
published as complete.

Acceptance:

- Existing CPU preview, Source 1:1, and export remain bit-identical after
  contract extraction.
- Contract tests cover crop offsets, all four Bayer phases, patterned black
  levels, every accepted white-level form, odd dimensions, padded RAW strides,
  invalid metadata, and checked dimension arithmetic.
- Cache identities change only where the extracted contract reveals a missing
  pixel-producing input; intentional identity changes receive an algorithm or
  representation version.

### 2. Implement bounded GPU normalization

Create focused `contract`, `normalize`, `highlight`, `demosaic`, `resources`,
and `qualification` modules under `crates/gpu/src/sensor/`. Let the existing
ordered spatial processor own or call this sensor executor; do not create a
second general pipeline abstraction.

For normalization:

1. Resolve and validate crop, levels, CFA phase, dimensions, device limits, and
   the complete memory lifetime before allocating or submitting work.
2. Pack one bounded crop-local u16 tile from the immutable decoded RAW. Upload it
   as an unfiltered `R16Uint` texture and access it with integer `textureLoad`.
   Treat lack of the required integer-texture capability as typed unsupported
   behavior before partial execution.
3. Normalize into one tiled R32Float mosaic using
   `(sample - black) / (white - black)`. Black-level lookup uses absolute sensor
   coordinates; CFA and white-level lookup use the same current CPU rules. The
   resident mosaic uses crop-local global coordinates and the correctly shifted
   Bayer phase.
4. Reuse one upload tile and staging allocation. Do not retain a full u16 device
   copy after its samples have been normalized. Preserve negative and above-one
   values and reject non-finite output.

The first implementation may retain the one full f32 normalized mosaic while
highlight handling and demosaic run. A separate highlight-output mosaic is
allowed when the algorithm cannot safely overwrite its input, but the input must
be released before allocating demosaic RGB. Do not retain duplicate mosaics
merely for cache convenience. Rebuilding from immutable decoded RAW is the
bounded fallback when there is no room to retain an upstream mosaic.

Acceptance:

- Compare the actual shader result against `normalize_raw_cancellable`, not a
  rewritten test formula.
- Exercise every Bayer phase, even and odd crop origins, black-level repeat
  boundaries, channel white levels, zero/maximum u16 samples, below-black and
  above-white results, padded source rows, tiny images, tile boundaries, and
  nontrivial recommended crops.
- Successful execution uploads only the selected u16 crop and performs no
  full-image readback. Cancellation and resource failure publish nothing and
  leave the immutable CPU frame usable for recovery.

### 3. Move all current RAW highlight methods

Implement highlight handling against the normalized R32Float mosaic before
demosaic. Preserve the algorithms and diagnostics owned by
`rohditor-highlight`; this is an execution port, not a redesign.

#### 3a. Off and Clip

- `Off` transfers the normalized mosaic state without a traversal.
- `Clip` resolves the existing WB-dependent per-channel ceilings on the CPU,
  then writes a highlight-completed mosaic with the current
  affected/changed/nominal-over-white rules. A small counter buffer records the
  existing channel and total diagnostics. Fuse Clip with normalization only
  after the separate passes establish parity and the fusion has its own tests.
- A Clip WB or camera-profile dependency change rebuilds highlight handling and
  every downstream stage. During a drag, the current marked WB draft may remain
  visible; a settled preview, Source 1:1, and export require exact new Clip
  provenance.

Off and Clip plus MHC form the first end-to-end milestone 5 path because Clip is
the current default. Do not enable that path until section 4's demosaic and
integration gates pass.

#### 3b. Local Ratios and Opposed

Port the existing two-stage structure rather than approximating either method:

1. Build immutable summaries for each logical 2x2 Bayer cell: three f32 means
   and the existing usable/clipped flags.
2. Dispatch one reconstruction invocation per suspected site. Preserve radius
   order, candidate eligibility, fixed candidate limits, median selection,
   cross-channel guards, estimate bounds, and explicit fallback behavior.
3. Write the completed mosaic from the immutable normalized input. Release that
   input, cell summaries, and counter scratch before allocating the full RGB
   demosaic destination.
4. Read back only the bounded diagnostic counters. Do not read the reconstructed
   mosaic back for normal processing.

Use deterministic fixed-size local arrays or an equivalently bounded selection
implementation for the 24 Local-Ratios and 48 Opposed candidates. A different
median, neighborhood order, or confidence rule is an algorithm change and is
out of scope. If WGSL math changes a threshold decision, investigate and either
match the reference or treat the change as a separately versioned algorithm;
do not hide it with a broad final-image tolerance.

Until a method passes its complete gates, selecting it uses the existing CPU
sensor path and reports why GPU sensor development was not selected. Do not add
a normalized-mosaic readback bridge solely to obtain partial GPU utilization.

Acceptance:

- Cover every Bayer phase, odd last cells, border-touching regions, one- and
  two-channel support, insufficient evidence, dark support, candidate-limit
  cases, fully unsupported sites, signed values, HDR values, and parameter
  extremes.
- CPU/GPU diagnostics and accept/fallback decisions must match. Pixel comparison
  is stage-local and occurs before demosaic.
- Scratch and counters use checked sizes, fit the reservation, are cancellable
  between bounded submissions, and are released before the demosaic destination
  reaches its peak lifetime.

### 4. Implement bilinear and MHC into resident camera planes

Port bilinear first as the border/reference helper, then MHC as the first normal
demosaic. Both read the highlight-completed tiled mosaic through a global
crop-local loader and write the existing three planar R32Float camera textures.

Preserve these contracts exactly:

- all four shifted Bayer phases and original measured CFA values;
- bilinear's available-neighbor edge averaging;
- MHC's published 5x5 integer-over-16 kernels and two-pixel bilinear border;
- signed and over-range camera RGB without clipping;
- identity WB at demosaic, because WB remains in downstream color processing;
  and
- finite-output validation before publishing resident camera RGB.

For capture sharpening Off, demosaic directly into the final resident camera
planes and release the mosaic after successful completion. For active capture,
avoid requiring two full resident RGB images at large dimensions: demosaic each
capture input tile, including the capture halo plus the two-pixel MHC halo, from
the immutable reconstructed mosaic into bounded capture input scratch. Run the
existing eight-iteration capture passes and scatter only the valid core into the
final resident planes. True image edges retain the demosaic and capture border
rules; artificial tile edges cannot influence a retained core.

This direct MHC-to-capture handoff is part of the first slice, not a later
optimization. Otherwise a 48 MP active-capture request would require the full
mosaic, an unsharpened RGB image, a captured RGB image, and scratch at the same
time. At exactly 48 million sites, one f32 mosaic plus three f32 RGB planes
already occupy 768,000,000 logical bytes before tile padding, staging, capture
scratch, or output. Budget using actual padded extents and overlapping lifetimes.
If the minimum valid combined work unit still cannot fit, return a typed resource
error and apply the selected backend policy.

Acceptance:

- Small asymmetric fixtures cover constants, impulses, affine fields, color
  edges, checkerboards, signed/HDR samples, minimum dimensions, the complete
  bilinear border, MHC interior/border transitions, forced mosaic-tile crossings,
  forced capture-tile crossings, and non-finite rejection.
- Measured CFA sites are preserved within the stated numerical budget. Compare
  camera RGB before capture, after capture, and after final output separately.
- A successful default path is
  `CPU decode -> GPU normalize -> GPU Clip -> GPU MHC -> resident capture/optics`
  with no full f32 mosaic or camera-RGB CPU materialization and no image readback.

### 5. Integrate preview, Source 1:1, export, caches, and recovery

Add one sensor-development entry point to the ordered GPU processor and use it
from both desktop preview preparation and `GpuExportProcessor`. It consumes an
immutable decoded frame plus the backend-neutral description and produces the
same resident demosaiced boundary that capture/optics already consume.

Cache identities and invalidation boundaries are:

- decoded RAW: file/source identity and decoder output identity;
- normalized mosaic: decoded identity, crop geometry, level contract,
  normalization algorithm identity, CFA phase, and GPU layout version;
- highlight-completed mosaic: normalized identity plus method, method-specific
  settings and version, and Clip's resolved WB/profile-dependent ceilings;
- demosaiced resident source: highlighted identity plus demosaic algorithm and
  implementation version;
- capture-completed source: demosaiced identity plus capture settings, ceilings,
  and capture algorithm identity; and
- optics/reduction/downstream output: the existing milestone 4 and color keys.

These are invalidation boundaries, not a requirement to retain every image.
Keep the currently displayed frame and the useful capture-completed source
first. Retain a normalized or highlight-completed mosaic only when the global
reservation has room; otherwise rebuild it from immutable decoded RAW. Release
the mosaic once the resident RGB boundary is complete when that is necessary to
fit optics, Source 1:1 output, or export bands.

CPU selection bypasses GPU sensor development. `auto` attempts the qualified GPU
sensor path; unsupported algorithms, device limits, allocation/validation errors,
non-finite flags, synchronization failure, or device loss record a specific
reason and restart from the immutable `RawFrame` through the existing CPU-sensor
path. Cancellation and supersession are not fallback triggers. Required-GPU
export must not encode a replacement after a GPU execution failure. No recovery
path may continue from a partially normalized, reconstructed, or demosaiced
device state.

Preserve the serial export worker's one-active/one-queued snapshots, desktop
document/revision checks, old-frame-until-replacement behavior, and separate
preview/export devices. Diagnostics must state whether sensor development and
spatial/color processing were GPU or CPU; a CPU-sensor/GPU-spatial recovery must
not be labeled as fully GPU-developed.

Acceptance:

- Fit preview, Source 1:1, and 8/16-bit export use the same sensor contracts and
  kernels for a matching algorithm and recipe.
- Cache tests cover crop, each highlight method, Clip/WB, demosaic changes,
  capture changes, optics-only edits, downstream-only edits, eviction, preview
  size changes, cancellation, rapid document switches, device loss, and recovery.
- Instrumented default-path runs prove one crop's worth of bounded u16 RAW tile
  uploads, zero full normalized/demosaiced image readbacks, no camera-RGB upload,
  and only final bounded export readbacks.

### 6. Port RCD, then AMaZE

After the default MHC path is qualified, port the remaining selectable methods
in this order: RCD, then AMaZE. Keep each as a separate reviewable slice.

- Share the existing constants, epsilon policies, fixed tile geometry, halos,
  border fallback, and measured-site preservation contract from
  `rohditor-demosaic`.
- Implement explicit stage passes matching the CPU order. Use bounded per-tile
  scratch and write only the CPU algorithm's valid core. Tile origins and CFA
  phase are global; a GPU work-unit boundary must not become an image boundary.
- RCD retains its bilinear base, 10-pixel border, low-pass/direction stages, and
  ratio-corrected interpolation. AMaZE retains its bilinear outer band,
  16-pixel halo, gradient/green/chroma stages, and current highlight guards.
- Establish intermediate-plane parity before comparing only final RGB. Optimize
  pass fusion or parallel tile batches only after the unfused baseline passes.
- For active capture, use a bounded direct tile handoff only if it can reproduce
  the CPU method's fixed-tile semantics. Otherwise materialize separate resident
  demosaic and capture images only when the reservation permits; under pressure,
  use the explicit CPU-sensor fallback rather than changing the algorithm.

Do not change the desktop default as part of these ports. After both are
qualified, compare complete-path image quality, latency, and memory against MHC
before making any separate default-quality decision.

## Milestone 5 validation and qualification

### Numerical gates

Freeze stage-local tolerances before enabling each slice. Initial gates to
validate, rather than numbers to widen automatically, are:

- normalization: `2e-7 + 2e-7 * abs(cpu)` per mosaic sample;
- Clip: exact changed/affected decisions and `2e-7 + 2e-7 * abs(cpu)` per sample;
- Local Ratios/Opposed: exact reconstruction/fallback decisions and diagnostics,
  with `2e-5 + 2e-5 * abs(cpu)` for reconstructed samples;
- bilinear/MHC camera RGB: `2e-5 + 2e-5 * abs(cpu)` per channel; and
- RCD/AMaZE camera RGB: begin with the MHC gate, inspect intermediate stages,
  and approve any larger algorithm-specific gate only from observed f32 error.

Final display/export keeps the established gates: at most two codes at 8-bit
and 16 codes at 16-bit, with corpus mean errors no greater than 0.1 and one code
respectively. Exact bypass behavior stays exact. A final-output pass does not
excuse a wrong CFA phase, highlight decision, border, or measured sensor site.

### Deterministic and integration fixtures

In addition to the stage-specific cases above, cover:

- all eight orientations, nontrivial output crop, both output-gamut policies,
  8/16-bit output, both dither modes, and capture Off/On;
- forced RAW upload, mosaic layout, highlight, demosaic, capture, optics, and
  export band boundaries, including odd dimensions and padded source strides;
- multiple constrained budgets, minimum valid work units, limit rejection,
  cancellation between submissions, simulated allocation/synchronization/device
  failure, and stale-result rejection; and
- comparison of the new path with both the CPU reference and the currently
  implemented CPU-sensor/GPU-spatial path, so downstream differences are not
  misattributed to sensor development.

Run for every implementation slice:

```sh
./scripts/check.sh
cargo test --release --workspace --tests -- --ignored --nocapture
cargo test --release -p rohditor-gpu -- --ignored --nocapture
```

Software Vulkan establishes shader, dispatch, and deterministic fixture
behavior only. It does not establish RX 9070 XT parity, performance, memory, or
interaction quality.

### Real-image review

Save matched CPU and GPU fit, Source 1:1, and export comparisons from the Sony
corpus. Include:

- fine foliage, hair, fabric, roof tiles, and diagonal structure for false color,
  zippering, and demosaic seams;
- clipped clouds, neutral speculars, colored lights, saturated object edges, and
  large unsupported regions for every highlight method;
- high-ISO shadows and flat areas for noise texture and capture interaction;
- skin, saturated colors, strong lens corners, and reconstructed highlights; and
- 12-bit and 14-bit inputs, plus corrected 24 MP and representative 48 MP files.

Review stage-matched crops at Source 1:1. Fit previews alone cannot qualify
demosaic output, and numerical parity alone cannot establish visible quality.

### Hardware, memory, and interaction

On the RX 9070 XT, record adapter/driver, dimensions, RAW bit depth, crop,
highlight and demosaic selections, capture/optics settings, tile/halo sizes,
budget, and cold/warm state. Measure separately:

- CPU decode/metadata and sensor-contract preparation;
- u16 packing/upload, normalization, highlight passes, demosaic, capture,
  optics/reduction, color/output, presentation, and export readback;
- uploaded/read-back bytes versus the milestone 4 baseline;
- reservation peaks and actual host/device/driver memory, including concurrent
  preview and export devices;
- first-open, warm downstream edit, Clip WB settle, highlight-method change,
  demosaic change, capture edit, optics edit, Source 1:1 entry, and export time;
- cancellation latency, rapid-edit newest-wins behavior, and preview p50/p95
  latency during concurrent export; and
- constrained-budget fallback, unsupported-algorithm fallback, allocation
  failure, synchronization failure, and device-loss recovery.

Compare complete paths, not isolated shader time. Enable GPU sensor development
as the normal `auto` path only where transfer-inclusive latency and interaction
are no worse than the maintained CPU-sensor/GPU-spatial path and memory stays
within the reservation. Record the backend-selection decision per demosaic
algorithm; one successful MHC result does not qualify RCD or AMaZE.

## Completion checklist

- [ ] Record the current RX 9070 XT milestone 1 through 4 baseline and close or
  explicitly carry forward each open visual, performance, memory, interaction,
  and recovery item.
- [ ] Share validated normalization, highlight, and demosaic execution contracts
  with the unchanged CPU reference.
- [ ] GPU normalization preserves crop geometry, sensor-coordinate levels, CFA
  phase, signed/HDR values, and bounded upload/memory behavior.
- [ ] Off, Clip, Local Ratios, and Opposed preserve CPU decisions, diagnostics,
  recipe dependencies, and algorithm versions.
- [ ] Bilinear and MHC produce resident camera planes and active capture can use
  a bounded direct handoff without a full CPU or duplicate-RGB bridge.
- [ ] Preview, Source 1:1, and export share the GPU sensor path, cache identity,
  cancellation, stale-result rejection, and observable recovery semantics.
- [ ] RCD and AMaZE pass stage-local, tile-boundary, corpus, and supported-hardware
  qualification. A decision not to port either method requires an explicit
  strategy revision and documented CPU-sensor fallback; it does not satisfy this
  checklist item.
- [ ] `./scripts/check.sh` and both ignored release suites pass for the completed
  implementation.
- [ ] RX 9070 XT 24/48 MP parity, saved visual review, transfer-inclusive
  performance, actual peak memory, interaction, concurrency, and recovery are
  recorded for every algorithm enabled by `auto`.

The strategy is complete when supported RAW development and editing execute
through the resident GPU engine, preview and export share processing semantics,
ordinary edits avoid unnecessary transfers and CPU fallback, and memory-limited
workloads and unavailable GPUs have qualified observable behavior. Do not retire
the roadmap while an algorithm selected by the normal UI silently takes an
unreported path.

This document owns GPU architecture and sequencing. Algorithm decisions for RAW
highlight reconstruction, base rendering, and gamut remain in
[highlight reconstruction, base rendering, and output gamut mapping](clipping-and-reconstruction.md).
