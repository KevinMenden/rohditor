"""Run-folder bookkeeping: `ml/runs/<variant>/` holds the resolved config, the
exact command (`train.sh`), the CSV log, TensorBoard events (`tb/`) and the
checkpoints. View the runs side by side with `tensorboard --logdir ml/runs`."""

from __future__ import annotations

import csv
import shlex
import subprocess
import sys
import time
from pathlib import Path

import torch
import yaml
from torch.utils.tensorboard import SummaryWriter

from ..paths import ML_ROOT

LOG_FIELDS = ("time", "elapsed_s", "step", "stage", "stage_step", "lr", "loss", "val_loss", "val_psnr_clean",
              "val_psnr_real", "samples_per_s", "peak_mem_gb")


def git_commit() -> str:
    """The training commit, with `-dirty` when the work tree has changes."""
    def git(*args: str) -> str:
        return subprocess.run(["git", *args], cwd=ML_ROOT, capture_output=True, text=True, check=False).stdout.strip()

    commit = git("rev-parse", "HEAD") or "unknown"
    return commit + ("-dirty" if git("status", "--porcelain", "--", ".") else "")


def write_run_files(run_dir: Path, config: dict, argv: list[str]) -> None:
    run_dir.mkdir(parents=True, exist_ok=True)
    (run_dir / "config.yaml").write_text(yaml.safe_dump(config, sort_keys=False), encoding="utf-8")
    command = " ".join(shlex.quote(a) for a in ["python", "-m", "rohditor_ml.train", *argv])
    script = run_dir / "train.sh"
    script.write_text(
        "#!/usr/bin/env bash\n"
        f"# The exact training command, written by rohditor_ml.train on {time.strftime('%Y-%m-%d %H:%M:%S')}\n"
        f"# at commit {git_commit()}. Runs inside the rocm distrobox, from ml/.\n"
        "set -euo pipefail\n"
        f"cd {shlex.quote(str(ML_ROOT))}\n"
        f"UV_PROJECT_ENVIRONMENT=.venv-rocm uv run --group train {command}\n",
        encoding="utf-8",
    )
    script.chmod(0o755)


# CSV column -> TensorBoard tag; the other columns are bookkeeping.
SCALAR_TAGS = {"loss": "train/loss", "lr": "train/lr", "samples_per_s": "train/samples_per_s",
               "peak_mem_gb": "train/peak_mem_gb", "val_loss": "val/loss", "val_psnr_clean": "val/psnr_clean",
               "val_psnr_real": "val/psnr_real"}


class RunLog:
    """One log, three sinks: `log.csv`, TensorBoard events in `tb/`, and stderr."""

    def __init__(self, run_dir: Path):
        path = run_dir / "log.csv"
        new = not path.exists()
        self.file = path.open("a", newline="", encoding="utf-8")
        self.writer = csv.DictWriter(self.file, LOG_FIELDS)
        if new:
            self.writer.writeheader()
        self.tensorboard = SummaryWriter(run_dir / "tb")
        self.started = time.time()

    def write(self, **row) -> None:
        row.setdefault("time", time.strftime("%Y-%m-%d %H:%M:%S"))
        row.setdefault("elapsed_s", round(time.time() - self.started))
        self.writer.writerow({k: _fmt(v) for k, v in row.items()})
        self.file.flush()
        for key, tag in SCALAR_TAGS.items():
            if row.get(key) is not None:
                self.tensorboard.add_scalar(tag, row[key], row["step"])
        self.tensorboard.flush()
        print("  ".join(f"{k}={_fmt(v)}" for k, v in row.items() if k not in ("time",) and v != ""), file=sys.stderr)

    def image(self, tag: str, image: torch.Tensor, step: int) -> None:
        """A `[3, H, W]` image in [0, 1]."""
        self.tensorboard.add_image(tag, image, step)


def _fmt(value) -> str:
    if isinstance(value, float):
        return f"{value:.6g}"
    return "" if value is None else str(value)


def save_checkpoint(path: Path, **state) -> None:
    """Atomic save, so an interrupted run never leaves a truncated checkpoint."""
    path.parent.mkdir(parents=True, exist_ok=True)
    part = path.with_name(path.name + ".part")
    torch.save(state, part)
    part.replace(path)
