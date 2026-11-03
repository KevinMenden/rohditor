"""Turning download catalogs into items with camera, scene and split assigned."""

from __future__ import annotations

import json
import re
from dataclasses import dataclass, field
from pathlib import Path

from .decode import camera_name
from .download.http import file_checksum
from .split import assign_split, choose_holdout_camera, scenes_by_camera


@dataclass
class Item:
    source: str
    name: str
    path: Path
    license: str
    camera: str
    scene: str
    iso: int | None
    is_clean: bool  # a clean low-ISO reference (everything except RawNIND's noisy shots)
    reserved_for_test: bool
    sha256: str = ""
    split: str = ""
    meta: dict = field(default_factory=dict)

    @property
    def scene_key(self) -> str:
        return f"{self.source}:{self.scene}"


def _iso(value) -> int | None:
    match = re.search(r"\d+", str(value))
    return int(match.group()) if match else None


def load_items(raw_dir: Path, sources: list[str], allowed: frozenset[str]) -> list[Item]:
    """Every downloaded catalog file whose license is allowed and which exists on disk."""
    items: list[Item] = []
    for source in sources:
        catalog = json.loads((raw_dir / source / "catalog.json").read_text(encoding="utf-8"))
        for entry in catalog["files"]:
            path = raw_dir / source / entry["name"]
            if entry["license"] not in allowed or not path.exists():
                continue
            meta = entry["meta"]
            if source == "pixls":
                scene, is_clean, reserved = f"pixls-{meta['pixls_id']}", True, False
                camera = camera_name(path)
            elif source == "pixelshift200":
                scene, is_clean, reserved = meta["scene"], True, meta["upstream_split"] == "test"
                camera = meta["camera"]
            else:  # rawnind
                scene, is_clean, reserved = meta["scene"], meta["is_clean"], meta["test_reserve"]
                camera = camera_name(path)
            items.append(Item(source, entry["name"], path, entry["license"], camera, scene,
                              _iso(meta.get("iso")), is_clean, reserved, meta=meta))
    return items


def assign_splits(items: list[Item], holdout_camera: str | None, seed: int) -> str | None:
    """Fill in `item.split`; returns the held-out camera (chosen automatically if not given)."""
    if holdout_camera is None:
        # Only cameras with real noisy/clean pairs can serve as a meaningful unseen-sensor test.
        paired = [(i.camera, i.scene_key) for i in items if i.source == "rawnind" and not i.is_clean]
        holdout_camera = choose_holdout_camera(scenes_by_camera(paired))
    for item in items:
        item.split = assign_split(item.scene_key, item.camera, item.reserved_for_test, holdout_camera, seed)
        item.sha256 = file_checksum(item.path, "sha256")
    return holdout_camera
