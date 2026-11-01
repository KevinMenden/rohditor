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
use rohditor_demosaic::DemosaicAlgorithm;
use rohditor_detail::{
    ChromaDenoise, FalseColor, LumaDenoise, NeverCancelled, denoise_chroma, denoise_luma,
    estimate_luma_noise, estimate_noise, gaussian_blur, luma_plane, split_opponent,
    suppress_false_color,
};
use rohditor_image::LinearRgbImage;
use rohditor_reference::detail_scenes::{self, DetailScene, EDGE_PERIOD, Scene};
use rohditor_reference::metrics::{detail_scores, edge_acutance, edge_rise_distance};
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
    /// Luminance noise reduction at `--strength` and `--detail`.
    LumaNr,
    /// False-colour suppression with `--steps` median passes.
    FalseColor,
    /// Experiment: false-colour suppression applied only where the luma
    /// high-pass is strong (blend weight reaches 1 at [`MASK_FULL`]).
    FalseColorMasked,
}

/// Knobs shared by the stages; each stage reads the ones it has.
#[derive(Debug, Clone, Copy)]
pub(super) struct StageSettings {
    pub strength: f32,
    pub detail: f32,
    /// Median passes of the false-colour stages.
    pub steps: u8,
    /// Demosaic the scenes go through before the stage (the stage input).
    pub demosaic: DemosaicAlgorithm,
}

/// Luma high-pass (input minus a [`MASK_SIGMA`] blur) at which the masked
/// false-colour experiment applies the full filter.
const MASK_FULL: f32 = 0.05;
const MASK_SIGMA: f32 = 1.5;

impl DetailStage {
    fn apply(
        self,
        input: &LinearRgbImage<f32>,
        settings: StageSettings,
    ) -> Result<LinearRgbImage<f32>> {
        const BALANCE: [f32; 3] = [1.0; 3];
        const CEILINGS: [f32; 3] = [f32::INFINITY; 3];
        let mut output = input.clone();
        match self {
            Self::Noop => {}
            Self::FalseColor | Self::FalseColorMasked => {
                suppress_false_color(
                    &mut output,
                    FalseColor {
                        steps: settings.steps,
                        balance: BALANCE,
                        ceilings: CEILINGS,
                    },
                    &NeverCancelled,
                )?;
                if matches!(self, Self::FalseColorMasked) {
                    blend_by_luma_highpass(input, &mut output)?;
                }
            }
            Self::ChromaNr | Self::LumaNr => {
                // Too small to profile: the stage leaves the image alone.
                let Some(noise) = estimate_luma_noise(input, BALANCE, &NeverCancelled)? else {
                    return Ok(output);
                };
                if matches!(self, Self::ChromaNr) {
                    denoise_chroma(
                        &mut output,
                        ChromaDenoise {
                            strength: settings.strength,
                            balance: BALANCE,
                            ceilings: CEILINGS,
                            luma_noise: noise,
                        },
                        &NeverCancelled,
                    )?;
                } else {
                    denoise_luma(
                        &mut output,
                        LumaDenoise {
                            strength: settings.strength,
                            detail: settings.detail,
                            balance: BALANCE,
                            ceilings: CEILINGS,
                            noise,
                        },
                        &NeverCancelled,
                    )?;
                }
            }
        }
        Ok(output)
    }
}

/// Replace `filtered` by `input` where the luma high-pass is weak, with a
/// linear ramp up to [`MASK_FULL`]. Both images share one layout.
fn blend_by_luma_highpass(
    input: &LinearRgbImage<f32>,
    filtered: &mut LinearRgbImage<f32>,
) -> Result<()> {
    let luma = luma_plane(input, [1.0; 3])?;
    let blurred = gaussian_blur(&luma, MASK_SIGMA, &NeverCancelled)?;
    let (width, stride) = (input.width(), input.row_stride());
    for y in 0..input.height() {
        for x in 0..width {
            let weight = ((luma.get(x, y) - blurred.get(x, y)).abs() / MASK_FULL).min(1.0);
            let at = y * stride + x * 3;
            for c in 0..3 {
                let original = input.data()[at + c];
                filtered.data_mut()[at + c] =
                    original + weight * (filtered.data()[at + c] - original);
            }
        }
    }
    Ok(())
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
    /// Fringed-edge scene only: see [`edge_acutance`].
    edge_acutance: Option<f64>,
    /// Gradient scene only: estimated / true sigma at mid grey.
    sigma_ratio: Option<f64>,
}

pub(super) fn detail_compare(
    stage: DetailStage,
    settings: StageSettings,
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
            rows.push(score(stage, settings, scene, level, *noise, seed)?);
        }
    }
    write_stdout(&format_table(stage, settings, &rows))
}

fn score(
    stage: DetailStage,
    settings: StageSettings,
    scene: &Scene,
    level: &'static str,
    noise: Option<NoiseModel>,
    seed: u64,
) -> Result<Row> {
    let input = scene.noisy_input(noise, seed, settings.demosaic)?;
    let output = stage.apply(&input, settings)?;
    let scores = detail_scores(&scene.truth, &output, MARGIN);

    let (edge_rise_px, edge_acutance) = if scene.kind == Some(DetailScene::FringedEdges) {
        let (from, to) = (EDGE_PERIOD / 2 - 12, EDGE_PERIOD / 2 + 12);
        (
            edge_rise_distance(&output, from, to),
            edge_acutance(&output, from, to),
        )
    } else {
        (None, None)
    };

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
        edge_acutance,
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

fn format_table(stage: DetailStage, settings: StageSettings, rows: &[Row]) -> String {
    let optional = |value: Option<f64>, precision: usize| {
        value.map_or_else(|| "-".to_owned(), |v| format!("{v:.precision$}"))
    };
    let mut out = format!(
        "detail-compare stage={stage:?} strength={} detail={} steps={} demosaic={:?} size={WIDTH}x{HEIGHT} margin={MARGIN}\n\
         {:<16} {:<9} {:>10} {:>11} {:>12} {:>9} {:>9} {:>9}\n",
        settings.strength,
        settings.detail,
        settings.steps,
        settings.demosaic,
        "scene",
        "noise",
        "luma PSNR",
        "chroma RMSE",
        "colour added",
        "rise px",
        "acutance",
        "σ ratio"
    );
    for row in rows {
        let _ = writeln!(
            out,
            "{:<16} {:<9} {:>10} {:>11.5} {:>12.5} {:>9} {:>9} {:>9}",
            row.scene,
            row.level,
            format!("{:.2}", row.luma_psnr_db),
            row.chroma_rmse,
            row.colour_added,
            optional(row.edge_rise_px, 2),
            optional(row.edge_acutance, 3),
            optional(row.sigma_ratio, 2),
        );
    }
    out.truncate(out.trim_end().len());
    out
}
