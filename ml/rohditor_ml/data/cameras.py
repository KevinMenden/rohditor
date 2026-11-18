"""Camera names: one canonical spelling across all sources.

The unseen-camera split and the per-camera noise profiles both match cameras
by name, across sources: a RawNIND camera named from its EXIF ("NIKON
CORPORATION" / "NIKON D40") must equal the same camera from the raw.pixls.us
table ("Nikon" / "D40"), or its pixls shots would leak into training. So every
name goes through `canonical_camera`, which gives a lower-case "brand model".

Known limit: marketing aliases of one sensor (EOS 650D = Rebel T4i = Kiss X6i)
stay distinct names.
"""

from __future__ import annotations

import re
from pathlib import Path

import rawpy
from PIL import Image

# Makes that don't name the camera maker; the model then carries the brand.
_NOT_A_BRAND = {"adobe"}  # "Adobe DNG Converter" on converted DNGs
# Canon CRW (CIFF) stores make and model as one NUL-separated value (tag 0x080a).
_CRW_MAKE_MODEL = re.compile(rb"Canon\x00(Canon [\x20-\x7e]+)\x00")


def canonical_camera(make: str, model: str) -> str:
    """'NIKON CORPORATION' + 'NIKON D40' and 'Nikon' + 'D40' both -> 'nikon d40'."""
    brand = make.strip(" \0").split(" ")[0].lower() if make.strip(" \0") else ""
    if brand in _NOT_A_BRAND:
        brand = ""
    words = model.strip(" \0").lower().split()
    if brand and words and words[0] == brand:  # the model often repeats the make
        words = words[1:]
    return " ".join([brand, *words]).strip()


def exif_camera(path: Path) -> str:
    """Canonical camera of a raw file from its own metadata, else a sensor fingerprint.

    libraw (and so `rawpy`) doesn't expose make and model, so they come from the
    TIFF/EXIF header (via Pillow) or, for Canon CRW, from the CIFF heap. The
    fallback groups by file type and sensor size, which is what matters for
    "same sensor" splits.
    """
    if path.suffix.lower() == ".crw":
        match = _CRW_MAKE_MODEL.search(path.read_bytes())
        if match:
            return canonical_camera("Canon", match[1].decode("ascii"))
    else:
        try:
            with Image.open(path) as image:
                exif = image.getexif()
            make, model = str(exif.get(271, "")), str(exif.get(272, ""))
            if model.strip(" \0"):
                return canonical_camera(make, model)
        except Exception:  # noqa: BLE001 - any unreadable container falls back
            pass
    try:
        with rawpy.imread(str(path)) as raw:
            width, height = raw.sizes.width, raw.sizes.height
    except Exception:  # noqa: BLE001
        width = height = 0
    return f"unknown-{path.suffix.lstrip('.').lower()}-{width}x{height}"
