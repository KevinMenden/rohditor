//! Single-channel planes and the luma/opponent split used by chroma filters.

use rayon::prelude::*;
use rohditor_image::{LinearRgbImage, LinearRgbSpace};

use crate::DetailError;

/// A tightly packed single-channel `f32` image.
#[derive(Debug, Clone, PartialEq)]
pub struct Plane {
    width: usize,
    height: usize,
    data: Vec<f32>,
}

impl Plane {
    /// Wrap `data`, checking that it is exactly `width * height` samples.
    pub fn new(width: usize, height: usize, data: Vec<f32>) -> Result<Self, DetailError> {
        let expected = checked_len(width, height)?;
        if data.len() != expected {
            return Err(DetailError::DataLength {
                width,
                height,
                expected,
                actual: data.len(),
            });
        }
        Ok(Self {
            width,
            height,
            data,
        })
    }

    /// A plane of `value` with checked size arithmetic.
    pub fn filled(width: usize, height: usize, value: f32) -> Result<Self, DetailError> {
        Self::new(width, height, vec![value; checked_len(width, height)?])
    }

    #[must_use]
    pub const fn width(&self) -> usize {
        self.width
    }

    #[must_use]
    pub const fn height(&self) -> usize {
        self.height
    }

    #[must_use]
    pub fn data(&self) -> &[f32] {
        &self.data
    }

    pub fn data_mut(&mut self) -> &mut [f32] {
        &mut self.data
    }

    #[must_use]
    pub fn into_data(self) -> Vec<f32> {
        self.data
    }

    #[must_use]
    pub fn get(&self, x: usize, y: usize) -> f32 {
        self.data[y * self.width + x]
    }

    pub(crate) fn same_size(&self, other: &Self) -> Result<(), DetailError> {
        if (self.width, self.height) == (other.width, other.height) {
            Ok(())
        } else {
            Err(DetailError::DimensionMismatch)
        }
    }
}

fn checked_len(width: usize, height: usize) -> Result<usize, DetailError> {
    match width.checked_mul(height) {
        Some(len) if len > 0 && len.checked_mul(size_of::<f32>()).is_some() => Ok(len),
        _ => Err(DetailError::InvalidDimensions { width, height }),
    }
}

/// Luma and two colour-difference planes of a balanced RGB image.
#[derive(Debug, Clone, PartialEq)]
pub struct OpponentPlanes {
    /// `(r + 2g + b) / 4`
    pub luma: Plane,
    /// `r - g`
    pub c1: Plane,
    /// `b - g`
    pub c2: Plane,
}

fn validate_balance(balance: [f32; 3]) -> Result<(), DetailError> {
    match balance.iter().find(|v| !v.is_finite() || **v <= 0.0) {
        Some(&value) => Err(DetailError::InvalidParameter {
            name: "channel balance",
            value,
        }),
        None => Ok(()),
    }
}

/// Balanced luma `(r + 2g + b) / 4` alone (the `Y` of [`split_opponent`]), with
/// non-finite results replaced by 0 so they cannot poison neighbours.
pub fn luma_plane(image: &LinearRgbImage<f32>, balance: [f32; 3]) -> Result<Plane, DetailError> {
    validate_balance(balance)?;
    let (width, height) = (image.width(), image.height());
    let mut y = vec![0.0_f32; checked_len(width, height)?];
    y.par_chunks_mut(width).enumerate().for_each(|(row, y)| {
        let start = row * image.row_stride();
        let pixels = image.data()[start..start + width * 3].as_chunks::<3>().0;
        for (y, px) in y.iter_mut().zip(pixels) {
            let luma = (px[0] * balance[0] + 2.0 * px[1] * balance[1] + px[2] * balance[2]) * 0.25;
            *y = if luma.is_finite() { luma } else { 0.0 };
        }
    });
    Plane::new(width, height, y)
}

/// Split into opponent planes after multiplying each channel by `balance`
/// (e.g. white-balance gains, so neutral greys have `C1 = C2 = 0`).
pub fn split_opponent(
    image: &LinearRgbImage<f32>,
    balance: [f32; 3],
) -> Result<OpponentPlanes, DetailError> {
    validate_balance(balance)?;
    let (width, height) = (image.width(), image.height());
    let len = checked_len(width, height)?;
    let (mut y, mut c1, mut c2) = (vec![0.0_f32; len], vec![0.0_f32; len], vec![0.0_f32; len]);
    y.par_chunks_mut(width)
        .zip(c1.par_chunks_mut(width))
        .zip(c2.par_chunks_mut(width))
        .enumerate()
        .for_each(|(row, ((y, c1), c2))| {
            let start = row * image.row_stride();
            let pixels = image.data()[start..start + width * 3].as_chunks::<3>().0;
            for (x, px) in pixels.iter().enumerate() {
                let (r, g, b) = (px[0] * balance[0], px[1] * balance[1], px[2] * balance[2]);
                y[x] = (r + 2.0 * g + b) * 0.25;
                c1[x] = r - g;
                c2[x] = b - g;
            }
        });
    Ok(OpponentPlanes {
        luma: Plane::new(width, height, y)?,
        c1: Plane::new(width, height, c1)?,
        c2: Plane::new(width, height, c2)?,
    })
}

/// Exact inverse of [`split_opponent`] for the same `balance`.
pub fn merge_opponent(
    planes: &OpponentPlanes,
    balance: [f32; 3],
    space: LinearRgbSpace,
) -> Result<LinearRgbImage<f32>, DetailError> {
    validate_balance(balance)?;
    planes.luma.same_size(&planes.c1)?;
    planes.luma.same_size(&planes.c2)?;
    let (width, height) = (planes.luma.width(), planes.luma.height());
    let mut data = Vec::with_capacity(planes.luma.data().len() * 3);
    for ((&y, &c1), &c2) in planes
        .luma
        .data()
        .iter()
        .zip(planes.c1.data())
        .zip(planes.c2.data())
    {
        // Y = g + (C1 + C2) / 4, so g is recoverable from the three planes.
        let g = y - (c1 + c2) * 0.25;
        data.extend_from_slice(&[(g + c1) / balance[0], g / balance[1], (g + c2) / balance[2]]);
    }
    Ok(LinearRgbImage::new(width, height, width * 3, space, data)?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plane_rejects_wrong_length_and_empty() {
        assert!(Plane::new(3, 2, vec![0.0; 5]).is_err());
        assert!(Plane::new(0, 2, vec![]).is_err());
        assert!(Plane::filled(usize::MAX, 2, 0.0).is_err());
    }

    #[test]
    fn split_merge_round_trips_with_padded_stride() {
        // Asymmetric size, padded stride, non-trivial balance.
        let (w, h, stride) = (5, 3, 17);
        let data: Vec<f32> = (0..stride * h)
            .map(|i| (i * 13 % 29) as f32 / 29.0)
            .collect();
        let image = LinearRgbImage::new(w, h, stride, LinearRgbSpace::CameraNative, data).unwrap();
        let balance = [2.1, 1.0, 1.6];
        let planes = split_opponent(&image, balance).unwrap();
        let back = merge_opponent(&planes, balance, LinearRgbSpace::CameraNative).unwrap();
        for y in 0..h {
            for x in 0..w {
                let (a, b) = (image.pixel(x, y).unwrap(), back.pixel(x, y).unwrap());
                for c in 0..3 {
                    assert!((a[c] - b[c]).abs() < 1e-5, "({x},{y}) c{c}");
                }
            }
        }
    }

    #[test]
    fn luma_plane_matches_the_split_and_zeroes_non_finite_pixels() {
        let (w, h, stride) = (4, 3, 14);
        let data: Vec<f32> = (0..stride * h)
            .map(|i| (i * 7 % 23) as f32 / 23.0)
            .collect();
        let mut image =
            LinearRgbImage::new(w, h, stride, LinearRgbSpace::CameraNative, data).unwrap();
        let balance = [2.1, 1.0, 1.6];
        let split = split_opponent(&image, balance).unwrap().luma;
        assert_eq!(luma_plane(&image, balance).unwrap(), split);
        image.data_mut()[stride + 3] = f32::NAN;
        let plane = luma_plane(&image, balance).unwrap();
        assert_eq!(plane.get(1, 1), 0.0);
        assert!(plane.data().iter().all(|v| v.is_finite()));
    }

    #[test]
    fn balanced_grey_has_no_chroma() {
        let balance = [2.0, 1.0, 1.5];
        let data = [0.25, 0.5, 1.0 / 3.0].repeat(4);
        let image = LinearRgbImage::new(2, 2, 6, LinearRgbSpace::CameraNative, data).unwrap();
        let planes = split_opponent(&image, balance).unwrap();
        assert!(planes.c1.data().iter().all(|v| v.abs() < 1e-6));
        assert!(planes.c2.data().iter().all(|v| v.abs() < 1e-6));
        assert!(planes.luma.data().iter().all(|v| (v - 0.5).abs() < 1e-6));
    }
}
