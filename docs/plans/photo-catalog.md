# Photo catalog plan

## Outcome

Add a basic photo catalog to Rohditor. A user will be able to choose a folder,
browse its Sony ARW files in a thumbnail grid, and open an image in the existing
Develop workflow. The first version deliberately does not include ratings,
flags, tags, search, collections, batch operations, or a Develop filmstrip.

## Design decisions

- Use an explicit Lightroom-style `Library` / `Develop` mode switch.
- Scan one folder non-recursively.
- Show ARW files only in the first version. This matches the current supported
  editing scope and means every catalog item can be opened for editing.
- A single click selects an item; a double click opens it in Develop mode.
- Use the embedded JPEG already extracted by `rohditor-raw`; do not decode the
  sensor buffer to populate the catalog.
- Keep the document/render coordinator unchanged and add a separate catalog
  worker in the desktop app during the UI integration phase.

## Architecture

The reusable, UI-independent catalog logic lives in a new `rohditor-catalog`
crate:

- `scanner.rs` lists regular, non-hidden `.arw` files and returns naturally
  sorted entries with a `SourceIdentity` fingerprint.
- `thumbnail.rs` probes a RAW, extracts its embedded JPEG, applies the decoded
  image orientation, reduces it to a bounded thumbnail, and encodes a JPEG.
- `cache.rs` stores thumbnails below the XDG cache directory. Cache keys
  include the source path, source fingerprint, thumbnail dimensions, and JPEG
  quality. Cache entries use an image file plus JSON metadata and are written
  transactionally.

The desktop integration will add a separate catalog worker, incremental events,
visible-first thumbnail requests, and a bounded texture cache. It will reuse the
existing `open_path()` flow when an item is opened, so RAW decoding, preview
staleness checks, editing, and export remain in their current pipeline.

## Phases

1. **Catalog crate**: implement folder scanning, embedded-preview thumbnail
   generation, persistent cache, and focused unit tests. This is the current
   phase.
2. **Worker integration**: add catalog state, a background catalog worker,
   cancellation/generation guards, incremental events, and lazy loading.
3. **Library UI**: add the mode switch, folder picker, responsive thumbnail
   grid, selection/open behavior, empty states, and catalog activity status.
4. **Polish**: add capture-date sorting, remembered last folder, stale-cache
   cleanup, and better placeholder presentation.

## Explicitly out of scope for the first version

- Ratings, flags, color labels, tags, collections, and search.
- Recursive folder scanning.
- Multi-selection, batch processing, and batch export.
- XMP sidecars or catalog database persistence.
- JPEG/PNG catalog items or non-RAW editing.
- A Develop filmstrip.
- GPU thumbnail generation.

## Verification

- Test scanner behavior for extensions, hidden entries, directories, natural
  sorting, and source identity fields.
- Test thumbnail orientation, bounded dimensions, missing embedded previews,
  and encoded output.
- Test cache round trips, source invalidation, option invalidation, malformed
  metadata, corrupt image data, and interrupted/partial writes where practical.
- Run `./scripts/check.sh` before handing off the phase.
