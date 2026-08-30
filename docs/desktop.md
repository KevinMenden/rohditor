# Phases 4 and 5 desktop application

Phase 4 provides the minimal `eframe` editor around the CPU reference pipeline.
Phase 5 adds a downstream GPU preview processor while retaining that CPU path.
The desktop crate owns widgets and document coordination; the core and RAW
crates remain independent of `egui`.

## Running the application

```console
cargo run --release -p rohditor-desktop
cargo run --release -p rohditor-desktop -- testdata/private/DSC00851.ARW
```

The optional file argument bypasses the open dialog and is useful for repeatable
diagnostics. The normal **Open RAW…** action uses `rfd` with its XDG desktop
portal backend. Both Wayland and X11 window-system support are compiled.

The UI renderer is independent of image processing:

```console
rohditor-desktop --renderer auto   # wgpu first, then glow on initialization error
rohditor-desktop --renderer wgpu   # require the Vulkan-backed wgpu UI
rohditor-desktop --renderer glow   # OpenGL UI fallback
```

Interactive preview processing is separate from UI renderer selection:

```console
rohditor-desktop --processor auto  # shared hardware wgpu processor, otherwise CPU
rohditor-desktop --processor gpu   # require a compatible shared wgpu processor
rohditor-desktop --processor cpu   # CPU preview only; no Rohditor GPU resources
```

In `Auto`, a wgpu renderer supplies its existing adapter, device, and queue to
the GPU processor. A CPU rasterizer, incompatible texture formats, or a later
GPU preview failure produces a visible CPU fallback reason. `GPU` reports an
initialization/rendering error rather than silently changing processors. `glow`
does not expose shared wgpu state, so it supports CPU preview only.

## Document and worker model

One document has an immutable source path/frame, an `EditRecipe`, a monotonic
revision, bounded in-memory undo/redo history, and display state. It never owns
a mutable reference to an in-flight render buffer.

A dedicated ordinary Rust thread receives these jobs:

1. Open one coherent RAW session, probe metadata, decode and orient the optional
   embedded JPEG, then decode the sensor frame.
2. Develop a resolution-limited linear CPU base. CPU mode completes its display
   conversion on this worker; GPU mode also packs a half-float upload payload
   here, then sends it to the UI thread for one GPU upload and downstream shader
   work.
3. Render and transactionally encode a full-resolution export.

The worker sends progress and typed result messages and asks `egui` to repaint.
No RAW decode, demosaic, adjustment pass, or file encoding runs in
`eframe::App::update`.

Every preview request and result carries both a document ID and recipe revision.
Queued slider requests for the same document are collapsed to the newest
revision before work begins. Work already running is allowed to finish, but the
UI installs a result only when both identity fields still match. Closing a
document invalidates its active identity, and every replacement receives a new
ID; queued open, preview, and export work for closed IDs is marked abandoned.
The coordinator retains the worker handle and reports an unexpected worker
panic to the UI. Shutdown does not wait for an in-flight long render.

## Preview path

The embedded camera JPEG is decoded and physically oriented on the worker and
shown only as a loading placeholder. It is replaced by a RAW-developed sRGB8
texture.

`PreviewOptions::default()` limits the developed preview to a 2560-pixel long
edge. Scaling occurs while the sensor mosaic is normalized, before RGB
demosaic. Sampling maps the red, green, and blue CFA sub-grids independently,
so reduced coordinates retain their Bayer phase. A 6000×4000 A6400 crop becomes
2560×1707 (or 1707×2560 after portrait orientation). Full-resolution render and
export behavior is unchanged.

CPU mode retains one typed demosaiced linear base on the worker for the active
frame and white balance. GPU mode retains its equivalent linear base as an
`Rgba16Float` texture on eframe's shared device. Exposure, contrast, saturation,
and orientation changes reuse the relevant base; a source or white-balance
change rebuilds it. The GPU display texture is registered directly with egui;
it is not copied back to CPU before the viewport samples it. See
[`gpu-preview.md`](gpu-preview.md) for the detailed texture contract.

Fit, 100%, wheel zoom, and drag panning transform only the displayed texture.
They do not enqueue RAW development.

## Edits and history

The right panel exposes:

- as-shot or manual relative red/green/blue white-balance multipliers;
- exposure, contrast, and saturation over the core-defined ranges;
- reset, undo, and redo.

Each installed recipe value increments the revision. Intermediate values during
a slider drag receive individual revisions so stale render results are
unambiguous, while the complete drag is stored as one undo command. History is
limited to 100 recipe snapshots and is intentionally not persisted yet.

## Export

The export panel maps directly to UI-independent `ExportSettings`: JPEG quality,
PNG 8/16-bit depth, dithering, safe metadata, and explicit overwrite. The save
dialog supplies a JPEG or PNG destination. The app also refuses to replace the
source RAW through a canonical path, symbolic link, or hard link.

An export job owns a cloned recipe and revision. Later slider changes therefore
cannot change an in-flight export. Full-resolution development and the Phase 3
transactional encoder both run on the worker. The status bar reports its stage,
snapshot revision, completion time, dimensions, and byte count; encoder and
destination errors are shown in the panel with their original actionable
context.

## Verification and current limits

Unit tests cover revision advancement, drag coalescing, reset/undo/redo, stale
ticket rejection, preview-result coalescing, CPU-to-GPU base handoff, EXIF
orientation mapping, and UI to core export-setting conversion. Core tests cover
phase-preserving preview scaling. Opt-in Vulkan tests compare GPU output against
the CPU reference with a rotated synthetic fixture and `DSC00851.ARW`; the
acceptance tolerance is at most two 8-bit sRGB codes per channel. The opt-in
private worker test performs real asynchronous open, 2560-edge preview, and
snapshot export with `DSC00851.ARW`.

On the reference Plasma Wayland desktop, both glow and Vulkan/wgpu displayed a
real A6400 CPU preview correctly. The wgpu run selected the RX 9070 XT through
RADV. GPU parity checks run against that Vulkan path. Cooperative cancellation,
bounded command queues, multi-level cache eviction, and measured performance
tuning remain Phase 6 work; stale results are safe now, but an obsolete CPU
base render that has already started may still consume CPU until it finishes.
