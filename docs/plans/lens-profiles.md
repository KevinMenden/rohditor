# Lens profile corrections

**Status:** planned

**Plan date:** 2026-09-07

**Canonical scope:** profile-driven geometric distortion, vignetting, and
lateral chromatic-aberration correction under the desktop **Optics** section

## 1. Objective and user outcome

Rohditor should use capture metadata to find a trustworthy Lensfun camera/lens
profile and apply the profile's available corrections consistently to the
interactive preview, Source 1:1 view, and full-resolution export.

The first production version should let a user:

- leave lens correction off, which remains the default while the feature is
  being qualified;
- enable an automatically matched profile;
- see exactly which camera and lens profile was selected;
- choose among plausible profiles when metadata is missing or ambiguous;
- independently enable distortion, vignetting, and lateral chromatic
  aberration (TCA) when the selected profile contains those calibrations; and
- get an explicit explanation when a correction could not be applied.

No profile is better than a confidently wrong profile. Automatic matching must
therefore be conservative and must not silently select the first fuzzy result.

This is a new, self-contained feature plan. It does not change the completed
MVP plan.

## 2. Scope and non-goals

### In scope for the first production release

- A focused `rohditor-optics` crate around Lensfun profile loading, matching,
  interpolation, coordinate generation, and deterministic CPU correction.
- A bundled, pinned Lensfun database snapshot so the feature works without a
  system package or network access.
- Camera/lens matching from Rohditor-owned RAW metadata.
- An explicit manual Lensfun profile override for ambiguous or incorrect EXIF.
- Distortion, vignetting, and lateral CA toggles.
- Automatic scaling that keeps the output dimensions unchanged while avoiding
  invalid borders introduced by geometric correction.
- Versioned recipe state, preview-cache identity, diagnostics, desktop
  controls, and CLI coverage.
- A CPU correctness implementation shared by preview, Source 1:1, and export.
- Reuse of the corrected camera-native preview as the existing GPU backend's
  immutable source; lens correction itself remains CPU-only initially.

### Deliberately out of scope for the first release

- Camera input profiles, DCP/ICC loading, and camera-matching looks. Those are
  color-calibration/rendering features, not lens corrections.
- Embedded manufacturer lens-correction metadata. It is an important later
  profile source, but rawler 0.7.2 does not currently expose a normalized
  correction model through Rohditor's decoder boundary.
- Manual distortion/vignetting/CA strength sliders. Profile selection and
  component toggles come first; manual correction can later use a separate
  `ManualOpticsAdjustments` model.
- Longitudinal CA, purple-fringe removal, or defringe. Lensfun TCA corrects
  lateral channel displacement; defringe belongs under Detail.
- Creative post-processing vignette. Profile vignetting is an early linear
  light compensation; a creative vignette belongs under Effects.
- Perspective correction, fisheye projection choice, panorama projection, or
  user-controlled scale. These belong with future transform/geometry work.
- Automatic online database downloads. Rendering must never perform network
  I/O, and database update policy needs its own security, provenance, and
  reproducibility design.
- GPU lens warping or full-resolution GPU export until the CPU path and its
  visual quality are established.

## 3. Decisions at a glance

| Question | Initial decision | Reason |
| --- | --- | --- |
| Profile ecosystem | Lensfun | It supplies camera/lens matching plus calibrated distortion, TCA, and vignetting models. |
| Integration candidate | Pin and wrap the pure-Rust `lensfun` crate, initially evaluating `=0.7.0` | It avoids a native C/C++/GLib runtime dependency and keeps Rohditor code compatible with `unsafe_code = "forbid"`. Its maturity must be verified before adoption. |
| Rohditor module | New `crates/optics` workspace crate | Isolates the third-party API, database policy, matching, and remapping math from RAW decoding, core orchestration, GPU code, and UI. |
| Recipe placement | New top-level `EditRecipe::optics` group | Lens corrections are user-visible edits, but are neither sensor-mosaic RAW reconstruction nor late canvas geometry. |
| Processing domain | Linear camera-native RGB | Lensfun specifically requires early linear sensor-space processing for TCA and vignetting. |
| Processing order | After highlight reconstruction and demosaic; before preview downsampling, white balance/color conversion, creative edits, orientation, and user crop | Keeps one authoritative correction result and preserves the existing typed pipeline boundaries. |
| Default | Off | Avoids changing existing renders until profile matching, quality, and performance pass the real-corpus gates. |
| Matching | Exact normalized match may auto-apply; ambiguity requires a user choice | Prevents silently applying a plausible but incorrect profile. |
| Output canvas | Same dimensions, automatically scaled to avoid invalid borders | Gives a useful correction without introducing an alpha/border representation into the current RGB pipeline. |
| Interpolation | One combined geometry+TCA remap using a deterministic high-quality cubic sampler | Avoids two interpolation passes and the associated blur/error. |
| GPU strategy | CPU-correct once, upload corrected camera-native preview, then reuse the current downstream GPU path | Delivers the feature without duplicating Lensfun math in WGSL. |

## 4. Current Rohditor constraints

The current source tree already has useful boundaries:

- `crates/raw` owns decoding and maps rawler data into `RawFileInfo` and
  `CaptureMetadata`. It already exposes camera make/model, clean camera names,
  focal length, aperture, lens make, and lens model.
- rawler's EXIF model also has `subject_distance`, but Rohditor does not yet map
  it into `CaptureMetadata`.
- `crates/edit` owns the validated, serialized `EditRecipe`; the current schema
  is version 6.
- `crates/core` owns the deterministic CPU pipeline and the exact stage order.
- A `ReconstructedPreview` currently contains reduced, unbalanced,
  camera-native RGB. It is reused across downstream white-balance, color, and
  light changes where RAW highlight provenance permits that reuse.
- The desktop preview cache separates decoded RAW, reconstructed camera RGB,
  color-converted base, and adjusted display preview.
- `crates/gpu` uploads the reconstructed camera-native preview, applies
  downstream operations, and rejects a recipe whose source-producing
  semantics do not match the uploaded source.
- Orientation and user crop are late output geometry. Lens correction must not
  be folded into that code merely because distortion changes coordinates.
- Background jobs are document/revision keyed, cancellable, coalesced, and
  newest-wins. A CPU rebuild must not blank the last valid visible frame.

There is one important preview/full-resolution difference today:

```text
preview reconstruction
  normalize -> highlight -> demosaic with identity WB -> area downsample

full-resolution base
  normalize -> highlight -> demosaic with selected WB -> camera transform
```

Lens correction can be inserted into both paths while they are still in the
camera RGB color space. The initial implementation does not need to refactor
white balance out of full-resolution demosaic, because per-channel WB scaling
does not mix channels or apply a transfer curve. Nevertheless, preview/export
equivalence tests must catch any difference caused by interpolation ordering.

## 5. Proposed architecture

### 5.1 Dependency direction

```text
                         third-party lensfun crate + bundled XML data
                                          |
                                          v
rohditor-image <---------------- rohditor-optics
       ^                                 ^ \
       |                                 |  \
rohditor-raw      rohditor-edit ---------+   rohditor-core
       \                 /                       ^
        \---------------/------------------------+
                                                |
                                  CLI, desktop, and rohditor-gpu
```

The exact diagram is less important than these rules:

- `raw` must not depend on Lensfun or decide which profile to use. It reports
  source facts only.
- `edit` must not depend on Lensfun. It stores user intent using an opaque,
  validated profile identifier.
- `optics` must not depend on `raw`, `core`, `gpu`, or an application. It
  accepts a small owned query and a typed linear RGB image.
- `core` maps RAW metadata and recipe intent into an optics request and owns
  the processing order, cancellation, timings, memory accounting, and errors.
- No Lensfun type crosses the public `rohditor-optics` boundary.
- UI code receives small presentation models, not a database handle or
  correction coefficients.

### 5.2 New crate layout

```text
crates/optics/
  Cargo.toml
  src/
    lib.rs          public Rohditor-owned types and entry points
    catalog.rs      database loading, snapshot provenance, profile summaries
    matching.rs     conservative camera/lens resolution and candidate ranking
    plan.rs         validated immutable correction plan and cache fingerprint
    correction.rs   vignetting pass and combined coordinate remap orchestration
    resample.rs     checked, cancellable camera-RGB interpolation
  tests/
    matching.rs
    correction.rs
    upstream_parity.rs
  benches/
    correction.rs
```

Do not create separate crates for matching, database access, and resampling at
this stage. They form one cohesive lens-correction domain and can remain
private modules. Conversely, do not put this code in `core/src/cpu.rs`: doing
so would leak Lensfun-specific policy into an already central module and make
the correction engine difficult to test independently.

### 5.3 Public optics concepts

Names may change during implementation, but the boundary should contain these
concepts:

```rust
pub struct OpticsQuery {
    pub camera_make: String,
    pub camera_model: String,
    pub camera_clean_make: String,
    pub camera_clean_model: String,
    pub lens_make: Option<String>,
    pub lens_model: Option<String>,
    pub focal_length_mm: Option<f32>,
    pub aperture_f_number: Option<f32>,
    pub focus_distance_m: Option<f32>,
}

pub struct LensProfileSummary {
    pub id: String,
    pub camera: String,
    pub lens: String,
    pub mount: String,
    pub available: CorrectionComponents,
}

pub enum ProfileMatch {
    Unique(LensProfileSummary),
    Ambiguous(Vec<LensProfileSummary>),
    MissingMetadata { fields: Vec<MetadataField> },
    CameraNotFound,
    LensNotFound,
}

pub struct LensCorrectionPlan { /* private Lensfun-independent contents */ }

pub struct OpticsProvenance {
    pub profile: LensProfileSummary,
    pub database: DatabaseProvenance,
    pub requested: CorrectionComponents,
    pub applied: CorrectionComponents,
    pub used_infinity_distance_fallback: bool,
    pub content_fingerprint: u64,
}
```

`LensCorrectionPlan` is the important seam. Profile lookup and calibration
interpolation happen once, producing an immutable plan that the pixel loop can
use without touching XML, performing fuzzy matching, allocating strings, or
taking a global lock. Its content fingerprint must include all interpolated
calibration data and Rohditor's correction algorithm version, not merely the
profile name.

### 5.4 Pipeline resource ownership

`CpuPipeline` is currently a unit struct. Convert it into a small
resource-owning processor that receives an `Arc<OpticsService>` explicitly at
construction. The service owns one immutable database snapshot and a small
resolution/plan cache. This is a justified API change in a pre-release project:
it makes I/O and profile data explicit, lets tests inject a tiny XML database,
and avoids hidden global state.

Provide an explicitly named no-profile constructor for focused tests and
callers that render only recipes with optics off. If a recipe requests a lens
profile on such a pipeline, return a typed error instead of silently rendering
without it. Do not make `Default` perform database I/O.

The desktop worker and CLI each create one profile service at startup and
reuse it for all jobs. Failure to load the database disables only the Optics
feature; opening and editing images with optics off must still work.

## 6. Recipe and metadata model

### 6.1 Recipe schema version 7

Add a top-level group rather than putting lens correction under `raw` or
`geometry`:

```rust
pub struct EditRecipe {
    pub schema_version: u32,
    pub raw: RawAdjustments,
    pub optics: OpticsAdjustments,
    pub light: LightAdjustments,
    pub color: ColorAdjustments,
    pub geometry: GeometryAdjustments,
}

pub struct OpticsAdjustments {
    pub profile: LensProfileSelection,
    pub distortion: bool,
    pub vignetting: bool,
    pub chromatic_aberration: bool,
}

pub enum LensProfileSelection {
    Off,
    Automatic,
    Lensfun { profile_id: String },
}
```

Defaults:

```text
profile = Off
distortion = true
vignetting = true
chromatic_aberration = true
```

The component flags intentionally remain true while the profile is off, so
enabling a profile gives the common complete correction and disabling it does
not erase the user's component choices. Validate explicit IDs as bounded,
non-empty opaque strings; `edit` must not parse Lensfun naming conventions.

Increment `EDIT_RECIPE_SCHEMA_VERSION` to 7. Deserialize all currently
accepted version 1-6 recipes with `OpticsAdjustments::default()`, preserving
their pixels because the default is off. Add JSON round-trip and migration
tests. No correction coefficients or database XML belong in the recipe.

Automatic selection is deliberately evaluated against the current bundled
snapshot. An explicit selection stores a stable logical ID containing the
canonical Lensfun maker/model/mount identity, not a vector index or fuzzy
search rank. The resolved plan separately records a content fingerprint so a
database change cannot accidentally reuse stale cached pixels.

### 6.2 RAW metadata additions

Extend `CaptureMetadata` with:

```rust
pub focus_distance: Option<RationalValue>
```

Map it from rawler's `Exif::subject_distance` when the value is finite and
positive. Preserve the exact rational at the decoder boundary and convert to
`f32` only when core builds an `OpticsQuery`.

Do not add Lensfun crop factor or mount to `RawFileInfo`; those are profile
database facts. Do not expose rawler maker-note correction blobs. When rawler
later provides normalized embedded correction data, add Rohditor-owned types
after separately validating their semantics.

Lensfun needs focal length for distortion/TCA and aperture plus distance for
vignetting. Use these rules:

- missing or invalid focal length prevents profile correction and is reported;
- missing aperture disables only vignetting, while distortion and TCA may
  still apply;
- missing focus distance uses Lensfun's conventional infinity fallback of
  `1000 m` for vignetting and records that approximation in diagnostics; and
- missing lens make may still permit a unique model match, but never weakens a
  multiple-result set into an automatic choice.

## 7. Profile database and matching

### 7.1 Dependency qualification before feature work

The preferred integration candidate is the pure-Rust `lensfun` crate pinned to
an exact reviewed version, initially `=0.7.0`. It bundles the upstream XML data,
loads custom directories, exposes `Database`/`Modifier`, and avoids native
linking. Its published documentation reports upstream A/B coordinate parity,
but its documentation currently uses both "Beta" and "Pre-alpha" labels. Do
not treat it as production-ready without a Rohditor-owned qualification step.

Before accepting the dependency:

1. Confirm its MSRV is compatible with Rust 1.88 and that it passes the
   repository's lint/build configuration.
2. Audit its license files, bundled database attribution, dependencies, build
   script, unsafe usage, and release ownership.
3. Run its distortion, TCA, vignetting, auto-scale, database, and matching APIs
   on fixed Lensfun reference vectors.
4. Compare generated coordinates and vignette gains to upstream Lensfun C/C++
   for representative `poly3`, `poly5`, `ptlens`, linear TCA, poly3 TCA, and
   `pa` vignetting profiles.
5. Confirm the bundled snapshot contains Sony Alpha 6400 and the private-corpus
   Tamron 17-70mm F/2.8 Di III-A VC RXD profile with all three corrections. The
   current upstream development list contains that combination, but the
   candidate crate's bundled snapshot must be checked directly.
6. Verify `Database`, resolved profile data, and correction plans can be shared
   safely with the desktop worker model without a mutex in the pixel hot path.
7. Record the reviewed crate and database versions in diagnostics and release
   attribution.

If this gate fails, use the same `rohditor-optics` public API and evaluate the
mature upstream C API behind maintained external Rust bindings. Keep all unsafe
FFI inside the third-party binding crate, accept the added system packaging
burden explicitly, and do not leak C pointers or Lensfun structs into
Rohditor. Do not respond to a failed dependency evaluation by reimplementing
the Lensfun database and calibration models in `core`.

### 7.2 Initial database policy

The first release uses the bundled database snapshot only:

- load it once per process;
- perform no network access;
- do not silently merge whichever system Lensfun database happens to be
  installed; and
- expose snapshot version/timestamp and a stable content fingerprint in
  diagnostics.

This provides reproducible tests and identical out-of-box behavior across
machines. A later release may load user XML overrides from an explicit
Rohditor XDG data directory, with deterministic precedence and per-file error
reporting. Online updates and automatic use of system/user Lensfun directories
remain separate work because upstream permits later database definitions to
override earlier ones.

Before distributing the database, add the required Lensfun attribution and
third-party notices. Upstream licenses the library under LGPL-3.0 and its data
under CC BY-SA 3.0; Rohditor's GPL-3.0-or-later license does not remove the need
to preserve the database's attribution and license terms.

### 7.3 Conservative resolution algorithm

Build matching as an inspectable sequence, not one call followed by
`.first()`:

1. Normalize surrounding whitespace, case, repeated separators, and common
   maker prefixes without discarding model numbers.
2. Search camera maker/model using both original and rawler-cleaned values.
3. Auto-accept only one exact normalized camera match. Otherwise return an
   explicit ambiguous/not-found state.
4. Use the matched camera mount and crop factor to constrain lens candidates.
5. Match lens maker when present, then lens model. Prefer exact normalized
   equality; retain fuzzy results only as manual candidates.
6. Reject candidates whose declared focal range cannot contain the captured
   focal length, allowing only a small documented metadata-rounding tolerance.
7. Auto-accept only one remaining exact candidate. Any multiple or fuzzy-only
   set requires user selection.
8. For an explicit profile ID, resolve it exactly and verify camera mount/crop
   compatibility. A missing former ID is an unresolved profile, not permission
   to fall back to a different lens.
9. Report which requested components have calibration at the captured focal,
   aperture, and distance. Partial profile coverage is valid and visible.

Candidate ordering must be deterministic and stable across thread counts. A
`ProfileMatch` returned to applications contains owned summaries; it must not
borrow the database or expose raw Lensfun lifetimes.

## 8. Processing contract

### 8.1 Authoritative stage order

With optics enabled, the pipeline becomes:

```text
immutable RawFrame
  -> recommended sensor crop
  -> black/white normalization
  -> RAW highlight operation
  -> demosaic to linear camera RGB
  -> lens vignetting gain in camera RGB
  -> one combined distortion + TCA coordinate remap
  -> preview area downsample, when applicable
  -> white balance, when not already applied by full-resolution demosaic
  -> camera RGB -> linear Rec.2020/D65
  -> light/color adjustments
  -> EXIF/user orientation
  -> normalized user crop
  -> output gamut/transfer/quantization
```

This follows Lensfun's requirement that TCA and vignetting operate in linear
sensor color space. Lensfun divides correction into a color pass (vignetting),
a geometry-coordinate pass, and a subpixel channel-coordinate pass, and
recommends combining the last two to avoid a second interpolation.

Important invariants:

- The immutable `RawFrame` is never modified.
- Negative and over-range linear values remain valid; lens correction must not
  clamp them to `0..1`.
- Lens correction uses the un-oriented recommended RAW crop. Orientation and
  user crop remain late operations.
- The first release supports profile correction only with
  `RawCropPolicy::Recommended`. Lensfun profiles are calibrated for the normal
  camera image raster; silently applying them to the wider active area can
  shift the assumed optical center. An ActiveArea request with optics enabled
  returns a clear unsupported-combination error.
- Corrected output keeps the same width and height as the demosaiced input.
- Preview correction happens before its antialiased area reduction, so the
  reduced preview represents the full-resolution reference pipeline.
- Preview, Source 1:1, export, histogram, and picker sampling all see corrected
  pixels when the recipe enables optics.
- Crop coordinates continue to describe normalized edges of the final
  oriented corrected canvas. Because optics preserves dimensions, existing
  crop storage and viewport geometry need no coordinate migration.

### 8.2 Correction plan construction

Core creates one plan from:

```text
resolved profile identity and calibration contents
camera crop factor from Lensfun
recommended developed width and height
captured focal length
captured aperture, if vignetting is requested
captured/fallback focus distance
requested component flags
automatic scale policy
OPTICS_ALGORITHM_VERSION
```

Plan construction validates dimensions and all finite parameters, interpolates
the available calibration samples, determines the actually applied component
set, computes the automatic scale, and produces `OpticsProvenance`.

The automatic scale must keep every enabled channel's complete interpolation
footprint inside the source image. Lensfun's auto-scale is the starting value;
Rohditor should validate the transformed perimeter at the selected cubic
kernel radius and increase scale minimally when needed. Do not hide invalid
coordinates with edge clamping, which produces stretched border pixels.

If only vignetting is active, correct in place and allocate no remap buffer. If
distortion or TCA is active, allocate one output image and combine both
coordinate transforms into a single sampling pass.

### 8.3 Sampling implementation

Lensfun generates source coordinates but deliberately does not interpolate
image pixels. `rohditor-optics` therefore owns the deterministic sampler.

Production target:

- a separable 4x4 cubic kernel with fixed documented coefficients;
- one RGB coordinate when TCA is off and three channel coordinates when TCA is
  on;
- checked row/dimension/allocation arithmetic;
- row-stride-aware input and tightly packed typed output;
- Rayon row parallelism with per-worker coordinate scratch rather than one
  full-frame coordinate map;
- cooperative cancellation at least once per output row;
- no clamping of finite linear sample values; and
- a test-only scalar bilinear/reference sampler for simple golden cases, not a
  second production mode exposed to users.

Use Lensfun's combined subpixel/geometry coordinate operation when available.
If the Rust port exposes only separate methods, compose coordinates in the
plan without materializing or interpolating an intermediate image.

Do not add a configurable interpolation selector in the first version. First
establish one quality implementation and benchmark it. If the full-resolution
correction plus area-downsample path misses the interactive latency target,
retain it as the CPU/export reference and separately evaluate a reduced-scale
preview optimization against that reference; do not silently move correction
after preview reduction.

### 8.4 Failure behavior

Profile selection and rendering errors need distinct behavior:

- `Off`: exact no-op with no pixel allocation, correction lookup, or output
  change.
- `Automatic` with one valid exact match: apply available requested
  components.
- `Automatic` with ambiguous/no match: do not apply any profile; return an
  unresolved status to the desktop so it can request a selection. Export with
  an enabled-but-unresolved recipe is disabled/fails explicitly.
- Explicit profile missing or incompatible: unresolved error; never substitute
  another profile automatically.
- Profile lacks one calibration component: apply the other requested
  components and report the unavailable one. This is not a total failure.
- Missing aperture: same partial behavior for vignetting.
- Database load failure: optics is unavailable, but recipes with `Off` render
  normally.
- Non-finite coordinates, gain, or output introduced by the correction engine:
  abort that render with a typed error; do not publish a partial frame.
- Cancellation: drop the unfinished image and surface the existing cancelled
  result so newest-wins scheduling remains intact.

The desktop must keep showing the last valid preview throughout an unresolved
selection or CPU rebuild. A failed lens correction must not clear the texture.

## 9. Core, cache, and GPU integration

### 9.1 Core pipeline changes

Add a narrow optics adapter module, for example `crates/core/src/optics.rs`,
which:

- converts `RawFileInfo` plus `EditRecipe::optics` into `OpticsQuery` and a
  plan request;
- maps optics errors into `PipelineError` without losing the field/reason;
- invokes the correction at the one authoritative point after demosaic;
- returns `OpticsDiagnostics`, timing, scratch/peak bytes, and provenance; and
- keeps `pipeline.rs` responsible for ordering rather than Lensfun details.

Extend these result types with optics diagnostics/provenance as appropriate:

- `ReconstructedPreview`;
- `DemosaicedBase`;
- `RenderResult` and `ExportRenderResult`;
- `StageTimings` with a real `optics` duration; and
- `MemoryEstimate` with optics input/output/scratch accounting.

Do not overload `resampling`, `geometry`, or `color_conversion` timings with
lens work. Update the CLI timing report, desktop structured trace, and
diagnostics panel.

### 9.2 Preview cache identity

Optics belongs in `ReconstructedCameraRgbKey`, because its output is
camera-native RGB produced before white balance and color conversion. Include:

```text
profile selection state
resolved plan content fingerprint
distortion enabled/applied
vignetting enabled/applied
TCA enabled/applied
captured focal/aperture/distance or fallback marker
automatic scale bits
OPTICS_ALGORITHM_VERSION
```

Also bump the existing `reconstruction_version` when the retained source first
becomes lens-corrected.

Expected reuse behavior:

- Light, Color, orientation, and user-crop changes reuse the corrected
  reconstruction under the current rules.
- White-balance changes reuse it for Off/Local ratios/Opposed highlight modes,
  because profile correction is WB-independent and remains camera-native.
- Clip still forces the existing WB-sensitive rebuild.
- Changing profile, correction toggles, focal/aperture/distance inputs, scale,
  database contents, or optics algorithm version rebuilds reconstruction.
- An external database reload clears all correction-plan and reconstructed
  caches before new jobs are accepted.

Add focused key tests for every item above. Do not rely only on the recipe
schema version as an invalidation mechanism.

### 9.3 GPU boundary

Do not implement Lensfun in WGSL initially. The corrected
`ReconstructedPreview` remains camera-native linear RGB and is therefore a
valid `GpuPreviewUpload` source.

Carry `OpticsProvenance` (or its compact fingerprint plus applied component
set) through `GpuPreviewUpload` and `GpuPreviewSource`.
`GpuPreviewProcessor::render` must reject a recipe whose required optics plan
does not match the resident source, just as it rejects incompatible RAW
highlight provenance today.

Active optics does not make a recipe generally GPU-unsupported: the source is
prepared on the CPU, then supported downstream edits still use the GPU. On an
optics change, the coordinator must:

1. retain the current visible frame;
2. cancel/coalesce superseded CPU reconstruction jobs;
3. upload only the newest document/revision result;
4. reject stale uploads using document, revision, and optics provenance; and
5. replace/release the former frame only when the new one is ready.

Add CPU/GPU display parity tests using a synthetically corrected source. A
real-GPU run validates the downstream handoff, not the CPU Lensfun algorithm
itself.

## 10. Desktop and CLI behavior

### 10.1 Desktop module placement

`apps/desktop/src/ui/adjustment_panel.rs` is already large. Add
`apps/desktop/src/ui/optics.rs` for the complete panel presentation and keep
only one call site in `adjustment_panel.rs`.

The module consumes a desktop-owned view model such as:

```text
OpticsPanelModel
  recipe values
  metadata camera/lens/focal/aperture
  match state and candidates
  resolved profile summary
  available/applied components
  database/error status
```

It emits intent-only actions such as select mode/profile and toggle a
component. `app.rs`/`document.rs` translate those actions into the edit session
so each click participates in undo/redo and advances the recipe revision.

The database and match engine stay on the worker/service side. Send owned,
bounded summaries to the UI; never perform XML parsing or broad fuzzy search
inside an egui frame.

### 10.2 Optics panel

Place **Optics** after Color/Color grading and before Export:

```text
Optics

Profile                 Off / Automatic / Selected profile…
Detected                Tamron 17-70mm F/2.8 Di III-A VC RXD
Captured                35 mm · f/2.8 · focus distance unknown

[x] Distortion          available
[x] Vignetting          available · using infinity distance
[x] Chromatic aberration available

Profile database        Lensfun <snapshot>
```

Behavior:

- Off is neutral and hides/disables component toggles without forgetting
  them.
- Automatic displays the resolved profile, never only the source EXIF string.
- An ambiguous match opens a deterministic candidate dropdown and applies
  nothing until the user chooses.
- Manual selection lists camera-compatible profiles and supports a small
  filter field if the list is long. It stores the exact profile ID.
- Unavailable components are disabled with a concise reason.
- Missing/ambiguous metadata and the infinity-distance fallback are persistent
  inline status, not transient toast-only feedback.
- A reset returns the whole group to Off and restores component defaults.
- No correction-strength sliders appear in this release.

Preview the image at the existing last valid frame during resolution and
rebuild. The panel may show "Preparing corrected preview…", but it must not
introduce a blank intermediate state.

### 10.3 CLI coverage

The CLI is the simplest end-to-end correctness surface. Add develop/export
options equivalent to:

```text
--lens-profile off|auto|<stable-profile-id>
--no-lens-distortion
--no-lens-vignetting
--no-lens-tca
```

`off` remains the default. If `auto` or an explicit ID is requested and cannot
resolve, fail before encoding output with camera/lens/cause in the error. Print
the resolved profile, requested/applied components, database provenance,
fallback distance, optics timing, and correction scale in the development
report.

Add a read-only profile listing/inspection path only if it is needed to obtain
stable IDs for CLI manual selection; avoid a broad profile-management command
in this feature.

## 11. Implementation sequence

Each step should land with its own focused tests. Keep the feature inaccessible
from the UI until the CPU pipeline and cache contracts are correct.

### Step 0 — dependency and corpus qualification

1. Pin the candidate Rust Lensfun version on a temporary branch/spike.
2. Complete the API, parity, MSRV, license, database, and thread-safety checks
   in section 7.1.
3. Inspect every available private Sony file's make/model/lens/focal/aperture
   metadata and record expected match states in a test fixture.
4. Confirm that files with missing lens metadata remain unresolved rather than
   matching by camera alone.
5. Decide whether the Rust dependency passes or whether the same owned API
   needs a native upstream adapter. Do not begin UI work before this decision.

Exit criterion: an approved dependency path and reproducible correction
coordinates for at least one profile containing all three components.

### Step 1 — owned optics crate and recipe/metadata types

1. Add `crates/optics` to the workspace and keep the third-party dependency in
   that crate only.
2. Define owned query, summary, match, plan, provenance, component, diagnostic,
   and error types.
3. Add a tiny in-memory XML database fixture for tests.
4. Add `OpticsAdjustments` and `LensProfileSelection` to recipe schema 7,
   validation, defaults, migration, and serialization tests.
5. Map positive `subject_distance` into `CaptureMetadata::focus_distance` and
   extend synthetic/private metadata fixtures.

Exit criterion: profile intent round-trips, old recipes remain visually off,
and no Lensfun type escapes `rohditor-optics`.

### Step 2 — deterministic database loading and matching

1. Implement bundled snapshot loading and `DatabaseProvenance`.
2. Generate stable logical profile IDs from canonical maker/model/mount facts.
3. Implement exact/ambiguous/missing states and deterministic candidate order.
4. Add focal-range and camera compatibility checks.
5. Cache query results without placing a lock in correction row loops.
6. Add tracing for database load and resolution, excluding private file paths.

Exit criterion: synthetic cases plus the private Sony/Tamron metadata resolve
as expected, and deliberately ambiguous metadata never auto-selects.

### Step 3 — CPU correction engine

1. Build immutable plans with component availability, interpolation inputs,
   auto scale, version, and content fingerprint.
2. Implement the cancellable in-place vignetting pass.
3. Implement checked cubic sampling and combined distortion/TCA remapping.
4. Use bounded row/per-worker scratch and Rayon without changing results by
   thread count.
5. Add tracing, exact memory estimates, and typed errors.
6. Benchmark vignette-only, distortion-only, TCA-only, and all-components on
   representative preview/full-frame dimensions.

Exit criterion: upstream parity vectors pass, synthetic image invariants pass,
and correction is deterministic for one versus multiple Rayon threads.

### Step 4 — core, cache, Source 1:1, export, and GPU handoff

1. Make `CpuPipeline` own/inject the optics service and update callers/tests.
2. Add the post-demosaic optics hook to preview reconstruction and the
   full-resolution base.
3. Extend timings, memory estimates, diagnostics, and pipeline errors.
4. Extend reconstructed/base provenance and preview cache keys.
5. Carry and verify optics provenance at GPU upload/render boundaries.
6. Verify Source 1:1, crop/orientation, histogram, picker, and export all use
   the corrected image.
7. Preserve last-frame handoff during every CPU optics rebuild.

Exit criterion: all render paths agree, stale or mismatched sources are
rejected, and optics-off output has no regression.

### Step 5 — desktop and CLI controls

1. Add the worker-side match/candidate event and document panel state.
2. Add `ui/optics.rs` with Off/Automatic/manual selection, component toggles,
   metadata, availability, and persistent error/fallback status.
3. Wire actions through `EditSession`, revisioning, undo/redo, reset, and
   newest-wins preview scheduling.
4. Add CLI recipe construction, strict resolution failure, reports, and parser
   tests.
5. Add desktop state tests for unique, partial, missing, and ambiguous matches.

Exit criterion: the UI and CLI can request the same recipe and identify the
same profile and applied components.

### Step 6 — qualification and release decision

1. Run all synthetic, workspace, ignored private-corpus, and host-GPU suites.
2. Compare corrected exports against upstream Lensfun or another trusted
   Lensfun client using straight-line, corner-brightness, and color-edge
   fixtures—not only screenshots.
3. Review every private-corpus frame at 100% and fit view for straight-line
   geometry, corner exposure, TCA, edge sampling, crop, and unexpected blur.
4. Measure first correction, cached downstream edit, Source 1:1, and export
   time plus peak memory.
5. Add required third-party license/attribution assets before distribution.
6. Keep the recipe default Off. Make automatic-on-by-default a later explicit
   product decision supported by broader camera/lens evidence.

Exit criterion: all acceptance criteria below pass and the remaining
limitations are visible to users.

## 12. Test and validation matrix

### 12.1 `rohditor-optics` unit tests

- Database load succeeds for the bundled snapshot and tiny test XML.
- Malformed XML/profile entries produce bounded typed errors.
- Exact camera/lens match, clean-name match, missing maker, no match, and
  multiple match states.
- Stable candidate ordering and profile IDs.
- Focal-range rejection and small metadata rounding tolerance.
- Explicit ID resolution and incompatible-camera rejection.
- Component availability at prime and interpolated zoom focal lengths.
- Aperture/distance interpolation and the `1000 m` fallback marker.
- Coordinate parity against upstream Lensfun for every supported model family.
- Identity/no-component correction changes no samples.
- Constant-color vignette fixture produces expected radial gain without hue
  shift.
- Non-square asymmetric grid fixtures reveal transposed axes or wrong center.
- Red/green/blue edge fixture verifies per-channel TCA coordinates.
- Distortion fixture verifies straight-line displacement direction and scale.
- Combined distortion+TCA performs one interpolation and matches composed
  reference coordinates.
- Odd dimensions, non-tight row strides, 1xN/too-small rejection, overflow,
  invalid values, cancellation, and out-of-bounds prevention.
- Negative and over-range linear samples are preserved rather than clipped.
- One-thread and multi-thread output is bit-identical.

### 12.2 Recipe, RAW, and core tests

- Schema 7 defaults/round-trip and version 1-6 migration to optics Off.
- Focus-distance mapping accepts positive rationals and rejects zero/invalid
  values for optics use.
- Off path is byte-for-byte identical to the pre-feature CPU reference
  fixtures and does not allocate an optics output buffer.
- ActiveArea plus enabled optics returns the documented error.
- Stage order is demosaic -> optics -> preview reduction/color conversion.
- Direct preview equals split reconstruction/base rendering.
- A downscaled corrected full-resolution reference agrees with the normal
  corrected preview within a documented per-channel tolerance.
- Source 1:1 and export use the same plan fingerprint as normal preview.
- Orientation and crop operate on corrected output without swapped dimensions.
- Each profile/toggle/metadata/database/version change invalidates
  reconstructed cache; light/color/crop changes do not.
- WB reuse remains valid for WB-independent highlight modes and invalid for
  Clip under the existing rules.
- Timings and memory estimates include the optics stage and peak buffers.

### 12.3 Desktop/GPU/CLI tests

- Unique match, ambiguous chooser, no match, partial component coverage, and
  database failure panel states.
- Undo/redo/reset for profile and each component toggle.
- Rapid toggles publish only the newest revision.
- Old CPU/GPU frame remains visible while corrected reconstruction is pending.
- Stale GPU upload and optics-provenance mismatch are rejected.
- CPU and hardware-GPU downstream display parity from the same corrected
  camera-native source.
- CLI `off`, `auto`, explicit ID, component-disable flags, and strict failures.
- CLI and desktop resolve the same fixture metadata to the same stable ID.

### 12.4 Real corpus and quality checks

Use at least:

- the Sony Alpha 6400/Tamron 17-70mm private files already covered by RAW
  metadata fixtures;
- one file near each available zoom endpoint and one intermediate focal;
- wide-open and stopped-down aperture samples when available;
- strong straight lines near image edges for distortion;
- flat/low-texture corners for vignetting;
- high-contrast radial edges near corners for TCA; and
- a missing-lens-metadata file that must remain unresolved.

Record objective metrics where possible:

- line curvature or displacement before/after distortion correction;
- corner-to-center luminance ratio before/after vignetting correction;
- red/blue edge displacement relative to green before/after TCA;
- preview versus downscaled export difference;
- full-frame peak memory;
- uncached correction and cached downstream-edit p50/p95 time; and
- GPU upload/render timing after the corrected source is ready.

Do not claim Lensfun correction quality from synthetic tests alone. Do not
claim GPU acceleration for the CPU lens stage, and do not treat a CPU Vulkan
rasterizer as host-GPU validation.

## 13. Acceptance criteria

The first production release is complete only when:

- the Lensfun dependency/database path passes the qualification gate;
- existing recipes migrate with optics off and existing output remains
  unchanged;
- a supported exact camera/lens pair resolves without user intervention;
- ambiguous or fuzzy-only metadata never applies a profile automatically;
- manual selection persists as a stable profile ID and fails clearly if that
  profile later disappears;
- distortion, vignetting, and TCA can be toggled independently and unavailable
  components are accurately reported;
- correction runs in linear camera RGB before preview reduction and color
  conversion;
- output dimensions, late orientation, and normalized crop semantics remain
  stable;
- preview, Source 1:1, histogram/pickers, and export all reflect the same
  profile plan;
- all pixel-producing profile semantics participate in reconstructed cache and
  GPU-source provenance;
- rapid edits remain cancellable/newest-wins and never flash a blank image;
- the CLI fails rather than silently exporting uncorrected pixels after an
  explicitly requested unresolved profile;
- timings and memory diagnostics identify optics separately;
- synthetic, private-corpus, and host-GPU handoff tests pass; and
- third-party code/data licensing and attribution are included in packaging.

Run at minimum before handoff:

```sh
./scripts/check.sh
cargo test --release --workspace --tests -- --ignored --nocapture
cargo test --release -p rohditor-gpu -- --ignored --nocapture --test-threads=1
cargo bench -p rohditor-optics --bench correction
```

The ignored workspace suite requires the private corpus, and the GPU suite must
identify the real host adapter before it counts as hardware validation.

## 14. Risks and mitigations

| Risk | Mitigation |
| --- | --- |
| Rust Lensfun port is still immature or changes API | Pin an exact reviewed version, hide it fully behind `rohditor-optics`, keep upstream parity fixtures, and retain a native-adapter fallback. |
| Fuzzy metadata selects the wrong lens | Exact normalized matching only for auto-apply; mount/focal compatibility checks; user choice for all ambiguous/fuzzy-only results. |
| Bundled database becomes stale | Report snapshot provenance, support explicit profile failure, update only through reviewed Rohditor releases initially, and add user overrides later. |
| Database update changes pixels under the same profile name | Put actual plan content plus algorithm version in provenance/cache keys; keep regression exports and avoid runtime auto-updates. |
| Full-frame cubic remap makes first preview slow | Correct asynchronously, retain the old frame, cache at reconstructed level, benchmark first, and optimize preview separately only against the full reference. |
| Remap adds blur or edge artifacts | Combine geometry and TCA into one interpolation, use a fixed high-quality kernel, validate its full footprint, and inspect high-contrast real edges at 100%. |
| Auto-scale crops more than expected | Display the applied scale in diagnostics, test wide-angle/fisheye extremes, and defer unsupported projection profiles rather than over-cropping silently. |
| Vignetting over-brightens corners or amplifies noise | Apply only calibrated gains in linear space, report fallback distance/coverage, preserve HDR values, and review flat-field/corner-noise samples. |
| RAW crop differs from the raster assumed by Lensfun | Support Recommended crop first and reject ActiveArea with optics enabled until an optical-center mapping is proven. |
| GPU shows stale uncorrected pixels after a profile edit | Add optics provenance to reconstructed cache and GPU source, revision-check uploads, and preserve/replace the visible frame atomically. |
| Embedded corrections are applied twice later | Keep embedded metadata out of v1; when added, make source resolution exclusive and record the selected source in recipe/provenance. |
| Licensing/attribution is incomplete | Audit the pinned crate and bundled XML, ship LGPL and CC BY-SA notices/attribution, and record exact snapshot provenance. |

Rollback is straightforward because the feature is recipe-default Off and
isolated behind the optics crate/service. If correction quality or dependency
qualification fails late, keep schema deserialization and diagnostics but
disable non-Off selection; do not remove or reinterpret saved explicit user
intent silently.

## 15. Later extensions

These are follow-ups, not hidden requirements for the first release.

### 15.1 Embedded manufacturer profiles

Add only after rawler/Rohditor can expose a validated, format-independent
model. `raw` should own normalized immutable metadata; `optics` should turn it
into the same `LensCorrectionPlan`/diagnostic boundary used by Lensfun. Extend
recipe selection to distinguish `Automatic`, `Embedded`, and `Lensfun`, with an
explicit precedence policy and double-correction tests. Do not let `core`
parse Sony/Fujifilm/DNG maker-note bytes.

### 15.2 User database overrides and updates

Load reviewed user XML from an explicit Rohditor XDG directory after the
bundled snapshot, with deterministic override rules, schema validation,
per-file diagnostics, cache eviction, and a no-network processing path. A
download/update UI needs authenticity, atomic installation, rollback, and
license/provenance handling.

### 15.3 Manual optics controls

Add separate manual distortion, vignette, and CA adjustments only after
profile corrections are stable. Keep manual optical vignetting distinct from
creative vignette and manual TCA distinct from defringe. Define whether manual
values supplement or replace a profile and include that composition order in
the recipe and cache key.

### 15.4 Contribution workflow

For genuinely missing lenses, link users to Lensfun's calibration/contribution
workflow rather than maintaining an incompatible Rohditor-only database. A
future diagnostic export may package the non-private metadata needed to report
a missing camera/lens, but it must never upload RAW files without explicit
user action.

### 15.5 GPU correction

Consider WGSL correction only if measured CPU reconstruction latency remains a
problem after caching and preview-specific optimization. A GPU implementation
must use the same resolved calibration plan, component composition, scale,
sampling kernel, edge policy, and parity fixtures as the CPU reference. It must
not make full-resolution export dependent on GPU availability.

## 16. References

- [Lensfun library architecture](https://lensfun.github.io/manual/latest/basearch.html): database/camera/lens/modifier model, matching workflow, ambiguity handling, and early linear sensor-space requirement.
- [Lensfun modifier API](https://lensfun.github.io/manual/latest/structlfModifier.html): correction order, combined geometry/TCA coordinates, and the application's responsibility for pixel interpolation.
- [Lensfun database loading and override behavior](https://github.com/lensfun/lensfun/blob/master/docs/manual-main.txt): standard locations, timestamps, and later-definition precedence.
- [Lensfun supported camera/lens list](https://lensfun.github.io/lenslist/): current upstream development coverage, including Sony Alpha 6400 and Tamron 17-70mm F/2.8 Di III-A VC RXD.
- [Lensfun calibration workflow](https://lensfun.github.io/calibration/): upstream contribution path for missing profiles.
- [Lensfun licensing](https://lensfun.github.io/manual/latest/license.html): LGPL-3.0 library and CC BY-SA 3.0 database terms.
- [Pure-Rust `lensfun` crate 0.7.0 overview](https://docs.rs/crate/lensfun/0.7.0): bundled database, correction surface, parity claims, dependencies, status, and licensing.
- [Pure-Rust `lensfun` API documentation](https://docs.rs/lensfun/0.7.0/lensfun/): owned `Database`/`Modifier` API and the current pre-1.0 maturity warning.
