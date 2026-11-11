//! Pixel containers: a borrowed linear RGB input and the 8-bit gain-map output.

use crate::GainMapError;

/// Borrowed, interleaved linear RGB (`f32`, three samples per pixel, no row padding).
///
/// Values are scene/display-linear relative to diffuse white = 1.0 and may exceed 1.0.
#[derive(Debug, Clone, Copy)]
pub struct LinearRgbView<'a> {
    width: usize,
    height: usize,
    pixels: &'a [f32],
}

impl<'a> LinearRgbView<'a> {
    /// Checks that `pixels` holds exactly `width * height * 3` samples.
    pub fn new(width: usize, height: usize, pixels: &'a [f32]) -> Result<Self, GainMapError> {
        let expected = sample_count(width, height, 3)?;
        if pixels.len() != expected {
            return Err(GainMapError::InvalidDimensions {
                width,
                height,
                channels: 3,
                reason: "pixel buffer length does not match the dimensions",
            });
        }
        Ok(Self {
            width,
            height,
            pixels,
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
    pub const fn pixels(&self) -> &'a [f32] {
        self.pixels
    }

    /// One row of interleaved samples.
    pub(crate) fn row(&self, y: usize) -> &'a [f32] {
        let stride = self.width * 3;
        &self.pixels[y * stride..(y + 1) * stride]
    }
}

/// An encoded gain map: 8-bit samples, `channels` interleaved, no row padding.
///
/// The samples are the *encoded* values (normalised and gamma-encoded), exactly what a codec
/// stores. Use [`crate::GainMapMetadata`] to interpret them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GainMapImage {
    width: usize,
    height: usize,
    channels: usize,
    data: Vec<u8>,
}

impl GainMapImage {
    /// Checks the layout. `channels` is 1 (luminance) or 3 (RGB).
    pub fn new(
        width: usize,
        height: usize,
        channels: usize,
        data: Vec<u8>,
    ) -> Result<Self, GainMapError> {
        if channels != 1 && channels != 3 {
            return Err(GainMapError::UnsupportedChannelCount(channels));
        }
        if data.len() != sample_count(width, height, channels)? {
            return Err(GainMapError::InvalidDimensions {
                width,
                height,
                channels,
                reason: "data length does not match the dimensions",
            });
        }
        Ok(Self {
            width,
            height,
            channels,
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
    pub const fn channels(&self) -> usize {
        self.channels
    }

    #[must_use]
    pub fn data(&self) -> &[u8] {
        &self.data
    }

    #[must_use]
    pub fn into_data(self) -> Vec<u8> {
        self.data
    }
}

/// `width * height * channels`, rejecting empty images and overflow.
pub(crate) fn sample_count(
    width: usize,
    height: usize,
    channels: usize,
) -> Result<usize, GainMapError> {
    let invalid = |reason| GainMapError::InvalidDimensions {
        width,
        height,
        channels,
        reason,
    };
    if width == 0 || height == 0 {
        return Err(invalid("dimensions must be non-zero"));
    }
    width
        .checked_mul(height)
        .and_then(|pixels| pixels.checked_mul(channels))
        .ok_or_else(|| invalid("sample count overflows"))
}
