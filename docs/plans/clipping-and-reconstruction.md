# Clipping, highlight reconstruction, gamut mapping, and tone mapping

**Status:** active consolidated plan

**Last implementation audit:** 2026-09-07

**Canonical scope:** RAW highlight handling, later RGB gamut mapping, and later
tone mapping

This document replaces the original architectural proposal and the separate
Clip and Local ratios implementation plans. It records the contracts that have
already landed and defines the remaining work without treating future ideas as
implemented features.

Implementation status below describes the current source tree. It does not
claim that old benchmark runs, private-corpus reviews, or host-GPU runs are
current. Those evidence gates remain explicit wherever they affect a quality or
default-method decision.

## 1. Current status and roadmap

| Area | Status | Role | Next decision |
| --- | --- | --- | --- |
| Off | **Implemented** | Preserve normalized CFA data unchanged | Remains the default |
| Shared highlight crate and detection primitives | **Implemented** | Independent, deterministic CPU processing on normalized Bayer mosaics | Extend only when a new algorithm needs a real shared primitive |
| Clip | **Implemented** | Destructive neutralizing baseline | Keep as a predictable fallback and comparison method |
| Local ratios v1 | **Implemented** | Conservative reconstruction of small, partially clipped regions | Validate quality against Opposed before considering a default change |
| Opposed / local inpainting | **Implemented, opt-in** | Camera-native local opposing-channel recovery with explicit fallbacks | Refresh benchmark/corpus evidence; do not change the default yet |
| Segmentation reconstruction | **Planned later** | Region-aware recovery for larger clipped areas | Start only after Opposed failure cases are measured |
| Guided Laplacian reconstruction | **Research planned** | Multiscale, structure-guided high-quality mode | Confirm its processing domain and cost before fixing an API |
| LCh reconstruction | **Deferred** | Historical alternative | Add only if comparisons reveal a gap the selected methods do not cover |
| Hard output-gamut clipping | **Implemented baseline** | Clamp linear sRGB to the display/output range | Preserve for compatibility and comparison |
| Chroma-compressing gamut mapping | **Planned separately** | Map out-of-gamut RGB without independent channel clipping | Implement after its colorimetric contract is fixed |
| Dedicated scene-to-display tone mapping | **Planned separately** | Compress scene-referred dynamic range | Reconcile with existing Light controls before adding a new stage |

The intended quality progression is:

```text
RAW highlight handling
    Off
      -> Clip                         implemented baseline
      -> Local ratios v1              implemented reconstruction
      -> Opposed / local inpainting   implemented, opt-in
      -> Segmentation                 later
      -> Guided Laplacian             later, after a research gate

RGB output handling
    current hard gamut clip
      -> chroma-compressing gamut map
      -> dedicated tone mapper when HDR/scene-to-display needs justify it
```

LCh is deliberately not on the main implementation path. It is not rejected
forever, but building it now would add another perceptual-space method before
Rohditor has evaluated the more promising spatial approaches.

## 2. Boundaries and non-negotiable contracts

Highlight reconstruction, gamut mapping, and tone mapping solve different
problems and must not share a catch-all crate or ambiguous setting.

```text
decode immutable RawFrame
  -> select sensor crop and normalize CFA samples
  -> RAW highlight operation                 crates/highlight
  -> demosaic
  -> preview resample where applicable
  -> white balance and camera -> Rec.2020
  -> scene-light and creative edits
  -> scene-to-display tone mapping           future crates/tonemap
  -> Rec.2020 -> output gamut mapping         current core, future crates/gamut
  -> output transfer function and quantize
```

The ordering between a future tone mapper and gamut mapper must be verified
with saturated-color fixtures before either new mode ships. The diagram states
the starting contract, not permission to couple the two implementations.

### 2.1 RAW highlight boundary

- `raw` decodes metadata and immutable sensor data.
- `rohditor-highlight` consumes `rohditor_image::MosaicImage<f32>` after black
  subtraction and white normalization and before demosaic.
- The highlight crate must not depend on `raw`, `core`, `edit`, `gpu`, or an
  application crate.
- `core` resolves metadata, white balance, recipe intent, stage ordering,
  cancellation, diagnostics, and errors.
- `edit` owns versioned, serializable user intent.
- Preview, Source 1:1, and export use the same deterministic CPU highlight
  implementation. GPU work begins downstream from the retained reconstructed
  source.
- Algorithms consume and update the normalized mosaic rather than mutating
  `RawFrame` or inventing a duplicate `RawHighlightInput` type.
- Visible samples are validated; stride padding is never classified, counted,
  or changed.
- Checked dimension and allocation arithmetic, cancellation, deterministic
  output, and a CPU correctness reference are required for every method.

Do not add a public reconstruction trait until runtime polymorphism or an
external implementation creates a concrete need. The current closed enum and
static dispatch are simpler and make recipe coverage exhaustive.

### 2.2 Detection is not one universal threshold

Detection is separated from treatment, but the meaning of a threshold remains
method-specific:

- **Clip level:** a WB-dependent treatment ceiling selected to make the
  post-WB channel ceilings equal.
- **Detection level:** a camera-native boundary above which a sample is
  *suspected clipped* for reconstruction.

A normalized value at or above `1.0` is not proof of physical sensor
saturation. Rohditor intentionally retains over-range normalized values, and
diagnostics must not describe every such value as destroyed information.

The common comparison at a supplied level is:

```text
suspected_or_affected := sample >= level
numerically_changed   := sample > level       # Clip only
```

`detect_clipping`, `detect_local_ratios`, and `detect_opposed` materialize masks
for diagnostics and tests. Hot paths may use the same scalar classification
without allocating a full-frame mask.

### 2.3 CPU/GPU and retained-preview boundary

RAW highlight methods are CPU-only until a GPU implementation has exact CPU
parity coverage. Uploading their camera-native result to the existing GPU
preview does not make the reconstruction algorithm GPU-accelerated.

Cache identity must include every input that can change reconstructed camera
RGB:

```text
Off
Clip { clip threshold bits, white-balance key }
LocalRatios { detection threshold bits, algorithm version }
Opposed { detection threshold bits, algorithm version }
future method { all numerical options, algorithm version, WB if required }
```

Off, Local ratios, and Opposed support dynamic downstream white balance. Clip
does not, because its pre-WB limits depend on the selected WB gains. Each
future method must prove which category it belongs to rather than inheriting
one silently.
The desktop retains the last valid frame while a CPU rebuild is pending and
discards stale document/revision results.

## 3. Implemented RAW highlight foundation

### 3.1 Crate and public API

`crates/highlight` is implemented with this focused layout:

```text
crates/highlight/
  src/
    lib.rs           public types, errors, cancellation, algorithm version
    detect.rs        Clip, Local-ratio, and Opposed classifiers and masks
    clip.rs          destructive Clip pass
    cells.rs         compact logical Bayer-cell summaries
    local_ratios.rs  Local ratios v1
    opposed.rs       Opposed / local inpainting v1
  tests/
    clip.rs
    local_ratios.rs
    opposed.rs
  benches/
    clip.rs
    local_ratios.rs
    opposed.rs
```

The crate depends only on `rohditor-image`, Rayon, and `thiserror` in normal
builds. Its implemented public concepts include:

- `ChannelClipLevels`, `ClipOutput`, and `ClipStats`;
- `ChannelDetectionLevels`, `LocalRatioOptions`, `LocalRatioOutput`, and
  `ReconstructionStats`;
- `OpposedOptions`, `OpposedOutput`, and `OpposedStats`;
- cancellable and non-cancellable Clip, Local-ratio, and Opposed entry points;
- materialized `ClippingMask` entry points for all three classifications; and
- `LOCAL_RATIOS_ALGORITHM_VERSION` and `OPPOSED_ALGORITHM_VERSION` for
  preview-cache identity.

`ReconstructionStats` currently maintains these invariants:

```text
suspected_clipped_sites = reconstructed_sites + fallback_sites
changed_sites <= reconstructed_sites
fully_unsupported_sites <= fallback_sites
sum(suspected_by_channel) = suspected_clipped_sites
```

### 3.2 Off — implemented

Off is a true pass-through at the highlight stage:

- it performs no image traversal or image-sized allocation;
- negative and over-range normalized samples are retained;
- diagnostics report `HighlightDiagnostics::Off`; and
- reconstructed camera RGB remains reusable across WB changes.

Off remains the recipe default. Neither the presence of more advanced methods
nor a successful synthetic test is enough to change that default.

### 3.3 Clip — implemented baseline

Clip is destructive treatment, not reconstruction. It caps normalized CFA
sites before demosaic and invents no spatial detail.

For validated active WB gains `g` and user threshold `t`:

```text
common_ceiling = t * min(g_r, g_g, g_b)
limit_c        = common_ceiling / g_c
output_c       = min(input_c, limit_c)
```

This guarantees `output_c * g_c <= common_ceiling` for every color. Multiplying
all WB gains by the same positive scale does not alter which sites Clip affects.

Implemented behavior:

- default threshold `1.0`, validated in `0.5 ..= 1.5`;
- negative and below-limit values remain unchanged;
- values above a per-color limit are capped in place;
- `affected`, `changed`, nominal-over-white, and per-channel counts are kept
  distinct;
- all Bayer layouts and non-tight row strides are supported;
- the preview cache includes the active WB selection; and
- CLI, desktop control, undo/reset, diagnostics, preview, Source 1:1, and export
  paths are wired.

Clip remains useful even after better reconstruction lands: it is robust,
cheap, honest about discarding color, and often appropriate for naturally
neutral highlights such as clouds or specular light sources.

### 3.4 Local ratios v1 — implemented reconstruction

Local ratios uses a separate camera-native detection threshold. `core` expands
the current scalar setting to equal red, green, and blue detection levels; it
does not derive those levels from white balance.

The implemented v1 estimator:

1. validates visible samples and counts suspected sites;
2. returns without allocating summaries when no site is suspected;
3. partitions the mosaic into logical 2x2 Bayer cells using the image's actual
   shifted Bayer pattern;
4. stores compact immutable cell RGB means and flags, including partial cells
   at odd image edges;
5. searches candidate cells at Chebyshev radii one and two;
6. estimates the missing target/support ratio using medians;
7. uses a `0.5 EV` cross-channel consistency guard when two support channels
   exist;
8. requires two independent estimates to agree within `1 EV`;
9. caps an accepted estimate at four times the detection level; and
10. writes `max(original, estimate)`, so reconstruction never darkens a
    suspected clipped site.

The candidate minimum is three with two supporting channels and five with one.
Dark or non-finite ratio operands are rejected. Unsupported sites remain
unchanged; Local ratios does not silently fall back to Clip.

This method is deliberately conservative and local. It is designed for
isolated sites and small clipped areas with trustworthy nearby color. It cannot
recover a large fully clipped region and should not claim otherwise.

Local-ratio integration includes its method-specific recipe group, diagnostics,
cache identity, dynamic-WB reuse, CPU/GPU handoff, and dedicated Criterion
target. Opposed's separate integration slice is recorded in section 3.5.

### 3.5 Opposed / local inpainting — implemented slice

Opposed is an original camera-native Bayer-domain method informed by the
opposed/inpainting family in darktable and RawTherapee. It is not a line-by-line
port of either demosaiced-RGB implementation.

Its frozen contract is:

1. The input is normalized `MosaicImage<f32>` before white balance and
   demosaic. Detection uses one user-controlled camera-native threshold,
   expanded to equal R/G/B levels by `core`; white balance is not part of the
   math and the result is reusable across dynamic-WB changes.
2. The method builds immutable logical 2x2 Bayer-cell summaries using the
   actual shifted CFA phase. Suspected samples (`sample >= level`) are excluded
   from means; finite positive, non-suspected samples are valid evidence.
3. For a suspected target with at least one clean opposing channel, the
   opposing reference is the cube of the mean cube-root of the surviving
   opposing channels. A median chrominance offset is then gathered from clean
   target-channel cells in Chebyshev rings one through three. Candidates must
   be above `0.2 × level`; when two opposing channels survive, their ratio must
   agree within `0.75 EV` with the target cell.
4. At least three candidates are required. The result is finite, capped at
   `4 × level`, and written as `max(original, estimate)`, so valid sites and
   suspected sites are never darkened. The fixed radii, tolerance, candidate
   count, and bound are not user controls.
5. A partially supported site falls back unchanged when it has insufficient
   clean evidence. A fully unsupported site is counted separately. Opposed
   never falls back implicitly to Local ratios or Clip, and all suspected,
   reconstructed, changed, fallback, and unsupported counts satisfy the same
   exact accounting invariants as the Local-ratio path.

The vertical slice includes `opposed.rs`, method-specific options/output/stats,
materialized detection, checked scratch estimates, schema-6 migration, CPU
dispatch, cache identity with `OPPOSED_ALGORITHM_VERSION`, CLI and desktop
controls/diagnostics, dynamic-WB reuse, and downstream GPU source matching.
Opposed remains CPU-only; GPU receives its retained camera-native result.

### 3.6 Remaining validation for the implemented methods

The implementation is landed, but these are ongoing evidence requirements,
especially before changing the default or claiming broad quality superiority:

- record fresh benchmark numbers for no-clipping, sparse, edge-adjacent, and
  large unsupported cases at representative dimensions;
- an initial release Criterion run on 2026-09-07 measured Opposed at 8.23 ms
  with no clipping, 46.92 ms with sparse clipping, and 61.07 ms with a fully
  unsupported 6000x4000 fixture; the 37x23 padded fixtures measured 42.85 µs
  and 44.38 µs respectively. These are host-specific kernel baselines, not
  image-quality evidence;
- compare Off, Clip, Local ratios, and Opposed on identical Source 1:1 sensor
  crops and full-resolution exports from the private camera corpus;
- record suspected/reconstructed/changed/fallback counts and scratch bytes;
- include colored lights, saturated object boundaries, specular highlights,
  clouds, and large fully clipped regions;
- record every crop where Clip is visibly preferable and use failures as the
  input set for Opposed; and
- run downstream GPU regression tests on the RX 9070 XT without describing
  them as GPU reconstruction benchmarks.

## 4. Remaining RAW reconstruction plan

Every new method is its own vertical slice. Do not add empty modules, enum
variants, recipe fields, or UI controls for later phases before their algorithm
is implemented and tested.

### 4.1 Phase 3 — Opposed / local inpainting — completed implementation

**Status:** the CPU vertical slice is implemented and remains opt-in. Corpus
quality review and benchmark evidence are still open.

**Goal:** improve chrominance continuity and boundary behavior without taking
on segmentation or a multiscale solver.

The implementation uses a Rohditor-specific Bayer-domain contract rather than
claiming source-level parity with either reference application. The exact
contract is recorded in section 3.5; the remaining work in this section is
evidence gathering and failure-case review.

#### Step A: freeze the algorithm contract

The source study and implementation decisions were:

1. Trace both reference implementations from input buffer through masks,
   dilation/neighborhood construction, opposing-channel or chrominance
   estimates, transition handling, and fallbacks.
2. Record the exact processing domain, normalization assumptions, CFA handling,
   constants, border policy, numerical bounds, and cancellation points.
3. Identify which behavior is algorithmic and which is coupled to the source
   application's pipeline.
4. Create an original method informed by both references. No reference source
   was copied into Rohditor, so there is no derived-source header to preserve;
   the source map remains in section 10.
5. Define whether the result is WB-independent. Cache behavior is a conclusion
   of the math, not an API preference.
6. Define explicit behavior for partially supported and fully unsupported
   sites. Any fallback to Local ratios or Clip must be named in the method
   contract and counted; it must not occur implicitly.

The contract should expose only controls that materially affect user intent.
Start with a detection threshold if one is required. Do not expose kernel
radii, iteration counts, mask expansion, or tolerance constants merely because
the implementation contains them.

#### Step B: implemented isolated algorithm

- Add `opposed.rs`; add `mask.rs` or neighborhood helpers only when sharing is
  real rather than speculative.
- Reuse `MosaicImage<f32>`, typed detection levels, error handling, and
  cancellation. Reuse `cells.rs` only if its logical-cell representation fits
  the frozen contract.
- Consume the input mosaic and avoid an output-sized clone. Account for every
  mask, temporary plane, and pyramid-like allocation with checked arithmetic.
- Add method-specific options, output, and statistics rather than stretching
  Local-ratio counters into ambiguous meanings.
- Keep scalar CPU processing as the reference. Parallelize only independent
  passes with deterministic reductions.

#### Step C: implemented crate correctness coverage

Synthetic asymmetric tests must include:

- all four Bayer phases and both green locations;
- one-channel and multi-channel clipping;
- a flat colored surface crossing the clipping boundary;
- a red/green object edge that would expose color bleeding;
- thin structure crossing a clipped patch;
- a colored light, a neutral specular highlight, and a fully clipped patch;
- mask boundaries, odd dimensions, padding, invalid floats, cancellation, and
  deterministic results across Rayon thread counts;
- exact fallback/statistics invariants; and
- explicit comparison fixtures where Local ratios fails and Opposed improves
  the defined metric without harming valid samples.

Quality assertions should use measurable properties—unchanged valid sites,
hue/chroma continuity, edge leakage, bounded output, and finite results—not
large brittle golden images alone.

#### Step D: implemented vertical slice

- Extend `HighlightMethod`, method-specific recipe settings, validation, schema
  migration, CLI parsing, desktop controls, undo/reset, and diagnostics.
- Add an exhaustive core dispatch branch immediately after normalization.
- Add the method and algorithm version to preview cache identity.
- Prove dynamic-WB compatibility or require exact-WB rebuilding.
- Update timing and scratch-memory estimates before enabling the method on
  full-resolution inputs.
- Preserve newest-wins CPU preview handoff and downstream GPU source checks.

#### Step E: benchmark and evaluate — open

Benchmark validation/mask construction, reconstruction, total highlight-stage
time, scratch bytes, and full preview preparation on no-clipping, sparse,
edge-adjacent, and larger clipped regions. Then compare Off, Clip, Local ratios,
and Opposed on fixed private-corpus crops.

The implementation is complete; the open benchmark/corpus gate determines
whether its quality wins justify broader use. It does not automatically become
the default when it lands.

### 4.2 Phase 4 — Segmentation-based reconstruction

**Prerequisite:** Opposed is implemented and its remaining large-region or
cross-edge failures are captured as reproducible fixtures.

**Goal:** reason about contiguous clipped regions and their boundaries instead
of allowing every target site to borrow unrelated nearby color.

Implementation plan:

1. Define a deterministic region mask and connected-component labeling pass
   over suspected clipped sites. Specify Bayer-cell versus photosite
   connectivity and border behavior before coding.
2. Extract each region's boundary and filter candidates using valid-channel
   support, brightness floors, gradients, and edge consistency.
3. Produce a region estimate and confidence from boundary evidence. Confidence
   must affect a named acceptance/fallback rule, not merely diagnostics.
4. Fall back explicitly to Opposed or leave the region unchanged when evidence
   is insufficient. Count regions and sites by reconstructed, rejected, and
   fallback outcomes.
5. Keep stable component IDs and deterministic merge/reduction order so thread
   count cannot alter results.
6. Use compact labels and region metadata, checked allocation, cancellation
   between passes, and explicit peak-memory accounting.
7. Integrate recipe, cache, CLI, desktop, diagnostics, and CPU/GPU boundaries
   as a complete vertical slice.

Required fixtures include two clipped objects separated diagonally, a clipped
object touching an image border, nested or narrow regions, a large uniform
patch, a boundary adjacent to a differently colored object, insufficient
boundary evidence, and many tiny components. Benchmarks must include worst-case
component counts as well as one large component.

Do not begin this phase merely to match another editor's method list. Begin it
when the Opposed corpus review demonstrates that region identity is the missing
information.

### 4.3 Phase 5 — Guided Laplacian reconstruction

**Prerequisite:** a research spike must first determine whether the chosen
method belongs on normalized CFA data, demosaiced camera RGB, or another typed
linear representation. The crate boundary follows that answer; it must not be
forced into `rohditor-highlight` for roadmap symmetry.

**Goal:** recover multiscale chromatic structure in small-to-medium clipped
regions using valid intensity/detail as guidance.

Research and implementation plan:

1. Translate the reference algorithm into mathematical pseudocode: pyramid
   construction, norm/chromaticity representation, Laplacian fitting, masks,
   scale schedule, boundary conditions, convergence/fallback rules, and output
   bounds.
2. Create a scalar single-scale prototype on tiny fixtures and compare it with
   hand-calculated results.
3. Add multiscale reconstruction with typed pyramid levels, fallible checked
   allocation, cancellation between levels/passes, and deterministic execution.
4. Add fixtures for a thin bright line, hair-like detail, a lamp boundary,
   textured metal, smooth gradients, color edges, and fully unsupported
   regions. Compare structure preservation and halo width against Opposed and
   Segmentation.
5. Establish a strict peak-memory budget and representative CPU timing before
   application integration. A high-quality method may be slower, but the UI
   must communicate/rebuild asynchronously without blocking or flashing.
6. Integrate it as a vertical slice only after the processing domain and
   retained-preview cache contract are proven.
7. Consider GPU acceleration only after CPU fixtures define parity tolerances
   for every intermediate and final output. Validate on the host AMD GPU.

The method is complete only when multiscale halos, transition seams, memory,
and cancellation have explicit coverage. A visually impressive single image is
not sufficient evidence.

### 4.4 LCh reconstruction — deferred decision

No LCh implementation is currently planned. It mixes perceptual color-space
logic into a problem that the current architecture treats in sensor or linear
RGB domains, and it is lower priority than the spatial methods above.

Reconsider it only if the comparison corpus reveals a repeatable class of
highlights where Clip, Opposed, Segmentation, and Guided Laplacian all have an
unacceptable tradeoff and an LCh prototype addresses it. If reconsidered, its
first task is to state the typed image domain and color-space conversion
contract; it must not be added to the RAW crate by default.

## 5. Shared acceptance gates for every new highlight method

Each method must satisfy all of these before handoff:

### API and architecture

- The deterministic CPU pipeline remains the reference.
- RAW data remains immutable; the normalized mosaic may be consumed in place.
- The algorithm crate has no upward dependency on core, recipes, GPU, or UI.
- Settings have one unambiguous meaning and are stored in a versioned recipe.
- No method-specific data is flattened into misleading common diagnostics.
- Preview, Source 1:1, and export run the same algorithm at equivalent source
  coordinates and resolution.

### Correctness

- Small asymmetric fixtures cover CFA phase, odd dimensions, non-tight stride,
  thresholds, negative and over-range samples, invalid data, cancellation, and
  deterministic multi-threading.
- Valid or rejected samples obey the method's bit-preservation contract.
- Fully unsupported regions degrade predictably and are counted honestly.
- Cache invalidation, WB compatibility, stale-result rejection, CLI validation,
  desktop control state, undo/reset, and diagnostics have focused tests.

### Performance and evidence

- Kernel time is measured separately from RAW decode, demosaic, UI rendering,
  and file I/O.
- Scratch allocations and peak working set are included in rejection checks.
- Representative benchmarks include no-op, sparse, boundary, and large-region
  cases.
- Private-corpus Source 1:1 and export comparisons record both wins and
  regressions.
- Downstream GPU tests run on the RX 9070 XT when cache/upload behavior changes;
  a software rasterizer is not hardware validation.

## 6. Separate gamut-mapping plan

Highlight reconstruction repairs missing or unreliable sensor samples. Gamut
mapping operates later on complete RGB pixels and must remain a separate
domain.

### 6.1 Current state — baseline implemented

Rohditor currently converts its linear Rec.2020 working image to linear sRGB,
hard-clips each output channel to `[0, 1]`, applies the sRGB transfer function,
and quantizes. `OutputPolicy::ClipToSrgb` names that behavior. This is a useful
reference but can shift hue and lose chroma detail for saturated colors.

There is not yet a dedicated gamut crate or a chroma-compressing mapper.

### 6.2 First gamut slice — chroma compression

**Goal:** preserve in-gamut pixels exactly and compress only the out-of-gamut
chroma needed to reach the target gamut, with stable hue and lightness behavior.

Implementation plan:

1. Freeze the colorimetric contract: linear Rec.2020 input, sRGB/D65 target,
   chosen perceptual or opponent representation, gamut-boundary calculation,
   neutral-axis behavior, hue path, and numerical tolerances.
2. Create `rohditor-gamut` only when that contract is ready. It should depend
   on typed RGB/color primitives, not RAW metadata, recipes, the desktop, or
   encoders.
3. Move or wrap the current hard-clip reference without changing its output.
   Add `ChromaCompress` beside it; do not add speculative `AcesLike` variants.
4. Test identity for in-gamut RGB, neutrals, primary/secondary hue sweeps,
   negative components, extreme finite values, continuity at the gamut
   boundary, monotonic compression, and no NaN/infinity production.
5. Use small exact fixtures plus dense generated sweeps. Track maximum hue,
   lightness, and round-trip errors rather than relying only on screenshots.
6. Integrate through `OutputPolicy` because this first mapper is target-output
   behavior, not a scene edit. Preview and export must select the same mapper.
7. Implement GPU preview parity only after the CPU reference is fixed. Test
   tolerances in linear values and encoded sRGB codes on the host adapter.
8. Benchmark the mapper separately from transfer encoding and quantization and
   add clipping/compression counts to diagnostics if they remain cheap.

Working-gamut compression during camera-to-Rec.2020 conversion is a separate
future decision. Do not silently reuse the output mapper there: its target,
purpose, and acceptable appearance tradeoffs differ.

## 7. Separate tone-mapping plan

Tone mapping compresses scene-referred dynamic range; it does not reconstruct
missing RAW channels and it is not gamut mapping.

### 7.1 Current state

Rohditor already has exposure, highlights/shadows/whites/blacks, contrast, and a
four-region tone curve with CPU/GPU behavior. Those are creative Light edits.
There is no dedicated scene-to-display tone-mapping stage or independent
`rohditor-tonemap` crate.

### 7.2 First tone-mapping slice

**Entry condition:** real HDR or high-dynamic-range scene cases show that the
existing Light controls plus output clipping cannot produce a stable default
display rendering without manual compensation.

**Goal:** add one understandable scene-to-display curve before considering a
menu of filmic looks.

Implementation plan:

1. Audit the existing Light-tone LUT and tone curve to eliminate semantic
   overlap. State which operations are user edits and which are display-range
   rendering policy.
2. Specify one curve first—most likely a luminance-preserving shoulder or
   sigmoid—with exposure anchor, middle-gray behavior, white point, asymptote,
   negative-input policy, and invertibility/monotonicity requirements.
3. Put reusable math in `rohditor-tonemap` only when the contract is stable.
   Operate on typed linear RGB and preserve chromaticity by scaling from a
   clearly defined luminance or norm; do not apply independent channel curves
   accidentally.
4. Decide ownership explicitly: creative parameters belong in the edit recipe;
   target-display parameters belong in render/output settings. Do not store one
   value in both.
5. Add exact tests for black, middle gray, white, over-range highlights,
   negatives, saturated colors, monotonicity, continuity, finite output, and
   CPU/GPU parity.
6. Compare tone-map-before-gamut-map with the reverse order on saturated HDR
   fixtures and fix one pipeline order with tests.
7. Integrate preview and export together, add method/version cache identity,
   preserve async frame handoff, and benchmark the kernel separately.
8. Evaluate highlight roll-off, local contrast, hue stability, and interaction
   with existing Highlights/Whites and tone-curve controls on a fixed corpus.

AgX-style transforms, multiple filmic curves, local tone mapping, and automatic
parameter selection are out of scope for the first slice.

## 8. Execution order

The recommended order is:

1. Keep Off, Clip, and Local ratios stable and refresh their benchmark/private
   corpus evidence when making quality claims.
2. Keep the implemented Opposed contract and refresh its benchmark/private
   corpus evidence.
3. Compare all implemented RAW methods and decide whether any method is ready
   to replace Off as the default. A default change is a separate, evidence-led
   decision.
4. Implement Segmentation only if region-level failures justify it.
5. Run the Guided Laplacian research gate, then choose its typed domain and
   crate before implementation.
6. Pursue chroma-compressing gamut mapping independently of RAW reconstruction.
7. Add a dedicated tone mapper only after its need and relationship to current
   Light controls are demonstrated.

## 9. Verification commands

For documentation-only changes, `./scripts/check.sh` is sufficient to confirm
that the audited implementation still passes the normal workspace checks. For
algorithm or integration changes, run the relevant focused and complete suites:

```bash
cargo test -p rohditor-highlight
cargo test -p rohditor-edit
cargo test -p rohditor-core
cargo test -p rohditor-gpu
cargo test -p rohditor-desktop
cargo test -p rohditor-cli
cargo bench -p rohditor-highlight --bench clip
cargo bench -p rohditor-highlight --bench local_ratios
cargo bench -p rohditor-highlight --bench opposed
./scripts/check.sh
cargo test --release --workspace --tests -- --ignored --nocapture
cargo test --release -p rohditor-gpu -- --ignored --nocapture
```

Add a dedicated benchmark command when each new algorithm lands. Ignored GPU
tests validate downstream rendering and cache/upload behavior unless the new
algorithm itself has a GPU implementation with CPU parity fixtures.

## 10. Reference implementations and licensing

Existing research identified these algorithm families:

- RawTherapee exposes luminance, CIELab, color-propagation, blend, and newer
  opposed-style recovery. Its more advanced paths use substantial spatial
  filtering rather than simple channel substitution.
- darktable exposes Clip, LCh, color inpainting/opposed, segmentation, and
  guided-Laplacian approaches. The latter two address region identity and
  multiscale structure at greater complexity and cost.
- Both projects provide useful evidence for an opposed/inpainting middle tier
  between Local ratios and segmentation or Laplacian methods.

Rohditor is GPL-3.0-or-later, so compatible GPL source may be studied and
adapted. Derived work must retain the applicable attribution, copyright, and
license notices. Prefer a Rust design built around Rohditor's typed images,
fallible allocation, cancellation, and tests over a mechanical line-by-line
translation.

References:

- [darktable highlight reconstruction manual](https://docs.darktable.org/usermanual/development/en/module-reference/processing-modules/highlight-reconstruction/)
- [darktable `highlights.c`](https://github.com/darktable-org/darktable/blob/master/src/iop/highlights.c)
- [darktable opposed reconstruction](https://github.com/darktable-org/darktable/blob/master/src/iop/hlreconstruct/opposed.c)
- [darktable guided Laplacian reconstruction](https://github.com/darktable-org/darktable/blob/master/src/iop/hlreconstruct/laplacian.c)
- [RawTherapee highlight reconstruction](https://github.com/RawTherapee/RawTherapee/blob/dev/rtengine/hilite_recon.cc)
- [RawTherapee RAW highlight/WB ordering](https://github.com/RawTherapee/RawTherapee/blob/dev/rtengine/rawimagesource.cc)
