"""Memory-mapped patch arrays for training.

The `.npz` shards from the dataset build are converted once into one `.npy`
array per field and per (split, kind), which can be memory-mapped: the full set
is larger than what fits in RAM next to training, and one array per field makes
a uniform shuffle across all shards a plain random index.

    uv run python -m rohditor_ml.train.store            # data/shards -> data/npy

Layout: `<root>/<split>/<kind>/{mosaic,rgb,wb,file_index,mask}.npy` plus a
`source.json` that records the manifest's shard hashes, so an unchanged split
is skipped and a rebuilt one is converted again.
"""

from __future__ import annotations

import argparse
import json
import shutil
import sys
from collections import defaultdict
from dataclasses import dataclass
from pathlib import Path

import numpy as np

from ..paths import ML_ROOT

SHARDS_DIR = ML_ROOT / "data" / "shards"
NPY_DIR = ML_ROOT / "data" / "npy"
FIELDS = ("mosaic", "rgb", "wb", "file_index", "mask")


def _convert_group(shards_dir: Path, entries: list[dict], out: Path) -> None:
    """Concatenate the shards of one (split, kind) into `out` (written via a `.part` folder)."""
    part = out.with_name(out.name + ".part")
    if part.exists():
        shutil.rmtree(part)
    part.mkdir(parents=True)
    total = sum(e["patches"] for e in entries)
    arrays: dict[str, np.ndarray] = {}
    start = 0
    for n, entry in enumerate(entries, 1):
        with np.load(shards_dir / entry["path"]) as shard:
            count = len(shard["mosaic"])
            for field in FIELDS:
                if field not in shard:
                    continue
                values = shard[field]
                if field not in arrays:
                    arrays[field] = np.lib.format.open_memmap(
                        part / f"{field}.npy", mode="w+", dtype=values.dtype, shape=(total, *values.shape[1:])
                    )
                arrays[field][start : start + count] = values
            start += count
        print(f"\r  {out.parent.name}/{out.name}: {n}/{len(entries)} shards", end="", flush=True)
    print()
    for array in arrays.values():
        array.flush()
    del arrays
    (part / "source.json").write_text(json.dumps([e["sha256"] for e in entries]) + "\n", encoding="utf-8")
    if out.exists():
        shutil.rmtree(out)
    part.replace(out)


def convert(shards_dir: Path = SHARDS_DIR, out_dir: Path = NPY_DIR) -> None:
    manifest = json.loads((shards_dir / "manifest.json").read_text(encoding="utf-8"))
    groups: dict[tuple[str, str], list[dict]] = defaultdict(list)
    for entry in manifest["shards"]:
        groups[(entry["split"], entry["kind"])].append(entry)
    for (split, kind), entries in sorted(groups.items()):
        out = out_dir / split / kind
        hashes = [e["sha256"] for e in entries]
        source = out / "source.json"
        if source.exists() and json.loads(source.read_text(encoding="utf-8")) == hashes:
            print(f"  {split}/{kind}: up to date")
            continue
        _convert_group(shards_dir, entries, out)
    shutil.copy2(shards_dir / "manifest.json", out_dir / "manifest.json")


@dataclass
class PatchArrays:
    """The arrays of one (split, kind), memory-mapped read-only (or in memory, for tests)."""

    mosaic: np.ndarray  # [N, P, P]
    rgb: np.ndarray  # [N, 3, P, P]
    wb: np.ndarray  # [N, 3]
    file_index: np.ndarray  # [N]
    mask: np.ndarray | None = None  # [N, P, P], real pairs only

    def __len__(self) -> int:
        return len(self.mosaic)

    @classmethod
    def open(cls, root: Path, split: str, kind: str) -> PatchArrays | None:
        folder = root / split / kind
        if not (folder / "mosaic.npy").exists():
            return None
        arrays = {f: np.load(folder / f"{f}.npy", mmap_mode="r") for f in FIELDS if (folder / f"{f}.npy").exists()}
        return cls(**arrays)


def warm_page_cache(root: Path, split: str, chunk: int = 64 << 20) -> int:
    """Read a split's arrays once, sequentially, so the OS page cache holds them.

    Training reads random patches; from a cold cache each one is a separate
    small read, which starves the GPU. A sequential pass runs at full disk
    speed. Returns the bytes read.
    """
    total = 0
    for path in sorted((root / split).glob("*/*.npy")):
        with path.open("rb", buffering=0) as file:
            while block := file.read(chunk):
                total += len(block)
    return total


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(prog="python -m rohditor_ml.train.store", description=__doc__)
    parser.add_argument("--shards-dir", type=Path, default=SHARDS_DIR)
    parser.add_argument("--out-dir", type=Path, default=NPY_DIR)
    args = parser.parse_args(argv)
    convert(args.shards_dir, args.out_dir)
    return 0


if __name__ == "__main__":
    sys.exit(main())
