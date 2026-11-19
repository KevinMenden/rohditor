"""CPU smoke test: a tiny store, the real sample pipeline and a few training steps."""

import csv
import json

import numpy as np
import pytest

torch = pytest.importorskip("torch")

from rohditor_ml.data.bayer import mosaic_from_rgb  # noqa: E402
from rohditor_ml.train.dataset import SplitData, training_noise_doc  # noqa: E402
from rohditor_ml.train.trainer import Trainer  # noqa: E402

P = 64
NOISE = {"cameras": {"cam a": {"k_min": 1e-4, "k_max": 1e-3, "log_k_points": [-9.0, -7.0],
                               "log_sigma_points": [-8.0, -6.5]},
                     "held out": {"k_min": 1e-2, "k_max": 2e-2, "log_k_points": [-4.0, -3.0],
                                  "log_sigma_points": [-3.0, -2.0]}}}


def _store(root, split, n=6):
    rng = np.random.default_rng(len(split))
    files = [
        {"license": "CC-BY-4.0", "truth": "pixelshift", "noise_profile": "pooled"},
        {"license": "CC0-1.0", "truth": "bin2_edge_aware", "noise_profile": "cam a"},
        {"license": "CC-BY-SA-4.0", "truth": "bin2_edge_aware", "noise_profile": "cam a"},
        {"license": "CC-BY-SA-4.0", "truth": "edge_aware_demosaic", "noise_profile": "cam a"},
        {"license": "CC0-1.0", "truth": "edge_aware_demosaic", "noise_profile": "cam a"},
    ]
    root.mkdir(parents=True, exist_ok=True)
    (root / "manifest.json").write_text(json.dumps({"files": files, "holdout_camera": "held out"}))
    rgb = (rng.random((n, 3, P, P)) * 0.5).astype(np.float32)
    mosaic = np.stack([mosaic_from_rgb(x) for x in rgb])
    for kind, file_index in (("clean", [0, 1, 2, 0, 1, 2]), ("noisy", [3, 4, 3, 4, 3, 4])):
        folder = root / split / kind
        folder.mkdir(parents=True)
        np.save(folder / "mosaic.npy", mosaic)
        np.save(folder / "rgb.npy", rgb)
        np.save(folder / "wb.npy", np.ones((n, 3), np.float32))
        np.save(folder / "file_index.npy", np.array(file_index, np.int32))
        if kind == "noisy":
            np.save(folder / "mask.npy", np.ones((n, P, P), np.uint8))


@pytest.fixture
def store(tmp_path):
    root = tmp_path / "npy"
    for split in ("train", "val"):
        _store(root, split)
    (tmp_path / "noise.json").write_text(json.dumps(NOISE))
    return tmp_path


def test_license_filter_selects_the_variant(store):
    data = SplitData.load(store / "npy", "train", {"CC0-1.0", "CC-BY-4.0"})
    assert list(data.pixelshift) == [0, 3] and list(data.binned) == [1, 4]
    assert list(data.real_indices) == [1, 3, 5]
    full = SplitData.load(store / "npy", "train", {"CC0-1.0", "CC-BY-4.0", "CC-BY-SA-4.0"})
    assert len(full.binned) == 4 and len(full.real_indices) == 6


def test_holdout_camera_never_supplies_training_noise():
    assert set(training_noise_doc(NOISE, "held out")["cameras"]) == {"cam a"}


def test_samples_have_contract_shapes(store):
    data = SplitData.load(store / "npy", "train", {"CC0-1.0", "CC-BY-4.0"})
    rng = np.random.default_rng(0)
    clean = data.clean_sample(0, NOISE, 48, 0.25, rng)
    assert clean.input.shape == (4, 24, 24) and clean.target.shape == (3, 48, 48) and not clean.real
    real = data.real_sample(1, 48, rng)
    assert real.mask.shape == (1, 48, 48) and real.real


def test_a_few_training_steps_on_cpu(store, tmp_path):
    config = {
        "variant": "smoke-test", "licenses": ["CC0-1.0", "CC-BY-4.0"],
        "model": {"channels": [8, 16, 32, 64]},
        "data": {"npy_dir": str(store / "npy"), "noise_params": str(store / "noise.json"), "crop": 48,
                 "exposure_min": 0.25, "pixelshift_fraction": 0.5, "limit_patches": None, "workers": 0},
        "train": {"seed": 0, "batch_size": 2, "weight_decay": 1e-4, "bf16": True, "grad_clip": 1.0,
                  "log_every": 2, "val_every": 3, "checkpoint_every": 3, "val_clean": 2, "val_real": 2},
        "stages": [{"name": "synthetic", "steps": 3, "lr": 1e-3, "warmup": 1, "real_fraction": 0.0},
                   {"name": "real", "steps": 2, "lr": 1e-4, "warmup": 1, "real_fraction": 0.5}],
    }
    run_dir = tmp_path / "run"
    Trainer(config, run_dir, torch.device("cpu")).run()
    assert (run_dir / "model.pt").exists()
    assert (run_dir / "checkpoints" / "stage1-real.pt").exists()
    rows = list(csv.DictReader((run_dir / "log.csv").open()))
    assert rows and all(np.isfinite(float(r["loss"])) for r in rows if r["loss"])
    assert any(r["val_psnr_real"] for r in rows)
    from tensorboard.backend.event_processing.event_accumulator import EventAccumulator

    events = EventAccumulator(str(run_dir / "tb"), size_guidance={"images": 0}).Reload()
    assert {"train/loss", "val/psnr_clean", "val/psnr_real"} <= set(events.Tags()["scalars"])
    assert {"val_synthetic/0", "val_real/0"} <= set(events.Tags()["images"])
