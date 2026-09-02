use std::error::Error;
use std::sync::Arc;
use std::time::Duration;

use rayon::ThreadPoolBuilder;
use rohditor_core::{
    CancellationToken, CpuPipeline, CropPolicy, DitherMode, ExportImage, OutputBitDepth,
    PipelineError, PreviewOptions, RenderOptions, camera_color_transform,
};
use rohditor_edit::{EditRecipe, WhiteBalance};
use rohditor_image::{BayerPattern, CfaColor, LinearRgbSpace, Orientation};
use rohditor_raw::{
    CameraColorMatrix, CaptureMetadata, CfaPattern, ImageRect, LevelPattern,
    PhotometricInterpretation, RawFileInfo, RawFrame,
};

#[test]
fn synthetic_pipeline_is_identical_with_one_and_multiple_rayon_threads()
-> Result<(), Box<dyn Error>> {
    let frame = synthetic_rggb_frame();
    let mut recipe = EditRecipe::default();
    recipe.color.white_balance = WhiteBalance::ManualMultipliers {
        red: 1.1,
        green: 1.0,
        blue: 0.9,
    };
    recipe.light.exposure_ev = 0.5;
    recipe.light.contrast = 0.25;
    recipe.color.saturation = 1.2;
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
    assert_eq!(single.memory.resample_intermediate_bytes, 0);
    assert_eq!(single.memory.linear_rgb_bytes, 288);
    assert_eq!(single.memory.display_rgb_bytes, 72);
    assert_eq!(single.memory.estimated_peak_bytes, 480);
    Ok(())
}

#[test]
fn preview_pipeline_reconstructs_full_crop_before_area_reduction() -> Result<(), Box<dyn Error>> {
    let frame = synthetic_rggb_frame();
    let result = CpuPipeline.render_preview(
        &frame,
        &EditRecipe::default(),
        PreviewOptions {
            render: RenderOptions::default(),
            max_long_edge: 3,
        },
    )?;

    // The recommended crop is 6x4 and the fixture is rotated 270 degrees.
    assert_eq!((result.image.width(), result.image.height()), (2, 3));
    assert_eq!(result.memory.normalized_mosaic_bytes, 96);
    assert_eq!(result.memory.resample_intermediate_bytes, 144);
    assert_eq!(result.memory.linear_rgb_bytes, 72);
    assert_eq!(result.memory.display_rgb_bytes, 18);
    assert_eq!(result.memory.decoded_raw_bytes, 96);
    assert_eq!(result.memory.estimated_peak_bytes, 528);
    Ok(())
}

#[test]
fn unbounded_preview_target_falls_back_to_the_full_crop_without_size_overflow()
-> Result<(), Box<dyn Error>> {
    let frame = synthetic_rggb_frame();
    let result = CpuPipeline.render_preview(
        &frame,
        &EditRecipe::default(),
        PreviewOptions {
            render: RenderOptions::default(),
            max_long_edge: usize::MAX,
        },
    )?;
    assert_eq!((result.image.width(), result.image.height()), (4, 6));
    Ok(())
}

#[test]
fn reusable_preview_base_matches_direct_render_and_rejects_stale_white_balance()
-> Result<(), Box<dyn Error>> {
    let frame = synthetic_rggb_frame();
    let options = PreviewOptions {
        render: RenderOptions::default(),
        max_long_edge: 3,
    };
    let base_recipe = EditRecipe::default();
    let base = CpuPipeline.prepare_preview_base(&frame, &base_recipe, options)?;
    assert_eq!(base.image().space(), LinearRgbSpace::Rec2020D65);
    assert_eq!((base.image().width(), base.image().height()), (3, 2));

    let mut adjusted = base_recipe.clone();
    adjusted.light.exposure_ev = 0.75;
    adjusted.light.contrast = 0.2;
    adjusted.color.saturation = 1.3;
    let reused =
        CpuPipeline.render_preview_from_base(&base, &adjusted, options.render.output_policy)?;
    let direct = CpuPipeline.render_preview(&frame, &adjusted, options)?;
    assert_eq!(reused.image, direct.image);
    assert_eq!(
        reused.memory.estimated_peak_bytes,
        direct.memory.estimated_peak_bytes
    );

    let mut stale = adjusted;
    stale.color.white_balance = WhiteBalance::ManualMultipliers {
        red: 1.1,
        green: 1.0,
        blue: 0.9,
    };
    assert!(
        CpuPipeline
            .render_preview_from_base(&base, &stale, options.render.output_policy)
            .is_err()
    );
    Ok(())
}

#[test]
fn split_reconstruction_and_color_stages_match_the_combined_preview_base()
-> Result<(), Box<dyn Error>> {
    let frame = synthetic_rggb_frame();
    let options = PreviewOptions {
        max_long_edge: 3,
        ..PreviewOptions::default()
    };
    let recipe = EditRecipe::default();
    let reconstructed = CpuPipeline.prepare_preview_reconstruction(&frame, options)?;
    let split = CpuPipeline.prepare_preview_base_from_reconstruction(&reconstructed, &recipe)?;
    let combined = CpuPipeline.prepare_preview_base(&frame, &recipe, options)?;

    assert_eq!(split.image(), combined.image());
    assert_eq!(split.timings().normalization, Duration::ZERO);
    assert_eq!(split.timings().demosaic, Duration::ZERO);
    assert_eq!(split.timings().resampling, Duration::ZERO);
    assert!(reconstructed.buffer_bytes() > 0);
    assert_eq!(reconstructed.image().space(), LinearRgbSpace::CameraNative);
    Ok(())
}

#[test]
fn preview_stages_return_the_typed_cancellation_error() {
    let frame = synthetic_rggb_frame();
    let cancellation = CancellationToken::new();
    cancellation.cancel();

    let error = CpuPipeline
        .prepare_preview_reconstruction_cancellable(
            &frame,
            PreviewOptions::default(),
            &cancellation,
        )
        .expect_err("cancelled work must stop before normalization");

    assert!(matches!(error, PipelineError::Cancelled));
}

#[test]
fn source_scale_preview_matches_full_output_dimensions_and_is_cancellable()
-> Result<(), Box<dyn Error>> {
    let frame = synthetic_rggb_frame();
    let recipe = EditRecipe::default();
    let rendered = CpuPipeline.render_source_scale_preview_cancellable(
        &frame,
        &recipe,
        RenderOptions::default(),
        &CancellationToken::new(),
    )?;
    assert_eq!((rendered.image.width(), rendered.image.height()), (4, 6));
    assert_eq!(rendered.memory.resample_intermediate_bytes, 0);

    let cancellation = CancellationToken::new();
    cancellation.cancel();
    let error = CpuPipeline
        .render_source_scale_preview_cancellable(
            &frame,
            &recipe,
            RenderOptions::default(),
            &cancellation,
        )
        .expect_err("cancelled source-scale work must stop");
    assert!(matches!(error, PipelineError::Cancelled));
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
fn unreasonable_cpu_working_set_is_rejected_before_image_allocation() {
    let mut frame = synthetic_rggb_frame();
    frame.info.width = 200_000;
    frame.info.height = 200_000;
    let error = CpuPipeline
        .render(&frame, &EditRecipe::default(), RenderOptions::default())
        .expect_err("unreasonable working set must fail");
    assert!(matches!(
        error,
        rohditor_core::PipelineError::WorkingSetLimit { .. }
    ));
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
    let pattern = BayerPattern::Rggb;
    let mut samples = Vec::with_capacity(width * height);
    for y in 0..height {
        for x in 0..width {
            let phase = (y & 1) * 2 + (x & 1);
            let gradient = (x + y) as f32 / (width + height) as f32 * 0.2;
            let normalized = match pattern.color_at(x, y) {
                CfaColor::Red => 0.2 + gradient,
                CfaColor::Green => 0.35 + gradient,
                CfaColor::Blue => 0.5 + gradient,
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
            source_identity: None,
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
            orientation: Orientation::Rotate270,
            capture: CaptureMetadata::default(),
            embedded_preview: None,
        },
        row_stride: width,
        mosaic: Arc::from(samples),
    }
}
