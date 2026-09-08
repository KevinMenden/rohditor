use rayon::ThreadPoolBuilder;
use rohditor_image::{LinearRgbImage, LinearRgbSpace};
use rohditor_optics::{
    Cancellation, CorrectionComponents, OpticsError, OpticsQuery, OpticsService, ProfileRequest,
};

#[test]
fn no_requested_components_is_a_pixel_noop() {
    let service = OpticsService::load_bundled().expect("bundled Lensfun data should load");
    let query = OpticsQuery {
        camera_make: "Sony".to_owned(),
        camera_model: "ILCE-6400".to_owned(),
        camera_clean_make: "Sony".to_owned(),
        camera_clean_model: "Alpha 6400".to_owned(),
        lens_make: Some("Tamron".to_owned()),
        lens_model: Some("Tamron 17-70mm F/2.8 Di III-A VC RXD".to_owned()),
        focal_length_mm: Some(35.0),
        aperture_f_number: Some(2.8),
        focus_distance_m: Some(10.0),
    };
    let plan = service
        .resolve_plan(
            &query,
            ProfileRequest::Automatic,
            CorrectionComponents::none(),
            8,
            8,
        )
        .expect("profile should resolve even when all components are disabled");
    let pixels = (0..8 * 8 * 3)
        .map(|index| index as f32 / 10.0)
        .collect::<Vec<_>>();
    let expected = pixels.clone();
    let image = LinearRgbImage::new(8, 8, 8 * 3, LinearRgbSpace::CameraNative, pixels)
        .expect("test image dimensions should be valid");
    let result = plan.apply(image).expect("no-op correction should complete");
    assert_eq!(result.image.data(), expected.as_slice());
    assert_eq!(result.output_bytes, 0);
    assert_eq!(result.scratch_bytes, 0);
}

#[test]
fn vignetting_only_corrects_in_place_without_remap_buffers() {
    let service = OpticsService::load_bundled().expect("bundled Lensfun data should load");
    let query = OpticsQuery {
        camera_make: "Sony".to_owned(),
        camera_model: "ILCE-6400".to_owned(),
        camera_clean_make: "Sony".to_owned(),
        camera_clean_model: "Alpha 6400".to_owned(),
        lens_make: Some("Tamron".to_owned()),
        lens_model: Some("Tamron 17-70mm F/2.8 Di III-A VC RXD".to_owned()),
        focal_length_mm: Some(35.0),
        aperture_f_number: Some(2.8),
        focus_distance_m: Some(10.0),
    };
    let plan = service
        .resolve_plan(
            &query,
            ProfileRequest::Automatic,
            CorrectionComponents {
                distortion: false,
                vignetting: true,
                chromatic_aberration: false,
            },
            8,
            8,
        )
        .expect("profile should provide vignetting calibration");
    assert!(plan.provenance().applied.vignetting);

    let pixels = vec![1.0_f32; 8 * 8 * 3];
    let image = LinearRgbImage::new(8, 8, 8 * 3, LinearRgbSpace::CameraNative, pixels)
        .expect("test image dimensions should be valid");
    let input_ptr = image.data().as_ptr();
    let result = plan
        .apply(image)
        .expect("vignetting correction should complete");
    assert_eq!(result.image.data().as_ptr(), input_ptr);
    assert_eq!(result.output_bytes, 0);
    assert_eq!(result.scratch_bytes, 0);
}

#[test]
fn correction_accepts_non_tight_rows_without_touching_padding() {
    let service = OpticsService::load_bundled().expect("bundled Lensfun data should load");
    let plan = service
        .resolve_plan(
            &query(),
            ProfileRequest::Automatic,
            CorrectionComponents {
                distortion: false,
                vignetting: true,
                chromatic_aberration: false,
            },
            8,
            8,
        )
        .expect("profile should provide vignetting calibration");
    let row_stride = 8 * 3 + 2;
    let mut pixels = vec![0.0_f32; row_stride * 8];
    for row in pixels.chunks_exact_mut(row_stride) {
        for pixel in row[..8 * 3].chunks_exact_mut(3) {
            pixel.copy_from_slice(&[1.0, 2.0, 3.0]);
        }
        row[8 * 3..].fill(77.0);
    }
    let image = LinearRgbImage::new(8, 8, row_stride, LinearRgbSpace::CameraNative, pixels)
        .expect("non-tight image dimensions should be valid");
    let result = plan.apply(image).expect("vignetting correction");
    assert_eq!(result.image.row_stride(), row_stride);
    assert!(
        result
            .image
            .data()
            .chunks_exact(row_stride)
            .all(|row| row[8 * 3..].iter().all(|value| *value == 77.0))
    );
    assert!(result.image.data().chunks_exact(row_stride).all(|row| {
        row[..8 * 3]
            .chunks_exact(3)
            .all(|pixel| (pixel[1] / pixel[0] - 2.0).abs() < 1.0e-5)
    }));
}

#[test]
fn combined_correction_is_bit_identical_across_thread_counts() {
    let service = OpticsService::load_bundled().expect("bundled Lensfun data should load");
    let plan = service
        .resolve_plan(
            &query(),
            ProfileRequest::Automatic,
            CorrectionComponents {
                distortion: true,
                vignetting: true,
                chromatic_aberration: true,
            },
            16,
            12,
        )
        .expect("all-components profile should resolve");
    let pixels = (0..16 * 12 * 3)
        .map(|index| (index as f32 - 100.0) / 37.0)
        .collect::<Vec<_>>();
    let image = LinearRgbImage::new(16, 12, 16 * 3, LinearRgbSpace::CameraNative, pixels)
        .expect("test image dimensions should be valid");
    let one = ThreadPoolBuilder::new()
        .num_threads(1)
        .build()
        .expect("single-thread pool");
    let many = ThreadPoolBuilder::new()
        .num_threads(4)
        .build()
        .expect("multi-thread pool");
    let single = one
        .install(|| plan.clone().apply(image.clone()))
        .expect("single-thread correction");
    let multiple = many
        .install(|| plan.apply(image))
        .expect("multi-thread correction");
    assert_eq!(single.image.data(), multiple.image.data());
}

#[test]
fn cancellation_drops_a_remap_without_publishing_partial_output() {
    let service = OpticsService::load_bundled().expect("bundled Lensfun data should load");
    let plan = service
        .resolve_plan(
            &query(),
            ProfileRequest::Automatic,
            CorrectionComponents {
                distortion: true,
                vignetting: false,
                chromatic_aberration: false,
            },
            16,
            12,
        )
        .expect("distortion profile should resolve");
    let image = LinearRgbImage::new(
        16,
        12,
        16 * 3,
        LinearRgbSpace::CameraNative,
        vec![1.0; 16 * 12 * 3],
    )
    .expect("test image dimensions should be valid");
    let error = plan
        .apply_cancellable(image, &AlwaysCancelled)
        .expect_err("cancelled correction must not return an image");
    assert!(matches!(error, OpticsError::Cancelled));
}

fn query() -> OpticsQuery {
    OpticsQuery {
        camera_make: "Sony".to_owned(),
        camera_model: "ILCE-6400".to_owned(),
        camera_clean_make: "Sony".to_owned(),
        camera_clean_model: "Alpha 6400".to_owned(),
        lens_make: Some("Tamron".to_owned()),
        lens_model: Some("Tamron 17-70mm F/2.8 Di III-A VC RXD".to_owned()),
        focal_length_mm: Some(35.0),
        aperture_f_number: Some(2.8),
        focus_distance_m: Some(10.0),
    }
}

struct AlwaysCancelled;

impl Cancellation for AlwaysCancelled {
    fn is_cancelled(&self) -> bool {
        true
    }
}
