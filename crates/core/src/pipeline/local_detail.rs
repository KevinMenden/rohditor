//! Local noise reduction, moiré suppression and sharpening.
//!
//! These filters reach over a neighbourhood at source resolution, before optics, whereas masks
//! live in the developed frame and range masks read pixels only known after development. So the
//! blend is not done where the filters run. Instead the whole image is developed again per
//! distinct setting (detail stage, optics, defringe, reduction, colour conversion), and the
//! adjustment stage blends that copy in per layer with `p = mix(p, copy, alpha)`, using the alpha
//! it computes anyway. This needs no inverse optics map, leaves range masks as they are, and an
//! edit of a mask's shape only repeats the blend.

use std::mem::size_of;
use std::sync::Arc;
use std::time::Instant;

use rohditor_edit::{DetailSettings, EditRecipe, MaskLayer};
use rohditor_image::LinearRgbImage;
use rohditor_optics::OpticsService;
use rohditor_raw::RawFrame;

use super::camera_source::{complete_camera_source, prepare_camera_source};
use super::{
    DemosaicedBase, PreviewOptions, dimension_overflow, prepare_demosaiced_preview_owned,
    validate_working_set,
};
use crate::cpu::preview_dimensions;
use crate::{CancellationToken, DetailProvenance, PipelineError};

/// One copy of the image developed with other detail settings, at the size and in the colour
/// space of the [`DemosaicedBase`] it belongs to.
#[derive(Debug, Clone)]
pub struct LocalDetailImage {
    provenance: Option<DetailProvenance>,
    image: Arc<LinearRgbImage<f32>>,
}

impl LocalDetailImage {
    /// Wraps a base developed for `recipe.with_detail_settings(settings)`.
    #[must_use]
    pub fn new(base: DemosaicedBase) -> Self {
        Self {
            provenance: base.detail,
            image: Arc::new(base.image),
        }
    }

    pub(super) fn bytes(&self) -> usize {
        self.image.data().len().saturating_mul(size_of::<f32>())
    }
}

/// The detail settings `recipe` needs a developed copy for: those of its active layers, without
/// duplicates and without settings that do the same as the global ones.
#[must_use]
pub fn local_detail_variants(recipe: &EditRecipe) -> Vec<DetailSettings> {
    let global = recipe.detail_settings();
    let global_work = DetailProvenance::for_settings(global);
    let mut variants = Vec::new();
    let mut seen = Vec::new();
    for layer in recipe.local.layers.iter().filter(|layer| layer.is_active()) {
        if layer.settings.detail.is_neutral() {
            continue;
        }
        let settings = global.with_local(layer.settings.detail);
        let work = DetailProvenance::for_settings(settings);
        if work != global_work && !seen.contains(&work) {
            seen.push(work);
            variants.push(settings);
        }
    }
    variants
}

/// The copy `layer` blends in, if it has any.
pub(super) fn image_for_layer(
    images: &[LocalDetailImage],
    recipe: &EditRecipe,
    layer: &MaskLayer,
) -> Option<Arc<LinearRgbImage<f32>>> {
    if layer.settings.detail.is_neutral() {
        return None;
    }
    let work =
        DetailProvenance::for_settings(recipe.detail_settings().with_local(layer.settings.detail));
    images
        .iter()
        .find(|image| image.provenance == work)
        .map(|image| Arc::clone(&image.image))
}

/// Whether `images` are exactly the copies `recipe` needs.
pub(super) fn matches_recipe(images: &[LocalDetailImage], recipe: &EditRecipe) -> bool {
    let variants = local_detail_variants(recipe);
    variants.len() == images.len()
        && variants.into_iter().all(|settings| {
            let work = DetailProvenance::for_settings(settings);
            images.iter().any(|image| image.provenance == work)
        })
}

pub(super) fn images_bytes(images: &[LocalDetailImage]) -> usize {
    images
        .iter()
        .fold(0, |bytes, image| bytes.saturating_add(image.bytes()))
}

/// Develops the base of `recipe` together with one copy per entry of `variants`, sharing one
/// demosaic. The copies are made first, so the base can consume the camera source in place.
pub(super) fn prepare_base(
    optics: Option<&OpticsService>,
    frame: &RawFrame,
    recipe: &EditRecipe,
    options: PreviewOptions,
    variants: &[DetailSettings],
    cancellation: &CancellationToken,
) -> Result<DemosaicedBase, PipelineError> {
    let peak = validate_peak(frame, recipe, options, variants)?;
    let source = prepare_camera_source(frame, recipe, options, cancellation)?;

    let copies_started = Instant::now();
    let mut images = Vec::with_capacity(variants.len());
    for &settings in variants {
        let variant = recipe.with_detail_settings(settings);
        let captured = source
            .clone()
            .with_recipe(&variant, options)?
            .capture_cpu(cancellation)?;
        let reconstructed = complete_camera_source(optics, captured, cancellation)?;
        images.push(LocalDetailImage::new(prepare_demosaiced_preview_owned(
            reconstructed,
            &variant,
            cancellation,
        )?));
    }
    let local_detail = copies_started.elapsed();

    let captured = source.capture_cpu(cancellation)?;
    let reconstructed = complete_camera_source(optics, captured, cancellation)?;
    let preparation = reconstructed.timings;
    let mut base = prepare_demosaiced_preview_owned(reconstructed, recipe, cancellation)?;
    let color_conversion = base.timings.color_conversion;
    base.timings = preparation;
    base.timings.color_conversion = color_conversion;
    base.timings.local_detail = local_detail;
    base.timings.total += color_conversion + local_detail;
    base.preparation_peak_bytes = base.preparation_peak_bytes.max(peak);
    base.local_detail = images;
    Ok(base)
}

/// Checks the working set before anything is allocated, since the stages only see their own
/// buffers: the finished copies stay alive while the next one is developed.
///
/// At the peak, the last copy is being developed: the other copies at output size, the camera
/// source, its working copy, and either the detail scratch or the optics output. Developing the
/// base afterwards needs no more: it consumes the source in place, so it trades the source for
/// one more finished image, which `preview_dimensions` never makes larger than the source.
fn validate_peak(
    frame: &RawFrame,
    recipe: &EditRecipe,
    options: PreviewOptions,
    variants: &[DetailSettings],
) -> Result<usize, PipelineError> {
    let (width, height) = (frame.info.width, frame.info.height);
    let overflow = || dimension_overflow(width, height);
    let image_bytes =
        |width: usize, height: usize| width.checked_mul(height)?.checked_mul(3 * size_of::<f32>());
    let (target_width, target_height) = preview_dimensions(width, height, options.max_long_edge)?;
    let source = image_bytes(width, height).ok_or_else(overflow)?;
    let finished = image_bytes(target_width, target_height).ok_or_else(overflow)?;
    debug_assert!(finished <= source, "the peak below relies on this");

    let mut scratch = source; // The optics output, at least as large as the source.
    for recipe in std::iter::once(recipe.clone())
        .chain(variants.iter().map(|s| recipe.with_detail_settings(*s)))
    {
        super::super::detail::validate_working_set(frame, &recipe)?;
        scratch = scratch.max(super::super::detail::scratch_bytes(&recipe, width, height)?);
        scratch = scratch.max(super::super::defringe::scratch_bytes(
            &recipe, width, height,
        )?);
    }
    let raw = frame
        .mosaic
        .len()
        .checked_mul(size_of::<u16>())
        .ok_or_else(overflow)?;
    let peak = finished
        .checked_mul(variants.len().saturating_sub(1))
        .and_then(|bytes| bytes.checked_add(source.checked_mul(2)?))
        .and_then(|bytes| bytes.checked_add(scratch))
        .and_then(|bytes| bytes.checked_add(raw))
        .ok_or_else(overflow)?;
    validate_working_set(peak)?;
    Ok(peak)
}
