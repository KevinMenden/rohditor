//! Local adjustments (mask layers) through the whole CPU pipeline: preview, full-size render
//! and export agree on where a mask lands, and crop/orientation never move it.

use std::error::Error;
use std::sync::Arc;

use rohditor_core::{
    CpuPipeline, DitherMode, ExportImage, OutputBitDepth, OutputSize, PreviewOptions, RenderOptions,
};
use rohditor_edit::{
    EditRecipe, LinearGradient, LocalSettings, MaskComponent, MaskLayer, MaskMode, MaskPoint,
    MaskShape, NormalizedCropRect, RadialGradient,
};
use rohditor_image::{CfaColor, Orientation};
use rohditor_raw::{
    CameraColorMatrix, CameraMatrixOrigin, CaptureMetadata, CfaPattern, ImageRect, LevelPattern,
    PhotometricInterpretation, RawFileInfo, RawFrame,
};

const WIDTH: usize = 128;
const HEIGHT: usize = 96;
const LAYER_ID: u32 = 5;

/// A flat grey frame, so everything that varies across the output comes from the mask.
fn flat_frame() -> RawFrame {
    let black = [64.0_f32, 80.0, 96.0, 112.0];
    let white = [1064.0_f32, 1080.0, 1096.0, 1112.0];
    let samples: Vec<u16> = (0..WIDTH * HEIGHT)
        .map(|index| {
            let (x, y) = (index % WIDTH, index / WIDTH);
            let phase = (y & 1) * 2 + (x & 1);
            let value = match rohditor_image::BayerPattern::Rggb.color_at(x, y) {
                CfaColor::Red => 0.30,
                CfaColor::Green => 0.30,
                CfaColor::Blue => 0.30,
            };
            (black[phase] + value * (white[phase] - black[phase])).round() as u16
        })
        .collect();
    RawFrame {
        info: RawFileInfo {
            format: "synthetic".to_owned(),
            make: "Rohditor".to_owned(),
            model: "Flat fixture".to_owned(),
            clean_make: "Rohditor".to_owned(),
            clean_model: "Flat fixture".to_owned(),
            source_size_bytes: 0,
            source_identity: None,
            width: WIDTH,
            height: HEIGHT,
            components_per_pixel: 1,
            source_bits_per_sample: Some(16),
            decoded_bits_per_sample: 16,
            compression: None,
            active_area: Some(ImageRect {
                x: 0,
                y: 0,
                width: WIDTH,
                height: HEIGHT,
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
                values: black.to_vec(),
                repeat_width: 2,
                repeat_height: 2,
                components_per_pixel: 1,
            },
            white_levels: white.to_vec(),
            as_shot_white_balance: [Some(1.0), Some(1.0), Some(1.0), None],
            xyz_to_camera: [[0.0; 3]; 4],
            color_matrices: vec![CameraColorMatrix {
                illuminant: "D65".to_owned(),
                values: vec![1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0],
                origin: CameraMatrixOrigin::DecoderDatabase,
            }],
            orientation: Orientation::Normal,
            capture: CaptureMetadata::default(),
            embedded_preview: None,
        },
        row_stride: WIDTH,
        mosaic: Arc::from(samples),
    }
}

/// Darkens the left part of the frame through a soft vertical edge around x = 0.5.
fn masked_recipe() -> EditRecipe {
    let mut recipe = EditRecipe::default();
    recipe.local.layers.push(MaskLayer {
        id: LAYER_ID,
        name: "Left".to_owned(),
        enabled: true,
        opacity: 1.0,
        components: vec![MaskComponent {
            mode: MaskMode::Add,
            invert: false,
            shape: MaskShape::Linear(LinearGradient {
                start: MaskPoint { x: 0.4, y: 0.5 },
                end: MaskPoint { x: 0.6, y: 0.5 },
            }),
        }],
        settings: LocalSettings {
            exposure_ev: -2.0,
            ..LocalSettings::default()
        },
    });
    recipe
}

fn green(image: &rohditor_image::DisplayRgbImage<u8>, x: usize, y: usize) -> u8 {
    image.data()[y * image.row_stride() + x * 3 + 1]
}

/// Normalized x of the edge: where a left-dark, right-bright row crosses halfway.
fn edge_position(image: &rohditor_image::DisplayRgbImage<u8>) -> f32 {
    let y = image.height() / 2;
    let row: Vec<f32> = (0..image.width())
        .map(|x| f32::from(green(image, x, y)))
        .collect();
    let (low, high) = (row[0], row[row.len() - 1]);
    assert!(
        high > low + 20.0,
        "the mask must be visible: {low} vs {high}"
    );
    let middle = (low + high) / 2.0;
    let index = row.iter().position(|value| *value >= middle).expect("edge");
    let fraction = (middle - row[index - 1]) / (row[index] - row[index - 1]);
    (index as f32 - 1.0 + fraction + 0.5) / image.width() as f32
}

#[test]
fn fit_preview_and_full_resolution_export_put_the_mask_edge_at_the_same_place()
-> Result<(), Box<dyn Error>> {
    let (frame, recipe) = (flat_frame(), masked_recipe());
    let pipeline = CpuPipeline::default();
    let preview = pipeline.render_preview(
        &frame,
        &recipe,
        PreviewOptions {
            render: RenderOptions::default(),
            max_long_edge: 32,
        },
    )?;
    let export = pipeline.render_export(
        &frame,
        &recipe,
        RenderOptions::default(),
        OutputBitDepth::Eight,
        DitherMode::None,
        OutputSize::Original,
        None,
    )?;
    let ExportImage::Rgb8(export_image) = &export.image else {
        panic!("8-bit export expected");
    };
    assert_eq!((preview.image.width(), preview.image.height()), (32, 24));
    assert_eq!(
        (export_image.width(), export_image.height()),
        (WIDTH, HEIGHT)
    );

    let (preview_edge, export_edge) = (edge_position(&preview.image), edge_position(export_image));
    // The smoothstep midpoint sits at x = 0.5; both resolutions must agree within one preview pixel.
    assert!(
        (export_edge - 0.5).abs() < 1.0 / WIDTH as f32 * 2.0,
        "{export_edge}"
    );
    assert!(
        (preview_edge - export_edge).abs() <= 1.0 / preview.image.width() as f32,
        "preview {preview_edge} vs export {export_edge}"
    );
    Ok(())
}

#[test]
fn crop_and_orientation_do_not_move_the_mask_relative_to_the_content() -> Result<(), Box<dyn Error>>
{
    let frame = flat_frame();
    let pipeline = CpuPipeline::default();
    let full = pipeline.render(&frame, &masked_recipe(), RenderOptions::default())?;
    assert_eq!((full.image.width(), full.image.height()), (WIDTH, HEIGHT));

    let mut cropped_recipe = masked_recipe();
    cropped_recipe.geometry.crop = Some(NormalizedCropRect {
        left: 0.25,
        top: 0.25,
        right: 0.75,
        bottom: 0.75,
    });
    let cropped = pipeline.render(&frame, &cropped_recipe, RenderOptions::default())?;
    assert_eq!((cropped.image.width(), cropped.image.height()), (64, 48));
    for y in 0..cropped.image.height() {
        for x in 0..cropped.image.width() {
            assert_eq!(
                green(&cropped.image, x, y),
                green(&full.image, x + 32, y + 24),
                "{x},{y}"
            );
        }
    }

    let mut rotated_recipe = masked_recipe();
    rotated_recipe.geometry.orientation_override = Some(Orientation::Rotate180);
    let rotated = pipeline.render(&frame, &rotated_recipe, RenderOptions::default())?;
    for y in 0..HEIGHT {
        for x in 0..WIDTH {
            assert_eq!(
                green(&rotated.image, x, y),
                green(&full.image, WIDTH - 1 - x, HEIGHT - 1 - y),
                "{x},{y}"
            );
        }
    }
    Ok(())
}

#[test]
fn working_set_accounts_for_prepared_masks_only_when_they_are_active() -> Result<(), Box<dyn Error>>
{
    let frame = flat_frame();
    let pipeline = CpuPipeline::default();
    let plain = pipeline.render(&frame, &EditRecipe::default(), RenderOptions::default())?;
    assert_eq!(plain.memory.local_adjustment_bytes, 0);

    let mut recipe = masked_recipe();
    recipe.local.layers.push(MaskLayer {
        id: 9,
        name: "Spot".to_owned(),
        enabled: true,
        opacity: 0.5,
        components: vec![MaskComponent {
            mode: MaskMode::Add,
            invert: false,
            shape: MaskShape::Radial(RadialGradient {
                center: MaskPoint { x: 0.5, y: 0.5 },
                radius_x: 0.2,
                radius_y: 0.1,
                angle_degrees: 30.0,
                feather: 0.5,
            }),
        }],
        settings: LocalSettings {
            shadows: 0.5,
            ..LocalSettings::default()
        },
    });
    let masked = pipeline.render(&frame, &recipe, RenderOptions::default())?;
    assert!(masked.memory.local_adjustment_bytes > 0);
    assert!(masked.memory.estimated_peak_bytes >= plain.memory.estimated_peak_bytes);
    Ok(())
}
