//! Dehaze, after the dark channel prior (He, Sun & Tang, 2009).
//!
//! Haze is modelled as `I = J * t + A * (1 - t)`: the scene `J` is attenuated by the
//! transmission `t` and veiled by the atmospheric light `A`. In a haze-free outdoor patch at
//! least one colour channel is close to black, so the darkest channel of a hazy patch measures
//! how much veil it carries. The transmission is estimated at a reduced resolution, refined
//! with a guided filter so it follows the picture's edges, and inverted per pixel.
//!
//! Negative amounts run the model forwards instead: a uniform veil of `A` is added.

use rayon::prelude::*;
use rohditor_image::LinearRgbImage;

use super::blur::box_mean;
use super::local_contrast::encode;
use crate::{CancellationCheck, DetailError, Plane};

/// Long edge of the reduced image the transmission is estimated on.
const ESTIMATE_LONG_EDGE: usize = 320;
/// Fraction of the brightest dark-channel pixels that define the atmospheric light.
const AIRLIGHT_FRACTION: f32 = 0.002;
/// Darkest an atmospheric light channel may be, so a night scene does not divide by ~0.
const AIRLIGHT_FLOOR: f32 = 0.05;
/// Share of the measured haze that is removed at full strength; some is kept so the result
/// still looks like air.
const KEPT_HAZE: f32 = 0.05;
/// Lowest transmission the inversion divides by: caps the contrast stretch at 4x.
const MIN_TRANSMISSION: f32 = 0.25;
/// Radius of the dark-channel patch on the reduced image, in pixels.
const PATCH_RADIUS: usize = 1;
/// Guided-filter window radius on the reduced image, in pixels.
const GUIDE_RADIUS: usize = 4;
/// Guided-filter regularisation in encoded-luma units squared: lower follows edges more.
const GUIDE_EPSILON: f32 = 1.0e-3;
/// Transmission of the veil added by a dehaze of -1.
const FULL_VEIL_TRANSMISSION: f32 = 0.4;

/// A reduced copy of the image: per-channel block means and block minima.
struct Reduced {
    width: usize,
    height: usize,
    factor: usize,
    mean: [Vec<f32>; 3],
    minimum: [Vec<f32>; 3],
}

/// Remove (`amount > 0`) or add (`amount < 0`) haze, in `-1..=1`.
pub(super) fn apply_dehaze(
    image: &mut LinearRgbImage<f32>,
    luma_weights: [f32; 3],
    amount: f32,
    cancellation: &dyn CancellationCheck,
) -> Result<(), DetailError> {
    let reduced = reduce(image, cancellation)?;
    let airlight = estimate_airlight(&reduced);
    if amount < 0.0 {
        add_veil(image, airlight, -amount);
        return check(cancellation);
    }
    let transmission = guided_transmission(&reduced, airlight, luma_weights, cancellation)?;
    remove_veil(
        image,
        &reduced,
        &transmission,
        airlight,
        luma_weights,
        amount,
    );
    check(cancellation)
}

fn check(cancellation: &dyn CancellationCheck) -> Result<(), DetailError> {
    if cancellation.is_cancelled() {
        Err(DetailError::Cancelled)
    } else {
        Ok(())
    }
}

/// Block-reduce the image so that its long edge is at most [`ESTIMATE_LONG_EDGE`].
fn reduce(
    image: &LinearRgbImage<f32>,
    cancellation: &dyn CancellationCheck,
) -> Result<Reduced, DetailError> {
    let (width, height, stride) = (image.width(), image.height(), image.row_stride());
    let factor = width.max(height).div_ceil(ESTIMATE_LONG_EDGE).max(1);
    let (reduced_width, reduced_height) = (width.div_ceil(factor), height.div_ceil(factor));
    let cells = reduced_width * reduced_height;
    // One reduced row per task, writing [mean r, g, b, min r, g, b] per cell.
    let mut packed = vec![0.0_f32; cells * 6];
    packed
        .par_chunks_mut(reduced_width * 6)
        .enumerate()
        .for_each(|(cell_y, out)| {
            if cancellation.is_cancelled() {
                return;
            }
            let rows = cell_y * factor..((cell_y + 1) * factor).min(height);
            for (cell_x, out) in out.as_chunks_mut::<6>().0.iter_mut().enumerate() {
                let columns = cell_x * factor..((cell_x + 1) * factor).min(width);
                let mut sum = [0.0_f32; 3];
                let mut minimum = [f32::MAX; 3];
                let count = (rows.len() * columns.len()) as f32;
                for y in rows.clone() {
                    let row = &image.data()[y * stride..y * stride + width * 3];
                    for pixel in row[columns.start * 3..columns.end * 3].as_chunks::<3>().0 {
                        for c in 0..3 {
                            // Negative or non-finite samples count as black.
                            let value = if pixel[c].is_finite() {
                                pixel[c].max(0.0)
                            } else {
                                0.0
                            };
                            sum[c] += value;
                            minimum[c] = minimum[c].min(value);
                        }
                    }
                }
                for c in 0..3 {
                    out[c] = sum[c] / count;
                    out[3 + c] = minimum[c];
                }
            }
        });
    check(cancellation)?;
    let unpack = |offset: usize| -> [Vec<f32>; 3] {
        std::array::from_fn(|c| {
            packed
                .as_chunks::<6>()
                .0
                .iter()
                .map(|cell| cell[offset + c])
                .collect()
        })
    };
    Ok(Reduced {
        width: reduced_width,
        height: reduced_height,
        factor,
        mean: unpack(0),
        minimum: unpack(3),
    })
}

/// The atmospheric light: the mean colour of the cells whose darkest channel is highest, i.e.
/// the regions that look most like pure haze.
fn estimate_airlight(reduced: &Reduced) -> [f32; 3] {
    let darkest: Vec<f32> = (0..reduced.mean[0].len())
        .map(|i| {
            reduced
                .minimum
                .iter()
                .map(|c| c[i])
                .fold(f32::MAX, f32::min)
        })
        .collect();
    let count = ((darkest.len() as f32 * AIRLIGHT_FRACTION) as usize).max(1);
    let mut sorted = darkest.clone();
    let (_, &mut threshold, _) =
        sorted.select_nth_unstable_by(darkest.len() - count, f32::total_cmp);
    let mut sum = [0.0_f32; 3];
    let mut selected = 0.0_f32;
    for (i, &dark) in darkest.iter().enumerate() {
        if dark >= threshold {
            for (sum, mean) in sum.iter_mut().zip(&reduced.mean) {
                *sum += mean[i];
            }
            selected += 1.0;
        }
    }
    sum.map(|value| (value / selected).max(AIRLIGHT_FLOOR))
}

/// Transmission on the reduced grid, as the guided-filter coefficients `(a, b)` such that
/// `t = a * guide + b`, with the guide being encoded luma.
struct GuidedTransmission {
    a: Plane,
    b: Plane,
}

fn guided_transmission(
    reduced: &Reduced,
    airlight: [f32; 3],
    luma_weights: [f32; 3],
    cancellation: &dyn CancellationCheck,
) -> Result<GuidedTransmission, DetailError> {
    let (width, height) = (reduced.width, reduced.height);
    let cells = width * height;
    // Darkest channel relative to the airlight, then the darkest value in a small patch.
    let relative: Vec<f32> = (0..cells)
        .map(|i| {
            (0..3)
                .map(|c| reduced.minimum[c][i] / airlight[c])
                .fold(f32::MAX, f32::min)
        })
        .collect();
    let dark = patch_minimum(&relative, width, height, PATCH_RADIUS);
    let rough: Vec<f32> = dark
        .iter()
        .map(|dark| (1.0 - (1.0 - KEPT_HAZE) * dark).clamp(0.0, 1.0))
        .collect();
    let guide: Vec<f32> = (0..cells)
        .map(|i| {
            let luma: f32 = (0..3).map(|c| luma_weights[c] * reduced.mean[c][i]).sum();
            encode(luma)
        })
        .collect();

    let guide = Plane::new(width, height, guide)?;
    let rough = Plane::new(width, height, rough)?;
    let mean = |plane: &Plane| box_mean(plane, GUIDE_RADIUS, cancellation);
    let product = |left: &Plane, right: &Plane| {
        let data = left
            .data()
            .iter()
            .zip(right.data())
            .map(|(l, r)| l * r)
            .collect();
        Plane::new(width, height, data)
    };
    let mean_guide = mean(&guide)?;
    let mean_rough = mean(&rough)?;
    let mean_guide_squared = mean(&product(&guide, &guide)?)?;
    let mean_cross = mean(&product(&guide, &rough)?)?;
    let mut a = vec![0.0_f32; cells];
    let mut b = vec![0.0_f32; cells];
    for i in 0..cells {
        let (mg, mr) = (mean_guide.data()[i], mean_rough.data()[i]);
        let variance = mean_guide_squared.data()[i] - mg * mg;
        let covariance = mean_cross.data()[i] - mg * mr;
        a[i] = covariance / (variance.max(0.0) + GUIDE_EPSILON);
        b[i] = mr - a[i] * mg;
    }
    Ok(GuidedTransmission {
        a: mean(&Plane::new(width, height, a)?)?,
        b: mean(&Plane::new(width, height, b)?)?,
    })
}

/// Minimum over a `(2 * radius + 1)` square window, with the window clipped at the borders.
fn patch_minimum(data: &[f32], width: usize, height: usize, radius: usize) -> Vec<f32> {
    let smallest = |values: &mut dyn Iterator<Item = f32>| values.fold(f32::MAX, f32::min);
    let mut horizontal = vec![0.0_f32; data.len()];
    for y in 0..height {
        for x in 0..width {
            let columns = x.saturating_sub(radius)..=(x + radius).min(width - 1);
            horizontal[y * width + x] = smallest(&mut columns.map(|k| data[y * width + k]));
        }
    }
    let mut out = vec![0.0_f32; data.len()];
    for y in 0..height {
        let rows = y.saturating_sub(radius)..=(y + radius).min(height - 1);
        for x in 0..width {
            out[y * width + x] = smallest(&mut rows.clone().map(|k| horizontal[k * width + x]));
        }
    }
    out
}

/// Divide the veil out of every pixel: `J = (I - A) / t + A`.
fn remove_veil(
    image: &mut LinearRgbImage<f32>,
    reduced: &Reduced,
    transmission: &GuidedTransmission,
    airlight: [f32; 3],
    luma_weights: [f32; 3],
    amount: f32,
) {
    let (width, stride) = (image.width(), image.row_stride());
    image
        .data_mut()
        .par_chunks_mut(stride)
        .enumerate()
        .for_each(|(y, row)| {
            for (x, pixel) in row[..width * 3]
                .as_chunks_mut::<3>()
                .0
                .iter_mut()
                .enumerate()
            {
                let luma: f32 = (0..3).map(|c| luma_weights[c] * pixel[c].max(0.0)).sum();
                let guide = encode(luma);
                let a = sample(&transmission.a, reduced, x, y);
                let b = sample(&transmission.b, reduced, x, y);
                let measured = (a * guide + b).clamp(0.0, 1.0);
                let t = (1.0 - amount * (1.0 - measured)).max(MIN_TRANSMISSION);
                for (value, light) in pixel.iter_mut().zip(airlight) {
                    // Keep out-of-gamut negatives as they were rather than deepening them.
                    *value = ((*value - light) / t + light).max(value.min(0.0));
                }
            }
        });
}

/// Mix a uniform veil of the atmospheric light into every pixel.
fn add_veil(image: &mut LinearRgbImage<f32>, airlight: [f32; 3], amount: f32) {
    let t = 1.0 - amount * (1.0 - FULL_VEIL_TRANSMISSION);
    let (width, stride) = (image.width(), image.row_stride());
    image.data_mut().par_chunks_mut(stride).for_each(|row| {
        for pixel in row[..width * 3].as_chunks_mut::<3>().0 {
            for (value, light) in pixel.iter_mut().zip(airlight) {
                *value = *value * t + light * (1.0 - t);
            }
        }
    });
}

/// Bilinear sample of a reduced-grid plane at the centre of full-resolution pixel `(x, y)`.
fn sample(plane: &Plane, reduced: &Reduced, x: usize, y: usize) -> f32 {
    let factor = reduced.factor as f32;
    let position = |pixel: usize, size: usize| {
        (((pixel as f32 + 0.5) / factor) - 0.5).clamp(0.0, (size - 1) as f32)
    };
    let (fx, fy) = (position(x, reduced.width), position(y, reduced.height));
    let (x0, y0) = (fx.floor() as usize, fy.floor() as usize);
    let (x1, y1) = (
        (x0 + 1).min(reduced.width - 1),
        (y0 + 1).min(reduced.height - 1),
    );
    let (tx, ty) = (fx - x0 as f32, fy - y0 as f32);
    let top = plane.get(x0, y0) * (1.0 - tx) + plane.get(x1, y0) * tx;
    let bottom = plane.get(x0, y1) * (1.0 - tx) + plane.get(x1, y1) * tx;
    top * (1.0 - ty) + bottom * ty
}
