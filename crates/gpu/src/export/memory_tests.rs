//! What exports cost in GPU memory, measured while they run, at 24 and 48 MP.
//!
//! The reservation counter is a logical budget; the kernel's pool counters are what the card
//! really holds. Both are sampled so a run can be judged against the 768 MiB budget (#30).
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, Instant};

use super::super::tests::{gpu_test_guard, neutral_recipe};
use super::tests::processor;
use super::*;
use crate::masks::test_recipes::colored_frame;
use rohditor_core::{OutputMedium, OutputSharpeningStrength};

/// Used bytes of a memory pool as the kernel reports it, on the amdgpu driver.
fn amdgpu_used(pool: &str) -> u64 {
    std::fs::read_dir("/sys/class/drm")
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|entry| {
            std::fs::read_to_string(entry.path().join(format!("device/mem_info_{pool}_used"))).ok()
        })
        .filter_map(|used| used.trim().parse::<u64>().ok())
        .max()
        .unwrap_or(0)
}

/// Highest values seen while a closure runs, sampled every few milliseconds. The process-wide
/// reservation peak never falls, so it cannot tell one run from the one before it.
#[derive(Default)]
struct Peaks {
    reservation: AtomicU64,
    vram: AtomicU64,
    gtt: AtomicU64,
    done: AtomicBool,
}

/// Memory in use just before a measured run, to report growth over.
struct Baseline {
    vram: u64,
    gtt: u64,
}

pub(super) struct Measured {
    peaks: Arc<Peaks>,
    baseline: Baseline,
}

impl Measured {
    /// One line: reservation peak, then VRAM and GTT growth over the level before the run.
    pub(super) fn describe(&self) -> String {
        let growth = |peak: &AtomicU64, before: u64| {
            peak.load(Ordering::Acquire).saturating_sub(before) >> 20
        };
        format!(
            "reservation peak {} MiB, VRAM +{} MiB (of {} MiB in use), GTT +{} MiB",
            self.peaks.reservation.load(Ordering::Acquire) >> 20,
            growth(&self.peaks.vram, self.baseline.vram),
            self.baseline.vram >> 20,
            growth(&self.peaks.gtt, self.baseline.gtt),
        )
    }
}

/// Run `work` while sampling memory. Pauses first so the previous run's resources are gone and
/// the baseline is the level just before this one.
pub(super) fn measure<T>(work: impl FnOnce() -> T) -> (T, Duration, Measured) {
    std::thread::sleep(Duration::from_millis(500));
    let baseline = Baseline {
        vram: amdgpu_used("vram"),
        gtt: amdgpu_used("gtt"),
    };
    let peaks = Arc::new(Peaks::default());
    let sampler = {
        let peaks = Arc::clone(&peaks);
        std::thread::spawn(move || {
            while !peaks.done.load(Ordering::Acquire) {
                peaks.reservation.fetch_max(
                    crate::gpu_memory_reservations().current_bytes,
                    Ordering::AcqRel,
                );
                peaks.vram.fetch_max(amdgpu_used("vram"), Ordering::AcqRel);
                peaks.gtt.fetch_max(amdgpu_used("gtt"), Ordering::AcqRel);
                std::thread::sleep(Duration::from_millis(2));
            }
        })
    };
    let started = Instant::now();
    let result = work();
    let elapsed = started.elapsed();
    peaks.done.store(true, Ordering::Release);
    sampler.join().expect("sampler");
    (result, elapsed, Measured { peaks, baseline })
}

/// Every source-side detail stage on, without and with defringe (which needs a second resident
/// plane set).
fn detail_recipes() -> [(&'static str, EditRecipe); 2] {
    let mut detail = neutral_recipe();
    detail.false_color.steps = 1;
    detail.noise_reduction.chroma = 0.5;
    detail.noise_reduction.luminance = 0.4;
    detail.capture_sharpening.enabled = true;
    let mut defringe = detail.clone();
    defringe.defringe.purple_amount = 0.5;
    defringe.defringe.green_amount = 0.5;
    [("detail stage", detail), ("detail + defringe", defringe)]
}

/// Every stage on at 24 and 48 MP. A recipe that does not fit the budget has to say so with a
/// routing answer (the caller runs the CPU path and keeps the device), and the next export on
/// the same device must still work.
#[test]
#[ignore = "requires a hardware Vulkan GPU; prints reservation peak and VRAM for detail exports at 24 and 48 MP"]
fn detail_export_memory_at_24_and_48_mp() {
    let _guard = gpu_test_guard();
    let Some(mut gpu) = processor(true) else {
        return;
    };
    let cpu = CpuPipeline::default();
    let token = CancellationToken::new();
    let sharpening = Some(OutputSharpening {
        medium: OutputMedium::Screen,
        strength: OutputSharpeningStrength::Standard,
    });
    for (name, width, height) in [("24 MP", 6000, 4000), ("48 MP", 8000, 6000)] {
        let frame = colored_frame(width, height, Orientation::Normal);
        for (recipe_name, recipe) in detail_recipes() {
            for (output, size, sharpening) in [
                ("original", OutputSize::Original, None),
                (
                    "2048 px sharpened",
                    OutputSize::LongEdge { pixels: 2048 },
                    sharpening,
                ),
            ] {
                let (result, elapsed, memory) = measure(|| {
                    gpu.render(
                        &cpu,
                        &frame,
                        &recipe,
                        RenderOptions::default(),
                        OutputBitDepth::Eight,
                        DitherMode::None,
                        size,
                        sharpening,
                        &token,
                    )
                });
                let label = format!("{name} {recipe_name}, {output}");
                match result {
                    Ok(result) => eprintln!(
                        "{label}: GPU {elapsed:?}, {}, estimate {} MiB",
                        memory.describe(),
                        result.estimated_gpu_bytes >> 20,
                    ),
                    Err(error) => {
                        eprintln!("{label}: not on the GPU after {elapsed:?}: {error}");
                        assert!(error.is_routing(), "{label}: a device failure: {error}");
                        // The device must still work for the next export.
                        gpu.render(
                            &cpu,
                            &colored_frame(64, 48, Orientation::Normal),
                            &neutral_recipe(),
                            RenderOptions::default(),
                            OutputBitDepth::Eight,
                            DitherMode::None,
                            OutputSize::Original,
                            None,
                            &token,
                        )
                        .expect("the device recovers after an over-budget export");
                    }
                }
            }
        }
    }
}
