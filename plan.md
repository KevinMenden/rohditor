# Rohditor implementation plan

## 1. Purpose

Rohditor is a personal, Linux-first RAW photo editor tailored to one Sony camera and one user's workflow. It will be implemented primarily in Rust, use the GPU for interactive image processing where that is beneficial, and retain a complete CPU processing path so that editing and export do not depend on a hardware GPU.

The project will be developed incrementally. The first MVP is deliberately small: open a supported Sony RAW file, display a developed preview, apply a few global adjustments, and export a JPEG or PNG. The architecture must allow later additions such as better demosaicing, lens correction, local masks, noise reduction, sidecars, batch processing, and color-managed displays without requiring a rewrite of the MVP.

This document records the current decisions, why they were made, the proposed structure, the exact implementation order, and the acceptance criteria for each milestone.

## 2. MVP scope

### 2.1 Included

The MVP will:

- Run as a native Linux desktop application.
- Open RAW files produced by the target Sony camera in every RAW compression mode that the owner actually uses.
- Extract the embedded preview for fast initial display when available.
- Decode the sensor mosaic and required metadata.
- Produce an orientation-correct developed preview from the RAW sensor data.
- Provide non-destructive global controls for:
  - white balance,
  - exposure,
  - contrast,
  - saturation,
  - reset to defaults.
- Let the user select `Auto`, `GPU`, or `CPU` processing.
- Export an 8-bit sRGB JPEG with a quality setting.
- Export an 8-bit or 16-bit sRGB PNG.
- Preserve the source RAW without modifying it.
- Include a headless CLI for inspection, development, testing, and export.
- Remain responsive while decoding, processing, or exporting.

### 2.2 Explicitly deferred

The following are not part of the first MVP:

- A photo catalog or database.
- Folder browsing, ratings, tags, or library management.
- Local adjustments, brushes, gradients, or masks.
- Lens correction.
- Perspective correction.
- Noise reduction and sharpening beyond a possible minimal display sharpening pass.
- HDR merging, panorama stitching, or focus stacking.
- Pixel-perfect emulation of Sony's in-camera JPEG rendering.
- Plugin support.
- Cross-platform packaging.
- Full monitor ICC color management.
- DCP camera profiles or automatic ColorChecker calibration.
- Full-resolution GPU export.
- A general-purpose dynamic processing-node graph.

These items may be added later, but MVP code must not assume they will never exist.

## 3. Assumptions and validation gate

The exact Sony camera model is not yet recorded. RAW support must not be assumed from the `.ARW` extension alone because Sony cameras can produce uncompressed, lossless-compressed, and lossy-compressed variants with model-specific metadata.

Before implementing the editor pipeline, collect a private validation corpus containing at least:

- The exact camera model name as stored in EXIF.
- One file for every RAW compression and bit-depth mode that will be used.
- A normal daylight image with neutral objects.
- A high-dynamic-range image with bright highlights and deep shadows.
- A low-light/high-ISO image.
- A portrait or other image with important skin tones, if relevant to the intended workflow.
- A deliberately rotated portrait-orientation image.
- Ideally, matching in-camera JPEGs for visual reference. These are references, not required output targets.

Real RAW samples belong in `testdata/private/` and must be ignored by version control. Small synthetic mosaics and freely redistributable fixtures may live in `testdata/synthetic/`.

**Gate A: decoder approval**

Implementation may proceed beyond the decoder spike only after every required camera mode can be decoded with correct dimensions, CFA layout, crop, black/white levels, orientation, as-shot white balance, and camera color matrices. If `rawler` fails this gate, first investigate a contained fix or update. If it remains unsuitable, add an optional LibRaw adapter despite the additional C/C++ dependency. The rest of the architecture must not depend directly on either library.

## 4. Architectural decisions

### D-001: Use a Rust 2024 Cargo workspace

**Decision:** Application code will use stable Rust, the Rust 2024 edition, and a Cargo workspace. `Cargo.lock` will be committed. The workspace will use Cargo feature resolver version 3.

**Why:** Rust provides the desired performance, memory safety, concurrency, and a single host-language codebase. A workspace preserves clear boundaries without forcing the entire application into one large crate. Committing the lock file is important because `wgpu`, `egui`, and `rawler` evolve quickly.

### D-002: Keep the UI separate from image-processing logic

**Decision:** The desktop UI will depend on the core, RAW, and GPU crates; none of those crates may depend on the desktop UI.

**Why:** The same processing code must be usable from the GUI, CLI, tests, and benchmarks. This also makes headless CPU operation possible.

### D-003: Use `rawler` through a private adapter

**Decision:** Use `rawler` for the initial Sony RAW decoder, but expose it to the rest of the program only through Rohditor-owned types and a `RawDecoder` boundary.

**Why:** `rawler` is written in Rust and supports many Sony ARW variants, which best matches the one-language preference. Its API is not stable and RAW decoding is a high-risk dependency, so isolating it makes upgrades or replacement feasible.

`rawler`'s complete development pipeline will not become Rohditor's public architecture. It may be used as a temporary comparison/reference during bring-up.

### D-004: Treat input files as trusted during the MVP

**Decision:** The MVP is intended for RAW files produced by the owner's camera. It will validate sizes and report ordinary decoding errors, but it will not claim sandbox-grade safety for hostile RAW files.

**Why:** `rawler` explicitly prioritizes decoder development over panic-free processing of malformed input. If support for arbitrary untrusted files is needed later, decoding should be moved to a separate worker process with memory and time limits.

### D-005: Use a CPU reference backend and a `wgpu` backend

**Decision:** Every required MVP processing operation will have a CPU implementation. GPU implementations will be checked against CPU results within documented tolerances.

**Why:** The CPU path guarantees operation without a hardware GPU and provides a simple correctness oracle. It also prevents shader behavior from becoming the only specification of the image pipeline.

### D-006: Use `wgpu`, Vulkan, and WGSL rather than ROCm-specific APIs

**Decision:** GPU processing will use `wgpu` compute/render pipelines with WGSL shaders. Linux will normally use Vulkan and request a high-performance adapter.

**Why:** `wgpu` supports the RX 9070 XT through the normal Linux Vulkan stack while remaining portable and integrated with the selected GUI. ROCm would create a vendor-specific compute path, complicate display interop, and make fallback behavior harder. CUDA is not applicable to the AMD GPU.

WGSL is the one intentional second implementation language. Experimental Rust-to-GPU toolchains will not be used for the MVP.

### D-007: Use `egui`/`eframe` for the desktop application

**Decision:** Build the UI with `egui` and `eframe`, compiling both the `wgpu` and `glow` renderers where supported.

**Why:** The application is personal and custom, so rapid iteration matters more than native-looking widgets. The `wgpu` renderer exposes its device and textures, allowing GPU-developed previews to be displayed without a CPU readback.

The `glow` renderer provides a possible UI fallback when a usable `wgpu` adapter cannot be created. In that mode, processing is CPU-only and the CPU preview is uploaded as a normal UI texture.

### D-008: Separate display rendering from image processing

**Decision:** The settings `renderer` and `processor` are distinct:

- Renderer: `Auto`, `Wgpu`, or `Glow`.
- Processor: `Auto`, `GPU`, or `CPU`.

**Why:** A CPU-processing session still needs something to draw the desktop window. Conversely, the UI renderer should not decide whether full image processing is executed on the GPU.

In automatic mode:

1. Prefer the `wgpu` UI renderer.
2. Prefer a hardware GPU processor when a suitable compute device exists.
3. Otherwise use CPU processing while retaining any available UI renderer.
4. If `wgpu` initialization is unavailable, try `glow` with CPU processing.
5. The CLI remains usable without either renderer.

### D-009: Use a non-destructive, versioned edit recipe

**Decision:** RAW pixels are immutable. All user changes are represented by a serializable `EditRecipe` with a schema version.

**Why:** This gives predictable reset, undo/redo, reproducible export, and a direct path to JSON/RON sidecars later. It also allows background jobs to render a stable snapshot while sliders continue to change.

### D-010: Use explicit image-state types and a scene-linear working space

**Decision:** Use different types for mosaic data, scene-linear RGB, and display/output RGB. The working space will initially be linear Rec.2020 with a D65 white point. CPU processing uses `f32`. GPU working textures normally use `RGBA16Float`, with `f32` shader arithmetic where available.

**Why:** Explicit types prevent operations from accidentally mixing sensor, linear, and gamma-encoded values. Linear Rec.2020 provides a wider working gamut than linear sRGB while remaining simpler than an ACES/D60 workflow. `f32` is a clear and sufficiently precise CPU reference; half-float GPU storage substantially reduces memory and bandwidth.

No stage may silently clamp scene-linear values to `[0, 1]`. Clipping or gamut mapping occurs only in an explicitly named output transform.

### D-011: Use a fixed ordered pipeline for the MVP

**Decision:** Implement a fixed sequence of typed stages with cache/invalidation metadata. Do not implement a general node graph yet.

**Why:** A graph engine adds scheduling, lifetime, invalidation, serialization, and UI complexity before local edits require it. Typed fixed stages are easier to test and teach. Stage boundaries will still be explicit enough to become graph nodes later.

### D-012: Use separate preview and export paths

**Decision:** Interactive previews are resolution-limited and cached. MVP exports are rendered at full resolution on the CPU. The stage API must carry image regions/halos so tiled CPU and GPU export can be added later.

**Why:** A 61 MP RGB `f32` image occupies roughly 732 MB before extra buffers. Keeping several full-resolution intermediates for every slider movement is wasteful. GPU acceleration provides its largest immediate benefit when a preview remains resident on the GPU. CPU export is initially simpler, deterministic, and guarantees fallback behavior.

### D-013: Do not block the UI thread

**Decision:** RAW decode, preview development, and export run on background workers. Use ordinary Rust threads, scoped work, and channels first; do not introduce Tokio solely for this desktop pipeline.

**Why:** Image work is CPU/GPU-bound rather than network-bound. A small explicit job system is easier to reason about. Jobs carry a document revision, and obsolete preview results are discarded.

### D-014: Use `rayon` for CPU data parallelism

**Decision:** Parallelize CPU image stages over rows or tiles with `rayon`. Begin with compiler auto-vectorization; add explicit SIMD only after profiling.

**Why:** Rayon provides simple work-stealing parallelism and keeps the initial implementation readable. Manual SIMD and fixed tile sizes should be justified by measurements on the actual machine.

### D-015: Use `image` for MVP JPEG and PNG encoding

**Decision:** Use the Rust `image` ecosystem for 8-bit JPEG and 8/16-bit PNG export. Embed an sRGB profile and selected EXIF data when the encoder supports it.

**Why:** It supports both required formats without adding a C/C++ codec dependency. More specialized encoders can be evaluated later using benchmarks and metadata requirements.

### D-016: Phase in color management

**Decision:** The MVP owns the camera-matrix, working-space, and sRGB transforms explicitly. Monitor ICC conversion and additional output profiles are post-MVP work, likely using `moxcms`.

**Why:** RAW input color and display color management are separate problems. Keeping the initial output fixed to sRGB makes results testable while preserving a clear insertion point for ICC transforms.

### D-017: Use a fused GPU preview pipeline where practical

**Decision:** Adjacent per-pixel GPU operations such as matrix conversion, exposure, saturation, and tone adjustment should be combined when doing so does not obscure their defined CPU equivalents.

**Why:** Extra full-frame passes cost memory bandwidth and intermediate textures. Logical stages remain separate in the core specification even if a GPU shader evaluates several stages in one pass.

## 5. Proposed repository structure

```text
rohditor/
├── Cargo.toml
├── Cargo.lock
├── rust-toolchain.toml
├── plan.md
├── apps/
│   ├── desktop/
│   │   ├── Cargo.toml
│   │   └── src/
│   │       ├── main.rs
│   │       ├── app.rs
│   │       ├── commands.rs
│   │       ├── render_coordinator.rs
│   │       └── ui/
│   └── cli/
│       ├── Cargo.toml
│       └── src/main.rs
├── crates/
│   ├── core/
│   │   ├── Cargo.toml
│   │   ├── benches/
│   │   └── src/
│   │       ├── document.rs
│   │       ├── edit.rs
│   │       ├── image.rs
│   │       ├── color.rs
│   │       ├── pipeline.rs
│   │       ├── cpu/
│   │       └── export.rs
│   ├── raw/
│   │   ├── Cargo.toml
│   │   └── src/
│   │       ├── lib.rs
│   │       └── rawler.rs
│   └── gpu/
│       ├── Cargo.toml
│       └── src/
│           ├── lib.rs
│           ├── context.rs
│           ├── pipeline.rs
│           └── shaders/
├── testdata/
│   ├── synthetic/
│   └── private/
└── docs/
    ├── architecture.md
    └── color-pipeline.md
```

The additional documents should be created only when implementation begins to exceed this plan. `plan.md` remains the project roadmap and decision record.

## 6. Core interfaces and data ownership

Names below are provisional Rust names but represent committed architectural boundaries.

### 6.1 Decoder boundary

```rust
pub trait RawDecoder: Send + Sync {
    fn probe(&self, path: &Path) -> Result<RawFileInfo, RawError>;
    fn decode(&self, path: &Path) -> Result<RawFrame, RawError>;
    fn embedded_preview(&self, path: &Path) -> Result<Option<EncodedPreview>, RawError>;
}
```

`RawFrame` owns or shares:

- `Arc<[u16]>` mosaic samples.
- Width, height, row stride, and active crop.
- CFA description, not merely an assumed RGGB enum.
- Per-channel or per-row black levels when available.
- Sensor white level.
- As-shot white-balance values.
- Camera-to-XYZ/color calibration matrices and their illuminants.
- Orientation.
- Capture metadata required by the UI/exporter.
- Source identity/fingerprint.

No `rawler` type crosses this boundary.

### 6.2 Image-state types

At minimum, define:

- `MosaicImage<T>` for one-channel CFA samples.
- `LinearRgbImage<T>` with an explicit working-space marker.
- `DisplayRgbImage<T>` with an explicit transfer/profile marker.
- `ImageRegion` and `Halo` for future tile processing.

Dimensions, stride, and channel layout must be explicit. Do not use an unlabelled `Vec<f32>` as a public image type.

### 6.3 Edit recipe

The initial recipe will contain:

```text
schema_version
white_balance:
  AsShot | ManualMultipliers { red, green, blue }
exposure_ev
contrast
saturation
orientation_override (optional)
```

For the MVP, manual white balance is stored as channel multipliers relative to the decoded as-shot balance. The UI may present these as relative warmth/tint controls, but it must not label a value as an exact Kelvin temperature until a defined chromaticity/CCT conversion and dual-illuminant camera-profile interpolation are implemented.

Later fields must have defaults so older recipes can be migrated. Deserialization must check `schema_version`.

### 6.4 Processing boundary

```rust
pub trait PipelineExecutor {
    fn capabilities(&self) -> PipelineCapabilities;
    fn render_preview(&mut self, request: PreviewRequest) -> Result<PreviewResult, PipelineError>;
    fn render_export(&mut self, request: ExportRequest) -> Result<ExportImage, PipelineError>;
}
```

This interface describes behavior, not necessarily one trait object in the final implementation. CPU and GPU code may have different internal ownership requirements, especially around `wgpu::Device`, `Queue`, and texture lifetimes.

### 6.5 Document and jobs

`Document` contains:

- Immutable source identity and metadata.
- Current `EditRecipe`.
- Monotonic `revision: u64`.
- Undo/redo history of edit commands.
- Current preview handle and preview status.
- No mutable reference to an in-flight processing buffer.

Every background request carries its source identity and revision. A result is installed only if both still match the active document. Export takes an immutable recipe snapshot and is not affected by later slider changes.

## 7. Defined MVP image pipeline

The logical order is:

1. Decode/unpack Sony ARW into a sensor mosaic and metadata.
2. Apply the active sensor crop and orientation metadata bookkeeping.
3. Subtract black level, handle per-channel black levels where present, and normalize against the sensor white level.
4. Apply white-balance multipliers without prematurely clipping highlights.
5. Demosaic the Bayer mosaic into camera RGB.
6. Transform camera RGB through the supplied calibration matrix into XYZ and then linear Rec.2020/D65, including chromatic adaptation when required.
7. Apply exposure as a linear gain of `2^exposure_ev`.
8. Apply the defined contrast/tone curve around a documented pivot.
9. Apply saturation using a documented working-space luminance definition.
10. Apply the display/output transform, including the initial highlight policy and conversion to linear sRGB.
11. Apply the sRGB transfer function.
12. Quantize with optional dithering.
13. Display or encode the result.

The first CPU demosaic algorithm will be bilinear because it is compact and easy to validate with synthetic fixtures. A higher-quality algorithm such as MHC, PPG, or AHD is a separate milestone. Demosaicing is selected through an enum from the beginning so the baseline algorithm does not become hard-coded throughout the application.

The initial output transform may clamp out-of-gamut and over-range values, but that behavior must live in a named function and have tests. A highlight roll-off and better gamut mapping are post-MVP improvements, not implicit behavior scattered across adjustment code.

## 8. Cache and invalidation model

Preview processing will cache these conceptual levels:

1. `DecodedRaw`: source-dependent only.
2. `NormalizedMosaic`: source, crop, black/white-level policy, and preview scale.
3. `DemosaicedBase`: normalized mosaic, white balance, demosaic algorithm, and working-space transform.
4. `AdjustedPreview`: base plus exposure, contrast, saturation, and output transform.

An edit declares the earliest stage it invalidates. Exposure must not cause ARW decoding or demosaicing to repeat. A white-balance change may initially invalidate `DemosaicedBase`; if the chosen demosaic remains linear, an optimized implementation may apply balance after demosaic while retaining equivalent CPU/GPU behavior.

The default developed preview target will be configurable, initially using a 2560-pixel long edge. The embedded JPEG is only a loading placeholder and is replaced by a preview generated from RAW data.

## 9. Error model and diagnostics

Library crates use typed errors with `thiserror`. Application binaries may use `anyhow` only at command/UI boundaries where contextual reporting is more useful than exhaustive matching.

Errors must distinguish at least:

- Unsupported camera or RAW variant.
- Corrupt or incomplete input.
- Invalid/missing calibration metadata.
- Allocation or unreasonable-dimension refusal.
- GPU adapter/device initialization failure.
- Shader/pipeline failure.
- CPU/GPU processing failure.
- Export encoding failure.
- Destination I/O failure.
- Cancelled or obsolete job.

Use `tracing` spans around decode and every pipeline stage. Log the selected UI renderer, processing backend, adapter name, graphics backend, important limits, preview dimensions, cache hits, stage durations, and export duration. Never log full private file paths unless verbose diagnostics are enabled.

## 10. Detailed implementation sequence

Each phase is a vertical checkpoint. Do not begin a later phase when the previous phase's acceptance criteria are failing, except for small exploratory spikes that are not merged into the main path.

### Phase 0: Record hardware/input facts and scaffold the workspace

#### Tasks

1. Record the Sony model, camera firmware, enabled RAW modes, CPU, RAM, Linux distribution, Mesa version, kernel version, and Vulkan adapter information in a local development note.
2. Add the private sample corpus described in Section 3.
3. Create `rust-toolchain.toml` using stable Rust with `rustfmt` and `clippy` components.
4. Create the workspace and the `core`, `raw`, `gpu`, `desktop`, and `cli` packages shown above.
5. Add `.gitignore` entries for `target/`, `testdata/private/`, generated images, traces, and local benchmark output.
6. Configure workspace-wide Rust 2024 edition, resolver 3, license metadata, and lint policy.
7. Add a minimal CI/check script running formatting, clippy, and tests with no hardware GPU requirement.
8. Add `README.md` with build prerequisites and commands after those prerequisites have been validated on the target Linux installation.

#### Acceptance criteria

- `cargo fmt --check` passes.
- `cargo clippy --workspace --all-targets` passes without unexplained warnings.
- `cargo test --workspace` passes without a GPU.
- Desktop and CLI binaries print their versions and exit successfully.
- Private RAW samples cannot accidentally be committed.

### Phase 1: RAW decoder and inspection CLI

#### Tasks

1. Define Rohditor-owned RAW metadata and `RawFrame` types in the `raw` crate.
2. Implement the `rawler` adapter without exposing `rawler` types publicly.
3. Validate dimensions before allocating buffers and define a configurable maximum input size.
4. Implement embedded-preview extraction.
5. Add `rohditor-cli inspect <file>` with human-readable output and optional JSON output.
6. Report make, model, dimensions, crop, CFA, bit depth, black levels, white level, white balance, matrices, orientation, ISO, shutter, aperture, and embedded-preview information.
7. Add `rohditor-cli extract-preview <file> <output>` for diagnosis and placeholder validation.
8. Run the commands against every private sample and store scrubbed expected metadata in tests where possible.
9. Complete Gate A. If it fails, make the raw-decoder decision before continuing.

#### Acceptance criteria

- Every required Sony RAW mode is correctly identified and decoded.
- Mosaic sample counts and strides match validated dimensions.
- Portrait samples report the correct orientation.
- Invalid extensions do not determine format support; content probing does.
- Unsupported or corrupt files produce errors rather than UI/CLI hangs.
- No code outside the adapter imports a `rawler` type.

### Phase 2: CPU reference pipeline

#### Tasks

1. Implement typed mosaic, linear RGB, and display RGB buffers.
2. Implement active-area cropping and black/white normalization.
3. Create synthetic Bayer fixtures covering RGGB and any CFA pattern used by the target camera.
4. Implement and test bilinear demosaicing on the CPU.
5. Parse and validate as-shot white balance and camera matrices.
6. Implement camera RGB to XYZ to linear Rec.2020/D65 conversion.
7. Implement the sRGB output matrix and transfer function independently of the export codec.
8. Define adjustment parameter ranges and neutral values.
9. Implement exposure, relative manual white balance, contrast, and saturation.
10. Define and test the initial highlight clipping/output policy.
11. Parallelize row/tile operations with Rayon after the serial tests pass.
12. Add `rohditor-cli develop <input> <output>` with explicit recipe arguments.
13. Add deterministic golden outputs or numeric checks from the private sample corpus.
14. Record stage timings and peak-memory observations.

#### Acceptance criteria

- A neutral recipe produces a correctly oriented, recognizable, reasonably colored image from every required sample.
- Exposure gain matches `2^EV` numerically before the tone/output transform.
- Neutral adjustment values are identity operations within floating-point tolerance.
- Synthetic demosaic and matrix tests pass.
- Processing succeeds when Rayon is restricted to one thread and to multiple threads.
- CLI export works without initializing `wgpu`, a window system, or a physical GPU.
- Repeated CPU rendering of the same recipe is deterministic within the chosen encoder constraints.

### Phase 3: Export implementation

#### Tasks

1. Define `ExportSettings` independently of UI widgets.
2. Implement 8-bit sRGB JPEG export with configurable quality.
3. Implement 8-bit and 16-bit sRGB PNG export.
4. Embed an sRGB ICC profile when supported.
5. Preserve selected safe EXIF fields and orientation semantics; do not copy a stale orientation tag after physically rotating pixels.
6. Add optional output dithering before integer quantization.
7. Write to a temporary sibling file and atomically rename on successful completion where the filesystem permits it.
8. Refuse accidental overwrite unless the caller explicitly enables it.
9. Add CLI integration tests for dimensions, bit depth, orientation, and decodability of outputs.

#### Acceptance criteria

- JPEG and PNG outputs open in at least two independent Linux viewers.
- Output dimensions and orientation are correct.
- The output is tagged as sRGB.
- JPEG quality changes file size and visible compression as expected.
- 16-bit PNG contains 16-bit samples rather than an up-converted 8-bit buffer.
- A failed export does not leave the final destination partially written.

### Phase 4: Minimal desktop application using the CPU pipeline

#### Tasks

1. Start an `eframe` application with the `wgpu` renderer and optional `glow` fallback compiled in.
2. Implement a native open dialog using the XDG portal-capable `rfd` backend.
3. Add the central image viewport and right adjustment panel.
4. Display the embedded preview as a temporary placeholder.
5. Implement a render coordinator and background worker messages.
6. Develop the configured 2560-pixel RAW preview on the CPU.
7. Upload CPU results as display textures and replace the placeholder.
8. Add white-balance, exposure, contrast, saturation, and reset controls.
9. Increment document revision for every committed recipe change.
10. Coalesce rapid slider changes and discard stale preview results.
11. Add zoom, fit-to-window, and basic panning without triggering unnecessary RAW development.
12. Add export dialog/settings and run export on a background worker.
13. Show progress, selected backend, and actionable errors.
14. Add an in-memory command history for undo/redo of edit changes.

#### Acceptance criteria

- Opening, adjustment, and export never perform long work on the UI thread.
- Rapid slider movement cannot install an older result over a newer revision.
- Reset and undo/redo produce the expected recipes and previews.
- Closing a document safely abandons its pending preview work.
- CPU mode works when GPU processing is disabled.
- Export uses a recipe snapshot and is unaffected by subsequent UI changes.

### Phase 5: GPU capability detection and first accelerated preview

#### Tasks

1. Define `RendererPreference` and `ProcessorPreference` and expose them through CLI flags/configuration before adding settings UI.
2. When `eframe` uses `wgpu`, obtain and share its `Device`, `Queue`, adapter information, and target format rather than creating a second GPU device.
3. Implement capability detection for texture formats, storage textures, maximum dimensions, timestamp queries, and adapter type.
4. Build a GPU pipeline that accepts an already demosaiced linear preview.
5. Upload the linear base preview once and keep it resident.
6. Implement the working-space adjustment and sRGB display transform in WGSL.
7. Render into an off-screen `RGBA16Float` working texture and a UI-compatible display texture.
8. Register/display the resulting native `wgpu` texture through `egui-wgpu` without readback.
9. Fuse compatible per-pixel stages after individual-stage parity has been established.
10. Implement `Auto`, `GPU`, and `CPU` selection and visible backend reporting.
11. If GPU initialization or processing fails in `Auto`, report the reason in diagnostics and fall back to CPU processing for the document.
12. Add CPU/GPU numeric comparison tests that can be run locally on the target GPU.

#### Acceptance criteria

- The application reports the RX 9070 XT and Vulkan backend when selected.
- Adjustments do not upload the unchanged base image for every slider event.
- GPU previews are displayed without copying the final image back to CPU.
- CPU and GPU display outputs differ by no more than the documented tolerance; the initial target is at most 2 code values in 8-bit sRGB for ordinary in-gamut pixels.
- `ProcessorPreference::CPU` does not create image-processing GPU resources.
- Automatic GPU failure leaves the document editable through the CPU path.
- No shader contains an undocumented color transform or clamp absent from the CPU specification.

### Phase 6: Preview caching, cancellation, and performance pass

#### Tasks

1. Implement the cache levels from Section 8 with explicit keys.
2. Avoid repeating decode and demosaic for downstream-only edits.
3. Add cooperative cancellation checkpoints to CPU stages where worthwhile.
4. Bound the number of queued preview jobs; prefer the newest revision.
5. Reuse CPU buffers and GPU textures rather than allocating on every edit.
6. Add `tracing` stage timings and a small developer diagnostics panel.
7. Add Criterion benchmarks for normalization, demosaic, adjustments, and output conversion.
8. Measure preview performance and memory on representative high-resolution files.
9. Adjust preview size, pass fusion, tile size, and buffer lifetime based on measurements.

#### Target budgets on the reference workstation

These are goals rather than cross-machine correctness requirements:

- Embedded placeholder visible within 500 ms for a local SSD file when available.
- First developed preview visible within 2 seconds for a representative file.
- Cached GPU adjustment preview at or below 33 ms at the default preview size.
- Cached CPU adjustment preview at or below 250 ms at the default preview size.
- No unbounded growth while repeatedly moving sliders or opening documents.
- Interactive mode should avoid allocating full-resolution `f32` RGB intermediates.

#### Acceptance criteria

- Benchmarks and traces demonstrate that cache hits skip expected stages.
- Memory returns to a stable range after obsolete jobs finish.
- The newest slider state becomes visible even when an older expensive job was started first.
- Performance measurements are documented with image dimensions and active backend.

### Phase 7: MVP stabilization

#### Tasks

1. Test all private samples through GUI and CLI in CPU mode.
2. Test GPU mode and CPU/GPU parity on the RX 9070 XT.
3. Test automatic fallback by forcing CPU and unavailable-adapter scenarios.
4. Validate behavior on both Wayland and X11 if both are relevant to the target system.
5. Validate file dialogs on the installed desktop environment.
6. Add corrupted/truncated derivative fixtures where redistribution permits.
7. Review all allocation calculations and conversions from file-provided dimensions.
8. Add a diagnostics export containing versions, adapter data, stage timings, and errors but no image contents.
9. Document build, run, CLI, supported camera modes, known color limitations, and troubleshooting instructions.
10. Produce a release build and record binary size, initial-load performance, export time, and peak memory.

#### MVP definition of done

- Every target-camera RAW mode passes Gate A and end-to-end rendering.
- The application opens a RAW, shows a developed preview, applies all scoped edits, and exports valid JPEG/PNG files.
- Source RAW files are never modified.
- CPU mode is complete and does not require GPU processing.
- GPU mode accelerates interactive adjustments and matches the CPU reference within tolerance.
- The UI remains responsive during decode and export.
- Failures are actionable and do not silently produce a misleading image.
- Known differences from Sony's in-camera rendering are documented.

## 11. Testing strategy

### 11.1 Unit tests

Unit-test:

- Black/white normalization, including per-channel levels.
- CFA indexing at image borders.
- Every supported Bayer layout.
- Demosaicing on small hand-checkable mosaics.
- Matrix direction, white point, and chromatic adaptation.
- Exposure identity and known EV gains.
- Contrast neutral behavior and pivot behavior.
- Saturation neutral behavior and grayscale preservation.
- Linear/sRGB transfer round trips.
- Quantization and dithering bounds.
- Edit-recipe defaults and schema migration.
- Cache-key invalidation.

### 11.2 Integration tests

Integration-test:

- Decoder metadata against scrubbed expectations.
- CPU pipeline output dimensions and orientation.
- CLI inspect/develop/export commands.
- JPEG/PNG re-decoding and metadata.
- Revision handling and stale-result rejection.
- Explicit CPU operation with graphics initialization disabled.

### 11.3 Golden image tests

Use small redistributable or synthetic images in normal CI. Keep full private-camera goldens in a local opt-in suite. Compare linear or display pixels with numeric tolerances instead of compressed JPEG bytes.

Store alongside each golden:

- Input identity or checksum.
- Camera mode.
- Recipe JSON.
- Pipeline version.
- Expected dimensions and color encoding.
- Numeric tolerance and reason for it.

### 11.4 GPU parity tests

GPU tests are opt-in locally because normal CI may not expose a stable GPU. Compare stage outputs before quantization where possible. Maintain separate tolerances for `f32`, half-float storage, and final 8-bit output.

A visual match alone is insufficient. Conversely, GPU output does not need to be bit-identical when documented precision differences are harmless.

## 12. Dependency policy

- Prefer Rust implementations where quality and maintenance are adequate.
- Disable unused default features, especially in `image`, to reduce build size and attack surface.
- Keep `eframe`, `egui-wgpu`, and direct `wgpu` usage on compatible versions; avoid two independent `wgpu` versions in the same desktop binary.
- Pin the tested `rawler` version through `Cargo.lock` and keep all adaptation in one crate.
- Run `cargo tree -d` when upgrading graphics dependencies.
- Review licenses before distributing binaries.
- Do not add a general async runtime, ECS, plugin framework, database, or serialization format without a concrete current requirement.
- Prefer `thiserror` in libraries, `tracing` for diagnostics, `serde` for recipes/configuration, `rayon` for CPU pixel work, and `rfd` for dialogs.

## 13. Performance and memory rules

- Never process a full-resolution image merely because the viewport was resized.
- Never read a GPU preview back to the CPU just to display it.
- Never re-upload an unchanged base image for a downstream-only adjustment.
- Avoid more than two full-size working textures unless a measured algorithm requires them.
- Reuse allocations and texture pools after correctness is established.
- Process export in tiles when full-frame memory becomes excessive; neighborhood stages declare their required halo.
- Benchmark before adding manual SIMD, unsafe code, shader pass fusion, or custom allocators.
- Track dimensions and byte counts with checked arithmetic before allocation.

## 14. Post-MVP roadmap

Suggested order after the MVP:

1. Higher-quality selectable demosaic algorithm with CPU reference.
2. GPU normalization and GPU demosaic.
3. Better highlight reconstruction and filmic output transform.
4. JSON/RON sidecar save/load with recipe migration.
5. Proper temperature-in-Kelvin/tint UI and dual-illuminant camera profiles.
6. Monitor ICC support and soft proofing using a Rust color-management library.
7. Histogram, clipping warnings, and channel inspection.
8. Sharpening and noise reduction.
9. Lens profiles, distortion, vignetting, and chromatic-aberration correction.
10. Tiled full-resolution GPU export.
11. Crop/rotate/perspective tools.
12. Local masks and adjustments; reevaluate whether a processing graph is now justified.
13. Batch export and reusable presets.
14. Optional catalog only if the personal workflow actually needs it.

## 15. Open decisions

These must be answered through input samples or implementation measurements rather than guessed now:

- Exact Sony camera model and RAW variants.
- Typical megapixel count and available system RAM.
- Whether Wayland, X11, or both must be supported.
- Whether matching a preferred Sony Creative Look is eventually important.
- The desired default contrast/tone curve.
- Whether sidecar persistence is needed immediately after the MVP.
- Whether 16-bit TIFF should be the next export format.
- Which higher-quality demosaic algorithm gives the preferred quality/performance tradeoff.
- Whether full-resolution export exceeds acceptable RAM or time budgets on the target workstation.

## 16. Immediate next action

The next implementation session should complete Phase 0 and begin Phase 1. Before adding image-adjustment or GPU code, obtain the exact camera model and sample RAW modes, scaffold the workspace, implement `rohditor-cli inspect`, and make the decoder approval decision at Gate A.
