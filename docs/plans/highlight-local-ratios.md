# Highlight local-ratios implementation plan

**Status:** proposed  
**Depends on:** the Highlight Clip feature described in
[`highlight-clipping.md`](highlight-clipping.md)  
**Scope:** the second RAW-highlight feature: a conservative, deterministic
local-ratio reconstruction method for Bayer mosaics  
**Out of scope:** opposed/global-chrominance inpainting, iterative propagation,
segmentation, guided Laplacians, LCh reconstruction, GPU reconstruction,
gamut mapping, and tone mapping

The Clip implementation is still in progress while this document is written.
Names below describe the intended contracts. Before implementing this plan,
reconcile them with the API, recipe schema, diagnostics, and cache key that
actually land with Clip. Do not maintain parallel abstractions merely to match
the proposed names in either plan.

## 1. Why local ratios are the next slice

Clip is a destructive baseline rather than reconstruction: it reduces surviving
channel data to prevent false-colored highlights. The next useful step should
recover a missing channel from valid local color relationships while remaining
small enough to understand and validate thoroughly.

Local ratios are suitable because they:

- operate on the normalized CFA mosaic before demosaic;
- reuse the highlight crate, validation, cancellation, recipe, cache, and UI
  integration established by Clip;
- are local, deterministic, and amenable to row-parallel CPU processing;
- create the CFA-neighborhood and reconstruction-statistics primitives needed
  by later methods;
- can be tested exactly on synthetic Bayer scenes; and
- expose the failure cases that an opposed or segmentation method must solve.

This first method is deliberately conservative. It should handle isolated and
small partially clipped regions well. It should not pretend to recover color
when all useful local evidence is gone. It remains an opt-in comparison method,
not the production default.

## 2. Keep detection separate from Clip's treatment limit

The Clip plan uses active white-balance gains to derive per-channel limits that
produce a common post-WB ceiling:

```text
common_ceiling = clip_threshold * min(wb_gains)
clip_limit[c]  = common_ceiling / wb_gain[c]
```

Those limits answer:

> At what value should Clip cap this channel for the selected white balance?

They do not answer:

> Which sensor sample has probably lost information through saturation?

Local-ratio detection must instead use camera-native normalized values. For a
validated detection threshold `t_c`:

```text
suspected_clipped(sample, c) := sample >= t_c
```

The initial UI supplies one scalar threshold, and `core` expands it to equal
per-channel normalized thresholds:

```text
t_r = t_g = t_b = detection_threshold
```

The algorithm API should retain a three-channel threshold type so later camera
evidence can supply distinct values without redesigning the crate.

Use **suspected clipped** consistently. A decoder-normalized value at or above a
selected threshold is not proof that a sensor photosite physically saturated.
Rohditor preserves values above nominal white, and this method must not describe
all such values as destroyed sensor measurements.

Clip and Local ratios therefore have separate method-specific thresholds:

```text
Clip threshold:
    WB-dependent treatment ceiling

Local-ratio detection threshold:
    WB-independent suspected-saturation boundary
```

Do not pass Clip's derived channel limits to the local-ratio detector, and do
not reuse one recipe field with both meanings.

## 3. User-visible behavior

Extend **Highlight reconstruction** to:

```text
Off
Clip
Local ratios
```

The default remains **Off**. Adding this feature must not change existing
recipes, previews, or exports until the user selects Local ratios.

When Local ratios is selected:

- expose a **Detection threshold** control;
- default it to `1.0`, relative to decoder-normalized white;
- validate the same broad `0.5 ..= 1.5` range used for normalized highlight
  thresholds, while keeping it stored separately from Clip's threshold;
- preserve every sample below the selected detection threshold bit-for-bit;
- never reduce a sample at or above the threshold;
- replace a suspected-clipped sample only when a trustworthy local estimate is
  available and greater than the retained sample;
- preserve negative values outside candidate calculations;
- leave unsupported and fully clipped areas unchanged;
- behave consistently for all four supported Bayer layouts; and
- produce the same result for preview preparation, Source 1:1, and export at
  the same source resolution and crop.

The tooltip should explain that lowering the threshold classifies more RAW
samples as potentially clipped and can reconstruct legitimate over-range color
if set too low. Do not expose neighborhood radius, candidate counts, chroma
tolerance, or recovery bounds as UI controls in this slice.

CLI development should expose an equivalent method and method-specific flag,
for example:

```text
--highlight-reconstruction off|clip|local-ratios
--highlight-clip-threshold <0.5..1.5>
--highlight-detection-threshold <0.5..1.5>
```

If Clip lands with `--highlight-threshold`, retain it as a documented Clip
alias or migrate it cleanly rather than silently changing its meaning. Reject a
method-specific threshold when its method is not selected.

## 4. Version-1 algorithm contract

The implementation operates on the normalized Bayer mosaic. It builds compact
summaries for logical 2x2 Bayer cells, estimates ratios from nearby clean cells,
and writes accepted estimates back only to suspected-clipped photosites.

This is an original, deliberately limited local-ratio method. It is not a port
of darktable's inpaint-opposed implementation. The opposed method uses
additional concepts such as opposing-channel references, mask dilation, and
near-highlight chrominance correction; those belong in the next algorithm.

### 4.1 Logical Bayer cells

Partition the visible mosaic into cells whose origins are at even `(x, y)`
coordinates in the cropped `MosaicImage`. The shifted Bayer pattern stored by
the image determines the colors; never assume that `(0, 0)` is red.

A complete cell contains one red, two green, and one blue photosite. Edge cells
may be incomplete when a dimension is odd. For each cell and color `c`, record:

- the mean of positive, finite, below-threshold samples of color `c`;
- whether at least one usable sample of `c` exists; and
- whether any sample of `c` in the cell is suspected clipped.

For green, average the usable green sites. A candidate numerator is considered
clean only if the cell contains the expected color and no site of that color is
suspected clipped. A supporting channel needs at least one usable sample.

Use checked ceiling division for cell dimensions:

```text
cell_width  = ceil(width / 2)
cell_height = ceil(height / 2)
```

Partial edge cells remain usable when they contain the channels required by a
particular estimate. Do not mirror pixels or read padding to manufacture a
complete cell.

### 4.2 Candidate ratios

For a suspected-clipped target photosite of color `c`, take its logical cell as
the target cell. Let `S` contain usable target-cell channels other than `c`.

If `S` is empty, the site is fully unsupported and remains unchanged.

For each supporting channel `s` in `S`, inspect nearby cells in increasing
Chebyshev radius. Version 1 searches radius 1 and then radius 2, giving a
maximum 5x5-cell neighborhood. A cell is a candidate for ratio `c/s` only when:

- its `c` numerator is clean;
- it has a usable `s` denominator;
- numerator and denominator are positive and finite;
- the denominator is greater than `0.02 * t_s`; and
- it passes the optional cross-channel consistency test below.

The candidate contributes:

```text
ratio[c/s] = candidate[c] / candidate[s]
```

Stop at the first radius that provides enough candidates. Require at least
three candidates when the target has two supporting channels and at least five
when it has only one. The one-channel case has less evidence and therefore uses
the stricter requirement.

Candidate arrays are bounded by the fixed neighborhood and should use stack
storage. Sort with a defined total float ordering and use the median. Do not
allocate a `Vec` per target site.

### 4.3 Cross-channel edge guard

When the target has two usable supporting channels `s` and `u`, their ratio is
evidence about which side of a color edge the target belongs to. A candidate
that also has both supporting channels is accepted only if:

```text
abs(log2(candidate[s] / candidate[u])
  - log2(target[s] / target[u])) <= 0.5 EV
```

Candidates missing `u` are rejected in the two-support case. This conservative
guard is intended to stop a clipped red object from borrowing `R/G` ratios from
an adjacent green object. It is not full edge-aware reconstruction; the
synthetic edge tests and private corpus decide whether the fixed tolerance is
adequate before the algorithm is considered complete.

When only one supporting channel survives, there is no cross-channel edge test.
The higher minimum candidate count is the only version-1 safeguard.

### 4.4 Estimate and acceptance

For each supporting channel with enough candidates:

```text
estimate_from_s = target[s] * median(local ratio[c/s])
```

If both supporting channels produce estimates, require them to agree within one
stop:

```text
abs(log2(estimate_from_s / estimate_from_u)) <= 1.0 EV
```

Combine two accepted estimates with their geometric mean. Use the single
estimate when only one supporting channel is available.

Bound the version-1 estimate to two stops above the selected detection level:

```text
maximum_estimate[c] = 4.0 * t_c
bounded_estimate    = min(estimate, maximum_estimate[c])
output              = max(original, bounded_estimate)
```

The lower-bound rule is essential: if a suspected-clipped value really is
saturated, its retained value is only a lower bound on the true signal. Local
ratios must never turn a bright sample darker merely because its neighbors
produce a weak estimate.

Reject non-finite or non-positive estimates. If no estimate survives all
checks, retain the original sample and report a fallback.

The constants in this section define Local ratios algorithm version 1. They may
be adjusted during implementation only with corresponding focused tests and
recorded corpus evidence. Once released in a recipe, later numerical changes
must bump the reconstruction/cache algorithm version.

### 4.5 No implicit Clip fallback

Version 1 does not run Clip automatically for unsupported sites. Doing so would:

- make Local ratios depend on active white balance;
- make its retained camera-native preview unsafe for dynamic WB reuse;
- silently combine two methods with different meanings; and
- hide exactly the large/fully clipped failure cases needed to evaluate the
  next reconstruction algorithm.

Unsupported sites remain unchanged and are counted. Users can select Clip when
neutralization is preferable. A later opposed or segmentation method may add a
different explicit recovery policy after corpus evidence justifies it.

## 5. Crate and API design

Extend the `rohditor-highlight` crate that lands with Clip. Keep it independent
of `raw`, `core`, `edit`, `gpu`, and `apps/desktop`.

The resulting source layout should be approximately:

```text
crates/highlight/src/
  lib.rs
  detect.rs
  clip.rs
  cells.rs
  local_ratios.rs
```

`cells.rs` owns Bayer-cell indexing, compact summaries, bounded neighborhood
iteration, and odd-dimension handling. Keep ratio policy in `local_ratios.rs` so
future opposed reconstruction can reuse cell geometry without inheriting this
algorithm's decisions.

Extend the landed API with concepts equivalent to:

```rust
pub struct ChannelDetectionLevels {
    pub red: f32,
    pub green: f32,
    pub blue: f32,
}

pub struct LocalRatioOptions {
    pub detection_levels: ChannelDetectionLevels,
}

pub struct ReconstructionStats {
    pub suspected_clipped_sites: usize,
    pub reconstructed_sites: usize,
    pub changed_sites: usize,
    pub fallback_sites: usize,
    pub fully_unsupported_sites: usize,
    pub suspected_by_channel: [usize; 3],
}

pub struct LocalRatioOutput {
    pub mosaic: MosaicImage<f32>,
    pub stats: ReconstructionStats,
}

pub fn reconstruct_local_ratios(
    mosaic: MosaicImage<f32>,
    options: LocalRatioOptions,
) -> Result<LocalRatioOutput, HighlightError>;

pub fn reconstruct_local_ratios_cancellable(
    mosaic: MosaicImage<f32>,
    options: LocalRatioOptions,
    cancellation: &dyn CancellationCheck,
) -> Result<LocalRatioOutput, HighlightError>;
```

Required statistics invariants are:

```text
suspected_clipped_sites = reconstructed_sites + fallback_sites
changed_sites <= reconstructed_sites
fully_unsupported_sites <= fallback_sites
sum(suspected_by_channel) = suspected_clipped_sites
```

`reconstructed_sites` means a valid estimate passed the contract, even when the
lower-bound rule leaves the stored value unchanged. `changed_sites` records
actual numerical increases.

Keep Clip's public API usable. A common static dispatcher is now reasonable if
it simplifies `core`, but do not introduce a public object-safe reconstruction
trait merely because two functions exist. Recipe selection is closed and
compile-time dispatch is sufficient.

Validate every detection level before scanning. Reject non-finite or
non-positive thresholds. Validate all visible input samples before mutation and
report the coordinate of a non-finite value. Padding must never be validated,
classified, summarized, counted, or changed.

## 6. Determinism, allocation, and cancellation

The algorithm needs immutable neighborhood evidence while updating the mosaic.
Avoid a second full-resolution mosaic by building a compact structure-of-arrays
cell summary:

```text
cell RGB values: 3 * f32 * ceil(width/2) * ceil(height/2)
cell flags:      compact byte flags per cell
```

For a 6000x4000 mosaic, the RGB summaries are approximately 72 MiB before
flags. Account for the actual allocation and alignment rather than calling it
free scratch memory.

Use this processing order:

1. validate thresholds and all visible samples while counting suspected sites;
2. return the original mosaic without scratch allocation if none are suspected;
3. build immutable cell summaries in deterministic row-parallel passes;
4. update mosaic rows in parallel, reading only the summaries and the target
   sample; and
5. reduce row-local statistics in a stable order.

No output-sized clone is needed. The summary layout must use checked dimension
and byte arithmetic and the repository's fallible allocation conventions.

Check cancellation at least once per source row during validation, once per
cell row during summary construction, and once per output row during
reconstruction. A cancellation error after mutation is safe because the
function consumes the mosaic and returns no partially processed output.

Results and statistics must be bit-identical for Rayon thread counts 1 and
greater than 1. Do not make floating-point reduction order depend on scheduling;
all per-site medians are local and statistics are integers.

## 7. Recipe ownership and migration

Extend `rohditor-edit::HighlightMethod` with `LocalRatios`.

Do not give the Clip threshold and Local-ratio threshold one shared field. If
Clip lands with the proposed version-4 shape:

```rust
pub struct HighlightAdjustments {
    pub method: HighlightMethod,
    pub threshold: f32,
}
```

migrate the next schema to a method-specific shape such as:

```rust
pub struct HighlightAdjustments {
    pub method: HighlightMethod,
    pub clip: ClipAdjustments,
    pub local_ratios: LocalRatioAdjustments,
}

pub struct ClipAdjustments {
    pub threshold: f32,
}

pub struct LocalRatioAdjustments {
    pub detection_threshold: f32,
}
```

If the landed Clip schema already separates its settings, extend that structure
instead. Prefer one clear migration over preserving an ambiguous Rust field.

Assuming Clip lands as schema version 4, bump to version 5. The v4 migration
must preserve its threshold as `clip.threshold`, set the Local-ratio detection
threshold to `1.0`, and preserve the selected `Off` or `Clip` method. Versions
1-3 should continue through the existing migrations and receive the same safe
defaults.

Tests must cover:

- default recipe serialization and round-trip;
- v1-v4 migration to the new representation;
- exact preservation of a non-default v4 Clip threshold;
- missing method-specific groups receiving defaults;
- `LocalRatios` serialization and round-trip;
- invalid detection thresholds and unknown methods;
- switching methods without copying one method's threshold into the other;
- reset, undo, and redo; and
- older recipes remaining `Off` unless they explicitly selected Clip.

## 8. Core pipeline and diagnostics

Local ratios occupies the same stage as Clip:

```text
decode immutable RawFrame
  -> choose crop and normalize CFA samples
  -> selected highlight operation
  -> demosaic
  -> preview resample
  -> white balance and camera color conversion
  -> downstream edits
```

Extend the `core` highlight adapter to:

1. validate the recipe;
2. expand the scalar detection threshold into camera-normalized channel
   thresholds without using active WB gains;
3. call the cancellable Local-ratio implementation;
4. map typed errors without string matching; and
5. propagate method-specific statistics.

Clip continues to derive WB-dependent treatment limits exactly as specified by
its own plan. Keeping those two branches visibly different in the adapter is a
correctness feature.

Once more than Clip uses the stage, generalize a landed timing name such as
`highlight_clipping` to `highlight_processing` or `highlight_reconstruction`.
Do not add separate adjacent timing fields when only one method can run.

Represent diagnostics as a method-tagged enum or an equally explicit type:

```rust
pub enum HighlightDiagnostics {
    Off,
    Clip(ClipStats),
    LocalRatios(ReconstructionStats),
}
```

Propagate it through reconstructed preview, demosaiced base, render, export,
CLI, and desktop diagnostics. Do not flatten `affected` and
`suspected_clipped` into one misleading counter.

## 9. Preview cache and GPU boundary

Extend the reconstructed-camera-RGB cache key conceptually to:

```text
highlight key =
  Off
  Clip {
    clip threshold bits,
    white-balance key,
  }
  LocalRatios {
    detection threshold bits,
    local-ratios algorithm version,
  }
```

Local ratios is camera-native and independent of selected WB, including for
unsupported sites because they remain unchanged. A reconstructed Local-ratio
preview therefore supports dynamic white balance just like Off:

```text
Off:          dynamic WB supported
Clip:         exact reconstruction WB required
LocalRatios:  dynamic WB supported
```

Changing the Local-ratio detection threshold or method invalidates the
reconstructed source. Changing only white balance must reuse it and invalidate
only the downstream demosaiced/adjusted levels required by the existing cache
contract.

Local ratios remains a CPU RAW-stage algorithm. Its camera-native result may be
uploaded for the existing downstream GPU preview. Do not add a GPU Local-ratio
implementation in this slice, and do not claim GPU algorithm parity merely
because the downstream upload and rendering paths pass.

The desktop must retain the last valid visible frame during asynchronous
reconstruction, discard stale completions, and install only the newest
document/revision result.

Add cache and coordinator tests for:

- Local ratios plus WB change reusing reconstructed camera RGB;
- identical method and threshold reusing it;
- threshold change invalidating it;
- every transition among Off, Clip, and Local ratios invalidating the correct
  cache levels;
- Clip retaining its WB-sensitive behavior;
- stale Local-ratio CPU completion being discarded; and
- downstream GPU upload preserving the dynamic-WB capability flag.

## 10. Correctness tests

### `rohditor-highlight` tests

Use small asymmetric mosaics, odd dimensions, and non-tight strides. Cover:

1. **No suspected clipping:** output is bit-identical, statistics are zero, and
   no cell-summary allocation is performed.
2. **Flat colored patch:** generate known cell RGB values, clamp selected red
   sites at the threshold, and recover the original red value from exact local
   ratios without a hue discontinuity.
3. **Each channel:** reconstruct red, blue, each individual green site, and both
   green sites in a cell.
4. **All Bayer layouts:** RGGB, BGGR, GRBG, and GBRG produce phase-equivalent
   output and statistics.
5. **One and two supporting channels:** exercise both candidate-count rules and
   the two-estimate agreement check.
6. **Color edge:** a clipped red-side site next to a green region rejects
   cross-channel-inconsistent candidates and does not borrow the green-side
   ratio.
7. **Insufficient candidates:** the original value is retained and the site is
   counted as fallback.
8. **Fully unsupported cell:** all other colors suspected clipped leaves the
   target unchanged and increments both fallback and fully-unsupported counts.
9. **Lower-bound preservation:** a valid but lower estimate never reduces the
   input; it is reconstructed but not changed.
10. **Recovery bound:** a pathological denominator cannot raise the output more
    than two stops above the detection level.
11. **Dark candidate rejection:** zero, negative, and near-zero denominators do
    not produce ratios.
12. **Threshold boundary:** samples just below, exactly equal to, and just above
    each channel threshold follow the shared comparison rule.
13. **Odd edges and padding:** incomplete cells work when sufficient channels
    exist, while padding is untouched and uncounted.
14. **Invalid data:** thresholds are rejected before scanning; non-finite input
    reports the correct visible coordinate before mutation.
15. **Cancellation:** cancellation is observed in validation, summary, and
    reconstruction phases.
16. **Thread determinism:** output and statistics are identical under Rayon
    thread counts 1 and greater than 1.

Use the same detector entry point in the no-clipping fast path, summary builder,
and target reconstruction. Tests must prove materialized diagnostic masks agree
with the fused classifications.

### Core integration tests

Add fixtures proving:

- Local ratios runs after normalization and before demosaic;
- below-threshold normalized values remain bit-identical;
- a synthetic clipped patch produces the expected recovered scene-linear RGB
  after demosaic;
- changing WB after Local-ratio reconstruction matches a fresh full pipeline
  render for that WB;
- Local-ratio reconstruction itself is invariant under WB changes;
- fit preview, Source 1:1, and export agree when run on the same source crop and
  resolution;
- cancellation maps to `PipelineError::Cancelled`;
- allocation failures and invalid data retain stage context; and
- method-specific diagnostics are not confused with Clip statistics.

### Recipe, CLI, desktop, and GPU tests

- Recipe migration and validation cases from section 7.
- CLI parsing for every valid method/threshold combination and rejection of
  mismatched method-specific flags.
- Desktop visibility/enabling of only the selected method's threshold.
- One discrete undo entry for selector and committed threshold changes.
- Cache-key and dynamic-WB cases from section 9.
- GPU upload/base compatibility tests; the algorithm itself remains CPU-only.

## 11. Benchmark and memory validation

Add a Criterion benchmark beside the Clip benchmark using deterministic
normalized mosaics at:

- 6000x4000 with tight stride;
- a smaller asymmetric, odd-sized mosaic with padded stride;
- no suspected sites;
- sparse isolated clipped sites;
- a small clipped colored region near an edge; and
- a large unsupported clipped region.

Measure and report separately:

- detection/validation fast path;
- cell-summary construction;
- reconstruction;
- total highlight-stage time;
- allocated scratch bytes; and
- full preview-preparation time before and after the feature.

The no-clipping path must allocate no cell summary. The clipping path must not
allocate an output-sized mosaic clone or allocate per target site. The initial
performance target is no more than 20% added to full preview preparation for a
representative sparse-highlight 6000x4000 image on the development machine. If
it misses, profile the summary layout and candidate traversal before considering
unsafe code; workspace policy continues to forbid unsafe Rust.

Update `MemoryEstimate` and working-set rejection tests with the actual compact
summary bytes. Do not assume `size_of::<CellSummary>()` matches the intended
sum of fields without asserting its layout or, preferably, using separate
compact arrays whose capacities can be measured directly.

## 12. Private-corpus validation

For the six private Sony ILCE-6400 files, compare the same sensor-coordinate
crops under Off, Clip, and Local ratios at Source 1:1 and export resolution.

Include, where available:

- colored lights with one channel clipped;
- saturated object color adjacent to a differently colored edge;
- specular highlights;
- clouds and other naturally neutral highlights;
- small clipped regions with valid surroundings;
- large or fully clipped regions; and
- samples with normalized values above `1.0` that may still contain useful
  variation.

Record:

- suspected, reconstructed, changed, fallback, and fully unsupported counts;
- whether lowering or raising the detection threshold improves the known crop;
- hue continuity across the clipping boundary;
- edge color bleeding;
- false-color behavior in unsupported regions;
- fit-preview versus export agreement;
- highlight-stage wall time and scratch memory; and
- every crop where Clip remains visibly preferable.

Do not change the default from Off in this feature. Do not promote Local ratios
to a production default until the corpus demonstrates a consistent improvement
and its unsupported-region behavior is acceptable. The recorded failures are
inputs to the opposed/inpainting plan, not reasons to expand this slice into
that algorithm.

## 13. Implementation sequence

1. Reconcile this plan with the landed Clip API, schema, cache key, timing, and
   diagnostics names.
2. Split recipe settings into unambiguous method-specific groups and implement
   migration/validation tests.
3. Add camera-native detection levels and prove they are distinct from Clip's
   WB-derived limits.
4. Implement and test compact Bayer-cell summaries, odd dimensions, and
   bounded neighborhood iteration.
5. Implement the version-1 ratio estimator, edge guard, lower bound, recovery
   bound, fallback statistics, and cancellation.
6. Add benchmarks and exact scratch-memory accounting before pipeline
   integration.
7. Extend the core adapter and all full-resolution and preview preparation
   paths.
8. Generalize stage timings and method-tagged diagnostics.
9. Extend reconstructed-preview cache keys and dynamic-WB capability tests.
10. Add CLI and desktop controls with undo/reset behavior.
11. Run focused tests, repository checks, ignored private tests, and downstream
    host GPU regressions.
12. Perform Source 1:1 corpus review and report measured evidence in the
    implementation handoff rather than adding results to this plan.

## 14. Required verification before handoff

Use the equivalent focused commands if package or target names differ after the
Clip implementation lands:

```bash
cargo test -p rohditor-highlight
cargo test -p rohditor-edit
cargo test -p rohditor-core
cargo test -p rohditor-gpu
cargo test -p rohditor-desktop
cargo test -p rohditor-cli
cargo bench -p rohditor-highlight --bench local_ratios
./scripts/check.sh
cargo test --release --workspace --tests -- --ignored --nocapture
cargo test --release -p rohditor-gpu -- --ignored --nocapture
```

The ignored GPU suite validates the retained-source/upload contract and
downstream rendering. It does not validate a GPU Local-ratio algorithm. Any RX
9070 XT timing or parity claim must come from the host hardware run rather than
a sandbox software adapter.

## 15. Definition of done

The feature is complete when:

- existing recipes remain Off and render unchanged;
- Local ratios uses a camera-native detection threshold distinct from Clip's
  WB-dependent treatment limits;
- the version-1 estimator is fully specified, bounded, deterministic, and
  conservative at unsupported sites;
- every below-threshold sample and every rejected target remains bit-identical;
- all Bayer phases, odd dimensions, padding, cancellation, invalid input, and
  thread-count cases are covered;
- the highlight crate remains independent of RAW metadata, recipes, core, GPU,
  and UI;
- preview, Source 1:1, and export use the same pre-demosaic implementation;
- dynamic WB safely reuses Local-ratio reconstructed camera RGB while Clip
  retains its stricter WB cache dependency;
- timing, statistics, cache invalidation, stale-result rejection, and memory
  accounting are propagated and tested;
- focused, workspace, ignored private, and host GPU regression suites pass;
- benchmarks confirm no per-site or output-sized allocation; and
- the handoff reports corpus wins and failures without changing the default or
  claiming recovery in fully unsupported regions.

## References checked for this plan

- [Broader Rohditor clipping and reconstruction proposal](clipping-and-reconstruction.md)
- [Rohditor Highlight Clip implementation plan](highlight-clipping.md)
- [darktable `highlights.c`](https://github.com/darktable-org/darktable/blob/master/src/iop/highlights.c)
- [darktable opposed reconstruction source](https://github.com/darktable-org/darktable/blob/master/src/iop/hlreconstruct/opposed.c)
- [RawTherapee highlight reconstruction source](https://github.com/RawTherapee/RawTherapee/blob/dev/rtengine/hilite_recon.cc)
