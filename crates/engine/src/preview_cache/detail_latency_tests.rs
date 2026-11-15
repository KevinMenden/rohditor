//! Slider-to-reduced-preview time while noise reduction is dragged (#78, #36).
//!
//! A detail slider changes the captured source, so every move redevelops the camera planes on
//! the GPU before reducing them to the fit preview. This measures that, on a 24 MP frame, from
//! the edit until the reduced preview exists. The colour pass and presentation come after it
//! and do not depend on the detail settings, so they are not included.
use std::time::Instant;

use super::sensor_tests::{prepare, setup};
use super::*;

const MOVES: usize = 20;

/// A noisy gradient mosaic with edges, so the noise profile and every filter have work to do.
fn large_frame(base: RawFrame, width: usize, height: usize) -> RawFrame {
    let mut frame = base;
    frame.info.width = width;
    frame.info.height = height;
    frame.row_stride = width;
    frame.mosaic = (0..width * height)
        .map(|index| {
            let (x, y) = (index % width, index / width);
            let mut value = 9_000.0 + 25_000.0 * x as f32 / width as f32;
            value += 12_000.0 * y as f32 / height as f32;
            if (y / 17) % 2 == 0 && x > width / 2 {
                value -= 7_000.0;
            }
            let grain = ((index as u32).wrapping_mul(2_654_435_761) >> 20) as f32 - 2_048.0;
            (value + 0.6 * grain).clamp(0.0, 65_535.0) as u16
        })
        .collect::<Vec<_>>()
        .into();
    frame
}

fn percentile(samples: &mut [f64], fraction: f64) -> f64 {
    samples.sort_by(f64::total_cmp);
    samples[((samples.len() - 1) as f64 * fraction).round() as usize]
}

#[test]
#[ignore = "requires a hardware Vulkan adapter; prints noise-reduction slider latency at 24 MP"]
fn noise_reduction_slider_latency_at_24_mp() {
    let (mut cache, small) = setup();
    let frame = large_frame(small, 6000, 4000);
    let options = PreviewOptions::default();
    let mut report = |name: &str, recipe: EditRecipe, moved: &dyn Fn(&mut EditRecipe, f32)| {
        // The first preparation develops the source and builds the pipelines.
        let started = Instant::now();
        prepare(&mut cache, &frame, &recipe, options);
        let first = started.elapsed().as_secs_f64() * 1e3;
        let (mut wall, mut capture, mut reduction) = (Vec::new(), Vec::new(), Vec::new());
        for step in 1..=MOVES {
            let mut recipe = recipe.clone();
            moved(&mut recipe, 0.2 + 0.02 * step as f32);
            let started = Instant::now();
            let metrics = prepare(&mut cache, &frame, &recipe, options);
            wall.push(started.elapsed().as_secs_f64() * 1e3);
            capture.push(metrics.capture.total.as_secs_f64() * 1e3);
            reduction.push(metrics.spatial.as_secs_f64() * 1e3);
            assert!(metrics.sensor_gpu, "{name}: left the GPU sensor path");
        }
        eprintln!(
            "{name}: p50 {:.0} ms, p95 {:.0} ms, max {:.0} ms, first {first:.0} ms; of p50: detail stage {:.0} ms, reduction {:.0} ms",
            percentile(&mut wall, 0.5),
            percentile(&mut wall, 0.95),
            percentile(&mut wall, 1.0),
            percentile(&mut capture, 0.5),
            percentile(&mut reduction, 0.5),
        );
    };
    report(
        "no noise reduction, sharpening radius moving (baseline)",
        EditRecipe::default(),
        &|recipe, value| {
            recipe.capture_sharpening.enabled = true;
            recipe.capture_sharpening.radius = 0.4 + value;
        },
    );
    report("chroma moving", EditRecipe::default(), &|recipe, value| {
        recipe.noise_reduction.chroma = value
    });
    let mut with_luma = EditRecipe::default();
    with_luma.noise_reduction.luminance = 0.4;
    report(
        "chroma moving, luma on",
        with_luma.clone(),
        &|recipe, value| {
            recipe.noise_reduction.chroma = value;
        },
    );
    report("luma moving, chroma on", with_luma, &|recipe, value| {
        recipe.noise_reduction.chroma = 0.5;
        recipe.noise_reduction.luminance = value;
    });
    // The CPU path for the same edit, a few moves only.
    let cpu = CpuPipeline::default();
    let mut cpu_times = Vec::new();
    for step in 1..=3 {
        let mut recipe = EditRecipe::default();
        recipe.noise_reduction.chroma = 0.2 + 0.02 * step as f32;
        recipe.noise_reduction.luminance = 0.4;
        let started = Instant::now();
        cpu.render_preview(&frame, &recipe, options)
            .expect("CPU preview");
        cpu_times.push(started.elapsed().as_secs_f64() * 1e3);
    }
    eprintln!(
        "CPU preview with chroma + luma: p50 {:.0} ms",
        percentile(&mut cpu_times, 0.5)
    );
}
