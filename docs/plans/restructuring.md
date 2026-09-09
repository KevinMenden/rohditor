# Rohditor maintainability and restructuring plan

**Status:** current implementation plan

**Last audit:** 2026-09-09

This document replaces the original restructuring review. It is based on the
current workspace, including the recently landed Standard rendering profile
and the in-progress white-balance changes in the working tree. It is a
maintainability plan, not a request to change image-processing behavior while
moving code.

## 1. Direction and stopping rules

Rohditor already has the important domain boundaries. The next restructuring
should make those boundaries easier to navigate and extend without creating a
crate for every long file.

A change belongs in a new crate only when it provides a real dependency or
ownership boundary:

- it is consumed by more than one existing crate;
- it has a small, independently testable domain API;
- it removes a heavyweight or unrelated dependency from another crate; or
- it is expected to evolve independently with a clear owner.

Otherwise, split the existing crate into private modules. In particular, do
not create separate crates for masks, denoising, histograms, resampling,
cancellation, individual adjustment groups, or desktop controllers merely
because those areas are growing.

Every restructuring step must preserve the deterministic CPU output, recipe
serialization, cache identity, cancellation, and CPU/GPU boundary. A move is
complete only after the focused tests, `./scripts/check.sh`, and the relevant
private/GPU suites have been run.

## 2. Boundaries that are already in place

These extractions are complete and should not be planned again:

| Boundary | Current owner | What it owns |
| --- | --- | --- |
| Typed image states | `rohditor-image` | Mosaic, linear RGB, display RGB, orientation, CFA, checked layouts |
| Edit domain | `rohditor-edit` | `EditRecipe`, schema migrations, validation, Light/Color/Geometry/Optics/Rendering intent |
| Bayer algorithms | `rohditor-demosaic` | Bilinear, MHC, RCD, AMaZE, cancellation and algorithm tests |
| RAW highlight algorithms | `rohditor-highlight` | Off, Clip, Local ratios, Opposed, detection, statistics, scratch estimates |
| Camera profiles | `rohditor-camera-profile` | Matrix-only DCP parsing, validation, profile payloads and evaluator identity |
| Lens correction | `rohditor-optics` | Lensfun database, matching, distortion/vignetting/TCA correction and provenance |
| Catalog primitives | `rohditor-catalog` | Folder scan, embedded-preview thumbnails, cache and catalog ordering |
| RAW decoding | `rohditor-raw` | `rawler` adapter, immutable `RawFrame`, metadata and decoder errors |

The dependency direction is healthy:

```text
rohditor-image
   ^       ^        ^          ^
   |       |        |          |
 edit  demosaic  highlight  optics
   ^       ^        ^          ^
   +-------+--------+----------+---- raw/camera-profile/catalog as domain inputs
                         ^
                    rohditor-core
                    ^             ^
              rohditor-gpu     apps/cli + apps/desktop
```

The diagram is conceptual rather than a complete Cargo graph. The invariant
is that no lower-level crate depends on `rohditor-core`, `rohditor-gpu`, or an
application. `rohditor-core` remains the deterministic CPU reference;
`rohditor-gpu` remains an optional downstream preview implementation.

## 3. Current pressure points

The following are the actual areas that now justify restructuring:

| Area | Approx. size | Responsibilities currently mixed | Recommended boundary |
| --- | ---: | --- | --- |
| `crates/core/src/cpu.rs` | 2,043 lines | normalization, WB, camera conversion, global edits, base rendering, output conversion, quantization, tests | private stage modules; keep `cpu` as a facade |
| `crates/core/src/pipeline.rs` | 1,610 lines | public API, preview preparation, cache products, memory estimates, render/export orchestration | `pipeline/` facade plus `types`, `prepare`, `render`, `memory` |
| `crates/core/src/color.rs` | 909 lines | matrices, camera calibration/profile resolution, WB coupling, transfer functions, output clipping | split calibration/transform/output; extract shared color math before gamut work |
| `crates/core/src/export.rs` | 861 lines | quantized image types, JPEG/PNG codecs, ICC/EXIF, validation and export policy | split render-output types from encoder/metadata code; extract only if the boundary stays useful |
| `crates/gpu/src/preview.rs` | 2,366 lines | resource lifecycle, upload packing, uniforms, dispatch, readback and parity tests | private `upload`, `parameters`, `resources`, `readback` modules |
| `apps/desktop/src/app.rs` | 3,741 lines | lifecycle, commands, document transitions, GPU events, export, UI model and view composition | keep application-local; split controller/actions from view composition |
| `apps/desktop/src/coordinator.rs` | 2,555 lines | worker protocol, scheduling, open/decode, preview/cache flow, sampling and export | existing coordinator modules: protocol, scheduler, worker, preview, sampling, export |
| `apps/desktop/src/preview_cache.rs` | 983 lines | four cache keys, provenance, eviction, memory accounting and tests | split key types/provenance from cache storage; keep one cache owner |
| `apps/desktop/src/ui/adjustment_panel.rs` | 1,355 lines | Light, Color, Rendering, RAW, Optics, histogram, messages and reset behavior | panel sections behind one facade |
| `apps/cli/src/main.rs` | 2,248 lines | Clap model, all commands, output formatting, quality tools and tests | `args`, `commands/*`, `output`, and optional devtools binary |
| `crates/edit/src/lib.rs` | 1,199 lines | recipe model, schema migration, validation, defaults and shared constants | split recipe groups and migration/validation modules; retain one public facade |
| `crates/raw/src/rawler_adapter.rs` | 742 lines | decoder session, metadata, mosaic conversion and embedded preview | internal adapter modules; keep rawler private |

Long files are not automatically bad. The goal is to make stage ownership and
public APIs obvious before masks, denoising, and richer gamut behavior add more
cross-cutting code.

## 4. Recommended module and crate work

### 4.1 First: module-only splits

Do this without changing public behavior or introducing new crates.

Suggested `rohditor-core` layout:

```text
core/src/
  cpu/
    mod.rs              public stage facade
    normalize.rs        crop, black/white levels, CFA validation
    white_balance.rs    gain resolution and temperature/tint math
    adjustments.rs      exposure, Standard, Light, HSL, grading
    display.rs          orientation, output conversion, quantization
    tests.rs             focused private tests
  pipeline/
    mod.rs              CpuPipeline facade and stage sequence
    types.rs             public options/results/timings
    prepare.rs           reconstruction and demosaiced-base preparation
    render.rs            preview/source-scale/export render orchestration
    memory.rs            checked peak-working-set estimates
  color/
    mod.rs              public facade
    calibration.rs      camera/profile resolution
    transforms.rs       matrices and chromatic adaptation
    output.rs           target conversion and gamut-policy dispatch
```

The current working-tree `white_balance.rs` is a useful first seam. It should
remain the single implementation used by CPU, camera-profile resolution, and
future temperature/tint controls; do not reintroduce a second implementation
inside `color.rs`.

For `rohditor-edit`, use private modules such as:

```text
edit/src/
  lib.rs                public facade and recipe assembly
  recipe.rs             EditRecipe and serde migration
  raw.rs                highlight method/settings
  light.rs              Light and tone-curve settings
  color.rs              WB, HSL and grading settings
  geometry.rs           orientation/crop settings
  optics.rs             lens settings
  rendering.rs          Neutral/Standard selection
  validation.rs         shared ranges and field validation
```

Keep the existing public names and derive/serde behavior during the move.
Recipe migration tests should remain next to the migration code.

### 4.2 Extract the shared color boundary before gamut mapping

The next plausible new library is `rohditor-color`, not a collection of
single-purpose matrix crates. It should own pure, reusable color operations:

- `Matrix3` and matrix composition/inversion;
- chromatic adaptation and standard RGB/XYZ matrices;
- linear sRGB transfer functions;
- Rec.2020/D65 to target-space conversion;
- the output gamut mapper and its versioned diagnostics once implemented.

It must not accept `RawFileInfo`, own recipe migration, or depend on GPU/UI.
`rohditor-core` should translate decoder/profile metadata into narrow
calibration inputs and resolve recipe choices. `rohditor-gpu` can consume the
same constants and algorithm contract while retaining its shader-specific
implementation.

Do not extract `rohditor-gamut` separately unless a second consumer or a
different target-space implementation proves that color and gamut need
independent release boundaries. A `color::gamut` module is the simpler first
home.

The output policy must remain downstream of Standard rendering and creative
edits. It must not leak into camera calibration, highlight reconstruction, or
the retained camera-native/demosaiced cache keys.

### 4.3 Keep export as an outer boundary for now

`core/export.rs` is a real candidate for `rohditor-export`, but extracting it
before the render result is independent of codec types would create a cycle.
First introduce a core-owned quantized display result and render options. Then,
if codec/metadata dependencies remain isolated, move JPEG/PNG, ICC/EXIF, and
transactional writes to `rohditor-export`:

```text
rohditor-export -> rohditor-core + rohditor-raw
rohditor-core  -/-> rohditor-export
```

Do not move deterministic dithering or display-image layout into the encoder
crate. The CPU reference should be able to render output bytes without taking
an encoder dependency.

### 4.4 Add seams for future masks and denoising, not crates yet

Masks and denoising are new processing domains, but they do not yet justify
standalone packages. Prepare the architecture by:

- representing future masks as validated, typed spatial data owned by the edit
  or image boundary, with an explicit source/display coordinate contract;
- keeping local-edit evaluation after base rendering and before output gamut
  mapping, unless a feature explicitly requires RAW-stage data;
- defining a processing-stage interface in core modules so local adjustments
  do not grow `cpu.rs` into another monolith; and
- keeping denoising separate from sharpening: capture noise reduction belongs
  near camera-native/demosaiced data, while output sharpening belongs after
  resizing and before encoding.

Do not add recipe fields, GPU uniforms, or empty modules for these features
until one complete algorithm is ready to pass through the CPU reference,
cache, preview, export, and UI seams.

## 5. Application and GPU organization

### Desktop

Keep all desktop code in `apps/desktop`. It has one consumer and should not
become a library. Split responsibilities as follows:

```text
app/
  mod.rs or facade       eframe update and top-level composition
  actions.rs             validated user intents and recipe transitions
  lifecycle.rs           open/close/save/dirty-document transitions
  gpu.rs                 eframe GPU attachment
document/
  state.rs               immutable frame, recipe, revision and history
  persistence.rs         sidecar/project recipe load/save boundary
coordinator/
  protocol.rs            requests, events and progress
  scheduler.rs           newest-wins mailbox and cancellation
  worker.rs              worker loop and error boundary
  open.rs                decode and embedded-preview setup
  preview.rs             cache/base/adjusted rendering
  sampling.rs            picker and auto-tone samples
  export.rs              export jobs
ui/
  adjustment_panel/{raw,light,color,rendering,optics,export}.rs
```

The most important missing seam is persistence: the current document recipe
and bounded undo/redo are in memory, while session persistence only remembers
the last folder. `open_path` currently abandons the old document. Add a
desktop-local persistence boundary before more editing state is introduced:
sidecar/project format, atomic writes, dirty state, save/load errors, and an
unsaved-change decision on close/switch. This is application workflow, not a
new core crate.

Views should return intents. Filesystem, worker, and recipe transitions should
be handled by controller/lifecycle code that can be tested without egui.

### GPU

Keep one `rohditor-gpu` crate. Split `preview.rs` internally while retaining
all `wgpu` ownership in that crate:

```text
gpu/src/preview/
  mod.rs          processor facade and public frame/source types
  resources.rs    textures, bind groups and frame reuse
  upload.rs       typed-image packing and provenance
  parameters.rs   Rust uniform packing and layout checks
  dispatch.rs     compute submission and queue completion
  readback.rs     test/picker readback
  parity.rs       CPU/GPU tests
```

Every WGSL uniform change must update the Rust word layout and a runtime
`wgpu` validation test. The output-gamut policy must be an explicit parameter;
the shader must not silently remain hard-clipping when CPU selects a different
policy.

### CLI and quality tools

Keep the CLI package, but split commands from the argument model. If
`quality-crops` or `verify-libraw` grows further, move those commands to an
`apps/devtools` binary package rather than placing test-only APIs in core.

## 6. Cache, recipe, and dependency contracts

- Cache keys include every pixel-producing input: algorithm versions, numeric
  options, camera/profile evaluator payloads, optics database provenance,
  rendering process version, and output gamut policy/version at the adjusted
  display level.
- A future local mask must invalidate the smallest correct level and carry its
  coordinate-space identity. It must not be smuggled into a global adjustment
  bitset.
- RAW-stage products remain reusable across dynamic white-balance changes only
  when the algorithm proves that property. Clip remains WB-sensitive.
- `EditRecipe` remains serializable and versioned. Schema migration belongs in
  `rohditor-edit`; application persistence chooses where serialized recipes
  are stored.
- Keep public facades small and default modules private. Temporary re-exports
  are acceptable during a migration, then should be removed deliberately.
- Add or retain a dependency-direction check covering all foundational crates,
  core, GPU, CLI, and desktop.

## 7. Implementation sequence

Each phase should be a behavior-preserving change unless its acceptance gate
explicitly names a product change.

1. **Baseline and module map.** Record normal test results, private/GPU suite
   availability, output fixtures, and the current public re-exports. Do not
   mix a restructuring diff with gamut or white-balance behavior changes.
2. **Core/edit module splits.** Split `cpu.rs`, `pipeline.rs`, `color.rs`, and
   `edit/lib.rs`; keep facades and APIs stable. Land the white-balance seam only
   with its focused tests and migration review.
3. **Desktop/GPU/CLI splits.** Move private implementation code behind the
   existing facades. Preserve document/revision IDs, newest-wins scheduling,
   cache eviction, and retained-frame behavior.
4. **Shared color boundary.** Extract `rohditor-color` only after the module
   split shows a stable calibration/transform API. Move pure matrix/transfer
   tests with it and leave recipe/RAW adapters in core.
5. **Output boundary.** Separate quantized render results from codecs. Extract
   `rohditor-export` only if the dependency graph remains one-way and the
   resulting public API is smaller than the current core facade.
6. **Persistence seam.** Add desktop-local recipe persistence and dirty-state
   transitions before masks or batch editing introduce more document state.
7. **Gamut/local-processing readiness review.** Verify that output policy,
   future masks, denoising, sharpening, and cache levels have explicit stage
   owners before implementing those algorithms.

## 8. Acceptance criteria

The restructuring is successful when:

- `rohditor-image`, `edit`, `demosaic`, `highlight`, `camera-profile`,
  `optics`, `catalog`, and `raw` remain independent of core/UI;
- core's public pipeline sequence can be read from its facade without opening
  a 2,000-line algorithm file;
- CPU, GPU, CLI, and desktop share one recipe/color/output contract rather than
  parallel parameter meanings;
- no module grows a second implementation of white balance, gamut mapping,
  rendering-profile math, or cache-key semantics;
- recipe and cache tests catch changes to pixel-producing inputs;
- desktop lifecycle tests cover save/load/dirty decisions once persistence is
  added; and
- normal checks pass, with release private-corpus and real-GPU results recorded
  separately when processing or shader behavior changes.

The near-term stopping point is the module-only split plus the shared color
boundary. Do not proceed to an export crate or additional algorithm crates if
those changes do not make ownership clearer.

## 9. Verification commands

```bash
cargo fmt --all
./scripts/check.sh
cargo test -p rohditor-core
cargo test -p rohditor-edit
cargo test -p rohditor-gpu
cargo test --release --workspace --tests -- --ignored --nocapture
cargo test --release -p rohditor-gpu -- --ignored --nocapture
git diff --check
```

The ignored commands require the private corpus and a usable hardware Vulkan
adapter; a CPU Vulkan rasterizer is structural validation, not GPU parity.
