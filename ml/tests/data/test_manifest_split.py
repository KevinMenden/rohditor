"""Licensing/privacy gate, scene-level splits, shards."""

import json

import numpy as np
import pytest

from rohditor_ml.data import split
from rohditor_ml.data.download import licenses
from rohditor_ml.data.manifest import ManifestError, validate_manifest
from rohditor_ml.data.shards import ShardWriter
from rohditor_ml.paths import ML_ROOT, PRIVATE_DIR, assert_not_private


def record(**changes):
    base = {"source": "pixls", "name": "a.dng", "license": licenses.CC0, "camera": "Cam", "scene": "s",
            "split": "train", "sha256": "ab" * 32}
    return {**base, **changes}


def test_manifest_accepts_allowed_licenses():
    validate_manifest({"files": [record(), record(license=licenses.CC_BY)]})


@pytest.mark.parametrize("bad", [
    record(license=licenses.CC_BY_SA),
    record(license="CC-BY-NC-SA-4.0"),
    record(license=licenses.RESEARCH_ONLY),
    record(license=None),
    record(sha256=""),
    record(name=str(PRIVATE_DIR / "x.dng")),
])
def test_manifest_rejects_bad_records(bad):
    with pytest.raises(ManifestError):
        validate_manifest({"files": [bad]})


def test_default_download_allow_list_equals_training_allow_list():
    assert licenses.DEFAULT_ALLOWED == licenses.TRAINING_ALLOWED


def test_built_manifest_if_present_is_clean_and_split_by_scene():
    path = ML_ROOT / "data" / "shards" / "manifest.json"
    if not path.exists():
        pytest.skip("no shards built")
    manifest = json.loads(path.read_text())
    validate_manifest(manifest)
    splits_of_scene: dict[str, set[str]] = {}
    for f in manifest["files"]:
        splits_of_scene.setdefault(f"{f['source']}:{f['scene']}", set()).add(f["split"])
    assert all(len(s) == 1 for s in splits_of_scene.values()), "a scene appears in more than one split"
    held_out = manifest["holdout_camera"]
    if held_out:
        assert {f["split"] for f in manifest["files"] if f["camera"] == held_out} == {"test_unseen_camera"}
        assert all(f["camera"] == held_out for f in manifest["files"] if f["split"] == "test_unseen_camera")


def test_private_corpus_is_refused():
    with pytest.raises(PermissionError):
        assert_not_private(PRIVATE_DIR / "some" / "file.cr2")
    assert_not_private(ML_ROOT / "data" / "raw" / "x.arw")


def test_split_is_stable_and_scene_based():
    a = split.assign_split("rawnind:scene1", "Cam", False, None, seed=0)
    assert a == split.assign_split("rawnind:scene1", "Cam", False, None, seed=0)
    counts = {s: 0 for s in split.SPLITS}
    for i in range(2000):
        counts[split.assign_split(f"x:{i}", "Cam", False, None, seed=0)] += 1
    assert counts["test_unseen_camera"] == 0
    assert 0.75 < counts["train"] / 2000 < 0.85 and counts["val"] > 100 and counts["test"] > 100


def test_reserved_scenes_and_held_out_camera_win_over_the_hash():
    assert split.assign_split("x:1", "Cam", True, None, 0) == "test"
    assert split.assign_split("x:1", "Held", True, "Held", 0) == "test_unseen_camera"
    assert split.assign_split("x:1", "Held", False, "Held", 0) == "test_unseen_camera"


def test_holdout_camera_choice():
    scenes = {"A": {"1", "2", "3"}, "B": {"1", "2"}, "C": {"1"}, "D": {"1", "2"}}
    assert split.choose_holdout_camera(scenes) == "B"  # fewest scenes with >= 2, ties alphabetical
    assert split.choose_holdout_camera({"C": {"1"}}) is None


def test_shard_roundtrip(tmp_path):
    writer = ShardWriter(tmp_path)
    for i in range(3):
        writer.add("train", "clean", mosaic=np.full((4, 4), i, np.float32), rgb=np.zeros((3, 4, 4)),
                   wb=np.array([2, 1, 1.5]), file_index=i, origin=(2 * i, 4))
    (entry,) = writer.close()
    assert entry["patches"] == 3 and entry["split"] == "train"
    with np.load(tmp_path / entry["path"]) as shard:
        assert shard["mosaic"].shape == (3, 4, 4) and shard["file_index"].tolist() == [0, 1, 2]
        assert shard["origin"].tolist() == [[0, 4], [2, 4], [4, 4]] and "mask" not in shard
