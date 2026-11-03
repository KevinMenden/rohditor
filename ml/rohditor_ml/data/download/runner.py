"""Plan, report and execute downloads for a set of sources."""

from __future__ import annotations

import json
import shutil
import threading
from concurrent.futures import ThreadPoolExecutor, as_completed
from dataclasses import asdict
from pathlib import Path
from types import ModuleType

from .files import Listing, RemoteFile
from .http import download


def _gb(size: int) -> str:
    return f"{size / 1e9:.2f} GB"


def write_catalog(source_dir: Path, source: str, subset: str, files: list[RemoteFile]) -> None:
    """Record what was selected (urls, checksums, licenses) next to the data."""
    source_dir.mkdir(parents=True, exist_ok=True)
    catalog = {"source": source, "subset": subset, "files": [asdict(f) for f in sorted(files, key=lambda f: f.name)]}
    path = source_dir / "catalog.json"
    part = path.with_name(path.name + ".part")
    part.write_text(json.dumps(catalog, indent=1) + "\n", encoding="utf-8")
    part.replace(path)


def print_plan(name: str, listing: Listing) -> None:
    by_license: dict[str, list[int]] = {}
    for f in listing.files:
        by_license.setdefault(f.license, []).append(f.size)
    total = sum(f.size for f in listing.files)
    print(f"\n{name}: {len(listing.files)} files, {_gb(total)}")
    for license_id, sizes in sorted(by_license.items()):
        print(f"  license {license_id}: {len(sizes)} files, {_gb(sum(sizes))}")
    for reason, count in sorted(listing.skipped.items()):
        print(f"  skipped {count}: {reason}")


def run(
    sources: list[ModuleType],
    subset: str,
    raw_dir: Path,
    allowed: frozenset[str],
    jobs: int,
    dry_run: bool,
    verify: bool,
) -> bool:
    """Returns True when everything selected is present and verified."""
    plans: list[tuple[Path, Listing]] = []
    for source in sources:
        source_dir = raw_dir / source.NAME
        listing = source.list_files(source_dir, subset, allowed)
        print_plan(source.NAME, listing)
        write_catalog(source_dir, source.NAME, subset, listing.files)
        plans.append((source_dir, listing))
    if dry_run:
        return True

    jobs_to_run = [(d, f) for d, listing in plans for f in listing.files]
    needed = sum(f.size for d, f in jobs_to_run if not (d / f.name).exists())
    raw_dir.mkdir(parents=True, exist_ok=True)
    free = shutil.disk_usage(raw_dir).free
    if needed > free:
        print(f"\nNot enough disk space: need up to {_gb(needed)}, {_gb(free)} free.")
        return False

    print_lock = threading.Lock()
    failures: list[str] = []
    with ThreadPoolExecutor(jobs) as pool:
        futures = {pool.submit(download, f, d, verify=verify): f for d, f in jobs_to_run}
        for done, future in enumerate(as_completed(futures), 1):
            file = futures[future]
            try:
                status = future.result()
            except Exception as error:  # noqa: BLE001 - keep going, report at the end
                status = f"FAILED ({error})"
                failures.append(f"{file.source}/{file.name}: {error}")
            with print_lock:
                print(f"[{done}/{len(futures)}] {status}: {file.source}/{file.name}", flush=True)

    if failures:
        print(f"\n{len(failures)} file(s) failed; re-run to retry:")
        for failure in failures:
            print(f"  {failure}")
    return not failures

