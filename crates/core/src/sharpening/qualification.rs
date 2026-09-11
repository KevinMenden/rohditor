//! Optional private-corpus comparison artifacts; USM exists only as a test baseline.
use std::error::Error;
use std::path::Path;

use rohditor_edit::EditRecipe;
use rohditor_image::Orientation;
use rohditor_raw::{RawDecoder, RawlerDecoder};
use serde::Deserialize;

use super::*;
use crate::{CpuPipeline, OutputPolicy, PreviewOptions};

#[derive(Deserialize)]
struct Manifest {
    crops: Vec<Crop>,
}
#[derive(Deserialize)]
struct Crop {
    source: String,
    name: String,
    x: usize,
    y: usize,
    width: usize,
    height: usize,
}

#[test]
#[ignore = "capture sharpening comparison on the private Sony corpus; optional ROHDITOR_CAPTURE_ARTIFACTS directory"]
fn private_capture_comparison() -> Result<(), Box<dyn Error>> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../testdata");
    let manifest: Manifest =
        serde_json::from_slice(&std::fs::read(root.join("phase-9-quality-crops.json"))?)?;
    let destination = std::env::var_os("ROHDITOR_CAPTURE_ARTIFACTS").map(std::path::PathBuf::from);
    if let Some(path) = &destination {
        std::fs::create_dir_all(path)?;
    }
    let mut sources: Vec<_> = manifest.crops.iter().map(|c| c.source.as_str()).collect();
    sources.sort_unstable();
    sources.dedup();
    let decoder = RawlerDecoder::default();
    let token = CancellationToken::new();
    for name in sources {
        let frame = decoder.decode(&root.join("private").join(name))?;
        let mut recipe = EditRecipe::default();
        recipe.geometry.orientation_override = Some(Orientation::Normal);
        let source = CpuPipeline::default().prepare_preview_reconstruction(
            &frame,
            &recipe,
            PreviewOptions {
                max_long_edge: usize::MAX,
                ..PreviewOptions::default()
            },
        )?;
        let resolved = crate::resolve_camera_colour(
            source.calibration(),
            &recipe.color.camera_profile,
            recipe.color.white_balance,
        )?;
        let gains = resolved.white_balance_gains;
        let ceiling =
            recipe.raw.highlights.clip.threshold * gains.red.min(gains.green).min(gains.blue);
        let levels = [
            ceiling / gains.red,
            ceiling / gains.green,
            ceiling / gains.blue,
        ];
        for method in ["off", "usm", "rl"] {
            let mut linear = source.image().clone();
            let settings = CaptureSharpening {
                enabled: true,
                ..CaptureSharpening::default()
            };
            let started = std::time::Instant::now();
            match method {
                "rl" => apply_cancellable(&mut linear, settings, levels, &token)?,
                "usm" => apply_usm_baseline(&mut linear, &token)?,
                _ => {}
            }
            eprintln!(
                "capture corpus {name} {method}: {:.1} ms",
                started.elapsed().as_secs_f64() * 1000.0
            );
            assert!(linear.data().iter().all(|v| v.is_finite()));
            crate::cpu::apply_white_balance_cancellable(&mut linear, gains, &token)?;
            crate::cpu::apply_camera_color_transform_cancellable(
                &mut linear,
                &resolved.camera_color_transform(),
                &token,
            )?;
            crate::apply_adjustments(&mut linear, &recipe)?;
            let display = crate::render_display_srgb8(
                &linear,
                Orientation::Normal,
                OutputPolicy::ClipToSrgb,
            )?;
            drop(linear);
            if method == "rl" {
                let mut active = recipe.clone();
                active.capture_sharpening = settings;
                let pipeline_output = CpuPipeline::default().render(
                    &frame,
                    &active,
                    crate::RenderOptions::default(),
                )?;
                assert_eq!(
                    display.data(),
                    pipeline_output.image.data(),
                    "camera stage and full pipeline diverged for {name}"
                );
            }
            if let Some(path) = &destination {
                for crop in manifest.crops.iter().filter(|c| c.source == name) {
                    let mut pixels = Vec::with_capacity(crop.width * crop.height * 3);
                    for y in crop.y..crop.y + crop.height {
                        let start = y * display.row_stride() + crop.x * 3;
                        pixels.extend_from_slice(&display.data()[start..start + crop.width * 3]);
                    }
                    let buffer =
                        image::RgbImage::from_raw(crop.width as u32, crop.height as u32, pixels)
                            .ok_or("invalid crop")?;
                    buffer.save(path.join(format!("{}-{method}-100.png", crop.name)))?;
                    image::imageops::resize(
                        &buffer,
                        crop.width as u32 * 2,
                        crop.height as u32 * 2,
                        image::imageops::FilterType::Nearest,
                    )
                    .save(path.join(format!("{}-{method}-200.png", crop.name)))?;
                }
            }
        }
    }
    Ok(())
}

fn apply_usm_baseline(
    image: &mut LinearRgbImage<f32>,
    token: &CancellationToken,
) -> Result<(), PipelineError> {
    let width = image.width();
    let guide: Vec<f32> = image
        .data()
        .chunks_exact(3)
        .map(|rgb| rgb.iter().map(|v| v.max(0.0) / 3.0).sum::<f32>().max(FLOOR))
        .collect();
    let mut lowpass = vec![0.0; guide.len()];
    blur(
        &guide,
        &mut vec![0.0; guide.len()],
        &mut lowpass,
        width,
        &gaussian(0.6),
        token,
    )?;
    for ((rgb, g), b) in image.data_mut().chunks_exact_mut(3).zip(guide).zip(lowpass) {
        let gain = ((g + 0.5 * (g - b)) / g).clamp(0.5, 2.0);
        for c in rgb {
            *c *= gain;
        }
    }
    Ok(())
}
