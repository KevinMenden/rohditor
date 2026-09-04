# Library UI redesign plan

## Outcome

Make the Library workspace feel like one deliberate photo-browsing surface:

- Keep `Open File…` and `Open Folder…` together in the top-left toolbar.
- Remove duplicate Library headings and Open Folder buttons.
- Give the library header and thumbnail grid consistent outer margins and
  spacing.
- Keep the current folder, photo count, thumbnail progress, and sorting control
  visible without competing with the main content.
- Preserve Rohditor's current single-folder, Sony ARW scope; do not introduce a
  persistent catalog database or a full folder tree until a real feature needs
  one.

## Design rationale

Established photo applications make the source of the current images a clear,
stable part of the browsing workspace. Lightroom's Library module uses folders
and collections as the source navigation for its central grid. darktable puts
Import and Collections in the left panel, with the lighttable grid in the
center. digiKam similarly uses a left sidebar for album selection and a central
thumbnail area.

References:

- [Lightroom Classic: viewing and organizing photos](https://helpx.adobe.com/uk/lightroom-classic/help/library-module-basic-workflow.html)
- [darktable: lighttable view layout](https://docs.darktable.org/usermanual/development/en/lighttable/lighttable-view-layout/)
- [digiKam: image view](https://docs.digikam.org/en/main_window/image_view.html)

Rohditor currently has only one browsed folder, so the useful principle is the
separation of global source actions from current-source context. A full source
sidebar would add complexity without improving the current workflow.

## Target layout

The top toolbar should group the global source actions on the left:

```text
[Menu] Rohditor   [Open File…] [Open Folder…]       [Library] [Develop] ...
```

The Library content should show the current source once:

```text
photoshoot-2026-08-30                         Sort: Filename
37 photos · Loading thumbnails…

[thumbnail grid with consistent outer margins]
```

When no folder is open, the content should show a quiet empty state:

```text
No folder open
Use Open Folder… in the toolbar to browse Sony ARW photos.
```

The empty state should not add another Open Folder button because the toolbar
action is persistent and immediately visible.

## Implementation phases

### 1. Consolidate source actions

Update the toolbar so `Open File…` and `Open Folder…` are adjacent in the
left-side identity/action group, immediately after the Rohditor identity.

- Add a folder icon to the existing icon set and use the existing file icon for
  the file action.
- Use one shared button treatment and consistent tooltips.
- Label the visible file action `Open File…`; explain in its tooltip and dialog
  that the current supported input is Sony ARW.
- Remove the Library header's visible `Open Folder…` button.
- Remove the Library empty-state `Open Folder…` button.
- Remove redundant visible `Open RAW…` buttons from the Develop file panel and
  empty viewport where the global toolbar already provides the same action.
- Keep application-menu commands as secondary desktop-menu equivalents, but
  route them through the same toolbar actions.

Relevant code:

- `apps/desktop/src/ui/toolbar.rs`
- `apps/desktop/src/ui/catalog.rs`
- `apps/desktop/src/ui/viewport.rs`
- `apps/desktop/src/ui/icons.rs`

### 2. Make navigation behavior explicit

The global actions should have unambiguous results:

- `Open File…` opens the selected RAW and switches to Develop.
- `Open Folder…` starts the catalog scan and switches to Library.
- Cancelling either dialog leaves the current view and document unchanged.
- Toolbar and menu invocations use the same application-level action path.

Rename ambiguous UI output fields such as `open` to `open_file` where that
improves readability. Remove the Library-specific `open_folder` output once the
content no longer owns an Open Folder button.

Relevant code:

- `apps/desktop/src/app.rs`
- `apps/desktop/src/ui/toolbar.rs`
- `apps/desktop/src/ui/catalog.rs`

### 3. Redesign the Library header

Use the header for current-folder context rather than repeating the workspace
name.

- Show the folder name as the primary heading.
- Show the number of photos and thumbnail-loading status as muted secondary
  information.
- Keep the sort selector on the right side of the same padded header row.
- Add a tooltip containing the full path when the folder name is truncated.
- When no folder is open, use `No folder open` rather than another `Library`
  heading.
- Avoid repeating the same catalog count and folder name in the status bar;
  the status bar should focus on global processing, errors, and activity.

### 4. Add a proper Library content frame

The Library should have its own layout frame instead of using the bare viewport
frame.

- Add Library-specific content padding, initially around 20–24 px on each side.
- Add named theme metrics for content padding, header spacing, and grid gaps.
- Apply the same horizontal inset to the header, error messages, empty state,
  and grid.
- Use a consistent grid gutter, initially around 12 px horizontally and
  vertically.
- Include gutters in cell-width calculations so the final column aligns with
  the content frame.
- Preserve the existing thumbnail-card colors, selection treatment, and lazy
  loading behavior unless the new spacing exposes a separate visual defect.

Relevant code:

- `apps/desktop/src/ui/catalog.rs`
- `apps/desktop/src/ui/theme.rs`

### 5. Simplify empty and error states

Use one clear message for each state:

- No folder: explain how to use the toolbar and avoid a second button.
- Empty folder: say that no supported Sony ARW files were found.
- Scan failure: show the existing error styling and explain that another folder
  can be selected from the toolbar.
- Thumbnail failure: keep the per-card failure state, but do not let it create
  another competing page-level action.

The empty state may use a restrained card or icon treatment, but it should not
look like a second page header.

### 6. Handle narrow windows deliberately

The two source actions should remain adjacent at normal widths. At narrow
widths:

- Use compact labels or icon-plus-tooltip presentation if the toolbar cannot
  fit the full labels.
- Keep both actions discoverable and avoid silently dropping either one.
- Ensure the Library/Develop switcher does not overlap the source actions.
- Add or update the narrow-toolbar breakpoint behavior and tests.

## Testing and verification

Add or update focused tests for:

- Toolbar action routing and action naming.
- Open File switching to Develop.
- Open Folder switching to Library.
- Library grid column and gutter calculations.
- Library content padding at normal and narrow widths.
- Empty, populated, loading, and failed catalog states.

Manually verify:

- Starting with no folder open.
- Opening a folder from the top-left toolbar.
- Opening a RAW from the same toolbar.
- Switching folders while already in Library.
- Opening a thumbnail with single and double click behavior unchanged.
- Thumbnail loading and scan errors.
- Normal and narrow window sizes.
- Absence of duplicate visible Library headings and Open Folder buttons.

Finish with:

```text
./scripts/check.sh
```

## Acceptance criteria

- There is one persistent visible `Open File…` action and one persistent
  visible `Open Folder…` action, adjacent in the top-left toolbar.
- The Library header contains the current folder context, not a duplicate
  generic `Library` title.
- The no-folder state contains no competing Open Folder button.
- The header and grid have clear, consistent outer margins.
- Grid gutters and card alignment remain correct at different window widths.
- Opening files, opening folders, view switching, catalog scanning, and lazy
  thumbnail loading retain their existing behavior.
- `./scripts/check.sh` passes.
