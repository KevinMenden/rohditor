use lensfun::{Camera, Database, Lens, Modifier};
use rohditor_image::{LinearRgbImage, LinearRgbSpace};
use rohditor_optics::{CorrectionComponents, OpticsQuery, OpticsService, ProfileRequest};

const WIDTH: usize = 16;
const HEIGHT: usize = 12;

fn sony_tamron_query() -> OpticsQuery {
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

fn known_pair(database: &Database) -> (&Camera, &Lens) {
    let camera = database
        .cameras
        .iter()
        .find(|camera| camera.model == "ILCE-6400")
        .expect("bundled database should contain Sony ILCE-6400");
    let lens = database
        .lenses
        .iter()
        .find(|lens| {
            lens.model == "E 17-70mm F2.8 B070"
                || lens
                    .model_localized
                    .values()
                    .any(|model| model == "Tamron 17-70mm F/2.8 Di III-A VC RXD")
        })
        .expect("bundled database should contain Tamron 17-70mm");
    (camera, lens)
}

#[test]
fn vignetting_matches_the_pinned_lensfun_modifier() {
    let service = OpticsService::load_bundled().expect("bundled Lensfun data should load");
    let query = sony_tamron_query();
    let plan = service
        .resolve_plan(
            &query,
            ProfileRequest::Automatic,
            CorrectionComponents {
                distortion: false,
                vignetting: true,
                chromatic_aberration: false,
            },
            WIDTH,
            HEIGHT,
        )
        .expect("vignetting plan");

    let database = Database::load_bundled().expect("bundled Lensfun data should load");
    let (camera, lens) = known_pair(&database);
    let mut modifier = Modifier::new(
        lens,
        35.0,
        camera.crop_factor,
        WIDTH as u32,
        HEIGHT as u32,
        // Lensfun's color pass interprets `reverse = false` as de-vignetting,
        // while the geometry passes use `reverse = true` for correction.
        false,
    );
    assert!(modifier.enable_vignetting_correction(lens, 2.8, 10.0));

    let pixels = (0..WIDTH * HEIGHT * 3)
        .map(|index| (index as f32 - 70.0) / 37.0)
        .collect::<Vec<_>>();
    let mut expected = pixels.clone();
    assert!(modifier.apply_color_modification_f32(&mut expected, 0.0, 0.0, WIDTH, HEIGHT, 3,));
    let image = LinearRgbImage::new(
        WIDTH,
        HEIGHT,
        WIDTH * 3,
        LinearRgbSpace::CameraNative,
        pixels,
    )
    .expect("test image dimensions should be valid");
    let actual = plan.apply(image).expect("vignetting correction");
    // The plan performs its row traversal in parallel and keeps the final
    // coordinate arithmetic in the owned camera-native pipeline. Both paths
    // use the pinned Lensfun coefficients and kernel; allow the small f32
    // rounding difference from their distinct evaluation order.
    assert!(
        actual
            .image
            .data()
            .iter()
            .zip(expected.iter())
            .all(|(actual, expected)| {
                (actual - expected).abs() <= 1.0e-5 * (1.0 + expected.abs())
            })
    );
}
