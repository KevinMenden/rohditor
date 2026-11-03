"""Deterministic, diverse subset selection for the `small` builds."""

from __future__ import annotations

import hashlib
from collections.abc import Mapping, Sequence

from .files import RemoteFile

# A bundle is files that must stay together (e.g. every capture of one scene).
Bundle = Sequence[RemoteFile]


def select_round_robin(groups: Mapping[str, Sequence[Bundle]], budget_bytes: int) -> list[RemoteFile]:
    """Take bundles from each group in turn until the byte budget is used up.

    Round-robin spreads the budget over groups (cameras, makes, splits) instead
    of exhausting the first one. Within a group the order is a hash of the
    bundle's first file name: stable across runs and machines, but not
    alphabetical. A bundle that no longer fits is skipped.
    """
    queues = {
        key: sorted(bundles, key=lambda b: hashlib.sha1(b[0].name.encode()).hexdigest())
        for key, bundles in sorted(groups.items())
    }
    chosen: list[RemoteFile] = []
    remaining = budget_bytes
    while any(queues.values()):
        for queue in queues.values():
            while queue:
                bundle = queue.pop(0)
                size = sum(f.size for f in bundle)
                if size <= remaining:
                    chosen.extend(bundle)
                    remaining -= size
                    break
    return chosen
