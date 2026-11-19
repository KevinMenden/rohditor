"""100% side-by-side crops (PNG), one strip per case: each method, then the truth."""

from __future__ import annotations

from pathlib import Path

import numpy as np
from PIL import Image, ImageDraw

from .metrics import to_display

LABEL_HEIGHT = 14


def exposure_for(reference: np.ndarray) -> float:
    """A display gain that puts the reference's 99th percentile at 0.9 (same for the whole strip)."""
    return 0.9 / max(float(np.percentile(reference, 99)), 1e-4)


def to_uint8(rgb: np.ndarray, gain: float) -> np.ndarray:
    return (to_display(rgb * gain).transpose(1, 2, 0) * 255 + 0.5).astype(np.uint8)


def save_strip(path: Path, panels: list[tuple[str, np.ndarray]], gain: float) -> None:
    """`panels` are `(label, [3, H, W])` balanced linear images of one size."""
    h, w = panels[0][1].shape[1:]
    strip = Image.new("RGB", (w * len(panels), h + LABEL_HEIGHT), "black")
    draw = ImageDraw.Draw(strip)
    for i, (label, rgb) in enumerate(panels):
        strip.paste(Image.fromarray(to_uint8(rgb, gain)), (i * w, LABEL_HEIGHT))
        draw.text((i * w + 2, 1), label, fill="white")
    path.parent.mkdir(parents=True, exist_ok=True)
    strip.save(path)
