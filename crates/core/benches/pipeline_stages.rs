use std::hint::black_box;

use criterion::{BatchSize, Criterion, Throughput, criterion_group, criterion_main};
use rohditor_core::{
    OutputPolicy, RawCropPolicy, apply_adjustments, normalize_raw_preview, render_display_srgb8,
};
use rohditor_edit::{EditRecipe, RenderingProfileSelection};
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
    let row_stride = PREVIEW_WIDTH * 3;
    let data = vec![0.18; row_stride * PREVIEW_HEIGHT];
    must(LinearRgbImage::new(
        PREVIEW_WIDTH,
        PREVIEW_HEIGHT,
        row_stride,
        LinearRgbSpace::Rec2020D65,
        data,
    ))
}

criterion_group!(
    benches,
    benchmark_normalization,
    benchmark_adjustments,
    benchmark_output_conversion
);
criterion_main!(benches);
