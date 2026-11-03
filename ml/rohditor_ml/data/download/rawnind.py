"""RawNIND (UCLouvain Dataverse, doi:10.14428/DVN/DEQCIM): real noisy/clean raw pairs.

File names encode everything: `Bayer_<scene>[_GT]_ISO<iso>_sha1=<sha1>.<ext>`.
The dataset-level license is CC BY-SA 4.0, but `permissions.txt` grants CC0 (or
CC-BY) for individual scenes and marks a few as research-only. Per scene we use
the stricter-of-record: the entry in `permissions.txt` if there is one, else the
dataset license. X-Trans captures are skipped (Bayer RGGB only).
"""

from __future__ import annotations

import json
import re
from collections import Counter, defaultdict
from pathlib import Path
from typing import Any

from . import licenses
from .files import Checksum, Listing, RemoteFile
from .http import cached_text, get_bytes
from .selection import select_round_robin

NAME = "rawnind"
SERVER = "https://dataverse.uclouvain.be"
DOI = "doi:10.14428/DVN/DEQCIM"
VERSION = "1.0"  # pinned so the listing doesn't shift under us
DATASET_LICENSE = licenses.CC_BY_SA
SMALL_BUDGET = 2_000_000_000

RAW_EXTENSIONS = {"arw", "cr2", "crw", "nef", "dng"}
FILE_RE = re.compile(
    r"^Bayer_(?:(?:TEST|UNK)_)?(?P<scene>.+?)(?P<gt>_GT)?_ISO(?P<iso>.+?)"
    r"_sha1=(?P<sha1>[0-9a-f]{40})\.(?P<ext>\w+)$"
)


def parse_permissions(text: str) -> dict[str, str]:
    """`permissions.txt` lines look like `scene: CC0, by Someone`; returns scene -> SPDX id."""
    result: dict[str, str] = {}
    for line in text.splitlines():
        scene, sep, note = line.partition(":")
        if not sep:
            continue
        note = note.strip()
        if note.startswith("CC0"):
            result[scene.strip()] = licenses.CC0
        elif "CC-BY license" in note:
            # The note doesn't name a version; assume the current 4.0.
            result[scene.strip()] = licenses.CC_BY
        else:  # "fair-use / research purpose only" and anything unforeseen
            result[scene.strip()] = licenses.RESEARCH_ONLY
    return result


def parse_test_reserve(text: str) -> set[str]:
    """Scenes the dataset authors reserve for testing (`  - scene` lines)."""
    return set(re.findall(r"^\s*-\s*(\S+)\s*$", text, flags=re.MULTILINE))


def build_files(
    listing_json: dict[str, Any],
    scene_licenses: dict[str, str],
    test_reserve: set[str],
    allowed: frozenset[str],
) -> tuple[dict[str, list[RemoteFile]], Counter[str]]:
    """Group the allowed Bayer raw files by scene; also count what was skipped."""
    scenes: dict[str, list[RemoteFile]] = defaultdict(list)
    skipped: Counter[str] = Counter()
    for entry in listing_json["data"]["files"]:
        label = entry["label"]
        match = FILE_RE.match(label)
        if not match or match["ext"].lower() not in RAW_EXTENSIONS:
            skipped["not a Bayer raw (X-Trans, sidecar or metadata)"] += 1
            continue
        scene = match["scene"]
        license_id = scene_licenses.get(scene, DATASET_LICENSE)
        if license_id not in allowed:
            skipped[f"license {license_id}"] += 1
            continue
        data = entry["dataFile"]
        scenes[scene].append(
            RemoteFile(
                source=NAME,
                name=label,
                url=f"{SERVER}/api/access/datafile/{data['id']}",
                size=data["filesize"],
                license=license_id,
                checksum=Checksum("md5", data["md5"]),
                meta={
                    "scene": scene,
                    "is_clean": match["gt"] is not None,
                    "iso": match["iso"],
                    "test_reserve": scene in test_reserve,
                },
            )
        )
    return scenes, skipped


def list_files(source_dir: Path, subset: str, allowed: frozenset[str]) -> Listing:
    meta_dir = source_dir / "_meta"
    # Dataverse serves the small metadata files by id; look them up in the listing.
    listing_json = _fetch_listing()
    ids = {f["label"]: f["dataFile"]["id"] for f in listing_json["data"]["files"]}
    permissions = cached_text(f"{SERVER}/api/access/datafile/{ids['permissions.txt']}", meta_dir / "permissions.txt")
    reserve = cached_text(f"{SERVER}/api/access/datafile/{ids['test_reserve.yaml']}", meta_dir / "test_reserve.yaml")

    scenes, skipped = build_files(listing_json, parse_permissions(permissions), parse_test_reserve(reserve), allowed)
    if subset == "full":
        return Listing([f for files in scenes.values() for f in files], skipped)

    # Small: whole scenes that have a clean reference, spread over camera brands
    # (file extension is the only camera hint in the names).
    groups: dict[str, list[list[RemoteFile]]] = defaultdict(list)
    for files in scenes.values():
        clean = [f for f in files if f.meta["is_clean"]]
        if clean:
            groups[clean[0].name.rsplit(".", 1)[-1].lower()].append(files)
    return Listing(select_round_robin(groups, SMALL_BUDGET), skipped)


def _fetch_listing() -> dict[str, Any]:

    url = f"{SERVER}/api/datasets/:persistentId/versions/{VERSION}?persistentId={DOI}"
    return json.loads(get_bytes(url))
