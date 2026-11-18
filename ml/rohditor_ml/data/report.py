"""Sanity outputs: dataset statistics and a contact sheet of clean vs noisy patches.

    uv run python -m rohditor_ml.data.report [--shards DIR]
"""

from __future__ import annotations

import argparse
import json
from collections import Counter, defaultdict
from pathlib import Path

import numpy as np
from PIL import Image

from ..paths import ML_ROOT
from . import bayer
from .noise import NoiseParams, add_noise

SHARDS_DIR = ML_ROOT / "data" / "shards"
NOISE_PARAMS = ML_ROOT / "data" / "noise_params.json"
FALLBACK_NOISE = NoiseParams(k_min=1e-4, k_max=2e-3, sigma_log_slope=0.5, sigma_log_intercept=-2.4)
CELL = 256


def stats(manifest: dict) -> str:
    """Counts per camera/ISO/split and the license of every source, as text."""
    files = manifest["files"]
    lines = [f"{len(files)} files, {sum(f['patches'] for f in files)} patches, "
             f"{len(manifest.get('skipped', []))} files skipped"]
    lines.append("\nLicense per source (files / patches):")
    by_source: dict[tuple[str, str], list[int]] = defaultdict(lambda: [0, 0])
    for f in files:
        counts = by_source[(f["source"], f["license"])]
        counts[0] += 1
        counts[1] += f["patches"]
    lines += [f"  {s:14s} {lic:12s} {c[0]:5d} / {c[1]:6d}" for (s, lic), c in sorted(by_source.items())]
    lines.append("\nPatches per split:")
    split_counts = Counter()
    for f in files:
        split_counts[(f["split"], f["role"])] += f["patches"]
    lines += [f"  {s:20s} {role:6s} {n:6d}" for (s, role), n in sorted(split_counts.items())]
    lines.append("\nFiles / patches per camera:")
    by_camera: dict[str, list[int]] = defaultdict(lambda: [0, 0])
    for f in files:
        by_camera[f["camera"]][0] += 1
        by_camera[f["camera"]][1] += f["patches"]
    lines += [f"  {c:34s} {n[0]:5d} / {n[1]:6d}" for c, n in sorted(by_camera.items())]
    lines.append("\nPatches per camera and ISO bucket:")
    iso_counts: dict[str, Counter] = defaultdict(Counter)
    for f in files:
        iso = f["iso"]
        bucket = "unknown" if iso is None else f"<=400" if iso <= 400 else f"<=3200" if iso <= 3200 else ">3200"
        iso_counts[f["camera"]][bucket] += f["patches"]
    for camera, buckets in sorted(iso_counts.items()):
        lines.append(f"  {camera:34s} " + "  ".join(f"{b}: {n}" for b, n in sorted(buckets.items())))
    return "\n".join(lines)


def _view(rgb: np.ndarray, wb: np.ndarray, exposure: float) -> np.ndarray:
    """Linear `[3, H, W]` -> displayable uint8 `[H, W, 3]` (white balance, exposure, gamma)."""
    display = np.clip(rgb * wb[:, None, None] * exposure, 0, 1) ** (1 / 2.2)
    return (display.transpose(1, 2, 0) * 255).astype(np.uint8)


def _mosaic_view(mosaic: np.ndarray, wb: np.ndarray, exposure: float) -> np.ndarray:
    """A mosaic shown as its half-size packed RGB, enlarged 2x so it lines up with the target."""
    packed = bayer.pack(mosaic)
    rgb = np.stack([packed[0], 0.5 * (packed[1] + packed[2]), packed[3]])
    return np.kron(_view(rgb, wb, exposure), np.ones((2, 2, 1), np.uint8))


def contact_sheet(shards_dir: Path, manifest: dict, out: Path, count: int = 12, seed: int = 0) -> None:
    """Random clean patches: [target | clean mosaic | mosaic with synthetic noise], two patches per row."""
    rng = np.random.default_rng(seed)
    entries = [e for e in manifest["shards"] if e["kind"] == "clean"]
    params_doc = json.loads(NOISE_PARAMS.read_text()) if NOISE_PARAMS.exists() else {}
    cells: list[np.ndarray] = []
    for _ in range(count):
        entry = entries[rng.integers(len(entries))]
        with np.load(shards_dir / entry["path"]) as shard:
            i = int(rng.integers(len(shard["mosaic"])))
            mosaic, rgb, wb, file_index = shard["mosaic"][i], shard["rgb"][i], shard["wb"][i], int(shard["file_index"][i])
        record = manifest["files"][file_index]
        params = _noise_for(record, params_doc)
        noisy = add_noise(mosaic, params, rng)
        exposure = float(np.clip(0.18 / max(np.median(rgb.mean(axis=0)), 1e-4), 1, 200))
        cells += [_view(rgb, wb, exposure), _mosaic_view(mosaic, wb, exposure), _mosaic_view(noisy, wb, exposure)]
    columns = 6
    rows = [np.concatenate(cells[r * columns : (r + 1) * columns], axis=1) for r in range(len(cells) // columns)]
    out.parent.mkdir(parents=True, exist_ok=True)
    Image.fromarray(np.concatenate(rows, axis=0)).save(out)


def _noise_for(record: dict, params_doc: dict) -> NoiseParams:
    profile = record.get("noise_profile")
    values = params_doc.get("cameras", {}).get(profile) or params_doc.get("pooled")
    return NoiseParams.from_dict(values) if values else FALLBACK_NOISE


def main() -> None:
    parser = argparse.ArgumentParser(description="Statistics and a contact sheet for built shards.")
    parser.add_argument("--shards", type=Path, default=SHARDS_DIR)
    args = parser.parse_args()
    manifest = json.loads((args.shards / "manifest.json").read_text())
    print(stats(manifest))
    contact_sheet(args.shards, manifest, args.shards / "contact_sheet.png")
    print(f"\ncontact sheet: {args.shards / 'contact_sheet.png'}")


if __name__ == "__main__":
    main()
