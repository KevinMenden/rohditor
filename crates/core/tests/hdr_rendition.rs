//! The extended-range (HDR) rendition of a recipe against its SDR rendition.
//!
//! Shadows and midtones must come out bit-identical, highlights must rise toward the headroom
//! without passing it, and nothing in between may reorder tones or move a mask.

use std::error::Error;
use std::sync::Arc;

use rohditor_color::REC2020_LUMINANCE;
use rohditor_core::{
    CancellationToken, CpuPipeline, DitherMode, ExportImage, HDR_SHOULDER_KNEE, OutputBitDepth,
    OutputGeometry, OutputPolicy, OutputSize, PipelineError, RenderOptions, apply_adjustments,
    apply_adjustments_extended, base_rendering_lut, render_extended_linear_with_geometry,
    srgb_to_linear_srgb,
};
use rohditor_edit::{
    EditRecipe, LinearGradient, LocalSettings, LuminanceRange, MaskComponent, MaskLayer, MaskMode,
    MaskPoint, MaskShape, RenderingProfileSelection, ToneCurve,
};
use rohditor_image::{LinearRgbImage, LinearRgbSpace, Orientation, RgbPrimaries};
use rohditor_raw::{
    CameraColorMatrix, CameraMatrixOrigin, CaptureMetadata, CfaPattern, ImageRect, LevelPattern,
    PhotometricInterpretation, RawFileInfo, RawFrame,
};

const HEADROOM: f32 = 8.0;

fn luminance(pixel: &[f32; 3]) -> f32 {
    pixel
        .iter()
        .zip(REC2020_LUMINANCE)
        .map(|(value, weight)| value * weight)
        .sum()
}

fn row_image(pixels: &[[f32; 3]]) -> LinearRgbImage<f32> {
    LinearRgbImage::new(
        pixels.len(),
        1,
        pixels.len() * 3,
        LinearRgbSpace::Rec2020D65,
        pixels.iter().flatten().copied().collect(),
    )
    .expect("one-row image")
}

/// Neutral and coloured pixels from deep shadow to far above diffuse white. The colours are
/// asymmetric so a swapped channel cannot pass.
fn scene_pixels() -> Vec<[f32; 3]> {
    (0..=60)
        .flat_map(|index| {
            let level = 0.18 * (index as f32 / 4.0 - 8.0).exp2();
            [
                [level; 3],
                [level * 1.3, level * 0.9, level * 0.6],
                [level * 0.5, level * 0.8, level * 1.4],
            ]
        })
        .collect()
}

fn profiles() -> [RenderingProfileSelection; 3] {
    [
        RenderingProfileSelection::STANDARD,
        RenderingProfileSelection::MATCHED,
        RenderingProfileSelection::NEUTRAL,
    ]
}

/// Every per-pixel stage after the base rendering is active.
fn edited_recipe(profile: RenderingProfileSelection) -> EditRecipe {
    let mut recipe = EditRecipe::default();
    recipe.rendering.profile = profile;
    recipe.light.exposure_ev = 0.5;
    recipe.light.contrast = 0.2;
    recipe.light.shadows = 0.3;
    recipe.light.highlights = -0.2;
    recipe.light.whites = -0.6;
    recipe.light.blacks = 0.1;
    recipe.light.tone_curve = ToneCurve::through(&[[0.6, 0.64], [0.8, 0.83]]);
    recipe.color.saturation = 1.15;
    recipe.color.vibrance = 0.2;
    recipe.color.hsl.channels[0].saturation = 0.3;
    recipe.color.hsl.channels[5].luminance = -0.2;
    recipe.color.grading.midtones = [0.2, -0.1, 0.1];
    recipe.color.grading.highlights = [-0.1, 0.1, 0.3];
    recipe
}

/// SDR and extended renditions of the same pixels.
fn both_renditions(pixels: &[[f32; 3]], recipe: &EditRecipe) -> (Vec<f32>, Vec<f32>) {
    let mut sdr = row_image(pixels);
    apply_adjustments(&mut sdr, recipe).expect("SDR rendition");
    let mut hdr = row_image(pixels);
    apply_adjustments_extended(&mut hdr, recipe, HEADROOM).expect("HDR rendition");
    (sdr.data().to_vec(), hdr.data().to_vec())
}

#[test]
fn shadows_and_midtones_are_identical_to_the_sdr_rendition() {
    let pixels = scene_pixels();
    for profile in profiles() {
        let recipe = edited_recipe(profile);
        let gain = recipe.light.exposure_ev.exp2();
        // The base rendering is where the renditions part; Neutral is the identity there.
        let base = base_rendering_lut(&recipe.rendering);
        let base_luminance =
            |scene: f32| base.as_ref().map_or(scene, |lut| lut.sample(scene * gain));
        let (sdr, hdr) = both_renditions(&pixels, &recipe);
        let mut compared = 0;
        let mut brighter = 0;
        for ((pixel, sdr), hdr) in pixels
            .iter()
            .zip(sdr.as_chunks::<3>().0.iter())
            .zip(hdr.as_chunks::<3>().0.iter())
        {
            let rendered = base_luminance(luminance(pixel));
            if rendered < HDR_SHOULDER_KNEE - 0.01 {
                assert_eq!(sdr, hdr, "{profile:?}: {pixel:?} differs below the knee");
                compared += 1;
            } else if luminance(hdr) > luminance(sdr) * 1.01 {
                brighter += 1;
            }
        }
        assert!(
            compared > 50,
            "{profile:?}: only {compared} pixels below the knee"
        );
        // Neutral has no shoulder: its two renditions part only at the output ceiling.
        if base.is_some() {
            assert!(
                brighter > 20,
                "{profile:?}: only {brighter} brighter highlights"
            );
        }
    }
}

#[test]
fn highlights_reach_but_never_exceed_the_headroom() {
    let pixels = scene_pixels();
    for profile in [
        RenderingProfileSelection::STANDARD,
        RenderingProfileSelection::MATCHED,
    ] {
        let mut recipe = EditRecipe::default();
        recipe.rendering.profile = profile;
        let mut extreme = pixels.clone();
        extreme.push([1.0e5; 3]);
        let mut adjusted = row_image(&extreme);
        apply_adjustments_extended(&mut adjusted, &recipe, HEADROOM).expect("HDR rendition");
        let rendered: Vec<f32> = adjusted
            .data()
            .as_chunks::<3>()
            .0
            .iter()
            .map(luminance)
            .collect();
        let peak = rendered.last().copied().expect("extreme pixel");
        assert!(peak > 0.99 * HEADROOM, "{profile:?}: peak {peak}");

        let geometry =
            OutputGeometry::new(extreme.len(), 1, Orientation::Normal, None).expect("geometry");
        let (output, _) = render_extended_linear_with_geometry(
            &adjusted,
            geometry,
            RgbPrimaries::DisplayP3,
            HEADROOM,
            OutputPolicy::ChromaCompressToSrgb,
            &CancellationToken::new(),
        )
        .expect("extended output");
        assert!(output.data().iter().all(|&value| value <= HEADROOM));
        assert!(output.data().iter().copied().fold(0.0, f32::max) > 0.99 * HEADROOM);
    }
}

#[test]
fn lowered_whites_keep_tone_order_across_diffuse_white() {
    let sweep: Vec<[f32; 3]> = (0..=400)
        .map(|index| [0.01 * (index as f32 / 50.0).exp2(); 3])
        .collect();
    for profile in profiles() {
        let mut recipe = EditRecipe::default();
        recipe.rendering.profile = profile;
        recipe.light.whites = -1.0;
        let mut image = row_image(&sweep);
        apply_adjustments_extended(&mut image, &recipe, HEADROOM).expect("HDR rendition");
        let rendered: Vec<f32> = image
            .data()
            .as_chunks::<3>()
            .0
            .iter()
            .map(luminance)
            .collect();
        assert!(rendered.iter().any(|&value| value > 1.5), "{profile:?}");
        for (index, pair) in rendered.windows(2).enumerate() {
            assert!(
                pair[1] >= pair[0] - 1.0e-6,
                "{profile:?}: {:?} renders darker than {:?}",
                sweep[index + 1],
                sweep[index]
            );
        }
    }
}

#[test]
fn range_masks_select_the_same_pixels_in_both_renditions() {
    // Bright pixels sit in the feather of a luminance range measured on the SDR reference.
    // The local exposure then pulls them below the knee, where the renditions must agree; a
    // mask measured on the brighter HDR reference would select them more fully.
    let mut recipe = EditRecipe::default();
    recipe.local.layers.push(MaskLayer {
        id: 1,
        name: "Highlights".to_owned(),
        enabled: true,
        opacity: 1.0,
        components: vec![
            MaskComponent {
                mode: MaskMode::Add,
                invert: false,
                shape: MaskShape::Linear(LinearGradient {
                    // Selects everything left of the edge, i.e. the whole frame.
                    start: MaskPoint { x: 1.5, y: 0.5 },
                    end: MaskPoint { x: 2.0, y: 0.5 },
                }),
            },
            MaskComponent {
                mode: MaskMode::Intersect,
                invert: false,
                shape: MaskShape::Luminance(LuminanceRange {
                    low: 0.95,
                    high: 1.0,
                    feather_low: 0.3,
                    feather_high: 0.0,
                }),
            },
        ],
        settings: LocalSettings {
            exposure_ev: -2.0,
            ..LocalSettings::default()
        },
    });
    let pixels = [
        [0.9, 0.9, 0.9],
        [1.0, 0.95, 0.9],
        [0.85, 1.0, 1.1],
        [0.05, 0.05, 0.05],
    ];
    let (sdr, hdr) = both_renditions(&pixels, &recipe);
    let mut unmasked = row_image(&pixels);
    apply_adjustments(&mut unmasked, &EditRecipe::default()).expect("unmasked");
    for (index, (sdr, hdr)) in sdr
        .as_chunks::<3>()
        .0
        .iter()
        .zip(hdr.as_chunks::<3>().0.iter())
        .enumerate()
    {
        assert!(luminance(sdr) < HDR_SHOULDER_KNEE - 0.05, "pixel {index}");
        assert_eq!(sdr, hdr, "pixel {index}");
    }
    // The mask did act on the bright pixels and not on the dark one.
    assert!(
        luminance(&sdr.as_chunks::<3>().0[0])
            < 0.5 * luminance(&unmasked.data().as_chunks::<3>().0[0])
    );
    assert_eq!(&sdr[9..], &unmasked.data()[9..]);
}

#[test]
fn unusable_headroom_is_rejected() {
    for headroom in [0.99, f32::NAN, f32::INFINITY] {
        let mut image = row_image(&[[0.5; 3]]);
        assert!(matches!(
            apply_adjustments_extended(&mut image, &EditRecipe::default(), headroom),
            Err(PipelineError::InvalidHeadroom { .. })
        ));
        assert!(matches!(
            CpuPipeline::default().render_extended_export(
                &ramp_frame(),
                &EditRecipe::default(),
                RenderOptions::default(),
                RgbPrimaries::DisplayP3,
                headroom,
                OutputSize::Original,
            ),
            Err(PipelineError::InvalidHeadroom { .. })
        ));
    }
}

const FRAME_WIDTH: usize = 48;
const FRAME_HEIGHT: usize = 32;

/// A horizontal ramp from near black to the white level, with a slight vertical tilt so the
/// frame is not symmetric under a flip.
fn ramp_frame() -> RawFrame {
    let black = 64.0_f32;
    let white = 4_095.0_f32;
    let samples: Vec<u16> = (0..FRAME_WIDTH * FRAME_HEIGHT)
        .map(|index| {
            let (x, y) = (index % FRAME_WIDTH, index / FRAME_WIDTH);
            let level = 0.01
                + 0.97 * (x as f32 / (FRAME_WIDTH - 1) as f32).powi(2)
                + 0.01 * y as f32 / FRAME_HEIGHT as f32;
            (black + level * (white - black)).round() as u16
        })
        .collect();
    RawFrame {
        info: RawFileInfo {
            format: "synthetic".to_owned(),
            make: "Rohditor".to_owned(),
            model: "Ramp fixture".to_owned(),
            clean_make: "Rohditor".to_owned(),
            clean_model: "Ramp fixture".to_owned(),
            source_size_bytes: 0,
            source_identity: None,
            width: FRAME_WIDTH,
            height: FRAME_HEIGHT,
            components_per_pixel: 1,
            source_bits_per_sample: Some(12),
            decoded_bits_per_sample: 16,
            compression: None,
            active_area: Some(ImageRect {
                x: 0,
                y: 0,
                width: FRAME_WIDTH,
                height: FRAME_HEIGHT,
            }),
            crop_area: None,
            photometric_interpretation: PhotometricInterpretation::Cfa {
                pattern: CfaPattern {
                    name: "RGGB".to_owned(),
                    width: 2,
                    height: 2,
                },
            },
            black_levels: LevelPattern {
                values: vec![black; 4],
                repeat_width: 2,
                repeat_height: 2,
                components_per_pixel: 1,
            },
            white_levels: vec![white],
            as_shot_white_balance: [Some(1.0), Some(1.0), Some(1.0), None],
            xyz_to_camera: [[0.0; 3]; 4],
            color_matrices: vec![CameraColorMatrix {
                illuminant: "D65".to_owned(),
                values: vec![1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0],
                origin: CameraMatrixOrigin::DecoderDatabase,
            }],
            orientation: Orientation::Rotate90,
            capture: CaptureMetadata::default(),
            embedded_preview: None,
        },
        row_stride: FRAME_WIDTH,
        mosaic: Arc::from(samples),
    }
}

#[test]
fn pipeline_rendition_matches_the_sdr_export_below_the_knee() -> Result<(), Box<dyn Error>> {
    // Rotate90 swaps the axes of the 48x32 frame; the resize then also runs on the rotated,
    // non-square image, so a geometry mix-up between the two paths cannot pass.
    for (size, dimensions) in [
        (OutputSize::Original, (FRAME_HEIGHT, FRAME_WIDTH)),
        (OutputSize::LongEdge { pixels: 30 }, (20, 30)),
    ] {
        compare_with_sdr_export(size, dimensions)?;
    }
    Ok(())
}

fn compare_with_sdr_export(
    size: OutputSize,
    (width, height): (usize, usize),
) -> Result<(), Box<dyn Error>> {
    let frame = ramp_frame();
    let mut recipe = EditRecipe::default();
    recipe.light.exposure_ev = 1.5;
    let options = RenderOptions::default();
    let pipeline = CpuPipeline::default();
    let sdr = pipeline.render_export(
        &frame,
        &recipe,
        options,
        OutputBitDepth::Sixteen,
        DitherMode::None,
        size,
        None,
    )?;
    let hdr = pipeline.render_extended_export(
        &frame,
        &recipe,
        options,
        RgbPrimaries::Srgb,
        HEADROOM,
        size,
    )?;
    let ExportImage::Rgb16(sdr) = sdr.image else {
        return Err("expected a 16-bit export".into());
    };
    let hdr = hdr.image;
    assert_eq!((hdr.width(), hdr.height()), (width, height), "{size:?}");
    assert_eq!((sdr.width(), sdr.height()), (width, height), "{size:?}");
    assert_eq!(hdr.primaries(), RgbPrimaries::Srgb);
    assert_eq!(hdr.headroom(), HEADROOM);
    // The sensor's horizontal ramp runs down the rotated output.
    let green = |x: usize, y: usize| hdr.data()[y * hdr.row_stride() + x * 3 + 1];
    assert!(
        green(0, 0) < green(0, height - 1),
        "{size:?}: ramp not rotated"
    );
    assert!((green(0, 0) - green(width - 1, 0)).abs() < 0.05, "{size:?}");

    let mut compared = 0;
    for (sdr, hdr) in sdr
        .data()
        .as_chunks::<3>()
        .0
        .iter()
        .zip(hdr.data().as_chunks::<3>().0.iter())
    {
        let sdr = sdr.map(|code| srgb_to_linear_srgb(f32::from(code) / 65_535.0));
        // Neutral pixels, so sRGB-primaries luminance is fine for the selection. The margin
        // keeps resampled pixels whose footprint reaches above the knee out of the comparison.
        if sdr.iter().all(|&value| value < HDR_SHOULDER_KNEE - 0.05) {
            for (sdr, hdr) in sdr.iter().zip(hdr) {
                assert!(
                    (sdr - hdr).abs() < 1.0e-3,
                    "{size:?}: SDR {sdr} vs HDR {hdr}"
                );
            }
            compared += 1;
        } else {
            for (sdr, hdr) in sdr.iter().zip(hdr) {
                assert!(
                    *hdr >= sdr - 1.0e-3,
                    "{size:?}: HDR {hdr} dimmer than SDR {sdr}"
                );
            }
        }
    }
    assert!(
        compared > width * height / 4,
        "{size:?}: only {compared} pixels compared"
    );
    let peak = hdr.data().iter().copied().fold(0.0, f32::max);
    assert!(peak > 2.0 && peak <= HEADROOM, "{size:?}: peak {peak}");
    Ok(())
}
