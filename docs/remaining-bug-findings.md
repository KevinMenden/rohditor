# Remaining Rohditor bug findings

**Review date:** 2026-09-01
**Scope:** findings from the desktop preview, Light controls, tone curve, and Color mixer investigation.

The Source 1:1 toolbar-selection issue is fixed separately. The items below remain open and should not be lost while implementing that fix.

## 1. Source 1:1 quality and latency

### Observed behavior

- Source 1:1 takes noticeably longer than the fit preview.
- The zoomed source image can show more false colour or edge artifacts than the fit preview.
- Zooming back out from source resolution can look worse than the antialiased fit preview.

### Current cause

Source inspection is intentionally a separate, full-resolution CPU render. The desktop worker calls `render_source_scale_preview_cancellable`, clears the normal preview cache, and produces a complete 6000x4000 display image before installing it. A private A6400 run measured about 1.04 s and 413 MiB peak.

The fit preview is reconstructed and reduced with camera-linear area filtering. Source mode is displayed with a single full-resolution texture and ordinary linear sampling, without a mip/pyramid level. This aliases during minification and exposes demosaic detail that area reduction hides. MHC can also produce false colour around saturated, high-contrast edges because of its negative filter lobes.

Relevant paths:

- `apps/desktop/src/coordinator.rs`: `process_source_scale_preview`
- `crates/core/src/pipeline.rs`: `render_source_scale_preview_cancellable`
- `apps/desktop/src/ui/viewport.rs`: texture sampling and scaling

### Proposed solution

Short term:

- Keep the fit preview available while source inspection is prepared.
- Use the fit/area-reduced representation below 100% magnification and source pixels at or above 100%.
- Keep source-resolution mode and magnification independent in the status UI.

Long term:

- Build a tiled, mipmapped source pyramid.
- Develop only visible source tiles, with a demosaic border, instead of converting and uploading the entire frame for every inspection request.
- Apply downstream adjustments on the GPU where possible, retaining the CPU path as the correctness fallback.
- Add quality fixtures for diagonal edges, saturated boundaries, and image borders before adding any false-colour suppression.

## 4. Tone curve has insufficient range

### Observed behavior

The curve cannot be bent strongly, and moving one point can appear to stop responding.

### Current cause

The recipe stores only four fixed points at x positions 0.12, 0.35, 0.65, and 0.88. Each offset is restricted to `-0.25..=0.25`. Evaluation clamps outputs and projects them into a non-decreasing sequence. If one point crosses a neighbour, its visible output is pinned even though its stored value continues to change.

Relevant paths:

- `crates/core/src/edit.rs`: `TONE_CURVE_RANGE`
- `crates/core/src/cpu.rs`: `evaluate_tone_curve`
- `apps/desktop/src/ui/adjustment_panel.rs`: graph handles and drag mapping

### Proposed solution

Preferred:

- Replace the four offsets with validated, ordered `(x, y)` points, including editable endpoints.
- Support adding, dragging, and deleting points.
- Enforce x ordering, but do not silently hide y movement. If monotonic mode is required, clamp visibly against neighbouring points.
- Evaluate through a shared LUT for CPU and GPU.
- Migrate existing recipes into equivalent points when the recipe schema changes.

Interim:

- Expand each point's legal y range to the graph bounds.
- Clamp against neighbouring points before storing the value, so the handle and recipe never disagree.

Coverage to add:

- Extreme bends, endpoint edits, neighbour crossings, and reset behavior.
- Monotonic and non-monotonic policy tests.
- CPU/GPU evaluator parity.

## 5. Color Mixer is unwieldy and the HSL bands overlap incorrectly

### Observed behavior

The Color mixer shows eight colour groups with three sliders each. It is difficult to identify a target colour or make a controlled adjustment, and it currently has no targeted colour picker.

### Current cause

The UI renders all 24 HSL controls in one long section. The processing weights use broad, overlapping triangular bands without normalization. A colour can receive contributions from multiple neighbouring bands, and equal adjustments can be amplified. Evenly spaced bands also do not correspond well to conventional Red/Orange/Yellow/Green/Aqua/Blue/Purple/Magenta centres.

Relevant paths:

- `apps/desktop/src/ui/adjustment_panel.rs`: `show_color_mixer_controls`
- `crates/core/src/cpu.rs`: `apply_hsl_adjustments`
- `crates/gpu/src/preview.rs`: HSL is currently unsupported by the shader and routes to CPU

### Proposed solution

UI:

- Provide Hue/Saturation/Luminance tabs with eight coloured controls, or show eight colour swatches and expose three controls only for the selected swatch.
- Add a targeted picker that samples a colour from the viewport and selects the relevant mixer band(s).
- Freeze the selected hue/weights for the duration of a drag gesture.

Processing:

- Define explicit hue centres and widths.
- Normalize feathered weights so their sum is one.
- Preserve the signed/wide-gamut working range deliberately.
- Keep CPU as the reference until the corrected operation has parity tests; then add a matching GPU implementation.

The picker should be a purpose-specific mode (for example, white balance versus Color mixer), not another boolean shared by unrelated sampling actions.

Coverage to add:

- Exact band centres, midpoints, and wraparound at Red/Magenta.
- One-band and equal-all-band adjustments.
- Neutral, saturated, negative, and over-range working values.
- Picker selection and drag behavior on representative colours.

## Suggested implementation order

1. Free-form tone curve.
2. Color Mixer UX, corrected HSL weighting, and targeted picker.
3. Tiled/mipmapped Source 1:1 inspection and GPU-assisted source tiles.

The Source 1:1 toolbar-selection fix is already implemented and should remain separate from these larger changes.

## Validation recorded during the investigation

- `cargo test --release -p rohditor-core --lib`: 54 passed.
- `cargo test --release -p rohditor-desktop`: 40 passed, 2 ignored.
- `cargo test --release -p rohditor-gpu --lib`: 4 passed, 7 hardware tests ignored.
- `cargo test --release -p rohditor-core --test private_pipeline -- --ignored --nocapture`: 2 passed; Source 1:1 measured about 1.04 s and 413 MiB peak.
- `./scripts/check.sh`: passed.
