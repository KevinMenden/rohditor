//! Mask overlay output: one layer's alpha as an 8-bit image in display geometry.
//!
//! The Masks tool shows the selected mask on top of the preview. The overlay is produced in the
//! output stage, from the same [`PreparedMask`] maths as the adjustment stage, and is oriented
//! and cropped exactly like the colour image, so the UI can tint and draw it without knowing
//! about the developed frame.

use std::mem::size_of;

use rayon::prelude::*;
use rohditor_edit::EditRecipe;
use rohditor_image::allocate_zeroed_u8;
use rohditor_mask::{MaskFrame, PreparedMask};

use super::local_stage::mask_error;
use crate::{CancellationToken, OutputGeometry, PipelineError};

/// An 8-bit alpha image (`0` = outside the mask, `255` = fully inside) in the geometry of the
/// preview it accompanies: oriented and cropped like the colour image.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MaskAlphaImage {
    width: usize,
    height: usize,
    data: Vec<u8>,
}

impl MaskAlphaImage {
    #[must_use]
    pub const fn width(&self) -> usize {
        self.width
    }

    #[must_use]
    pub const fn height(&self) -> usize {
        self.height
    }

    /// Row-major alpha samples, `width * height` long (rows are not padded).
    #[must_use]
    pub fn data(&self) -> &[u8] {
        &self.data
    }
}

/// A requested overlay, resolved for one render: the layer's mask and the geometry to present it in.
#[derive(Debug)]
pub(crate) struct MaskOverlayPlan {
    mask: PreparedMask,
    frame: (usize, usize),
    geometry: OutputGeometry,
}

impl MaskOverlayPlan {
    /// Resolves layer `layer_id` of `recipe`, or `None` if the recipe has no such layer.
    ///
    /// The overlay shows the *shape* of the mask at full strength, whether or not the layer is
    /// enabled or has non-neutral sliders, so a layer can be inspected while it is being set up.
    pub(crate) fn prepare(
        recipe: &EditRecipe,
        layer_id: u32,
        frame: (usize, usize),
        geometry: OutputGeometry,
    ) -> Result<Option<Self>, PipelineError> {
        let Some(layer) = recipe.local.layer(layer_id) else {
            return Ok(None);
        };
        let mut shown = layer.clone();
        shown.opacity = 1.0;
        let mask = PreparedMask::prepare(
            &shown,
            MaskFrame {
                width: frame.0,
                height: frame.1,
            },
        )
        .map_err(mask_error)?;
        Ok(Some(Self {
            mask,
            frame,
            geometry,
        }))
    }

    /// Bytes this overlay needs while rendering: the prepared mask, the developed-frame alpha
    /// plane, the output image and one f32 row per worker thread.
    pub(crate) fn bytes(&self) -> usize {
        let (width, height) = self.frame;
        let (output_width, output_height) = self.geometry.output_dimensions();
        self.mask
            .bytes()
            .saturating_add(width.saturating_mul(height))
            .saturating_add(output_width.saturating_mul(output_height))
            .saturating_add(
                width
                    .saturating_mul(size_of::<f32>())
                    .saturating_mul(rayon::current_num_threads()),
            )
    }

    pub(crate) fn render(
        &self,
        cancellation: &CancellationToken,
    ) -> Result<MaskAlphaImage, PipelineError> {
        let (width, height) = self.frame;
        let mut plane = allocate_zeroed_u8(width * height)?;
        // Quantize in the developed frame, then gather through the output geometry.
        plane.par_chunks_mut(width).enumerate().try_for_each_init(
            Vec::<f32>::new,
            |row, (y, out)| -> Result<(), PipelineError> {
                cancellation.checkpoint()?;
                row.resize(width, 0.0);
                self.mask.evaluate_row(y, 0..width, row);
                for (byte, alpha) in out.iter_mut().zip(row.iter()) {
                    *byte = (alpha.clamp(0.0, 1.0) * 255.0).round() as u8;
                }
                Ok(())
            },
        )?;

        let (output_width, output_height) = self.geometry.output_dimensions();
        let mut data = allocate_zeroed_u8(output_width * output_height)?;
        data.par_chunks_mut(output_width)
            .enumerate()
            .for_each(|(output_y, row)| {
                for (output_x, byte) in row.iter_mut().enumerate() {
                    let (x, y) = self
                        .geometry
                        .source_coordinate_in_bounds(output_x, output_y);
                    *byte = plane[y * width + x];
                }
            });
        cancellation.checkpoint()?;
        Ok(MaskAlphaImage {
            width: output_width,
            height: output_height,
            data,
        })
    }
}
