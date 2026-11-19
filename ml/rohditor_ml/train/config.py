"""Training configs (`ml/configs/*.yaml`).

A config may name a `base:` file (relative to itself); its values are merged
under the config's own, recursively for mappings. Lists (such as `stages`)
are replaced as a whole.
"""

from __future__ import annotations

from pathlib import Path

import yaml


def _merge(base: dict, override: dict) -> dict:
    merged = dict(base)
    for key, value in override.items():
        if isinstance(value, dict) and isinstance(merged.get(key), dict):
            merged[key] = _merge(merged[key], value)
        else:
            merged[key] = value
    return merged


def load_config(path: Path) -> dict:
    path = Path(path)
    config = yaml.safe_load(path.read_text(encoding="utf-8")) or {}
    base = config.pop("base", None)
    if base is not None:
        config = _merge(load_config(path.parent / base), config)
    return config
