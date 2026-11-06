//! CPU/GPU mask parity at full resolution, and what brush layers cost an export.
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use super::super::tests::{gpu_test_guard, neutral_recipe};
use super::tests::{difference, processor};
use super::*;
use crate::masks::test_recipes::{
    brush, colored_frame, component, layer, mask_recipes, with_unique_ids,
};
use rohditor_core::{OutputMedium, OutputSharpeningStrength, sample_color_range};
use rohditor_edit::{ColorSample, LocalSettings, MaskMode, NormalizedCropRect};

fn frame_sample(cpu: &CpuPipeline, frame: &RawFrame, recipe: &EditRecipe) -> ColorSample {
    let base = cpu
        .prepare_preview_base(
            frame,
            recipe,
            rohditor_core::PreviewOptions {
                render: RenderOptions::default(),
                max_long_edge: frame.info.width.max(frame.info.height),
            },
        )
        .expect("fixture base");
    sample_color_range(
        base.image(),
        base.source_orientation(),
        recipe,
        (0.35, 0.65),
    )
    .expect("the fixture has colour there")
}

#[test]
#[ignore = "requires Vulkan; mask parity through every export path against the CPU reference"]
fn exported_masks_match_cpu_at_full_resolution_with_orientation_and_crop() {
    let _guard = gpu_test_guard();
    let mut gpu = processor(false).expect("export device");
    let cpu = CpuPipeline::default();
    let token = CancellationToken::new();
    let sharpened = Some(OutputSharpening {
        medium: OutputMedium::Screen,
        strength: OutputSharpeningStrength::Standard,
    });
    let sizes = [
        ("full", OutputSize::Original, None),
        ("reduced", OutputSize::LongEdge { pixels: 130 }, None),
        ("sharpened", OutputSize::Original, sharpened),
        (
            "reduced sharpened",
            OutputSize::LongEdge { pixels: 170 },
            sharpened,
        ),
    ];
    let crops = [
        None,
        Some(NormalizedCropRect {
            left: 0.08,
            top: 0.12,
            right: 0.8,
            bottom: 0.95,
        }),
    ];
    for orientation in [
        Orientation::Normal,
        Orientation::Rotate90,
        Orientation::Transverse,
    ] {
        let frame = colored_frame(301, 197, orientation);
        let base = neutral_recipe();
        for (name, mut recipe) in mask_recipes(&base, frame_sample(&cpu, &frame, &base)) {
            // A mask that selects nothing would pass parity without testing anything.
            let options = RenderOptions::default();
            let unmasked = cpu
                .render_export(
                    &frame,
                    &base,
                    options,
                    OutputBitDepth::Eight,
                    DitherMode::None,
                    OutputSize::Original,
                    None,
                )
                .expect("CPU export");
            let masked = cpu
                .render_export(
                    &frame,
                    &recipe,
                    options,
                    OutputBitDepth::Eight,
                    DitherMode::None,
                    OutputSize::Original,
                    None,
                )
                .expect("CPU export");
            let (largest_change, _) = difference(&masked.image, &unmasked.image);
            assert!(
                largest_change >= 8,
                "{name} {orientation:?} barely changes the image: {largest_change}"
            );
            let mut worst = [(0, 0.0_f64); 2];
            for crop in crops {
                recipe.geometry.crop = crop;
                for (size_name, size, sharpening) in sizes {
                    for (slot, depth) in [OutputBitDepth::Eight, OutputBitDepth::Sixteen]
                        .into_iter()
                        .enumerate()
                    {
                        let options = RenderOptions::default();
                        let expected = cpu
                            .render_export(
                                &frame,
                                &recipe,
                                options,
                                depth,
                                DitherMode::None,
                                size,
                                sharpening,
                            )
                            .expect("CPU export");
                        let actual = gpu
                            .render(
                                &cpu,
                                &frame,
                                &recipe,
                                options,
                                depth,
                                DitherMode::None,
                                size,
                                sharpening,
                                &token,
                            )
                            .expect("GPU export");
                        let (maximum, mean) = difference(&actual.image, &expected.image);
                        worst[slot] = (worst[slot].0.max(maximum), worst[slot].1.max(mean));
                        // The GPU and CPU sensor stages differ slightly: this fixture differs
                        // by 26 16-bit codes with no mask at all, on the near-zero channel of
                        // saturated pixels, and a mask's gain amplifies that. The mask maths
                        // itself is pinned much tighter by the prepared-source test below.
                        let (max_limit, mean_limit) = if depth == OutputBitDepth::Eight {
                            (2, 0.05)
                        } else {
                            (160, 1.0)
                        };
                        assert!(
                            maximum <= max_limit && mean <= mean_limit,
                            "{name} {orientation:?} {size_name} {crop:?} {depth:?}: max {maximum}, mean {mean}"
                        );
                    }
                }
            }
            eprintln!(
                "export {name} {orientation:?}: 8-bit max {} worst-crop mean {:.4}; 16-bit max {} worst-crop mean {:.3}",
                worst[0].0, worst[0].1, worst[1].0, worst[1].1
            );
        }
    }
}

/// The prepared path takes the CPU's own reconstruction, so it isolates the colour pass and its
/// masks from small GPU/CPU sensor differences. It also has its own band loop and bindings.
#[test]
#[ignore = "requires Vulkan; mask parity on a CPU-prepared source, without the GPU sensor stages"]
fn prepared_export_masks_match_cpu_without_sensor_differences() {
    let _guard = gpu_test_guard();
    let mut gpu = processor(false).expect("export device");
    let cpu = CpuPipeline::default();
    let token = CancellationToken::new();
    for orientation in [
        Orientation::Normal,
        Orientation::Rotate90,
        Orientation::Transverse,
    ] {
        let frame = colored_frame(301, 197, orientation);
        let base = neutral_recipe();
        let mut cases = vec![("no masks", base.clone())];
        cases.extend(mask_recipes(&base, frame_sample(&cpu, &frame, &base)));
        for (name, mut recipe) in cases {
            recipe.geometry.crop = Some(NormalizedCropRect {
                left: 0.08,
                top: 0.12,
                right: 0.8,
                bottom: 0.95,
            });
            let options = RenderOptions::default();
            let prepared = cpu
                .prepare_export_source(&frame, &recipe, options, &token)
                .expect("prepared source");
            let mut worst = (0, 0.0_f64);
            for depth in [OutputBitDepth::Eight, OutputBitDepth::Sixteen] {
                let expected = cpu
                    .render_export(
                        &frame,
                        &recipe,
                        options,
                        depth,
                        DitherMode::None,
                        OutputSize::Original,
                        None,
                    )
                    .expect("CPU export");
                let actual = gpu
                    .render_prepared(
                        &prepared,
                        &recipe,
                        options.output_policy,
                        depth,
                        DitherMode::None,
                        &token,
                    )
                    .expect("GPU export");
                let (maximum, mean) = difference(&actual.image, &expected.image);
                if depth == OutputBitDepth::Sixteen {
                    worst = (maximum, mean);
                }
                let limit = if depth == OutputBitDepth::Eight {
                    1
                } else {
                    16
                };
                assert!(
                    maximum <= limit,
                    "{name} {orientation:?} {depth:?}: max {maximum}, mean {mean}"
                );
            }
            eprintln!(
                "prepared {name} {orientation:?}: 16-bit max {} mean {:.3}",
                worst.0, worst.1
            );
        }
    }
}

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

/// Highest values seen while an export runs, sampled every few milliseconds. The process-wide
/// reservation peak never falls, so it cannot tell one run from the one before it.
#[derive(Default)]
struct Peaks {
    reservation: AtomicU64,
    vram: AtomicU64,
    gtt: AtomicU64,
    done: AtomicBool,
}

/// Reservations and measured memory with 8 brush layers. The kernel's numbers include everything
/// else on the card, so they are printed as growth over the level just before the export.
#[test]
#[ignore = "requires a hardware Vulkan GPU; prints reservation peak and VRAM at 24 and 48 MP"]
fn eight_brush_layer_export_memory_at_24_and_48_mp() {
    let _guard = gpu_test_guard();
    let Some(mut gpu) = processor(true) else {
        return;
    };
    let cpu = CpuPipeline::default();
    let token = CancellationToken::new();
    let settings = LocalSettings {
        exposure_ev: 0.3,
        contrast: 0.2,
        ..LocalSettings::default()
    };
    let mut painted = neutral_recipe();
    painted.local.layers = with_unique_ids(
        (0..8)
            .map(|index| {
                layer(
                    vec![component(MaskMode::Add, false, brush(index as f32 / 40.0))],
                    settings,
                )
            })
            .collect(),
    );
    let plain = neutral_recipe();
    for (name, width, height) in [("24 MP", 6000, 4000), ("48 MP", 8000, 6000)] {
        let frame = colored_frame(width, height, Orientation::Normal);
        for (recipe_name, recipe) in [("no masks", &plain), ("8 brush layers", &painted)] {
            // Let the previous run's resources go before taking the starting level.
            std::thread::sleep(Duration::from_millis(500));
            let (vram_before, gtt_before) = (amdgpu_used("vram"), amdgpu_used("gtt"));
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
            let result = gpu.render(
                &cpu,
                &frame,
                recipe,
                RenderOptions::default(),
                OutputBitDepth::Sixteen,
                DitherMode::None,
                OutputSize::Original,
                None,
                &token,
            );
            let elapsed = started.elapsed();
            peaks.done.store(true, Ordering::Release);
            sampler.join().expect("sampler");
            let result = result.expect("GPU export");
            let growth = |peak: &AtomicU64, before: u64| {
                peak.load(Ordering::Acquire).saturating_sub(before) >> 20
            };
            eprintln!(
                "{name} {recipe_name}: {elapsed:?}, reservation peak {} MiB, VRAM +{} MiB (of {} MiB in use), GTT +{} MiB, uploaded {} MiB, estimate {} MiB",
                peaks.reservation.load(Ordering::Acquire) >> 20,
                growth(&peaks.vram, vram_before),
                vram_before >> 20,
                growth(&peaks.gtt, gtt_before),
                result.uploaded_bytes >> 20,
                result.estimated_gpu_bytes >> 20,
            );
        }
    }
}
