use std::sync::Arc;

use rohditor_core::{
    CancellationToken, RawCropPolicy, RenderOptions, SensorDevelopmentDescription, normalize_raw,
};
use rohditor_demosaic::{ChannelBalance, DemosaicAlgorithm, WhiteBalanceGains, demosaic};
use rohditor_edit::{CaptureSharpening, EditRecipe, HighlightMethod};
use rohditor_raw::{ImageRect, LevelPattern, RawFrame};

use super::tests::processor;
use super::{GpuSensorCameraSource, GpuSensorProcessor};

pub(super) fn frame(pattern: &str, width: usize, height: usize, kind: usize) -> RawFrame {
    let mut frame = super::tests::frame(pattern, vec![2048.0]);
    frame.info.width = width + 2;
    frame.info.height = height + 2;
    frame.info.active_area = None;
    frame.info.crop_area = Some(ImageRect {
        x: 1,
        y: 1,
        width,
        height,
    });
    frame.row_stride = width + 5;
    frame.info.black_levels = LevelPattern {
        values: vec![1024.0],
        repeat_width: 1,
        repeat_height: 1,
        components_per_pixel: 1,
    };
    let mut data = vec![0; frame.row_stride * frame.info.height];
    for y in 0..height {
        for x in 0..width {
            data[(y + 1) * frame.row_stride + x + 1] = match kind {
                0 => 1536,
                1 => {
                    if (x, y) == (width / 2, height / 2) {
                        8192
                    } else {
                        1024
                    }
                }
                2 => (1024 + 11 * x + 19 * y) as u16,
                3 => {
                    if x < width / 2 {
                        1500
                    } else {
                        3000
                    }
                }
                4 => {
                    if (x + y) % 2 == 0 {
                        1024
                    } else {
                        2048
                    }
                }
                6 => (1100 + (x * 17 + y * 31 + x * y) % 700) as u16,
                _ => [0, 512, 1024, 1536, 2048, 16384, 65535][(x * 7 + y * 3 + x / 2) % 7],
            };
        }
    }
    frame.mosaic = Arc::from(data);
    frame
}

pub(super) fn description(
    frame: &RawFrame,
    algorithm: DemosaicAlgorithm,
    clip: bool,
) -> SensorDevelopmentDescription {
    let mut recipe = EditRecipe::default();
    recipe.raw.highlights.method = if clip {
        HighlightMethod::Clip
    } else {
        HighlightMethod::Off
    };
    SensorDevelopmentDescription::from_frame(
        frame,
        &recipe,
        RenderOptions {
            raw_crop_policy: RawCropPolicy::Recommended,
            demosaic: algorithm,
            ..Default::default()
        },
    )
    .expect("sensor description")
}

#[test]
fn rcd_and_amaze_support_is_limited_to_off_and_clip_highlights() {
    let frame = frame("RGGB", 29, 31, 0);
    assert!(GpuSensorProcessor::supports(&description(
        &frame,
        DemosaicAlgorithm::Rcd,
        false
    )));
    assert!(GpuSensorProcessor::supports(&description(
        &frame,
        DemosaicAlgorithm::Rcd,
        true
    )));
    assert!(GpuSensorProcessor::supports(&description(
        &frame,
        DemosaicAlgorithm::Amaze,
        false
    )));
    let mut recipe = EditRecipe::default();
    recipe.raw.highlights.method = HighlightMethod::LocalRatios;
    let unsupported = SensorDevelopmentDescription::from_frame(
        &frame,
        &recipe,
        RenderOptions {
            demosaic: DemosaicAlgorithm::Rcd,
            ..Default::default()
        },
    )
    .expect("RCD highlight description");
    assert!(!GpuSensorProcessor::supports(&unsupported));
    assert_eq!(DemosaicAlgorithm::Rcd.contract().algorithm_version(), 3);
}

pub(super) fn highlighted(
    gpu: &GpuSensorProcessor,
    frame: &RawFrame,
    description: &SensorDevelopmentDescription,
) -> super::GpuHighlightedMosaic {
    let token = CancellationToken::new();
    let normalized = gpu
        .normalize(frame, description.normalization(), &token)
        .expect("normalize")
        .0;
    gpu.apply_highlight(normalized, description, &token)
        .expect("highlight")
        .0
}

pub(super) fn readback(gpu: &GpuSensorProcessor, source: &GpuSensorCameraSource) -> Vec<f32> {
    readback_planes(gpu, &source.planes)
}

pub(super) fn readback_planes(
    gpu: &GpuSensorProcessor,
    planes: &crate::spatial::source::ResidentCameraPlanes,
) -> Vec<f32> {
    let layout = planes.layout;
    let mosaic_layout = super::resources::MosaicLayout {
        width: layout.width,
        height: layout.height,
        tile_width: layout.tile_width,
        tile_height: layout.tile_height,
        columns: layout.columns,
        rows: layout.rows,
        layers: layout.layers,
        padded_pixels: layout.padded_pixels,
        resident_bytes: layout.resident_bytes / 3,
        estimated_peak_bytes: layout.estimated_peak_bytes,
    };
    let mut rgb = vec![0.0; layout.width as usize * layout.height as usize * 3];
    for channel in 0..3 {
        let data = super::normalize::readback_texture_for_qualification(
            gpu,
            &planes.textures[channel],
            mosaic_layout,
            &CancellationToken::new(),
        )
        .expect("RGB qualification readback");
        for (pixel, value) in data.into_iter().enumerate() {
            rgb[pixel * 3 + channel] = value;
        }
    }
    rgb
}

#[test]
#[ignore = "requires Vulkan; CPU comparison for bilinear/MHC phases, borders and tile seams"]
fn bilinear_and_mhc_match_cpu_at_borders_and_across_mosaic_tiles() {
    let _guard = crate::preview::processor::tests::gpu_test_guard();
    let mut gpu = processor();
    gpu.force_maximum_tile_edge(3);
    let capture =
        crate::capture_only_contract(CaptureSharpening::default(), [1.0; 3]).expect("capture off");
    for algorithm in [
        DemosaicAlgorithm::Bilinear,
        DemosaicAlgorithm::MalvarHeCutler,
    ] {
        for pattern in ["RGGB", "BGGR", "GRBG", "GBRG"] {
            for (width, height) in [(2, 2), (2, 7), (3, 5), (13, 11)] {
                for kind in 0..6 {
                    let frame = frame(pattern, width, height, kind);
                    let description = description(&frame, algorithm, false);
                    let reference_mosaic =
                        normalize_raw(&frame, RawCropPolicy::Recommended).expect("CPU normalize");
                    let reference =
                        demosaic(&reference_mosaic, WhiteBalanceGains::identity(), algorithm)
                            .expect("CPU demosaic");
                    let input = highlighted(&gpu, &frame, &description);
                    let (source, metrics) = gpu
                        .demosaic(input, &description, &capture, &CancellationToken::new())
                        .expect("GPU demosaic");
                    assert_eq!(source.description(), &description);
                    assert_eq!(metrics.diagnostic_readback_bytes, 4);
                    assert_eq!(metrics.capture.uploaded_bytes, 0);
                    let actual = readback(&gpu, &source);
                    for (i, (&a, &b)) in actual.iter().zip(reference.data()).enumerate() {
                        assert!(
                            (a - b).abs() <= 2e-5 + 2e-5 * b.abs(),
                            "{algorithm:?} {pattern} {width}x{height} kind={kind} index={i}: GPU {a}, CPU {b}"
                        );
                    }
                    for y in 0..height {
                        for x in 0..width {
                            let c = reference_mosaic.pattern().color_at(x, y).channel_index();
                            assert_eq!(
                                actual[(y * width + x) * 3 + c],
                                *reference_mosaic.sample(x, y)
                            );
                        }
                    }
                }
            }
        }
    }
}

#[test]
#[ignore = "requires Vulkan; GPU RCD with a non-unit as-shot channel balance"]
fn rcd_matches_cpu_under_a_non_unit_channel_balance() {
    let _guard = crate::preview::processor::tests::gpu_test_guard();
    let gpu = processor();
    let capture =
        crate::capture_only_contract(CaptureSharpening::default(), [1.0; 3]).expect("capture off");
    for pattern in ["RGGB", "GBRG"] {
        for kind in [0, 2, 5] {
            let mut frame = frame(pattern, 199, 201, kind);
            frame.info.as_shot_white_balance = [Some(2.1), Some(1.0), Some(2.6), None];
            let description = description(&frame, DemosaicAlgorithm::Rcd, false);
            let balance = description.demosaic_balance();
            assert!((balance.red - 2.1).abs() < 1e-6 && (balance.blue - 2.6).abs() < 1e-6);
            let mosaic =
                normalize_raw(&frame, RawCropPolicy::Recommended).expect("CPU normalized mosaic");
            let reference = demosaic(&mosaic, balance, DemosaicAlgorithm::Rcd)
                .expect("CPU balanced RCD reference");
            let (source, _) = gpu
                .demosaic(
                    highlighted(&gpu, &frame, &description),
                    &description,
                    &capture,
                    &CancellationToken::new(),
                )
                .expect("GPU balanced RCD");
            let actual = readback(&gpu, &source);
            for (i, (&a, &b)) in actual.iter().zip(reference.data()).enumerate() {
                assert!(
                    (a - b).abs() <= 2e-5 + 2e-5 * b.abs(),
                    "balanced RCD {pattern} kind={kind} index={i}: GPU {a}, CPU {b}"
                );
            }
        }
    }
}

#[test]
#[ignore = "requires Vulkan; RCD fixed-grid core and partial-tile camera RGB parity"]
fn rcd_matches_cpu_with_mosaic_layer_seams_and_scratch_reuse() {
    let _guard = crate::preview::processor::tests::gpu_test_guard();
    let mut gpu = processor();
    gpu.force_maximum_tile_edge(31);
    let capture =
        crate::capture_only_contract(CaptureSharpening::default(), [1.0; 3]).expect("capture off");
    for pattern in ["RGGB", "BGGR", "GRBG", "GBRG"] {
        for (width, height) in [
            (24, 39),
            (25, 27),
            (174, 25),
            (194, 27),
            (199, 201),
            (208, 198),
        ] {
            for kind in [0, 2, 5, 6] {
                let frame = frame(pattern, width, height, kind);
                let description = description(&frame, DemosaicAlgorithm::Rcd, false);
                let mosaic = normalize_raw(&frame, RawCropPolicy::Recommended)
                    .expect("CPU normalized mosaic");
                let reference = demosaic(
                    &mosaic,
                    description.demosaic_balance(),
                    DemosaicAlgorithm::Rcd,
                )
                .expect("CPU RCD reference");
                let highlighted = highlighted(&gpu, &frame, &description);
                let (source, metrics) = gpu
                    .demosaic(
                        highlighted,
                        &description,
                        &capture,
                        &CancellationToken::new(),
                    )
                    .expect("GPU RCD");
                assert_eq!(metrics.diagnostic_readback_bytes, 4);
                let actual = readback(&gpu, &source);
                for (i, (&a, &b)) in actual.iter().zip(reference.data()).enumerate() {
                    assert!(
                        (a - b).abs() <= 2e-5 + 2e-5 * b.abs(),
                        "RCD {pattern} {width}x{height} kind={kind} index={i}: GPU {a}, CPU {b}"
                    );
                }
                for y in 0..height {
                    for x in 0..width {
                        let channel = mosaic.pattern().color_at(x, y).channel_index();
                        assert_eq!(actual[(y * width + x) * 3 + channel], *mosaic.sample(x, y));
                    }
                }
            }
        }
    }
}

#[test]
#[ignore = "requires Vulkan; compares GPU RCD scratch stages with frozen CPU probes"]
fn rcd_intermediate_stages_match_signed_partial_tile_cpu_snapshot() {
    use super::rcd::RcdExecutor;
    use crate::spatial::{resources::ResidentLayout, source::ResidentCameraPlanes};

    let _guard = crate::preview::processor::tests::gpu_test_guard();
    let gpu = processor();
    let frame = frame("BGGR", 29, 31, 0); // odd crop shifts the selected phase to RGGB
    let desc = description(&frame, DemosaicAlgorithm::Rcd, false);
    let mosaic = highlighted(&gpu, &frame, &desc);
    assert_eq!(
        mosaic.normalization.crop().pattern(),
        rohditor_image::BayerPattern::Rggb
    );
    assert_eq!(mosaic.layout.layers, 1);
    let values: Vec<f32> = (0..29 * 31)
        .map(|i| {
            let (x, y) = (i % 29, i / 29);
            match (x + 3 * y) % 11 {
                0 => -0.0,
                1 => 1.0e-6,
                2 => -1.0e-6,
                3 => 1.5,
                4 => -0.2,
                5 => 2.0,
                _ => ((x * 37 + y * 19) % 101) as f32 / 63.0 - 0.4,
            }
        })
        .collect();
    gpu.queue.write_texture(
        wgpu::TexelCopyTextureInfo {
            texture: &mosaic._texture,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        bytemuck::cast_slice(&values),
        wgpu::TexelCopyBufferLayout {
            offset: 0,
            bytes_per_row: Some(29 * 4),
            rows_per_image: Some(31),
        },
        wgpu::Extent3d {
            width: 29,
            height: 31,
            depth_or_array_layers: 1,
        },
    );
    let layout = ResidentLayout::new_generated(
        29,
        31,
        mosaic.estimated_bytes(),
        RcdExecutor::scratch_bytes() + 64 * 1024,
        gpu.budget,
        &gpu.device.limits(),
        gpu.maximum_tile_edge,
    )
    .expect("RCD diagnostic layout");
    let planes = ResidentCameraPlanes::with_budget(&gpu.device, layout, gpu.budget)
        .expect("RCD diagnostic planes");
    let validation = gpu.device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("RCD stage test flag"),
        size: 4,
        usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC,
        mapped_at_creation: false,
    });
    let view = mosaic._texture.create_view(&wgpu::TextureViewDescriptor {
        dimension: Some(wgpu::TextureViewDimension::D2Array),
        ..Default::default()
    });
    let executor = RcdExecutor::new(&gpu, ChannelBalance::new(WhiteBalanceGains::identity()))
        .expect("RCD stage executor");
    let snapshots = executor
        .stage_probes(
            &gpu,
            &mosaic,
            &view,
            &planes,
            &validation,
            &CancellationToken::new(),
        )
        .expect("GPU RCD stage snapshots");
    assert_eq!(snapshots.len(), 9);
    let full = 194 * 194;
    let half = full / 2;
    let red = 10 * 194 + 10;
    let green = red + 1;
    let probes = [
        (1, 4 * full + red, 0.51112366_f32),
        (2, 5 * full + red / 2, 2.155159),
        (3, 2 * full + red, 0.56720024),
        (4, 5 * full + half + green / 2, 1.1650273),
        (4, 5 * full + 2 * half + green / 2, 63.11414),
        (5, 5 * full + red / 2, 0.20751585),
        (6, 3 * full + red, 0.9845681),
        (7, full + green, 0.95685244),
        (7, 3 * full + green, 1.0756333),
    ];
    for (stage, index, expected) in probes {
        let actual = snapshots[stage][index];
        assert!(
            (actual - expected).abs() <= 2e-5 + 2e-5 * expected.abs(),
            "RCD stage {stage} scratch {index}: GPU {actual}, CPU {expected}"
        );
    }
    for stage in &snapshots {
        assert_eq!(stage[full - 1], 0.0, "unused CFA scratch must be clear");
    }
    drop(executor);
    drop(planes);
    drop(view);
    let cpu_mosaic =
        rohditor_image::MosaicImage::new(29, 31, 29, rohditor_image::BayerPattern::Rggb, values)
            .expect("signed CPU mosaic");
    let reference = demosaic(&cpu_mosaic, desc.demosaic_balance(), DemosaicAlgorithm::Rcd)
        .expect("signed CPU RCD");
    let capture =
        crate::capture_only_contract(CaptureSharpening::default(), [1.0; 3]).expect("capture off");
    let (source, _) = gpu
        .demosaic(mosaic, &desc, &capture, &CancellationToken::new())
        .expect("signed GPU RCD");
    let actual = readback(&gpu, &source);
    for (index, (&gpu_value, &cpu_value)) in actual.iter().zip(reference.data()).enumerate() {
        assert!(
            (gpu_value - cpu_value).abs() <= 2e-5 + 2e-5 * cpu_value.abs(),
            "signed RCD index={index}: GPU {gpu_value}, CPU {cpu_value}"
        );
    }
    for y in 0..31 {
        for x in 0..29 {
            let channel = cpu_mosaic.pattern().color_at(x, y).channel_index();
            assert_eq!(
                actual[(y * 29 + x) * 3 + channel].to_bits(),
                cpu_mosaic.sample(x, y).to_bits(),
                "measured-site bits at {x},{y}"
            );
        }
    }
}

#[test]
fn rcd_capture_two_image_floor_exceeds_48mp_budget() {
    let budget = 768_u64 * 1024 * 1024;
    assert!(28_u64 * 6000 * 4000 < budget);
    assert!(28_u64 * 8000 * 6000 > budget);
}

#[test]
#[ignore = "requires Vulkan; RCD/AMaZE capture allocation fails before camera publication"]
fn rcd_and_amaze_capture_budget_failure_releases_all_reservations() {
    let _guard = crate::preview::processor::tests::gpu_test_guard();
    let mut gpu = processor();
    let before = crate::gpu_memory_reservations().current_bytes;
    gpu.set_budget(before + 1_150_000);
    let frame = frame("RGGB", 81, 67, 6);
    let capture = crate::capture_only_contract(
        CaptureSharpening {
            enabled: true,
            radius: 0.6,
            ..Default::default()
        },
        [1.0; 3],
    )
    .expect("active capture fixture");
    for algorithm in [DemosaicAlgorithm::Rcd, DemosaicAlgorithm::Amaze] {
        let description = description(&frame, algorithm, false);
        let input = highlighted(&gpu, &frame, &description);
        let result = gpu.demosaic(input, &description, &capture, &CancellationToken::new());
        assert!(
            matches!(result, Err(crate::GpuPreviewError::Unsupported { .. })),
            "{algorithm:?}"
        );
        assert_eq!(crate::gpu_memory_reservations().current_bytes, before);
    }
}

#[test]
#[ignore = "requires Vulkan; RCD/AMaZE finite flag rejects a bad measured sample and recovers"]
fn rcd_and_amaze_nonfinite_output_does_not_publish_camera_planes() {
    let _guard = crate::preview::processor::tests::gpu_test_guard();
    let gpu = processor();
    let before = crate::gpu_memory_reservations().current_bytes;
    let capture =
        crate::capture_only_contract(CaptureSharpening::default(), [1.0; 3]).expect("capture off");
    // The bad sample lies inside each algorithm's reconstructed core.
    for (algorithm, size, bad) in [
        (DemosaicAlgorithm::Rcd, (29, 31), 12),
        (DemosaicAlgorithm::Amaze, (45, 47), 24),
    ] {
        let frame = frame("BGGR", size.0, size.1, 6);
        let description = description(&frame, algorithm, false);
        let input = highlighted(&gpu, &frame, &description);
        gpu.queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &input._texture,
                mip_level: 0,
                origin: wgpu::Origin3d {
                    x: bad,
                    y: bad,
                    z: 0,
                },
                aspect: wgpu::TextureAspect::All,
            },
            bytemuck::bytes_of(&f32::NAN),
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(4),
                rows_per_image: Some(1),
            },
            wgpu::Extent3d {
                width: 1,
                height: 1,
                depth_or_array_layers: 1,
            },
        );
        assert!(
            gpu.demosaic(input, &description, &capture, &CancellationToken::new())
                .is_err(),
            "{algorithm:?}"
        );
        assert_eq!(crate::gpu_memory_reservations().current_bytes, before);
        let good = highlighted(&gpu, &frame, &description);
        let (source, _) = gpu
            .demosaic(good, &description, &capture, &CancellationToken::new())
            .expect("recovers after invalid input");
        drop(source);
        assert_eq!(crate::gpu_memory_reservations().current_bytes, before);
    }
}

#[test]
#[ignore = "requires Vulkan; cancellation between fixed RCD tiles"]
fn rcd_cancellation_between_tiles_keeps_immutable_raw_for_recovery() {
    use super::rcd::RcdExecutor;
    use crate::spatial::{resources::ResidentLayout, source::ResidentCameraPlanes};

    let _guard = crate::preview::processor::tests::gpu_test_guard();
    let gpu = processor();
    let before = crate::gpu_memory_reservations().current_bytes;
    let frame = frame("RGGB", 200, 200, 6);
    let original = frame.mosaic.clone();
    let description = description(&frame, DemosaicAlgorithm::Rcd, false);
    let mosaic = highlighted(&gpu, &frame, &description);
    let layout = ResidentLayout::new_generated(
        200,
        200,
        mosaic.estimated_bytes(),
        RcdExecutor::scratch_bytes() + 64 * 1024,
        gpu.budget,
        &gpu.device.limits(),
        gpu.maximum_tile_edge,
    )
    .expect("RCD cancellation layout");
    let planes = ResidentCameraPlanes::with_budget(&gpu.device, layout, gpu.budget)
        .expect("RCD cancellation planes");
    let validation = gpu.device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("RCD cancellation flag"),
        size: 4,
        usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC,
        mapped_at_creation: false,
    });
    let view = mosaic._texture.create_view(&wgpu::TextureViewDescriptor {
        dimension: Some(wgpu::TextureViewDimension::D2Array),
        ..Default::default()
    });
    let executor = RcdExecutor::new(&gpu, ChannelBalance::new(WhiteBalanceGains::identity()))
        .expect("RCD cancellation executor");
    let token = CancellationToken::new();
    assert!(matches!(
        executor.cancel_after_first_tile(&gpu, &mosaic, &view, &planes, &validation, &token),
        Err(crate::GpuPreviewError::Cancelled)
    ));
    assert_eq!(frame.mosaic, original);
    drop((view, planes, mosaic, executor));
    assert_eq!(crate::gpu_memory_reservations().current_bytes, before);
    normalize_raw(&frame, RawCropPolicy::Recommended).expect("CPU recovery after cancellation");
}

#[test]
#[ignore = "requires Vulkan; direct demosaic-to-capture with forced halo/tile crossings"]
fn mhc_rcd_and_amaze_feed_capture_without_rgb_upload_or_readback() {
    let _guard = crate::preview::processor::tests::gpu_test_guard();
    let mut gpu = processor();
    gpu.force_maximum_tile_edge(16);
    let capture = crate::capture_only_contract(
        CaptureSharpening {
            enabled: true,
            radius: 0.6,
            noise_protection: 0.0,
            ..Default::default()
        },
        [1.0; 3],
    )
    .expect("capture");
    for algorithm in [
        DemosaicAlgorithm::MalvarHeCutler,
        DemosaicAlgorithm::Rcd,
        DemosaicAlgorithm::Amaze,
    ] {
        for clip in [false, true] {
            let frame = frame("GRBG", 81, 67, 6);
            let description = description(&frame, algorithm, clip);
            let mut mosaic =
                normalize_raw(&frame, RawCropPolicy::Recommended).expect("CPU normalize");
            if let rohditor_core::HighlightExecution::Clip(levels) =
                description.highlight_execution()
            {
                mosaic = rohditor_highlight::clip(mosaic, levels)
                    .expect("CPU Clip")
                    .mosaic;
            }
            let mut reference =
                demosaic(&mosaic, WhiteBalanceGains::identity(), algorithm).expect("CPU demosaic");
            capture
                .capture()
                .apply_cpu(&mut reference, &CancellationToken::new())
                .expect("CPU capture");
            let input = highlighted(&gpu, &frame, &description);
            let (source, metrics) = gpu
                .demosaic(input, &description, &capture, &CancellationToken::new())
                .expect("resident demosaic/capture");
            assert!(metrics.capture.tiles > 1);
            // Producer, all RL passes and the scatter share one submission per tile.
            assert_eq!(metrics.capture.submissions as usize, metrics.capture.tiles);
            assert_eq!(metrics.capture.uploaded_bytes, 0);
            assert_eq!(metrics.capture.readback_bytes, 0);
            for (a, b) in readback(&gpu, &source).iter().zip(reference.data()) {
                assert!(
                    (a - b).abs() <= 2e-5 + 2e-5 * b.abs(),
                    "{algorithm:?} capture GPU {a}, CPU {b}"
                );
            }
        }
    }
}

#[test]
#[ignore = "requires Vulkan; whole detail stage on resident planes against the CPU stage"]
fn detail_stage_runs_on_resident_planes_for_every_algorithm() {
    let _guard = crate::preview::processor::tests::gpu_test_guard();
    let mut gpu = processor();
    gpu.force_maximum_tile_edge(48);
    let mut recipe = rohditor_edit::EditRecipe::default();
    recipe.false_color.steps = 2;
    recipe.noise_reduction.chroma = 0.6;
    recipe.noise_reduction.luminance = 0.5;
    recipe.capture_sharpening.enabled = true;
    recipe.capture_sharpening.radius = 0.6;
    let balance = WhiteBalanceGains {
        red: 2.0,
        green: 1.0,
        blue: 1.5,
    };
    let detail =
        rohditor_core::DetailContract::new(&recipe, balance, WhiteBalanceGains::identity())
            .expect("detail contract");
    for algorithm in [
        DemosaicAlgorithm::MalvarHeCutler,
        DemosaicAlgorithm::Rcd,
        DemosaicAlgorithm::Amaze,
    ] {
        let frame = frame("GRBG", 301, 187, 6);
        let description = description(&frame, algorithm, false);
        let mosaic = normalize_raw(&frame, RawCropPolicy::Recommended).expect("CPU normalize");
        let mut reference =
            demosaic(&mosaic, WhiteBalanceGains::identity(), algorithm).expect("CPU demosaic");
        detail
            .apply_cpu(&mut reference, &CancellationToken::new())
            .expect("CPU detail stage");
        let input = highlighted(&gpu, &frame, &description);
        let (source, metrics) = gpu
            .demosaic(input, &description, &detail, &CancellationToken::new())
            .expect("resident demosaic and detail");
        assert!(metrics.capture.tiles > 1);
        // Only the profiled rows cross to the CPU; no RGB upload.
        assert_eq!(metrics.capture.uploaded_bytes, 0);
        let mut worst = 0.0_f32;
        for (a, b) in readback(&gpu, &source).iter().zip(reference.data()) {
            worst = worst.max((a - b).abs() / (1.0 + b.abs()));
        }
        eprintln!("{algorithm:?} detail stage: max error {worst:e}");
        assert!(worst <= 1e-3, "{algorithm:?}: deviates by {worst:e}");
    }
}

#[test]
#[ignore = "requires Vulkan; prints timings of the detail stage on a synthetic 24 MP mosaic"]
fn detail_stage_timings_at_24mp() {
    let _guard = crate::preview::processor::tests::gpu_test_guard();
    let gpu = processor();
    let balance = WhiteBalanceGains {
        red: 2.0,
        green: 1.0,
        blue: 1.5,
    };
    let frame = frame("RGGB", 6000, 4000, 6);
    let description = description(&frame, DemosaicAlgorithm::Rcd, false);
    type Configure = fn(&mut rohditor_edit::EditRecipe);
    let cases: [(&str, Configure); 4] = [
        ("no detail", |_| {}),
        ("capture sharpening", |r| {
            r.capture_sharpening.enabled = true;
        }),
        ("chroma + luma NR", |r| {
            r.noise_reduction.chroma = 0.5;
            r.noise_reduction.luminance = 0.5;
        }),
        ("everything", |r| {
            r.false_color.steps = 2;
            r.noise_reduction.chroma = 0.5;
            r.noise_reduction.luminance = 0.5;
            r.capture_sharpening.enabled = true;
        }),
    ];
    for (name, configure) in cases {
        let mut recipe = rohditor_edit::EditRecipe::default();
        configure(&mut recipe);
        let detail =
            rohditor_core::DetailContract::new(&recipe, balance, WhiteBalanceGains::identity())
                .expect("detail contract");
        let input = highlighted(&gpu, &frame, &description);
        let (_source, metrics) = gpu
            .demosaic(input, &description, &detail, &CancellationToken::new())
            .expect("resident demosaic and detail");
        eprintln!(
            "{name}: total {:?}, detail {:?} ({} tiles of {} + halo {}), gpu {} MiB",
            metrics.total,
            metrics.capture.total,
            metrics.capture.tiles,
            metrics.capture.tile_edge,
            metrics.capture.halo,
            metrics.estimated_gpu_bytes >> 20,
        );
    }
}

#[test]
#[ignore = "requires Vulkan; rejection must never publish invalid camera RGB"]
fn demosaic_rejects_mismatched_cancelled_overbudget_and_nonfinite_results() {
    let _guard = crate::preview::processor::tests::gpu_test_guard();
    let mut gpu = processor();
    let frame = frame("RGGB", 13, 11, 0);
    let capture =
        crate::capture_only_contract(CaptureSharpening::default(), [1.0; 3]).expect("off");
    let desc = description(&frame, DemosaicAlgorithm::MalvarHeCutler, false);
    let input = highlighted(&gpu, &frame, &desc);
    let different = description(&frame, DemosaicAlgorithm::MalvarHeCutler, true);
    assert!(matches!(
        gpu.demosaic(input, &different, &capture, &CancellationToken::new()),
        Err(crate::GpuPreviewError::BaseMismatch { .. })
    ));
    let input = highlighted(&gpu, &frame, &desc);
    let token = CancellationToken::new();
    token.cancel();
    assert!(matches!(
        gpu.demosaic(input, &desc, &capture, &token),
        Err(crate::GpuPreviewError::Cancelled)
    ));
    for value in [f32::NAN, f32::INFINITY, f32::MAX] {
        let input = highlighted(&gpu, &frame, &desc);
        gpu.queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &input._texture,
                mip_level: 0,
                origin: wgpu::Origin3d { x: 2, y: 2, z: 0 },
                aspect: wgpu::TextureAspect::All,
            },
            bytemuck::bytes_of(&value),
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(4),
                rows_per_image: Some(1),
            },
            wgpu::Extent3d {
                width: 1,
                height: 1,
                depth_or_array_layers: 1,
            },
        );
        assert!(
            gpu.demosaic(input, &desc, &capture, &CancellationToken::new())
                .is_err(),
            "must reject {value}"
        );
    }
    let input = highlighted(&gpu, &frame, &desc);
    gpu.set_budget(64);
    assert!(matches!(
        gpu.demosaic(input, &desc, &capture, &CancellationToken::new()),
        Err(crate::GpuPreviewError::Unsupported { .. })
    ));
    normalize_raw(&frame, RawCropPolicy::Recommended).expect("immutable RAW remains usable");
}

#[test]
#[ignore = "requires Vulkan; capture halo must fit alongside mosaic and final RGB"]
fn demosaic_capture_budget_counts_overlapping_lifetimes() {
    let _guard = crate::preview::processor::tests::gpu_test_guard();
    let mut gpu = processor();
    let before = crate::gpu_memory_reservations().current_bytes;
    let frame = frame("RGGB", 81, 67, 6);
    let desc = description(&frame, DemosaicAlgorithm::MalvarHeCutler, false);
    let input = highlighted(&gpu, &frame, &desc);
    let capture = crate::capture_only_contract(
        CaptureSharpening {
            enabled: true,
            radius: 0.6,
            ..Default::default()
        },
        [1.0; 3],
    )
    .expect("capture");
    gpu.set_budget(300_000);
    assert!(matches!(
        gpu.demosaic(input, &desc, &capture, &CancellationToken::new()),
        Err(crate::GpuPreviewError::Unsupported { .. })
    ));
    assert_eq!(crate::gpu_memory_reservations().current_bytes, before);
    gpu.set_budget(super::resources::DEFAULT_BUDGET);
    let input = highlighted(&gpu, &frame, &desc);
    let (source, _) = gpu
        .demosaic(input, &desc, &capture, &CancellationToken::new())
        .expect("capture fits restored budget");
    assert_eq!(
        crate::gpu_memory_reservations().current_bytes,
        before + source.estimated_bytes()
    );
    drop(source);
    assert_eq!(crate::gpu_memory_reservations().current_bytes, before);
}

#[test]
#[ignore = "private 24 MP Sony RAW and Vulkan; timings include queue completion, not presentation"]
fn private_mhc_camera_parity_and_full_resolution_timings() -> Result<(), Box<dyn std::error::Error>>
{
    private_camera_parity_and_timings(DemosaicAlgorithm::MalvarHeCutler, 2)
}

#[test]
#[ignore = "private 24 MP Sony RAW and Vulkan; RCD camera RGB parity, software timing only"]
fn private_rcd_camera_parity_and_full_resolution_timings() -> Result<(), Box<dyn std::error::Error>>
{
    private_camera_parity_and_timings(DemosaicAlgorithm::Rcd, 1)
}

#[test]
#[ignore = "private 24 MP Sony RAW and Vulkan; AMaZE camera RGB parity, hardware timing"]
fn private_amaze_camera_parity_and_full_resolution_timings()
-> Result<(), Box<dyn std::error::Error>> {
    private_camera_parity_and_timings(DemosaicAlgorithm::Amaze, 3)
}

fn private_camera_parity_and_timings(
    algorithm: DemosaicAlgorithm,
    iterations: usize,
) -> Result<(), Box<dyn std::error::Error>> {
    use rohditor_raw::{RawDecoder, RawlerDecoder};
    let _guard = crate::preview::processor::tests::gpu_test_guard();
    let gpu = processor();
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../testdata/private/DSC00851.ARW");
    let frame = RawlerDecoder::default().decode(&path)?;
    let desc = description(&frame, algorithm, true);
    let capture = crate::capture_only_contract(CaptureSharpening::default(), [1.0; 3])?;
    let token = CancellationToken::new();
    let mosaic = normalize_raw(&frame, RawCropPolicy::Recommended)?;
    let rohditor_core::HighlightExecution::Clip(levels) = desc.highlight_execution() else {
        unreachable!()
    };
    let mosaic = rohditor_highlight::clip(mosaic, levels)?.mosaic;
    let cpu_started = std::time::Instant::now();
    let reference = demosaic(&mosaic, desc.demosaic_balance(), algorithm)?;
    let cpu_time = cpu_started.elapsed();
    drop(mosaic);
    // Reconstruction runs on the balanced mosaic, so f32 rounding differences
    // between GPU and CPU grow by the inverse gain when scaled back.
    let slack = ChannelBalance::new(desc.demosaic_balance())
        .inverse()
        .into_iter()
        .fold(1.0_f32, f32::max);
    for iteration in 0..iterations {
        let (normalized, normalization) = gpu.normalize(&frame, desc.normalization(), &token)?;
        let (highlighted, _, highlight) = gpu.apply_highlight(normalized, &desc, &token)?;
        let (source, metrics) = gpu.demosaic(highlighted, &desc, &capture, &token)?;
        let actual = readback(&gpu, &source);
        let mut maximum = 0.0_f32;
        let mut outliers = 0usize;
        for (&a, &b) in actual.iter().zip(reference.data()) {
            let error = (a - b).abs();
            if error > slack * (2e-5 + 2e-5 * b.abs()) {
                outliers += 1;
            }
            maximum = maximum.max(error);
        }
        // A few directional decisions can flip on f32 rounding differences;
        // everything else must match within tolerance. Measured on DSC00851:
        // RCD has a cluster of 84 samples with max error 1.8e-3. AMaZE makes
        // many more threshold decisions and has about 1.2k isolated samples
        // with max error 3.2e-3 (1.6e-5 of all samples).
        let allowed_fraction = match algorithm {
            DemosaicAlgorithm::Amaze => 5e-5,
            _ => 1e-5,
        };
        assert!(
            outliers as f64 <= allowed_fraction * actual.len() as f64 && maximum <= 5e-3,
            "{outliers} samples outside tolerance, max error {maximum}"
        );
        eprintln!(
            "sensor {algorithm:?} DSC00851 {:?} iteration={iteration} (first/repeated dispatch, same device): CPU={cpu_time:?}, GPU normalization={:?}, Clip={:?}, demosaic={:?}, max camera error={maximum:e}, uploaded={} diagnostic readback={} logical demosaic peak={} retained={} RCD/base tiles={} submissions={}",
            source.dimensions(),
            normalization.total,
            highlight.total,
            metrics.total,
            normalization.uploaded_bytes,
            highlight.diagnostic_readback_bytes + metrics.diagnostic_readback_bytes,
            metrics.estimated_gpu_bytes,
            metrics.resident_gpu_bytes,
            metrics.tiles,
            metrics.submissions,
        );
    }
    Ok(())
}
