//! `detail-compare`: score a detail stage on the synthetic ground-truth set.
//!
//! Every scene is rendered at the clean level and at three ISO-like noise
//! levels, the stage under test is applied, and the result is scored against
//! the clean truth. Later detail work (noise reduction, false-colour
//! suppression, defringe, output sharpening) adds its stage here, so changes
//! are judged by numbers instead of screenshots.

use std::fmt::Write as _;
use std::fs;
use std::path::Path;

use anyhow::{Context, Result};
use clap::ValueEnum;
use rohditor_detail::{
    ChromaDenoise, NeverCancelled, denoise_chroma, estimate_noise, split_opponent,
};
use rohditor_image::LinearRgbImage;
use rohditor_reference::detail_scenes::{self, DetailScene, EDGE_PERIOD, Scene};
use rohditor_reference::metrics::{detail_scores, edge_rise_distance};
use rohditor_reference::noise::{NOISE_LEVELS, NoiseModel};

use super::commands::write_stdout;

/// Scene size: wide enough for the noise estimate's 16x16 tiles.
const WIDTH: usize = 768;
const HEIGHT: usize = 512;
/// Pixels excluded on every side (filter borders, stage halos).
const MARGIN: usize = 16;

/// Stage under test. Each detail issue adds a variant.
#[derive(Debug, Clone, Copy, Default, ValueEnum)]
pub(super) enum DetailStage {
    /// Returns the input unchanged: the baseline every stage must beat.
    #[default]
    Noop,
    /// Chroma noise reduction at `--strength`.
    ChromaNr,
}

impl DetailStage {
    fn apply(self, input: &LinearRgbImage<f32>, strength: f32) -> Result<LinearRgbImage<f32>> {
        match self {
            Self::Noop => Ok(input.clone()),
            Self::ChromaNr => {
                let mut output = input.clone();
                denoise_chroma(
                    &mut output,
                    ChromaDenoise {
                        strength,
                        balance: [1.0; 3],
                        ceilings: [f32::INFINITY; 3],
                    },
                    &NeverCancelled,
                )?;
                Ok(output)
            }
        }
    }
}

/// One scored (scene, noise level) pair.
struct Row {
    scene: String,
    level: &'static str,
    luma_psnr_db: f64,
    chroma_rmse: f64,
    colour_added: f64,
    /// Fringed-edge scene only.
    edge_rise_px: Option<f64>,
    /// Gradient scene only: estimated / true sigma at mid grey.
    sigma_ratio: Option<f64>,
}

pub(super) fn detail_compare(
    stage: DetailStage,
    strength: f32,
    raw_dir: Option<&Path>,
) -> Result<()> {
    let mut scenes = detail_scenes::synthetic_set(WIDTH, HEIGHT)?;
    if let Some(dir) = raw_dir {
        scenes.extend(load_raw_scenes(dir)?);
    }

    let levels = std::iter::once(("clean", None)).chain(
        NOISE_LEVELS
            .iter()
            .map(|(name, model)| (*name, Some(*model))),
    );
    let levels: Vec<_> = levels.collect();

    let mut rows = Vec::new();
    for (scene_index, scene) in scenes.iter().enumerate() {
        for (level_index, (level, noise)) in levels.iter().enumerate() {
            let seed = (scene_index * 16 + level_index) as u64 + 1;
            rows.push(score(stage, strength, scene, level, *noise, seed)?);
        }
    }
    write_stdout(&format_table(stage, strength, &rows))
}

fn score(
    stage: DetailStage,
    strength: f32,
    scene: &Scene,
    level: &'static str,
    noise: Option<NoiseModel>,
    seed: u64,
) -> Result<Row> {
    let input = scene.noisy_input(noise, seed)?;
    let output = stage.apply(&input, strength)?;
    let scores = detail_scores(&scene.truth, &output, MARGIN);

    let edge_rise_px = (scene.kind == Some(DetailScene::FringedEdges))
        .then(|| {
            let edge = EDGE_PERIOD / 2;
            edge_rise_distance(&output, edge - 12, edge + 12)
        })
        .flatten();

    // The noise estimate is read from the stage output, so a denoiser shows
    // up as a ratio below one.
    let sigma_ratio = match (scene.kind, noise) {
        (Some(DetailScene::Gradient), Some(model)) => {
            let luma = split_opponent(&output, [1.0; 3])?.luma;
            let profile = estimate_noise(&luma, &luma, &NeverCancelled)?;
            // Luma averages four samples (weights 1, 2, 1) of the channel noise.
            let expected = model.sigma(0.5) * (6.0_f32).sqrt() / 4.0;
            Some(f64::from(profile.sigma(0.5) / expected))
        }
        _ => None,
    };

    Ok(Row {
        scene: scene.name.clone(),
        level,
        luma_psnr_db: scores.luma_psnr_db,
        chroma_rmse: scores.chroma_rmse,
        colour_added: scores.colour_added,
        edge_rise_px,
        sigma_ratio,
    })
}

/// Decode every readable file in `dir` as a real-RAW scene; others are skipped.
fn load_raw_scenes(dir: &Path) -> Result<Vec<Scene>> {
    let mut paths: Vec<_> = fs::read_dir(dir)
        .with_context(|| format!("could not read RAW directory {}", dir.display()))?
        .filter_map(|entry| entry.ok().map(|e| e.path()))
        .filter(|path| path.is_file())
        .collect();
    paths.sort();
    let mut scenes = Vec::new();
    for path in paths {
        match detail_scenes::raw_scene(&path, WIDTH, HEIGHT) {
            Ok(scene) => scenes.push(scene),
            Err(error) => eprintln!("skipping {}: {error}", path.display()),
        }
    }
    Ok(scenes)
}

fn format_table(stage: DetailStage, strength: f32, rows: &[Row]) -> String {
    let optional = |value: Option<f64>, precision: usize| {
        value.map_or_else(|| "-".to_owned(), |v| format!("{v:.precision$}"))
    };
    let mut out = format!(
        "detail-compare stage={stage:?} strength={strength} size={WIDTH}x{HEIGHT} margin={MARGIN}\n\
         {:<16} {:<9} {:>10} {:>11} {:>12} {:>9} {:>9}\n",
        "scene", "noise", "luma PSNR", "chroma RMSE", "colour added", "rise px", "σ ratio"
    );
    for row in rows {
        let _ = writeln!(
            out,
            "{:<16} {:<9} {:>10} {:>11.5} {:>12.5} {:>9} {:>9}",
            row.scene,
            row.level,
            format!("{:.2}", row.luma_psnr_db),
            row.chroma_rmse,
            row.colour_added,
            optional(row.edge_rise_px, 2),
            optional(row.sigma_ratio, 2),
        );
    }
    out.truncate(out.trim_end().len());
    out
}
