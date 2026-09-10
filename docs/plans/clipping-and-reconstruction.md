# Highlight reconstruction, base rendering, and output gamut mapping

**Status:** active implementation roadmap

**Last audit:** 2026-09-09

This document is the current owner for three related but separate display
problems:

1. missing or unreliable sensor-channel information in RAW highlights;
2. scene-to-display base rendering; and
3. mapping complete RGB pixels into the chosen output gamut.

They must remain separate stages. A highlight method must not be used to hide
an output-gamut problem, and a gamut mapper must not alter the camera-native
source retained for preview reuse.

## 1. Current status

| Area | Status | Current behavior | Remaining work |
| --- | --- | --- | --- |
| Highlight `Off` | Implemented | RAW-stage pass-through | Keep as reference method |
| Highlight `Clip` | Implemented, current RAW default | WB-aware destructive channel ceiling | Keep as compatibility baseline |
| Highlight `LocalRatios` | Implemented | Conservative local camera-native reconstruction | Refresh corpus evidence |
| Highlight `Opposed` | Implemented, opt-in | Camera-native local inpainting with explicit fallback | Refresh corpus evidence; do not silently change default |
| Segmentation reconstruction | Not implemented | No region/component model exists | Add only for measured large-region failures |
| Guided Laplacian reconstruction | Research only | No fixed processing-domain contract | Research domain, cost, and fallback behavior |
| LCh reconstruction | Deferred | No implementation | Reconsider only if the spatial methods leave a measured gap |
| Rohditor Standard | Implemented, current recipe default | Versioned luminance base LUT, CPU/GPU/CLI/desktop integration | Qualify appearance and interaction with output gamut |
| Rohditor Neutral | Implemented | Identity base rendering for migrated/reference recipes | Retain as an explicit comparison mode |
| Hard sRGB clipping | Implemented baseline | Rec.2020 to linear sRGB, per-channel clamp, sRGB transfer | Preserve as a reference/output fallback |
| Chroma compression | Implemented, opt-in | Versioned OKLab/OKLCH mapper across CPU/GPU/CLI/desktop/export | Complete private-corpus visual review and hardware-GPU parity |
| Wide-gamut/monitor ICC output | Not implemented | Export embeds sRGB ICC | Separate future color-management scope |

The current recipe schema in this checkout is 10. Standard rendering is
already present in `crates/core/src/rendering.rs` and
`crates/edit/src/rendering.rs`; no separate Standard plan is present in this
checkout, so this document records its remaining qualification gates directly.

## 2. Pipeline boundary and ordering

The current CPU reference path is:

```text
immutable RawFrame
  -> crop and black/white normalization
  -> RAW highlight method (rohditor-highlight)
  -> Bayer demosaic
  -> lens correction, when enabled
  -> preview resampling, for reduced previews
  -> white balance and camera -> linear Rec.2020/D65
  -> user Exposure
  -> base rendering (Standard or Neutral)
  -> remaining Light and Color edits
  -> linear Rec.2020/D65 -> linear sRGB
  -> output gamut policy (Clip default, opt-in ChromaCompress v1)
  -> sRGB transfer function and quantization
```

The order is a contract, not an implementation suggestion:

- highlight reconstruction sees normalized CFA data before demosaic;
- Standard operates on scene-linear Rec.2020 luminance before creative Light
  and Color controls;
- gamut mapping sees complete, adjusted linear RGB pixels;
- transfer encoding and dithering happen after gamut mapping; and
- GPU preview starts from the retained camera-native/demosaiced source and
  must produce the same downstream result as the CPU reference.

The output mapper is target-output behavior. It is not a camera profile, a
white-balance operation, a highlight method, a scene-light edit, or a monitor
profile. Working-gamut compression during camera calibration is explicitly
out of scope for the first slice.

## 3. Landed RAW highlight behavior

The implemented foundation lives in `crates/highlight` and is already wired
through recipe validation, schema migration, CPU dispatch, preview cache
identity, CLI controls, desktop controls, diagnostics, and retained GPU-source
provenance.

The available methods are:

- `Off`: no traversal or image-sized allocation; normalized values are kept;
- `Clip`: computes WB-dependent common ceilings and caps CFA samples in place;
- `LocalRatios`: reconstructs small partially clipped regions from logical
  Bayer-cell ratios; and
- `Opposed`: reconstructs camera-native cells with opposing-channel evidence,
  explicit fallbacks, and method-specific statistics.

The following contracts are already implemented and should not be re-planned:

- RAW data is immutable; normalized mosaics are the mutable stage product.
- Every method supports the four Bayer phases, visible row strides, odd edges,
  checked allocation, cancellation, finite-output validation, and deterministic
  statistics.
- Cache identity includes method-specific numeric inputs and algorithm
  versions. Clip is WB-sensitive; LocalRatios and Opposed are reusable across
  dynamic downstream white balance.
- Uploading a reconstructed source to `rohditor-gpu` does not claim that RAW
  reconstruction itself is GPU accelerated.

### Remaining highlight evidence

The implementation is complete, but quality/default evidence is not:

- compare Off, Clip, LocalRatios, and Opposed on identical Source 1:1 crops and
  full-resolution exports from the private Sony A6400 corpus;
- include neutral specular highlights, colored lights, clouds, saturated object
  edges, thin structure, large clipped regions, border-touching regions, and
  fully unsupported areas;
- record reconstruction/fallback/unsupported counts, scratch bytes, wall time,
  and visible regressions; and
- keep Clip as the current default until a measured alternative is clearly
  better for the intended first-version scope.

## 4. Remaining RAW reconstruction work

Do not add enum variants, recipe fields, UI controls, or empty cache branches
for these methods before an isolated algorithm and a complete vertical slice
are ready.

### 4.1 Segmentation-based reconstruction

**Entry condition:** the Opposed corpus review contains reproducible failures
where region identity or boundary evidence, rather than a local opposing
estimate, is the missing information.

Implementation slice:

1. Define the mask domain and connectivity (Bayer photosites versus logical
   cells), border policy, and deterministic connected-component labeling.
2. Extract each component boundary and filter candidate evidence by valid
   channels, brightness, gradients, and edge consistency.
3. Produce a region estimate and confidence. Confidence must change a named
   accept/fallback decision, not only a diagnostic number.
4. Fall back explicitly to Opposed or leave the region unchanged when evidence
   is insufficient; count regions and sites separately.
5. Use checked compact labels, deterministic reductions, cancellation between
   passes, and an explicit peak-memory estimate.
6. Wire recipe/schema, cache identity, CLI, desktop, diagnostics, and retained
   GPU-source behavior as one vertical slice.

Required fixtures include diagonal-separated objects, narrow and nested
regions, border-touching components, a large uniform patch, an adjacent object
with a different color, insufficient boundary evidence, and many tiny
components. Benchmarks must include both component-count and large-region
worst cases.

### 4.2 Guided Laplacian reconstruction

This remains research-only. First determine whether the method belongs on
normalized CFA data, demosaiced camera RGB, or another typed linear image. The
processing domain determines the crate boundary; it must not be forced into
the highlight crate merely for roadmap symmetry.

Before integration, write mathematical pseudocode for the pyramid, masks,
chromaticity representation, boundary conditions, scale schedule, convergence,
fallback, and output bounds. Prove a scalar single-scale prototype on tiny
fixtures, then establish peak memory, cancellation, deterministic execution,
and CPU quality against Opposed/Segmentation. GPU work is later and requires
CPU parity fixtures first.

## 5. Base rendering status and gates

Rohditor Standard is now implemented as a versioned base-rendering profile:

- `RenderingProfileSelection::{RohditorNeutral,RohditorStandard}` is part of
  the edit recipe;
- Standard process version 1 uses a shared sampled luminance LUT with an
  explicit middle-gray anchor and finite/negative-input policy;
- CPU and GPU use the same LUT contract and cache identity includes the profile
  and process version; and
- CLI, desktop controls, diagnostics, and recipe migration are wired.

This does not mean Standard is fully qualified. Before calling it a release
default, compare Standard and Neutral on the private corpus and synthetic
scene-linear fixtures, especially saturated over-range colors. Verify that
Exposure, the existing Light tone LUT, tone curve, HSL/grading, and output
gamut mapping remain in the documented order and do not double-compress
luminance.

Standard and gamut mapping must retain independent identities. A rendering
profile change invalidates adjusted pixels; a gamut-policy change invalidates
only the output-adjusted level when the base is unchanged.

## 6. Output gamut mapping: first implementation slice

**Implementation status:** landed as opt-in Chroma Compress v1. Hard clipping
remains the default until private-corpus visual qualification and real-hardware
GPU parity are complete.

### 6.1 Current baseline

The output path converts linear Rec.2020/D65 to linear sRGB, applies the
selected gamut policy, and then applies the sRGB transfer function.
`OutputPolicy::ClipToSrgb` remains the explicit compatibility policy and the
default. It is deterministic and easy to validate, but independent channel
clipping can shift hue, remove chroma, and produce the familiar
saturated-highlight color cast.

The CPU and GPU paths now also implement `ChromaCompressToSrgb`. The policy and
its algorithm version are explicit GPU parameters and adjusted-cache inputs;
they are never inferred from a UI or shader default.

### 6.2 Recommended v1 contract

Implement one policy beside the baseline, named consistently with the existing
API, for example `OutputPolicy::ChromaCompressToSrgb`.

The contract is:

- input: finite or non-finite linear Rec.2020/D65 pixels after all recipe
  edits;
- target: linear sRGB/D65, followed by the existing IEC sRGB transfer;
- in-gamut identity: if converted linear sRGB is finite and every channel is
  in `[0, 1]`, return it bit-for-bit before any perceptual conversion;
- out-of-gamut mapping: convert linear sRGB to OKLab/OKLCH using signed cube
  roots, preserve hue and (when feasible) OKLCH lightness, and binary-search
  the largest chroma whose inverse conversion lies inside the target gamut;
- lightness edge cases: clamp OKLCH lightness to the target range only when
  necessary to obtain a valid output, and count this as a limited/fallback
  case rather than pretending it was pure chroma compression;
- neutral edge cases: near-zero chroma keeps a stable neutral axis and uses
  target-range clamping without inventing a hue;
- numerical policy: use a fixed iteration count and epsilon, clamp only the
  final rounding residue, and never emit NaN or infinity; and
- invalid-input fallback: use the existing hard-clip policy for non-finite or
  otherwise unrepresentable pixels, with a diagnostic count.

This is deliberately a bounded first mapper. It does not add multiple filmic
variants, local gamut mapping, automatic intent selection, wide-gamut output,
or an additional working-gamut compression step.

The early in-gamut return is important: converting every pixel through OKLCH
would make a supposedly neutral policy alter ordinary photographs. The binary
search is also preferable to independently clamping channels because it gives
one monotonic chroma control and a stable hue path for saturated colors.

### 6.3 Ownership and data flow

Implement the color math in the shared `rohditor-color` boundary, initially as
a `color::gamut` module if the crate extraction has not landed. The ownership
should be:

```text
rohditor-edit       user recipe and creative intent
rohditor-core       OutputPolicy, stage ordering, CPU reference, diagnostics
rohditor-color      matrices, OKLab/OKLCH, gamut algorithm/version
rohditor-gpu        shader implementation and uniform/texture contract
apps/cli            output-gamut argument and report
apps/desktop        preference/control, cache key, diagnostics text
```

The first slice should keep output policy in `RenderOptions`, as it is today,
rather than adding a recipe field and another schema migration. If users later
need the policy to travel with a document, promote it deliberately in a
separate recipe change. Export and preview must receive the same effective
policy; CLI hard-coded `ClipToSrgb` and desktop-only defaults must disappear.

Add a version constant for the mapper (for example
`CHROMA_COMPRESS_ALGORITHM_VERSION`). Include it in the adjusted cache key and
diagnostics. Changing matrices, OKLab constants, search iterations, epsilon,
or invalid-input fallback is a pixel-producing process change.

### 6.4 CPU reference implementation

Implement in this order:

1. Add a small typed result/statistics structure: mapped pixel, whether it was
   in-gamut, compressed, limited, clipped-fallback, or invalid.
2. Add matrix conversion helpers and OKLab/OKLCH round trips with signed
   `cbrt`, finite checks, and exact small asymmetric tests.
3. Implement `clip` as the unchanged reference and `chroma-compress` with the
   contract above. Keep the mapper independent of image traversal.
4. Make `render_display_srgb8`, dithered 8-bit output, and 16-bit output call
   one policy-dispatching per-pixel function. Transfer encoding and quantization
   must remain after mapping.
5. Thread policy and diagnostics through preview, source-scale inspection, and
   export. Preserve transactional file output and existing sRGB ICC metadata.

Do not duplicate the mapping algorithm in preview, export, and tests. The CPU
per-pixel function is the reference used to define GPU tolerances.

### 6.5 CPU correctness tests

Required unit and integration coverage:

- exact identity for neutral and in-gamut RGB values;
- black, white, gray ramps, and near-neutral values;
- red/green/blue primaries, secondaries, hue sweeps, and saturated gradients;
- values below zero, above one, and mixed-sign channels;
- boundary continuity just inside and outside the sRGB hull;
- monotonic chroma reduction and stable hue/lightness within stated error;
- fixed behavior for non-finite input and no non-finite output;
- 8-bit and 16-bit quantization, dithering, orientation, and crop geometry;
- Standard-before-gamut versus Neutral-before-gamut ordering on HDR fixtures;
- unchanged `ClipToSrgb` output fixtures; and
- deterministic results across Rayon thread counts.

Generated sweeps should report maximum hue, lightness, chroma, and round-trip
errors. Screenshots are useful for review but cannot replace numerical gates.

### 6.6 Full vertical integration

After the CPU mapper is stable:

1. Add the output-policy CLI option and validation. The report must state the
   selected policy and algorithm version.
2. Add a desktop setting/control and diagnostics text. Changing it must reuse
   the prepared/demosaiced base and invalidate only adjusted display pixels.
3. Include policy/version in `AdjustedPreviewKey` tests, including Standard and
   Neutral combinations.
4. Ensure source-scale preview and full-resolution export use the same policy
   and mapper, not a display-only shortcut.
5. Keep sRGB ICC embedding accurate; do not imply that an sRGB export is a
   wide-gamut or monitor-managed output.

### 6.7 GPU parity

Only after the CPU reference and vertical seams are complete:

- add an explicit output-policy code and mapper version to the GPU render
  parameters;
- update WGSL and the Rust packed uniform layout together;
- keep the intermediate `working_linear` texture in adjusted Rec.2020 before
  target gamut mapping;
- implement the same OKLab/OKLCH constants, signed-root behavior, fixed search
  iterations, epsilon, and fallback policy in WGSL;
- add structural shader/layout tests and CPU/GPU parity on synthetic saturated
  fixtures; and
- run the ignored hardware suite on the RX 9070 XT when available. A software
  Vulkan adapter only proves shader compilation/dispatch structure.

If the GPU implementation cannot meet the CPU tolerance, the desktop must
fall back to the CPU preview rather than silently displaying a different
gamut policy.

### 6.8 Performance and visual qualification

Benchmark the mapper separately from matrix conversion, transfer encoding,
quantization, RAW decode, and UI rendering. Include in-gamut no-op, sparse
out-of-gamut, saturated gradients, and full-frame worst cases. Record scratch
bytes and peak working-set impact; the first mapper should not allocate an
image-sized auxiliary buffer.

Qualify on the private Sony A6400 corpus plus synthetic HDR/color charts. Look
for hue shifts, gray neutrality, banding, clipped skies, saturated foliage,
skin tones, and interaction with Standard. Record both visible wins and cases
where hard clipping is preferable. Chroma compression should not become the
unqualified default solely because it looks better on one image.

## 7. Shared acceptance gates

Every future highlight method and the gamut mapper must satisfy:

### Architecture

- CPU remains the deterministic correctness reference.
- RAW data stays immutable and typed image states remain explicit.
- Recipe, cache, CLI, desktop, preview, source-scale, and export meanings are
  identical; no UI-only implementation is accepted.
- Algorithm versions and all pixel-producing numeric inputs are explicit.
- GPU provenance and fallback behavior are honest and observable.

### Correctness

- Asymmetric fixtures cover odd dimensions, non-tight stride, boundaries,
  invalid values, cancellation, and deterministic parallel execution.
- Valid input preservation and unsupported/fallback behavior are measured,
  not inferred from a single golden image.
- CPU/GPU tolerances are stated in linear values and encoded sRGB codes.
- Existing Clip/Neutral compatibility fixtures remain green.

### Evidence

- Benchmarks separate kernel cost from decode, demosaic, optics, resampling,
  UI, and file I/O.
- Private-corpus Source 1:1 and export comparisons record regressions as well
  as improvements.
- Ignored GPU tests identify the actual adapter and do not call a CPU rasterizer
  hardware parity.

## 8. Recommended execution order

1. Keep the landed highlight methods and Standard profile stable while
   refreshing their corpus evidence.
2. Split the shared color boundary as described in the maintainability plan,
   or create the initial `color::gamut` module if extraction is not yet ready.
3. Implement CPU `ChromaCompressToSrgb`, its tests, output-policy dispatch,
   cache identity, CLI, desktop, source-scale, and export seams.
4. Implement and qualify GPU parity; retain CPU fallback on mismatch.
5. Decide whether the combined Standard-before-gamut result is suitable for a
   release default. Keep Neutral and hard clipping available for comparison.
6. Only then revisit Segmentation or Guided Laplacian if highlight evidence
   shows that they solve a real first-version problem.
7. Treat monitor ICC/wide-gamut display, output resize/sharpening, and local
   masks as separate plans rather than expanding this gamut slice.

## 9. Verification commands

For documentation-only changes, the normal workspace gate is enough. For the
implementation, run focused tests while iterating and the full evidence suites
before changing defaults:

```bash
cargo test -p rohditor-highlight
cargo test -p rohditor-core
cargo test -p rohditor-edit
cargo test -p rohditor-gpu
cargo test -p rohditor-cli
./scripts/check.sh
cargo test --release --workspace --tests -- --ignored --nocapture
cargo test --release -p rohditor-gpu -- --ignored --nocapture
```

The last two commands require the private corpus and a usable hardware Vulkan
adapter. Record their availability and actual adapter identity in the change
that alters image-processing or shader behavior.

## 10. Reference and licensing notes

Highlight research may continue to consult darktable and RawTherapee's
highlight reconstruction implementations. The gamut mapper should be an
original, typed implementation with explicit colorimetric tests; do not copy a
reference application's pipeline assumptions without recording the target
space, transfer function, and license implications.

Rohditor is GPL-3.0-or-later. Any adapted GPL-compatible source must retain
the applicable attribution, copyright, and license notices.
