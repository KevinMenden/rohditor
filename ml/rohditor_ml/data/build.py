"""One command from nothing to shards:

    uv run python -m rohditor_ml.data.build --subset small|full

Downloads (resume-safe, a no-op when everything is present), decodes, builds
ground truth, fits the noise model, cuts patches into shards and writes
`manifest.json`, `stats.txt` and a contact sheet next to them.
"""

from __future__ import annotations

import argparse
import shutil
import sys
from pathlib import Path

from ..paths import ML_ROOT, RAW_DIR
from . import process, report
from ..contract import CONTRACT
from .download import pixelshift200, pixls, rawnind
from .download.licenses import TRAINING_ALLOWED
from .download.runner import run as run_downloads
from .manifest import write_manifest
from .noise.fit import NOISE_PARAMS, fit_manifest, write_params
from .prepare import assign_splits, load_items
from .shards import ShardWriter

SOURCES = [pixelshift200, pixls, rawnind]


def build(args: argparse.Namespace) -> int:
    names = [module.NAME for module in SOURCES]
    if not args.skip_download:
        if not run_downloads(SOURCES, args.subset, args.raw_dir, TRAINING_ALLOWED, args.jobs, False, False):
            print("download failed; fix and re-run", file=sys.stderr)
            return 1

    items = load_items(args.raw_dir, names, TRAINING_ALLOWED)
    holdout = assign_splits(items, args.holdout_camera, args.seed)
    print(f"\n{len(items)} files; held-out camera for the unseen-sensor test: {holdout}")

    # Rebuild from scratch so shards never mix runs.
    if args.shards_dir.exists():
        shutil.rmtree(args.shards_dir)
    writer = ShardWriter(args.shards_dir)
    records, skipped = process.run(items, writer, args.seed, args.workers)
    shard_entries = writer.close()

    manifest = {
        "contract_version": CONTRACT["version"],
        "subset": args.subset,
        "seed": args.seed,
        "holdout_camera": holdout,
        "allowed_licenses": sorted(TRAINING_ALLOWED),
        "files": records,
        "skipped": skipped,
        "shards": shard_entries,
    }
    # Clean images get noise at training time from their own camera's fit, else a random fitted camera.
    noise = fit_manifest(manifest)
    noise.update({"subset": args.subset, "seed": args.seed})
    write_params(noise)
    write_manifest(manifest, args.shards_dir / "manifest.json")  # validates licenses first
    text = report.stats(manifest)
    (args.shards_dir / "stats.txt").write_text(text + "\n", encoding="utf-8")
    report.contact_sheet(args.shards_dir, manifest, args.shards_dir / "contact_sheet.png")
    print("\n" + text)
    print(f"\nshards: {args.shards_dir}\nnoise parameters: {NOISE_PARAMS}")
    return 0


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(prog="python -m rohditor_ml.data.build", description=__doc__)
    parser.add_argument("--subset", choices=["small", "full"], default="small")
    parser.add_argument("--raw-dir", type=Path, default=RAW_DIR)
    parser.add_argument("--shards-dir", type=Path, default=ML_ROOT / "data" / "shards")
    parser.add_argument("--seed", type=int, default=0)
    parser.add_argument("--holdout-camera", help="camera for the unseen-sensor test (default: chosen automatically)")
    parser.add_argument("--jobs", type=int, default=4, help="parallel downloads")
    # Each worker holds one scene in memory: a PixelShift200 scene needs a few GB.
    parser.add_argument("--workers", type=int, default=6, help="scenes processed in parallel")
    parser.add_argument("--skip-download", action="store_true", help="use whatever is already in --raw-dir")
    return build(parser.parse_args(argv))


if __name__ == "__main__":
    sys.exit(main())
