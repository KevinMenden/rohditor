# Camera profiles: first implementation

**Status:** proposed implementation plan

**Planning audit:** 2026-09-07

**Canonical scope:** camera input colour profiles only

This plan adds a small but real camera-profile feature without conflating input
characterisation, creative camera looks, monitor profiles, or lens correction.
The first implementation keeps Rohditor's current decoder-derived matrix as the
default and adds opt-in, matrix-based DNG Camera Profile (`.dcp`) import and
selection.

The feature is intentionally narrower than full DCP support. A profile is
accepted only when Rohditor can account for every profile component that would
change its rendered pixels. Unsupported look tables, hue/saturation maps, tone
curves, gain tables, and three-illuminant profiles produce a clear import error;
they are never silently ignored.

## 1. Decision summary

Implement one vertical slice with these user-visible choices:

```text
Camera profile
  Automatic (RAW / decoder matrix)    default
  <compatible imported matrix DCPs>

  [Import DCP...]
```

The main decisions are:

- Keep `Automatic` byte-for-byte compatible with the current pipeline. For a
  normal Sony ARW this is rawler's camera database matrix; for a DNG it is the
  matrix rawler decoded from the DNG.
- Support user-provided, three-channel, matrix-based `.dcp` files. Do not add
  LibRaw: Rohditor already receives equivalent matrix metadata from rawler.
- Use `ColorMatrix1`/`ColorMatrix2` and their calibration illuminants. Use the
  matching `ForwardMatrix` when present. Version 1 selects D65 first, then D50,
  then Standard Light A, matching the current baseline; it does not interpolate
  dual-illuminant matrices.
- Store the small validated matrix payload in the edit recipe, not an external
  path. This keeps an edit reproducible and lets export use exactly the previewed
  transform even if the original DCP later moves or changes.
- Also install imported DCP files in Rohditor's XDG configuration directory so
  they remain available as choices for later documents.
- Treat profile selection as an input-colour edit. It is undoable, resettable,
  revisioned, cancellable, and used identically by preview, Source 1:1, GPU
  preview, and export.
- Reuse the retained camera-native reconstruction when a profile changes. A
  profile normally invalidates only the colour-converted and adjusted cache
  levels. The one exception is `Clip` combined with Temperature/Tint white
  balance, because that highlight operation derives its channel ceilings from
  profile-dependent white-balance gains.
- Keep camera looks out of the first implementation. Do not add an unused
  `CameraLook` abstraction yet; add it when Rohditor actually implements a DCP
  hue/saturation map, look table, or another creative look.

This is preferable to an `Automatic / Embedded / Camera matrix` menu with only
one effective choice on most files. It preserves the existing automatic path
while giving the user a meaningful, standards-based alternative.

## 2. Current Rohditor state

Rohditor already has the core of a matrix camera profile, but it is implicit and
not selectable:

1. `crates/raw/src/rawler_adapter.rs` maps rawler's `RawImage::color_matrix`
   into `RawFileInfo::color_matrices` and retains the legacy
   `RawImage::xyz_to_cam` fallback.
2. `crates/core/src/color.rs::camera_color_transform` chooses D65, then D50,
   then Standard Light A, normalises and inverts the XYZ-to-camera matrix,
   Bradford-adapts it to D65 where necessary, and composes it with the
   XYZ-to-linear-Rec.2020 transform.
3. White balance is applied to camera-native RGB before that transform.
4. `ReconstructedPreview` retains reduced camera-native RGB, while
   `DemosaicedBase` retains the white-balanced, converted Rec.2020 result.
5. The desktop cache keys the converted base by white balance but not by a
   profile identity, because there is currently no profile choice.
6. The GPU upload stores the active camera-to-XYZ and camera-to-Rec.2020
   matrices in `GpuPreviewSource`, even though the uploaded texture itself is
   camera-native.

That last point is the main refactor required by this feature. The selected
transform must no longer be inseparable from camera-native pixels that are
otherwise valid for another profile.

The production decoder is rawler 0.7.2, not LibRaw. LibRaw is used only by the
CLI verification utility. Adding LibRaw to the processing path would duplicate
the current decoder boundary, create a second camera database, and complicate
provenance without being necessary for this first feature.

## 3. Terminology and processing contract

Use these names consistently:

- **Automatic calibration:** matrix metadata selected from `RawFileInfo`.
- **Matrix camera profile:** a validated set of one or two illuminant-labelled
  3x3 transforms imported from a DCP.
- **Resolved camera transform:** the single camera-to-XYZ-D65 and
  camera-to-linear-Rec.2020 transform selected for one render.
- **Camera look:** a creative or appearance transform such as a DCP
  hue/saturation map, look table, or tone curve. It is out of scope.

The first implementation keeps this stage order:

```text
immutable RawFrame
  -> black/white normalisation
  -> RAW highlight operation
  -> demosaic to camera-native linear RGB
  -> preview area reduction when applicable
  -> white balance
  -> selected camera input profile
  -> linear Rec.2020/D65 working RGB
  -> Light and Color edits
  -> output conversion
```

The selected camera profile is not an output ICC profile, display profile,
preset, LUT, or lens profile. It must not apply a transfer curve or creative
contrast in this version.

## 4. MVP DCP subset

### 4.1 Accepted profile data

The parser accepts classic TIFF-based DCP files with either byte order and a
strict file-size limit of 16 MiB. It extracts:

- `ProfileName`;
- the unique camera model restriction;
- `ProfileCopyright`, when present;
- `CalibrationIlluminant1` and `ColorMatrix1`;
- the complete second calibration pair, when present;
- `ForwardMatrix1` and `ForwardMatrix2`, when present for the corresponding
  calibration; and
- profile embed/signature metadata for diagnostics only.

Version 1 supports:

- exactly three camera colour channels;
- finite 3x3 matrices with valid rational denominators;
- D65, D50, and Standard Light A calibrations; and
- one or two calibration entries, with a complete illuminant/matrix pairing.

When more than one supported calibration exists, evaluation uses this stable
priority:

```text
D65 -> D50 -> Standard Light A
```

This deliberately matches Rohditor's existing automatic matrix selection. The
selected illuminant is displayed in diagnostics so a dual-illuminant profile is
not mistaken for an interpolated result.

### 4.2 Rejected profile data

Import fails with a specific reason for:

- missing profile name or camera-model restriction;
- a camera-model restriction that does not match the open RAW;
- malformed TIFF offsets, counts, types, strings, or rational values;
- non-RGB or non-3x3 matrices;
- no supported calibration illuminant;
- incomplete paired calibration tags;
- three-illuminant or custom-illuminant profiles;
- `CameraCalibration`/`ReductionMatrix` requirements that the v1 evaluator
  cannot honour;
- profile hue/saturation maps;
- profile look tables;
- profile tone curves;
- baseline-exposure/default-black changes or profile gain tables; and
- any other recognised pixel-producing profile component not implemented by
  the evaluator.

Unknown metadata-only tags may be preserved or ignored. An unknown tag whose
effect cannot be classified must fail closed until it is understood.

The UI error should say, for example:

```text
Could not import Camera Portrait.dcp: this profile uses a hue/saturation map,
which Rohditor's matrix-only DCP implementation does not support yet.
```

Do not import such a profile under a “base colour only” label. That would make
profiles which differ primarily in their look data appear selectable but render
the same or incorrectly.

### 4.3 Matrix evaluation

For a selected calibration with a `ForwardMatrix`, validate that it is a usable
camera-to-XYZ-D50 transform, apply it to white-balanced camera RGB, and Bradford
adapt XYZ D50 to XYZ D65 before composing with Rec.2020.

Without a `ForwardMatrix`, reuse the current `ColorMatrix` path:

1. interpret `ColorMatrix` as XYZ-at-calibration-illuminant to camera RGB;
2. normalise its rows against the calibration white;
3. reject a singular or numerically unusable matrix;
4. invert it to camera RGB to source XYZ;
5. Bradford-adapt source XYZ to D65; and
6. compose it with `XYZ_D65_TO_LINEAR_REC2020`.

The existing automatic evaluator must remain a separate compatibility path in
version 1 so a refactor cannot silently change current output. Shared matrix
validation and composition helpers may be reused once equivalence tests prove
the automatic result is unchanged.

Add a public constant such as `CAMERA_PROFILE_EVALUATOR_VERSION: u8 = 1` and
include it in cache/provenance identity. Changing illuminant selection,
normalisation, ForwardMatrix handling, or adaptation later requires a version
bump and refreshed image evidence.

## 5. Data model and recipe

### 5.1 New focused crate

Create `crates/camera-profile` (`rohditor-camera-profile`) with this initial
layout:

```text
crates/camera-profile/
  src/
    lib.rs       public profile types, validation, errors, evaluator version
    dcp.rs       private TIFF/DCP parsing adapter
  tests/
    dcp.rs       generated valid and malformed profile fixtures
```

The crate owns profile-file parsing and static profile validation. It must not
depend on `core`, `gpu`, or either application. It may use rawler's generic TIFF
reader behind `dcp.rs`; rawler is already pinned and already parses the same DNG
tags. No rawler type should escape the crate's public API.

Keep the public representation small:

```rust
pub struct MatrixCameraProfile {
    pub format_version: u8,
    pub source_sha256: String,
    pub name: String,
    pub camera_model: String,
    pub copyright: Option<String>,
    pub calibrations: Vec<MatrixCalibration>,
}

pub struct MatrixCalibration {
    pub illuminant: CalibrationIlluminant,
    pub xyz_to_camera: [[f32; 3]; 3],
    pub forward_camera_to_xyz_d50: Option<[[f32; 3]; 3]>,
}
```

This is illustrative rather than a requirement to expose mutable public fields.
The implementation should use constructors/accessors if they make invalid
states harder to create. Cap all strings and calibration counts during parsing
and deserialisation.

### 5.2 Recipe selection

Add the following concept under `ColorAdjustments`:

```rust
pub enum CameraProfileSelection {
    Automatic,
    Matrix(MatrixCameraProfile),
}
```

The serialised shape should remain readable:

```json
{
  "camera_profile": {
    "mode": "matrix",
    "profile": {
      "format_version": 1,
      "source_sha256": "<lowercase sha256>",
      "name": "Studio Neutral",
      "camera_model": "Sony ILCE-6400",
      "calibrations": [
        {
          "illuminant": "d65",
          "xyz_to_camera": [[0.7, -0.2, -0.1], [-0.4, 1.2, 0.2], [-0.1, 0.2, 0.6]],
          "forward_camera_to_xyz_d50": null
        }
      ]
    }
  }
}
```

Do not serialise an absolute DCP path. The embedded payload is small, makes the
recipe self-contained, and ensures an export does not change after a profile
file is replaced.

The source SHA-256 is provenance and registry identity, not sufficient cache
identity by itself. Deserialised recipes can be edited independently of the
original DCP, so cache keys must include the actual selected matrix bits,
illuminants, and evaluator version.

Bump `EDIT_RECIPE_SCHEMA_VERSION` to 8. The known v1-v6 recipe migrations should
receive `CameraProfileSelection::Automatic`; this is a small continuation of
the existing migration code, not a promise of general pre-release backwards
compatibility. Validate bounded strings, hash syntax, calibration count,
finite matrix values, and all structural invariants at the recipe boundary.
Source-camera compatibility remains a core render check because recipe
validation alone does not know the active RAW.

## 6. RAW metadata and provenance

Extend `CameraColorMatrix` with an explicit origin:

```text
EmbeddedDng
DecoderDatabase
LegacyDecoderFallback
```

For the pinned rawler implementation, DNG `color_matrix` values are read from
the DNG tags, while native Sony values come from rawler's camera definition.
Map that distinction inside `rawler_adapter.rs`; no application should infer it
from a display string.

The automatic path should report:

- origin;
- selected illuminant;
- the camera make/model used for matching; and
- evaluator version.

An explicitly selected DCP reports its name, source digest, model restriction,
selected illuminant, whether a ForwardMatrix was used, and evaluator version.
Do not describe either source as “correct” or “accurate” without measurement.

## 7. Core pipeline changes

### 7.1 Separate calibration from a selected transform

Introduce a compact immutable core type, for example `CameraCalibration`, built
from `RawFileInfo`. It contains only the camera identity, as-shot WB values, and
automatic matrix candidates required to resolve white balance and a transform.

Change `ReconstructedPreview` to retain this calibration rather than one
already-selected `CameraColorTransform`. Camera-native pixels do not have a
camera profile applied, so attaching the current profile transform to that
cache level gives it the wrong identity.

Add one resolver used by every processing path:

```text
resolve_camera_colour(calibration, recipe.color.camera_profile, white_balance)
  -> ResolvedCameraColour {
       profile_provenance,
       white_balance_gains,
       camera_to_xyz_d65,
       camera_to_linear_rec2020,
     }
```

The automatic branch calls the existing evaluator. The matrix-profile branch
checks camera compatibility and evaluates the selected DCP calibration. CPU
preview, Source 1:1, export, and GPU parameter construction must all call this
resolver rather than duplicate precedence or matrix maths.

### 7.2 Retained-base provenance

Add the resolved profile key to `DemosaicedBase`. Its recipe-compatibility check
must require both the same white balance and the same pixel-producing profile
payload/evaluator version.

Full-resolution render and export resolve the profile before applying white
balance and camera conversion. They must use the same error messages and matrix
path as preview.

### 7.3 White-balance interaction

As-shot and manual-relative white balance use decoder multipliers and do not
depend on the selected camera matrix. Temperature/Tint converts a requested XYZ
white through `camera_to_xyz_d65.inverse()`, so its gains do depend on the
selected profile.

This has one early-pipeline consequence:

```text
Highlight method     Profile change may reuse reconstructed camera RGB?
Off                  yes
Local ratios         yes
Opposed              yes
Clip + As shot       yes
Clip + Manual RGB    yes
Clip + Temp/Tint     no
```

Encode this rule once and share it between cache keys, reconstructed-source
matching, and GPU base matching. Do not conservatively rebuild demosaic for all
profile changes merely to avoid expressing the dependency.

## 8. Desktop cache and asynchronous behaviour

Add a `CameraProfileKey` to `DemosaicedBaseKey`. It contains:

- automatic versus matrix selection;
- actual matrix `f32::to_bits()` values;
- illuminant and ForwardMatrix presence/content; and
- `CAMERA_PROFILE_EVALUATOR_VERSION`.

Do not use only a profile name, path, or source digest.

For `ReconstructedCameraRgbKey`, include the profile key only for the
`Clip + TemperatureTint` case described above. Tests must demonstrate the
expected cache hit/miss pattern for every row of the table.

A profile selection is a discrete `EditSession` edit:

- one selection equals one undo step;
- revision increments exactly once;
- redo restores the complete embedded profile payload;
- Reset All returns to Automatic; and
- stale preview/GPU events from the previous selection are rejected by the
  existing document/revision/sequence checks.

Keep the last valid visible preview while a profile-dependent result is being
prepared. An invalid profile must not clear the viewport or replace the last
valid recipe.

## 9. GPU boundary

The GPU source texture contains camera-native RGB and should remain reusable
across ordinary profile changes.

Refactor `GpuPreviewUpload`/`GpuPreviewSource` so the uploaded source owns the
compact `CameraCalibration` and RAW-highlight provenance, not a permanently
selected camera transform. On each supported recipe render:

1. resolve the profile and white-balance gains with the shared CPU-side core
   resolver;
2. write the resulting gains and camera-to-Rec.2020 matrix to the existing
   uniform buffer;
3. reuse the camera-native source texture and output textures when dimensions
   permit; and
4. reject the source only when RAW highlight provenance is incompatible,
   including `Clip + TemperatureTint` profile changes.

No DCP parsing, file I/O, or profile lookup belongs in WGSL. The shader already
accepts white-balance gains and a 3x3 camera transform; the work is chiefly
moving profile resolution out of source-upload identity.

CPU output is the correctness reference. Existing encoded-sRGB parity
tolerances remain the first acceptance limit; profile coverage must include
negative and over-range camera samples rather than only `[0, 1]` colours.

## 10. Profile installation and UI

### 10.1 Registry

Add an application-level `camera_profiles.rs` module. It scans:

```text
$XDG_CONFIG_HOME/rohditor/camera-profiles/
```

with the existing home-directory fallback used by `storage.rs`. Imported files
are validated before installation, named `<sha256>.dcp`, and written with the
existing transactional replacement helper. Duplicate contents are deduplicated
by digest; duplicate display names remain distinguishable by a short digest in
diagnostics/tooltips.

Registry scans are deterministic and bounded. One bad profile logs/skips that
file and reports a concise non-fatal warning; it must not prevent the desktop
from opening. If the configuration directory cannot be written, the newly
parsed profile may still be used for the current document because its validated
payload is embedded in the recipe, with a warning that it was not installed.

Do not scan Adobe application directories, download profiles, or bundle
third-party DCP files in this version.

### 10.2 Adjustment panel

Place Camera profile at the top of the existing Color section, before white
balance:

```text
Color

Camera profile   [Automatic (RAW / decoder matrix) v]
                 Rawler camera matrix · D65
                 [Import DCP...]

White balance    [As shot v]
...
```

Only compatible installed profiles appear as selectable choices. The detail
line changes to profile name, origin, and selected illuminant. A tooltip may
show the full model restriction and digest. Avoid a profile-strength slider;
an input profile is not an effect.

Importing a valid compatible DCP installs it and selects it as one discrete
edit. Importing an invalid or incompatible file leaves the current selection
and preview untouched.

### 10.3 CLI

Add to `rohditor-cli develop`:

```text
--camera-profile <PROFILE.dcp>
```

The CLI parses and validates the file, embeds the same matrix profile in the
recipe, and uses the common CPU pipeline. It does not install the profile into
the desktop registry. Default omission means Automatic and must preserve the
current output.

Add the resolved profile provenance to development tracing. `inspect` may
report automatic matrix origins through the extended `RawFileInfo`; it should
not claim which external profile would be selected.

## 11. Implementation sequence

### Phase 1 — parser fixtures and profile crate

1. Add generated little-endian and big-endian matrix-DCP fixtures whose matrix
   values can be checked by hand. Do not commit proprietary Adobe profiles.
2. Prove rawler's generic TIFF reader can safely expose every required tag from
   those fixtures. Keep that dependency behind `dcp.rs`.
3. Implement bounded parsing, typed errors, SHA-256 provenance, matrix/profile
   validation, and explicit unsupported-tag rejection.
4. Add malformed offset/count/type/denominator/string/matrix tests and fuzz-like
   short/truncated byte cases. No parser panic is acceptable.

**Gate:** a matrix-only DCP round-trips into the intended typed payload, and
every recognised unsupported pixel-producing tag fails with its own error.

### Phase 2 — recipe and core colour resolver

1. Add profile selection to `ColorAdjustments`, bump recipe schema 7, and cover
   default, migration, JSON round-trip, bounds, and invalid matrices.
2. Add raw matrix origins and the compact `CameraCalibration` type.
3. Refactor automatic resolution without changing its numerical output.
4. Add the selected DCP evaluator, ForwardMatrix path, camera-model matching,
   provenance, and evaluator version.
5. Use the resolver in combined render, split preview, Source 1:1, and export.

**Gate:** Automatic produces identical pixels to a pre-change golden fixture;
a hand-calculated DCP fixture produces the expected linear Rec.2020 values in
all entry points.

### Phase 3 — cache and GPU source reuse

1. Add exact profile payload identity to converted/adjusted cache keys.
2. Encode the `Clip + TemperatureTint` reconstruction dependency and test all
   highlight/WB combinations.
3. Move selected profile matrices out of permanent GPU upload identity.
4. Resolve gains/matrix per GPU recipe and preserve source/output texture reuse.
5. Extend GPU base-mismatch, parity, stale-event, and no-black-frame tests.

**Gate:** switching profiles normally records a reconstructed-cache hit and no
camera-native texture upload, while the Clip/Temperature exception rebuilds and
never reuses incorrect pixels.

### Phase 4 — desktop registry, UI, and CLI

1. Add bounded XDG profile scanning and transactional import.
2. Add the compatible-profile dropdown, detail text, import action, errors,
   undo/redo, reset, and revision handling.
3. Add `--camera-profile` to CLI development.
4. Surface the same resolved profile provenance in desktop diagnostics and CLI
   tracing.

**Gate:** one imported profile can be selected, previewed, undone/redone,
exported, rediscovered after desktop restart, and used by the CLI with the same
CPU result.

### Phase 5 — real image validation

1. Use the private Sony ILCE-6400 RAW corpus with a legally usable profile made
   for that exact model. Keep the DCP outside the repository unless its licence
   explicitly permits redistribution.
2. Compare Automatic and the imported profile at fit preview, Source 1:1, and
   16-bit export. Include neutrals, saturated colours, skin tones if available,
   and difficult highlights. Do not call a profile better based on one scene.
3. Run the ignored private workspace suite and real AMD GPU suite. A software
   Vulkan adapter does not establish hardware parity or source reuse.
4. Record profile-switch cache hits, texture reuse, CPU/GPU maximum code error,
   export equality, and any colour clipping regressions.

**Gate:** the imported result is deterministic, CPU preview/export agree, GPU
meets the existing parity tolerance, and no profile switch shows stale or blank
intermediate output.

## 12. Test matrix

### Profile parser and validation

- little- and big-endian TIFF/DCP;
- exact 3x3 signed rational parsing;
- D65 selection from A + D65 calibrations;
- D50/A fallback and Bradford adaptation;
- paired ForwardMatrix use;
- truncated header/IFD, offset overflow, count overflow, zero denominator;
- NaN/infinity after conversion, singular matrix, oversized file/string;
- missing/mismatched model restriction;
- two files with the same name but different digest;
- each explicitly unsupported pixel-producing tag; and
- deterministic parse errors with no panic.

### Recipe and colour correctness

- Automatic is the default and schema migration result;
- profile payload JSON round-trip and invalid-payload rejection;
- camera-model matching against raw and cleaned make/model forms;
- Automatic output regression at exact pixels;
- selected ColorMatrix and ForwardMatrix hand-calculated transforms;
- neutral/reference-white mapping;
- D50/A adaptation to D65;
- Temperature/Tint gains change with the selected profile;
- As-shot/manual gains remain profile-independent;
- negative, zero, and over-range camera-native samples remain finite; and
- one-thread/multi-thread deterministic output.

### Cache, desktop, and GPU

- profile change misses demosaiced/adjusted caches;
- ordinary profile change hits reconstructed camera RGB;
- Clip + Temperature/Tint profile change misses reconstructed camera RGB;
- all other highlight/WB combinations follow the dependency table;
- GPU camera-native source texture is reused for ordinary profile changes;
- GPU rejects only genuinely incompatible highlight provenance;
- CPU/GPU parity for Automatic and selected DCP, all orientations;
- stale old-profile events are discarded;
- visible frame remains installed while replacement is pending;
- selection/import/reset/undo/redo each have correct revision semantics;
- failed import leaves recipe, history, and preview unchanged;
- registry deduplication and restart discovery; and
- CLI and desktop CPU export equality.

## 13. Acceptance criteria

The first implementation is complete only when all of these are true:

- Automatic remains the default and preserves current output exactly.
- A compatible matrix-based DCP can be imported, selected, previewed, exported,
  undone/redone, and rediscovered after restart.
- Unsupported DCP rendering components are rejected explicitly; the UI and CLI
  never claim full DCP or camera-look support.
- The selected matrix payload is self-contained in the recipe and no absolute
  profile path affects reproducibility.
- Preview, Source 1:1, export, and GPU preview use the same resolved profile and
  white-balance semantics.
- Cache identity includes actual pixel-producing profile data and evaluator
  version.
- Camera-native preview/GPU sources are reused across profile changes except
  when Clip's profile-dependent Temperature/Tint gains require reconstruction.
- CPU/GPU parity and stale-result/no-blank-frame behaviour have focused tests.
- `./scripts/check.sh`, `git diff --check`, the ignored private workspace suite,
  and the ignored real-GPU suite pass when their required corpus/hardware is
  available.
- Documentation and diagnostics name the selected origin/illuminant honestly
  and make no unmeasured colour-accuracy claim.

## 14. Deliberately deferred

These are not part of the first implementation:

- DCP dual-/triple-illuminant interpolation;
- DCP hue/saturation maps, look tables, tone curves, baseline exposure, gain
  tables, or camera-matching looks;
- ICC input profiles;
- embedded non-matrix DNG profile processing or selection among extra embedded
  profiles;
- bundled Adobe, darktable, RawTherapee, or Rohditor camera-profile databases;
- automatic selection of an imported DCP instead of Automatic;
- online profile download/update;
- ColorChecker calibration/profile creation;
- display/monitor ICC management and wide-gamut output profiles; and
- lens profiles or lens correction.

The next camera-profile step should be chosen from observed rejected-profile
usage. If most desired profiles require DCP hue/saturation maps, implement that
as a tested input-calibration component before creative look tables. If users
need temperature-dependent accuracy first, implement dual-illuminant
interpolation and validate it against published DNG SDK vectors. Do not jump
directly to ICC, bundled databases, and camera looks at the same time.

## 15. References

- [Adobe DNG resources, specification, SDK, and Profile Editor](https://helpx.adobe.com/camera-raw/desktop/dng-and-file-formats/digital-negative.html)
- [Adobe DNG 1.7.1.0 specification](https://helpx.adobe.com/content/dam/help/en/photoshop/pdf/DNG_Spec_1_7_1_0.pdf)
- Current Rohditor boundaries: `crates/raw/src/rawler_adapter.rs`,
  `crates/core/src/color.rs`, `crates/core/src/pipeline.rs`,
  `apps/desktop/src/preview_cache.rs`, and `crates/gpu/src/preview.rs`
