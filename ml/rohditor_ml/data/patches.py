"""Choosing and cutting 256x256 patches.

Origins are always even, so every patch starts on a red photosite and keeps the
contract's RGGB phase. A margin keeps patches away from the image border, where
demosaicing and alignment are least reliable.
"""

from __future__ import annotations

import numpy as np

from ..contract import PATCH_SIZE

BORDER_MARGIN = 16
MIN_MEAN = 0.003  # skip patches that are essentially black
MAX_CLIPPED_FRACTION = 0.5  # ... or mostly saturated


def sample_origins(
    mosaic: np.ndarray, count: int, rng: np.random.Generator, patch: int = PATCH_SIZE
) -> list[tuple[int, int]]:
    """Up to `count` random even `(y, x)` origins of usable patches, without repeats."""
    h, w = mosaic.shape
    lo = BORDER_MARGIN
    ys, xs = range(lo, h - patch - lo + 1, 2), range(lo, w - patch - lo + 1, 2)
    if len(ys) == 0 or len(xs) == 0:
        return []
    chosen: list[tuple[int, int]] = []
    seen: set[tuple[int, int]] = set()
    for _ in range(count * 8):  # bounded retries for images with few usable patches
        origin = (int(rng.choice(ys)), int(rng.choice(xs)))
        if origin in seen:
            continue
        seen.add(origin)
        window = mosaic[origin[0] : origin[0] + patch, origin[1] : origin[1] + patch]
        if window.mean() >= MIN_MEAN and (window >= 1.0).mean() <= MAX_CLIPPED_FRACTION:
            chosen.append(origin)
            if len(chosen) == count:
                break
    return chosen


def cut(array: np.ndarray, origin: tuple[int, int], patch: int = PATCH_SIZE) -> np.ndarray:
    """The `patch`-square window at `origin` over the last two axes."""
    y, x = origin
    return array[..., y : y + patch, x : x + patch]
