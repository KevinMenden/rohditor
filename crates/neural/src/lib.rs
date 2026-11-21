//! Neural joint demosaic + denoise (epic #115).
//!
//! Runs the U-Net from `ml/` on a normalized Bayer mosaic, following the
//! contract in `ml/contract.json`: the mosaic is phase-aligned to RGGB,
//! white balanced to green = 1, packed into `[R, G1, G2, B]` channels, run
//! in 1024 px tiles with a 128 px halo, and the balanced RGB output is stitched
//! and brought back to camera-native linear RGB.
//!
//! This crate knows nothing about RAW decoding, recipes or the UI. It owns
//! model verification, packing, tiling, stitching, the highlight fallback blend
//! and cancellation between tiles; inference sits behind [`TileRunner`].

mod error;
mod highlights;
mod layout;
mod model;
#[cfg(feature = "onnx-runtime")]
mod ort_runner;
mod pack;
mod runner;

use std::fmt;

use rohditor_image::{LinearRgbImage, LinearRgbSpace, MosaicImage, allocate_zeroed_f32};

pub use error::NeuralError;
pub use highlights::{FALLBACK_FULL, FALLBACK_START, blend_highlights, needs_highlight_fallback};
pub use layout::{SIZE_MULTIPLE, Tiling};
pub use model::{CONTRACT_VERSION, ModelFile, ModelId};
#[cfg(feature = "onnx-runtime")]
pub use ort_runner::{Backend, OrtRunner};
pub use pack::Balance;
pub use runner::{TileRunner, TileTensor};

use layout::Canvas;

/// A loaded model, ready to reconstruct mosaics.
pub struct NeuralDemosaicer {
    runner: Box<dyn TileRunner>,
    id: ModelId,
    tiling: Tiling,
}

impl NeuralDemosaicer {
    /// Use any backend; `id` identifies its numerical behavior for caches.
    #[must_use]
    pub fn new(runner: Box<dyn TileRunner>, id: ModelId) -> Self {
        Self {
            runner,
            id,
            tiling: Tiling::CONTRACT,
        }
    }

    /// Load the model at `path` into ONNX Runtime. `Backend::Gpu` falls back
    /// to the CPU when no GPU session can be created; `Backend::Cpu` never
    /// touches the GPU.
    #[cfg(feature = "onnx-runtime")]
    pub fn load(path: &std::path::Path, backend: Backend) -> Result<Self, NeuralError> {
        let model = ModelFile::read(path)?;
        let runner = match backend {
            Backend::Gpu => OrtRunner::preferring_gpu(&model)?,
            Backend::Cpu => OrtRunner::new(&model, Backend::Cpu)?,
        };
        tracing::info!(
            path = %path.display(),
            backend = %runner.backend(),
            id = %model.id(),
            "neural model loaded"
        );
        Ok(Self::new(Box::new(runner), model.id()))
    }

    #[must_use]
    pub fn with_tiling(mut self, tiling: Tiling) -> Self {
        self.tiling = tiling;
        self
    }

    #[must_use]
    pub const fn model_id(&self) -> ModelId {
        self.id
    }

    /// Reconstruct a normalized mosaic (black 0, white 1 per channel) into
    /// camera-native linear RGB. `as_shot` is the white balance the model
    /// reconstructs under; only its ratios matter. Values are not clipped.
    /// `cancelled` is polled between tiles.
    pub fn reconstruct(
        &self,
        mosaic: &MosaicImage<f32>,
        as_shot: [f32; 3],
        cancelled: &dyn Fn() -> bool,
    ) -> Result<LinearRgbImage<f32>, NeuralError> {
        let balance = Balance::new(as_shot)?;
        let (width, height) = (mosaic.width(), mosaic.height());
        let canvas = Canvas::new(width, height, mosaic.pattern(), self.tiling)?;
        let stride = width
            .checked_mul(3)
            .ok_or_else(|| NeuralError::invalid_input("RGB stride overflowed"))?;
        let elements = stride
            .checked_mul(height)
            .ok_or_else(|| NeuralError::invalid_input("RGB sample count overflowed"))?;
        let mut output = allocate_zeroed_f32(elements)?;
        let tiles = canvas.tiles();
        let span = tracing::info_span!("neural.reconstruct", width, height, tiles = tiles.len());
        let _guard = span.enter();
        for tile in tiles {
            if cancelled() {
                return Err(NeuralError::Cancelled);
            }
            let input = pack::pack(mosaic, &canvas, tile.input, balance)?;
            let rgb = self.runner.run_tile(&input)?;
            pack::unpack_core(&rgb, &canvas, tile, balance, &mut output, (width, height))?;
        }
        if cancelled() {
            return Err(NeuralError::Cancelled);
        }
        if let Some(index) = output.iter().position(|value| !value.is_finite()) {
            return Err(NeuralError::Runtime {
                reason: format!(
                    "the model produced a non-finite value at ({}, {})",
                    (index / 3) % width,
                    index / stride
                ),
            });
        }
        Ok(LinearRgbImage::new(
            width,
            height,
            stride,
            LinearRgbSpace::CameraNative,
            output,
        )?)
    }
}

/// The whole image as the network's balanced, packed input, without halo:
/// what one untiled pass would see. Returns the tensor and the canvas position
/// of image pixel `(0, 0)` (the RGGB phase shift, `0` or `1` per axis); the
/// canvas is padded by mirroring to a multiple of [`SIZE_MULTIPLE`]. Used to
/// check the packing against the Python training pipeline.
pub fn pack_image(
    mosaic: &MosaicImage<f32>,
    as_shot: [f32; 3],
) -> Result<(TileTensor, (usize, usize)), NeuralError> {
    let balance = Balance::new(as_shot)?;
    let canvas = Canvas::new(
        mosaic.width(),
        mosaic.height(),
        mosaic.pattern(),
        Tiling::new(usize::MAX / 4 / SIZE_MULTIPLE * SIZE_MULTIPLE, 0)?,
    )?;
    let [tile] = canvas.tiles()[..] else {
        return Err(NeuralError::invalid_input(
            "the image needs more than one tile",
        ));
    };
    Ok((
        pack::pack(mosaic, &canvas, tile.input, balance)?,
        canvas.offset(),
    ))
}

impl fmt::Debug for NeuralDemosaicer {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("NeuralDemosaicer")
            .field("id", &self.id)
            .field("tiling", &self.tiling)
            .finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests;
