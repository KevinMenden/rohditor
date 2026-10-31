//! Synthetic scenes with known ground truth for scoring detail stages.
//!
//! Each [`DetailScene`] provides a clean `truth` and the `input` a stage
//! should improve. Sensor noise and the mosaic round trip are applied by the
//! caller (see [`crate::noise`] and [`Scene::through_sensor`]).

use std::path::Path;

use rohditor_demosaic::{DemosaicAlgorithm, WhiteBalanceGains, demosaic};
use rohditor_image::{BayerPattern, LinearRgbImage, LinearRgbSpace};

use crate::ReferenceError;
use crate::noise::{NoiseModel, add_noise};
use crate::truth::{downsample2, load_ground_truth, remosaic};

/// Width in pixels of each synthetic fringe band.
pub const FRINGE_WIDTH: usize = 3;
/// Period of the stripe pattern in the fringed-edge scene.
pub const EDGE_PERIOD: usize = 64;
/// Dark and bright levels of the edge scenes.
pub const EDGE_LOW: f32 = 0.03;
pub const EDGE_HIGH: f32 = 0.9;

/// Built-in synthetic scenes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DetailScene {
    /// Neutral chirp: any chroma after demosaicing is false colour.
    ZonePlate,
    /// Black/white vertical edges; the input carries purple/green fringes.
    FringedEdges,
    /// Neutral brightness ramp for noise estimation.
    Gradient,
    /// Vertical bars of decreasing period, for detail loss.
    ResolutionBars,
}

impl DetailScene {
    pub const ALL: [Self; 4] = [
        Self::ZonePlate,
        Self::FringedEdges,
        Self::Gradient,
        Self::ResolutionBars,
    ];

    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::ZonePlate => "zone-plate",
            Self::FringedEdges => "fringed-edges",
            Self::Gradient => "gradient",
            Self::ResolutionBars => "resolution-bars",
        }
    }

    fn truth_value(self, x: usize, y: usize, width: usize, height: usize) -> f32 {
        match self {
            Self::ZonePlate => {
                let (dx, dy) = (
                    x as f32 - width as f32 / 2.0,
                    y as f32 - height as f32 / 2.0,
                );
                // Instantaneous frequency reaches ~0.4 cycles/pixel at 512 px radius.
                let k = std::f32::consts::PI * 0.4 / 512.0;
                0.5 + 0.35 * (k * (dx * dx + dy * dy)).cos()
            }
            Self::FringedEdges => {
                if x % EDGE_PERIOD < EDGE_PERIOD / 2 {
                    EDGE_LOW
                } else {
                    EDGE_HIGH
                }
            }
            Self::Gradient => 0.03 + 0.94 * x as f32 / (width.max(2) - 1) as f32,
            Self::ResolutionBars => {
                // Five blocks of bars with half-periods 8, 4, 3, 2, 1 pixels.
                const HALF_PERIODS: [usize; 5] = [8, 4, 3, 2, 1];
                let block = (y * HALF_PERIODS.len() / height.max(1)).min(HALF_PERIODS.len() - 1);
                let half = HALF_PERIODS[block];
                if (x / half).is_multiple_of(2) {
                    0.2
                } else {
                    0.8
                }
            }
        }
    }

    /// Noise-free, neutral (equal channels) ground truth.
    pub fn truth(self, width: usize, height: usize) -> Result<LinearRgbImage<f32>, ReferenceError> {
        let mut data = Vec::with_capacity(width * height * 3);
        for y in 0..height {
            for x in 0..width {
                data.extend([self.truth_value(x, y, width, height); 3]);
            }
        }
        Ok(LinearRgbImage::new(
            width,
            height,
            width * 3,
            LinearRgbSpace::CameraNative,
            data,
        )?)
    }

    /// The clean image a stage should repair: the truth, except for
    /// [`Self::FringedEdges`], which gets purple fringes of [`FRINGE_WIDTH`]
    /// pixels on the dark side of each edge and green ones on the bright side.
    pub fn input(self, width: usize, height: usize) -> Result<LinearRgbImage<f32>, ReferenceError> {
        let mut image = self.truth(width, height)?;
        if self == Self::FringedEdges {
            for y in 0..height {
                for x in 0..width {
                    let half = EDGE_PERIOD / 2;
                    let phase = x % EDGE_PERIOD;
                    let dark_side = phase < half;
                    // Pixels to the nearest edge on this side of the stripe.
                    let to_edge = (phase % half).min(half - 1 - phase % half);
                    if to_edge >= FRINGE_WIDTH {
                        continue;
                    }
                    let offset = if dark_side {
                        [0.25, -0.02, 0.25] // purple
                    } else {
                        [-0.12, 0.0, -0.12] // green
                    };
                    let start = y * image.row_stride() + x * 3;
                    for (v, o) in image.data_mut()[start..start + 3].iter_mut().zip(offset) {
                        *v = (*v + o).max(0.0);
                    }
                }
            }
        }
        Ok(image)
    }
}

/// One entry of the harness set: ground truth plus the stage input.
#[derive(Debug, Clone)]
pub struct Scene {
    pub name: String,
    pub kind: Option<DetailScene>,
    pub truth: LinearRgbImage<f32>,
    pub input: LinearRgbImage<f32>,
}

impl Scene {
    /// Whether the input should go through remosaic + demosaic (so false
    /// colour and correlated noise appear) after noise injection.
    #[must_use]
    pub fn through_sensor(&self) -> bool {
        self.kind != Some(DetailScene::Gradient)
    }

    /// The stage input at one noise level: noise is added in sensor space,
    /// then (where meaningful) the frame is re-mosaiced and demosaiced.
    pub fn noisy_input(
        &self,
        noise: Option<NoiseModel>,
        seed: u64,
    ) -> Result<LinearRgbImage<f32>, ReferenceError> {
        let noisy = match noise {
            Some(model) => add_noise(&self.input, model, seed)?,
            None => self.input.clone(),
        };
        if !self.through_sensor() {
            return Ok(noisy);
        }
        let mosaic = remosaic(&noisy, BayerPattern::Rggb)?;
        demosaic(
            &mosaic,
            WhiteBalanceGains::identity(),
            DemosaicAlgorithm::Bilinear,
        )
        .map_err(|e| ReferenceError::Other(e.to_string()))
    }
}

/// The built-in synthetic set at `width` x `height`.
pub fn synthetic_set(width: usize, height: usize) -> Result<Vec<Scene>, ReferenceError> {
    DetailScene::ALL
        .into_iter()
        .map(|kind| {
            Ok(Scene {
                name: kind.name().to_owned(),
                kind: Some(kind),
                truth: kind.truth(width, height)?,
                input: kind.input(width, height)?,
            })
        })
        .collect()
}

/// A real-RAW scene: the binned frame is both truth and input (its own noise
/// stays in the truth, so scores measure change rather than absolute quality).
pub fn raw_scene(path: &Path, width: usize, height: usize) -> Result<Scene, ReferenceError> {
    // Binning 2x2 then halving again averages sixteen sensor samples per pixel.
    let truth = downsample2(&load_ground_truth(path, width * 2, height * 2)?)?;
    Ok(Scene {
        name: path
            .file_stem()
            .map_or_else(|| "raw".into(), |s| s.to_string_lossy().into_owned()),
        kind: None,
        input: truth.clone(),
        truth,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fringed_input_differs_from_truth_only_near_edges() {
        let (w, h) = (2 * EDGE_PERIOD + 10, 5);
        let truth = DetailScene::FringedEdges.truth(w, h).unwrap();
        let input = DetailScene::FringedEdges.input(w, h).unwrap();
        let changed: Vec<usize> = (0..w)
            .filter(|&x| truth.pixel(x, 2) != input.pixel(x, 2))
            .collect();
        assert!(!changed.is_empty());
        // Every changed column is within FRINGE_WIDTH of an edge.
        for x in changed {
            let phase = x % (EDGE_PERIOD / 2);
            let to_edge = phase.min(EDGE_PERIOD / 2 - 1 - phase);
            assert!(to_edge < FRINGE_WIDTH, "x={x}");
        }
        // Fringes are chromatic: purple has R and B above G.
        let purple = input.pixel(EDGE_PERIOD / 2 - 1, 0).unwrap();
        assert!(purple[0] > purple[1] && purple[2] > purple[1]);
    }

    #[test]
    fn zone_plate_is_neutral_but_demosaicing_adds_colour() {
        let scene = &synthetic_set(256, 192).unwrap()[0];
        let truth = &scene.truth;
        assert!(truth.data().chunks(3).all(|p| p[0] == p[1] && p[1] == p[2]));
        let seen = scene.noisy_input(None, 0).unwrap();
        let max_chroma = seen
            .data()
            .chunks(3)
            .map(|p| (p[0] - p[1]).abs())
            .fold(0.0_f32, f32::max);
        assert!(max_chroma > 0.05, "no false colour: {max_chroma}");
    }
}
