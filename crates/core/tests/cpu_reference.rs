use std::error::Error;
use std::sync::Arc;

use rayon::ThreadPoolBuilder;
use rohditor_core::{
    CpuPipeline, CropPolicy, DitherMode, EditRecipe, ExportImage, OutputBitDepth, RenderOptions,
    WhiteBalance, camera_color_transform,
};
use rohditor_raw::{
    CameraColorMatrix, CaptureMetadata, CfaPattern, ImageRect, LevelPattern,
    PhotometricInterpretation, RawFileInfo, RawFrame, RawOrientation,
};

#[test]
fn synthetic_pipeline_is_identical_with_one_and_multiple_rayon_threads()
-> Result<(), Box<dyn Error>> {
    let frame = synthetic_rggb_frame();
    let recipe = EditRecipe {
        white_balance: WhiteBalance::ManualMultipliers {
            red: 1.1,
            green: 1.0,
            blue: 0.9,
        },
        exposure_ev: 0.5,
        contrast: 0.25,
        saturation: 1.2,
        ..EditRecipe::default()
    };
    let options = RenderOptions {
        crop_policy: CropPolicy::Recommended,
        ..RenderOptions::default()
    };
    let single_pool = ThreadPoolBuilder::new().num_threads(1).build()?;
    let multi_pool = ThreadPoolBuilder::new().num_threads(4).build()?;
    let single = single_pool.install(|| CpuPipeline.render(&frame, &recipe, options))?;
    let multiple = multi_pool.install(|| CpuPipeline.render(&frame, &recipe, options))?;

    assert_eq!((single.image.width(), single.image.height()), (4, 6));
    assert_eq!(single.image.data(), multiple.image.data());
    assert!(single.image.data().iter().any(|value| *value > 0));
    assert_eq!(single.memory.decoded_raw_bytes, 96);
    assert_eq!(single.memory.normalized_mosaic_bytes, 96);
    assert_eq!(single.memory.linear_rgb_bytes, 288);
    assert_eq!(single.memory.display_rgb_bytes, 72);
    assert_eq!(single.memory.estimated_peak_bytes, 480);
    Ok(())
}

#[test]
fn sixteen_bit_dithered_export_is_identical_across_rayon_thread_counts()
-> Result<(), Box<dyn Error>> {
    let frame = synthetic_rggb_frame();
    let recipe = EditRecipe::default();
    let options = RenderOptions::default();
    let single_pool = ThreadPoolBuilder::new().num_threads(1).build()?;
    let multi_pool = ThreadPoolBuilder::new().num_threads(4).build()?;
    let single = single_pool.install(|| {
        CpuPipeline.render_export(
            &frame,
            &recipe,
            options,
            OutputBitDepth::Sixteen,
            DitherMode::Ordered8x8,
        )
    })?;
    let multiple = multi_pool.install(|| {
        CpuPipeline.render_export(
            &frame,
            &recipe,
            options,
            OutputBitDepth::Sixteen,
            DitherMode::Ordered8x8,
        )
    })?;
    let (ExportImage::Rgb16(single_image), ExportImage::Rgb16(multiple_image)) =
        (single.image, multiple.image)
    else {
        return Err("16-bit render returned the wrong buffer type".into());
    };

    assert_eq!(single_image.data(), multiple_image.data());
    assert_eq!(single.memory.display_rgb_bytes, 144);
    assert_eq!(single.memory.estimated_peak_bytes, 528);
    Ok(())
}

#[test]
fn missing_camera_calibration_is_an_actionable_error() {
    let mut frame = synthetic_rggb_frame();
    frame.info.color_matrices.clear();
    let error = CpuPipeline
        .render(&frame, &EditRecipe::default(), RenderOptions::default())
        .expect_err("missing calibration must fail");
    assert!(error.to_string().contains("color_matrices"));
}

#[test]
fn camera_matrix_direction_maps_a_balanced_neutral_to_rec2020_white() {
    let frame = synthetic_rggb_frame();
    let transform = camera_color_transform(&frame.info).expect("valid synthetic matrix");
    let neutral = transform
        .camera_to_linear_rec2020
        .transform([1.0, 1.0, 1.0]);
    for channel in neutral {
        assert!((channel - 1.0).abs() < 2.0e-5, "{neutral:?}");
    }
}

fn synthetic_rggb_frame() -> RawFrame {
    let width = 8;
    let height = 6;
    let black = [64.0_f32, 80.0, 96.0, 112.0];
    let white = [1064.0_f32, 1080.0, 1096.0, 1112.0];
    let pattern = rohditor_core::BayerPattern::Rggb;
    let mut samples = Vec::with_capacity(width * height);
    for y in 0..height {
        for x in 0..width {
            let phase = (y & 1) * 2 + (x & 1);
            let gradient = (x + y) as f32 / (width + height) as f32 * 0.2;
            let normalized = match pattern.color_at(x, y) {
                rohditor_core::CfaColor::Red => 0.2 + gradient,
                rohditor_core::CfaColor::Green => 0.35 + gradient,
                rohditor_core::CfaColor::Blue => 0.5 + gradient,
            };
            samples
                .push((black[phase] + normalized * (white[phase] - black[phase])).round() as u16);
        }
    }

    RawFrame {
        info: RawFileInfo {
            format: "synthetic".to_owned(),
            make: "Rohditor".to_owned(),
            model: "RGGB fixture".to_owned(),
            clean_make: "Rohditor".to_owned(),
            clean_model: "RGGB fixture".to_owned(),
            source_size_bytes: 0,
            width,
            height,
            components_per_pixel: 1,
            source_bits_per_sample: Some(16),
            decoded_bits_per_sample: 16,
            compression: None,
            active_area: Some(ImageRect {
                x: 0,
                y: 0,
                width,
                height,
            }),
            crop_area: Some(ImageRect {
                x: 2,
                y: 1,
                width: 6,
                height: 4,
            }),
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
            as_shot_white_balance: [Some(2.0), Some(1.0), Some(1.5), None],
            xyz_to_camera: [[0.0; 3]; 4],
            color_matrices: vec![CameraColorMatrix {
                illuminant: "D65".to_owned(),
                values: vec![1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0],
            }],
            orientation: RawOrientation::Rotate270,
            capture: CaptureMetadata::default(),
            embedded_preview: None,
        },
        row_stride: width,
        mosaic: Arc::from(samples),
    }
}
