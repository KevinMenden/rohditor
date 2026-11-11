//! Wide-gamut SDR and extended-range linear output stages.
//!
//! Images are deliberately asymmetric (3x2) so a transposed or mirrored
//! result cannot pass by symmetry.

use rohditor_color::{LINEAR_DISPLAY_P3_TO_XYZ_D65, XYZ_D65_TO_LINEAR_REC2020};
use rohditor_core::{
    CancellationToken, DitherMode, OutputGeometry, OutputPolicy, PipelineError,
    linear_srgb_to_srgb, render_display_p3_8_with_geometry, render_display_srgb8_with_geometry,
    render_extended_linear_with_geometry,
};
use rohditor_image::{
    DisplayColorSpace, LinearRgbImage, LinearRgbSpace, Orientation, RgbPrimaries,
};

/// Linear Rec.2020 coordinates of a linear Display P3 colour.
fn p3_to_rec2020(p3: [f32; 3]) -> [f32; 3] {
    LINEAR_DISPLAY_P3_TO_XYZ_D65
        .then(XYZ_D65_TO_LINEAR_REC2020)
        .transform(p3)
}

fn rec2020_image(pixels: [[f32; 3]; 6]) -> LinearRgbImage<f32> {
    LinearRgbImage::new(
        3,
        2,
        9,
        LinearRgbSpace::Rec2020D65,
        pixels.into_iter().flatten().collect(),
    )
    .expect("3x2 image")
}

fn neutral(value: f32) -> [f32; 3] {
    [value; 3]
}

fn geometry(orientation: Orientation) -> OutputGeometry {
    OutputGeometry::new(3, 2, orientation, None).expect("geometry")
}

#[test]
fn p3_8_bit_matches_srgb_on_neutrals_in_every_position() {
    // Same white point and transfer, so neutrals are identical in both spaces.
    let image = rec2020_image([
        neutral(0.0),
        neutral(0.02),
        neutral(0.18),
        neutral(0.4),
        neutral(0.75),
        neutral(1.0),
    ]);
    for orientation in [Orientation::Normal, Orientation::Rotate90] {
        let p3 = render_display_p3_8_with_geometry(
            &image,
            geometry(orientation),
            OutputPolicy::ChromaCompressToSrgb,
            DitherMode::None,
        )
        .expect("p3 render");
        let srgb = render_display_srgb8_with_geometry(
            &image,
            geometry(orientation),
            OutputPolicy::ChromaCompressToSrgb,
        )
        .expect("srgb render");
        assert_eq!(p3.color_space(), DisplayColorSpace::DISPLAY_P3);
        assert_eq!(srgb.color_space(), DisplayColorSpace::SRGB);
        assert_eq!((p3.width(), p3.height()), (srgb.width(), srgb.height()));
        assert_eq!(p3.data(), srgb.data(), "{orientation:?}");
    }
}

#[test]
fn p3_8_bit_keeps_colours_that_srgb_cannot_represent() {
    // Saturated green: inside Display P3, outside sRGB.
    let p3_linear = [0.05, 0.8, 0.12];
    let mut pixels = [neutral(0.5); 6];
    pixels[4] = p3_to_rec2020(p3_linear);
    let image = rec2020_image(pixels);

    for policy in [OutputPolicy::ClipToSrgb, OutputPolicy::ChromaCompressToSrgb] {
        let p3 = render_display_p3_8_with_geometry(
            &image,
            geometry(Orientation::Normal),
            policy,
            DitherMode::None,
        )
        .expect("p3 render");
        let expected = p3_linear.map(|value| (linear_srgb_to_srgb(value) * 255.0).round() as u8);
        // Pixel (1, 1) of the 3x2 image.
        assert_eq!(p3.pixel(1, 1), Some(&expected[..]), "{policy:?}");
    }

    let srgb = render_display_srgb8_with_geometry(
        &image,
        geometry(Orientation::Normal),
        OutputPolicy::ChromaCompressToSrgb,
    )
    .expect("srgb render");
    let p3 = render_display_p3_8_with_geometry(
        &image,
        geometry(Orientation::Normal),
        OutputPolicy::ChromaCompressToSrgb,
        DitherMode::None,
    )
    .expect("p3 render");
    assert_ne!(srgb.pixel(1, 1), p3.pixel(1, 1));
}

#[test]
fn extended_output_keeps_values_above_diffuse_white_and_orientation() {
    // Distinct neutrals, three of them above 1.0.
    let values = [0.25, 0.5, 1.0, 1.5, 2.5, 3.5];
    let image = rec2020_image(values.map(neutral));
    let headroom = 4.0;
    let render = |orientation| {
        render_extended_linear_with_geometry(
            &image,
            geometry(orientation),
            RgbPrimaries::DisplayP3,
            headroom,
            OutputPolicy::ChromaCompressToSrgb,
            &CancellationToken::new(),
        )
        .expect("extended render")
        .0
    };
    let channel = |image: &rohditor_image::ExtendedLinearRgbImage<f32>, x, y| {
        let pixel = image.pixel(x, y).expect("pixel");
        // Neutral in, neutral out.
        assert!((pixel[0] - pixel[1]).abs() < 1.0e-4 && (pixel[2] - pixel[1]).abs() < 1.0e-4);
        pixel[1]
    };

    let normal = render(Orientation::Normal);
    assert_eq!(normal.primaries(), RgbPrimaries::DisplayP3);
    assert_eq!(normal.headroom(), headroom);
    for (index, expected) in values.into_iter().enumerate() {
        let actual = channel(&normal, index % 3, index / 3);
        assert!((actual - expected).abs() < 1.0e-4, "{actual} != {expected}");
    }

    // EXIF 6 turns [[a, b, c], [d, e, f]] into [[d, a], [e, b], [f, c]].
    let rotated = render(Orientation::Rotate90);
    assert_eq!((rotated.width(), rotated.height()), (2, 3));
    let expected = [[1.5, 0.25], [2.5, 0.5], [3.5, 1.0]];
    for (y, row) in expected.into_iter().enumerate() {
        for (x, value) in row.into_iter().enumerate() {
            assert!((channel(&rotated, x, y) - value).abs() < 1.0e-4);
        }
    }
}

#[test]
fn extended_output_has_a_p3_base_with_a_per_pixel_ratio() {
    // A highlight colour inside P3 at 2.5x diffuse white: the extended image
    // is exactly the P3 linear value, and dividing by the SDR base's linear
    // value (same primaries) gives the gain directly.
    let p3_linear = [2.0, 1.2, 0.4];
    let mut pixels = [neutral(0.5); 6];
    pixels[0] = p3_to_rec2020(p3_linear);
    let image = rec2020_image(pixels);
    let (extended, diagnostics) = render_extended_linear_with_geometry(
        &image,
        geometry(Orientation::Normal),
        RgbPrimaries::DisplayP3,
        4.0,
        OutputPolicy::ChromaCompressToSrgb,
        &CancellationToken::new(),
    )
    .expect("extended render");
    let pixel = extended.pixel(0, 0).expect("pixel");
    for (actual, expected) in pixel.iter().zip(p3_linear) {
        assert!((actual - expected).abs() < 1.0e-4, "{actual} != {expected}");
    }
    assert_eq!(diagnostics.total_pixels(), 6);
    assert_eq!(diagnostics.in_gamut_pixels, 6);
}

#[test]
fn extended_output_maps_out_of_gamut_colours_into_the_range() {
    // Pure Rec.2020 green is outside P3 (negative red); the 3.0 neutral in the
    // next pixel must not be pulled down by the mapping.
    let mut pixels = [neutral(1.0); 6];
    pixels[0] = [0.0, 1.0, 0.0];
    pixels[1] = neutral(3.0);
    let image = rec2020_image(pixels);
    for policy in [OutputPolicy::ClipToSrgb, OutputPolicy::ChromaCompressToSrgb] {
        let (extended, diagnostics) = render_extended_linear_with_geometry(
            &image,
            geometry(Orientation::Normal),
            RgbPrimaries::DisplayP3,
            4.0,
            policy,
            &CancellationToken::new(),
        )
        .expect("extended render");
        assert!(
            extended
                .data()
                .iter()
                .all(|value| (0.0..=4.0).contains(value)),
            "{policy:?}"
        );
        assert!((extended.pixel(1, 0).expect("pixel")[1] - 3.0).abs() < 1.0e-4);
        assert_eq!(diagnostics.in_gamut_pixels, 5, "{policy:?}");
    }
}

#[test]
fn extended_output_rejects_bad_input() {
    let image = rec2020_image([neutral(0.5); 6]);
    let render = |image: &LinearRgbImage<f32>, headroom, token: &CancellationToken| {
        render_extended_linear_with_geometry(
            image,
            geometry(Orientation::Normal),
            RgbPrimaries::DisplayP3,
            headroom,
            OutputPolicy::ClipToSrgb,
            token,
        )
        .map(|_| ())
    };
    for headroom in [0.5, f32::NAN, f32::INFINITY] {
        assert!(matches!(
            render(&image, headroom, &CancellationToken::new()),
            Err(PipelineError::InvalidHeadroom { .. })
        ));
    }

    let camera = LinearRgbImage::new(3, 2, 9, LinearRgbSpace::CameraNative, vec![0.5; 18])
        .expect("camera image");
    assert!(matches!(
        render(&camera, 2.0, &CancellationToken::new()),
        Err(PipelineError::WrongImageState { .. })
    ));

    let cancelled = CancellationToken::new();
    cancelled.cancel();
    assert!(matches!(
        render(&image, 2.0, &cancelled),
        Err(PipelineError::Cancelled)
    ));
}
