//! Foundational typed image states and checked buffer-layout primitives.

use thiserror::Error;

mod orientation;

pub use orientation::{Orientation, OrientationMap};

/// Errors produced while constructing or allocating typed image buffers.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum ImageError {
    #[error("invalid image dimensions {width}x{height} with row stride {row_stride}: {reason}")]
    InvalidDimensions {
        width: usize,
        height: usize,
        row_stride: usize,
        reason: String,
    },

    #[error("could not allocate {elements} image elements")]
    Allocation { elements: usize },

    #[error("unsupported CFA pattern {name} ({width}x{height})")]
    UnsupportedCfa {
        name: String,
        width: usize,
        height: usize,
    },
}

/// One color site in a Bayer color-filter array.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CfaColor {
    Red,
    Green,
    Blue,
}

impl CfaColor {
    #[must_use]
    pub const fn channel_index(self) -> usize {
        match self {
            Self::Red => 0,
            Self::Green => 1,
            Self::Blue => 2,
        }
    }
}

/// A supported 2x2 Bayer layout.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BayerPattern {
    Rggb,
    Bggr,
    Grbg,
    Gbrg,
}

impl BayerPattern {
    pub fn parse(name: &str, width: usize, height: usize) -> Result<Self, ImageError> {
        if width == 2 && height == 2 {
            match name.to_ascii_uppercase().as_str() {
                "RGGB" => return Ok(Self::Rggb),
                "BGGR" => return Ok(Self::Bggr),
                "GRBG" => return Ok(Self::Grbg),
                "GBRG" => return Ok(Self::Gbrg),
                _ => {}
            }
        }
        Err(ImageError::UnsupportedCfa {
            name: name.to_owned(),
            width,
            height,
        })
    }

    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Rggb => "RGGB",
            Self::Bggr => "BGGR",
            Self::Grbg => "GRBG",
            Self::Gbrg => "GBRG",
        }
    }

    #[must_use]
    pub const fn color_at(self, x: usize, y: usize) -> CfaColor {
        let index = ((y & 1) << 1) | (x & 1);
        match self {
            Self::Rggb => [
                CfaColor::Red,
                CfaColor::Green,
                CfaColor::Green,
                CfaColor::Blue,
            ][index],
            Self::Bggr => [
                CfaColor::Blue,
                CfaColor::Green,
                CfaColor::Green,
                CfaColor::Red,
            ][index],
            Self::Grbg => [
                CfaColor::Green,
                CfaColor::Red,
                CfaColor::Blue,
                CfaColor::Green,
            ][index],
            Self::Gbrg => [
                CfaColor::Green,
                CfaColor::Blue,
                CfaColor::Red,
                CfaColor::Green,
            ][index],
        }
    }

    #[must_use]
    pub fn shifted(self, x: usize, y: usize) -> Self {
        match (self, x & 1, y & 1) {
            (pattern, 0, 0) => pattern,
            (Self::Rggb, 1, 0) | (Self::Bggr, 0, 1) => Self::Grbg,
            (Self::Rggb, 0, 1) | (Self::Bggr, 1, 0) => Self::Gbrg,
            (Self::Rggb, 1, 1) => Self::Bggr,
            (Self::Bggr, 1, 1) => Self::Rggb,
            (Self::Grbg, 1, 0) | (Self::Gbrg, 0, 1) => Self::Rggb,
            (Self::Grbg, 0, 1) | (Self::Gbrg, 1, 0) => Self::Bggr,
            (Self::Grbg, 1, 1) => Self::Gbrg,
            (Self::Gbrg, 1, 1) => Self::Grbg,
            _ => unreachable!("Bayer shifts only use parity"),
        }
    }
}

/// A rectangular image region, ready to become a future tile boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ImageRegion {
    pub x: usize,
    pub y: usize,
    pub width: usize,
    pub height: usize,
}

/// Neighboring pixels required around an image region.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Halo {
    pub left: usize,
    pub right: usize,
    pub top: usize,
    pub bottom: usize,
}

/// A one-channel CFA image with explicit dimensions, stride, and Bayer phase.
#[derive(Debug, Clone, PartialEq)]
pub struct MosaicImage<T> {
    width: usize,
    height: usize,
    row_stride: usize,
    pattern: BayerPattern,
    data: Vec<T>,
}

impl<T> MosaicImage<T> {
    pub fn new(
        width: usize,
        height: usize,
        row_stride: usize,
        pattern: BayerPattern,
        data: Vec<T>,
    ) -> Result<Self, ImageError> {
        validate_layout(width, height, row_stride, width, data.len())?;
        Ok(Self {
            width,
            height,
            row_stride,
            pattern,
            data,
        })
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
    pub const fn row_stride(&self) -> usize {
        self.row_stride
    }

    #[must_use]
    pub const fn pattern(&self) -> BayerPattern {
        self.pattern
    }

    #[must_use]
    pub fn data(&self) -> &[T] {
        &self.data
    }

    #[must_use]
    pub fn into_data(self) -> Vec<T> {
        self.data
    }

    #[must_use]
    pub fn get(&self, x: usize, y: usize) -> Option<&T> {
        (x < self.width && y < self.height).then(|| &self.data[y * self.row_stride + x])
    }

    pub fn sample(&self, x: usize, y: usize) -> &T {
        &self.data[y * self.row_stride + x]
    }
}

/// The coordinate system carried by a scene-linear RGB buffer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LinearRgbSpace {
    CameraNative,
    Rec2020D65,
    SrgbD65,
}

impl LinearRgbSpace {
    pub const fn description(self) -> &'static str {
        match self {
            Self::CameraNative => "camera-native linear RGB",
            Self::Rec2020D65 => "linear Rec.2020/D65",
            Self::SrgbD65 => "linear sRGB/D65",
        }
    }
}

/// A three-channel scene-linear image with an explicit color space.
#[derive(Debug, Clone, PartialEq)]
pub struct LinearRgbImage<T> {
    width: usize,
    height: usize,
    row_stride: usize,
    space: LinearRgbSpace,
    data: Vec<T>,
}

impl<T> LinearRgbImage<T> {
    pub fn new(
        width: usize,
        height: usize,
        row_stride: usize,
        space: LinearRgbSpace,
        data: Vec<T>,
    ) -> Result<Self, ImageError> {
        let minimum_stride = width.checked_mul(3).ok_or_else(|| {
            invalid_layout(
                width,
                height,
                row_stride,
                "RGB row-stride calculation overflowed",
            )
        })?;
        validate_layout(width, height, row_stride, minimum_stride, data.len())?;
        Ok(Self {
            width,
            height,
            row_stride,
            space,
            data,
        })
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
    pub const fn row_stride(&self) -> usize {
        self.row_stride
    }

    #[must_use]
    pub const fn space(&self) -> LinearRgbSpace {
        self.space
    }

    #[must_use]
    pub fn data(&self) -> &[T] {
        &self.data
    }

    #[must_use]
    pub fn into_data(self) -> Vec<T> {
        self.data
    }

    #[must_use]
    pub fn pixel(&self, x: usize, y: usize) -> Option<&[T]> {
        if x >= self.width || y >= self.height {
            return None;
        }
        let start = y * self.row_stride + x * 3;
        Some(&self.data[start..start + 3])
    }

    pub fn data_mut(&mut self) -> &mut [T] {
        &mut self.data
    }

    pub fn set_space(&mut self, space: LinearRgbSpace) {
        self.space = space;
    }
}

/// Transfer function/profile carried by output-ready RGB samples.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DisplayTransfer {
    Srgb,
}

/// A three-channel display/output image with an explicit transfer function.
#[derive(Debug, Clone, PartialEq)]
pub struct DisplayRgbImage<T> {
    width: usize,
    height: usize,
    row_stride: usize,
    transfer: DisplayTransfer,
    data: Vec<T>,
}

impl<T> DisplayRgbImage<T> {
    pub fn new(
        width: usize,
        height: usize,
        row_stride: usize,
        transfer: DisplayTransfer,
        data: Vec<T>,
    ) -> Result<Self, ImageError> {
        let minimum_stride = width.checked_mul(3).ok_or_else(|| {
            invalid_layout(
                width,
                height,
                row_stride,
                "RGB row-stride calculation overflowed",
            )
        })?;
        validate_layout(width, height, row_stride, minimum_stride, data.len())?;
        Ok(Self {
            width,
            height,
            row_stride,
            transfer,
            data,
        })
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
    pub const fn row_stride(&self) -> usize {
        self.row_stride
    }

    #[must_use]
    pub const fn transfer(&self) -> DisplayTransfer {
        self.transfer
    }

    #[must_use]
    pub fn data(&self) -> &[T] {
        &self.data
    }

    #[must_use]
    pub fn into_data(self) -> Vec<T> {
        self.data
    }

    #[must_use]
    pub fn pixel(&self, x: usize, y: usize) -> Option<&[T]> {
        if x >= self.width || y >= self.height {
            return None;
        }
        let start = y * self.row_stride + x * 3;
        Some(&self.data[start..start + 3])
    }
}

#[doc(hidden)]
pub fn allocate_zeroed_f32(elements: usize) -> Result<Vec<f32>, ImageError> {
    let mut values = Vec::new();
    values
        .try_reserve_exact(elements)
        .map_err(|_| ImageError::Allocation { elements })?;
    values.resize(elements, 0.0);
    Ok(values)
}

#[doc(hidden)]
pub fn allocate_zeroed_u8(elements: usize) -> Result<Vec<u8>, ImageError> {
    let mut values = Vec::new();
    values
        .try_reserve_exact(elements)
        .map_err(|_| ImageError::Allocation { elements })?;
    values.resize(elements, 0);
    Ok(values)
}

#[doc(hidden)]
pub fn allocate_zeroed_u16(elements: usize) -> Result<Vec<u16>, ImageError> {
    let mut values = Vec::new();
    values
        .try_reserve_exact(elements)
        .map_err(|_| ImageError::Allocation { elements })?;
    values.resize(elements, 0);
    Ok(values)
}

fn validate_layout(
    width: usize,
    height: usize,
    row_stride: usize,
    minimum_stride: usize,
    actual_elements: usize,
) -> Result<(), ImageError> {
    if width == 0 || height == 0 {
        return Err(invalid_layout(
            width,
            height,
            row_stride,
            "dimensions must be non-zero",
        ));
    }
    if row_stride < minimum_stride {
        return Err(invalid_layout(
            width,
            height,
            row_stride,
            "row stride is smaller than one visible row",
        ));
    }
    let required = row_stride.checked_mul(height).ok_or_else(|| {
        invalid_layout(
            width,
            height,
            row_stride,
            "sample-count calculation overflowed",
        )
    })?;
    if actual_elements != required {
        return Err(invalid_layout(
            width,
            height,
            row_stride,
            &format!("buffer has {actual_elements} elements, expected {required}"),
        ));
    }
    Ok(())
}

fn invalid_layout(width: usize, height: usize, row_stride: usize, reason: &str) -> ImageError {
    ImageError::InvalidDimensions {
        width,
        height,
        row_stride,
        reason: reason.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::{BayerPattern, CfaColor, MosaicImage};

    #[test]
    fn every_supported_bayer_layout_indexes_all_sites() {
        let cases = [
            (BayerPattern::Rggb, "RGGB"),
            (BayerPattern::Bggr, "BGGR"),
            (BayerPattern::Grbg, "GRBG"),
            (BayerPattern::Gbrg, "GBRG"),
        ];
        for (pattern, expected) in cases {
            let actual = [
                pattern.color_at(0, 0),
                pattern.color_at(1, 0),
                pattern.color_at(0, 1),
                pattern.color_at(1, 1),
            ]
            .map(|color| match color {
                CfaColor::Red => 'R',
                CfaColor::Green => 'G',
                CfaColor::Blue => 'B',
            })
            .into_iter()
            .collect::<String>();
            assert_eq!(actual, expected);
        }
    }

    #[test]
    fn crop_offsets_shift_the_bayer_phase() {
        assert_eq!(BayerPattern::Rggb.shifted(1, 0), BayerPattern::Grbg);
        assert_eq!(BayerPattern::Rggb.shifted(0, 1), BayerPattern::Gbrg);
        assert_eq!(BayerPattern::Rggb.shifted(1, 1), BayerPattern::Bggr);
    }

    #[test]
    fn public_buffer_rejects_an_incomplete_stride() {
        assert!(MosaicImage::new(2, 2, 2, BayerPattern::Rggb, vec![0_u16; 3]).is_err());
    }
}
