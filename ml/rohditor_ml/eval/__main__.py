"""Evaluate trained variants against Rohditor's classic demosaics:

    UV_PROJECT_ENVIRONMENT=.venv-rocm uv run --group train \\
        python -m rohditor_ml.eval --run-dirs runs/shippable runs/full

Writes `ml/runs/eval/`: `scores.csv` (every case and method), `report.md`
(tables, the RCD + noise-reduction baseline chosen per tier, the go/no-go gate),
`summary.json`, and 100% crops under `crops/` (`crops/private/` for the private
corpus, visual only).
"""

from __future__ import annotations

import argparse
import csv
import json
import sys
from collections import defaultdict
from pathlib import Path

import numpy as np
import torch

from ..paths import ML_ROOT
from ..train.device import select_device
from ..train.export import load_trained
from .crops import exposure_for, save_strip
from .methods import Network, classic_methods, ensure_cli
from .metrics import scores
from .private import private_crops
from .report import BASELINE, METRICS, add_best_baseline, aggregate, gate, markdown
from .sets import build_sets

# (chroma, luminance) strengths tried for the RCD + noise-reduction baseline.
NR_SETTINGS = [(0.25, 0.0), (0.5, 0.25), (0.5, 0.5), (0.75, 0.75), (1.0, 1.0)]
COUNTS = {"pixelshift_sweep": 96, "real_pairs": 384, "unseen_camera": 64}


def evaluate(cases, methods, crop_count: int) -> tuple[list[dict], dict]:
    """Run every method on every case; returns score rows and the outputs kept for crops."""
    groups = defaultdict(list)
    for case in cases:
        groups[(case.set, case.tier)].append(case)
    rows, kept = [], {}
    for (set_name, tier), group in sorted(groups.items()):
        print(f"  {set_name} / {tier}: {len(group)} cases")
        mosaics = np.stack([c.mosaic for c in group])
        gains = np.stack([c.gains for c in group])
        crop_ids = {c.id for c in group[:crop_count]} if set_name != "charts" else {c.id for c in group}
        for method in methods:
            outputs = method.run(mosaics, gains)
            for case, output in zip(group, outputs, strict=True):
                rows.append({"set": set_name, "tier": tier, "id": case.id, "method": method.name,
                             **scores(output, case.target, case.mask, case.binned)})
                if case.id in crop_ids:
                    kept[(set_name, tier, case.id, method.name)] = output
    return rows, kept


def write_crops(out_dir: Path, cases, kept: dict, chosen: dict, networks: list[str]) -> None:
    for case in cases:
        key = (case.set, case.tier, case.id)
        if (*key, networks[0]) not in kept:
            continue
        shown = ["mhc", "rcd", chosen.get((case.set, case.tier), "rcd"), *networks]
        panels = [(BASELINE if i == 2 else name, kept[(*key, name)]) for i, name in enumerate(shown)]
        panels.append(("truth", case.target))
        save_strip(out_dir / "crops" / f"{case.set}_{case.tier}_{case.id}.png", panels, exposure_for(case.target))


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(prog="python -m rohditor_ml.eval", description=__doc__,
                                     formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--run-dirs", type=Path, nargs="+", required=True)
    parser.add_argument("--out-dir", type=Path, default=ML_ROOT / "runs" / "eval")
    parser.add_argument("--npy-dir", type=Path, default=ML_ROOT / "data" / "npy")
    parser.add_argument("--noise-params", type=Path, default=ML_ROOT / "data" / "noise_params.json")
    parser.add_argument("--sweep-camera", default="sony ilce-7c",
                        help="fitted camera whose noise range defines the synthetic tiers")
    parser.add_argument("--crops", type=int, default=2, help="crops per set and tier")
    parser.add_argument("--private-limit", type=int, default=None, help="private raws to crop (0: none)")
    parser.add_argument("--device", default="auto")
    args = parser.parse_args(argv)

    ensure_cli()
    device = select_device(args.device)
    networks = []
    for run_dir in args.run_dirs:
        model, config, _ = load_trained(run_dir)
        networks.append(Network(config["variant"], model.to(device), device))
    plain, nr = classic_methods(NR_SETTINGS)

    noise_doc = json.loads(args.noise_params.read_text(encoding="utf-8"))
    cases = build_sets(args.npy_dir, noise_doc, args.sweep_camera, COUNTS)
    print(f"{len(cases)} cases")
    rows, kept = evaluate(cases, [*plain, *nr, *networks], args.crops)

    args.out_dir.mkdir(parents=True, exist_ok=True)
    with (args.out_dir / "scores.csv").open("w", newline="", encoding="utf-8") as file:
        writer = csv.DictWriter(file, ["set", "tier", "id", "method", *METRICS])
        writer.writeheader()
        writer.writerows(rows)
    table = aggregate(rows)
    chosen = add_best_baseline(table, ["rcd", *(m.name for m in nr)])
    names = [n.name for n in networks]
    gates = {name: gate(table, name) for name in names}
    method_names = [m.name for m in plain] + names
    (args.out_dir / "report.md").write_text(markdown(table, chosen, gates, method_names), encoding="utf-8")
    summary = {
        "table": [{"set": s, "tier": t, "method": m, **v} for (s, t, m), v in sorted(table.items())],
        "baseline_choice": [{"set": s, "tier": t, "method": m} for (s, t), m in sorted(chosen.items())],
        "gate": {name: [{**c.__dict__, "passed": c.passed} for c in checks] for name, checks in gates.items()},
    }
    (args.out_dir / "summary.json").write_text(json.dumps(summary, indent=1) + "\n", encoding="utf-8")
    write_crops(args.out_dir, cases, kept, chosen, names)

    if args.private_limit != 0:
        middle = nr[len(nr) // 2]
        count = private_crops([plain[1], middle, *networks], args.out_dir / "crops" / "private", args.private_limit)
        print(f"{count} private crops (visual only, never publish)")
    print((args.out_dir / "report.md").read_text(encoding="utf-8"))
    return 0


if __name__ == "__main__":
    torch.set_grad_enabled(False)
    sys.exit(main())
