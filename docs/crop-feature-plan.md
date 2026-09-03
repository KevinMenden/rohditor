# Non-destructive crop feature plan

## Outcome

Add a familiar RAW-editor crop tool that is non-destructive, undoable, exact in
CPU export, and supported by the interactive GPU preview. The first version
should provide:

- an on-canvas crop rectangle with corner and edge handles;
- moving and resizing within the image bounds;
- a darkened outside area and rule-of-thirds guide;
- free, original, 1:1, 3:2, 4:3, 4:5, 5:7, and 16:9 aspect choices;
- locked/unlocked aspect ratio and portrait/landscape ratio switching;
- explicit Apply, Cancel, and Reset crop actions;
- one undo entry for an applied crop and no history change for a cancelled one;
- cropped fit previews, cropped source-scale inspection, histogram analysis of
  the committed crop, and cropped JPEG/PNG export.

Straightening by an arbitrary angle, perspective correction, lens correction,
content-aware fill, and export resizing are separate geometry features. They
should not be bundled into the initial crop implementation.

## Why this fits the current architecture

Rohditor already has the main boundaries this feature needs:

- `GeometryAdjustments` is part of the validated `EditRecipe`.
- CPU and GPU previews retain an uncropped linear base and apply orientation at
  final display conversion.
- Preview jobs are document/revision keyed, cancellable, and newest-wins.
- `EditSession` already supports discrete edits and one-undo-step gestures.
- The viewport already owns image-to-screen zoom and pan behavior.

The user crop should therefore be a **late output geometry operation**, not a
RAW sensor crop and not a destructive image-buffer mutation:

```text
immutable RAW
  -> metadata-selected sensor area
  -> normalize and demosaic
  -> preview resample when applicable
  -> white balance, color conversion, and adjustments
  -> orthogonal orientation
  -> user crop
  -> transfer encoding, histogram, and display/export image
```

Implementing the crop in the final coordinate map has four important
properties:

1. A crop-only change reuses the reconstructed and demosaiced preview caches.
2. A GPU crop reuses the resident source texture and needs no new upload.
3. Demosaicing still sees neighboring pixels outside the crop, avoiding seams
   at crop edges.
4. Pixels outside the crop remain available for reopening the crop tool and
   for future heal, clone, local-adjustment, and transform operations.

This matches the useful behavior shared by mainstream RAW editors: while the
crop tool is active, show the full uncropped image with an overlay; commit only
the framing rectangle. Aspect-ratio selection is an authoring constraint, not
part of the rendered image state.

## Terminology cleanup

The current `CropPolicy::{ActiveArea, Recommended}` selects a metadata-defined
RAW sensor area. It is not a user crop. Rename the Rust type to
`RawCropPolicy` (or similarly explicit wording) before adding the new domain
type. The CLI may retain its existing `--crop` spelling for compatibility, but
its help and internal fields should say “RAW sensor crop policy.”

Keep these three concepts distinct:

- **RAW sensor crop**: active/recommended area selected before normalization;
- **user crop**: non-destructive rectangle in `EditRecipe::geometry`;
- **quality crop**: the existing diagnostic artifact produced by the
  `quality-crops` command.

## Recipe model and coordinate contract

Move geometry-specific recipe types and validation into
`crates/edit/src/geometry.rs` rather than continuing to grow `lib.rs`.

The recipe should become conceptually:

```rust
pub struct GeometryAdjustments {
    pub orientation_override: Option<Orientation>,
    pub crop: Option<NormalizedCropRect>,
}

pub struct NormalizedCropRect {
    pub left: f64,
    pub top: f64,
    pub right: f64,
    pub bottom: f64,
}
```

The precise contract should be:

- `None` means the entire image and is the neutral/default state.
- Coordinates describe pixel **edges**, not pixel centers.
- Coordinates are normalized to the full developed canvas after the effective
  EXIF/user orthogonal orientation, but before the user crop.
- `left` and `top` are inclusive; `right` and `bottom` are exclusive.
- Every value is finite and in `0.0..=1.0`.
- `left < right` and `top < bottom`.
- `f64` is intentional for stable full-resolution edge placement; shader
  parameters receive resolved integer coordinates, not these floats.
- The core resolves edges to the nearest pixel boundary, clamps to the canvas,
  and rejects a rectangle that collapses below one pixel for the concrete
  image dimensions.

Use `left/top/right/bottom` instead of `x/y/width/height` so validation and
boundary clamping do not accumulate additions. Do not serialize an aspect
ratio or guide choice: those are crop-tool preferences and do not change the
result once the rectangle is known.

Increment `EDIT_RECIPE_SCHEMA_VERSION` from 2 to 3. The custom deserializer
must explicitly migrate both version 1 and version 2 recipes to `crop: None`.
Tests must cover both migrations, round trips, invalid/non-finite rectangles,
and a neutral recipe.

If a later UI adds rotate/flip actions, changing orientation and remapping the
crop must be one atomic recipe edit so the selected image content is retained.
That policy should live in a geometry helper, not in widget code.

## Shared output geometry

Add `crates/core/src/geometry.rs` with one CPU-reference resolver, for example
`OutputGeometry`. It should be constructed from:

- the uncropped linear source dimensions;
- the effective source/override orientation;
- the optional normalized user crop.

It should expose:

- full oriented dimensions;
- the resolved crop region in oriented coordinates;
- final output dimensions;
- an output-to-source coordinate mapping.

The mapping is:

```text
cropped output coordinate
  + crop origin in the oriented canvas
  -> inverse orthogonal orientation
  -> uncropped linear source coordinate
```

Build this on the existing `OrientationMap` rather than adding a second EXIF
orientation table. Small asymmetric fixtures must establish the rounding and
mapping contract before the CPU and GPU paths are changed.

Refactor the 8-bit and 16-bit CPU output conversion paths to consume this same
resolved geometry. Keep orientation-only public helpers as full-frame wrappers
if they are still useful, while all pipeline renders use the recipe-aware path.
Update memory estimates to use final cropped output dimensions for display
buffers.

Crop remains after the pixel adjustments semantically, even if a later
optimization skips work that provably cannot affect the crop.

## CPU pipeline and export

Apply `OutputGeometry` in every CPU result path:

- normal full render;
- cached fit preview;
- source-scale preview;
- 8-bit export;
- 16-bit export.

The output buffer must be allocated at the cropped dimensions and filled by
mapping each cropped output coordinate back to the uncropped linear source.
The histogram is then naturally calculated from only the committed crop.
Dithering coordinates should remain final output coordinates so repeated
exports are deterministic.

Add the crop fields only to `AdjustedPreviewKey`. They must not enter the
decoded, reconstructed, or demosaiced keys. A crop-only edit should therefore
report hits for the expensive cache levels.

Do not crop the normalized Bayer mosaic as part of the correctness path. A
future region-of-interest optimization may normalize/demosaic a crop plus the
algorithm's declared halo, but it needs its own full-frame parity tests and is
not necessary for the first feature.

## GPU preview

Implement crop support in the initial feature rather than treating it as an
unsupported CPU fallback. It is a coordinate change and fits the current
shader well.

Resolve the normalized crop on the Rust side with the same `OutputGeometry`
used by the CPU. Pass the integer crop origin and final dimensions in the
preview uniform. In WGSL, add the crop origin to the invocation coordinate in
the oriented canvas before calling the existing orientation mapping.

The GPU output texture uses the cropped dimensions. The resident source
texture remains uncropped, so applying or resetting a crop must not upload it
again. A prior output frame may be reused only when its source and cropped
output dimensions are compatible; otherwise only the output textures are
reallocated.

Keep crop recipes GPU-supported when the other recipe fields are supported.
Add parity coverage for odd-sized, off-center rectangles under all eight EXIF
orientations, including crops touching every boundary.

## Crop-tool state and preview intent

The committed recipe and the in-progress crop must be separate.

Add a `CropToolSession` in a focused desktop module such as
`apps/desktop/src/app/crop.rs`. It owns:

- the original committed crop;
- the draft crop;
- the current aspect constraint and lock state;
- whether the full-frame authoring preview is ready;
- any active handle/drag state needed above the presentation widget.

Entering crop mode must not mutate `EditSession`. It requests a temporary fit
preview using the current recipe with `crop: None`, while the old visible frame
is retained until that full-frame preview is ready. This reveals pixels outside
an existing crop without a demosaic/base rebuild. While the user drags, only
the overlay changes; no preview job is queued per pointer event.

Applying the crop:

1. canonicalizes full-frame crop to `None`;
2. installs the draft through `EditSession::set_discrete`;
3. exits crop mode;
4. queues the normal committed cropped preview.

Cancelling exits without changing the recipe or history and restores the
committed preview. Reset sets only `geometry.crop` to `None`; it does not reset
orientation or the light/color edits.

The existing `(document_id, recipe_revision)` ticket is insufficient for
temporary crop-tool previews because entering, cancelling, and re-entering can
all happen at one recipe revision. Extend preview identity with a monotonic
request/presentation sequence and an intent such as:

```text
CommittedFit
CropToolFullFrame
SourceScale
```

Both progress and result events must match document, recipe revision, request
sequence, and current intent before they update UI state. A crop-tool preview
may replace the displayed texture, but it must not replace the committed-crop
histogram or mark that histogram current for Auto Tone.

Entering crop mode should switch to a fit/full-frame authoring view and make it
mutually exclusive with Source 1:1 and the white-balance/Color Mixer pickers.
Replace the growing picker-only mode state with an application-level active
viewport tool enum when doing so.

## Viewport and controls

Keep crop interaction out of `app.rs` and keep image processing out of
`ui/viewport.rs`.

Refactor the viewport's existing image rectangle and coordinate conversions
into a small, pure `ViewportTransform`. It should provide image-normalized to
screen and screen to image-normalized mapping for navigation, pickers, and the
crop overlay.

Add a focused `apps/desktop/src/ui/crop.rs` (or `crop_overlay.rs`) which:

- paints the outside mask, rule-of-thirds guide, border, and eight handles;
- hit-tests corners before edges and edges before the crop interior;
- returns semantic drag events in normalized image coordinates;
- changes the cursor without adding/removing instructional layout rows;
- contains no recipe, worker, cache, or GPU knowledge.

Put a crop-tool button in the main tool area and show a compact Geometry/Crop
section while it is active. The panel should include aspect selection,
lock/unlock, ratio orientation, crop pixel dimensions, Reset, Cancel, and
Apply. Enter applies and Escape cancels after the pointer interaction is
settled. Shortcut `R` and ratio-orientation shortcut `X` can be added once they
do not conflict with existing text input.

Interaction rules:

- corner drag keeps the opposite corner fixed;
- edge drag changes one axis in free mode;
- locked edge/corner drags preserve the chosen ratio;
- dragging inside moves the rectangle without resizing it;
- the rectangle never leaves the image or inverts;
- a small screen-space minimum keeps handles usable, while core validation
  remains the final one-pixel safety check;
- crop handles take primary-drag precedence; pan remains available through the
  middle button or Space + primary drag; wheel zoom remains available.

## Preview quality policy

Keep the first implementation's reconstruction cache uncropped. This makes
crop entry, Apply, Cancel, undo, and redo responsive and preserves the current
memory model. The fit preview remains an antialiased full-frame source from
which the selected rectangle is displayed; Source 1:1 remains the truthful
full-resolution inspection path inside the committed crop.

A very tight crop can contain fewer preview texels than the viewport and may
therefore be enlarged on screen. Measure this explicitly rather than silently
moving crop upstream. If normal editing shows a material quality problem, add
a separate, optional **crop-focused preview** cache entry. Build it from a
source region plus the demosaic algorithm's declared halo, validate it against
the full-frame CPU result, and atomically replace the existing visible preview.
Do not make full-resolution linear images permanently resident merely to
improve tight-crop fit previews.

## Implementation sequence

### 1. Domain and terminology

- Rename the existing sensor-area `CropPolicy` type internally.
- Add `geometry.rs` to `rohditor-edit`.
- Add and validate `NormalizedCropRect`.
- Bump the recipe schema and implement v1/v2 migration.
- Add serialization, validation, and canonical-full-frame tests.

### 2. CPU reference geometry

- Add and test `OutputGeometry` with asymmetric images and all orientations.
- Route 8-bit/16-bit output conversion through it.
- Apply it to normal, preview, source-scale, and export results.
- Correct output memory estimates and histogram expectations.
- Add cropped export tests for dimensions and exact selected pixels.

### 3. Cache and GPU parity

- Add crop to only the adjusted CPU preview key.
- Extend GPU parameters and output allocation for the resolved crop.
- Verify resident-source reuse across crop changes.
- Add CPU/GPU crop parity tests for all orientations and awkward boundaries.

### 4. Preview request identity

- Add request sequence and preview intent to scheduling/events.
- Add the temporary uncropped crop-tool preview path.
- Preserve the old visible texture during full/cropped transitions.
- Keep committed histogram state separate from crop-tool presentation.
- Test rapid enter/cancel/re-enter and stale-result rejection.

### 5. Desktop crop tool

- Add `CropToolSession` and a general active viewport tool state.
- Add the toolbar/panel controls.
- Add the pure overlay and drag solver.
- Integrate Apply, Cancel, Reset, undo, redo, zoom, and pan behavior.
- Add unit tests for handles, boundaries, aspect ratios, and state transitions.

### 6. Quality and end-to-end validation

- Exercise loose and tight crops on landscape and portrait-oriented A6400 RAWs.
- Compare CPU preview, GPU preview, source-scale display, and exported pixels.
- Measure crop entry and Apply latency and confirm no crop-only base rebuild or
  GPU source upload in diagnostics.
- Decide from measurements whether a crop-focused preview is justified.
- Update the README Geometry checklist only after the acceptance gates pass.

## Acceptance gates

The crop feature is complete when all of the following hold:

- A neutral crop is pixel-identical to the pre-feature CPU output.
- CPU 8-bit, CPU 16-bit, source-scale, and export paths produce the same crop
  bounds for all EXIF orientations.
- GPU crop output has the expected dimensions and remains within the existing
  CPU/GPU sRGB parity tolerance.
- Crop-only edits hit reconstructed/demosaiced caches and reuse the GPU source.
- Reopening a crop shows the full image and the previously committed rectangle.
- Apply creates exactly one undo step; Cancel creates none; Reset is undoable.
- Fast mode changes never install a stale full-frame or cropped result and
  never show a blank/black intermediate frame.
- Aspect-locked drags remain in bounds without jumps, inversion, or ratio drift.
- Histogram and Auto Tone use the committed cropped result, not the temporary
  full-frame crop-tool preview.
- Source 1:1 remains truthfully one source pixel per display pixel inside the
  crop; a fit preview is never relabeled as source scale.
- `./scripts/check.sh` passes.
- The ignored private-corpus suite passes.
- The ignored GPU suite passes on the host RX 9070 XT/RADV device before GPU
  parity or latency is claimed.

## Main risks and mitigations

| Risk | Mitigation |
| --- | --- |
| Confusing metadata sensor crop with user crop | Rename the internal policy and keep separate types and terms. |
| Crop shifts under EXIF rotation | Define coordinates in the oriented canvas and test all eight mappings. |
| Dragging floods the worker | Keep a UI-only draft and render only on mode changes/Apply. |
| Old async results replace the wrong presentation | Add request sequence plus preview intent, not recipe revision alone. |
| First mode switch flashes blank | Retain the currently visible frame until its replacement is ready. |
| Crop invalidates expensive caches | Put it only in the adjusted key and keep the linear base uncropped. |
| Tight crop looks soft in Fit | Measure texel density; add a bounded crop-focused ROI preview only if needed. |
| Future local tools lose outside pixels | Keep canonical crop late and preserve the uncropped source/base. |
| Straighten/perspective balloons initial scope | Keep them as later geometry transforms with a new recipe/schema decision. |

## Deliberately deferred follow-up

Once axis-aligned crop is stable, the next geometry document should define the
transform order for arbitrary straighten, 90-degree rotate/flip controls, lens
distortion, and perspective correction together. Those transforms change the
shape of the valid image canvas and need an explicit “constrain crop to image”
policy; adding an angle field prematurely would leave those interactions
underspecified.
