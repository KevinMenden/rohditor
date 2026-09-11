# Automatic recipe sidecars

**Status:** implemented; 2026-09-11

## Goal

Persist every edited image automatically in a sidecar beside the source RAW.
For `photo.ARW`, the sidecar is `photo.ARW.rohdit`. Changing images or closing
Rohditor must not show an unsaved-edits dialog.

The RAW remains immutable. This change does not add a library, folder history,
ratings, or metadata storage.

## Behavior

- Opening an image loads and validates its `.rohdit` sidecar when present.
- A recipe change schedules an automatic save after a short debounce, so a
  slider drag does not write every intermediate value.
- Sidecars retain the existing versioned JSON envelope and transactional file
  replacement.
- Switching or closing an image queues its newest recipe immediately and then
  continues without asking the user. Application shutdown drains the queued
  save before the persistence worker exits.
- Save failures are reported in the application status/error UI. The editor
  keeps the recipe dirty and retries after the next edit or explicit save.
- The existing Save action may remain as a way to request an immediate
  background save, but it must not block the UI.

This pre-release change does not migrate or fall back to the old
`.rohditor.json` filename.

## Implementation plan

1. **Adopt the `.rohdit` sidecar name.** Change desktop persistence to derive
   `<complete source filename>.rohdit`, update persistence tests, and retain
   recipe validation, format-version checks, distinct load errors, and atomic
   sibling-file replacement.

2. **Add a small background save worker.** Give it immutable jobs containing
   document ID, recipe revision, source path, and a recipe snapshot. Debounce
   ordinary edits and coalesce queued jobs for the same source so only the
   newest snapshot is written. Return completion events with the same identity;
   no image processing or catalog behavior belongs in this worker.

3. **Track saved revisions safely.** Schedule autosave wherever a committed
   recipe revision currently queues a new preview, including undo, redo, reset,
   crop, metadata-derived defaults, and discrete controls. A completion may
   establish the clean baseline only for the exact recipe snapshot it wrote;
   completion of an older revision must leave newer edits dirty. Failed writes
   must never mark a recipe saved.

4. **Simplify document transitions.** Remove `PendingDocumentAction` and the
   Save/Discard/Cancel dialog. On image change or document close, enqueue the
   current dirty snapshot without debounce and proceed immediately. During app
   shutdown, stop accepting jobs, write the newest queued snapshot for each
   source, and join the worker. Surface errors that arrive after an image was
   closed as application-level notices.

5. **Update the save UI and verify the lifecycle.** Present useful states such
   as modified, saving, saved, and save failed; update the title/status text and
   make the existing Save action request an immediate asynchronous save. Add
   focused tests for naming and round trips, debounce/coalescing, stale
   completions, switching images during a pending save, write failure, and
   shutdown draining. Run `cargo fmt --all -- --check`, `git diff --check`, and
   `./scripts/check.sh`.

## Acceptance criteria

- Editing `photo.ARW` creates or updates `photo.ARW.rohdit` without freezing
  the UI.
- Reopening the image restores the last successfully saved recipe.
- Rapid slider changes do not cause one disk write per intermediate value, and
  the newest recipe wins even when an earlier save completes later.
- Changing images and closing a document never opens an unsaved-edits dialog.
- Normal application shutdown preserves the newest queued recipe.
- A failed save is visible and does not falsely show the document as saved.
