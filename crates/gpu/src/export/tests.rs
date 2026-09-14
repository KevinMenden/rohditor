use super::super::tests::{gpu_control_matrix, gpu_test_guard, neutral_recipe, synthetic_frame};
use super::*;
use rohditor_core::{apply_adjustments, render_display_srgb8_dithered, render_display_srgb16};
use rohditor_raw::{RawDecoder, RawlerDecoder};

fn processor(hardware: bool) -> Option<GpuExportProcessor> {
    let instance = wgpu::Instance::new(&wgpu::InstanceDescriptor {
        backends: wgpu::Backends::VULKAN,
        ..Default::default()
    });
    let adapter =
        pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions::default()))
            .expect("Vulkan adapter");
    let info = adapter.get_info();
    eprintln!(
        "Export adapter: {} {:?} {:?}, driver {} {}",
        info.name, info.backend, info.device_type, info.driver, info.driver_info
    );
    if hardware && info.device_type == wgpu::DeviceType::Cpu {
        eprintln!("SKIPPED hardware export qualification: CPU rasterizer");
        return None;
    }
    let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
        required_limits: wgpu::Limits::default().using_resolution(adapter.limits()),
        ..Default::default()
    }))
    .expect("export device");
    Some(GpuExportProcessor::new(&adapter, &device, &queue).expect("export pipeline"))
}

#[test]
#[ignore = "requires a Vulkan adapter; software execution validates structure only"]
fn export_bands_precision_geometry_and_cancellation() {
    let _guard = gpu_test_guard();
    let mut gpu = processor(false).expect("export qualification fixture");
    let cpu = CpuPipeline::default();
    let mut frame = synthetic_frame(Orientation::Normal);
    frame.info.width = 38;
    frame.info.height = 142;
    frame.row_stride = 38;
    frame.mosaic = (0..38 * 142)
        .map(|i| ((i * 137 + (i / 38) * 503) % 65536) as u16)
        .collect::<Vec<_>>()
        .into();
    for orientation in [
        Orientation::Normal,
        Orientation::Rotate90,
        Orientation::Rotate180,
        Orientation::HorizontalFlip,
        Orientation::VerticalFlip,
        Orientation::Transpose,
        Orientation::Transverse,
        Orientation::Rotate270,
    ] {
        for (label, mut recipe) in gpu_control_matrix() {
            recipe.geometry.orientation_override = Some(orientation);
            if label == "combined_supported" {
                recipe.geometry.crop = Some(rohditor_edit::NormalizedCropRect {
                    left: 0.13,
                    top: 0.07,
                    right: 0.91,
                    bottom: 0.94,
                });
            }
            let prepared = cpu
                .prepare_export_source(
                    &frame,
                    &recipe,
                    RenderOptions::default(),
                    &CancellationToken::new(),
                )
                .expect("export qualification fixture");
            assert_eq!(
                (prepared.image().width(), prepared.image().height()),
                (38, 142)
            );
            for depth in [OutputBitDepth::Eight, OutputBitDepth::Sixteen] {
                for dither in [DitherMode::None, DitherMode::Ordered8x8] {
                    let actual = gpu
                        .render_prepared(
                            &prepared,
                            &recipe,
                            OutputPolicy::ClipToSrgb,
                            depth,
                            dither,
                            &CancellationToken::new(),
                        )
                        .expect("export qualification fixture");
                    let reference = cpu
                        .render_export(&frame, &recipe, RenderOptions::default(), depth, dither)
                        .expect("export qualification fixture");
                    let (max, mean) = difference(&actual.image, &reference.image);
                    if let ExportImage::Rgb16(image) = &actual.image {
                        assert!(
                            image.data().iter().any(|sample| sample % 257 != 0),
                            "16-bit export must preserve precision beyond expanded 8-bit samples"
                        );
                    }
                    eprintln!(
                        "export {label} {orientation:?} {depth:?} {dither:?}: max={max}, mean={mean:.4}"
                    );
                    let limit = if depth == OutputBitDepth::Eight {
                        2
                    } else {
                        16
                    };
                    assert!(max <= limit, "{label}: {max} > {limit}");
                    if depth == OutputBitDepth::Eight && dither == DitherMode::None {
                        let source = gpu
                            .processor
                            .upload_prepared(
                                GpuPreviewUpload::from_reconstructed_preview_for_recipe(
                                    &prepared,
                                    &recipe,
                                    &CancellationToken::new(),
                                )
                                .expect("export qualification fixture"),
                            )
                            .expect("export qualification fixture");
                        let preview = gpu
                            .processor
                            .render(&source, &recipe, OutputPolicy::ClipToSrgb, None)
                            .expect("export qualification fixture");
                        let display = gpu
                            .processor
                            .readback_display(&preview)
                            .expect("export qualification fixture");
                        let ExportImage::Rgb8(image) = &actual.image else {
                            unreachable!()
                        };
                        for (a, b) in image
                            .data()
                            .chunks_exact(3)
                            .zip(display.rgba.chunks_exact(4))
                        {
                            assert!(a.iter().zip(b).all(|(a, b)| a.abs_diff(*b) <= 1));
                        }
                    }
                }
            }
        }
    }
    let cancelled = CancellationToken::new();
    cancelled.cancel();
    assert!(matches!(
        gpu.render(
            &cpu,
            &frame,
            &neutral_recipe(),
            RenderOptions::default(),
            OutputBitDepth::Eight,
            DitherMode::None,
            &cancelled
        ),
        Err(GpuPreviewError::Cancelled)
    ));
    assert!(gpu.validate_memory(16384, 16384).is_err());
    assert!(gpu.validate_memory(usize::MAX, 2).is_err());
}

fn difference(a: &ExportImage, b: &ExportImage) -> (u32, f64) {
    assert_eq!((a.width(), a.height()), (b.width(), b.height()));
    let differences: Vec<u32> = match (a, b) {
        (ExportImage::Rgb8(a), ExportImage::Rgb8(b)) => a
            .data()
            .iter()
            .zip(b.data())
            .map(|(a, b)| u32::from(a.abs_diff(*b)))
            .collect(),
        (ExportImage::Rgb16(a), ExportImage::Rgb16(b)) => a
            .data()
            .iter()
            .zip(b.data())
            .map(|(a, b)| u32::from(a.abs_diff(*b)))
            .collect(),
        _ => panic!("bit depth mismatch"),
    };
    (
        *differences
            .iter()
            .max()
            .expect("export qualification fixture"),
        differences.iter().map(|&n| f64::from(n)).sum::<f64>() / differences.len() as f64,
    )
}

#[test]
#[ignore = "requires private RAW corpus and hardware Vulkan GPU"]
fn private_full_resolution_export_parity() {
    private_export_parity(true);
}

#[test]
#[ignore = "private full-resolution software Vulkan check; not hardware qualification"]
fn private_full_resolution_export_software_structure() {
    private_export_parity(false);
}

fn private_export_parity(hardware: bool) {
    let _guard = gpu_test_guard();
    let Some(mut gpu) = processor(hardware) else {
        return;
    };
    let cpu = CpuPipeline::default();
    for name in [
        "DSC00851.ARW",
        "DSC01166.ARW",
        "DSC02382.ARW",
        "DSC03270.ARW",
        "DSC03687.ARW",
        "DSC03821.ARW",
    ] {
        if !hardware && name != "DSC00851.ARW" {
            continue;
        }
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../testdata/private")
            .join(name);
        let frame = RawlerDecoder::default().decode(&path).expect("private RAW");
        let (_, recipe) = gpu_control_matrix()
            .into_iter()
            .find(|(label, _)| *label == "combined_supported")
            .expect("export qualification fixture");
        let prepared = cpu
            .prepare_export_source(
                &frame,
                &recipe,
                RenderOptions::default(),
                &CancellationToken::new(),
            )
            .expect("export qualification fixture");
        for policy in [OutputPolicy::ClipToSrgb, OutputPolicy::ChromaCompressToSrgb] {
            let base = cpu
                .prepare_preview_base_from_reconstruction(&prepared, &recipe)
                .expect("export qualification fixture");
            let mut linear = base.image().clone();
            drop(base);
            apply_adjustments(&mut linear, &recipe).expect("export qualification fixture");
            for depth in [OutputBitDepth::Eight, OutputBitDepth::Sixteen] {
                let actual = gpu
                    .render_prepared(
                        &prepared,
                        &recipe,
                        policy,
                        depth,
                        DitherMode::Ordered8x8,
                        &CancellationToken::new(),
                    )
                    .expect("export qualification fixture");
                let reference = match depth {
                    OutputBitDepth::Eight => ExportImage::Rgb8(
                        render_display_srgb8_dithered(
                            &linear,
                            prepared.source_orientation(),
                            policy,
                            DitherMode::Ordered8x8,
                        )
                        .expect("export qualification fixture"),
                    ),
                    OutputBitDepth::Sixteen => ExportImage::Rgb16(
                        render_display_srgb16(
                            &linear,
                            prepared.source_orientation(),
                            policy,
                            DitherMode::Ordered8x8,
                        )
                        .expect("export qualification fixture"),
                    ),
                };
                let (max, mean) = difference(&actual.image, &reference);
                eprintln!(
                    "export {name} {}x{} {depth:?} {policy:?}: max={max} mean={mean:.5}, prepare={:?} upload={:?} color/readback={:?} gpu_bytes={} upload_bytes={} readback_bytes={} bands={}",
                    actual.image.width(),
                    actual.image.height(),
                    actual.preparation.total,
                    actual.upload_time,
                    actual.color_and_readback_time,
                    actual.estimated_gpu_bytes,
                    actual.uploaded_bytes,
                    actual.readback_bytes,
                    actual.submissions
                );
                assert!(
                    max <= if depth == OutputBitDepth::Eight {
                        2
                    } else {
                        16
                    }
                );
                assert!(
                    mean <= if depth == OutputBitDepth::Eight {
                        0.1
                    } else {
                        1.0
                    }
                );
            }
        }
    }
}
