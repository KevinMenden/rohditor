"""Train one model variant:

    UV_PROJECT_ENVIRONMENT=.venv-rocm uv run --group train \\
        python -m rohditor_ml.train --config configs/unet_v1_shippable.yaml

Writes `ml/runs/<variant>/` (config, `train.sh`, `log.csv`, checkpoints, and
`model.pt` at the end). `--resume` continues from `checkpoints/last.pt`.
"""

from __future__ import annotations

import argparse
import sys
from pathlib import Path

import torch

from ..paths import ML_ROOT
from .config import load_config
from .device import select_device
from .run import write_run_files
from .trainer import Trainer


def main(argv: list[str] | None = None) -> int:
    argv = sys.argv[1:] if argv is None else argv
    parser = argparse.ArgumentParser(prog="python -m rohditor_ml.train", description=__doc__,
                                     formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--config", type=Path, required=True)
    parser.add_argument("--run-dir", type=Path, help="default: ml/runs/<variant>")
    parser.add_argument("--device", default="auto", help="auto, cuda or cpu")
    parser.add_argument("--resume", action="store_true", help="continue from <run-dir>/checkpoints/last.pt")
    args = parser.parse_args(argv)

    config = load_config(args.config)
    run_dir = args.run_dir or ML_ROOT / "runs" / config["variant"]
    last = run_dir / "checkpoints" / "last.pt"
    if last.exists() and not args.resume:
        parser.error(f"{run_dir} already has a run; pass --resume or choose another --run-dir")
    device = select_device(args.device)
    torch.backends.cudnn.benchmark = True
    torch.manual_seed(config["train"]["seed"])
    write_run_files(run_dir, config, argv)
    trainer = Trainer(config, run_dir, device)
    if args.resume and last.exists():
        trainer.resume(last)
    trainer.run()
    return 0


if __name__ == "__main__":
    sys.exit(main())
