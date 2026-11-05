//! Mask overlay: one layer's alpha as an 8-bit image in display geometry.
//!
//! The Masks tool shows the selected mask on top of the preview. A frontend asks for it with
//! [`render_mask_overlay_preview`], separately from the preview render, so the tint can follow a
//! drag without waiting for one. It uses the same [`PreparedMask`] maths as the adjustment stage
//! and is oriented and cropped like the colour image, so the UI can tint and draw it without
//! knowing about the developed frame.
//!
//! Layers with luminance or colour range components select on what the image looks like, so
//! their overlay needs pixels: [`render_mask_overlay_from_image`] measures the reference from the
//! developed image, at that image's size. The image-free [`render_mask_overlay_preview`] is for
//! gradients and brushes, where the small size keeps the tint following a drag.

use rayon::prelude::*;
use rohditor_edit::EditRecipe;
use rohditor_image::{LinearRgbImage, Orientation, allocate_zeroed_u8};
use rohditor_mask::{MaskFrame, PreparedMask, ReferenceRow};

use super::ReferenceChain;
use super::local_stage::mask_error;
use super::range_reference::ReferenceRows;
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

/// The overlay of layer `layer_id` at a small size, for a frontend that wants the tint to follow
/// a drag without waiting for a render.
///
/// `frame` is the developed frame and `source_orientation` the file's; the recipe's orientation
/// override and crop apply as in a render. Mask lengths are in long-edge units, so the mask is
/// prepared on a copy of the frame scaled to `long_edge` pixels and gives the same shape as the
/// full-size overlay at a fraction of the cost. `Ok(None)` when the recipe has no such layer.
///
/// # Errors
/// An invalid recipe or geometry, or an allocation failure. A layer with range components has
/// no overlay without an image; use [`render_mask_overlay_from_image`] for those.
pub fn render_mask_overlay_preview(
    recipe: &EditRecipe,
    layer_id: u32,
    frame: (usize, usize),
    source_orientation: Orientation,
    long_edge: usize,
) -> Result<Option<MaskAlphaImage>, PipelineError> {
    let scale = (long_edge.max(1) as f64 / frame.0.max(frame.1).max(1) as f64).min(1.0);
    let small = (
        ((frame.0 as f64 * scale).round() as usize).max(1),
        ((frame.1 as f64 * scale).round() as usize).max(1),
    );
    let orientation = recipe
        .geometry
        .orientation_override
        .unwrap_or(source_orientation);
    let geometry = OutputGeometry::new(small.0, small.1, orientation, recipe.geometry.crop)?;
    MaskOverlayPlan::prepare(recipe, layer_id, small, geometry)?
        .map(|plan| plan.render(None, &CancellationToken::new()))
        .transpose()
}

/// The overlay of layer `layer_id` measured on the developed image itself, which layers with
/// luminance or colour range components need. Works for every layer.
///
/// `image` is the linear Rec.2020 image as it enters the adjustment stage (a preview base) and
/// `source_orientation` the file's. The overlay has the size of `image`, oriented and cropped as
/// in a render, and shows the layer at full strength like [`render_mask_overlay_preview`].
/// `Ok(None)` when the recipe has no such layer.
///
/// # Errors
/// An invalid recipe or geometry, or an allocation failure.
pub fn render_mask_overlay_from_image(
    image: &LinearRgbImage<f32>,
    source_orientation: Orientation,
    recipe: &EditRecipe,
    layer_id: u32,
) -> Result<Option<MaskAlphaImage>, PipelineError> {
    let frame = (image.width(), image.height());
    let orientation = recipe
        .geometry
        .orientation_override
        .unwrap_or(source_orientation);
    let geometry = OutputGeometry::new(frame.0, frame.1, orientation, recipe.geometry.crop)?;
    let chain = ReferenceChain::new(recipe);
    let reference = OverlayReference {
        image,
        chain: &chain,
    };
    MaskOverlayPlan::prepare(recipe, layer_id, frame, geometry)?
        .map(|plan| plan.render(Some(&reference), &CancellationToken::new()))
        .transpose()
}

/// The image a range mask is measured on: its pixels and the chain that turns them into the
/// reference pixels the masks read.
pub(super) struct OverlayReference<'a> {
    pub(super) image: &'a LinearRgbImage<f32>,
    pub(super) chain: &'a ReferenceChain,
}

/// A requested overlay, resolved for one render: the layer's mask and the geometry to present it in.
#[derive(Debug)]
pub(super) struct MaskOverlayPlan {
    mask: PreparedMask,
    frame: (usize, usize),
    geometry: OutputGeometry,
}

impl MaskOverlayPlan {
    /// Resolves layer `layer_id` of `recipe`, or `None` if the recipe has no such layer.
    ///
    /// The overlay shows the *shape* of the mask at full strength, whether or not the layer is
    /// enabled or has non-neutral sliders, so a layer can be inspected while it is being set up.
    pub(super) fn prepare(
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

    /// Renders the overlay. A mask with range components needs `reference` (an image of the
    /// plan's frame size); without it the render fails rather than guess.
    pub(super) fn render(
        &self,
        reference: Option<&OverlayReference<'_>>,
        cancellation: &CancellationToken,
    ) -> Result<MaskAlphaImage, PipelineError> {
        let (width, height) = self.frame;
        let needs = self.mask.reference_needs();
        let reference = match reference {
            Some(reference)
                if (reference.image.width(), reference.image.height()) != self.frame =>
            {
                return Err(PipelineError::InvalidRecipe {
                    field: "local.layers",
                    reason: "the mask overlay was prepared for another frame size".to_owned(),
                });
            }
            Some(reference) => Some(reference),
            None if needs.any() => {
                return Err(PipelineError::InvalidRecipe {
                    field: "local.layers",
                    reason: "a luminance or color range needs the image to show its overlay"
                        .to_owned(),
                });
            }
            None => None,
        };
        let mut plane = allocate_zeroed_u8(width * height)?;
        let row_stride = reference.map_or(0, |reference| reference.image.row_stride());
        // Quantize in the developed frame, then gather through the output geometry.
        plane.par_chunks_mut(width).enumerate().try_for_each_init(
            || (Vec::<f32>::new(), ReferenceRows::default()),
            |(row, measured), (y, out)| -> Result<(), PipelineError> {
                cancellation.checkpoint()?;
                row.resize(width, 0.0);
                let reference_row = match reference {
                    Some(reference) if needs.any() => {
                        let start = y * row_stride;
                        let pixels = &reference.image.data()[start..start + width * 3];
                        measured.measure(needs, pixels, |pixel| reference.chain.apply(pixel));
                        measured.row()
                    }
                    _ => ReferenceRow::NONE,
                };
                self.mask.evaluate_row(y, 0..width, reference_row, row);
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
