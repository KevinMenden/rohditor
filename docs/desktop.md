# Phase 4 desktop application

Phase 4 provides a minimal `eframe` editor around the CPU reference pipeline.
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

Phase 4 always develops images on the CPU. Selecting wgpu only changes how
`egui` draws the window and uploads the completed CPU image as a display
texture. GPU image processing starts in Phase 5.

## Document and worker model

One document has an immutable source path/frame, an `EditRecipe`, a monotonic
revision, bounded in-memory undo/redo history, and display state. It never owns
a mutable reference to an in-flight render buffer.

A dedicated ordinary Rust thread receives these jobs:

1. Probe metadata, decode and orient the embedded JPEG, then decode the sensor
   frame.
2. Develop a resolution-limited CPU preview.
3. Render and transactionally encode a full-resolution export.

The worker sends progress and typed result messages and asks `egui` to repaint.
No RAW decode, demosaic, adjustment pass, or file encoding runs in
`eframe::App::update`.

Every preview request and result carries both a document ID and recipe revision.
Queued slider requests for the same document are collapsed to the newest
revision before work begins. Work already running is allowed to finish, but the
UI installs a result only when both identity fields still match. Closing a
document invalidates its active identity, and every replacement receives a new
ID; queued preview work for closed IDs is marked abandoned. The worker is
detached during application shutdown so closing the window does not wait for a
long render.

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
ticket rejection, preview-result coalescing, EXIF orientation mapping, and UI to
core export-setting conversion. Core tests cover phase-preserving preview
scaling. The opt-in private worker test performs real asynchronous open,
2560-edge preview, and snapshot export with `DSC00851.ARW`.

On the reference Plasma Wayland desktop, both glow and Vulkan/wgpu displayed a
real A6400 CPU preview correctly. The wgpu run selected the RX 9070 XT through
RADV. Cooperative cancellation, stage caches, queue bounds, and GPU image
processing remain later-phase work; stale results are safe now, but an obsolete
render that has already started may still consume CPU until it finishes.
