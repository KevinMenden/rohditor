# Application settings plan

## Outcome

Add a small, durable settings system to the Rohditor desktop application and a
simple Settings dialog. The first persisted setting is the demosaic algorithm
used consistently for fit previews, Source 1:1 inspection, white-balance
sampling, and full-resolution export.

The user-facing choices in the first version are:

- Malvar-He-Cutler (`mhc`), the current default;
- Ratio-Corrected Demosaicing (`rcd`);
- AMaZE (`amaze`).

Bilinear demosaicing remains available through the existing development CLI
option, but is not presented as a normal editor preference.

Settings survive application restarts in:

```text
$XDG_CONFIG_HOME/rohditor/settings.json
```

When `XDG_CONFIG_HOME` is unset, the Linux fallback is:

```text
$HOME/.config/rohditor/settings.json
```

This is the same application configuration directory that currently contains
`session.json`, but the two files retain separate responsibilities.

## Recommendation

Implement the settings infrastructure now, but keep it in the desktop
application rather than adding `settings.rs` to `rohditor-core`.

`rohditor-core` owns deterministic processing contracts. It already exposes
`RenderOptions`, which is the correct input boundary for choices such as the
demosaic algorithm. Resolving XDG paths, loading JSON, remembering user
preferences, and presenting a Settings dialog are application concerns. If
the core crate owned the persisted settings object, a lower processing layer
would become coupled to the desktop application's storage format and future UI
preferences such as library folders.

No new crate is warranted for one desktop consumer. If another application
later needs the same persisted profile, extract a shared configuration crate
then, based on concrete shared requirements.

The first version should persist only the demosaic preference. This establishes
the file format, error handling, UI flow, and asynchronous invalidation rules
without prematurely turning developer controls into user preferences.

## Current-state findings

- `DemosaicAlgorithm` is owned by `rohditor-demosaic`; `RenderOptions` in
  `rohditor-core` carries it into the deterministic pipeline.
- The desktop app currently receives the demosaic algorithm from
  `--demosaic`, stores it in `RohditorApp::render_options`, and passes it to
  exports.
- The render worker captures one `PreviewOptions` value at construction time.
  Preview jobs do not currently carry their own processing options. Merely
  changing `RohditorApp::render_options` would therefore make exports use the
  new algorithm while previews continued to use the startup algorithm.
- Preview cache reconstruction keys already include the demosaic algorithm, so
  they will invalidate correctly once each preview job receives the current
  options.
- Preview tickets already contain a monotonic presentation sequence in
  addition to the edit-recipe revision. A settings change can use a new
  sequence to supersede older work without pretending that the preference is
  an undoable image edit.
- A resident GPU preview base can currently be reused based on white balance
  compatibility alone. Its demosaic algorithm must also match before it is
  reused after a setting change.
- `apps/desktop/src/session.rs` already resolves the Rohditor XDG configuration
  directory and tolerates missing or malformed session state. Its path logic
  can be shared, but the last browsed folder remains transient session state,
  not an application setting.

## Scope

### Included

- A versioned desktop settings model with the demosaic preference.
- Linux XDG configuration-path resolution shared with session persistence.
- Tolerant loading and transactional saving of `settings.json`.
- Startup precedence between a command-line override, the persisted setting,
  and the built-in default.
- An application-menu entry and a focused Settings dialog.
- Applying a new algorithm to the current document without changing its edit
  recipe or undo history.
- Correct cancellation, cache invalidation, GPU-base invalidation, and
  newest-result presentation.
- Consistent use of the active algorithm by preview, inspection, sampling, and
  export paths.

### Deferred

- Persistent library source folders. Do not add an unused path list to the
  schema now. When the library supports multiple roots, add the field with a
  migration based on that feature's real selection, missing-folder, and scan
  semantics.
- A catalog database, recursive watching, ratings, or collections.
- Persisting renderer or processor selection. Those choices affect startup and
  GPU initialization, so they need separate restart/fallback UX.
- Persisting developer diagnostics visibility.
- Configurable preview dimensions, cache limits, or other internal tuning.
- Presets, per-camera defaults, and per-document demosaic overrides.
- Synchronizing settings between concurrently running Rohditor instances. The
  last successful explicit save may win for the first version.

## Settings and session semantics

Keep two files below the same Rohditor configuration directory:

```text
rohditor/
  settings.json  # durable user choices
  session.json   # transient restore state, currently the last browsed folder
```

Do not merge them. A malformed session should not discard preferences, and
clearing recent/session state should not reset user choices. The future list of
configured library roots is likely durable configuration, while the last
active root or last selected image remains session state.

Add a small desktop-only storage/path module so `session.rs` and `settings.rs`
do not duplicate XDG resolution. Keep path lookup injectable or pure enough for
tests; tests must not write to the user's actual configuration directory.

No migration of the existing `session.json` location is part of this feature.

## Persisted model

Use an application-owned model, conceptually:

```text
AppSettings
  schema_version
  demosaic
```

The initial file is human-readable and stable:

```json
{
  "schema_version": 1,
  "demosaic": "mhc"
}
```

Design rules:

- Default to schema version 1 and MHC.
- Treat `mhc`, `rcd`, and `amaze` as stable persisted identifiers. UI labels
  may improve later without changing the stored values.
- Keep serialization details in the desktop settings module. Do not add disk
  paths or a settings container to `rohditor-core`.
- Validate the schema version explicitly. Missing fields in a supported schema
  receive defaults so new settings can be added compatibly.
- Ignore unknown fields within a supported schema so removing an older app
  version does not make the whole file unreadable.
- Treat malformed JSON, an unsupported schema version, or an unknown demosaic
  value as a non-fatal load failure. Start with defaults, log a warning, and
  make the problem visible in the Settings dialog.
- Do not rewrite a malformed or newer-version file merely because the app
  started. Only an explicit Apply action writes settings, preventing an older
  binary from silently destroying a newer file.

The runtime application should have one authoritative settings value. Derive
`RenderOptions` from it when creating a job rather than retaining a second
mutable copy that can drift out of sync.

## Storage behavior

Loading must never prevent Rohditor from launching. Return the selected
settings plus an optional load warning that the UI can show. A missing file is
the normal first-run case and produces no warning.

Saving should be transactional:

1. Serialize the complete settings object to pretty JSON.
2. Create the Rohditor configuration directory if needed.
3. Write and flush a uniquely named sibling temporary file.
4. Atomically rename it over `settings.json` on the supported Linux target.
5. Remove the temporary file after a failed write or rename where possible.

An interrupted save must leave either the old complete file or the new
complete file, never a truncated `settings.json`.

If persistence fails after the user clicks Apply, keep the new value active for
the current process and show a clear message that it could not be saved and
will not survive restart. Do not fail image processing because configuration
storage is unavailable.

## Startup and command-line precedence

Preserve `--demosaic` as a useful development override, with this precedence:

```text
explicit --demosaic > settings.json > AppSettings::default()
```

The existing CLI argument has a default value, so startup cannot distinguish
"not supplied" from an explicit `--demosaic mhc`. Change the desktop argument
to an optional value and resolve the effective setting after loading the file.

An explicit command-line override is for that run only and must not rewrite
`settings.json`. The Settings dialog displays the effective value. If the user
then deliberately chooses a value and clicks Apply, that action becomes the
new persisted preference.

This plan does not change the separate headless `rohditor-cli`; its explicit
command arguments remain independent of desktop preferences.

## Desktop UI

Add `Settings...` to the application menu. Do not add Settings as a third
Library/Develop workspace mode: it is an application-level preference and
should not discard or obscure the user's current workspace.

Use a compact centered Settings window with a staged draft:

```text
Settings

Processing
  Demosaic algorithm
    ( ) Malvar-He-Cutler (default)
    ( ) RCD
    ( ) AMaZE

  Used for preview, Source 1:1, and export.
  Changing this rebuilds the current developed image.

                           [Cancel] [Apply]
```

UI behavior:

- Opening the dialog copies the active settings into a draft.
- Cancel, Escape, or closing the window discards the draft.
- Apply is enabled only when the draft differs from the active settings.
- Applying updates the active setting immediately, attempts to persist the
  complete settings file, and closes the dialog on success.
- A save failure remains visible and understandable; the in-memory choice is
  still active for the current run.
- If no document is open, the new setting applies to the next document.
- The UI should not claim that one advanced algorithm is universally better
  until representative quality and performance measurements support such a
  ranking. Use neutral names, with MHC marked as the current default.

Keep `ui/settings.rs` presentation-only. It should receive a model/draft and
return commands; `app.rs` owns persistence, job scheduling, and document state.

## Applying a changed algorithm

Demosaicing is a processing option, not a non-destructive edit. Applying the
setting must not:

- mutate `EditRecipe`;
- increment the recipe revision;
- add an undo/redo entry;
- alter the immutable decoded RAW frame.

It must:

1. Install the new active settings value.
2. Allocate a new preview presentation sequence for the current document.
3. Cancel or supersede older preview work through the existing newest-wins
   scheduler.
4. Rebuild the current presentation using the new algorithm:
   - committed fit preview when in normal Develop;
   - uncropped crop-authoring preview when the crop tool is active;
   - full-resolution Source 1:1 when that mode is active.
5. Retain the currently visible texture until the replacement is ready, using
   the existing CPU/GPU handoff behavior.
6. Clear or refresh diagnostics and histogram validity just as a normal
   preview replacement does.

Introduce one application helper for "refresh the current presentation" so a
settings change does not accidentally bypass the crop-tool intent. The normal
preview queue currently distinguishes fit and Source 1:1 but does not itself
recreate the crop-authoring request.

An export already queued before the change keeps its immutable
`RenderOptions` snapshot and completes with the old algorithm. Exports queued
after Apply use the new algorithm. This matches the existing immutable export
job contract.

## Worker and cache changes

Make processing options explicit job data:

- Add `PreviewOptions` to `PreviewJob`.
- Pass the current snapshot into fit CPU preview, GPU-base preparation,
  crop-authoring preview, and Source 1:1 requests.
- Stop relying on one `PreviewOptions` captured by `worker_loop` at
  `RenderCoordinator` construction. The coordinator may no longer need a
  preview-options constructor argument.
- Keep white-balance sampling's existing explicit options snapshot.
- Keep export's existing explicit `RenderOptions` snapshot.

The cache already keys reconstructed data by algorithm. Verify that an
algorithm change retains the decoded RAW entry but invalidates reconstructed,
demosaiced, and adjusted entries.

Record the algorithm that produced each resident `GpuDocumentPreview` (or make
it queryable from its source metadata). Reuse a resident GPU base only when
both its algorithm and its other base-defining inputs match the new request.
Otherwise queue a new GPU-base preparation. Without this check, changing the
setting could relabel an old base without actually redemosaicing it.

Preview tickets do not need a new settings revision field for this first
setting because every Apply allocates a new presentation sequence and every
job carries immutable options. Tests should nevertheless prove that an older
result with the same recipe revision cannot replace the new selection.

## File/module changes

Expected ownership:

- `apps/desktop/src/settings.rs`
  - settings model, stable persisted demosaic representation, defaults,
    schema validation, load/save results, and focused tests;
- `apps/desktop/src/storage.rs` (or a similarly narrow name)
  - shared XDG config-directory resolution and transactional file writing;
- `apps/desktop/src/session.rs`
  - reuse the shared configuration-directory helper while retaining only
    session semantics;
- `apps/desktop/src/ui/settings.rs`
  - Settings window model, draft controls, and output commands;
- `apps/desktop/src/ui/toolbar.rs`
  - application-menu `Settings...` command;
- `apps/desktop/src/main.rs`
  - optional CLI override and startup precedence resolution;
- `apps/desktop/src/app.rs`
  - authoritative active settings, dialog state, Apply behavior, current
    presentation refresh, and derivation of render options;
- `apps/desktop/src/coordinator.rs`
  - immutable per-preview option snapshots;
- `apps/desktop/src/app/gpu.rs` and/or the GPU preview holder in `app.rs`
  - algorithm identity for safe resident-base reuse;
- `apps/desktop/src/preview_cache.rs`
  - likely no structural change, but add or retain coverage proving the
    existing algorithm key invalidates downstream cache layers.

Do not add a `core/src/settings.rs`, put XDG logic in a processing crate, or
make lower layers depend on the desktop app.

## Implementation phases

### 1. Settings model and storage

- Extract shared desktop configuration-directory resolution.
- Add the versioned settings model and stable JSON representation.
- Implement tolerant loading and transactional saving with injectable paths.
- Add pure startup-precedence resolution.
- Keep existing session behavior unchanged.

### 2. Immutable processing-option jobs

- Move preview options from worker construction into each preview job.
- Preserve explicit snapshots for sampling and export.
- Prove algorithm-specific cache invalidation and stale-result rejection.
- Add algorithm identity to the resident GPU base reuse check.

This phase should land before the visible control so the UI cannot create a
preview/export mismatch.

### 3. Settings dialog and application integration

- Add the menu command and presentation-only Settings window.
- Load and resolve settings at startup.
- Apply changes in memory and persist them.
- Refresh fit, crop-authoring, or Source 1:1 presentation as appropriate.
- Surface non-fatal load/save warnings without blocking editing.

### 4. End-to-end verification and polish

- Exercise restart persistence with an isolated XDG directory.
- Compare diagnostics and exports after each algorithm choice.
- Verify responsive dialog layout and keyboard close/apply behavior.
- Confirm no black/empty frame appears during a slow algorithm replacement.

## Automated tests

### Settings/storage

- Defaults select schema version 1 and MHC.
- `mhc`, `rcd`, and `amaze` round-trip with their exact stable JSON values.
- Missing new fields in schema version 1 receive defaults.
- Unknown fields in schema version 1 are tolerated.
- Missing files load defaults without a warning.
- Malformed JSON, unknown algorithm names, and unsupported schema versions load
  safe defaults with a warning and are not automatically overwritten.
- Saving creates the application directory and replaces an old complete file.
- A simulated write/rename failure preserves the previously valid file.
- XDG_CONFIG_HOME takes precedence over the HOME fallback, using injected
  environment values rather than process-global environment mutation in
  parallel tests.
- CLI override, persisted value, and built-in default follow the documented
  precedence.

### Processing integration

- Every preview job uses its own `PreviewOptions` snapshot.
- Switching algorithms reuses decoded RAW data but misses reconstructed and
  downstream cache entries.
- A newer settings-triggered presentation supersedes an in-flight result with
  the same edit-recipe revision.
- A resident GPU base created with one algorithm is not reused for another.
- Fit, crop-authoring, Source 1:1, white-balance sampling, and export all
  receive the active algorithm.
- An already queued export retains its original algorithm after settings
  change.
- Applying settings does not modify the recipe revision or undo/redo history.

### UI/application behavior

- The application-menu command opens the dialog.
- Cancel leaves active settings and disk state unchanged.
- Apply updates active settings and requests exactly one appropriate preview
  refresh when a decoded document is available.
- Applying with no document open performs no render work.
- A persistence failure keeps the runtime selection and exposes a useful
  warning.

## Manual verification

Use an isolated configuration directory so testing cannot modify the user's
real preferences:

```text
XDG_CONFIG_HOME=/tmp/rohditor-settings-test \
  cargo run -p rohditor-desktop --release -- testdata/private/<sample>.ARW
```

Verify:

1. First launch uses MHC and creates no settings file until Apply.
2. Selecting RCD and applying replaces the current preview without a blank
   frame; diagnostics report `rcd`.
3. Source 1:1 and export also report/use RCD.
4. Restart without `--demosaic` restores RCD.
5. Launch with `--demosaic amaze` uses AMaZE for that run without rewriting
   the saved RCD preference.
6. Applying MHC while the override run is active persists MHC deliberately.
7. A malformed `settings.json` does not prevent launch and produces a visible
   warning in Settings.
8. Applying during fit, Source 1:1, and crop-authoring states refreshes the
   correct presentation and rejects older completions.
9. Library scanning and the remembered last folder behave exactly as before.

Finish with:

```text
./scripts/check.sh
cargo test --release --workspace --tests -- --ignored --nocapture
cargo test --release -p rohditor-gpu -- --ignored --nocapture
```

The ignored workspace suite covers private full-resolution behavior, and the
ignored GPU suite is required because this feature changes resident GPU-base
reuse and preview handoff behavior. On a system where the sandbox exposes only
a software adapter, perform the final GPU check on the RX 9070 XT host.

## Acceptance criteria

- Rohditor starts normally with missing, malformed, or unreadable settings.
- `settings.json` is stored at the documented XDG/fallback location and is
  replaced transactionally.
- MHC, RCD, and AMaZE persist across normal desktop restarts with stable names.
- An explicit desktop `--demosaic` value overrides the saved preference only
  for that run.
- The selected algorithm is used consistently by fit preview, crop-authoring
  preview, Source 1:1, white-balance sampling, and export.
- Changing the setting refreshes the current presentation without becoming an
  image edit, affecting undo/redo, or briefly clearing the visible frame.
- Older async results and resident GPU bases created with another algorithm
  cannot be presented as current.
- The last browsed folder remains independent session state.
- No lower processing crate knows about desktop settings files or XDG paths.
- Normal, private, and real-GPU checks appropriate to the changed paths pass.

## Likely next settings

Do not include these in version 1, but the next settings with clear user value
are likely:

- persistent library source folders, when multi-root library behavior is
  designed;
- default or last-used export format, quality, bit depth, dithering, and safe
  metadata choices;
- processor preference, only after restart requirements and unavailable-GPU
  fallback are represented clearly in the UI.

Add each only when its owner, apply timing, migration default, and failure
behavior are defined. The settings struct can grow incrementally without
becoming a catch-all for transient document or UI state.
