#!/usr/bin/env python3
"""Compare complete CPU and hardware-GPU RAW exports on identical inputs.

Each sample starts a fresh CLI process and includes RAW decoding, GPU device
creation, processing, readback, PNG encoding, and file commit. This measures
cold-process export, not resident editor slider-to-display latency.
"""

import argparse
import json
import platform
import re
import shutil
import statistics
import subprocess
import sys
import tempfile
import threading
import time
from datetime import datetime, timezone
from pathlib import Path


ROOT = Path(__file__).resolve().parent.parent
DURATION = re.compile(r"([0-9]+(?:\.[0-9]+)?)(ns|µs|us|ms|s)\b")
UNITS = {"ns": 0.000001, "µs": 0.001, "us": 0.001, "ms": 1.0, "s": 1000.0}


def duration_ms(value):
    match = DURATION.fullmatch(value.strip())
    if not match:
        raise ValueError(f"unrecognized Rust duration: {value!r}")
    return float(match[1]) * UNITS[match[2]]


def parse_timings(output, backend):
    if backend == "cpu":
        match = re.search(r"^CPU stages: (.*)$", output, re.MULTILINE)
        if not match:
            raise ValueError("CPU stage timings missing from CLI output")
        fields = {}
        for part in match[1].split(", "):
            item = re.fullmatch(r"(.+?) ([0-9]+(?:\.[0-9]+)?) ms", part)
            if not item:
                raise ValueError(f"unrecognized CPU stage: {part!r}")
            fields[item[1].replace(" ", "_")] = float(item[2])
        return fields

    match = re.search(r"Full-quality preparation: StageTimings \{([^}]*)\}", output)
    if not match:
        raise ValueError("GPU stage timings missing from CLI output; rebuild the CLI")
    fields = {
        name: duration_ms(value)
        for name, value in re.findall(r"(\w+): ([0-9.]+(?:ns|µs|us|ms|s))", match[1])
    }
    for label, key in [
        ("RAW decode", "decode"),
        ("GPU upload", "upload"),
        ("color and readback", "color_and_readback"),
        ("encoding", "encode_commit"),
    ]:
        item = re.search(rf"{re.escape(label)}: ([0-9.]+(?:ns|µs|us|ms|s))", output)
        if not item:
            raise ValueError(f"GPU {label} timing missing from CLI output")
        fields[key] = duration_ms(item[1])
    count = re.search(r"read back (\d+) bytes in (\d+) submissions", output)
    if count:
        fields["submissions"] = int(count[2])
        fields["readback_bytes"] = int(count[1])
    capture = re.search(r"GPU capture: (\d+) tiles of (\d+) pixels, halo (\d+), compute/wait ([^,]+), upload ([^,]+), readback ([^\n]+)", output)
    if capture:
        fields["capture_tiles"] = int(capture[1])
        fields["capture_tile_edge"] = int(capture[2])
        fields["capture_halo"] = int(capture[3])
        fields["capture_compute_wait_ms"] = duration_ms(capture[4])
        fields["capture_upload_ms"] = duration_ms(capture[5])
        fields["capture_readback_ms"] = duration_ms(capture[6])
    return fields


def amd_vram_paths():
    paths = []
    for card in Path("/sys/class/drm").glob("card[0-9]*"):
        vendor = card / "device/vendor"
        usage = card / "device/mem_info_vram_used"
        try:
            if vendor.read_text().strip() == "0x1002" and usage.is_file():
                paths.append(usage)
        except OSError:
            pass
    return paths


def cpu_model():
    try:
        for line in Path("/proc/cpuinfo").read_text().splitlines():
            if line.startswith("model name"):
                return line.partition(":")[2].strip()
    except OSError:
        pass
    return platform.processor()


def vram_used(paths):
    try:
        return sum(int(path.read_text().strip()) for path in paths) if paths else None
    except OSError:
        return None


def run_sample(binary, raw, algorithm, capture, backend, lens_profile, output_dir, vram_paths, keep):
    with tempfile.TemporaryDirectory(prefix="sample-", dir=output_dir) as temporary:
        destination = Path(temporary) / "export.png"
        time_file = Path(temporary) / "time.txt"
        command = [
            str(binary), "develop", str(raw), str(destination),
            "--processor", backend, "--demosaic", algorithm,
            "--highlight-reconstruction", "clip", "--metadata", "none",
            "--png-bit-depth", "8", "--lens-profile", lens_profile,
        ]
        if capture:
            command.append("--capture-sharpening")
        measured = ["/usr/bin/time", "-f", "%M", "-o", str(time_file), *command]
        before_vram = vram_used(vram_paths)
        peak_vram = before_vram
        stop_sampling = threading.Event()

        def sample_vram():
            nonlocal peak_vram
            while not stop_sampling.wait(0.05):
                used = vram_used(vram_paths)
                if used is not None:
                    peak_vram = max(peak_vram or used, used)

        started = time.perf_counter()
        process = subprocess.Popen(measured, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
        sampler = threading.Thread(target=sample_vram, daemon=True)
        sampler.start()
        try:
            stdout, stderr = process.communicate()
        except KeyboardInterrupt:
            process.terminate()
            process.wait()
            raise
        finally:
            wall_ms = (time.perf_counter() - started) * 1000
            stop_sampling.set()
            sampler.join()
        used = vram_used(vram_paths)
        if used is not None:
            peak_vram = max(peak_vram or used, used)
        result = {
            "backend": backend,
            "wall_ms": round(wall_ms, 2),
            "peak_rss_kib": int(time_file.read_text().strip()),
            "vram_baseline_bytes": before_vram,
            "vram_peak_bytes": peak_vram,
            "vram_peak_delta_bytes": None if before_vram is None or peak_vram is None else peak_vram - before_vram,
            "returncode": process.returncode,
        }
        if process.returncode:
            result["error"] = stderr.strip() or stdout.strip()
            return result
        if not destination.is_file():
            result["error"] = "CLI succeeded without creating an export"
            return result
        result["output_bytes"] = destination.stat().st_size
        result["stages_ms"] = parse_timings(stdout, backend)
        if backend == "gpu":
            adapter = re.search(r"^Color processor: GPU, (.+)$", stdout, re.MULTILINE)
            sensor = re.search(r"^Sensor processor: (.+)$", stdout, re.MULTILINE)
            if not adapter or not sensor or sensor[1] != "GPU":
                raise ValueError("GPU run did not use GPU sensor and color processing")
            result["adapter"] = adapter[1]
        if keep:
            saved = output_dir / f"{raw.stem}-{algorithm}-capture-{int(capture)}-{backend}.png"
            shutil.copy2(destination, saved)
            result["image"] = str(saved)
        return result


def median(values):
    return round(statistics.median(values), 2) if values else None


def summarize(samples):
    summary = {}
    for backend in ("cpu", "gpu"):
        rows = [item for item in samples if item["backend"] == backend and "error" not in item]
        if not rows:
            continue
        stage_names = set().union(*(row["stages_ms"] for row in rows))
        summary[backend] = {
            "samples": len(rows),
            "wall_median_ms": median([row["wall_ms"] for row in rows]),
            "wall_max_ms": max(row["wall_ms"] for row in rows),
            "rss_peak_mib": round(max(row["peak_rss_kib"] for row in rows) / 1024, 1),
            "vram_peak_delta_mib": round(max(row["vram_peak_delta_bytes"] for row in rows if row["vram_peak_delta_bytes"] is not None) / 1048576, 1)
            if any(row["vram_peak_delta_bytes"] is not None for row in rows) else None,
            "stage_median_ms": {
                name: median([row["stages_ms"][name] for row in rows if name in row["stages_ms"]])
                for name in sorted(stage_names)
            },
        }
    if "cpu" in summary and "gpu" in summary:
        summary["cpu_over_gpu_wall_ratio"] = round(
            summary["cpu"]["wall_median_ms"] / summary["gpu"]["wall_median_ms"], 2
        )
    return summary


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--raw", action="append", type=Path, required=True, help="RAW file; repeat for multiple scenes")
    parser.add_argument("--algorithms", default="mhc,rcd", help="comma-separated: bilinear,mhc,rcd")
    parser.add_argument("--capture", choices=("both", "off", "on"), default="both")
    parser.add_argument("--lens-profile", default="off", help="CLI Lensfun selection, e.g. auto")
    parser.add_argument("--repeats", type=int, default=3)
    parser.add_argument("--warmups", type=int, default=2)
    parser.add_argument("--expect-adapter", default="RX 9070 XT")
    parser.add_argument("--binary", type=Path, help="existing release CLI; otherwise build it")
    parser.add_argument("--output-dir", type=Path, help="new directory for report and optional images")
    parser.add_argument("--keep-images", action="store_true", help="save one CPU/GPU PNG pair per case")
    args = parser.parse_args()
    algorithms = [name.strip() for name in args.algorithms.split(",")]
    if not algorithms or any(name not in {"bilinear", "mhc", "rcd"} for name in algorithms):
        parser.error("--algorithms must contain bilinear, mhc, or rcd")
    if args.repeats < 1 or args.warmups < 0:
        parser.error("--repeats must be positive and --warmups nonnegative")
    raws = [path.resolve() for path in args.raw]
    if any(not path.is_file() for path in raws):
        parser.error("every --raw path must exist")
    if not Path("/dev/dri").exists():
        parser.error("/dev/dri is unavailable; run on the host with the RX 9070 XT")
    if not Path("/usr/bin/time").is_file():
        parser.error("GNU /usr/bin/time is required for peak process RSS")
    binary = args.binary.resolve() if args.binary else ROOT / "target/release/rohditor-cli"
    if not args.binary:
        subprocess.run(["cargo", "build", "--release", "--locked", "-p", "rohditor-cli"], cwd=ROOT, check=True)
    if not binary.is_file():
        parser.error(f"CLI binary missing: {binary}")
    output_dir = args.output_dir.resolve() if args.output_dir else Path(tempfile.mkdtemp(prefix="rohditor-bench-"))
    if args.output_dir:
        output_dir.mkdir(parents=True, exist_ok=False)
    vram_paths = amd_vram_paths()
    report = {
        "created_utc": datetime.now(timezone.utc).isoformat(),
        "host": platform.uname()._asdict(),
        "cpu_model": cpu_model(),
        "git_commit": subprocess.run(["git", "rev-parse", "HEAD"], cwd=ROOT, text=True, capture_output=True, check=True).stdout.strip(),
        "git_dirty": bool(subprocess.run(["git", "status", "--porcelain"], cwd=ROOT, text=True, capture_output=True, check=True).stdout),
        "vram_counters": [str(path) for path in vram_paths],
        "method": "fresh CLI process per trial; includes decode, device creation, processing, readback, PNG encoding and commit; sampled sysfs VRAM is system-wide",
        "arguments": {"raw": [str(path) for path in raws], "algorithms": algorithms, "capture": args.capture, "lens_profile": args.lens_profile, "repeats": args.repeats, "warmups": args.warmups},
        "cases": [],
    }
    captures = [False, True] if args.capture == "both" else [args.capture == "on"]
    adapter_checked = False
    try:
        for raw in raws:
            for algorithm in algorithms:
                for capture in captures:
                    label = f"{raw.name} {algorithm} capture={'on' if capture else 'off'}"
                    print(label, flush=True)
                    case = {"raw": str(raw), "algorithm": algorithm, "capture": capture, "samples": [], "failures": []}
                    report["cases"].append(case)
                    failed = set()
                    for index in range(args.warmups + args.repeats):
                        for backend in (("gpu", "cpu") if index % 2 == 0 else ("cpu", "gpu")):
                            if backend in failed:
                                continue
                            measured = index >= args.warmups
                            print(f"  {backend} {'sample' if measured else 'warmup'} {index + 1}", flush=True)
                            result = run_sample(binary, raw, algorithm, capture, backend, args.lens_profile, output_dir, vram_paths, args.keep_images and measured and index == args.warmups)
                            if backend == "gpu" and "error" not in result:
                                if args.expect_adapter.casefold() not in result["adapter"].casefold():
                                    raise RuntimeError(f"GPU adapter is {result['adapter']!r}, expected {args.expect_adapter!r}")
                                adapter_checked = True
                                report["adapter"] = result["adapter"]
                            if "error" in result:
                                failed.add(backend)
                                case["failures"].append(result)
                                print(f"    FAILED: {result['error'][-400:]}", flush=True)
                            elif measured:
                                case["samples"].append(result)
                                print(f"    {result['wall_ms']:.0f} ms", flush=True)
                    case["summary"] = summarize(case["samples"])
                    if capture and args.capture == "both":
                        previous = report["cases"][-2]
                        case["capture_incremental_wall_ms"] = {
                            backend: round(case["summary"][backend]["wall_median_ms"] - previous["summary"][backend]["wall_median_ms"], 2)
                            for backend in ("cpu", "gpu")
                            if backend in case["summary"] and backend in previous["summary"]
                        }
                    if "cpu_over_gpu_wall_ratio" in case["summary"]:
                        ratio = case["summary"]["cpu_over_gpu_wall_ratio"]
                        print(f"  median CPU/GPU wall ratio: {ratio:.2f}x", flush=True)
                    for backend in ("cpu", "gpu"):
                        stages = case["summary"].get(backend, {}).get("stage_median_ms", {})
                        if stages:
                            print(f"  {backend} median stages: demosaic={stages.get('demosaic', 0):.1f} ms, capture={stages.get('capture_sharpening', 0):.1f} ms", flush=True)
                    if "capture_incremental_wall_ms" in case:
                        print(f"  capture incremental wall time: {case['capture_incremental_wall_ms']}", flush=True)
    finally:
        destination = output_dir / "results.json"
        destination.write_text(json.dumps(report, indent=2) + "\n")
        print(f"Results: {destination}", flush=True)
    if not adapter_checked:
        raise RuntimeError("No hardware GPU sample completed; inspect results.json for failures")


if __name__ == "__main__":
    try:
        main()
    except (OSError, ValueError, RuntimeError, subprocess.CalledProcessError) as error:
        sys.exit(f"benchmark failed: {error}")
