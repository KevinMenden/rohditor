//! Presence (texture, clarity, dehaze) through the whole CPU pipeline: the sliders reach preview
//! and export, and a neutral recipe is untouched.

use std::error::Error;
use std::sync::Arc;

use rohditor_core::{
    CpuPipeline, DitherMode, ExportImage, OutputBitDepth, OutputSize, PreviewOptions, RenderOptions,
};
use rohditor_edit::EditRecipe;
use rohditor_image::Orientation;
use rohditor_raw::{
    CameraColorMatrix, CameraMatrixOrigin, CaptureMetadata, CfaPattern, ImageRect, LevelPattern,
    PhotometricInterpretation, RawFileInfo, RawFrame,
};

const WIDTH: usize = 256;
const HEIGHT: usize = 96;

/// A frame of soft vertical stripes.
fn striped_frame() -> RawFrame {
    let black = [64.0_f32, 80.0, 96.0, 112.0];
    let white = [1064.0_f32, 1080.0, 1096.0, 1112.0];
    let samples: Vec<u16> = (0..WIDTH * HEIGHT)
        .map(|index| {
            let (x, y) = (index % WIDTH, index / WIDTH);
            let phase = (y & 1) * 2 + (x & 1);
            let _ = y;
            // Soft vertical stripes (period 24 px) in every colour: mid-scale structure.
            let value = 0.25 + 0.08 * (x as f32 * std::f32::consts::TAU / 24.0).sin();
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

fn export(frame: &RawFrame, recipe: &EditRecipe) -> Result<Vec<u8>, Box<dyn Error>> {
    let result = CpuPipeline::default().render_export(
        frame,
        recipe,
        RenderOptions::default(),
        OutputBitDepth::Eight,
        DitherMode::None,
        OutputSize::Original,
        None,
    )?;
    let ExportImage::Rgb8(image) = result.image else {
        panic!("8-bit export expected");
    };
    Ok(image.into_data())
}

/// Standard deviation of the green channel.
fn spread(rgb: &[u8]) -> f32 {
    let greens: Vec<f32> = rgb
        .iter()
        .skip(1)
        .step_by(3)
        .map(|&v| f32::from(v))
        .collect();
    let mean = greens.iter().sum::<f32>() / greens.len() as f32;
    (greens.iter().map(|v| (v - mean).powi(2)).sum::<f32>() / greens.len() as f32).sqrt()
}

#[test]
fn clarity_changes_the_export_and_a_neutral_recipe_does_not() -> Result<(), Box<dyn Error>> {
    let frame = striped_frame();
    let plain = export(&frame, &EditRecipe::default())?;

    let mut recipe = EditRecipe::default();
    recipe.presence.clarity = 1.0;
    let clearer = export(&frame, &recipe)?;
    assert!(spread(&clearer) > spread(&plain) * 1.05);

    recipe.presence.clarity = -1.0;
    let softer = export(&frame, &recipe)?;
    assert!(spread(&softer) < spread(&plain) * 0.95);

    // Setting a slider and returning it to zero leaves nothing behind.
    recipe.presence.clarity = 0.0;
    assert_eq!(export(&frame, &recipe)?, plain);
    Ok(())
}

#[test]
fn preview_applies_presence_too() -> Result<(), Box<dyn Error>> {
    let frame = striped_frame();
    let pipeline = CpuPipeline::default();
    let options = PreviewOptions {
        render: RenderOptions::default(),
        max_long_edge: 128,
    };
    let plain = pipeline.render_preview(&frame, &EditRecipe::default(), options)?;
    let mut recipe = EditRecipe::default();
    recipe.presence.dehaze = 0.8;
    let dehazed = pipeline.render_preview(&frame, &recipe, options)?;
    assert_ne!(plain.image.data(), dehazed.image.data());
    Ok(())
}
