//! Full-source camera parity and saved Source 1:1 crops. Software adapters are
//! deliberately identified separately from hardware qualification.
use super::*;
use rohditor_core::{OutputPolicy, PreviewOptions};
use rohditor_edit::EditRecipe;
use rohditor_raw::{RawDecoder, RawlerDecoder};
use std::error::Error;

#[test]
#[ignore = "private Sony RAWs and Vulkan; optional ROHDITOR_GPU_CAPTURE_ARTIFACTS directory"]
fn private_capture_camera_parity_and_visual_crops() -> Result<(), Box<dyn Error>> {
    let mut gpu = super::tests::processor();
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../testdata");
    let manifest: serde_json::Value =
        serde_json::from_slice(&std::fs::read(root.join("phase-9-quality-crops.json"))?)?;
    let crops = manifest["crops"].as_array().ok_or("missing crops")?;
    let mut names: Vec<_> = crops.iter().filter_map(|c| c["source"].as_str()).collect();
    names.sort_unstable();
    names.dedup();
    let destination =
        std::env::var_os("ROHDITOR_GPU_CAPTURE_ARTIFACTS").map(std::path::PathBuf::from);
    if let Some(path) = &destination {
        std::fs::create_dir_all(path)?;
    }
    let cpu = CpuPipeline::default();
    let token = CancellationToken::new();
    for name in names {
        let frame = RawlerDecoder::default().decode(&root.join("private").join(name))?;
        let mut recipe = EditRecipe::default();
        recipe.geometry.orientation_override = Some(rohditor_image::Orientation::Normal);
        recipe.capture_sharpening.enabled = true;
        let options = PreviewOptions {
            max_long_edge: usize::MAX,
            ..Default::default()
        };
        let source = cpu.prepare_camera_source(&frame, &recipe, options, &token)?;
        for noise in [0.5, 0.0] {
            recipe.capture_sharpening.noise_protection = noise;
            let source = source.clone().with_recipe(&recipe, options)?;
            let reference =
                cpu.complete_camera_source(source.clone().capture_cpu(&token)?, &token)?;
            let (actual, metrics) = gpu.prepare(&cpu, source, &token)?;
            let mut max_error = 0.0_f32;
            for (i, (a, b)) in reference
                .image()
                .data()
                .iter()
                .zip(actual.image().data())
                .enumerate()
            {
                let difference = (a - b).abs();
                if difference > 2e-5 + 2e-5 * a.abs() {
                    return Err(format!("{name} noise={noise} sample={i}: CPU={a} GPU={b}").into());
                }
                max_error = max_error.max(difference);
            }
            eprintln!(
                "capture corpus {name} {}x{} noise={noise}: max camera error={max_error:e}, bridge={:?}, tiles={} edge={} halo={}, upload={} readback={}, estimated GPU={} host scratch={}",
                actual.image().width(),
                actual.image().height(),
                metrics.total,
                metrics.tiles,
                metrics.tile_edge,
                metrics.halo,
                metrics.uploaded_bytes,
                metrics.readback_bytes,
                metrics.estimated_gpu_bytes,
                metrics.estimated_host_bytes
            );
            if let Some(path) = &destination {
                for (label, prepared) in [("cpu-on", reference), ("gpu-on", actual)] {
                    save_crops(
                        &cpu,
                        prepared,
                        &recipe,
                        crops,
                        name,
                        path,
                        &format!("{label}-noise-{noise}"),
                        &token,
                    )?;
                }
            }
        }
        if let Some(path) = &destination {
            recipe.capture_sharpening.enabled = false;
            let off = cpu.complete_camera_source(
                source.with_recipe(&recipe, options)?.capture_cpu(&token)?,
                &token,
            )?;
            save_crops(&cpu, off, &recipe, crops, name, path, "off", &token)?;
        }
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn save_crops(
    cpu: &CpuPipeline,
    prepared: ReconstructedPreview,
    recipe: &EditRecipe,
    crops: &[serde_json::Value],
    name: &str,
    path: &std::path::Path,
    label: &str,
    token: &CancellationToken,
) -> Result<(), Box<dyn Error>> {
    let base =
        cpu.prepare_preview_base_from_reconstruction_cancellable(&prepared, recipe, token)?;
    drop(prepared);
    let display = cpu
        .render_preview_from_base_reusing_cancellable(
            &base,
            recipe,
            OutputPolicy::ClipToSrgb,
            &mut rohditor_core::CpuPreviewWorkspace::default(),
            token,
        )?
        .image;
    for crop in crops.iter().filter(|c| c["source"].as_str() == Some(name)) {
        let number = |field: &str| {
            crop[field]
                .as_u64()
                .map(|v| v as usize)
                .ok_or("invalid crop")
        };
        let (x, y, w, h) = (
            number("x")?,
            number("y")?,
            number("width")?,
            number("height")?,
        );
        let mut pixels = Vec::with_capacity(w * h * 3);
        for row in y..y + h {
            pixels
                .extend_from_slice(&display.data()[row * display.row_stride() + x * 3..][..w * 3]);
        }
        image::RgbImage::from_raw(w as u32, h as u32, pixels)
            .ok_or("invalid crop image")?
            .save(path.join(format!(
                "{}-{label}.png",
                crop["name"].as_str().ok_or("crop name")?
            )))?;
    }
    Ok(())
}
