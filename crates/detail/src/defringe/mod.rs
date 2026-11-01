//! Defringe: removes purple and green fringes (axial chromatic aberration)
//! next to high-contrast edges.
//!
//! 1. [`edges::edge_mask`] marks pixels near strong luminance edges. Fringes
//!    only exist there, so flat coloured areas are safe.
//! 2. Inside the mask, each pixel gets a weight from how closely its hue
//!    matches a fringe colour, how saturated it is, and the user's amount.
//! 3. Pixels with a weight get their colour differences `C1 = r - g`,
//!    `C2 = b - g` (as-shot balanced) pulled toward the least coloured
//!    non-fringe pixel of their 5x5 neighbourhood, or toward neutral if there
//!    is none. Luma is kept, so no colour is invented and no detail is lost.
//!
//! Everything works on as-shot balanced camera RGB, so user white-balance
//! edits never change the cached result; the flip side is that a large white
//! balance change shifts which hues the final image shows as purple.

mod edges;
mod hue;

pub use edges::{DILATE_RADIUS, EDGE_FRACTION, LOCAL_MAX_RADIUS, NOISE_SIGMAS};
pub use hue::{HUE_ROLLOFF, NEUTRAL_SATURATION, TRUSTED_SATURATION};

use rayon::prelude::*;
use rohditor_image::LinearRgbImage;

use crate::{CancellationCheck, DetailError, NoiseProfile, Plane, for_each_row, luma_plane};
use hue::{hue_and_saturation, hue_window, saturation_ramp};

/// Planes alive at once: luma, gradient and local maximum while the edge mask
/// is built; later the weights and the two replacement planes.
const SCRATCH_PLANES: usize = 3;
/// The replacement colour is searched in a 5x5 window.
pub const NEIGHBOURHOOD_RADIUS: usize = 2;

/// Source pixels defringe reads beyond an output pixel (per side), for tiles
/// processed independently: the edge mask reaches the gradient, local maximum
/// and dilation, and the replacement search reads masks that far again.
pub const DEFRINGE_HALO: usize = 1 + LOCAL_MAX_RADIUS + DILATE_RADIUS + NEIGHBOURHOOD_RADIUS;

/// One fringe colour: how strongly to correct it and which hues it covers.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FringeColour {
    /// Correction strength in `[0, 1]`; 0 leaves this colour alone.
    pub amount: f32,
    /// Hue range in degrees (`[start, end]`, `start < end`, no wrap-around).
    pub hue: [f32; 2],
}

/// Parameters of one defringe pass.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FringeRemoval {
    pub purple: FringeColour,
    pub green: FringeColour,
    /// Per-channel gains that make neutral greys have `C1 = C2 = 0`
    /// (as-shot white balance).
    pub balance: [f32; 3],
    /// Row-major matrix taking balanced camera RGB to linear Rec.2020. Only
    /// used to measure hue, so the hue matches what the user sees.
    pub to_rec2020: [[f32; 3]; 3],
    /// Per-channel values at or above which a pixel counts as blown and is
    /// left unchanged.
    pub ceilings: [f32; 3],
    /// Noise of the balanced luma; edges weaker than a few sigma are ignored.
    /// `None` disables the noise floor.
    pub noise: Option<NoiseProfile>,
}

impl FringeRemoval {
    fn is_active(&self) -> bool {
        self.purple.amount > 0.0 || self.green.amount > 0.0
    }

    fn validate(&self) -> Result<(), DetailError> {
        if let Some(&value) = self.balance.iter().find(|v| !v.is_finite() || **v <= 0.0) {
            return Err(DetailError::InvalidParameter {
                name: "channel balance",
                value,
            });
        }
        if let Some(&value) = self.to_rec2020.iter().flatten().find(|v| !v.is_finite()) {
            return Err(DetailError::InvalidParameter {
                name: "colour matrix",
                value,
            });
        }
        for colour in [self.purple, self.green] {
            if !(0.0..=1.0).contains(&colour.amount) {
                return Err(DetailError::InvalidParameter {
                    name: "fringe amount",
                    value: colour.amount,
                });
            }
            let [start, end] = colour.hue;
            if !start.is_finite() || !end.is_finite() || start >= end {
                return Err(DetailError::InvalidParameter {
                    name: "fringe hue range",
                    value: if start.is_finite() { end } else { start },
                });
            }
        }
        Ok(())
    }

    /// Fringe weight of one pixel, 0 for blown and non-finite pixels.
    fn pixel_weight(&self, px: &[f32]) -> f32 {
        if px.iter().any(|v| !v.is_finite()) || px.iter().zip(&self.ceilings).any(|(v, c)| v >= c) {
            return 0.0;
        }
        let balanced: [f32; 3] = std::array::from_fn(|c| px[c] * self.balance[c]);
        let rgb = self
            .to_rec2020
            .map(|row| row[0] * balanced[0] + row[1] * balanced[1] + row[2] * balanced[2]);
        let Some((hue, saturation)) = hue_and_saturation(rgb) else {
            return 0.0;
        };
        let membership = (self.purple.amount * hue_window(hue, self.purple.hue))
            .max(self.green.amount * hue_window(hue, self.green.hue));
        membership * saturation_ramp(saturation)
    }
}

/// Heap bytes the filter allocates on top of the image itself.
#[must_use]
pub fn defringe_scratch_bytes(width: usize, height: usize) -> Option<usize> {
    width
        .checked_mul(height)?
        .checked_mul(SCRATCH_PLANES * size_of::<f32>())
}

/// Remove purple and green fringes from `image` in place.
///
/// Pixels with a non-finite channel and pixels at or above `ceilings` are
/// copied through, so clipped highlights never gain colour. With both amounts
/// at 0 the image is not touched.
pub fn remove_fringes(
    image: &mut LinearRgbImage<f32>,
    params: FringeRemoval,
    cancellation: &dyn CancellationCheck,
) -> Result<(), DetailError> {
    params.validate()?;
    if !params.is_active() {
        return Ok(());
    }
    let weights = fringe_weights(image, &params, cancellation)?;
    let (c1, c2) = replacement_chroma(image, &weights, &params, cancellation)?;
    blend_replacement(image, &weights, &c1, &c2, &params, cancellation)
}

/// Per-pixel correction strength: non-zero only near strong edges, where the
/// pixel's hue is a fringe colour.
fn fringe_weights(
    image: &LinearRgbImage<f32>,
    params: &FringeRemoval,
    cancellation: &dyn CancellationCheck,
) -> Result<Plane, DetailError> {
    let luma = luma_plane(image, params.balance)?;
    let mut weights = edges::edge_mask(&luma, params.noise.as_ref(), cancellation)?;
    drop(luma);
    let width = image.width();
    for_each_row(weights.data_mut(), width, cancellation, |y, row| {
        let start = y * image.row_stride();
        let pixels = image.data()[start..start + width * 3].as_chunks::<3>().0;
        for (weight, px) in row.iter_mut().zip(pixels) {
            if *weight > 0.0 {
                *weight = params.pixel_weight(px);
            }
        }
    })?;
    Ok(weights)
}

/// Balanced colour differences `(r - g, b - g)`, if the pixel is finite.
fn colour_differences(px: &[f32; 3], balance: [f32; 3]) -> Option<(f32, f32)> {
    let g = px[1] * balance[1];
    let (c1, c2) = (px[0] * balance[0] - g, px[2] * balance[2] - g);
    (c1.is_finite() && c2.is_finite()).then_some((c1, c2))
}

/// For every pixel with a weight, the colour differences of the least
/// coloured unweighted, unblown pixel within [`NEIGHBOURHOOD_RADIUS`], or `(0, 0)`
/// (neutral) when there is none. Entries of unweighted pixels are unused.
///
/// Reads only the untouched image, so the result does not depend on the
/// order pixels are visited in.
fn replacement_chroma(
    image: &LinearRgbImage<f32>,
    weights: &Plane,
    params: &FringeRemoval,
    cancellation: &dyn CancellationCheck,
) -> Result<(Plane, Plane), DetailError> {
    let (width, height) = (image.width(), image.height());
    let mut c1 = Plane::filled(width, height, 0.0)?;
    let mut c2 = Plane::filled(width, height, 0.0)?;
    c1.data_mut()
        .par_chunks_mut(width)
        .zip(c2.data_mut().par_chunks_mut(width))
        .enumerate()
        .for_each(|(y, (c1_row, c2_row))| {
            if cancellation.is_cancelled() {
                return;
            }
            let weight_row = &weights.data()[y * width..(y + 1) * width];
            let rows =
                y.saturating_sub(NEIGHBOURHOOD_RADIUS)..=(y + NEIGHBOURHOOD_RADIUS).min(height - 1);
            for x in (0..width).filter(|&x| weight_row[x] > 0.0) {
                let columns = x.saturating_sub(NEIGHBOURHOOD_RADIUS)
                    ..=(x + NEIGHBOURHOOD_RADIUS).min(width - 1);
                let mut best: Option<(f32, f32, f32)> = None; // (|C|², C1, C2)
                for ny in rows.clone() {
                    for nx in columns.clone() {
                        if weights.get(nx, ny) > 0.0 {
                            continue;
                        }
                        let start = ny * image.row_stride() + nx * 3;
                        let Some(px) = image.data()[start..].first_chunk::<3>() else {
                            continue;
                        };
                        // Blown pixels carry clipped, unreliable colour.
                        if px.iter().zip(&params.ceilings).any(|(v, c)| v >= c) {
                            continue;
                        }
                        let Some((d1, d2)) = colour_differences(px, params.balance) else {
                            continue;
                        };
                        let energy = d1 * d1 + d2 * d2;
                        if best.is_none_or(|(least, ..)| energy < least) {
                            best = Some((energy, d1, d2));
                        }
                    }
                }
                let (_, d1, d2) = best.unwrap_or((0.0, 0.0, 0.0));
                c1_row[x] = d1;
                c2_row[x] = d2;
            }
        });
    if cancellation.is_cancelled() {
        return Err(DetailError::Cancelled);
    }
    Ok((c1, c2))
}

/// Move each weighted pixel's colour differences toward its replacement by
/// its weight, keeping the balanced luma `Y = g + (C1 + C2) / 4` exactly.
fn blend_replacement(
    image: &mut LinearRgbImage<f32>,
    weights: &Plane,
    c1: &Plane,
    c2: &Plane,
    params: &FringeRemoval,
    cancellation: &dyn CancellationCheck,
) -> Result<(), DetailError> {
    let (width, stride) = (image.width(), image.row_stride());
    let balance = params.balance;
    image
        .data_mut()
        .par_chunks_mut(stride)
        .enumerate()
        .for_each(|(y, row)| {
            if cancellation.is_cancelled() {
                return;
            }
            let span = y * width..(y + 1) * width;
            let (weights, c1, c2) = (
                &weights.data()[span.clone()],
                &c1.data()[span.clone()],
                &c2.data()[span],
            );
            for (((px, &weight), &new1), &new2) in row[..width * 3]
                .as_chunks_mut::<3>()
                .0
                .iter_mut()
                .zip(weights)
                .zip(c1)
                .zip(c2)
            {
                if weight <= 0.0 {
                    continue;
                }
                let Some((old1, old2)) = colour_differences(px, balance) else {
                    continue;
                };
                let (r, g, b) = (px[0] * balance[0], px[1] * balance[1], px[2] * balance[2]);
                let luma = (r + 2.0 * g + b) * 0.25;
                let (d1, d2) = (old1 + weight * (new1 - old1), old2 + weight * (new2 - old2));
                let g = luma - (d1 + d2) * 0.25;
                let rebuilt = [(g + d1) / balance[0], g / balance[1], (g + d2) / balance[2]];
                if rebuilt.iter().all(|v| v.is_finite()) {
                    px.copy_from_slice(&rebuilt);
                }
            }
        });
    if cancellation.is_cancelled() {
        Err(DetailError::Cancelled)
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests;
