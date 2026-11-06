//! Local adjustments (mask layers) inside the per-pixel adjustment stage.
//!
//! `apply_adjustments_cancellable` owns the pixel loop; this module owns everything that is
//! specific to masks: resolving the active layers once per render ([`LocalStage::prepare`]),
//! evaluating their alpha one row at a time into per-thread scratch ([`RowAlphas`]), and the
//! two per-pixel terms that are inserted into the global chain:
//!
//! 1. **Scene-referred** ([`LocalStage::scene_terms`]), right after the global exposure gain and
//!    before base rendering. With `a_i` the alpha of layer `i`, exposure is multiplied by
//!    `2^(sum a_i * exposure_ev_i)` and the white is shifted by the gains of
//!    `sum a_i * temperature_i` / `sum a_i * tint_i`. Doing this before base rendering lets
//!    local exposure roll off through the base curve instead of clipping.
//! 2. **Display-referred** ([`LocalStage::apply_display`]), right after the global tone curve and
//!    before global saturation. For each layer in order with `a_i > 0`:
//!    `p' = saturation(light_tone_i(p))`, then `p = p + a_i * (p' - p)`. Layer order matters
//!    here: a later layer sees the result of the earlier ones.
//!
//! A layer with local detail settings adds a third term before those two
//! ([`LocalStage::blend_detail`]): `p = p + a_i * (copy_i - p)`, where `copy_i` is the image
//! developed with that layer's detail settings (see `pipeline::local_detail`). It runs first
//! because the copy is at the state of the pixel entering the stage, and everything after it acts
//! on the blended result.
//!
//! Layers with luminance or colour range components also need a per-pixel *reference* (see
//! `range_reference`): the stage measures it once per row, before evaluating any alpha, using a
//! function the adjustment stage passes in. That function is what keeps the reference free of
//! local terms, so one mask never changes what another one selects.
//!
//! A recipe without active layers never creates a [`LocalStage`], so the pipeline output is
//! bit-identical to a build without masks.

use std::mem::size_of;
use std::sync::Arc;

use rohditor_edit::{EditRecipe, LightAdjustments, LightToneLut, MaskLayer};
use rohditor_image::LinearRgbImage;
use rohditor_mask::{MaskError, MaskFrame, PreparedMask, ReferenceNeeds};

use super::range_reference::{ReferenceRows, row_bytes};
use super::stages::adjustments::{apply_light_tone, luminance};
use crate::PipelineError;

/// Active mask layers resolved for one frame size.
#[derive(Debug)]
pub(crate) struct LocalStage {
    width: usize,
    height: usize,
    layers: Vec<StageLayer>,
    /// Indices into `layers` that have a scene-referred term (exposure, temperature, tint).
    scene: Vec<usize>,
    /// Indices into `layers` that have a display-referred term (Light tone, saturation).
    display: Vec<usize>,
    /// Indices into `layers` that blend in a local detail copy.
    detail: Vec<usize>,
    /// What any layer's range components read; measured once per row.
    reference: ReferenceNeeds,
}

#[derive(Debug)]
struct StageLayer {
    mask: PreparedMask,
    exposure_ev: f32,
    temperature: f32,
    tint: f32,
    /// Built once per render from the layer's contrast/highlights/shadows/whites/blacks, so a
    /// GPU port can upload the very same table.
    tone: Option<Box<LightToneLut>>,
    /// `1 + saturation`: the local slider is 0 at neutral, unlike the global multiplier.
    saturation_scale: f32,
    /// The image developed with this layer's detail settings.
    detail: Option<Arc<LinearRgbImage<f32>>>,
}

/// Alpha of every layer for the row being processed, laid out `[layer][x]`. One per worker
/// thread, so the stage never holds a full-frame plane per layer.
#[derive(Debug, Default)]
pub(crate) struct RowAlphas {
    values: Vec<f32>,
    reference: ReferenceRows,
}

/// Alpha-weighted sums of the scene-referred sliders at one pixel.
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct SceneTerms {
    pub exposure_ev: f32,
    pub temperature: f32,
    pub tint: f32,
}

impl LocalStage {
    /// Resolves the active layers of `recipe` for a `width` x `height` developed frame, or
    /// `None` when no layer would change pixels. `detail_image` finds the copy a layer blends in.
    pub(crate) fn prepare(
        recipe: &EditRecipe,
        width: usize,
        height: usize,
        detail_image: &dyn Fn(&MaskLayer) -> Option<Arc<LinearRgbImage<f32>>>,
    ) -> Result<Option<Self>, PipelineError> {
        if !recipe.local.is_active() {
            return Ok(None);
        }
        recipe.local.validate()?;
        let frame = MaskFrame { width, height };
        let mut stage = Self {
            width,
            height,
            layers: Vec::new(),
            scene: Vec::new(),
            display: Vec::new(),
            detail: Vec::new(),
            reference: ReferenceNeeds::default(),
        };
        for layer in recipe.local.layers.iter().filter(|layer| layer.is_active()) {
            let index = stage.layers.len();
            let detail = detail_image(layer);
            if detail
                .as_ref()
                .is_some_and(|image| image.width() != width || image.height() != height)
            {
                return Err(PipelineError::InvalidRecipe {
                    field: "local.layers.settings.detail",
                    reason: "the local detail copy has another size than the frame".to_owned(),
                });
            }
            let layer = StageLayer::prepare(layer, frame, detail)?;
            stage.reference = stage.reference.union(layer.mask.reference_needs());
            if layer.has_scene_term() {
                stage.scene.push(index);
            }
            if layer.has_display_term() {
                stage.display.push(index);
            }
            if layer.detail.is_some() {
                stage.detail.push(index);
            }
            stage.layers.push(layer);
        }
        Ok(Some(stage))
    }

    /// Whether this stage was prepared for an image of this size.
    pub(crate) fn matches_frame(&self, width: usize, height: usize) -> bool {
        self.width == width && self.height == height
    }

    /// Bytes this stage keeps alive while rendering: prepared masks, tone tables, and one row
    /// of alpha per layer (plus the reference row) for every worker thread.
    pub(crate) fn bytes(&self) -> usize {
        let masks: usize = self
            .layers
            .iter()
            .map(|layer| {
                size_of::<StageLayer>()
                    + layer.mask.bytes()
                    + layer.tone.as_ref().map_or(0, |_| size_of::<LightToneLut>())
            })
            .sum();
        let row_scratch = self
            .layers
            .len()
            .saturating_mul(self.width)
            .saturating_mul(size_of::<f32>())
            .saturating_add(row_bytes(self.reference, self.width))
            .saturating_mul(rayon::current_num_threads());
        masks.saturating_add(row_scratch)
    }

    /// Fills `alphas` with every layer's alpha for row `y`.
    ///
    /// `pixels` is the row as it enters the adjustment stage (interleaved linear Rec.2020) and
    /// `to_reference` turns one such pixel into the reference pixel range components read. It is
    /// only called when some layer has a range component.
    pub(crate) fn evaluate_row(
        &self,
        y: usize,
        pixels: &[f32],
        to_reference: impl Fn(&mut [f32; 3]),
        alphas: &mut RowAlphas,
    ) {
        if self.reference.any() {
            alphas
                .reference
                .measure(self.reference, pixels, to_reference);
        }
        alphas.values.resize(self.layers.len() * self.width, 0.0);
        let reference = alphas.reference.row();
        for (layer, out) in self
            .layers
            .iter()
            .zip(alphas.values.chunks_exact_mut(self.width))
        {
            layer.mask.evaluate_row(y, 0..self.width, reference, out);
        }
    }

    /// Blends every layer's local detail copy into row `y` in layer order. `row` holds the
    /// frame's pixels as they enter the adjustment stage, and `alphas` was evaluated for it.
    pub(crate) fn blend_detail(&self, y: usize, row: &mut [f32], alphas: &RowAlphas) {
        for &index in &self.detail {
            let Some(image) = &self.layers[index].detail else {
                continue;
            };
            let start = y * image.row_stride();
            let copy = &image.data()[start..start + row.len()];
            let layer_alphas = &alphas.values[index * self.width..][..self.width];
            let pixels = row.as_chunks_mut::<3>().0.iter_mut();
            for ((pixel, copy), &alpha) in pixels.zip(copy.as_chunks::<3>().0).zip(layer_alphas) {
                if alpha.is_nan() || alpha <= 0.0 || copy.iter().any(|value| !value.is_finite()) {
                    continue;
                }
                if alpha >= 1.0 {
                    *pixel = *copy;
                } else {
                    for (value, copy) in pixel.iter_mut().zip(copy) {
                        *value += alpha * (copy - *value);
                    }
                }
            }
        }
    }

    /// Alpha-weighted scene-referred sums at column `x`, or `None` if no layer has such a term.
    #[inline]
    pub(crate) fn scene_terms(&self, alphas: &RowAlphas, x: usize) -> Option<SceneTerms> {
        if self.scene.is_empty() {
            return None;
        }
        let mut terms = SceneTerms::default();
        for &index in &self.scene {
            let alpha = alphas.values[index * self.width + x];
            if alpha.is_nan() || alpha <= 0.0 {
                continue;
            }
            let layer = &self.layers[index];
            terms.exposure_ev += alpha * layer.exposure_ev;
            terms.temperature += alpha * layer.temperature;
            terms.tint += alpha * layer.tint;
        }
        Some(terms)
    }

    /// Blends every layer's display-referred edit into `pixel` in layer order.
    #[inline]
    pub(crate) fn apply_display(&self, pixel: &mut [f32], alphas: &RowAlphas, x: usize) {
        for &index in &self.display {
            let alpha = alphas.values[index * self.width + x];
            if alpha.is_nan() || alpha <= 0.0 {
                continue;
            }
            let layer = &self.layers[index];
            let mut edited = [pixel[0], pixel[1], pixel[2]];
            if let Some(tone) = &layer.tone {
                apply_light_tone(&mut edited, tone);
            }
            if layer.saturation_scale != 1.0 {
                // The same luminance-preserving scale as the global Saturation control.
                let luminance = luminance(&edited);
                for value in &mut edited {
                    *value = luminance + layer.saturation_scale * (*value - luminance);
                }
            }
            if edited.iter().any(|value| !value.is_finite()) {
                continue;
            }
            if alpha >= 1.0 {
                pixel.copy_from_slice(&edited);
            } else {
                for (value, edited) in pixel.iter_mut().zip(edited) {
                    *value += alpha * (edited - *value);
                }
            }
        }
    }
}

impl StageLayer {
    fn prepare(
        layer: &MaskLayer,
        frame: MaskFrame,
        detail: Option<Arc<LinearRgbImage<f32>>>,
    ) -> Result<Self, PipelineError> {
        let settings = &layer.settings;
        let mask = PreparedMask::prepare(layer, frame).map_err(mask_error)?;
        let has_tone = settings.contrast != 0.0
            || settings.highlights != 0.0
            || settings.shadows != 0.0
            || settings.whites != 0.0
            || settings.blacks != 0.0;
        let tone = has_tone.then(|| {
            Box::new(LightToneLut::new(&LightAdjustments {
                contrast: settings.contrast,
                highlights: settings.highlights,
                shadows: settings.shadows,
                whites: settings.whites,
                blacks: settings.blacks,
                ..LightAdjustments::default()
            }))
        });
        Ok(Self {
            mask,
            exposure_ev: settings.exposure_ev,
            temperature: settings.temperature,
            tint: settings.tint,
            tone,
            saturation_scale: 1.0 + settings.saturation,
            detail,
        })
    }

    fn has_scene_term(&self) -> bool {
        self.exposure_ev != 0.0 || self.temperature != 0.0 || self.tint != 0.0
    }

    fn has_display_term(&self) -> bool {
        self.tone.is_some() || self.saturation_scale != 1.0
    }
}

pub(super) fn mask_error(error: MaskError) -> PipelineError {
    PipelineError::InvalidRecipe {
        field: "local.layers",
        reason: error.to_string(),
    }
}
