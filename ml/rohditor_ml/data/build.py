"""One command from nothing to shards:

    uv run python -m rohditor_ml.data.build --subset small|full

Downloads (resume-safe, a no-op when everything is present), decodes, builds
ground truth, fits the noise model, cuts patches into shards and writes
`manifest.json`, `stats.txt` and a contact sheet next to them.
"""

from __future__ import annotations

import argparse
import json
import shutil
import sys
from pathlib import Path

from ..paths import ML_ROOT, RAW_DIR
from . import report
from ..contract import CONTRACT
from .download import pixelshift200, pixls, rawnind
from .download.licenses import TRAINING_ALLOWED
from .download.runner import run as run_downloads
from .manifest import write_manifest
from .noise.fit import fit_all, group_by_camera
from .prepare import assign_splits, load_items
from .process import Processor
from .shards import ShardWriter

SOURCES = [pixelshift200, pixls, rawnind]
NOISE_PARAMS = ML_ROOT / "data" / "noise_params.json"


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
    processor = Processor(writer, args.seed)
    processor.run(items)
    shard_entries = writer.close()

    noise = fit_all(group_by_camera(processor.noise_estimates))
    noise.update({"subset": args.subset, "seed": args.seed})
    NOISE_PARAMS.write_text(json.dumps(noise, indent=1) + "\n", encoding="utf-8")
    known = set(noise["cameras"])
    for record in processor.records:
        # Clean images get noise at training time from their own camera's fit, else the pooled one.
        record["noise_profile"] = record["camera"] if record["camera"] in known else ("pooled" if noise["pooled"] else None)

    manifest = {
        "contract_version": CONTRACT["version"],
        "subset": args.subset,
        "seed": args.seed,
        "holdout_camera": holdout,
        "allowed_licenses": sorted(TRAINING_ALLOWED),
        "files": processor.records,
        "skipped": processor.skipped,
        "shards": shard_entries,
    }
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
    parser.add_argument("--skip-download", action="store_true", help="use whatever is already in --raw-dir")
    return build(parser.parse_args(argv))


if __name__ == "__main__":
    sys.exit(main())
