"""Looking up the noise parameters for a manifest file's `noise_profile`.

A profile is a camera name from `noise_params.json`, or `"pooled"` for files
whose camera has no fit (most raw.pixls.us cameras, and unseen sensors). Pooled
draws a random fitted camera each time, so every sample is a real sensor's
noise rather than an average of several.
"""

from __future__ import annotations

import numpy as np

from .model import NoiseParams

POOLED = "pooled"


def params_for(profile: str, params_doc: dict, rng: np.random.Generator) -> NoiseParams:
    cameras = params_doc["cameras"]
    if profile != POOLED:
        return NoiseParams.from_dict(cameras[profile])
    names = sorted(cameras)
    return NoiseParams.from_dict(cameras[names[rng.integers(len(names))]])


def profile_for(camera: str, params_doc: dict) -> str:
    return camera if camera in params_doc["cameras"] else POOLED
