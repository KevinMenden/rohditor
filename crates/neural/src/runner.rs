//! The inference boundary: one tile in, one tile out.
//!
//! Tiling, packing and stitching never see the backend. ONNX Runtime on the CPU
//! is the first implementation (`ort_runner.rs`); a second one (tract, WGSL)
//! only has to implement [`TileRunner`].

use crate::NeuralError;

/// A dense float32 `[1, channels, height, width]` tensor (NCHW, batch 1).
#[derive(Debug, Clone, PartialEq)]
pub struct TileTensor {
    channels: usize,
    height: usize,
    width: usize,
    data: Vec<f32>,
}

impl TileTensor {
    pub fn new(
        channels: usize,
        height: usize,
        width: usize,
        data: Vec<f32>,
    ) -> Result<Self, NeuralError> {
        let expected = channels
            .checked_mul(height)
            .and_then(|n| n.checked_mul(width))
            .ok_or_else(|| NeuralError::invalid_input("tile tensor size overflowed"))?;
        if data.len() != expected {
            return Err(NeuralError::invalid_input(format!(
                "tile tensor [{channels}, {height}, {width}] needs {expected} values, got {}",
                data.len()
            )));
        }
        Ok(Self {
            channels,
            height,
            width,
            data,
        })
    }

    #[must_use]
    pub const fn channels(&self) -> usize {
        self.channels
    }

    #[must_use]
    pub const fn height(&self) -> usize {
        self.height
    }

    #[must_use]
    pub const fn width(&self) -> usize {
        self.width
    }

    /// `[1, channels, height, width]`, as ONNX shapes are written.
    #[must_use]
    pub const fn shape(&self) -> [usize; 4] {
        [1, self.channels, self.height, self.width]
    }

    #[must_use]
    pub fn data(&self) -> &[f32] {
        &self.data
    }

    pub fn data_mut(&mut self) -> &mut [f32] {
        &mut self.data
    }

    /// The plane of one channel, `height * width` values.
    #[must_use]
    pub fn plane(&self, channel: usize) -> &[f32] {
        let size = self.height * self.width;
        &self.data[channel * size..(channel + 1) * size]
    }
}

/// Runs the network on one packed tile.
///
/// Input is the contract's `[1, 4, h/2, w/2]` balanced mosaic; output must be
/// `[1, 3, h, w]` balanced linear RGB.
pub trait TileRunner: Send + Sync {
    fn run_tile(&self, input: &TileTensor) -> Result<TileTensor, NeuralError>;
}
