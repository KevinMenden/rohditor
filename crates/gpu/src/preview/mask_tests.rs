//! CPU/GPU mask parity on the fit preview, and what masks cost per slider move.
use std::time::Instant;

use super::tests::{gpu_test_guard, gpu_test_processor, neutral_recipe, synthetic_frame};
use super::*;
use crate::masks::test_recipes::{
    self, brush, component, layer, linear, luminance, mask_recipes, with_unique_ids,
};
use rohditor_core::{CpuPipeline, PreviewOptions, apply_adjustments, sample_color_range};
use rohditor_edit::{
    ColorSample, LocalSettings, MAX_MASK_COMPONENTS, MAX_MASK_LAYERS, MaskLayer, MaskMode,
    NormalizedCropRect,
};
use rohditor_image::LinearRgbImage;

const WIDTH: usize = 211;
const HEIGHT: usize = 137;

/// Hue runs across the frame and brightness down it, into over-range values at the bottom, so
/// range masks select a shape that is neither symmetric nor aligned with the gradients.
fn fixture(width: usize, height: usize) -> Vec<f32> {
    let mut pixels = Vec::with_capacity(width * height * 3);
    for y in 0..height {
        let level = 1.6 * ((y as f32 + 0.5) / height as f32).powi(2);
        for x in 0..width {
            let hue = 6.0 * (x as f32 + 0.5) / width as f32;
            for phase in [0.0, 4.0, 2.0] {
                let wave = ((hue + phase) % 6.0 - 3.0).abs() - 1.0;
                pixels.push(level * (0.15 + 0.85 * wave.clamp(0.0, 1.0)));
            }
        }
    }
    pixels
}

fn upload_fixture(
    processor: &GpuPreviewProcessor,
    width: usize,
    height: usize,
) -> GpuPreviewSource {
    let base = CpuPipeline::default()
        .prepare_preview_base(
            &synthetic_frame(Orientation::Normal),
            &neutral_recipe(),
            PreviewOptions::default(),
        )
        .expect("fixture base");
    let mut upload = GpuPreviewUpload::from_demosaiced_base(&base).expect("fixture upload");
    upload.width = width as u32;
    upload.height = height as u32;
    upload.texels = pack_rgba32f(
        &fixture(width, height),
        width,
        height,
        width * 3,
        &CancellationToken::new(),
    )
    .expect("fixture packs");
    processor.upload_prepared(upload).expect("fixture uploads")
}

fn fixture_image(width: usize, height: usize) -> LinearRgbImage<f32> {
    LinearRgbImage::new(
        width,
        height,
        width * 3,
        LinearRgbSpace::Rec2020D65,
        fixture(width, height),
    )
    .expect("fixture image")
}

fn fixture_sample(recipe: &EditRecipe) -> ColorSample {
    sample_color_range(
        &fixture_image(WIDTH, HEIGHT),
        Orientation::Normal,
        recipe,
        (0.4, 0.55),
    )
    .expect("the fixture has colour there")
}

fn global_edits() -> EditRecipe {
    let mut recipe = neutral_recipe();
    recipe.light.exposure_ev = -0.3;
    recipe.light.contrast = 0.2;
    recipe.light.tone_curve.lights = 0.1;
    recipe.color.saturation = 1.2;
    recipe.color.grading.shadows = [0.1, 0.0, -0.1];
    recipe
}

const CROPS: [Option<NormalizedCropRect>; 3] = [
    None,
    Some(NormalizedCropRect {
        left: 0.1,
        top: 0.2,
        right: 0.75,
        bottom: 0.9,
    }),
    Some(NormalizedCropRect {
        left: 0.55,
        top: 0.0,
        right: 1.0,
        bottom: 0.4,
    }),
];

/// Largest and mean sRGB code difference between a GPU frame and the CPU image, which is compared
/// through the same orientation and crop so a mask must stay attached to the content.
fn display_difference(
    processor: &GpuPreviewProcessor,
    frame: &GpuPreviewFrame,
    cpu: &LinearRgbImage<f32>,
    recipe: &EditRecipe,
) -> (u8, f64) {
    let orientation = recipe
        .geometry
        .orientation_override
        .unwrap_or(Orientation::Normal);
    let geometry =
        OutputGeometry::new(cpu.width(), cpu.height(), orientation, recipe.geometry.crop)
            .expect("fixture geometry");
    let display =
        rohditor_core::render_display_srgb8(cpu, Orientation::Normal, OutputPolicy::ClipToSrgb)
            .expect("CPU display");
    let gpu = processor.readback_display(frame).expect("GPU readback");
    assert_eq!(
        (gpu.width as usize, gpu.height as usize),
        geometry.output_dimensions()
    );
    let (mut maximum, mut sum) = (0, 0_u64);
    for (y, row) in gpu.rgba.chunks_exact(gpu.width as usize * 4).enumerate() {
        for (x, pixel) in row.as_chunks::<4>().0.iter().enumerate() {
            let (sx, sy) = geometry.source_coordinate_in_bounds(x, y);
            let reference = &display.data()[(sy * cpu.width() + sx) * 3..][..3];
            for channel in 0..3 {
                let difference = pixel[channel].abs_diff(reference[channel]);
                maximum = maximum.max(difference);
                sum += u64::from(difference);
            }
        }
    }
    (maximum, sum as f64 / (gpu.rgba.len() / 4 * 3) as f64)
}

#[test]
#[ignore = "requires a hardware Vulkan adapter; mask parity on the fit preview"]
fn fit_preview_masks_match_cpu_with_orientation_and_crop() {
    let _guard = gpu_test_guard();
    let Some(processor) = gpu_test_processor() else {
        return;
    };
    let source = upload_fixture(&processor, WIDTH, HEIGHT);
    for (base_name, base) in [("neutral", neutral_recipe()), ("edited", global_edits())] {
        let mut unmasked = fixture_image(WIDTH, HEIGHT);
        apply_adjustments(&mut unmasked, &base).expect("CPU reference");
        for (name, mut recipe) in mask_recipes(&base, fixture_sample(&base)) {
            let mut cpu = fixture_image(WIDTH, HEIGHT);
            apply_adjustments(&mut cpu, &recipe).expect("CPU reference");
            // A mask that selects nothing would pass parity without testing anything.
            let changed = cpu
                .data()
                .as_chunks::<3>()
                .0
                .iter()
                .zip(unmasked.data().as_chunks::<3>().0)
                .filter(|(a, b)| a != b)
                .count();
            let total = WIDTH * HEIGHT;
            assert!(
                changed > total / 50,
                "{base_name} {name} changes only {changed} of {total} pixels"
            );
            let mut worst = (0_u8, 0.0_f64);
            for orientation in [
                Orientation::Normal,
                Orientation::Rotate90,
                Orientation::Rotate180,
                Orientation::Transverse,
                Orientation::HorizontalFlip,
            ] {
                recipe.geometry.orientation_override = Some(orientation);
                for crop in CROPS {
                    recipe.geometry.crop = crop;
                    let frame = processor
                        .render(&source, &recipe, OutputPolicy::ClipToSrgb, None)
                        .expect("GPU render");
                    let (maximum, mean) = display_difference(&processor, &frame, &cpu, &recipe);
                    worst = (worst.0.max(maximum), worst.1.max(mean));
                    assert!(
                        maximum <= 2,
                        "{base_name} {name} {orientation:?} {crop:?}: max {maximum}, mean {mean}"
                    );
                }
            }
            eprintln!(
                "fit {base_name} {name}: max {} codes, worst-crop mean {:.4}, {changed}/{total} pixels masked",
                worst.0, worst.1
            );
        }
    }
}

#[test]
#[ignore = "requires a hardware Vulkan adapter; compares linear output before 8-bit encoding"]
fn fit_preview_masks_match_cpu_in_linear_light() {
    let _guard = gpu_test_guard();
    let Some(processor) = gpu_test_processor() else {
        return;
    };
    let source = upload_fixture(&processor, WIDTH, HEIGHT);
    let base = global_edits();
    for (name, recipe) in mask_recipes(&base, fixture_sample(&base)) {
        let mut cpu = fixture_image(WIDTH, HEIGHT);
        apply_adjustments(&mut cpu, &recipe).expect("CPU reference");
        let frame = processor
            .render(&source, &recipe, OutputPolicy::ClipToSrgb, None)
            .expect("GPU render");
        let linear = super::color_tests::read_working(&processor, &frame);
        let mut maximum = 0.0_f32;
        for (sample, (&actual, &expected)) in linear.iter().zip(cpu.data()).enumerate() {
            let error = (actual - expected).abs();
            maximum = maximum.max(error);
            // RGBA16F output rounds by up to about 0.1%; 16-bit coverage adds 1/131070 of a mask.
            assert!(
                actual.is_finite() && error <= 2.0e-4 + 0.003 * expected.abs(),
                "{name} sample {sample}: GPU {actual}, CPU {expected}"
            );
        }
        eprintln!("linear {name}: max error {maximum}");
    }
}

/// Non-neutral masks at the recipe limits: the tables and shader loops must reach every slot.
#[test]
#[ignore = "requires a hardware Vulkan adapter; parity at the mask layer and component limits"]
fn the_recipe_limits_fit_the_tables_and_match_cpu() {
    let _guard = gpu_test_guard();
    let Some(processor) = gpu_test_processor() else {
        return;
    };
    let source = upload_fixture(&processor, WIDTH, HEIGHT);
    let mut recipe = neutral_recipe();
    let layers: Vec<MaskLayer> = (0..MAX_MASK_LAYERS)
        .map(|index| {
            let mut components =
                vec![component(MaskMode::Add, false, linear()); MAX_MASK_COMPONENTS - 1];
            // Only the very last component of each layer narrows it, and only the last layer
            // has a brush, so a truncated loop shows.
            components.push(component(
                MaskMode::Intersect,
                index % 2 == 0,
                if index + 1 == MAX_MASK_LAYERS {
                    brush(0.0)
                } else {
                    luminance()
                },
            ));
            layer(
                components,
                LocalSettings {
                    exposure_ev: 0.05,
                    contrast: 0.02 * (index as f32 + 1.0) / MAX_MASK_LAYERS as f32,
                    ..LocalSettings::default()
                },
            )
        })
        .collect();
    recipe.local.layers = with_unique_ids(layers);
    let mut cpu = fixture_image(WIDTH, HEIGHT);
    apply_adjustments(&mut cpu, &recipe).expect("CPU reference");
    let frame = processor
        .render(&source, &recipe, OutputPolicy::ClipToSrgb, None)
        .expect("GPU render");
    let (maximum, mean) = display_difference(&processor, &frame, &cpu, &recipe);
    eprintln!("32 layers x 16 components: max {maximum} codes, mean {mean:.4}");
    assert!(maximum <= 2, "max {maximum}, mean {mean}");
}

/// Coverage planes follow the strokes across renders, and go away with the last brush.
#[test]
#[ignore = "requires a hardware Vulkan adapter; brush planes are reused and replaced correctly"]
fn fit_coverage_follows_stroke_changes_between_renders() {
    let _guard = gpu_test_guard();
    let Some(processor) = gpu_test_processor() else {
        return;
    };
    let source = upload_fixture(&processor, WIDTH, HEIGHT);
    let exposure = LocalSettings {
        exposure_ev: 1.0,
        ..LocalSettings::default()
    };
    let painted = |offset: f32, settings| {
        let mut recipe = neutral_recipe();
        recipe.local.layers = vec![layer(
            vec![component(MaskMode::Add, false, brush(offset))],
            settings,
        )];
        recipe
    };
    let mut reusable = None;
    for recipe in [
        painted(0.0, exposure),
        // A slider move reuses the plane; the stale reservation of staging settles.
        painted(
            0.0,
            LocalSettings {
                exposure_ev: -1.0,
                ..exposure
            },
        ),
        // A new stroke position must replace it.
        painted(0.2, exposure),
        // Back to no masks, then a brush again.
        neutral_recipe(),
        painted(0.1, exposure),
    ] {
        let mut cpu = fixture_image(WIDTH, HEIGHT);
        apply_adjustments(&mut cpu, &recipe).expect("CPU reference");
        let frame = processor
            .render(&source, &recipe, OutputPolicy::ClipToSrgb, reusable.take())
            .expect("GPU render");
        let (maximum, mean) = display_difference(&processor, &frame, &cpu, &recipe);
        assert!(maximum <= 2, "max {maximum}, mean {mean}");
        reusable = Some(frame);
    }
}

/// A failed update must not leave a plane holding strokes the cache does not list.
#[test]
#[ignore = "requires a hardware Vulkan adapter; a degenerate brush leaves the planes consistent"]
fn a_failed_coverage_update_leaves_no_stale_plane_behind() {
    let _guard = gpu_test_guard();
    let Some(processor) = gpu_test_processor() else {
        return;
    };
    let source = upload_fixture(&processor, WIDTH, HEIGHT);
    let exposure = LocalSettings {
        exposure_ev: 1.0,
        ..LocalSettings::default()
    };
    let painted = |first: f32, second: rohditor_edit::MaskShape| {
        let mut recipe = neutral_recipe();
        recipe.local.layers = with_unique_ids(vec![
            layer(
                vec![component(MaskMode::Add, false, brush(first))],
                exposure,
            ),
            layer(vec![component(MaskMode::Add, false, second)], exposure),
        ]);
        recipe
    };
    let render =
        |recipe: &EditRecipe| processor.render(&source, recipe, OutputPolicy::ClipToSrgb, None);
    let good = painted(0.0, brush(0.1));
    render(&good).expect("GPU render");

    // Validation accepts this zigzag, but at the 0.25 pixel dab spacing it needs more dabs than
    // a brush may place, so it fails while its plane is being prepared. The first brush changed
    // in the same render, so its plane comes first.
    let mut dense = rohditor_edit::BrushMask::default();
    dense.strokes.push(rohditor_edit::BrushStroke {
        points: (0..2000)
            .map(|index| rohditor_edit::MaskPoint {
                x: if index % 2 == 0 { -1.0 } else { 2.0 },
                y: 0.5,
            })
            .collect(),
        radius: 0.0005,
        feather: 0.5,
        flow: 1.0,
        erase: false,
    });
    let failing = painted(0.2, rohditor_edit::MaskShape::Brush(dense));
    match render(&failing) {
        Err(GpuPreviewError::InvalidInput { .. }) => {
            // The original recipe must still show its own strokes on both planes.
            let frame = render(&good).expect("GPU render");
            let mut cpu = fixture_image(WIDTH, HEIGHT);
            apply_adjustments(&mut cpu, &good).expect("CPU reference");
            let (maximum, mean) = display_difference(&processor, &frame, &cpu, &good);
            assert!(maximum <= 2, "max {maximum}, mean {mean}");
        }
        other => panic!(
            "the fixture must reach the dab limit to test this path: {:?}",
            other.map(|_| ())
        ),
    }
}

fn percentile(samples: &mut [f64], fraction: f64) -> f64 {
    samples.sort_by(f64::total_cmp);
    samples[((samples.len() - 1) as f64 * fraction).round() as usize]
}

fn latency_layers(count: usize, brush_every: usize) -> EditRecipe {
    let mut recipe = neutral_recipe();
    recipe.local.layers = with_unique_ids(
        (0..count)
            .map(|index| {
                let shape = if brush_every != 0 && index % brush_every == 0 {
                    brush(index as f32 / 200.0)
                } else {
                    test_recipes::radial()
                };
                layer(
                    vec![
                        component(MaskMode::Add, false, shape),
                        component(MaskMode::Intersect, false, luminance()),
                    ],
                    LocalSettings {
                        exposure_ev: 0.1,
                        contrast: 0.1,
                        saturation: 0.1,
                        ..LocalSettings::default()
                    },
                )
            })
            .collect(),
    );
    recipe
}

/// A later point on the last stroke of the last brush layer, as painting adds.
fn extend_last_stroke(recipe: &mut EditRecipe, step: usize) {
    let shape = recipe.local.layers.iter_mut().rev().find_map(|layer| {
        layer
            .components
            .iter_mut()
            .find_map(|component| match &mut component.shape {
                rohditor_edit::MaskShape::Brush(brush) => Some(brush),
                _ => None,
            })
    });
    let stroke = shape
        .and_then(|brush| brush.strokes.first_mut())
        .expect("a painted layer");
    for index in 0..=step {
        let t = index as f32 / 60.0;
        stroke.points.push(rohditor_edit::MaskPoint {
            x: 0.1 + 0.8 * t,
            y: 0.9 - 0.3 * t,
        });
    }
}

/// Slider-to-result time with a neutral baseline (#36), on a default-size fit preview: from the
/// edit until the display pixels exist, on both sides. Each move changes one slider of the last
/// layer, or the strokes when `paint` is set. Excludes source preparation and presentation,
/// which masks do not change.
#[test]
#[ignore = "requires a hardware Vulkan adapter; prints fit-preview slider latency, GPU vs CPU"]
fn mask_slider_latency_on_the_fit_preview() {
    let _guard = gpu_test_guard();
    let Some(processor) = gpu_test_processor() else {
        return;
    };
    const FIT: (usize, usize) = (2560, 1707);
    const MOVES: usize = 30;
    let source = upload_fixture(&processor, FIT.0, FIT.1);
    let cpu_input = fixture_image(FIT.0, FIT.1);
    let report = |name: &str, recipe: EditRecipe, paint: bool| {
        let moved = |step: usize| {
            let mut recipe = recipe.clone();
            if paint {
                extend_last_stroke(&mut recipe, step);
            } else {
                let value = 0.2 + 0.01 * step as f32;
                match recipe.local.layers.last_mut() {
                    Some(layer) => layer.settings.contrast = value,
                    None => recipe.light.contrast = value,
                }
            }
            recipe
        };
        // The first render builds the textures and every brush plane.
        let started = Instant::now();
        let mut reusable = Some(
            processor
                .render(&source, &moved(0), OutputPolicy::ClipToSrgb, None)
                .expect("GPU render"),
        );
        processor.wait_for_queue().expect("queue");
        let first = started.elapsed().as_secs_f64() * 1e3;
        let mut gpu_times = Vec::new();
        for step in 1..=MOVES {
            let recipe = moved(step);
            let started = Instant::now();
            let frame = processor
                .render(&source, &recipe, OutputPolicy::ClipToSrgb, reusable.take())
                .expect("GPU render");
            processor.wait_for_queue().expect("queue");
            gpu_times.push(started.elapsed().as_secs_f64() * 1e3);
            reusable = Some(frame);
        }
        let mut cpu_times = Vec::new();
        for step in 1..=5 {
            let recipe = moved(step);
            let mut image = cpu_input.clone();
            let started = Instant::now();
            apply_adjustments(&mut image, &recipe).expect("CPU reference");
            rohditor_core::render_display_srgb8(
                &image,
                Orientation::Normal,
                OutputPolicy::ClipToSrgb,
            )
            .expect("CPU display");
            cpu_times.push(started.elapsed().as_secs_f64() * 1e3);
        }
        eprintln!(
            "{name}: GPU p50 {:.1} ms, p95 {:.1} ms, max {:.1} ms, first {first:.1} ms; CPU p50 {:.1} ms",
            percentile(&mut gpu_times, 0.5),
            percentile(&mut gpu_times, 0.95),
            percentile(&mut gpu_times, 1.0),
            percentile(&mut cpu_times, 0.5),
        );
    };
    report("no masks (baseline)", neutral_recipe(), false);
    for count in [1, 8, 32] {
        report(
            &format!("{count} radial+luminance layers"),
            latency_layers(count, 0),
            false,
        );
    }
    report(
        "8 brush layers, moving a slider",
        latency_layers(8, 1),
        false,
    );
    report(
        "32 layers with 8 brushes, moving a slider",
        latency_layers(32, 4),
        false,
    );
    report("1 brush layer, painting", latency_layers(1, 1), true);
    report("8 brush layers, painting one", latency_layers(8, 1), true);
}
