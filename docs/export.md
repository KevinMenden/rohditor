# Export contract

Phase 3 separates full-resolution rendering from codec and destination policy.
`ExportSettings` is a serializable core type and has no dependency on CLI or UI
widgets. It selects the format, JPEG quality or PNG depth, optional dithering,
metadata policy, and overwrite policy. The desktop export dialog can therefore
use the same validated settings and exporter as `rohditor-cli develop`.

## Formats and quantization

| Format | Samples | CLI selection | Default |
| --- | --- | --- | --- |
| JPEG | 8-bit RGB, encoded by `image` | `.jpg` or `.jpeg`; `--jpeg-quality 1..100` | Quality 90 |
| PNG | 8-bit RGB | `.png`; `--png-bit-depth 8` | 8-bit |
| PNG | 16-bit RGB | `.png`; `--png-bit-depth 16` | — |

Both sample depths are produced directly from the adjusted `f32` working image.
The 16-bit path does not expand an 8-bit buffer: tests decode the PNG and require
sample values that are not multiples of 257. The output transform clips in
linear sRGB, applies the sRGB transfer function, and then rounds to the selected
integer range.

`--dither` enables a deterministic 8 x 8 ordered Bayer offset before rounding.
The same offset is applied to all three channels of a pixel so neutral values
remain neutral. Dithering is off by default, preserving the Phase 2 8-bit output
and byte-for-byte determinism.

## Color and metadata

Every JPEG and PNG embeds Rohditor's deterministic matrix/TRC sRGB ICC profile.
The profile describes the standard sRGB primaries and transfer curve through a
D50 profile connection space. Output is therefore explicitly tagged rather
than relying on a viewer's untagged-RGB default.

The default `safe` metadata policy creates new EXIF instead of copying the
source block wholesale. Its allowlist is:

- camera make and model;
- exposure time, aperture, ISO, focal length, and capture time;
- lens make and model;
- oriented pixel dimensions;
- EXIF version and sRGB color-space declarations;
- Rohditor software version; and
- orientation set to top-left (`1`).

Pixels are physically oriented during rendering, so the source orientation tag
is never copied. GPS, serial numbers, thumbnails, maker notes, XMP, IPTC, and
unknown private tags are omitted. `--metadata none` omits EXIF completely; the
sRGB ICC profile remains mandatory.

## Destination safety

Export validates settings, extension, sample depth, transfer state, and packed
row layout before creating output. It then:

1. creates a unique temporary sibling with `create_new`;
2. encodes into that file and flushes it;
3. synchronizes the completed temporary file;
4. atomically replaces the destination with a same-directory rename when
   overwrite is enabled; or
5. atomically installs a no-clobber hard link when overwrite is disabled, then
   removes the temporary name.

The temporary guard removes its file on validation, encoding, flush, sync, or
commit failure. With `--force`, the old destination remains untouched until the
new encoding is complete. Without `--force`, both the early check and final
no-clobber operation reject an existing path. The CLI additionally refuses to
replace the source RAW through the same path, a symlink, or a hard link.

The no-clobber install requires same-filesystem hard-link support. The sibling
strategy provides that on the reference Linux filesystems; an unsupported
filesystem returns an actionable destination error. Directory fsync and
cross-filesystem moves are not part of the current contract.

## Verification

Tracked tests cover settings/extension validation, direct 16-bit samples,
deterministic dithering across one and four Rayon threads, ICC and EXIF
round-trips in JPEG and PNG, normalized orientation, quality-dependent JPEG
size/pixels, overwrite refusal, and preservation of an existing destination
after a forced encoding failure.

The opt-in Sony suite exercises the CLI against the private ILCE-6400 corpus. It
checks 6000 x 4000 landscape and 4000 x 6000 portrait geometry, RGB8 JPEG/PNG,
RGB16 PNG, ICC/EXIF extraction, top-left orientation, quality behavior, and
repeated deterministic PNG bytes. Representative Phase 3 files are also
accepted by `feh`, ImageMagick, FFmpeg, the Rust `image` decoder, and `file` on
the reference workstation.

Current limitations are fixed sRGB output, RGB-only JPEG/PNG, selected EXIF
rather than arbitrary metadata preservation, no alpha channel, and no 16-bit
TIFF. Full-resolution PNG16 also requires an encoder-side endian-conversion
buffer in addition to the pipeline memory estimate.
