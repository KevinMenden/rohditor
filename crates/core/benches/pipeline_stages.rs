use std::hint::black_box;

use criterion::{BatchSize, Criterion, Throughput, criterion_group, criterion_main};
use rohditor_core::{
    OutputPolicy, RawCropPolicy, apply_adjustments, normalize_raw_preview, render_display_srgb8,
};
use rohditor_edit::{
    BrushMask, BrushStroke, EditRecipe, LinearGradient, LocalSettings, MaskComponent, MaskLayer,
    MaskMode, MaskPoint, MaskShape, RenderingProfileSelection,
};
use rohditor_image::{LinearRgbImage, LinearRgbSpace, Orientation};

mod common;

use common::must;

const SENSOR_WIDTH: usize = 6_048;
const SENSOR_HEIGHT: usize = 4_024;
const PREVIEW_WIDTH: usize = 2_560;
const PREVIEW_HEIGHT: usize = 1_703;

fn benchmark_normalization(criterion: &mut Criterion) {
    let frame = common::raw_frame(SENSOR_WIDTH, SENSOR_HEIGHT);
    let mut group = criterion.benchmark_group("normalization");
    group.throughput(Throughput::Elements(
        (PREVIEW_WIDTH * PREVIEW_HEIGHT) as u64,
    ));
    group.bench_function("6048x4024_to_2560_long_edge", |bencher| {
        bencher.iter(|| {
            must(normalize_raw_preview(
                black_box(&frame),
                RawCropPolicy::Recommended,
                PREVIEW_WIDTH,
            ))
        });
    });
    group.finish();
}

fn benchmark_adjustments(criterion: &mut Criterion) {
    let image = representative_linear_image();
    let mut group = criterion.benchmark_group("adjustments");
    group.throughput(Throughput::Elements(
        (PREVIEW_WIDTH * PREVIEW_HEIGHT) as u64,
    ));
    let recipes = [
        ("standard_2560x1703", EditRecipe::default()),
        ("neutral_2560x1703", neutral_recipe()),
        ("fused_global_2560x1703", {
            let mut recipe = neutral_recipe();
            recipe.light.exposure_ev = 0.7;
            recipe.light.contrast = 0.25;
            recipe.color.saturation = 1.2;
            recipe
        }),
        ("tonal_curve_2560x1703", {
            let mut recipe = neutral_recipe();
            recipe.light.highlights = -0.4;
            recipe.light.shadows = 0.35;
            recipe.light.tone_curve.darks = 0.15;
            recipe.light.tone_curve.highlights = -0.12;
            recipe
        }),
        ("hsl_2560x1703", {
            let mut recipe = neutral_recipe();
            recipe.color.hsl.channels[0].hue = 0.2;
            recipe.color.hsl.channels[3].saturation = -0.3;
            recipe.color.hsl.channels[5].luminance = 0.2;
            recipe
        }),
        ("grading_2560x1703", {
            let mut recipe = neutral_recipe();
            recipe.color.grading.shadows = [0.4, -0.1, 0.2];
            recipe.color.grading.midtones = [-0.2, 0.25, 0.1];
            recipe.color.grading.highlights = [0.1, 0.2, -0.3];
            recipe
        }),
    ];
    for (name, recipe) in recipes {
        group.bench_function(name, |bencher| {
            bencher.iter_batched(
                || image.clone(),
                |mut working| {
                    must(apply_adjustments(&mut working, black_box(&recipe)));
                    black_box(working)
                },
                BatchSize::LargeInput,
            );
        });
    }
    group.finish();
}

/// Mask layers through the whole adjustment stage at fit-preview size and 24 MP. Every layer
/// carries both scene-referred (exposure, temperature) and display-referred (tone, saturation)
/// terms, the most expensive case.
fn benchmark_local_adjustments(criterion: &mut Criterion) {
    const MEGAPIXEL_24: (usize, usize) = (6_000, 4_000);
    let frames = [
        ("2560x1703", PREVIEW_WIDTH, PREVIEW_HEIGHT),
        ("24MP", MEGAPIXEL_24.0, MEGAPIXEL_24.1),
    ];
    for (size, width, height) in frames {
        let image = flat_linear_image(width, height);
        let mut group = criterion.benchmark_group(format!("local_adjustments_{size}"));
        group.throughput(Throughput::Elements((width * height) as u64));
        if width > PREVIEW_WIDTH {
            group.sample_size(10);
        }
        let mut cases: Vec<(String, EditRecipe)> = [0, 1, 4, 16]
            .into_iter()
            .map(|count| (format!("{count}_gradients"), gradient_layers(count)))
            .collect();
        cases.push(("1_brush".to_owned(), brush_layer()));
        for (name, recipe) in cases {
            group.bench_function(name, |bencher| {
                bencher.iter_batched(
                    || image.clone(),
                    |mut working| {
                        must(apply_adjustments(&mut working, black_box(&recipe)));
                        black_box(working)
                    },
                    BatchSize::LargeInput,
                );
            });
        }
        group.finish();
    }
}

fn local_settings() -> LocalSettings {
    LocalSettings {
        exposure_ev: 0.4,
        temperature: 0.2,
        contrast: 0.3,
        shadows: 0.25,
        saturation: 0.2,
        ..LocalSettings::default()
    }
}

fn layer(id: u32, shape: MaskShape) -> MaskLayer {
    MaskLayer {
        id,
        name: String::new(),
        enabled: true,
        opacity: 0.9,
        components: vec![MaskComponent {
            mode: MaskMode::Add,
            invert: false,
            shape,
        }],
        settings: local_settings(),
    }
}

fn point(x: f32, y: f32) -> MaskPoint {
    MaskPoint { x, y }
}

/// `count` overlapping linear gradients at different angles, so most pixels see several layers.
fn gradient_layers(count: u32) -> EditRecipe {
    let mut recipe = neutral_recipe();
    for id in 0..count {
        let t = id as f32 / count.max(1) as f32;
        recipe.local.layers.push(layer(
            id,
            MaskShape::Linear(LinearGradient {
                start: point(0.1 + 0.3 * t, 0.2),
                end: point(0.6 + 0.3 * t, 0.9 - 0.4 * t),
            }),
        ));
    }
    recipe
}

/// 50 wavy strokes of 200 points, as in the mask crate's benchmark.
fn brush_layer() -> EditRecipe {
    let strokes = (0..50)
        .map(|s| {
            let phase = s as f32 * 0.7;
            let (x0, y0) = (
                0.1 + 0.8 * ((s * 37) % 50) as f32 / 50.0,
                0.1 + 0.8 * ((s * 11) % 50) as f32 / 50.0,
            );
            BrushStroke {
                points: (0..200)
                    .map(|i| {
                        let t = i as f32 / 199.0;
                        point(
                            x0 + 0.25 * (t - 0.5) * phase.cos(),
                            y0 + 0.25 * (t - 0.5) * phase.sin() + 0.03 * (t * 12.0 + phase).sin(),
                        )
                    })
                    .collect(),
                radius: 0.02,
                feather: 0.5,
                flow: 0.6,
                erase: false,
            }
        })
        .collect();
    let mut recipe = neutral_recipe();
    recipe
        .local
        .layers
        .push(layer(0, MaskShape::Brush(BrushMask { strokes })));
    recipe
}

fn neutral_recipe() -> EditRecipe {
    let mut recipe = EditRecipe::default();
    recipe.rendering.profile = RenderingProfileSelection::NEUTRAL;
    recipe
}

fn benchmark_output_conversion(criterion: &mut Criterion) {
    let image = representative_linear_image();
    let mut group = criterion.benchmark_group("output_conversion");
    group.throughput(Throughput::Elements(
        (PREVIEW_WIDTH * PREVIEW_HEIGHT) as u64,
    ));
    group.bench_function("rec2020_to_srgb8_2560x1703", |bencher| {
        bencher.iter(|| {
            must(render_display_srgb8(
                black_box(&image),
                Orientation::Normal,
                OutputPolicy::ClipToSrgb,
            ))
        });
    });
    group.finish();
}

fn representative_linear_image() -> LinearRgbImage<f32> {
    flat_linear_image(PREVIEW_WIDTH, PREVIEW_HEIGHT)
}

fn flat_linear_image(width: usize, height: usize) -> LinearRgbImage<f32> {
    let row_stride = width * 3;
    let data = vec![0.18; row_stride * height];
    must(LinearRgbImage::new(
        width,
        height,
        row_stride,
        LinearRgbSpace::Rec2020D65,
        data,
    ))
}

criterion_group!(
    benches,
    benchmark_normalization,
    benchmark_adjustments,
    benchmark_local_adjustments,
    benchmark_output_conversion
);
criterion_main!(benches);
