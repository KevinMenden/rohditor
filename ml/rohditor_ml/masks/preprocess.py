"""Photo -> network input, and network output -> probability map."""

from __future__ import annotations

import numpy as np
from PIL import Image

from .models import IMAGENET_MEAN, IMAGENET_STD, MaskModel


def to_input(photo: Image.Image, size: int) -> np.ndarray:
    """`[1, 3, size, size]` float32: bilinear square resize, ImageNet normalisation."""
    rgb = np.asarray(photo.convert("RGB").resize((size, size), Image.BILINEAR), dtype=np.float32) / 255.0
    normalised = (rgb - np.float32(IMAGENET_MEAN)) / np.float32(IMAGENET_STD)
    return np.ascontiguousarray(normalised.transpose(2, 0, 1)[None], dtype=np.float32)


def to_probability(output: np.ndarray, model: MaskModel) -> np.ndarray:
    """The raw mask output as a probability in [0, 1], same shape."""
    output = output.astype(np.float32)
    if model.output_kind == "logits":
        # Stable sigmoid: no overflow in exp() for very negative logits.
        return 0.5 * (1.0 + np.tanh(0.5 * output))
    return np.clip(output, 0.0, 1.0)
