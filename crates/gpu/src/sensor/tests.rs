use std::sync::Arc;

use rohditor_core::{
    CancellationToken, HighlightDiagnostics, HighlightExecution, NormalizationContract,
    RawCropPolicy, RenderOptions, SensorDevelopmentDescription, normalize_raw,
};
use rohditor_edit::{EditRecipe, HighlightMethod};
use rohditor_image::Orientation;
use rohditor_raw::{CfaPattern, ImageRect, LevelPattern, PhotometricInterpretation, RawFrame};

use super::GpuSensorProcessor;
use super::highlight::readback_for_qualification as readback_highlight_for_qualification;
use super::normalize::readback_for_qualification;

pub(super) fn processor() -> GpuSensorProcessor {
    let instance = wgpu::Instance::new(&wgpu::InstanceDescriptor {
        backends: wgpu::Backends::VULKAN,
        ..Default::default()
    });
    let adapter =
        pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions::default()))
            .expect("Vulkan adapter");
    eprintln!("Sensor normalization adapter: {:?}", adapter.get_info());
    let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
        required_limits: wgpu::Limits::default().using_resolution(adapter.limits()),
        ..Default::default()
    }))
    .expect("sensor normalization device");
    GpuSensorProcessor::new(&adapter, &device, &queue).expect("sensor normalization processor")
}

pub(super) fn frame(pattern: &str, white_levels: Vec<f32>) -> RawFrame {
    let width = 7;
    let height = 5;
    let row_stride = 10;
    let mut mosaic = vec![u16::MAX; row_stride * height];
    for y in 0..height {
        for x in 0..width {
            mosaic[y * row_stride + x] = [0, 64, 512, u16::MAX][(x + 2 * y) % 4];
        }
    }
    let mut frame = crate::preview::processor::tests::synthetic_frame(Orientation::Normal);
    frame.info.width = width;
    frame.info.height = height;
    frame.info.active_area = Some(ImageRect {
        x: 0,
        y: 0,
        width,
        height,
    });
    frame.info.crop_area = Some(ImageRect {
        x: 1,
        y: 1,
        width: 5,
        height: 4,
    });
    frame.info.photometric_interpretation = PhotometricInterpretation::Cfa {
        pattern: CfaPattern {
            name: pattern.to_owned(),
            width: 2,
            height: 2,
        },
    };
    frame.info.black_levels = LevelPattern {
        values: vec![64.0, 100.0, 400.0, 300.0],
        repeat_width: 2,
        repeat_height: 2,
        components_per_pixel: 1,
    };
    frame.info.white_levels = white_levels;
    frame.row_stride = row_stride;
    frame.mosaic = Arc::from(mosaic);
    frame
}

#[test]
#[ignore = "requires Vulkan; software validates structure and numerical parity only"]
fn u16_tiles_match_cpu_normalization_for_all_cfa_and_white_level_forms() {
    let _guard = crate::preview::processor::tests::gpu_test_guard();
    let mut gpu = processor();
    // A 5x4 crop becomes multiple bounded tiles, exercising x/y boundaries
    // and the crop-shifted CFA phase rather than relying on one full dispatch.
    gpu.force_maximum_tile_edge(2);
    let cancellation = CancellationToken::new();
    for pattern in ["RGGB", "BGGR", "GRBG", "GBRG"] {
        for white_levels in [
            vec![1200.0],
            vec![1200.0, 2200.0, 3200.0],
            vec![1200.0, 1300.0, 1400.0, 1500.0],
        ] {
            let frame = frame(pattern, white_levels);
            let contract = NormalizationContract::from_frame(&frame, RawCropPolicy::Recommended)
                .expect("fixture contract");
            let reference = normalize_raw(&frame, RawCropPolicy::Recommended)
                .expect("CPU reference normalization");
            let (mosaic, metrics) = gpu
                .normalize(&frame, &contract, &cancellation)
                .expect("GPU normalization");
            assert_eq!(mosaic.dimensions(), (5, 4));
            assert!(mosaic.matches_contract(&contract));
            assert!(metrics.tiles > 1);
            assert_eq!(metrics.submissions as usize, metrics.tiles);
            assert_eq!(metrics.uploaded_bytes, 5 * 4 * 2);
            assert_eq!(metrics.resident_gpu_bytes, mosaic.estimated_bytes());
            assert!(metrics.estimated_gpu_bytes > mosaic.estimated_bytes());

            // This explicit test-only readback is the sole full-image
            // transfer in this slice's qualification; normal execution
            // returns the resident state directly to the next GPU stage.
            let actual = readback_for_qualification(&gpu, &mosaic, &cancellation)
                .expect("GPU normalized readback");
            assert_eq!(actual.len(), reference.data().len());
            for (index, (actual, expected)) in actual.iter().zip(reference.data()).enumerate() {
                assert!(
                    (actual - expected).abs() <= 1.0e-6,
                    "{pattern}, sample {index}: GPU {actual}, CPU {expected}"
                );
            }
        }
    }
}

#[test]
#[ignore = "requires Vulkan; validates recovery before any resident state is published"]
fn cancellation_and_budget_failure_leave_the_immutable_raw_frame_usable() {
    let _guard = crate::preview::processor::tests::gpu_test_guard();
    let mut gpu = processor();
    gpu.force_maximum_tile_edge(2);
    let frame = frame("RGGB", vec![1200.0, 2200.0, 3200.0]);
    let contract = NormalizationContract::from_frame(&frame, RawCropPolicy::Recommended)
        .expect("fixture contract");
    let original = frame.mosaic.clone();

    let cancelled = CancellationToken::new();
    cancelled.cancel();
    assert!(matches!(
        gpu.normalize(&frame, &contract, &cancelled),
        Err(crate::GpuPreviewError::Cancelled)
    ));
    assert_eq!(frame.mosaic, original);

    gpu.set_budget(64);
    assert!(
        gpu.normalize(&frame, &contract, &CancellationToken::new())
            .is_err()
    );
    assert_eq!(frame.mosaic, original);
    normalize_raw(&frame, RawCropPolicy::Recommended).expect("CPU recovery reference");
}

#[test]
#[ignore = "requires Vulkan; software validates structure and numerical parity only"]
fn off_and_clip_match_cpu_highlight_with_bounded_diagnostics() {
    let _guard = crate::preview::processor::tests::gpu_test_guard();
    let mut gpu = processor();
    gpu.force_maximum_tile_edge(2);
    let cancellation = CancellationToken::new();
    let options = RenderOptions {
        raw_crop_policy: RawCropPolicy::Recommended,
        ..RenderOptions::default()
    };

    for pattern in ["RGGB", "BGGR", "GRBG", "GBRG"] {
        let frame = frame(pattern, vec![1200.0, 2200.0, 3200.0]);
        let mut recipe = EditRecipe::default();
        recipe.raw.highlights.method = HighlightMethod::Clip;
        let description = SensorDevelopmentDescription::from_frame(&frame, &recipe, options)
            .expect("Clip sensor description");
        let reference =
            normalize_raw(&frame, RawCropPolicy::Recommended).expect("CPU reference normalization");
        let HighlightExecution::Clip(levels) = description.highlight_execution() else {
            panic!("Clip recipe must resolve Clip execution");
        };
        let expected = rohditor_highlight::clip(reference, levels).expect("CPU Clip reference");
        let (normalized, _) = gpu
            .normalize(&frame, description.normalization(), &cancellation)
            .expect("GPU normalization");
        let (highlighted, diagnostics, metrics) = gpu
            .apply_highlight(normalized, &description, &cancellation)
            .expect("GPU Clip highlight");

        assert!(highlighted.matches_description(&description));
        assert_eq!(highlighted.dimensions(), (5, 4));
        assert_eq!(diagnostics, HighlightDiagnostics::Clip(expected.stats));
        assert!(metrics.tiles > 1);
        assert_eq!(metrics.submissions as usize, metrics.tiles + 1);
        assert_eq!(metrics.diagnostic_readback_bytes, 6 * 4);
        assert_eq!(metrics.resident_gpu_bytes, highlighted.estimated_bytes());
        assert!(metrics.estimated_gpu_bytes > highlighted.estimated_bytes());

        let actual = readback_highlight_for_qualification(&gpu, &highlighted, &cancellation)
            .expect("GPU Clip qualification readback");
        assert_eq!(actual.len(), expected.mosaic.data().len());
        for (index, (actual, expected)) in actual.iter().zip(expected.mosaic.data()).enumerate() {
            assert!(
                (actual - expected).abs() <= 1.0e-6,
                "{pattern}, sample {index}: GPU {actual}, CPU {expected}"
            );
        }
    }

    let frame = frame("RGGB", vec![1200.0]);
    let mut recipe = EditRecipe::default();
    recipe.raw.highlights.method = HighlightMethod::Off;
    let description = SensorDevelopmentDescription::from_frame(&frame, &recipe, options)
        .expect("Off sensor description");
    let (normalized, _) = gpu
        .normalize(&frame, description.normalization(), &cancellation)
        .expect("GPU normalization for Off");
    let (highlighted, diagnostics, metrics) = gpu
        .apply_highlight(normalized, &description, &cancellation)
        .expect("GPU Off highlight");
    assert!(highlighted.matches_description(&description));
    assert_eq!(diagnostics, HighlightDiagnostics::Off);
    assert_eq!(metrics.tiles, 0);
    assert_eq!(metrics.submissions, 0);
    assert_eq!(metrics.diagnostic_readback_bytes, 0);
    let expected =
        normalize_raw(&frame, RawCropPolicy::Recommended).expect("CPU Off normalization reference");
    let actual = readback_highlight_for_qualification(&gpu, &highlighted, &cancellation)
        .expect("GPU Off qualification readback");
    for (index, (actual, expected)) in actual.iter().zip(expected.data()).enumerate() {
        assert!(
            (actual - expected).abs() <= 1.0e-6,
            "Off sample {index}: GPU {actual}, CPU {expected}"
        );
    }
}

#[test]
#[ignore = "requires Vulkan; validates Clip recovery before a highlighted state is published"]
fn clip_cancellation_and_budget_failure_preserve_cpu_recovery() {
    let _guard = crate::preview::processor::tests::gpu_test_guard();
    let mut gpu = processor();
    let frame = frame("RGGB", vec![1200.0, 2200.0, 3200.0]);
    let original = frame.mosaic.clone();
    let mut recipe = EditRecipe::default();
    recipe.raw.highlights.method = HighlightMethod::Clip;
    let description = SensorDevelopmentDescription::from_frame(
        &frame,
        &recipe,
        RenderOptions {
            raw_crop_policy: RawCropPolicy::Recommended,
            ..RenderOptions::default()
        },
    )
    .expect("Clip sensor description");

    let normalized = gpu
        .normalize(
            &frame,
            description.normalization(),
            &CancellationToken::new(),
        )
        .expect("GPU normalization before cancellation")
        .0;
    let cancelled = CancellationToken::new();
    cancelled.cancel();
    assert!(matches!(
        gpu.apply_highlight(normalized, &description, &cancelled),
        Err(crate::GpuPreviewError::Cancelled)
    ));
    assert_eq!(frame.mosaic, original);

    let normalized = gpu
        .normalize(
            &frame,
            description.normalization(),
            &CancellationToken::new(),
        )
        .expect("GPU normalization before budget rejection")
        .0;
    gpu.set_budget(64);
    assert!(matches!(
        gpu.apply_highlight(normalized, &description, &CancellationToken::new()),
        Err(crate::GpuPreviewError::Unsupported { .. })
    ));
    assert_eq!(frame.mosaic, original);
    normalize_raw(&frame, RawCropPolicy::Recommended).expect("CPU recovery reference");
}
