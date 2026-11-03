"""Splitting by scene, never by patch.

Splits: `train`, `val`, `test`, and `test_unseen_camera` (every file from one
held-out camera, from any source, so the sensor is truly unseen). Within the
remaining files a scene's split is a hash of its id, so it doesn't change when
files are added. Dataset-defined test scenes (RawNIND's `test_reserve`,
PixelShift200's own test set) always go to `test`.
"""

from __future__ import annotations

import hashlib
from collections import defaultdict

TRAIN_FRACTION, VAL_FRACTION = 0.8, 0.1
SPLITS = ("train", "val", "test", "test_unseen_camera")


def choose_holdout_camera(scenes_by_camera: dict[str, set[str]]) -> str | None:
    """The camera with the fewest scenes, among those with at least two.

    Given only the cameras that have real noisy/clean pairs, this costs the least
    training data while leaving a test set of more than one scene.
    """
    candidates = sorted((len(scenes), camera) for camera, scenes in scenes_by_camera.items() if len(scenes) >= 2)
    return candidates[0][1] if candidates else None


def assign_split(scene_key: str, camera: str, reserved_for_test: bool, holdout_camera: str | None, seed: int) -> str:
    if camera == holdout_camera:
        return "test_unseen_camera"
    if reserved_for_test:
        return "test"
    u = int.from_bytes(hashlib.sha256(f"{seed}:{scene_key}".encode()).digest()[:8], "big") / 2**64
    if u < TRAIN_FRACTION:
        return "train"
    return "val" if u < TRAIN_FRACTION + VAL_FRACTION else "test"


def scenes_by_camera(items: list[tuple[str, str]]) -> dict[str, set[str]]:
    result: dict[str, set[str]] = defaultdict(set)
    for camera, scene_key in items:
        result[camera].add(scene_key)
    return result
