"""Decode raw files into contract-scaled RGGB mosaics.

Two decoders: `decode_raw` (any camera raw via libraw/`rawpy`) and
`decode_pixelshift` (PixelShift200 `.mat` scenes). Both return float32 data with
black subtracted, divided by the white range, even dimensions and RGGB phase at
the origin.
"""

from __future__ import annotations

from dataclasses import dataclass
from pathlib import Path

import numpy as np
import rawpy
import scipy.io
from PIL import Image

from ..paths import assert_not_private
from .bayer import rggb_origin

# PixelShift200 values are 14-bit and already black-subtracted (no sample sits at
# the usual Sony black level of 512; the darkest percentile is near 0).
PIXELSHIFT_WHITE = 16383.0


class UnsupportedRaw(Exception):
    """The file decodes, but is not a plain 3-colour 2x2 Bayer raw (or doesn't decode)."""


@dataclass
class Mosaic:
    data: np.ndarray  # float32 [H, W]; H and W even; (0, 0) is a red photosite
    wb: np.ndarray  # as-shot gains [R, G, B], green = 1
    camera: str


@dataclass
class PixelShiftScene:
    planes: np.ndarray  # float32 [H, W, 4]: R, Gr, Gb, B at every pixel
    wb: np.ndarray
    camera: str


def _normalised_gains(rgbg: list[float] | None) -> np.ndarray:
    """libraw reports [R, G, B, G2] multipliers; reduce to [R, 1, B]."""
    if rgbg is not None and len(rgbg) >= 3 and rgbg[1] > 0 and rgbg[0] > 0 and rgbg[2] > 0:
        return np.array([rgbg[0] / rgbg[1], 1.0, rgbg[2] / rgbg[1]], np.float32)
    return np.ones(3, np.float32)


def camera_name(path: Path) -> str:
    """"Make Model" from the EXIF of TIFF-based raws, else a sensor fingerprint.

    libraw (and so `rawpy`) doesn't expose the model, and a few formats (Canon CRW)
    can't be read by Pillow. The fallback groups by file type and sensor size,
    which is what matters for "same sensor" splits.
    """
    try:
        with Image.open(path) as image:
            exif = image.getexif()
        make, model = str(exif.get(271, "")).strip(" \0"), str(exif.get(272, "")).strip(" \0")
        if model:
            # Models often repeat the make ("NIKON CORPORATION" / "NIKON D40").
            repeats = make and model.lower().startswith(make.split()[0].lower())
            return model if repeats else f"{make} {model}".strip()
    except Exception:  # noqa: BLE001 - any unreadable container falls back
        pass
    try:
        with rawpy.imread(str(path)) as raw:
            width, height = raw.sizes.width, raw.sizes.height
    except Exception:  # noqa: BLE001
        width = height = 0
    return f"unknown-{path.suffix.lstrip('.').lower()}-{width}x{height}"


def decode_raw(path: Path) -> Mosaic:
    assert_not_private(path)
    try:
        with rawpy.imread(str(path)) as raw:
            if (
                raw.raw_type != rawpy.RawType.Flat
                or raw.raw_pattern is None
                or raw.raw_pattern.shape != (2, 2)
                or raw.color_desc != b"RGBG"
            ):
                raise UnsupportedRaw(f"{path.name}: not a 3-colour 2x2 Bayer raw")
            image = raw.raw_image_visible.astype(np.float32)
            colors = raw.raw_colors_visible
            black = np.asarray(raw.black_level_per_channel, np.float32)
            white = float(raw.white_level)
            gains = raw.camera_whitebalance
            if not (gains and gains[1] > 0):
                gains = raw.daylight_whitebalance
    except UnsupportedRaw:
        raise
    except Exception as error:  # noqa: BLE001 - libraw raises many unrelated types
        raise UnsupportedRaw(f"{path.name}: {error}") from error

    # libraw colour indices: 0 = R, 1 = G, 2 = B, 3 = G2.
    dy, dx = rggb_origin(colors, r=0, g=(1, 3), b=2)
    per_pixel_black = black[colors]
    image = (image - per_pixel_black) / (white - per_pixel_black)
    image = image[dy:, dx:]
    image = np.ascontiguousarray(image[: image.shape[0] // 2 * 2, : image.shape[1] // 2 * 2])
    return Mosaic(image, _normalised_gains(list(gains) if gains is not None else None), camera_name(path))


def decode_pixelshift(path: Path, camera: str) -> PixelShiftScene:
    assert_not_private(path)
    mat = scipy.io.loadmat(str(path))
    planes = mat["rggb"].astype(np.float32) / PIXELSHIFT_WHITE
    meta = mat["metadata"][0, 0]
    gains = np.array([float(meta["red_gain"].ravel()[0]), 1.0, float(meta["blue_gain"].ravel()[0])], np.float32)
    h, w = planes.shape[0] // 2 * 2, planes.shape[1] // 2 * 2
    return PixelShiftScene(np.ascontiguousarray(planes[:h, :w]), gains, camera)
