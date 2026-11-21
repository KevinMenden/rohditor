"""Pinned upstream models for AI masks and their input/output contract.

Both models take an sRGB photo resized (bilinear, aspect ratio not kept) to a
fixed square, scaled to [0, 1] and normalised with the ImageNet mean and
standard deviation, as `[1, 3, S, S]` float32. Both return a probability map
in [0, 1] as `[1, 1, S, S]`, which the caller resizes back to the photo.
"""

from __future__ import annotations

from dataclasses import dataclass

IMAGENET_MEAN = (0.485, 0.456, 0.406)
IMAGENET_STD = (0.229, 0.224, 0.225)


@dataclass(frozen=True)
class HubFile:
    """A file in a Hugging Face repository, pinned to a commit and checksum."""

    repo: str
    revision: str
    filename: str
    sha256: str
    repo_type: str = "model"


@dataclass(frozen=True)
class MaskModel:
    id: str
    task: str
    size: int
    # Index of the network output that holds the mask.
    output_index: int
    # "logits" needs a sigmoid; "probability" is already in [0, 1].
    output_kind: str
    weights: HubFile
    # Model code executed by `trust_remote_code`, from the same pinned revision.
    code: tuple[HubFile, ...]
    license: dict
    training_data: list[str]
    # A CC0 photo (no people) for the sample input/output pair.
    sample_photo: HubFile


# CC0 phone photos (https://huggingface.co/datasets/Dima786/Photos_For_Ai).
_PHOTOS_REPO, _PHOTOS_REVISION = "Dima786/Photos_For_Ai", "d85cba43fa141d37a02e232bb475d0f72783bd7b"
CAT_PHOTO = HubFile(_PHOTOS_REPO, _PHOTOS_REVISION, "photos/photo_140@14-01-2025_22-12-04.jpg",
                    "85ff747ebcc3949a090c337b06507f36efbad7eafe7c6840d6aee1d876f002ae", "dataset")
HARBOUR_PHOTO = HubFile(_PHOTOS_REPO, _PHOTOS_REVISION, "photos/photo_1070@14-07-2025_21-18-37.jpg",
                        "09433ff3c4d8aac9abf2b15857483a1460affe3bfba6e0ea192999a1dff2bffd", "dataset")

# Salient subject: BiRefNet with a Swin-T backbone, exported here from PyTorch.
SUBJECT = MaskModel(
    id="birefnet-lite",
    task="subject",
    size=1024,
    output_index=0,
    output_kind="logits",
    weights=HubFile("ZhengPeng7/BiRefNet_lite", "aa62cd87eafb9cc43056d08ef3615a14628b831d", "model.safetensors",
                    "4417d89795250e698c3cb0ae8df15743810065f646f48a694fdfa7ca052d0815"),
    code=(
        HubFile("ZhengPeng7/BiRefNet_lite", "aa62cd87eafb9cc43056d08ef3615a14628b831d", "birefnet.py",
                "af8568b5be406bf4d2a68a7ed6d72e40f73b37a1fb6fc9ebd71b5b3cbcd069c9"),
        HubFile("ZhengPeng7/BiRefNet_lite", "aa62cd87eafb9cc43056d08ef3615a14628b831d", "BiRefNet_config.py",
                "e7b8c2a74f6cea6a59553d517f71d47f2c1d90e670a13416af17c25fe2f3dc52"),
    ),
    license={"code": "MIT", "weights": "MIT", "use": "local only: training data includes non-commercial sets"},
    training_data=["DIS5K (non-commercial research only)", "DUTS", "HRSOD", "UHRSD", "HRS10K",
                   "P3M-10k", "schirrmacher/humans (Apache-2.0, synthetic)"],
    sample_photo=CAT_PHOTO,
)

# Sky: a U-2-Net trained for sky segmentation, published only as ONNX (opset 11).
SKY = MaskModel(
    id="skyseg",
    task="sky",
    size=320,
    output_index=0,
    output_kind="probability",
    weights=HubFile("JianyuanWang/skyseg", "3ba8c6df1d9ba9ff26f637c7ba9568ac11a9aa7f", "skyseg.onnx",
                    "ab9c34c64c3d821220a2886a4a06da4642ffa14d5b30e8d5339056a089aa1d39"),
    code=(),
    license={"code": "MIT", "weights": "MIT (re-hosted from xiongzhu666/Sky-Segmentation-and-Post-processing)",
             "use": "local only: training data not documented"},
    training_data=["not documented"],
    sample_photo=HARBOUR_PHOTO,
)

MODELS = {m.id: m for m in (SUBJECT, SKY)}
