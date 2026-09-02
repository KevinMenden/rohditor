use std::fmt::Display;
use std::hint::black_box;
use std::sync::Arc;

use criterion::{BatchSize, Criterion, Throughput, criterion_group, criterion_main};
use rohditor_core::{
    CropPolicy, EditRecipe, LinearRgbImage, LinearRgbSpace, OutputPolicy, apply_adjustments,
    normalize_raw_preview, render_display_srgb8,
};
use rohditor_raw::{
    CaptureMetadata, CfaPattern, LevelPattern, PhotometricInterpretation, RawFileInfo, RawFrame,
    RawOrientation,
};

const SENSOR_WIDTH: usize = 6_048;
const SENSOR_HEIGHT: usize = 4_024;
const PREVIEW_WIDTH: usize = 2_560;
const PREVIEW_HEIGHT: usize = 1_703;

fn benchmark_normalization(criterion: &mut Criterion) {
    let frame = representative_raw_frame();
    let mut group = criterion.benchmark_group("normalization");
    group.throughput(Throughput::Elements(
        (PREVIEW_WIDTH * PREVIEW_HEIGHT) as u64,
    ));
    group.bench_function("6048x4024_to_2560_long_edge", |bencher| {
        bencher.iter(|| {
            must(normalize_raw_preview(
                black_box(&frame),
                CropPolicy::Recommended,
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
        ("fused_global_2560x1703", {
            let mut recipe = EditRecipe::default();
            recipe.light.exposure_ev = 0.7;
            recipe.light.contrast = 0.25;
            recipe.color.saturation = 1.2;
            recipe
        }),
        ("tonal_curve_2560x1703", {
            let mut recipe = EditRecipe::default();
            recipe.light.highlights = -0.4;
            recipe.light.shadows = 0.35;
            recipe.light.tone_curve.darks = 0.15;
            recipe.light.tone_curve.highlights = -0.12;
            recipe
        }),
        ("hsl_2560x1703", {
            let mut recipe = EditRecipe::default();
            recipe.color.hsl.channels[0].hue = 0.2;
            recipe.color.hsl.channels[3].saturation = -0.3;
            recipe.color.hsl.channels[5].luminance = 0.2;
            recipe
        }),
        ("grading_2560x1703", {
            let mut recipe = EditRecipe::default();
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
                RawOrientation::Normal,
                OutputPolicy::ClipToSrgb,
            ))
        });
    });
    group.finish();
}

fn representative_raw_frame() -> RawFrame {
    let samples = SENSOR_WIDTH * SENSOR_HEIGHT;
    let mosaic = (0..samples)
        .map(|index| 512_u16.saturating_add((index % 15_000) as u16))
        .collect::<Vec<_>>();
    RawFrame {
        info: raw_info(SENSOR_WIDTH, SENSOR_HEIGHT),
        row_stride: SENSOR_WIDTH,
        mosaic: Arc::from(mosaic),
    }
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

fn raw_info(width: usize, height: usize) -> RawFileInfo {
    RawFileInfo {
        format: "synthetic benchmark".to_owned(),
        make: "Rohditor".to_owned(),
        model: "Representative sensor".to_owned(),
        clean_make: "Rohditor".to_owned(),
        clean_model: "Representative sensor".to_owned(),
        source_size_bytes: 0,
        source_identity: None,
        width,
        height,
        components_per_pixel: 1,
        source_bits_per_sample: Some(14),
        decoded_bits_per_sample: 16,
        compression: None,
        active_area: None,
        crop_area: None,
        photometric_interpretation: PhotometricInterpretation::Cfa {
            pattern: CfaPattern {
                name: "RGGB".to_owned(),
                width: 2,
                height: 2,
            },
        },
        black_levels: LevelPattern {
            values: vec![512.0; 4],
            repeat_width: 2,
            repeat_height: 2,
            components_per_pixel: 1,
        },
        white_levels: vec![16_383.0],
        as_shot_white_balance: [Some(1.0); 4],
        xyz_to_camera: [[0.0; 3]; 4],
        color_matrices: Vec::new(),
        orientation: RawOrientation::Normal,
        capture: CaptureMetadata::default(),
        embedded_preview: None,
    }
}

fn must<T, E: Display>(result: Result<T, E>) -> T {
    match result {
        Ok(value) => value,
        Err(error) => panic!("benchmark fixture failed: {error}"),
    }
}

criterion_group!(
    benches,
    benchmark_normalization,
    benchmark_adjustments,
    benchmark_output_conversion
);
criterion_main!(benches);
