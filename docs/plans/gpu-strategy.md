# GPU processing strategy

Status: active roadmap; 2026-09-14. Milestone 1 is implemented; milestone 2
is implemented with software validation, with hardware qualification still open.

## Destination

Make the GPU the normal execution backend for image development and editing,
with a GPU-resident processing engine shared by preview and export. Keep CPU
RAW decoding, metadata handling, file encoding, and transactional output writes.
Retain a maintained CPU reference and fallback for correctness, unsupported
devices, and recovery.

The objective is predictable interaction, consistent output, and bounded memory,
not GPU utilization by itself. Avoid unnecessary transfers during editing;
multiple tile uploads and final output readbacks are acceptable when required.

Conceptual ownership at the destination:

```text
CPU decode / metadata
        |
        v
GPU sensor development: normalization / reconstruction / demosaic
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

## Starting point

As inspected on 2026-09-14:

- `crates/gpu` owns interactive preview processing and receives a retained
  camera-native RGB source. CPU preparation owns normalization, reconstruction,
  demosaic, capture sharpening, optics, and preview reduction.
- GPU preview applies WB, camera-to-Rec.2020 conversion, exposure, base rendering,
  Light, tone curve, saturation/vibrance, geometry, and output conversion.
  The initial HSL/grading CPU fallback has now been removed by milestone 1;
  see its implementation record for qualification and precision changes.
- Desktop presentation shares the eframe wgpu device and registers the output
  texture directly. Ordinary presentation does not require a CPU readback.
- Export can use the shared GPU color kernels after full-resolution CPU source
  preparation. Encoding accepts a codec-independent `ExportImage` through
  `crates/core/src/export.rs`; see the milestone 2 record below.

## Architectural rules

1. **Keep processing independent of presentation.** Recipe validation and shared
   mathematical contracts stay below the desktop. `crates/gpu` owns GPU execution
   and resource management; desktop owns presentation and interaction. Headless
   export must be able to supply a device without depending on egui. Revisit the
   current CLI-to-GPU dependency prohibition explicitly when integrating GPU
   export; do not introduce a core-to-GPU dependency.
2. **Retain results at meaningful boundaries.** Cache keys include every
   pixel-producing input and algorithm identity. Rerun from the earliest affected
   stage. An HSL edit must reuse RAW development, optics, and the uploaded source.
   Preserve immutable RAW data, typed image states, checked dimension/byte
   arithmetic, cancellation, and rejection of stale document/revision results.
3. **Share algorithms between preview and export.** Use the same stage ordering,
   parameter preparation, and GPU kernels. Make preview resolution and any
   approximate quality mode explicit. Export uses full quality. Define spatial
   radii in a documented coordinate system so zoom and output size do not silently
   change the effect. Share output operations while keeping display and export
   targets independently specified.
4. **Start with an explicit ordered pipeline.** Fuse compatible per-pixel work;
   give neighborhood operations separate passes and reusable scratch resources.
   Introduce a general graph scheduler only when actual dependencies need it.
   Split modules by concrete responsibility; do not build a speculative framework.
5. **Budget memory and work.** Account for source, retained results, scratch, and
   in-flight output against device limits and a memory budget. Support eviction
   and eventually tiles with operation-specific halos. Iterative filters require
   iteration-aware overlap; global statistics require separate reduction work.
   Prioritize recent previews and submit export in bounded work units so it does
   not monopolize interaction. Superseded submitted GPU work may finish, but its
   results must not replace newer results.
6. **Make precision and backend behavior explicit.** Choose intermediate precision
   per stage; do not assume RGBA16F is adequate for every RAW or export operation.
   Define CPU/GPU numerical tolerances and visually qualify results. Preserve an
   observable CPU fallback for unavailable or failed GPU execution. New editing
   features should include GPU support before normal release, with a deliberate,
   documented exception if that cannot be achieved.

Camera calibration, highlight reconstruction, base rendering, and output gamut
remain independent concerns. Keep the existing rendering order: exposure, base
rendering, Light, tone curve, saturation/vibrance, HSL, then grading. Output gamut
mapping follows the working-space edits; rendering profile selection must not
implicitly choose an output gamut policy.

WB is not always downstream-only: Clip reconstruction derives ceilings from WB,
and the current preview can show a temporary WB draft before exact reconstruction.
Preserve this dependency, and capture-sharpening provenance, when moving cache
boundaries. GPU residency does not make upstream dependencies disappear.

## Milestones

| Order | Deliverable | Completion evidence |
| --- | --- | --- |
| 1 | Complete GPU HSL and three-way grading | Implemented with hardware/corpus parity; desktop latency and skin-tone review remain open. See [implementation record](gpu-hsl-and-grading.md). |
| 2 | Share the existing GPU color pipeline with export | Implemented: headless execution, shared color kernels, full-resolution CPU camera RGB, direct 8/16-bit quantization, CPU encoding, bounded readback, and resource fallback. Software parity passed; RX 9070 XT parity, visual review, and interaction measurements remain open. |
| 3 | Move existing capture sharpening to GPU | Correct full-resolution camera RGB boundary before optics/reduction, reusable spatial scratch, CPU parity, source-pixel radius semantics, bounded memory and scheduling. |
| 4 | Move optics and preview resampling to GPU | Retain an earlier camera RGB source; optics edits reuse it; TCA, distortion, boundaries, and downsampling quality match the intended contract. |
| 5 | Move sensor development to GPU | Progress through normalization and reconstruction, then a bounded first demosaic method such as MHC, followed by measured higher-quality methods. Preserve CFA phase, sensor coordinates, highlight dependencies, signed/HDR data, and CPU fallback for unsupported methods. |

Milestone 3 changes the upload boundary to support full-resolution spatial work;
do not sharpen an already reduced preview as a substitute. Its algorithm remains
owned by [capture sharpening](capture-sharpening.md).

Milestone 4 may combine warping and reduction only with suitable antialiasing.
A single bilinear sample is not sufficient for substantial downsampling. Measure
the extra full-resolution upload and memory cost as well as saved CPU work.

Tiling is required wherever supported workloads exceed resource budgets; it is
not a universal shader wrapper. Qualify tiled output against untiled output,
including seams, global coordinates, and iterative neighborhood dependencies.

## Milestone 2 implementation and qualification

`CpuPipeline::prepare_export_source` shares the preview RAW, capture-sharpening,
and optics preparation with no resolution reduction. Exact Clip/WB, camera
profile, sharpening, and optics provenance remain mandatory. CPU reference
export is retained. `GpuExportProcessor` accepts a supplied wgpu device or creates
a headless hardware Vulkan device; there is no egui or core-to-GPU dependency.
The CLI-to-GPU prohibition in `scripts/check.sh` now permits this application
dependency while retaining the core and desktop dependency restrictions.

Preview and export call the same WGSL color and output-conversion functions and
use the same Rust uniform/LUT preparation. Source pixels and fused color work
remain f32. Export never passes through the RGBA16F working texture or RGBA8
display texture. Its final stage quantizes directly to 8/16-bit sRGB, including
the CPU ordered 8x8 dither and rounding contract. Crop, EXIF orientation, and
dither coordinates are global across the 64-row output bands. Each band has one
submission/readback in flight; readback currently uses u32 per RGB sample before
CPU packing into `ExportImage`. CPU encoding and transactional writes are shared.

Per-export GPU allocations are conservatively limited to 768 MiB, including
the full RGBA32F source, upload staging, output/readback bands, and LUTs. CPU
source/packing/staging/output buffers also respect the existing 2 GiB working-set
limit. These are buffer estimates, not measured RSS or driver heap peaks. Source
dimensions, storage-buffer sizes, and dispatch counts are checked against device
limits before upload. Oversized sources fall back to CPU; source tiling is not
implemented. GPU allocation, validation, synchronization, and readback errors
return before encoding. Cancellation is checked during preparation/packing and
between bounded submissions/readbacks.

CLI `develop --processor auto|cpu|gpu` defaults to auto; auto reports GPU failure
and uses CPU, while gpu reports failure without encoding a replacement result.
Desktop auto/gpu preferences attempt GPU export and report CPU recovery; cpu
forces CPU. A separate serial export worker retains its headless processor and
allows one active plus one queued snapshot, so export preparation/encoding no
longer occupy the preview worker. Export events retain document/export/revision
identities. Preview and export devices remain separate in this slice; combined
hardware memory use and scheduling responsiveness still need qualification.
GPU output-gamut pixel counters are explicitly unavailable, not reported as zero.

Software validation on 2026-09-14 used llvmpipe, LLVM 23.1.0, Mesa 26.2.2; this
environment had no `/dev/dri`. The asymmetric 38x142 fixture covers all eight
orientations, a nontrivial crop, both integer depths, both dither modes, existing
color controls, matched-resolution preview output, cancellation, and limit
rejection. Export comparisons allow at most 2 codes at 8-bit and 16 codes at
16-bit; corpus mean-error limits are 0.1 and 1 code respectively. Expanded 8-bit
values cannot satisfy the explicit 16-bit precision check.

The full-resolution software fixture uses DSC00851.ARW at 6000x4000, combined
Light/WB/HSL/grading controls, both gamut policies, and ordered dithering. It
observed maximum differences of 1 code for 8-bit and 16-bit clipping, and 11 codes
for 16-bit chroma compression. An initial 18-code chroma-compression discrepancy
was reduced by refining WGSL's cube-root approximation before OKLab conversion;
the tolerance was not widened. CPU gamut semantics and algorithm version remain
unchanged. One source upload is 384,000,000 bytes; output reads 288,000,000 bytes
in 63 bands; the conservative GPU allocation estimate is 777,281,536 bytes.
Software timings are structural evidence only, not RX 9070 XT performance.

- [x] Headless execution, full-quality preparation, shared color kernels, direct
  8/16-bit output, deterministic geometry/dither, and CPU encoding are connected.
- [x] Software asymmetric and full-resolution RAW comparisons pass.
- [x] `./scripts/check.sh`, the ignored release workspace suite, and the ignored
  release GPU suite complete successfully. Hardware-only tests skip on this
  software adapter. The private desktop snapshot-export test also passes after
  worker separation. CLI auto fallback produces a byte-identical 16-bit PNG to
  explicit CPU export; required-GPU failure creates no output file.
- [ ] RX 9070 XT hardware/corpus parity at both bit depths, including visual
  review of skin, saturated colors, shadows, and reconstructed highlights.
- [ ] Cold/warm device and source costs, hardware export throughput, actual peak
  memory, concurrent preview latency, and rapid-edit/stale-result review.
- [ ] Hardware failure/recovery and source-budget fallback qualification under
  device pressure. Source tiling remains a later workload-expansion requirement.

## Qualification and completion

For each implementation slice, run `./scripts/check.sh`. GPU changes require
`cargo test --release -p rohditor-gpu -- --ignored --nocapture`; decoder and
full-resolution changes additionally require
`cargo test --release --workspace --tests -- --ignored --nocapture`.

Use small asymmetric correctness fixtures and representative RAW images. Record
actual adapter/driver, dimensions, recipe, quality mode, and warm/cold cache state.
Measure image-open/source preparation, resident slider-to-display latency,
uploads/readbacks, export throughput, peak memory, and responsiveness separately.
Use the RX 9070 XT for hardware qualification; software Vulkan or skipped tests
cannot establish hardware parity or performance. Record unavailable checks and
leave their acceptance items open.

The strategy is complete when supported RAW development and editing execute
through the resident GPU engine, preview and export share processing semantics,
ordinary edits avoid unnecessary transfers and CPU fallback, and memory-limited
workloads and unavailable GPUs have qualified behavior. Do not retire the plan
merely because the first color milestone ships.

This document owns GPU architecture and sequencing. Algorithm decisions for
reconstruction, rendering, and gamut remain in
[clipping and reconstruction](clipping-and-reconstruction.md).
