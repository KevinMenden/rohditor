import pytest

torch = pytest.importorskip("torch")
pytest.importorskip("onnxruntime")

from rohditor_ml.train.export import export_onnx, verify_onnx  # noqa: E402
from rohditor_ml.train.model import UNet  # noqa: E402


def test_dynamic_onnx_export_matches_pytorch(tmp_path):
    torch.manual_seed(0)
    model = UNet((8, 16, 32, 64)).eval()
    # Non-zero head, so the check covers the learned branch and not only the bilinear one.
    torch.nn.init.normal_(model.head.weight, std=0.05)
    path = tmp_path / "model.onnx"
    export_onnx(model, path)
    assert verify_onnx(model, path) < 1e-4
