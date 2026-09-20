//! Private-RAW spatial display qualification and optional review artifacts.
//!
//! This is intentionally separate from the deterministic numerical tests: it
//! retains CPU/GPU crops for a human review of corrected corners and edges.

use std::error::Error;
use std::sync::Arc;

use rohditor_core::{CancellationToken, CpuPipeline, OutputPolicy, PreviewOptions};
use rohditor_edit::EditRecipe;
use rohditor_raw::{RawDecoder, RawlerDecoder};

use super::*;

const FIT_MAX_DISPLAY_CODE_ERROR: u8 = 3;
// Source 1:1 exposes the f64 Lensfun versus f32 WGSL coordinate rounding at
// isolated high-contrast cubic samples. The real-RAW gate retains both a
// maximum and a population bound, and the test writes the worst crop for
// visual review instead of hiding that difference behind an average.
const SOURCE_SCALE_MAX_DISPLAY_CODE_ERROR: u8 = 32;
const SOURCE_SCALE_MAX_OUTLIER_SAMPLES: usize = 256;

#[test]
#[ignore = "private Sony RAW, Vulkan, and optional ROHDITOR_GPU_SPATIAL_ARTIFACTS directory"]
fn private_spatial_display_parity_and_visual_crops() -> Result<(), Box<dyn Error>> {
    let _guard = crate::preview::processor::tests::gpu_test_guard();
    let (mut spatial, display) = processors();
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../testdata/private");
    let frame = RawlerDecoder::default().decode(&root.join("DSC00851.ARW"))?;
    let cpu = CpuPipeline::new(Arc::new(rohditor_core::OpticsService::load_bundled()?));
    let mut recipe = EditRecipe::default();
    recipe.optics.profile = rohditor_edit::LensProfileSelection::Automatic;
    let token = CancellationToken::new();
    let full_options = PreviewOptions {
        max_long_edge: frame.info.width.max(frame.info.height),
        ..PreviewOptions::default()
    };
    let fit_options = PreviewOptions {
        max_long_edge: 1001,
        ..full_options
    };

    // Build one immutable full source. Its fit clone shares source identity,
    // so this verifies that reduction and Source 1:1 use the same residency.
    let full_source = cpu.prepare_camera_source(&frame, &recipe, full_options, &token)?;
    let fit_source = full_source.clone().with_recipe(&recipe, fit_options)?;
    let fit_description = cpu.describe_spatial_completion(&fit_source, &token)?;
    let full_description = cpu.describe_spatial_completion(&full_source, &token)?;
    let (resident, upload) = spatial.upload_captured_source(&cpu, &full_source, &token)?;
    assert_eq!(upload.capture.readback_bytes, 0);

    let (fit_spatial, _) = spatial.reduce_preview(&resident, fit_description, &token)?;
    let fit_source = display.adopt_spatial_preview(fit_spatial, &recipe)?;
    let fit_frame = display.render(&fit_source, &recipe, OutputPolicy::ClipToSrgb, None)?;
    let gpu_fit = display.readback_display(&fit_frame)?;
    let cpu_fit = cpu.render_preview(&frame, &recipe, fit_options)?.image;
    let fit_error = display_error("fit", &gpu_fit, &cpu_fit)?;

    let full_source = spatial.full_resolution_source(&resident, full_description)?;
    let full_frame = display.render_spatial_full_cancellable(
        &full_source,
        &recipe,
        OutputPolicy::ClipToSrgb,
        None,
        &token,
    )?;
    let gpu_full = display.readback_display(&full_frame)?;
    let cpu_full = cpu
        .render_source_scale_preview_cancellable(&frame, &recipe, full_options.render, &token)?
        .image;
    let full_error = display_error("Source 1:1", &gpu_full, &cpu_full)?;

    if let Some(destination) = std::env::var_os("ROHDITOR_GPU_SPATIAL_ARTIFACTS") {
        let destination = std::path::PathBuf::from(destination);
        std::fs::create_dir_all(&destination)?;
        save_review_crops(
            &destination,
            "fit-cpu",
            cpu_fit.width(),
            cpu_fit.height(),
            cpu_fit.row_stride(),
            cpu_fit.data(),
        )?;
        save_review_crops(
            &destination,
            "fit-gpu",
            gpu_fit.width as usize,
            gpu_fit.height as usize,
            gpu_fit.width as usize * 4,
            &gpu_fit.rgba,
        )?;
        save_review_crops(
            &destination,
            "source-1-1-cpu",
            cpu_full.width(),
            cpu_full.height(),
            cpu_full.row_stride(),
            cpu_full.data(),
        )?;
        save_review_crops(
            &destination,
            "source-1-1-gpu",
            gpu_full.width as usize,
            gpu_full.height as usize,
            gpu_full.width as usize * 4,
            &gpu_full.rgba,
        )?;
        save_review_crop_at(
            &destination,
            "fit-maximum-cpu",
            cpu_fit.width(),
            cpu_fit.height(),
            cpu_fit.row_stride(),
            cpu_fit.data(),
            fit_error.coordinate,
        )?;
        save_review_crop_at(
            &destination,
            "fit-maximum-gpu",
            gpu_fit.width as usize,
            gpu_fit.height as usize,
            gpu_fit.width as usize * 4,
            &gpu_fit.rgba,
            fit_error.coordinate,
        )?;
        save_review_crop_at(
            &destination,
            "source-1-1-maximum-cpu",
            cpu_full.width(),
            cpu_full.height(),
            cpu_full.row_stride(),
            cpu_full.data(),
            full_error.coordinate,
        )?;
        save_review_crop_at(
            &destination,
            "source-1-1-maximum-gpu",
            gpu_full.width as usize,
            gpu_full.height as usize,
            gpu_full.width as usize * 4,
            &gpu_full.rgba,
            full_error.coordinate,
        )?;
    }
    if fit_error.maximum > FIT_MAX_DISPLAY_CODE_ERROR
        || full_error.maximum > SOURCE_SCALE_MAX_DISPLAY_CODE_ERROR
        || full_error.over_tolerance_samples > SOURCE_SCALE_MAX_OUTLIER_SAMPLES
    {
        return Err(format!(
            "CPU/GPU display error exceeds its real-RAW bound: fit={} ({} samples over {}), Source 1:1={} ({} samples over {}, max {} samples)",
            fit_error.maximum,
            fit_error.over_tolerance_samples,
            FIT_MAX_DISPLAY_CODE_ERROR,
            full_error.maximum,
            full_error.over_tolerance_samples,
            FIT_MAX_DISPLAY_CODE_ERROR,
            SOURCE_SCALE_MAX_OUTLIER_SAMPLES,
        )
        .into());
    }
    Ok(())
}

fn processors() -> (GpuSpatialProcessor, crate::GpuPreviewProcessor) {
    let instance = wgpu::Instance::new(&wgpu::InstanceDescriptor {
        backends: wgpu::Backends::VULKAN,
        ..Default::default()
    });
    let adapter =
        pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions::default()))
            .expect("Vulkan adapter");
    eprintln!("Spatial qualification adapter: {:?}", adapter.get_info());
    let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
        required_limits: wgpu::Limits::default().using_resolution(adapter.limits()),
        ..Default::default()
    }))
    .expect("spatial qualification device");
    let spatial = GpuSpatialProcessor::new(&device, &queue).expect("spatial processor");
    let display =
        crate::GpuPreviewProcessor::new(&adapter, &device, &queue, wgpu::TextureFormat::Rgba8Unorm)
            .expect("display processor");
    (spatial, display)
}

struct DisplayError {
    maximum: u8,
    coordinate: (usize, usize),
    over_tolerance_samples: usize,
}

fn display_error(
    label: &str,
    actual: &crate::GpuDisplayReadback,
    expected: &rohditor_image::DisplayRgbImage<u8>,
) -> Result<DisplayError, Box<dyn Error>> {
    if (actual.width as usize, actual.height as usize) != (expected.width(), expected.height()) {
        return Err(format!("{label} dimensions differ").into());
    }
    let expected_width = expected.width();
    let mut maximum = (0_u8, 0_usize, 0_usize, 0_usize, 0_u8, 0_u8);
    let mut over_tolerance_samples = 0_usize;
    let expected_samples = expected
        .data()
        .chunks(expected.row_stride())
        .flat_map(|row| row[..expected.width() * 3].chunks_exact(3));
    for (index, (actual, expected)) in actual
        .rgba
        .chunks_exact(4)
        .zip(expected_samples)
        .enumerate()
    {
        for channel in 0..3 {
            let difference = actual[channel].abs_diff(expected[channel]);
            if difference > FIT_MAX_DISPLAY_CODE_ERROR {
                over_tolerance_samples += 1;
            }
            if difference > maximum.0 {
                maximum = (
                    difference,
                    index % expected_width,
                    index / expected_width,
                    channel,
                    actual[channel],
                    expected[channel],
                );
            }
        }
    }
    eprintln!(
        "private spatial {label} maximum sRGB code error={} at ({}, {}), channel {}: GPU={} CPU={}, samples above {}={over_tolerance_samples}",
        maximum.0,
        maximum.1,
        maximum.2,
        maximum.3,
        maximum.4,
        maximum.5,
        FIT_MAX_DISPLAY_CODE_ERROR
    );
    Ok(DisplayError {
        maximum: maximum.0,
        coordinate: (maximum.1, maximum.2),
        over_tolerance_samples,
    })
}

fn save_review_crops(
    destination: &std::path::Path,
    label: &str,
    width: usize,
    height: usize,
    row_stride: usize,
    samples: &[u8],
) -> Result<(), Box<dyn Error>> {
    let channels = row_stride / width;
    for (name, left, top) in review_origins(width, height) {
        let edge = 256.min(width).min(height);
        let mut rgb = Vec::with_capacity(edge * edge * 3);
        for y in top..top + edge {
            for x in left..left + edge {
                let offset = y * row_stride + x * channels;
                rgb.extend_from_slice(&samples[offset..offset + 3]);
            }
        }
        image::RgbImage::from_raw(edge as u32, edge as u32, rgb)
            .ok_or("invalid spatial review crop")?
            .save(destination.join(format!("DSC00851-{name}-{label}.png")))?;
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn save_review_crop_at(
    destination: &std::path::Path,
    label: &str,
    width: usize,
    height: usize,
    row_stride: usize,
    samples: &[u8],
    coordinate: (usize, usize),
) -> Result<(), Box<dyn Error>> {
    let edge = 256.min(width).min(height);
    let left = coordinate.0.saturating_sub(edge / 2).min(width - edge);
    let top = coordinate.1.saturating_sub(edge / 2).min(height - edge);
    let channels = row_stride / width;
    let mut rgb = Vec::with_capacity(edge * edge * 3);
    for y in top..top + edge {
        for x in left..left + edge {
            let offset = y * row_stride + x * channels;
            rgb.extend_from_slice(&samples[offset..offset + 3]);
        }
    }
    image::RgbImage::from_raw(edge as u32, edge as u32, rgb)
        .ok_or("invalid spatial maximum-error crop")?
        .save(destination.join(format!("DSC00851-maximum-{label}.png")))?;
    Ok(())
}

fn review_origins(width: usize, height: usize) -> [(&'static str, usize, usize); 5] {
    let edge = 256.min(width).min(height);
    [
        ("top-left", 0, 0),
        ("top-right", width - edge, 0),
        ("bottom-left", 0, height - edge),
        ("bottom-right", width - edge, height - edge),
        ("center", (width - edge) / 2, (height - edge) / 2),
    ]
}
