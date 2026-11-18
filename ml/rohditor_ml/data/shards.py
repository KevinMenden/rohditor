"""Writing patch shards (uncompressed `.npz`).

Each shard holds up to `PATCHES_PER_SHARD` patches of one split and kind:

* `mosaic [N, P, P]`: the network input (clean for kind `clean`, a real noisy
  capture for kind `noisy`), contract-scaled, RGGB phase;
* `rgb [N, 3, P, P]`: the target;
* `wb [N, 3]`: as-shot white-balance gains;
* `file_index [N]`: index into the manifest's `files`; `origin [N, 2]`: the
  patch position in that file's cropped mosaic;
* `mask [N, P, P]` (kind `noisy` only): 1 where the pair is trustworthy.
"""

from __future__ import annotations

import hashlib
from collections import defaultdict
from pathlib import Path

import numpy as np

PATCHES_PER_SHARD = 128


class ShardWriter:
    def __init__(self, root: Path):
        self.root = root
        self.entries: list[dict] = []
        self._pending: dict[tuple[str, str], dict[str, list]] = defaultdict(lambda: defaultdict(list))
        self._count: dict[tuple[str, str], int] = defaultdict(int)

    def add(self, split: str, kind: str, *, mosaic, rgb, wb, file_index: int, origin, mask=None) -> None:
        pending = self._pending[(split, kind)]
        pending["mosaic"].append(mosaic)
        pending["rgb"].append(rgb)
        pending["wb"].append(wb)
        pending["file_index"].append(file_index)
        pending["origin"].append(origin)
        if mask is not None:
            pending["mask"].append(mask)
        if len(pending["mosaic"]) >= PATCHES_PER_SHARD:
            self._flush(split, kind)

    def close(self) -> list[dict]:
        for split, kind in list(self._pending):
            self._flush(split, kind)
        return self.entries

    def _flush(self, split: str, kind: str) -> None:
        pending = self._pending.pop((split, kind), None)
        if not pending or not pending["mosaic"]:
            return
        index = self._count[(split, kind)]
        self._count[(split, kind)] += 1
        path = self.root / split / f"{kind}-{index:04d}.npz"
        path.parent.mkdir(parents=True, exist_ok=True)
        arrays = {
            "mosaic": np.stack(pending["mosaic"]).astype(np.float32),
            "rgb": np.stack(pending["rgb"]).astype(np.float32),
            "wb": np.stack(pending["wb"]).astype(np.float32),
            "file_index": np.asarray(pending["file_index"], np.int32),
            "origin": np.asarray(pending["origin"], np.int32),
        }
        if pending["mask"]:
            arrays["mask"] = np.stack(pending["mask"]).astype(np.uint8)
        part = path.with_name(path.name + ".part.npz")
        np.savez(part, **arrays)
        part.replace(path)
        self.entries.append(
            {
                "path": str(path.relative_to(self.root)),
                "split": split,
                "kind": kind,
                "patches": len(arrays["mosaic"]),
                "sha256": hashlib.sha256(path.read_bytes()).hexdigest(),
            }
        )
