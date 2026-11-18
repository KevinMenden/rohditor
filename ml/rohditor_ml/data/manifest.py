"""The dataset manifest: one record per source file, one per shard.

`validate_manifest` is the licensing and privacy gate: it fails if any file has a
license outside the allowed list, lacks provenance, or points into the private
corpus. It runs at the end of every build and in the test-suite.
"""

from __future__ import annotations

import json
from pathlib import Path

from ..paths import PRIVATE_DIR
from .download.licenses import TRAINING_ALLOWED

REQUIRED_FILE_FIELDS = ("source", "name", "license", "camera", "scene", "split", "sha256")


class ManifestError(Exception):
    pass


def validate_manifest(manifest: dict, allowed: frozenset[str] = TRAINING_ALLOWED) -> None:
    problems: list[str] = []
    for record in manifest["files"]:
        missing = [f for f in REQUIRED_FILE_FIELDS if not record.get(f)]
        if missing:
            problems.append(f"{record.get('name')}: missing {missing}")
        if record.get("license") not in allowed:
            problems.append(f"{record.get('name')}: license {record.get('license')!r} is not allowed")
        if str(PRIVATE_DIR) in json.dumps(record) or "testdata/private" in json.dumps(record):
            problems.append(f"{record.get('name')}: refers to the private corpus")
    if problems:
        raise ManifestError("invalid manifest:\n  " + "\n  ".join(problems))


def write_manifest(manifest: dict, path: Path) -> None:
    validate_manifest(manifest)
    part = path.with_name(path.name + ".part")
    part.write_text(json.dumps(manifest, indent=1) + "\n", encoding="utf-8")
    part.replace(path)
