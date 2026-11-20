"""The training loop: stages in order, AdamW, warmup + cosine per stage, bf16 autocast."""

from __future__ import annotations

import json
import math
import time
from dataclasses import dataclass
from pathlib import Path

import torch
from torch.utils.data import DataLoader

from ..paths import ML_ROOT
from .dataset import Mix, SampleStream, SplitData, fixed_samples, training_noise_doc
from .loss import loss_per_sample, psnr
from .model import UNet, macs_per_megapixel, parameter_count
from .preview import preview_strip
from .run import RunLog, git_commit, save_checkpoint
from .store import warm_page_cache


@dataclass(frozen=True)
class Stage:
    name: str
    steps: int
    lr: float
    warmup: int
    real_fraction: float

    def lr_at(self, step: int) -> float:
        if step < self.warmup:
            return self.lr * (step + 1) / self.warmup
        t = (step - self.warmup) / max(1, self.steps - self.warmup)
        return self.lr * 0.5 * (1 + math.cos(math.pi * min(t, 1.0)))


def build_model(config: dict) -> UNet:
    return UNet(tuple(config["model"]["channels"]))


def describe_model(model: UNet) -> tuple[int, float]:
    params, macs = parameter_count(model), macs_per_megapixel(model)
    print(f"model: {params / 1e6:.2f}M parameters, {macs / 1e9:.1f} GMAC/MP")
    return params, macs


class Trainer:
    def __init__(self, config: dict, run_dir: Path, device: torch.device):
        self.config, self.run_dir, self.device = config, run_dir, device
        data_cfg, self.train_cfg = config["data"], config["train"]
        self.stages = [Stage(**s) for s in config["stages"]]
        root = ML_ROOT / data_cfg["npy_dir"]
        manifest = json.loads((root / "manifest.json").read_text(encoding="utf-8"))
        noise = json.loads((ML_ROOT / data_cfg["noise_params"]).read_text(encoding="utf-8"))
        self.noise_doc = training_noise_doc(noise, manifest.get("holdout_camera"))
        licenses = set(config["licenses"])
        limit, seed = data_cfg.get("limit_patches"), self.train_cfg["seed"]
        self.train_data = SplitData.load(root, "train", licenses, limit, seed)
        self.val_data = SplitData.load(root, "val", licenses, limit, seed)
        print(f"train: {self.train_data.describe()}\nval:   {self.val_data.describe()}")
        if data_cfg.get("warm_cache", True):
            started = time.time()
            size = warm_page_cache(root, "train")
            print(f"read {size / 2**30:.1f} GB of training data into the page cache in {time.time() - started:.0f} s")

        self.model = build_model(config).to(device)
        describe_model(self.model)
        self.optimizer = torch.optim.AdamW(self.model.parameters(), lr=self.stages[0].lr,
                                           weight_decay=self.train_cfg["weight_decay"])
        self.autocast = device.type == "cuda" and self.train_cfg["bf16"]
        self.stage_index, self.stage_step, self.global_step = 0, 0, 0
        run_dir.mkdir(parents=True, exist_ok=True)
        self.log = RunLog(run_dir)

    # -- state ------------------------------------------------------------------

    def _mix(self, stage: Stage) -> Mix:
        d = self.config["data"]
        return Mix(stage.real_fraction, d["pixelshift_fraction"], d["procedural_fraction"], d["crop"], d["exposure_min"])

    def checkpoint(self, name: str) -> None:
        save_checkpoint(
            self.run_dir / "checkpoints" / name,
            model=self.model.state_dict(), optimizer=self.optimizer.state_dict(), config=self.config,
            stage_index=self.stage_index, stage_step=self.stage_step, global_step=self.global_step,
            git_commit=git_commit(),
        )

    def resume(self, path: Path) -> None:
        state = torch.load(path, map_location=self.device, weights_only=False)
        self.model.load_state_dict(state["model"])
        self.optimizer.load_state_dict(state["optimizer"])
        self.stage_index, self.stage_step, self.global_step = state["stage_index"], state["stage_step"], state["global_step"]
        print(f"resumed from {path} at stage {self.stage_index} step {self.stage_step}")

    # -- loop -------------------------------------------------------------------

    def _loader(self, stage: Stage) -> DataLoader:
        seed = [self.train_cfg["seed"], self.stage_index, self.stage_step]  # fresh samples after a resume
        stream = SampleStream(self.train_data, self._mix(stage), self.noise_doc, hash(tuple(seed)) & 0xFFFFFFFF)
        workers = self.config["data"]["workers"]
        return DataLoader(stream, batch_size=self.train_cfg["batch_size"], num_workers=workers,
                          pin_memory=self.device.type == "cuda", persistent_workers=workers > 0,
                          prefetch_factor=4 if workers > 0 else None)

    def _forward(self, inputs: torch.Tensor) -> torch.Tensor:
        with torch.autocast(self.device.type, dtype=torch.bfloat16, enabled=self.autocast):
            return self.model(inputs)

    def run(self) -> None:
        val_set = None
        while self.stage_index < len(self.stages):
            stage = self.stages[self.stage_index]
            val_set = val_set or fixed_samples(self.val_data, self._mix(stage), self.noise_doc,
                                               self.train_cfg["val_clean"], self.train_cfg["val_real"], seed=1)
            print(f"stage {self.stage_index} ({stage.name}): steps {self.stage_step}..{stage.steps}")
            self._run_stage(stage, val_set)
            self.checkpoint(f"stage{self.stage_index}-{stage.name}.pt")
            self.stage_index, self.stage_step = self.stage_index + 1, 0
            self.checkpoint("last.pt")
        torch.save(self.model.state_dict(), self.run_dir / "model.pt")
        print(f"done: {self.run_dir / 'model.pt'}")

    def _run_stage(self, stage: Stage, val_set: list) -> None:
        cfg = self.train_cfg
        self.model.train()
        losses, window_start = [], time.time()
        if self.device.type == "cuda":
            torch.cuda.reset_peak_memory_stats()
        batches = iter(self._loader(stage))
        while self.stage_step < stage.steps:
            inputs, target, mask, real = (t.to(self.device, non_blocking=True) for t in next(batches))
            lr = stage.lr_at(self.stage_step)
            for group in self.optimizer.param_groups:
                group["lr"] = lr
            loss = loss_per_sample(self._forward(inputs), target, mask, real).mean()
            self.optimizer.zero_grad(set_to_none=True)
            loss.backward()
            torch.nn.utils.clip_grad_norm_(self.model.parameters(), cfg["grad_clip"])
            self.optimizer.step()
            losses.append(loss.detach())
            self.stage_step += 1
            self.global_step += 1

            row = {}
            if self.stage_step % cfg["log_every"] == 0 or self.stage_step == stage.steps:
                elapsed = time.time() - window_start
                row = {"loss": float(torch.stack(losses).mean()), "lr": lr,
                       "samples_per_s": len(losses) * cfg["batch_size"] / elapsed}
                if self.device.type == "cuda":
                    row["peak_mem_gb"] = torch.cuda.max_memory_allocated() / 2**30
                losses, window_start = [], time.time()
            if self.stage_step % cfg["val_every"] == 0 or self.stage_step == stage.steps:
                row.update(self.validate(val_set))
                self.log_previews(val_set)
            if row:
                self.log.write(step=self.global_step, stage=stage.name, stage_step=self.stage_step, **row)
            if self.stage_step % cfg["checkpoint_every"] == 0:
                self.checkpoint("last.pt")

    @torch.no_grad()
    def log_previews(self, val_set: list, count: int = 3) -> None:
        """The first `count` synthetic and real validation samples (the set is fixed, so
        each TensorBoard image tag follows the same patch through training)."""
        self.model.eval()
        clean = [s for s in val_set if s[3] < 0.5][:count]
        real = [s for s in val_set if s[3] > 0.5][:count]
        for kind, samples in (("synthetic", clean), ("real", real)):
            for i, (inputs, target, _, _) in enumerate(samples):
                inputs = inputs[None].to(self.device)
                output = self._forward(inputs)[0]
                strip = preview_strip(self.model.bilinear(inputs)[0], output, target.to(self.device))
                self.log.image(f"val_{kind}/{i}", strip.cpu(), self.global_step)
        self.model.train()

    @torch.no_grad()
    def validate(self, val_set: list) -> dict:
        self.model.eval()
        losses, psnrs, reals = [], [], []
        size = self.train_cfg["batch_size"]
        for i in range(0, len(val_set), size):
            inputs, target, mask, real = (torch.stack(t).to(self.device) for t in zip(*val_set[i : i + size], strict=True))
            output = self._forward(inputs)
            losses.append(loss_per_sample(output, target, mask, real).cpu())
            psnrs.append(psnr(output, target, mask, real).cpu())
            reals.append(real.cpu())
        self.model.train()
        loss, value, real = torch.cat(losses), torch.cat(psnrs), torch.cat(reals) > 0.5

        def mean(x: torch.Tensor):
            return float(x.mean()) if x.numel() else None

        return {"val_loss": mean(loss), "val_psnr_clean": mean(value[~real]), "val_psnr_real": mean(value[real])}
