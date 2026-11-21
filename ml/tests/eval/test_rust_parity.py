"""Parity between Rohditor's Rust neural demosaic (#118) and this package.

Opt-in: needs the release CLI (`cargo build --release -p rohditor-cli`), public
raws in `data/raw/pixls`, and for the output check torch plus the `shippable`
run with its export. Each test skips when its inputs are missing.
"""

from __future__ import annotations

import json
import subprocess
from pathlib import Path

import numpy as np
import pytest

from rohditor_ml.paths import ML_ROOT

CLI = ML_ROOT.parent / "target" / "release" / "rohditor-cli"
SHIPPABLE = ML_ROOT / "runs" / "shippable"
# One CC0 file per maker, so different CFA phases, crops and level layouts are
# covered, with how far rawler and libraw may disagree on its samples: lossless
# formats decode identically, lossy ones within the 8 DN `verify-libraw` allows,
# and a fraction of samples may differ more (the RW2 decoders disagree on a
# regular grid of G and B samples, about 0.05% of them).
RAW_PATTERNS = {
    "*Sony_-_ILCE-7M3_-_14bit_14bit_compressed*.ARW": (8, 0.0),
    "*Canon_-_EOS_5D_Mark_IV_-_RAW*.CR2": (0, 0.0),
    "*Nikon_-_D7500*.NEF": (8, 0.0),
    "*Panasonic_-_DMC-G6*.RW2": (0, 1e-3),
}
PACKED_OFFSETS = ((0, 0), (1, 0), (0, 1), (1, 1))  # (dx, dy) of R, G1, G2, B


def raw_files() -> list[Path]:
    return [f for p in RAW_PATTERNS for f in sorted((ML_ROOT / "data" / "raw" / "pixls").glob(p))[:1]]


def decoder_tolerance(raw: Path) -> tuple[int, float]:
    """Sample difference in DN, and the fraction of samples allowed to exceed it."""
    return next(tolerance for pattern, tolerance in RAW_PATTERNS.items() if raw.match(pattern))


def rust_input(raw: Path, tmp_path: Path) -> tuple[np.ndarray, dict]:
    if not CLI.exists():
        pytest.skip("build the release CLI first")
    out = tmp_path / "input.npy"
    result = subprocess.run([str(CLI), "neural-input-npy", str(raw), "-o", str(out), "--force"],
                            capture_output=True, text=True, check=True)
    return np.load(out)[0], json.loads(result.stdout)


@pytest.mark.parametrize("raw", raw_files(), ids=lambda p: p.name[:40])
def test_packed_input_matches_the_sensor_samples(raw: Path, tmp_path: Path):
    """Crop, CFA phase, packing order and white balance agree within 1e-6, up to the
    decoders' known sample differences.

    The reference is built here from libraw's raw samples with Rohditor's levels; the
    levels themselves are compared in `test_levels_match_libraw`.
    """
    rawpy = pytest.importorskip("rawpy")
    packed, meta = rust_input(raw, tmp_path)
    ox, oy = meta["sensor_origin"]
    cx, cy = meta["crop_origin"]
    width, height = meta["image_size"]
    with rawpy.imread(str(raw)) as decoded:
        sensor = decoded.raw_image.astype(np.float32)
    # Packed blocks lying wholly inside the image (the rest is mirror padding).
    px0, py0 = (cx - ox + 1) // 2, (cy - oy + 1) // 2
    px1, py1 = (cx + width - ox) // 2, (cy + height - oy) // 2
    gains = np.array(meta["gains"], np.float32)
    dn, outliers = decoder_tolerance(raw)
    for channel, (dx, dy) in enumerate(PACKED_OFFSETS):
        rows = slice(oy + 2 * py0 + dy, oy + 2 * py1 + dy, 2)
        cols = slice(ox + 2 * px0 + dx, ox + 2 * px1 + dx, 2)
        black = np.float32(meta["black"][channel])
        white = np.float32(meta["white"][channel])
        gain = gains[(0, 1, 1, 2)[channel]]
        expected = (sensor[rows, cols] - black) / (white - black) * gain
        actual = packed[channel, py0:py1, px0:px1]
        assert actual.shape == expected.shape
        outside = np.abs(actual - expected) > 1e-6 + dn / (white - black) * gain
        assert outside.mean() <= outliers, f"channel {channel}: {outside.sum()} samples differ"


@pytest.mark.xfail(strict=True, reason=(
    "Rohditor (rawler) divides by the camera's saturation level, the training decode by "
    "libraw's `white_level` (often the bit-depth maximum), so its inputs are 5-18% darker "
    "and clipped photosites sit below 1.0. Align `data/decode.py` before the next training."))
@pytest.mark.parametrize("raw", raw_files(), ids=lambda p: p.name[:40])
def test_levels_match_libraw(raw: Path, tmp_path: Path):
    rawpy = pytest.importorskip("rawpy")
    _, meta = rust_input(raw, tmp_path)
    with rawpy.imread(str(raw)) as decoded:
        colors = decoded.raw_colors
        black = np.asarray(decoded.black_level_per_channel, np.float32)
        white = float(decoded.white_level)
    ox, oy = meta["sensor_origin"]
    cx, cy = meta["crop_origin"]
    bx, by = cx + (cx - ox) % 2, cy + (cy - oy) % 2
    expected_black = [black[colors[by + dy, bx + dx]] for dx, dy in PACKED_OFFSETS]
    np.testing.assert_allclose(meta["black"], expected_black, atol=0.5)
    np.testing.assert_allclose(meta["white"], [white] * 4, atol=0.5)


def test_rust_output_matches_pytorch(tmp_path: Path):
    """The same contract-scaled mosaic through Rust (ONNX Runtime, contract tiling) and
    PyTorch agrees within 1e-3 away from the image border, where the two pad differently."""
    torch = pytest.importorskip("torch")
    from rohditor_ml.data.bayer import pack
    from rohditor_ml.data.decode import decode_raw
    from rohditor_ml.train.augment import packed_gains
    from rohditor_ml.train.export import load_trained

    onnx = SHIPPABLE / "export" / "model.onnx"
    files = raw_files()
    if not (CLI.exists() and onnx.exists() and files):
        pytest.skip("needs the release CLI, the shippable export and a public raw")
    mosaic = decode_raw(files[0])
    # Two by two contract tiles, compared beyond the receptive field of the border.
    size, margin = 1536, 128
    h0, w0 = (mosaic.data.shape[0] // 2 - size // 2) // 2 * 2, (mosaic.data.shape[1] // 2 - size // 2) // 2 * 2
    crop = np.ascontiguousarray(mosaic.data[h0:h0 + size, w0:w0 + size])
    np.save(tmp_path / "mosaic.npy", crop)
    np.save(tmp_path / "wb.npy", mosaic.wb.astype(np.float32))
    subprocess.run([str(CLI), "demosaic-npy", str(tmp_path / "mosaic.npy"), "--wb", str(tmp_path / "wb.npy"),
                    "--output", str(tmp_path / "rust.npy"), "--demosaic", "neural", "--neural-model", str(onnx)],
                   check=True)
    rust = np.load(tmp_path / "rust.npy")

    model, _, _ = load_trained(SHIPPABLE)
    packed = (pack(crop) * packed_gains(mosaic.wb))[None].astype(np.float32)
    with torch.no_grad():
        reference = model(torch.from_numpy(packed))[0].numpy()
    inner = (slice(None), slice(margin, -margin), slice(margin, -margin))
    worst = float(np.abs(rust[inner] - reference[inner]).max())
    assert worst < 1e-3, f"Rust differs from PyTorch by {worst:.2e}"
