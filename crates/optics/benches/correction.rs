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

criterion_group!(correction, correction_benchmark);
criterion_main!(correction);
