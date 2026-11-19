"""Aggregating per-case scores into tables, the RCD + noise-reduction baseline and
the go/no-go gate."""

from __future__ import annotations

from collections import defaultdict
from dataclasses import dataclass

import numpy as np

from .sets import tier_key

METRICS = ("psnr", "ssim", "psnr_display", "ssim_display")
BASELINE = "rcd+nr(best)"


def aggregate(rows: list[dict]) -> dict[tuple[str, str, str], dict]:
    """Mean of every metric per (set, tier, method), plus the case count."""
    groups: dict[tuple[str, str, str], list[dict]] = defaultdict(list)
    for row in rows:
        groups[(row["set"], row["tier"], row["method"])].append(row)
    return {key: {"n": len(g), **{m: float(np.mean([r[m] for r in g])) for m in METRICS}} for key, g in groups.items()}


def add_best_baseline(table: dict, candidates: list[str], select_by: str = "psnr") -> dict[tuple[str, str], str]:
    """Add `rcd+nr(best)`: per (set, tier), the noise-reduction setting with the best mean
    `select_by` (plain RCD included, so noise reduction can only help). Returns the choice."""
    chosen = {}
    for set_name, tier in {(s, t) for s, t, _ in table}:
        options = [c for c in candidates if (set_name, tier, c) in table]
        if not options:
            continue
        best = max(options, key=lambda c: table[(set_name, tier, c)][select_by])
        table[(set_name, tier, BASELINE)] = table[(set_name, tier, best)]
        chosen[(set_name, tier)] = best
    return chosen


@dataclass(frozen=True)
class GateCheck:
    set: str
    tier: str
    required_db: float  # minimum PSNR gain over the baseline (negative: allowed loss)
    gain_db: float

    @property
    def passed(self) -> bool:
        return self.gain_db >= self.required_db


def gate(table: dict, model: str) -> list[GateCheck]:
    """The proposed gate (issue #117) on the held-out sets with ground truth:
    >= +1 dB linear PSNR over RCD + NR at the top ISO tier, >= -0.3 dB at base ISO."""
    checks = []
    for set_name in ("pixelshift_sweep", "real_pairs"):
        tiers = sorted({t for s, t, _ in table if s == set_name and t != "clean"}, key=tier_key)
        if not tiers:
            continue
        for tier, required in ((tiers[-1], 1.0), (tiers[0], -0.3)):
            key, ref = (set_name, tier, model), (set_name, tier, BASELINE)
            if key in table and ref in table:
                checks.append(GateCheck(set_name, tier, required, table[key]["psnr"] - table[ref]["psnr"]))
    return checks


def markdown(table: dict, chosen: dict, gates: dict[str, list[GateCheck]], methods: list[str]) -> str:
    lines = ["# Neural demosaic evaluation", ""]
    for variant, checks in gates.items():
        verdict = "PASS" if checks and all(c.passed for c in checks) else "FAIL"
        lines += [f"## Gate: `{variant}` {verdict}", "", "| set | tier | required dB | gain dB | |", "|---|---|---|---|---|"]
        lines += [f"| {c.set} | {c.tier} | {c.required_db:+.1f} | {c.gain_db:+.2f} | {'ok' if c.passed else 'MISS'} |"
                  for c in checks]
        lines.append("")
    lines += ["Charts (zone plate, slanted edge) have no numeric gate: check `crops/charts_*.png` for artifacts.", ""]
    for set_name in sorted({s for s, _, _ in table}):
        tiers = sorted({t for s, t, _ in table if s == set_name}, key=tier_key)
        lines += [f"## {set_name}", "", "PSNR dB linear / display, SSIM display", "",
                  "| method | " + " | ".join(f"{t} (n={table[(set_name, t, methods[0])]['n']})" for t in tiers) + " |",
                  "|---|" + "---|" * len(tiers)]
        for method in [*methods, BASELINE]:
            cells = []
            for t in tiers:
                v = table.get((set_name, t, method))
                cells.append(f"{v['psnr']:.2f} / {v['psnr_display']:.2f} / {v['ssim_display']:.4f}" if v else "")
            lines.append(f"| {method} | " + " | ".join(cells) + " |")
        choice = ", ".join(f"{t}: {chosen[(set_name, t)]}" for t in tiers if (set_name, t) in chosen)
        lines += ["", f"`{BASELINE}` per tier: {choice}", ""]
    return "\n".join(lines) + "\n"
