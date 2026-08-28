# CPU reference pipeline

Phase 2 defines the deterministic behavior that later preview, export, and GPU
paths must match. The public core API keeps sensor mosaics, scene-linear RGB,
and transfer-encoded display RGB in separate types with explicit dimensions,
strides, CFA/color spaces, and output transfer.

## Fixed stage order

1. Select the active area or recommended crop while retaining original sensor
   coordinates for level-pattern indexing and CFA phase.
2. Normalize each sample as `(sample - black) / (white - black)`. Values below
   zero and above one are preserved.
3. Apply validated as-shot R/G/B gains, optionally multiplied by relative manual
   gains, as part of the linear bilinear demosaic.
4. Invert the selected XYZ-to-camera calibration matrix and convert through XYZ
   to linear Rec.2020/D65. D65 is preferred; D50 and Standard Light A use
   Bradford chromatic adaptation to D65.
5. Apply exposure, contrast, and saturation in linear Rec.2020.
6. Convert Rec.2020 through D65 XYZ to linear sRGB. The named Phase 2 output
   policy hard-clips each linear-sRGB component to `[0, 1]` at this boundary.
7. Apply the sRGB transfer function, quantize to the nearest 8-bit code value,
   and physically apply the selected EXIF orientation.

The lossless PNG encoder is a CLI consumer of the final display buffer; color
matrix and transfer behavior do not depend on that codec.

## Recipe definitions

| Parameter | Range | Neutral | Definition |
| --- | ---: | ---: | --- |
| Exposure | -5 to +5 EV | 0 EV | Linear gain `2^EV` |
| Contrast | -1 to +1 | 0 | Slope `2^contrast` around linear 0.18 |
| Saturation | 0 to 2 | 1 | Interpolation from Rec.2020 luminance (`0.2627 R + 0.6780 G + 0.0593 B`) |
| Relative WB R/G/B | 0.25 to 4 each | 1,1,1 | Multiplies the decoded as-shot gains |

The recipe schema version is validated during deserialization. Missing or
non-finite white balance, singular/unsupported matrices, invalid level patterns,
out-of-bounds crops, and out-of-range edits are typed errors.

## Determinism and private-corpus checks

Synthetic tests cover RGGB, BGGR, GRBG, and GBRG phase/border behavior,
per-phase levels, unclipped normalized highlights, matrix inversion/adaptation,
neutral edits, known exposure gains, grayscale saturation, clipping, sRGB
round-trips, and orientation. The same synthetic end-to-end render is byte
identical in Rayon pools restricted to one and four threads.

The opt-in private suite develops all six Sony ILCE-6400 samples. Each neutral
render has the expected 6000 x 4000 or oriented 4000 x 6000 dimensions, broad
numeric output range, and deterministic repeated full-image hash. The CLI suite
decodes its PNGs and verifies a landscape and portrait output plus byte-identical
repeated PNG encoding.

## Reference measurement

Measurement date: 2026-08-28. Command: release `rohditor-cli develop` on
`DSC00851.ARW`, recommended 6000 x 4000 crop, neutral recipe, reference machine
from `development-environment.md`.

| Stage | Time |
| --- | ---: |
| Metadata/color validation | <0.1 ms |
| Normalize | 168.9 ms |
| Bilinear demosaic | 466.5 ms |
| Camera color conversion | 37.2 ms |
| Neutral adjustments | 2.2 ms |
| sRGB/output orientation | 472.5 ms |
| CPU pipeline total | 1170.5 ms |
| End to end with decode and PNG | 2.01 s |

The estimated peak of simultaneously live core/decoded buffers was 413 MiB.
Measured maximum RSS was 475,368 KiB (about 464 MiB). Warm release tests without
PNG encoding rendered the six samples in 0.31 to 0.36 seconds each, demonstrating
that these timings vary materially with cache and machine load.

## Known baseline limitations

- Bilinear demosaic is a correctness reference, not the final detail/aliasing
  algorithm; saturated edges may show color artifacts.
- Dual-illuminant matrix interpolation is not implemented. The D65 matrix is
  preferred for the current Sony profile, while as-shot gains provide scene
  white balance.
- The neutral recipe intentionally has no camera-look tone curve or highlight
  roll-off. Out-of-gamut and over-range linear sRGB is hard-clipped.
- PNG samples use sRGB coordinates, but explicit ICC/sRGB chunk embedding is a
  Phase 3 export responsibility.
- Full-resolution processing currently retains roughly 413 MiB of decoded/core
  buffers. Preview scaling, buffer reuse, caching, and tiling remain later work.
