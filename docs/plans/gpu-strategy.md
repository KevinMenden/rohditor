# GPU processing strategy

Status: proposed roadmap; 2026-09-14. No implementation is claimed by this plan.

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
- Export processing is CPU-based. Encoding already accepts a codec-independent
  `ExportImage` through `crates/core/src/export.rs`.

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
| 2 | Share the existing GPU color pipeline with export | Headless execution, same color kernels, full-quality processing, explicit 8/16-bit output precision, CPU encoding, and preview/export agreement at matched resolution. Start with CPU-prepared camera RGB; define device-limit and memory fallback before enabling. |
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
