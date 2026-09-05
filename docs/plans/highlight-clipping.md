# Highlight clipping implementation plan

**Status:** proposed  
**Scope:** the first RAW-highlight feature only: the destructive **Clip highlights** baseline  
**Out of scope:** local-ratio, opposed/inpainting, LCh, segmentation, guided-Laplacian, gamut-mapping, and tone-mapping algorithms

## 1. Review of the broader proposal

The architecture in
[`clipping-and-reconstruction.md`](clipping-and-reconstruction.md) is sound in
its important decisions:

- RAW highlight handling belongs in a processing crate, not in the decoder,
  desktop application, or demosaic implementation.
- The operation belongs in the normalized CFA domain before demosaicing.
- Detection and treatment should share one definition of an affected sample.
- Synthetic Bayer fixtures are the right correctness foundation.
- The CPU path should remain the deterministic reference, with explicit
  cancellation, statistics, and later GPU fallback behavior.

The proposal needs these adjustments for the current repository:

1. `rohditor-demosaic` is already a separate crate, and
   `rohditor_image::MosaicImage<f32>` already carries dimensions, stride, and
   Bayer phase. A new `RawHighlightInput` or duplicate normalized-mosaic type
   would add coupling rather than remove it. The highlight crate should consume
   `MosaicImage<f32>`.
2. Highlight clipping is not the first pipeline operation. It cannot run until
   the decoder's black and white levels have been applied. Its position is:

   ```text
   decode immutable RawFrame
     -> choose sensor crop and normalize CFA samples
     -> clip highlights
     -> demosaic
     -> preview resample
     -> white balance and camera color conversion
     -> edits
     -> output conversion
   ```

3. The Clip method is not reconstruction. It deliberately discards surviving
   channel data so clipped highlights reach a common ceiling. That makes it a
   useful, predictable baseline against which later reconstruction can be
   compared, but it should be named and documented honestly.
4. Nominal normalized white (`1.0`) and proven physical sensor saturation are
   not necessarily identical. Rohditor intentionally preserves normalized
   values above one. The API and diagnostics must call the selected boundary an
   **effective clipping threshold**, not claim that every affected sample was
   physically saturated.
5. A scalar `sample.min(1.0)` before white balance is insufficient. If the
   active gains are `[2.0, 1.0, 1.5]`, equal raw-domain ceilings become unequal
   `[2.0, 1.0, 1.5]` ceilings after white balance. The clip limits must account
   for the active white balance, and preview cache keys must reflect that
   dependency.
6. Do not introduce a reconstruction trait or empty modules for all future
   algorithms yet. A small crate with clipping and shared detection primitives
   is enough. Add a dispatcher/trait when a second algorithm creates a real
   common interface.

This placement agrees with darktable's Bayer path: its Clip mode operates on
the raw/CFA buffer and clamps a site against a channel-aware clipping level
before later pipeline work. Its manual also describes Clip as clamping the
remaining valid channel information and warns that the result is destructive.

## 2. User-visible behavior for the first slice

Add a per-image **Highlight reconstruction** setting with two methods:

```text
Off
Clip
```

Add an **Effective threshold** control for Clip, expressed relative to the
decoder-normalized white level:

```text
default: 1.0
allowed range: 0.5 ..= 1.5
```

The initial default remains **Off**. This preserves existing recipes, preview
performance, and image output while the destructive baseline is validated on
the private corpus. It also preserves over-range data for later highlight or
tone-mapping work. Enabling Clip is an explicit edit.

When Clip is active:

- values below the effective per-color limit remain unchanged;
- values above that limit are capped;
- negative values remain unchanged;
- no spatial propagation or invented detail occurs;
- all supported Bayer layouts behave identically apart from CFA phase;
- preview, Source 1:1, and export use the same algorithm and parameters.

Place the controls near the top of the desktop Light section. Use a method
selector now rather than a one-off checkbox so later algorithms can extend the
same recipe field without changing its meaning. Only show/enable the threshold
when Clip is selected.

CLI development should expose equivalent options:

```text
--highlight-reconstruction off|clip
--highlight-threshold <0.5..1.5>
```

The threshold flag without `clip` should be rejected rather than silently
ignored.

## 3. Mathematical contract

Normalization already produces a camera-native CFA sample for color `c`:

```text
s_c = (raw - black_c) / (white_c - black_c)
```

Let the validated active white-balance gains be:

```text
g = [g_r, g_g, g_b], where every gain is finite and positive
```

Let the user threshold be `t`, with `t = 1.0` meaning the decoder's nominal
white boundary. Define a common post-WB ceiling and pre-WB per-color limits:

```text
common_ceiling = t * min(g_r, g_g, g_b)
limit_c        = common_ceiling / g_c
output_c       = min(s_c, limit_c)
```

This guarantees:

```text
output_c * g_c <= common_ceiling
```

for every channel, without applying white balance before demosaic. Scaling all
three gains by the same positive factor therefore does not change which raw
samples are clipped, while the existing overall white-balance/exposure
behavior remains downstream.

Core owns this conversion because it already resolves recipe white balance
from RAW metadata. `rohditor-highlight` receives only validated channel limits;
it must not depend on `RawFrame`, `RawFileInfo`, `EditRecipe`, or desktop state.

Use these terms consistently:

- **nominal over-white site:** normalized input is greater than `1.0`;
- **affected site:** input is at or above its effective per-color limit;
- **changed site:** input is strictly above its limit and is reduced by Clip.

Do not label `affected_sites` as physically clipped sites. The decoder white
level and selected threshold are processing inputs, not proof of sensor-well
saturation.

## 4. Crate and API design

Create `crates/highlight` as `rohditor-highlight` and add it to the workspace.
Its dependencies should initially be limited to:

- `rohditor-image` for `MosaicImage`, `BayerPattern`, `CfaColor`, and allocation
  errors;
- `rayon` for deterministic row-parallel processing;
- `thiserror` for a typed public error;
- no dependency on `raw`, `core`, `edit`, `gpu`, or `apps/desktop`.

Suggested first layout:

```text
crates/highlight/
  Cargo.toml
  src/
    lib.rs       public types, validation, cancellation contract
    detect.rs    common threshold classification and optional mask creation
    clip.rs      destructive Clip implementation
  tests/
    clip.rs      public-API and asymmetric Bayer fixtures
  benches/
    clip.rs      representative fused-pass benchmark
```

The public API should express these concepts, without committing to a general
reconstruction trait yet:

```rust
pub struct ChannelClipLevels {
    pub red: f32,
    pub green: f32,
    pub blue: f32,
}

pub struct ClipStats {
    pub affected_sites: usize,
    pub changed_sites: usize,
    pub nominal_over_white_sites: usize,
    pub affected_by_channel: [usize; 3],
}

pub struct ClipOutput {
    pub mosaic: MosaicImage<f32>,
    pub stats: ClipStats,
}

pub fn clip(
    mosaic: MosaicImage<f32>,
    levels: ChannelClipLevels,
) -> Result<ClipOutput, HighlightError>;

pub fn clip_cancellable(
    mosaic: MosaicImage<f32>,
    levels: ChannelClipLevels,
    cancellation: &dyn CancellationCheck,
) -> Result<ClipOutput, HighlightError>;
```

Consume the normalized mosaic and reuse its allocation. Add a `data_mut()`
accessor to `MosaicImage<T>`, matching the existing mutable access on
`LinearRgbImage<T>`. This keeps the full-resolution stage in place and avoids
another approximately 96 MiB `f32` buffer for a 24 MP source.

`detect.rs` should own the single comparison rule used by both the fused Clip
pass and any requested mask:

```text
affected := sample >= limit_for(CFA color)
changed  := sample >  limit_for(CFA color)
```

Expose a compact `ClippingMask` constructor for tests and future diagnostics,
but do not allocate a full-frame mask in the normal Clip path. The production
pass should classify, cap, and collect row-local statistics in one traversal.
Tests must prove that its statistics agree with the materialized detector.

Validate all channel limits before touching the mosaic. Reject non-finite or
non-positive limits and non-finite input samples with coordinates. Check
cancellation at least once per output row. Padding between visible row width
and row stride must never be classified, counted, or changed.

## 5. Recipe ownership and migration

Highlight handling is image-specific and changes rendered pixels, so it belongs
in the versioned edit recipe rather than global application settings or
`RenderOptions`.

In `rohditor-edit`, add a raw-domain recipe group conceptually like:

```rust
pub struct RawAdjustments {
    pub highlights: HighlightAdjustments,
}

pub struct HighlightAdjustments {
    pub method: HighlightMethod,
    pub threshold: f32,
}

pub enum HighlightMethod {
    Off,
    Clip,
}
```

Keep these serializable intent types in `rohditor-edit`; convert them to
`rohditor-highlight::ChannelClipLevels` in `core`. This avoids making the
algorithm crate aware of recipe schema or UI ranges.

Bump `EDIT_RECIPE_SCHEMA_VERSION` from 3 to 4. Version 4 adds a defaulted `raw`
group. Migrate versions 1, 2, and 3 to version 4 with `HighlightMethod::Off` and
threshold `1.0`. Tests must cover:

- default recipe serialization and round-trip;
- v1, v2, and v3 migration;
- missing `raw`/`highlights` fields receiving defaults;
- unknown method rejection;
- NaN, infinity, and out-of-range threshold rejection;
- reset, undo, and redo treating the setting like every other edit.

## 6. Core pipeline integration

Add a small `crates/core/src/highlight.rs` adapter, analogous to the existing
`core/src/demosaic.rs`. It should:

1. resolve active WB gains using existing calibration code;
2. calculate the three pre-WB limits from the contract above;
3. add a tracing span containing dimensions, method, and threshold;
4. map `HighlightError` into `PipelineError` without string matching;
5. call the cancellable CPU implementation;
6. return the processed mosaic and statistics.

Call it immediately after `normalize_raw_cancellable` and before every
`demosaic_cancellable` call in both preparation paths:

- full-resolution/export and Source 1:1 `prepare_base_cancellable`;
- fit-preview `prepare_reconstructed_preview`.

The fit-preview reconstruction APIs currently do not accept a recipe because
their result is reusable across white-balance changes. Change
`prepare_preview_reconstruction` and its cancellable form to accept either the
recipe or a validated raw-highlight request. Keep downstream-only edits out of
that cache boundary; only Clip's method, threshold, and required WB selection
participate. Update every core, coordinator, and GPU-test caller in the same
change so the public API cannot temporarily compile with ambiguous defaults.

Off must be a true pass-through: no image traversal, no allocation, and zero
Clip statistics. The normalized mosaic must continue to retain negative and
over-range samples when Off is selected.

Add a separate `highlight_clipping` field to `StageTimings`; do not hide its
cost inside normalization or demosaic. Propagate the statistics through
`ReconstructedPreview`, `DemosaicedBase`, `RenderResult`, and
`ExportRenderResult` so CLI and desktop diagnostics can report them.

No extra image buffer is required, so `MemoryEstimate` should remain unchanged.
If implementation work proves that a materialized mask is needed in the hot
path, add and report its bytes explicitly instead of burying them in the peak.

## 7. Preview cache and GPU boundary

This is the integration point most likely to produce a subtle correctness bug.
The current reconstructed preview is camera-native and can be reused while
white balance changes. Clip changes the mosaic using limits derived from the
active WB gains, so a clipped reconstruction is valid only for that exact WB
selection.

Update `ReconstructedCameraRgbKey` with:

```text
highlight key =
  Off
  Clip { threshold bits, white-balance key }
```

Do not include WB in the Off key; preserve today's fast dynamic-WB path for
unclipped images. Bump `reconstruction_version` when the retained source
semantics change.

Teach `ReconstructedPreview` whether dynamic white balance is safe:

- Off: dynamic WB remains supported;
- Clip: retain the WB selection used for its channel limits and require an
  exact match.

Propagate that contract into `GpuPreviewUpload`/
`GpuPreviewSource::supports_dynamic_white_balance`. Clip itself remains a CPU
RAW-stage operation, but its camera-native result may still be uploaded for the
existing downstream GPU preview. Changing WB while Clip is active must rebuild
the clipped reconstruction and upload a new source; it must never reuse the old
clipped texture with new gains.

The desktop must keep the last valid frame visible during that rebuild, using
the existing asynchronous newest-wins handoff. No black frame or stale clipped
preview may be installed.

Add cache tests for all of these cases:

- Off + WB change reuses reconstructed camera RGB;
- Clip + identical WB and threshold reuses it;
- Clip + WB change invalidates it;
- Clip + threshold change invalidates it;
- Clip -> Off and Off -> Clip invalidate it;
- stale reconstruction/upload completion is discarded.

## 8. Correctness tests

### `rohditor-highlight` unit tests

Use small asymmetric images and non-tight strides. Cover:

- all four Bayer layouts;
- distinct R/G/B limits and both green sites;
- a sample below, equal to, and above each limit;
- negative and nominal over-white inputs;
- visible pixels changed while padding remains untouched;
- exact per-channel and total statistics;
- invalid limits rejected before mutation;
- non-finite input reporting the correct `(x, y)`;
- cancellation before work and during a multi-row operation;
- deterministic output/statistics under Rayon thread counts 1 and greater than
  1.

### Core integration tests

Add fixtures that prove behavior, not just successful execution:

1. **Off is unchanged:** current neutral pipeline output remains byte-identical
   and over-range normalized CFA values survive to demosaic.
2. **WB-aligned ceiling:** a constant saturated Bayer patch with gains such as
   `[2.0, 1.0, 1.5]` reaches equal R/G/B ceilings after demosaic and WB.
3. **Gain-scale invariance:** multiplying every WB gain by the same positive
   factor does not change which CFA samples are clipped.
4. **Partial clipping:** samples below their color limit remain bit-identical;
   only over-limit sites change.
5. **Pipeline placement:** a fixture that would differ if clipping ran after
   demosaic verifies normalize -> Clip -> demosaic ordering.
6. **Path agreement:** fit preview, Source 1:1, and export report the same
   affected-site definition for the same source/crop, accounting for their
   dimensions.
7. **Cancellation/error mapping:** highlight cancellation surfaces as
   `PipelineError::Cancelled`; invalid data keeps stage and coordinate context.

### Recipe, CLI, desktop, and GPU tests

- Recipe migration and validation tests from section 5.
- CLI parser tests for valid combinations, invalid method names, invalid
  thresholds, and threshold-with-Off rejection.
- Desktop panel test for visibility/enabling and discrete undo history.
- GPU upload/base-mismatch tests proving that clipped sources cannot change WB
  dynamically.
- Diagnostics tests for the new timing and statistics fields.

## 9. Benchmarks and corpus validation

Add a Criterion benchmark for the Clip fused pass using deterministic in-memory
normalized mosaics at:

- 6000x4000, tight stride;
- a smaller asymmetric image with padded stride;
- no affected samples, sparse affected samples, and a large clipped region.

Report clipping time separately from normalization and demosaic. The initial
acceptance target is that the in-place pass allocates no image-sized buffer and
adds no more than 5% to full preview-preparation time on the development
machine. If it misses that target, profile before adding unsafe code; workspace
policy continues to forbid unsafe Rust.

For the six private Sony ILCE-6400 samples:

1. run Off and confirm existing deterministic hashes remain unchanged;
2. record affected/changed/over-white counts for Clip at threshold 1.0;
3. inspect known saturated edges, specular highlights, clouds, and colored
   lights at Source 1:1;
4. verify that Clip removes false color where expected and document cases where
   its intentional loss of color/detail is worse;
5. compare fit preview and export crops from the same sensor coordinates;
6. record wall time and peak memory without claiming GPU-algorithm parity—the
   Clip algorithm in this slice is CPU-only.

Do not change the default from Off as part of this feature. Choosing a future
production default belongs to the later reconstruction comparison, where Clip
can serve as the baseline.

## 10. Implementation sequence

1. Add `MosaicImage::data_mut` and its padding/layout tests.
2. Create `rohditor-highlight` with validated channel levels, shared detection,
   cancellable in-place Clip, statistics, tests, and benchmark.
3. Add the recipe types, version-4 migration, validation, and serialization
   tests.
4. Add the core adapter and integrate it into full-resolution and fit-preview
   preparation before demosaic.
5. Add timing/statistics propagation and update memory accounting assertions.
6. Update preview cache keys and the dynamic-WB capability contract.
7. Add CLI flags and focused CLI tests.
8. Add desktop controls, undo/reset wiring, async preview behavior, and
   diagnostics.
9. Run focused unit/integration tests, then the repository check and private
   suites.
10. Perform the Source 1:1 visual review and record benchmark/corpus evidence in
    the implementation handoff, not in this plan.

## 11. Required verification before handoff

```bash
cargo test -p rohditor-highlight
cargo test -p rohditor-edit
cargo test -p rohditor-core
cargo test -p rohditor-gpu
cargo test -p rohditor-desktop
cargo test -p rohditor-cli
cargo bench -p rohditor-highlight --bench clip
./scripts/check.sh
cargo test --release --workspace --tests -- --ignored --nocapture
cargo test --release -p rohditor-gpu -- --ignored --nocapture
```

The ignored GPU suite validates the existing downstream GPU path and upload
contract. It does not turn Clip into a GPU algorithm. Any RX 9070 XT performance
claim must come from the host hardware run rather than a sandbox software
rasterizer.

## 12. Definition of done

The feature is complete when:

- Off preserves current output and cache behavior;
- Clip operates on normalized Bayer data before demosaic in preview, Source
  1:1, and export;
- active-WB-derived channel limits produce a common post-WB ceiling;
- the algorithm crate is independent of RAW metadata, recipes, UI, and core;
- the hot path is cancellable, deterministic, stride-safe, and in-place;
- recipe migration, undo/reset, CLI, desktop controls, cache invalidation,
  diagnostics, and stale-result rejection are tested;
- focused, workspace, ignored private, and host GPU regression suites pass;
- benchmark and private-corpus evidence are reported, including the expected
  quality limitations of this destructive baseline.

## References checked for this plan

- [darktable highlight reconstruction manual](https://docs.darktable.org/usermanual/4.2/en/module-reference/processing-modules/highlight-reconstruction/)
- [darktable `highlights.c` Clip implementation](https://github.com/darktable-org/darktable/blob/master/src/iop/highlights.c)
- [RawTherapee RAW image highlight/WB ordering](https://github.com/RawTherapee/RawTherapee/blob/dev/rtengine/rawimagesource.cc)
