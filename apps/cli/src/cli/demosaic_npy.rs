//! `demosaic-npy`: Rohditor's classic demosaic (and optional noise reduction)
//! on contract-scaled `.npy` mosaics, as the baseline for the neural demosaic
//! evaluation (`ml/rohditor_ml/eval`).
//!
//! Input follows `ml/contract.json`: an RGGB mosaic, black-subtracted and
//! divided by `white - black`, not white balanced, `[H, W]` or a stack
//! `[N, H, W]`, plus as-shot gains `[3]` or `[N, 3]`. The output is linear RGB
//! in the balanced space the network produces: `[3, H, W]` or `[N, 3, H, W]`.
//!
//! Each image runs through the same stages as the pipeline: demosaic with the
//! gains as channel balance, then chroma and luminance noise reduction on
//! camera-native RGB (profiled on the image itself, with the default Clip
//! highlight ceilings), and finally the white balance.

use std::path::Path;
use std::sync::Arc;

use anyhow::{Context, Result, bail};
use rohditor_core::write_output_bytes;
use rohditor_demosaic::{DemosaicAlgorithm, WhiteBalanceGains, demosaic};
use rohditor_detail::{
    ChromaDenoise, LumaDenoise, NeverCancelled, denoise_chroma, denoise_luma,
    estimate_noise_profiles,
};
use rohditor_edit::{EditRecipe, NoiseReduction};
use rohditor_image::{BayerPattern, LinearRgbImage, MosaicImage};
use rohditor_neural::NeuralDemosaicer;

use super::npy::{encode_f32, read_f32};

pub(super) struct DemosaicNpy<'a> {
    pub input: &'a Path,
    pub wb: &'a Path,
    pub output: &'a Path,
    pub algorithm: DemosaicAlgorithm,
    /// Loaded model, required for [`DemosaicAlgorithm::Neural`].
    pub neural: Option<Arc<NeuralDemosaicer>>,
    pub noise_reduction: NoiseReduction,
    pub force: bool,
}

pub(super) fn demosaic_npy(args: &DemosaicNpy<'_>) -> Result<()> {
    args.noise_reduction.validate()?;
    let mosaic = read_f32(args.input)?;
    let gains = read_f32(args.wb)?;
    let (count, height, width) = match mosaic.shape[..] {
        [h, w] => (1, h, w),
        [n, h, w] => (n, h, w),
        _ => bail!("mosaic must be [H, W] or [N, H, W], got {:?}", mosaic.shape),
    };
    let per_image_gains = match gains.shape[..] {
        [3] => false,
        [n, 3] if n == count => true,
        _ => bail!("gains must be [3] or [{count}, 3], got {:?}", gains.shape),
    };
    if height % 2 != 0 || width % 2 != 0 {
        bail!("RGGB mosaics need even dimensions, got {width}x{height}");
    }

    let pixels = width * height;
    let mut output = Vec::with_capacity(count * 3 * pixels);
    for index in 0..count {
        let g = if per_image_gains {
            &gains.data[index * 3..index * 3 + 3]
        } else {
            &gains.data[..3]
        };
        let gains = WhiteBalanceGains {
            red: g[0],
            green: g[1],
            blue: g[2],
        };
        let samples = mosaic.data[index * pixels..(index + 1) * pixels].to_vec();
        let image = MosaicImage::new(width, height, width, BayerPattern::Rggb, samples)?;
        let rgb = reconstruct(
            &image,
            gains,
            args.algorithm,
            args.neural.as_deref(),
            args.noise_reduction,
        )
        .with_context(|| format!("image {index}"))?;
        append_planar_balanced(&rgb, gains, &mut output);
    }
    let shape = if mosaic.shape.len() == 2 {
        vec![3, height, width]
    } else {
        vec![count, 3, height, width]
    };
    write_output_bytes(args.output, &encode_f32(&shape, &output), args.force)?;
    Ok(())
}

/// Demosaic plus the active noise-reduction steps, in camera-native RGB.
fn reconstruct(
    mosaic: &MosaicImage<f32>,
    gains: WhiteBalanceGains,
    algorithm: DemosaicAlgorithm,
    neural: Option<&NeuralDemosaicer>,
    settings: NoiseReduction,
) -> Result<LinearRgbImage<f32>> {
    let mut rgb = match (algorithm, neural) {
        (DemosaicAlgorithm::Neural, Some(neural)) => {
            neural.reconstruct(mosaic, [gains.red, gains.green, gains.blue], &|| false)?
        }
        (DemosaicAlgorithm::Neural, None) => bail!("the neural demosaic needs a loaded model"),
        _ => demosaic(mosaic, gains, algorithm)?,
    };
    if !settings.is_active() {
        return Ok(rgb);
    }
    let balance = [gains.red, gains.green, gains.blue];
    let ceilings = clip_ceilings(gains);
    // Too small to profile: the pipeline leaves such images alone too.
    let Some(noise) = estimate_noise_profiles(&rgb, balance, &NeverCancelled)? else {
        return Ok(rgb);
    };
    if settings.chroma_active() {
        let strength = settings.chroma;
        denoise_chroma(
            &mut rgb,
            ChromaDenoise {
                strength,
                balance,
                ceilings,
                noise,
            },
            &NeverCancelled,
        )?;
    }
    if settings.luminance_active() {
        let (strength, detail) = (settings.luminance, settings.luminance_detail);
        let noise = noise.luma;
        denoise_luma(
            &mut rgb,
            LumaDenoise {
                strength,
                detail,
                balance,
                ceilings,
                noise,
            },
            &NeverCancelled,
        )?;
    }
    Ok(rgb)
}

/// Camera-native highlight ceilings of the default recipe (Clip at its default threshold).
fn clip_ceilings(gains: WhiteBalanceGains) -> [f32; 3] {
    let threshold = EditRecipe::default().raw.highlights.clip.threshold;
    let ceiling = threshold * gains.red.min(gains.green).min(gains.blue);
    [
        ceiling / gains.red,
        ceiling / gains.green,
        ceiling / gains.blue,
    ]
}

/// Append `rgb` as `[3, H, W]` planes, multiplied by the white-balance gains.
fn append_planar_balanced(
    rgb: &LinearRgbImage<f32>,
    gains: WhiteBalanceGains,
    output: &mut Vec<f32>,
) {
    let (width, stride) = (rgb.width(), rgb.row_stride());
    for (channel, gain) in [gains.red, gains.green, gains.blue].into_iter().enumerate() {
        for row in rgb.data().chunks(stride).take(rgb.height()) {
            output.extend(
                row[..width * 3]
                    .as_chunks::<3>()
                    .0
                    .iter()
                    .map(|px| px[channel] * gain),
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::npy::NpyArray;

    fn run(
        name: &str,
        mosaic: NpyArray,
        gains: NpyArray,
        algorithm: DemosaicAlgorithm,
    ) -> NpyArray {
        let dir = std::env::temp_dir().join(format!(
            "rohditor-demosaic-npy-{name}-{}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).expect("temp dir");
        let (input, wb, output) = (dir.join("m.npy"), dir.join("wb.npy"), dir.join("out.npy"));
        std::fs::write(&input, encode_f32(&mosaic.shape, &mosaic.data)).expect("write");
        std::fs::write(&wb, encode_f32(&gains.shape, &gains.data)).expect("write");
        demosaic_npy(&DemosaicNpy {
            input: &input,
            wb: &wb,
            output: &output,
            algorithm,
            neural: None,
            noise_reduction: NoiseReduction::default(),
            force: true,
        })
        .expect("demosaic-npy");
        let result = read_f32(&output).expect("read");
        std::fs::remove_dir_all(&dir).ok();
        result
    }

    #[test]
    fn flat_grey_scene_comes_back_balanced_per_image() {
        // A grey scene seen through a sensor whose red and blue read 1/2 and 1/4 of green.
        let (h, w) = (8, 10);
        let mut data = Vec::new();
        for scale in [0.4f32, 0.2] {
            for y in 0..h {
                for x in 0..w {
                    let factor = match (y % 2, x % 2) {
                        (0, 0) => 0.5,
                        (1, 1) => 0.25,
                        _ => 1.0,
                    };
                    data.push(scale * factor);
                }
            }
        }
        let mosaic = NpyArray {
            shape: vec![2, h, w],
            data,
        };
        let gains = NpyArray {
            shape: vec![2, 3],
            data: vec![2.0, 1.0, 4.0, 2.0, 1.0, 4.0],
        };
        let out = run("grey", mosaic, gains, DemosaicAlgorithm::MalvarHeCutler);
        assert_eq!(out.shape, vec![2, 3, h, w]);
        for (i, value) in out.data.iter().enumerate() {
            let expected = if i < 3 * h * w { 0.4 } else { 0.2 };
            assert!((value - expected).abs() < 1e-5, "sample {i}: {value}");
        }
    }

    #[test]
    fn red_stays_red() {
        // Only the R photosites (even row, even column) see light.
        let (h, w) = (8, 8);
        let data = (0..h * w)
            .map(|i| {
                if (i / w) % 2 == 0 && (i % w) % 2 == 0 {
                    0.5
                } else {
                    0.0
                }
            })
            .collect();
        let out = run(
            "red",
            NpyArray {
                shape: vec![h, w],
                data,
            },
            NpyArray {
                shape: vec![3],
                data: vec![1.0, 1.0, 1.0],
            },
            DemosaicAlgorithm::Bilinear,
        );
        let plane = |c: usize| &out.data[c * h * w..(c + 1) * h * w];
        assert!(plane(0).iter().all(|&v| v > 0.4));
        assert!(plane(2).iter().all(|&v| v.abs() < 1e-6));
    }
}
