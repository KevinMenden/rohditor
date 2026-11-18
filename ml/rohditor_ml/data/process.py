"""Turning items into shard patches (and noise-calibration measurements).

Three kinds of samples:

* clean samples (`kind="clean"`): a clean mosaic and its ground truth, from
  PixelShift200 (real full-sampled truth), or from clean raws via demosaic + 2x2
  binning. Noise is added later, at training time, with `noise.add_noise`.
* real pairs (`kind="noisy"`): a real RawNIND noisy mosaic, with the aligned
  clean image (demosaiced, gain-matched) as target plus a validity mask.
* noise measurements: every usable noisy/clean pair also yields a
  (K, sigma^2) estimate for `noise.fit`, stored in its manifest record.
"""

from __future__ import annotations

import hashlib
import multiprocessing
from collections import defaultdict
from dataclasses import asdict, dataclass
from collections.abc import Callable

import cv2
import numpy as np

from . import bayer, truth
from .align import find_alignment, loss_mask
from ..contract import PATCH_SIZE
from .decode import Mosaic, UnsupportedRaw, decode_pixelshift, decode_raw
from .noise.fit import estimate_pair
from .patches import cut, sample_origins
from .prepare import Item
from .shards import ShardWriter

# Patches taken per source file.
PATCHES_PER_FILE = {"pixelshift200": 64, "pixls": 16, "rawnind-clean": 32, "rawnind-noisy": 8}
MIN_ISO_RATIO = 2  # use a noisy shot for noise fitting only if its ISO is well above the clean one's


def file_rng(seed: int, item: Item) -> np.random.Generator:
    digest = hashlib.sha256(f"{seed}:{item.name}".encode()).digest()
    return np.random.default_rng(int.from_bytes(digest[:8], "big"))


class Processor:
    """Turns the items of one scene into patches; `writer` is anything with `ShardWriter.add`."""

    def __init__(self, writer, seed: int, log: Callable[[str], None] = print):
        self.writer, self.seed, self.log = writer, seed, log
        self.records: list[dict] = []  # manifest file records, in file_index order
        self.skipped: list[dict] = []

    # -- bookkeeping ---------------------------------------------------------

    def _record(self, item: Item, role: str, truth_kind: str, wb: np.ndarray, mosaic_shape, **extra) -> int:
        self.records.append(
            {
                "source": item.source, "name": item.name, "license": item.license, "camera": item.camera,
                "scene": item.scene, "iso": item.iso, "split": item.split, "sha256": item.sha256,
                "role": role, "truth": truth_kind, "wb": [float(v) for v in wb],
                "mosaic_shape": list(mosaic_shape), "patches": 0, **extra,
            }
        )
        return len(self.records) - 1

    def _skip(self, item: Item, reason: str) -> None:
        self.log(f"  skipped {item.name}: {reason}")
        self.skipped.append({"source": item.source, "name": item.name, "reason": reason})

    # -- clean samples -------------------------------------------------------

    def _write_clean(self, item: Item, mosaic: np.ndarray, rgb: np.ndarray, wb: np.ndarray, truth_kind: str, count: int):
        origins = sample_origins(mosaic, count, file_rng(self.seed, item))
        if not origins:
            return self._skip(item, "too small for any patch")
        index = self._record(item, "clean", truth_kind, wb, mosaic.shape)
        for origin in origins:
            self.writer.add(item.split, "clean", mosaic=cut(mosaic, origin), rgb=cut(rgb, origin), wb=wb,
                            file_index=index, origin=origin)
        self.records[index]["patches"] = len(origins)

    def pixelshift(self, item: Item) -> None:
        scene = decode_pixelshift(item.path, item.camera)
        mosaic, rgb = truth.from_pixelshift(scene)
        self._write_clean(item, mosaic, rgb, scene.wb, "pixelshift", PATCHES_PER_FILE["pixelshift200"])

    def clean_raw(self, item: Item, count: int) -> Mosaic | None:
        """Decode a clean raw and write its binned-truth samples; returns the decoded mosaic."""
        try:
            decoded = decode_raw(item.path)
        except UnsupportedRaw as error:
            self._skip(item, str(error))
            return None
        mosaic, rgb = truth.from_clean_mosaic(decoded.data)
        self._write_clean(item, mosaic, rgb, decoded.wb, "bin2_edge_aware", count)
        return decoded

    # -- RawNIND scenes: clean references plus their real noisy shots ----------

    def rawnind_scene(self, clean_items: list[Item], noisy_items: list[Item]) -> None:
        references: list[tuple[Item, Mosaic]] = []
        for item in clean_items:
            decoded = self.clean_raw(item, PATCHES_PER_FILE["rawnind-clean"])
            if decoded is not None:
                references.append((item, decoded))
        for item in noisy_items:
            if references:
                self._noisy_pair(item, references)
            else:
                self._skip(item, "scene has no usable clean reference")

    def _noisy_pair(self, item: Item, references: list[tuple[Item, Mosaic]]) -> None:
        try:
            noisy = decode_raw(item.path)
        except UnsupportedRaw as error:
            return self._skip(item, str(error))
        # Several clean shots of a scene may differ in framing: take the best match.
        scored = [(a, ref, item_ref) for item_ref, ref in references if (a := find_alignment(noisy.data, ref.data))]
        if not scored:
            return self._skip(item, "no clean reference with matching geometry")
        alignment, reference, ref_item = min(scored, key=lambda s: s[0].score)

        estimate = None
        if item.iso and ref_item.iso and item.iso >= MIN_ISO_RATIO * ref_item.iso:
            estimate = estimate_pair(noisy.data, reference.data, item.iso, file_rng(self.seed, item))

        origins = sample_origins(noisy.data, PATCHES_PER_FILE["rawnind-noisy"], file_rng(self.seed + 1, item))
        index = self._record(item, "noisy", "edge_aware_demosaic", noisy.wb, noisy.data.shape,
                             paired_with=ref_item.name,
                             alignment={"dy": alignment.dy, "dx": alignment.dx, "gain": alignment.gain},
                             noise_estimate=asdict(estimate) if estimate else None)
        kept = 0
        for y, x in origins:
            clean_rgb = truth.demosaic_window(reference.data, y + alignment.dy, x + alignment.dx, PATCH_SIZE)
            noisy_patch = cut(noisy.data, (y, x))
            mask = loss_mask(noisy_patch, clean_rgb, alignment.gain)
            if mask.mean() < 0.5:  # mostly misaligned or overexposed: not worth training on
                continue
            self.writer.add(item.split, "noisy", mosaic=noisy_patch, rgb=alignment.gain * clean_rgb, wb=noisy.wb,
                            file_index=index, origin=(y, x), mask=mask)
            kept += 1
        self.records[index]["patches"] = kept

    def scene(self, scene_items: list[Item]) -> None:
        source = scene_items[0].source
        if source == "pixelshift200":
            for item in scene_items:
                self.pixelshift(item)
        elif source == "pixls":
            for item in scene_items:
                self.clean_raw(item, PATCHES_PER_FILE["pixls"])
        else:
            self.rawnind_scene([i for i in scene_items if i.is_clean], [i for i in scene_items if not i.is_clean])


# -- driver: scenes in parallel, written in a fixed order ------------------------


class _PatchBuffer:
    """Collects one scene's `ShardWriter.add` calls so a worker process can return them."""

    def __init__(self):
        self.patches: list[tuple[str, str, dict]] = []

    def add(self, split: str, kind: str, **arrays) -> None:
        self.patches.append((split, kind, arrays))


@dataclass
class SceneResult:
    records: list[dict]
    skipped: list[dict]
    patches: list[tuple[str, str, dict]]  # file_index refers to `records`
    log: list[str]


def _init_worker() -> None:
    cv2.setNumThreads(1)  # the parallelism is across scenes; don't oversubscribe the cores


def process_scene(job: tuple[list[Item], int]) -> SceneResult:
    scene_items, seed = job
    buffer, lines = _PatchBuffer(), []
    processor = Processor(buffer, seed, log=lines.append)
    processor.scene(scene_items)
    return SceneResult(processor.records, processor.skipped, buffer.patches, lines)


def run(items: list[Item], writer: ShardWriter, seed: int, workers: int,
        log: Callable[[str], None] = print) -> tuple[list[dict], list[dict]]:
    """Process every scene; returns the manifest's file records and skipped files.

    Scenes run in `workers` processes, but their results are written in sorted
    scene order, so shards and manifest are identical for any worker count.
    """
    by_scene: dict[str, list[Item]] = defaultdict(list)
    for item in items:
        by_scene[item.scene_key].append(item)
    scenes = sorted(by_scene.items())
    records: list[dict] = []
    skipped: list[dict] = []
    with multiprocessing.get_context("spawn").Pool(workers, initializer=_init_worker) as pool:
        results = pool.imap(process_scene, [(scene_items, seed) for _, scene_items in scenes])
        for n, ((scene_key, scene_items), result) in enumerate(zip(scenes, results, strict=True), 1):
            log(f"[{n}/{len(scenes)}] {scene_key} ({len(scene_items)} files)")
            for line in result.log:
                log(line)
            offset = len(records)
            records.extend(result.records)
            skipped.extend(result.skipped)
            for split, kind, arrays in result.patches:
                writer.add(split, kind, **{**arrays, "file_index": arrays["file_index"] + offset})
    return records, skipped
