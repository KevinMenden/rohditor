"""PixelShift200 (Hugging Face, CC BY 4.0): sharp demosaic ground truth.

Pixel shift measures R, Gr, Gb and B at every pixel, so these Sony A7R III
scenes need no interpolation to become ground truth. We take the full-resolution
merged RGGB scenes (`PixelShift200_RawFiles/raw_rggb`, 109 `.mat` files of about
340 MB). Skipped on purpose: the original `.ARW` captures (37 GB), the 14.7 GB
4K zip and the 512 px crop zip (both derivable from the full scenes, and a zip
can't be fetched in parts), and the deprecated colour release.
"""

from __future__ import annotations

import json
from pathlib import Path

from . import licenses
from .files import Checksum, Listing, RemoteFile
from .http import get_bytes
from .selection import select_round_robin

NAME = "pixelshift200"
REPO = "guochengqian/PixelShift200"
# Pinned commit, so the file list and contents can't change between runs.
REVISION = "ae9ab96dc7958cfe7ac4afa03e7dfb045c831b4a"
SPLIT_DIRS = {
    "train": "PixelShift200_RawFiles/raw_rggb/train_raw_rggb",
    "test": "PixelShift200_RawFiles/raw_rggb/test_raw_rggb",
}
SMALL_BUDGET = 2_000_000_000
CAMERA = "Sony ILCE-7RM3"


def _api(path: str):
    return json.loads(get_bytes(f"https://huggingface.co/api/datasets/{REPO}/{path}"))


def dataset_license() -> str:
    """Read the license from the dataset card at the pinned revision."""
    card = _api(f"revision/{REVISION}").get("cardData", {})
    return str(card.get("license", "unknown")).upper()


def build_files(tree: list[dict], split: str, license_id: str) -> list[RemoteFile]:
    return [
        RemoteFile(
            source=NAME,
            name=entry["path"].rsplit("/", 1)[-1],
            url=f"https://huggingface.co/datasets/{REPO}/resolve/{REVISION}/{entry['path']}",
            size=entry["size"],
            license=license_id,
            checksum=Checksum("sha256", entry["lfs"]["oid"]),
            meta={
                "scene": entry["path"].rsplit("/", 1)[-1].removesuffix("_rggb.mat"),
                "camera": CAMERA,
                "upstream_split": split,
            },
        )
        for entry in tree
        if entry["type"] == "file" and entry["path"].endswith(".mat")
    ]


def list_files(source_dir: Path, subset: str, allowed: frozenset[str]) -> Listing:
    license_id = dataset_license()
    listing = Listing()
    if license_id not in allowed:
        listing.skipped[f"license {license_id}"] += 1
        return listing

    by_split = {
        split: build_files(_api(f"tree/{REVISION}/{directory}"), split, license_id)
        for split, directory in SPLIT_DIRS.items()
    }
    if subset == "full":
        listing.files = [f for files in by_split.values() for f in files]
    else:
        groups = {split: [[f] for f in files] for split, files in by_split.items()}
        listing.files = select_round_robin(groups, SMALL_BUDGET)
    return listing
