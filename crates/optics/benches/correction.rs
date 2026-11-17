use criterion::{Criterion, criterion_group, criterion_main};
use rohditor_image::{LinearRgbImage, LinearRgbSpace};
use rohditor_optics::{CorrectionComponents, OpticsQuery, OpticsService, ProfileRequest};

fn correction_benchmark(c: &mut Criterion) {
    let service = OpticsService::load_bundled().expect("bundled optics database");
    let query = OpticsQuery {
        camera_make: "Sony".to_owned(),
        camera_model: "ILCE-6400".to_owned(),
        camera_clean_make: "Sony".to_owned(),
        camera_clean_model: "ILCE-6400".to_owned(),
        lens_make: Some("Tamron".to_owned()),
        lens_model: Some("Tamron 17-70mm F/2.8 Di III-A VC RXD".to_owned()),
        focal_length_mm: Some(35.0),
        aperture_f_number: Some(4.0),
        focus_distance_m: Some(10.0),
    };
    let plan = service
        .resolve_plan(
            &query,
            ProfileRequest::Automatic,
            CorrectionComponents {
                distortion: true,
                vignetting: true,
                chromatic_aberration: true,
            },
            512,
            384,
        )
        .expect("profile plan");
    let image = LinearRgbImage::new(
        512,
        384,
        512 * 3,
        LinearRgbSpace::CameraNative,
        vec![0.5; 512 * 384 * 3],
    )
    .expect("image");
    c.bench_function("camera_native_lens_correction", |bencher| {
        bencher.iter(|| {
            let result = plan.clone().apply(image.clone()).expect("correction");
            criterion::black_box(result.image);
        });
    });
}

/// Full-size optics-only measurement seam (issue #55): the issue-#55 fixture
/// (Sony ILCE-6400 + Tamron 17-70 @35) at the native 6048x4024 A6400 sensor
/// size. This is what opening a RAW costs on the interactive preview path, so
/// it stays in the repo as a permanent seam.
fn full_size_benchmark(c: &mut Criterion) {
    const WIDTH: usize = 6_048;
    const HEIGHT: usize = 4_024;
    let service = OpticsService::load_bundled().expect("bundled optics database");
    let plan = service
        .resolve_plan(
            &fixture_query(),
            ProfileRequest::Automatic,
            CorrectionComponents {
                distortion: true,
                vignetting: true,
                chromatic_aberration: true,
            },
            WIDTH,
            HEIGHT,
        )
        .expect("full-size profile plan");
    let pixels = (0..WIDTH * HEIGHT * 3)
        .map(|index| (512 + (index % 15_000)) as f32 / 16_383.0)
        .collect::<Vec<_>>();
    let image = LinearRgbImage::new(
        WIDTH,
        HEIGHT,
        WIDTH * 3,
        LinearRgbSpace::CameraNative,
        pixels,
    )
    .expect("full-size image");
    c.bench_function("full_size_camera_native_lens_correction", |bencher| {
        // LargeInput keeps the input clone outside the measured region, so
        // the seam measures the optics apply itself, not the harness copy.
        bencher.iter_batched(
            || image.clone(),
            |input| {
                let result = plan.clone().apply(input).expect("correction");
                criterion::black_box(result.image)
            },
            criterion::BatchSize::LargeInput,
        );
    });
}

fn fixture_query() -> OpticsQuery {
    OpticsQuery {
        camera_make: "Sony".to_owned(),
        camera_model: "ILCE-6400".to_owned(),
        camera_clean_make: "Sony".to_owned(),
        camera_clean_model: "ILCE-6400".to_owned(),
        lens_make: Some("Tamron".to_owned()),
        lens_model: Some("Tamron 17-70mm F/2.8 Di III-A VC RXD".to_owned()),
        focal_length_mm: Some(35.0),
        aperture_f_number: Some(4.0),
        focus_distance_m: Some(10.0),
    }
}

criterion_group!(correction, correction_benchmark, full_size_benchmark);
criterion_main!(correction);
