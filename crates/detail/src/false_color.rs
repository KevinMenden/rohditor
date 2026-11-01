//! False-colour (moiré) suppression: median filtering of the colour
//! differences, as in RawTherapee's "false colour suppression".
//!
//! Fine repeating patterns alias in the Bayer mosaic and leave coloured bands
//! where the scene is neutral. After balancing with the as-shot gains the
//! differences `C1 = r - g` and `C2 = b - g` are median filtered (3x3, once per
//! step) and the image is rebuilt as `r = g + C1`, `b = g + C2`. Green is kept
//! as is: it is sampled twice as densely as red and blue, so it is the most
//! trustworthy channel and carries the luminance detail.

use rayon::prelude::*;
use rohditor_image::LinearRgbImage;

use crate::{CancellationCheck, DetailError, Plane, median3};

/// Colour-difference planes alive at once: `C1`, `C2` and the two outputs of
/// one median pass.
const SCRATCH_PLANES: usize = 4;

/// Parameters of one false-colour suppression pass.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FalseColor {
    /// Number of 3x3 median passes; 0 leaves the image untouched.
    pub steps: u8,
    /// Per-channel gains that make neutral greys have `C1 = C2 = 0`
    /// (as-shot white balance).
    pub balance: [f32; 3],
    /// Per-channel values at or above which a pixel counts as blown and is
    /// left unchanged (the ceilings chroma noise reduction uses).
    pub ceilings: [f32; 3],
}

/// Heap bytes the filter allocates on top of the image itself.
#[must_use]
pub fn false_color_scratch_bytes(width: usize, height: usize) -> Option<usize> {
    width
        .checked_mul(height)?
        .checked_mul(SCRATCH_PLANES * size_of::<f32>())
}

/// Suppress false colour in `image` in place.
///
/// Pixels with a non-finite channel and pixels at or above `ceilings` are
/// copied through (blown pixels still take part in their neighbours' median,
/// which is robust against them). With `steps == 0` the image is not touched.
pub fn suppress_false_color(
    image: &mut LinearRgbImage<f32>,
    params: FalseColor,
    cancellation: &dyn CancellationCheck,
) -> Result<(), DetailError> {
    if params.steps == 0 {
        return Ok(());
    }
    if let Some(&value) = params.balance.iter().find(|v| !v.is_finite() || **v <= 0.0) {
        return Err(DetailError::InvalidParameter {
            name: "channel balance",
            value,
        });
    }
    let (mut c1, mut c2) = colour_differences(image, params.balance)?;
    for _ in 0..params.steps {
        c1 = median3(&c1, cancellation)?;
        c2 = median3(&c2, cancellation)?;
    }
    write_back(image, &c1, &c2, &params, cancellation)
}

/// `r - g` and `b - g` of the balanced image. Non-finite results become 0 so
/// they cannot poison neighbours; such pixels are never written back.
fn colour_differences(
    image: &LinearRgbImage<f32>,
    balance: [f32; 3],
) -> Result<(Plane, Plane), DetailError> {
    let (width, height) = (image.width(), image.height());
    let mut c1 = Plane::filled(width, height, 0.0)?;
    let mut c2 = Plane::filled(width, height, 0.0)?;
    c1.data_mut()
        .par_chunks_mut(width)
        .zip(c2.data_mut().par_chunks_mut(width))
        .enumerate()
        .for_each(|(y, (c1, c2))| {
            let start = y * image.row_stride();
            let pixels = image.data()[start..start + width * 3].as_chunks::<3>().0;
            for ((c1, c2), px) in c1.iter_mut().zip(c2.iter_mut()).zip(pixels) {
                let g = px[1] * balance[1];
                let (d1, d2) = (px[0] * balance[0] - g, px[2] * balance[2] - g);
                *c1 = if d1.is_finite() { d1 } else { 0.0 };
                *c2 = if d2.is_finite() { d2 } else { 0.0 };
            }
        });
    Ok((c1, c2))
}

/// Rebuild red and blue from the filtered differences; green is untouched.
fn write_back(
    image: &mut LinearRgbImage<f32>,
    c1: &Plane,
    c2: &Plane,
    params: &FalseColor,
    cancellation: &dyn CancellationCheck,
) -> Result<(), DetailError> {
    let (w, stride) = (image.width(), image.row_stride());
    let (balance, ceilings) = (params.balance, params.ceilings);
    image
        .data_mut()
        .par_chunks_mut(stride)
        .enumerate()
        .for_each(|(y, row)| {
            if cancellation.is_cancelled() {
                return;
            }
            let (c1, c2) = (
                &c1.data()[y * w..(y + 1) * w],
                &c2.data()[y * w..(y + 1) * w],
            );
            for ((px, &d1), &d2) in row[..w * 3]
                .as_chunks_mut::<3>()
                .0
                .iter_mut()
                .zip(c1)
                .zip(c2)
            {
                let finite = px.iter().all(|v| v.is_finite());
                let blown = px.iter().zip(&ceilings).any(|(v, c)| v >= c);
                if !finite || blown {
                    continue;
                }
                let g = px[1] * balance[1];
                let (r, b) = ((g + d1) / balance[0], (g + d2) / balance[2]);
                if r.is_finite() && b.is_finite() {
                    px[0] = r;
                    px[2] = b;
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
mod tests {
    use super::*;
    use crate::NeverCancelled;
    use rohditor_image::LinearRgbSpace;

    const BALANCE: [f32; 3] = [2.0, 1.0, 1.5];
    const NO_CEILING: [f32; 3] = [f32::INFINITY; 3];

    fn params(steps: u8) -> FalseColor {
        FalseColor {
            steps,
            balance: BALANCE,
            ceilings: NO_CEILING,
        }
    }

    /// Asymmetric, padded-stride image: balanced-neutral grey with a smooth
    /// luma ramp, so any chroma is false colour.
    fn neutral(width: usize, height: usize, stride: usize) -> LinearRgbImage<f32> {
        let mut data = vec![0.0_f32; stride * height];
        for y in 0..height {
            for x in 0..width {
                let g = 0.2 + 0.01 * x as f32 + 0.02 * y as f32;
                let i = y * stride + x * 3;
                data[i..i + 3].copy_from_slice(&[g / BALANCE[0], g / BALANCE[1], g / BALANCE[2]]);
            }
        }
        LinearRgbImage::new(width, height, stride, LinearRgbSpace::CameraNative, data).unwrap()
    }

    #[test]
    fn zero_steps_leave_the_image_bit_identical() {
        let mut image = neutral(9, 7, 31);
        image.data_mut()[4 * 31 + 6] += 0.3;
        let before = image.clone();
        suppress_false_color(&mut image, params(0), &NeverCancelled).unwrap();
        assert_eq!(image, before);
    }

    #[test]
    fn removes_an_isolated_colour_speck_and_keeps_green() {
        let mut image = neutral(9, 7, 31);
        let clean = image.clone();
        // A magenta speck: red and blue up, green untouched.
        let i = 3 * 31 + 4 * 3;
        image.data_mut()[i] += 0.4;
        image.data_mut()[i + 2] += 0.4;
        suppress_false_color(&mut image, params(1), &NeverCancelled).unwrap();
        for y in 0..7 {
            for x in 0..9 {
                let (a, b) = (image.pixel(x, y).unwrap(), clean.pixel(x, y).unwrap());
                for c in 0..3 {
                    // The medians of a linear ramp reproduce it up to rounding.
                    assert!((a[c] - b[c]).abs() < 1e-5, "({x},{y}) c{c}");
                }
            }
        }
    }

    #[test]
    fn keeps_a_real_colour_edge_and_leaves_green_untouched() {
        // Left half neutral, right half red-tinted: a straight colour edge
        // survives a 3x3 median.
        let (w, h) = (12, 6);
        let mut image = neutral(w, h, w * 3);
        for y in 0..h {
            for x in w / 2..w {
                image.data_mut()[y * w * 3 + x * 3] += 0.2;
            }
        }
        let before = image.clone();
        suppress_false_color(&mut image, params(2), &NeverCancelled).unwrap();
        // The rebuild `(g + (r - g)) / gain` only differs by rounding.
        for (a, b) in image.data().iter().zip(before.data()) {
            assert!((a - b).abs() < 1e-5);
        }
    }

    #[test]
    fn green_is_never_modified() {
        let mut image = neutral(10, 8, 33);
        for (i, v) in image.data_mut().iter_mut().enumerate() {
            *v += ((i * 31) % 17) as f32 * 0.01;
        }
        let before = image.clone();
        suppress_false_color(&mut image, params(3), &NeverCancelled).unwrap();
        assert_ne!(image, before);
        for y in 0..8 {
            for x in 0..10 {
                assert_eq!(
                    image.pixel(x, y).unwrap()[1],
                    before.pixel(x, y).unwrap()[1]
                );
            }
        }
    }

    #[test]
    fn blown_and_non_finite_pixels_are_copied_through() {
        let mut image = neutral(9, 7, 27);
        let blown = 2 * 27 + 3 * 3;
        image.data_mut()[blown] += 0.5; // red speck, but at the ceiling
        let non_finite = 4 * 27 + 6 * 3;
        image.data_mut()[non_finite] = f32::NAN;
        image.data_mut()[non_finite + 2] += 0.5;
        let before = image.clone();
        let mut p = params(2);
        p.ceilings = [image.data()[blown], f32::INFINITY, f32::INFINITY];
        suppress_false_color(&mut image, p, &NeverCancelled).unwrap();
        assert_eq!(image.pixel(3, 2), before.pixel(3, 2));
        let (a, b) = (image.pixel(6, 4).unwrap(), before.pixel(6, 4).unwrap());
        assert!(a[0].is_nan() && b[0].is_nan());
        assert_eq!(a[2], b[2]);
        // The NaN must not spread to neighbours.
        assert!(image.data().iter().filter(|v| v.is_nan()).count() == 1);
    }

    #[test]
    fn rejects_invalid_balance_and_honours_cancellation() {
        let mut image = neutral(6, 6, 18);
        let mut bad = params(1);
        bad.balance[1] = 0.0;
        assert!(suppress_false_color(&mut image, bad, &NeverCancelled).is_err());
        assert_eq!(
            suppress_false_color(&mut image, params(1), &|| true),
            Err(DetailError::Cancelled)
        );
    }
}
