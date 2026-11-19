"""Visual-only crops of the private corpus (`testdata/private`).

It has no ground truth, so there are no metrics: each raw yields a few 100%
crops with every method side by side. The output stays under `ml/runs/`
(git-ignored) and must not be published.
"""

from __future__ import annotations

from pathlib import Path

import numpy as np

from ..data.decode import UnsupportedRaw, decode_raw
from ..paths import PRIVATE_DIR
from .crops import exposure_for, save_strip

CROP = 512
RAW_SUFFIXES = {".arw", ".cr2", ".cr3", ".nef", ".dng", ".orf", ".rw2", ".raf", ".pef"}


def crop_origins(h: int, w: int) -> list[tuple[int, int]]:
    """The centre and an upper-left third, at even offsets (RGGB phase)."""
    points = [(h // 2, w // 2), (h // 3, w // 3)]
    return [((y - CROP // 2) // 2 * 2, (x - CROP // 2) // 2 * 2) for y, x in points]


def private_crops(methods: list, out_dir: Path, limit: int | None = None) -> int:
    files = sorted(p for p in PRIVATE_DIR.glob("*") if p.suffix.lower() in RAW_SUFFIXES)[:limit]
    written = 0
    for path in files:
        try:
            raw = decode_raw(path, allow_private=True)
        except UnsupportedRaw as error:
            print(f"  skipped {path.name}: {error}")
            continue
        h, w = raw.data.shape
        if h < CROP or w < CROP:
            continue
        for n, (y, x) in enumerate(crop_origins(h, w)):
            mosaic = raw.data[y : y + CROP, x : x + CROP][None]
            gains = raw.wb[None].astype(np.float32)
            panels = [(m.name, m.run(mosaic, gains)[0]) for m in methods]
            save_strip(out_dir / f"{path.stem}_{n}.png", panels, exposure_for(panels[0][1]))
            written += 1
    return written
