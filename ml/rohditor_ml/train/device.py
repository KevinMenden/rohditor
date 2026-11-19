"""Choosing the training device and reporting what was found."""

from __future__ import annotations

import torch


def select_device(requested: str = "auto") -> torch.device:
    """`auto` takes the GPU (ROCm shows up as `cuda` in PyTorch) when there is one."""
    print(f"torch {torch.__version__}, hip {torch.version.hip}, cuda available: {torch.cuda.is_available()}")
    if torch.cuda.is_available():
        print(f"device 0: {torch.cuda.get_device_name(0)}")
    if requested == "auto":
        requested = "cuda" if torch.cuda.is_available() else "cpu"
    device = torch.device(requested)
    print(f"training on {device}")
    return device
