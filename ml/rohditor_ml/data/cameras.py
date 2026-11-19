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

import mmap
import re
import struct
from pathlib import Path

import rawpy

# Makes that don't name the camera maker; the model then carries the brand.
_NOT_A_BRAND = {"adobe"}  # "Adobe DNG Converter" on converted DNGs
# Canon CRW (CIFF) stores make and model as one NUL-separated value (tag 0x080a).
_CRW_MAKE_MODEL = re.compile(rb"Canon\x00(Canon [\x20-\x7e]+)\x00")
# TIFF-based raws: classic TIFF (42), plus Olympus ORF ("RO", "RS") and Panasonic RW2 (0x55).
_TIFF_MAGICS = {42, 0x4F52, 0x5352, 0x55}
_MAKE, _MODEL, _ASCII = 271, 272, 2


def tiff_make_model(data) -> tuple[str, str] | None:
    """Make and Model from the first IFD of a TIFF-based raw (ARW, NEF, CR2, DNG, ORF, RW2).

    Pillow refuses many of these (e.g. Sony ARW) as images, but the two tags are
    plain ASCII entries in IFD0 of all of them. `data` is `bytes` or an `mmap`
    (IFD0 may sit anywhere, even at the end of the file).
    """
    if len(data) < 8 or data[:2] not in (b"II", b"MM"):
        return None
    order = "<" if data[:2] == b"II" else ">"
    magic, ifd = struct.unpack_from(order + "HI", data, 2)
    if magic not in _TIFF_MAGICS or ifd + 2 > len(data):
        return None
    (count,) = struct.unpack_from(order + "H", data, ifd)
    values = {}
    for i in range(count):
        entry = ifd + 2 + 12 * i
        if entry + 12 > len(data):
            break
        tag, kind, length = struct.unpack_from(order + "HHI", data, entry)
        if tag in (_MAKE, _MODEL) and kind == _ASCII:
            # Values of up to 4 bytes sit in the entry itself, longer ones at an offset.
            start = entry + 8 if length <= 4 else struct.unpack_from(order + "I", data, entry + 8)[0]
            values[tag] = data[start : start + length].split(b"\0", 1)[0].decode("ascii", "replace")
    return (values.get(_MAKE, ""), values[_MODEL]) if values.get(_MODEL) else None


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
    TIFF header's IFD0 or, for Canon CRW, from the CIFF heap. The
    fallback groups by file type and sensor size, which is what matters for
    "same sensor" splits.
    """
    if path.suffix.lower() == ".crw":
        match = _CRW_MAKE_MODEL.search(path.read_bytes())
        if match:
            return canonical_camera("Canon", match[1].decode("ascii"))
    else:
        with path.open("rb") as handle, mmap.mmap(handle.fileno(), 0, access=mmap.ACCESS_READ) as data:
            found = tiff_make_model(data)
        if found:
            return canonical_camera(*found)
    try:
        with rawpy.imread(str(path)) as raw:
            width, height = raw.sizes.width, raw.sizes.height
    except Exception:  # noqa: BLE001
        width = height = 0
    return f"unknown-{path.suffix.lstrip('.').lower()}-{width}x{height}"
