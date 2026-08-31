# Rohditor desktop design system

Phase 7 gives the desktop editor a presentation layer owned by Rohditor rather
than treating stock egui visuals as the finished interface. The design system
is intentionally local to `apps/desktop`: it may depend on egui/eframe, but it
does not know about RAW frames, cache keys, CPU pipeline stages, or GPU
processors.

## Module boundary

```text
apps/desktop/src/ui/
    theme.rs              palette, type scale, spacing, surfaces, states
    icons.rs              small vector icons painted with egui primitives
    widgets.rs            reusable controls and formatting conventions
    adjustment_panel.rs   Develop and export presentation models
    diagnostics.rs        developer telemetry presentation models
    toolbar.rs            top toolbar, file rail, and compact status bar
    viewport.rs           texture presentation, zoom, pan, and overlays
```

`app.rs` remains the command boundary. It translates `EditRecipe` values into
presentation-only models and translates UI interaction records back into
`EditSession` operations. This preserves revision increments, one undo entry
per slider drag, newest-wins preview scheduling, and immutable export snapshots
without teaching reusable widgets about image processing.

## Visual tokens

The base surfaces progress from the nearly black viewport (`#090a0c`) through
the application background (`#0e1013`) to panels (`#16191e`) and raised cards
(`#1c2026`). Borders use `#2b313a`, with `#3d4550` reserved for stronger focus
or separation. Primary text is `#e6e9ee`; secondary text is `#9199a5`.

Rohditor uses a warm amber accent (`#db9d4b`) for primary actions, active
values, and short-lived attention. Success, warning, and error colors are
reserved for actual state reporting. They must not be used as decoration.

The normal type size is 13 points, buttons are 12.5, compact metadata is 11,
and top-level headings are 20. Section labels are uppercase 10.5-point text
with a divider. Interactive controls target a minimum 26-point height. Corner
radii are 3 points for controls, 5 for cards, and 8 for floating windows.

New UI code should use the named colors, metrics, and frame constructors from
`theme.rs`. One-off RGB values are appropriate only for content-derived colors
or translucent viewport effects.

## Widget conventions

Every develop adjustment uses `adjustment_slider`:

```text
Exposure                                      +0.35 EV
━━━━━━━━━━━━━━━━━━●━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
```

- The label and editable numeric value occupy the first row; the full-width
  rail occupies the second.
- The thin neutral marker is derived from the same range supplied by the core
  recipe contract.
- Exposure is signed EV with two decimals. Contrast and saturation are signed
  percentage offsets relative to their neutral `1.0` multipliers.
  Manual white-balance channels are unsigned multipliers.
- Users can drag the rail or value, focus the control and use arrow keys, or
  click the numeric value and type. The reset icon restores only that control;
  the section action restores the complete recipe.
- Widgets return value and interaction metadata. They never mutate a document,
  submit a preview, or own callbacks into the processing layer.

Toolbar icons are vector primitives rather than font glyphs, so their shape is
stable across Linux font configurations. Dropdown labels appear above a
full-width field and use the themed popup treatment. Primary amber buttons are
limited to workflow transitions such as Open and Export.

## Layout and responsive behavior

The top toolbar is 48 points high and intentionally sparse. The right Develop
panel defaults to 312 points, and the status strip is 27. At window widths of
1120 points or wider, a 190-point Files rail shows the current document. Below
that breakpoint the rail disappears and Open/Close remain available through
the application menu, preserving central viewport space. Toolbar labels also
compact below 1020 points.

The viewport owns the darkest surface and has no permanent chrome over the
photo. The embedded/developed source badge appears while an embedded preview is
being replaced or while the pointer is over a developed image. Wheel changes
show a transient zoom badge. Fit, 100%, and pan transform only the sampled
texture.

The histogram and clipping controls are explicitly marked shells; they do not
pretend to contain image analysis. Before/after is visible but disabled until a
comparison preview can be retained correctly. Developer diagnostics remain in
a separate floating window. GPU display textures continue through the direct
egui-wgpu registration path without a presentation-only CPU readback.

## Manual visual check

Use a release build and a private sample so the embedded-to-developed transition
is visible:

```console
cargo run --release -p rohditor-desktop -- \
  --renderer wgpu --processor auto testdata/private/DSC00851.ARW
```

For each visual release, check the following at approximately 1280×800 and at
the 900×600 minimum window size:

- the custom palette remains unchanged under both a light and dark desktop
  appearance, while native portal dialogs remain desktop-native;
- the Files rail is present at normal width and absent at narrow width;
- the photo remains larger and visually louder than either control panel;
- the embedded badge is replaced by a developed CPU/GPU state without a blank
  frame;
- Fit, 100%, wheel zoom, transient zoom feedback, and drag pan remain usable;
- typed adjustment values, per-control reset, undo/redo, and reset-all behave
  as one coherent edit history;
- diagnostics, progress, notices, warnings, and errors are legible without
  covering the image;
- the histogram/clipping/before-after shells are visibly unavailable rather
  than presenting fabricated results.

Record the renderer, processor, desktop session, sample, window sizes, and any
exceptions in this section when the check is performed.

### Recorded check: 2026-08-31

The Phase 7 implementation was checked on Plasma/KWin Wayland with
`DSC00851.ARW`. The wgpu renderer shared its Vulkan device with the GPU preview
processor and selected `AMD Radeon RX 9070 XT (RADV GFX1201)`. The developed
texture was 2560×1707 and stayed on the direct egui-wgpu display path.

- Under `BreezeDark`, the 1280×800 window showed the Files rail, dominant
  viewport, grouped Develop panel, contextual controls, and compact status bar.
  A second capture at exactly 900×600 confirmed that the Files rail disappears,
  toolbar labels compact, the panel scrolls, and the viewport remains usable.
- Plasma was temporarily changed to `BreezeLight` and the 1280×800 run was
  repeated. Native window chrome became light while Rohditor's palette,
  hierarchy, borders, disabled states, and text contrast remained deliberate.
  `BreezeDark` was restored immediately and verified through `kdeglobals`.
- `--diagnostics` opened the themed developer window at startup. Opening the
  intentionally unsupported local `Cargo.toml` path produced a wrapped red
  error card. That check found that messages were initially below the export
  fold; they now appear directly below the document identity.
- Fit and 100% states were inspected in both layouts. Focus/type behavior and
  one-undo-step slider gestures are covered by desktop regressions; viewport
  regressions cover fit, actual size, wheel zoom, retained pan, and transient
  zoom feedback. The live RAW run completed the embedded-to-developed worker
  path without a blank final viewport, though the embedded frame was shorter
  than the screenshot tool's capture latency.
- Histogram, clipping, and before/after appeared visibly unavailable and did
  not imply fabricated analysis. The final neutral controls omit their reset
  glyphs; reset appears only after a value leaves neutral.

One native-chrome exception remains: KWin/Wayland retains eframe's fallback
window icon when the app is run directly from Cargo, ignoring the runtime icon
payload. Phase 8 packaging should provide a stable Wayland application ID,
installed `.desktop` metadata, and a real icon instead of retaining a
procedural fallback that does not affect the target session.
