"""raw.pixls.us: clean low-ISO Bayer raws for synthetic noise.

The site's repository table gives make, model, license, sha256 and size for every
file but not the ISO, which is only in each file's EXIF dump. So we fetch (and
cache) the EXIF text of every allowed file to keep ISO <= 400. Files without a
readable ISO are skipped. Whether a file is Bayer RGGB is checked when decoding.
"""

from __future__ import annotations

import json
import re
from concurrent.futures import ThreadPoolExecutor
from pathlib import Path

from . import licenses
from .files import Checksum, Listing, RemoteFile
from .http import cached_text
from .selection import select_round_robin

NAME = "pixls"
BASE = "https://raw.pixls.us"
REPOSITORY_URL = f"{BASE}/json/getrepository.php?set=all"
MAX_ISO = 400
SMALL_BUDGET = 1_000_000_000
EXIF_WORKERS = 8
# Container formats that are never raw captures.
NON_RAW_EXTENSIONS = {"jpg", "jpeg", "png", "tif", "tiff", "mp4", "mov", "avi", "txt", "xmp"}
# Standard EXIF fields that may hold the ISO, best first.
ISO_KEYS = (
    "Exif.Photo.ISOSpeedRatings",
    "Exif.Photo.ISOSpeed",
    "Exif.Image.ISOSpeedRatings",
    "Exif.Photo.RecommendedExposureIndex",
)

_LICENSE_HREF = re.compile(r"href='([^']+)'")
_FILE_LINK = re.compile(r"href='(?P<url>[^']*/getfile\.php/(?P<id>\d+)/nice/(?P<name>[^']+))'")
_SHA256 = re.compile(r"\b([0-9a-f]{64})\b")
_SIZE = re.compile(r"\(([\d.]+)\s*(kB|MB|GB)\)")
_UNIT_BYTES = {"kB": 1_000, "MB": 1_000_000, "GB": 1_000_000_000}


def parse_row(row: list[str]) -> RemoteFile | None:
    """Turn one row of the repository table into a file, or None if unreadable."""
    make, model, mode, _megapixels, _notes, license_html, _date, file_html, exif_html = row
    link, sha, size = _FILE_LINK.search(file_html), _SHA256.search(file_html), _SIZE.search(file_html)
    href, exif = _LICENSE_HREF.search(license_html), _LICENSE_HREF.search(exif_html)  # exif may be absent
    if not (link and sha and size and href):
        return None
    safe_name = re.sub(r"[^\w.+-]+", "_", link["name"])
    return RemoteFile(
        source=NAME,
        # Ids are unique; the readable name stays for humans. Keep it filesystem-safe.
        name=f"{link['id']}_{safe_name}",
        url=link["url"],
        size=round(float(size[1]) * _UNIT_BYTES[size[2]]),
        exact_size=False,  # the table shows a rounded size
        license=licenses.from_creative_commons_url(href[1]),
        checksum=Checksum("sha256", sha[1]),
        meta={
            "camera": f"{make} {model}",
            "make": make,
            "model": model,
            "mode": mode,
            "pixls_id": int(link["id"]),
            "exif_url": exif[1] if exif else None,
        },
    )


def parse_iso(exif_text: str) -> int | None:
    for key in ISO_KEYS:
        match = re.search(rf"^{re.escape(key)}\s+(\d+)", exif_text, flags=re.MULTILINE)
        if match and int(match[1]) > 0:
            return int(match[1])
    return None


def _iso_of(file: RemoteFile, exif_dir: Path) -> int | None:
    if file.meta["exif_url"] is None:
        return None
    try:
        text = cached_text(file.meta["exif_url"], exif_dir / f"{file.meta['pixls_id']}.txt")
    except OSError:  # network or HTTP error: leave the ISO unknown
        return None
    return parse_iso(text)


def list_files(source_dir: Path, subset: str, allowed: frozenset[str]) -> Listing:
    meta_dir = source_dir / "_meta"
    rows: list[list[str]] = json.loads(cached_text(REPOSITORY_URL, meta_dir / "repository.json"))["data"]
    listing = Listing()

    candidates: list[RemoteFile] = []
    for row in rows:
        file = parse_row(row)
        if file is None:
            listing.skipped["unparsable table row"] += 1
        elif file.name.rsplit(".", 1)[-1].lower() in NON_RAW_EXTENSIONS:
            listing.skipped["not a raw file"] += 1
        elif file.license not in allowed:
            listing.skipped[f"license {file.license}"] += 1
        else:
            candidates.append(file)

    with ThreadPoolExecutor(EXIF_WORKERS) as pool:
        isos = list(pool.map(lambda f: _iso_of(f, meta_dir / "exif"), candidates))

    kept: list[RemoteFile] = []
    for file, iso in zip(candidates, isos, strict=True):
        if iso is None:
            listing.skipped["ISO unknown"] += 1
        elif iso > MAX_ISO:
            listing.skipped[f"ISO > {MAX_ISO}"] += 1
        else:
            file.meta["iso"] = iso
            kept.append(file)

    if subset == "full":
        listing.files = kept
    else:
        groups: dict[str, list[list[RemoteFile]]] = {}
        for file in kept:
            groups.setdefault(file.meta["make"], []).append([file])
        listing.files = select_round_robin(groups, SMALL_BUDGET)
    return listing
