"""Command line entry point: `python -m rohditor_ml.data.download`."""

from __future__ import annotations

import argparse
import sys
from pathlib import Path

from ...paths import RAW_DIR
from . import licenses, pixelshift200, pixls, rawnind
from .runner import run

SOURCES = {module.NAME: module for module in (rawnind, pixelshift200, pixls)}


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(
        prog="python -m rohditor_ml.data.download",
        description="Download the licensed raw datasets into ml/data/raw (resumable, checksum-verified).",
    )
    parser.add_argument("--subset", choices=["small", "full"], default="small",
                        help="small: about 5 GB for testing the pipeline; full: everything allowed")
    parser.add_argument("--source", choices=sorted(SOURCES), nargs="+", default=sorted(SOURCES))
    parser.add_argument("--raw-dir", type=Path, default=RAW_DIR)
    parser.add_argument("--jobs", type=int, default=4, help="parallel downloads")
    parser.add_argument("--allow-license", action="append", default=[], metavar="SPDX",
                        help=f"also download files under this license (default allowed: "
                             f"{', '.join(sorted(licenses.DEFAULT_ALLOWED))}); repeatable")
    parser.add_argument("--dry-run", action="store_true", help="list what would be downloaded, then stop")
    parser.add_argument("--verify", action="store_true", help="re-hash files that are already downloaded")
    args = parser.parse_args(argv)

    ok = run(
        [SOURCES[name] for name in args.source],
        args.subset,
        args.raw_dir,
        licenses.DEFAULT_ALLOWED | frozenset(args.allow_license),
        args.jobs,
        args.dry_run,
        args.verify,
    )
    return 0 if ok else 1


if __name__ == "__main__":
    sys.exit(main())
