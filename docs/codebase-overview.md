# Rohditor codebase overview

Rohditor is a Linux-first Rust RAW editor for Sony ILCE-6400 ARW files. It has
two execution modes: a deterministic, headless CPU pipeline used for
correctness and export, and an optional GPU preview path used by the desktop
application for interactive editing.

## The main boundaries

The workspace is organized from lower-level image and file representations to
application code:

```text
rohditor-image  <-  rohditor-edit
        ^        <-  rohditor-demosaic
        ^        <-  rohditor-raw
        ^             ^
        +--------- rohditor-core --------- rohditor-gpu
                       ^   ^                    ^
                       |   +--------------------+
                   CLI and desktop applications
```

The arrows describe dependency direction. Lower layers do not depend on the
desktop UI. `rohditor-image` is the foundation; `rohditor-edit`,
`rohditor-demosaic`, and `rohditor-raw` are independent domain libraries;
`rohditor-core` combines them; `rohditor-gpu` provides an optional interactive
processor; and `apps/` contains user-facing orchestration.

## Libraries

### `crates/image` — typed image states and geometry

`rohditor-image` owns the basic image vocabulary and checked buffers:

- `MosaicImage<T>` for one-channel Bayer sensor data;
- `LinearRgbImage<T>` for scene-linear RGB data, with an explicit color space;
- `DisplayRgbImage<T>` for display-encoded output;
- Bayer patterns, image regions, allocation/layout validation, and
  `OrientationMap`.

These types make processing stages explicit. A raw mosaic, a linear working
image, and an sRGB display image are not interchangeable `Vec`s.

### `crates/edit` — the edit recipe

`rohditor-edit` defines `EditRecipe`, its validated adjustment groups, defaults,
parameter ranges, white-balance choices, tone-curve values, HSL mixer values,
color grading, and geometry overrides. It also owns the shared light-tone LUT.

The recipe is the description of the edit, not the image data. Desktop edits
advance a revision number and the CLI constructs a recipe from command-line
arguments.

### `crates/raw` — decoder boundary and RAW metadata

`rohditor-raw` hides the `rawler` decoder behind `RawDecoder` and `RawSession`.
It converts decoder-specific results into Rohditor types such as `RawFileInfo`,
`RawFrame`, CFA/level metadata, capture metadata, and an optional embedded
JPEG preview. It also applies decoder limits and normalizes malformed-input
errors.

RAW data is treated as immutable after decoding. The rest of the program sees
the normalized model rather than `rawler` types.

### `crates/demosaic` — Bayer reconstruction algorithms

`rohditor-demosaic` contains the selectable Bayer algorithms: bilinear and
Malvar-He-Cutler (`mhc`). It accepts a validated mosaic and produces
camera-native linear RGB, with cancellation and algorithm-specific tests. The
small `crates/core/src/demosaic.rs` module is only the core-to-demosaic error
and cancellation adapter.

### `crates/core` — deterministic CPU processing and export

`rohditor-core` is the central processing library. Its important modules are:

- `pipeline.rs`: public orchestration, preview options, reusable bases,
  render results, timings, and memory estimates;
- `cpu.rs`: normalization, white balance, camera color conversion application,
  global adjustments, orientation, transfer encoding, and quantization;
- `color.rs`: camera calibration matrices, chromatic adaptation, Rec.2020 and
  sRGB transforms;
- `resample.rs`: cancellable antialiased preview reduction;
- `analysis.rs`: display-image histogram and clipping information;
- `export.rs` and `output.rs`: JPEG/PNG encoding, safe metadata, dithering,
  overwrite checks, and transactional writes;
- `cancel.rs` and `error.rs`: cooperative cancellation and pipeline-level
  errors.

The normal full pipeline is:

```text
RawFrame
  -> crop and black/white normalization
  -> Bayer demosaic
  -> optional preview resampling
  -> white balance
  -> camera color transform to linear Rec.2020/D65
  -> recipe adjustments
  -> orientation and sRGB conversion
  -> DisplayRgbImage / export encoding
```

For interactive previews, the work is split into reusable stages. A
`ReconstructedPreview` retains reduced, camera-native RGB. A `DemosaicedBase`
adds white balance and camera color conversion. Subsequent recipe adjustments
and display conversion can then be rerun without decoding and demosaicing the
RAW again. Full-resolution source-scale inspection and export use a separate
CPU path rather than treating a reduced preview as sensor-scale data.

### `crates/gpu` — optional interactive GPU preview

`rohditor-gpu` owns `wgpu` resources, upload packing, shader parameters,
dispatch, display textures, asynchronous readback, and CPU/GPU parity tests.
`preview.wgsl` applies the supported downstream preview operations to an
uploaded linear base and writes the display texture. The GPU path is optional:
unsupported recipes or unavailable hardware fall back to the CPU reference
implementation. The desktop deliberately disables processing on software
adapters, while the UI renderer may still have a separate fallback.

## Applications

### `apps/cli` — headless commands

`rohditor-cli` translates command-line arguments into a validated
`EditRecipe`, opens files through `RawlerDecoder`, invokes `CpuPipeline`, and
uses core's export/output APIs. Its commands cover metadata inspection,
embedded-preview extraction, development/export, quality-crop generation, and
LibRaw mosaic comparison. It is also a useful small end-to-end entry point
because it has no UI or worker-thread state.

### `apps/desktop` — UI and asynchronous orchestration

The desktop application is built with eframe/egui. Its responsibilities are
application state and coordination, not image-processing semantics:

- `main.rs`: command-line startup options, renderer/processor selection, and
  eframe initialization;
- `app.rs`: document lifecycle, UI commands, edit interaction, preview
  presentation, and event handling;
- `document.rs`: current document, immutable frame reference, edit session,
  revision counter, and bounded undo/redo;
- `coordinator.rs`: worker thread, RAW open/decode, preview and export jobs,
  cache use, progress, and worker events;
- `coordinator/scheduler.rs`: newest-wins preview mailbox, coalescing, and
  cancellation;
- `preview_cache.rs`: reconstructed/base/adjusted preview reuse;
- `app/gpu.rs`: attachment of the GPU processor to eframe's shared wgpu state;
- `ui/`: presentation-only toolbar, adjustment controls, viewport,
  diagnostics, theme, and reusable widgets.

The desktop flow is:

1. The UI requests an open and assigns a document identity.
2. The worker probes metadata, extracts an embedded loading preview when
   available, and decodes the immutable `RawFrame`.
3. A recipe change increments the document revision and queues a preview job.
4. The scheduler cancels superseded work and keeps at most the newest pending
   preview for a document.
5. The worker uses the CPU cache path or prepares a GPU upload from the shared
   reconstruction/base.
6. The UI accepts a result only when its document and revision are current.
   During CPU fallback, the existing visible GPU frame is retained until the
   replacement is ready, avoiding a blank intermediate frame.

Picker operations and source-scale inspection are separate asynchronous paths.
They use retained display pixels or explicit GPU readback as appropriate;
display preview pixels are not presented as sensor-resolution data.

## Tests and diagnostics

Tests are distributed with the owning layer: typed image and recipe invariants,
decoder/malformed-input tests, CPU reference and export tests, demosaic quality
tests, GPU parity tests, and desktop state/scheduler tests. The normal check is
`./scripts/check.sh`. Private Sony corpus tests and real-GPU tests are ignored
by default and must be opted into when validating decoder, image-quality, or
hardware behavior.

## Suggested reading order

Read in this order, following one simple image from file to screen:

1. `README.md` for scope, supported commands, and current/deferred features.
2. `crates/image/src/lib.rs` and `orientation.rs` for the typed image model.
3. `crates/edit/src/lib.rs` and `light.rs` for the recipe and validation.
4. `crates/raw/src/decoder.rs`, `model.rs`, and `rawler_adapter.rs` for the
   decoder boundary and `RawFrame`.
5. `crates/demosaic/src/lib.rs`, then `bilinear.rs` and
   `malvar_he_cutler.rs`, to understand Bayer reconstruction.
6. `crates/core/src/pipeline.rs` first for the stage sequence, then follow its
   calls into `cpu.rs`, `color.rs`, `resample.rs`, and `analysis.rs`.
7. `crates/core/tests/cpu_reference.rs` and `export.rs` to see the intended
   contracts exercised end to end.
8. `apps/cli/src/main.rs` for the simplest application-level use of the core.
9. `apps/desktop/src/document.rs`, `coordinator/scheduler.rs`,
   `preview_cache.rs`, and `coordinator.rs` for asynchronous state and cache
   behavior.
10. `apps/desktop/src/app.rs` and `ui/` for the UI translation layer.
11. `crates/gpu/src/preview.rs`, `preview.wgsl`, and its parity tests last;
    this is easiest once the CPU pipeline and reusable base are understood.

When reading, keep three questions in view: what image state is represented at
this point, which recipe revision does the work belong to, and whether the
operation is correctness-critical CPU work or optional interactive GPU work.
