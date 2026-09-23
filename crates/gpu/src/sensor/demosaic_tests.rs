use std::sync::Arc;

use rohditor_core::{
    CancellationToken, CaptureSharpeningContract, RawCropPolicy, RenderOptions,
    SensorDevelopmentDescription, normalize_raw,
};
use rohditor_demosaic::{DemosaicAlgorithm, WhiteBalanceGains, demosaic};
use rohditor_edit::{CaptureSharpening, EditRecipe, HighlightMethod};
use rohditor_raw::{ImageRect, LevelPattern, RawFrame};

use super::tests::processor;
use super::{GpuSensorCameraSource, GpuSensorProcessor};

fn frame(pattern: &str, width: usize, height: usize, kind: usize) -> RawFrame {
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

fn description(
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

fn highlighted(
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

fn readback(gpu: &GpuSensorProcessor, source: &GpuSensorCameraSource) -> Vec<f32> {
    let layout = source.planes.layout;
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
            &source.planes.textures[channel],
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
    let capture = CaptureSharpeningContract::new(CaptureSharpening::default(), [1.0; 3])
        .expect("capture off");
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
#[ignore = "requires Vulkan; direct demosaic-to-capture with forced halo/tile crossings"]
fn mhc_feeds_capture_without_rgb_upload_or_readback() {
    let _guard = crate::preview::processor::tests::gpu_test_guard();
    let mut gpu = processor();
    gpu.force_maximum_tile_edge(16);
    let capture = CaptureSharpeningContract::new(
        CaptureSharpening {
            enabled: true,
            radius: 0.6,
            noise_protection: 0.0,
            ..Default::default()
        },
        [1.0; 3],
    )
    .expect("capture");
    for clip in [false, true] {
        let frame = frame("GRBG", 81, 67, 6);
        let description = description(&frame, DemosaicAlgorithm::MalvarHeCutler, clip);
        let mut mosaic = normalize_raw(&frame, RawCropPolicy::Recommended).expect("CPU normalize");
        if let rohditor_core::HighlightExecution::Clip(levels) = description.highlight_execution() {
            mosaic = rohditor_highlight::clip(mosaic, levels)
                .expect("CPU Clip")
                .mosaic;
        }
        let mut reference = demosaic(
            &mosaic,
            WhiteBalanceGains::identity(),
            DemosaicAlgorithm::MalvarHeCutler,
        )
        .expect("CPU MHC");
        capture
            .apply_cpu(&mut reference, &CancellationToken::new())
            .expect("CPU capture");
        let input = highlighted(&gpu, &frame, &description);
        let (source, metrics) = gpu
            .demosaic(input, &description, &capture, &CancellationToken::new())
            .expect("resident demosaic/capture");
        assert!(metrics.capture.tiles > 1);
        assert_eq!(metrics.capture.uploaded_bytes, 0);
        assert_eq!(metrics.capture.readback_bytes, 0);
        for (a, b) in readback(&gpu, &source).iter().zip(reference.data()) {
            assert!(
                (a - b).abs() <= 2e-5 + 2e-5 * b.abs(),
                "capture GPU {a}, CPU {b}"
            );
        }
    }
}

#[test]
#[ignore = "requires Vulkan; rejection must never publish invalid camera RGB"]
fn demosaic_rejects_unsupported_cancelled_overbudget_and_nonfinite_results() {
    let _guard = crate::preview::processor::tests::gpu_test_guard();
    let mut gpu = processor();
    let frame = frame("RGGB", 13, 11, 0);
    let capture =
        CaptureSharpeningContract::new(CaptureSharpening::default(), [1.0; 3]).expect("off");
    for algorithm in [DemosaicAlgorithm::Rcd, DemosaicAlgorithm::Amaze] {
        let desc = description(&frame, algorithm, false);
        let input = highlighted(&gpu, &frame, &desc);
        assert!(matches!(
            gpu.demosaic(input, &desc, &capture, &CancellationToken::new()),
            Err(crate::GpuPreviewError::UnsupportedEdits { .. })
        ));
    }
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
    let capture = CaptureSharpeningContract::new(
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
    use rohditor_raw::{RawDecoder, RawlerDecoder};
    let _guard = crate::preview::processor::tests::gpu_test_guard();
    let gpu = processor();
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../testdata/private/DSC00851.ARW");
    let frame = RawlerDecoder::default().decode(&path)?;
    let desc = description(&frame, DemosaicAlgorithm::MalvarHeCutler, true);
    let capture = CaptureSharpeningContract::new(CaptureSharpening::default(), [1.0; 3])?;
    let token = CancellationToken::new();
    let mosaic = normalize_raw(&frame, RawCropPolicy::Recommended)?;
    let rohditor_core::HighlightExecution::Clip(levels) = desc.highlight_execution() else {
        unreachable!()
    };
    let mosaic = rohditor_highlight::clip(mosaic, levels)?.mosaic;
    let cpu_started = std::time::Instant::now();
    let reference = demosaic(
        &mosaic,
        WhiteBalanceGains::identity(),
        DemosaicAlgorithm::MalvarHeCutler,
    )?;
    let cpu_time = cpu_started.elapsed();
    drop(mosaic);
    for iteration in 0..2 {
        let (normalized, normalization) = gpu.normalize(&frame, desc.normalization(), &token)?;
        let (highlighted, _, highlight) = gpu.apply_highlight(normalized, &desc, &token)?;
        let (source, metrics) = gpu.demosaic(highlighted, &desc, &capture, &token)?;
        let actual = readback(&gpu, &source);
        let mut maximum = 0.0_f32;
        for (index, (&a, &b)) in actual.iter().zip(reference.data()).enumerate() {
            let error = (a - b).abs();
            assert!(
                error <= 2e-5 + 2e-5 * b.abs(),
                "RAW camera sample {index}: GPU={a} CPU={b}"
            );
            maximum = maximum.max(error);
        }
        eprintln!(
            "sensor MHC DSC00851 {:?} iteration={iteration} (first/repeated dispatch, same device): CPU MHC={cpu_time:?}, GPU normalization={:?}, Clip={:?}, MHC={:?}, max camera error={maximum:e}, uploaded={} diagnostic readback={} logical demosaic peak={} retained={}",
            source.dimensions(),
            normalization.total,
            highlight.total,
            metrics.total,
            normalization.uploaded_bytes,
            highlight.diagnostic_readback_bytes + metrics.diagnostic_readback_bytes,
            metrics.estimated_gpu_bytes,
            metrics.resident_gpu_bytes
        );
    }
    Ok(())
}
