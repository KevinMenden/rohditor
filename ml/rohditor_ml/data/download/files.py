"""Data types shared by all sources."""

from __future__ import annotations

from collections import Counter
from dataclasses import dataclass, field
from typing import Any


@dataclass(frozen=True)
class Checksum:
    algorithm: str  # a hashlib name: "md5", "sha1", "sha256"
    hexdigest: str


@dataclass(frozen=True)
class RemoteFile:
    """One downloadable file, with everything the manifest needs later."""

    source: str
    name: str  # file name below the source's directory
    url: str
    size: int  # bytes; an estimate when `exact_size` is False
    license: str  # SPDX id
    checksum: Checksum | None = None
    exact_size: bool = True
    # Source-specific facts (scene, ISO, camera, ...) carried into the catalog.
    meta: dict[str, Any] = field(default_factory=dict, compare=False, hash=False)


@dataclass
class Listing:
    """The files chosen for one source, plus why others were left out."""

    files: list[RemoteFile] = field(default_factory=list)
    skipped: Counter[str] = field(default_factory=Counter)
