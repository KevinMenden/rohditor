//! The red tint drawn over the preview for the selected mask: what it shows, when it is out of
//! date, and how it is made.
//!
//! The open document keeps the [`TintSource`] of the tint that the frontend shows and asks
//! [`plan_tint`] what to do about it, see `OpenDocument::refresh_mask_tint`.

use rohditor_core::{MaskAlphaImage, RawCropPolicy};
use rohditor_edit::{EditRecipe, GeometryAdjustments, MaskComponent, MaskLayer};
use rohditor_raw::RawFileInfo;

use super::MaskToolSession;

/// Peak opacity of the red mask tint, at alpha 1.
const OVERLAY_TINT_OPACITY: f32 = 0.5;

/// Long edge, in pixels, of the tint image. It is a soft red wash, so a small image stretched
/// over the preview is enough, and it is recomputed on every frame of a handle drag.
const TINT_LONG_EDGE: usize = 512;

/// What a tint shows: the shape of one layer in the displayed frame. Opacity, sliders and
/// everything else in the recipe do not change it, so a slider drag never recomputes it.
#[derive(Debug, Clone, PartialEq)]
pub struct TintSource {
    layer: u32,
    components: Vec<MaskComponent>,
    /// Orientation and crop of the displayed image.
    geometry: GeometryAdjustments,
    /// Sets the developed frame, and with it the aspect the mask lives in.
    policy: RawCropPolicy,
    /// For a layer with range components, whose shape depends on the image: the recipe revision
    /// it was measured at. `None` for a layer that the recipe alone determines.
    revision: Option<u64>,
}

impl TintSource {
    pub fn new(
        recipe: &EditRecipe,
        layer: &MaskLayer,
        policy: RawCropPolicy,
        revision: Option<u64>,
    ) -> Self {
        Self {
            layer: layer.id,
            components: layer.components.clone(),
            geometry: recipe.geometry.clone(),
            policy,
            revision,
        }
    }

    pub const fn layer(&self) -> u32 {
        self.layer
    }

    /// The source of a tint of layer `layer` that was measured at `revision` (see
    /// [`TintPlan::Measure`]), or `None` when the recipe has no such layer.
    pub fn measured(
        recipe: &EditRecipe,
        layer: u32,
        policy: RawCropPolicy,
        revision: u64,
    ) -> Option<Self> {
        let layer = recipe.local.layer(layer)?;
        Some(Self::new(recipe, layer, policy, Some(revision)))
    }

    /// Whether the tint of this source is still right, without copying the layer.
    pub fn matches(
        &self,
        recipe: &EditRecipe,
        layer: &MaskLayer,
        policy: RawCropPolicy,
        revision: Option<u64>,
    ) -> bool {
        self.layer == layer.id
            && self.revision == revision
            && self.policy == policy
            && self.geometry == recipe.geometry
            && self.components == layer.components
    }
}

/// What to do about the tint this frame.
#[derive(Debug, PartialEq)]
pub enum TintPlan {
    /// Leave the shown tint as it is (and show none if there is none).
    Keep,
    /// Show no tint: the selected layer is gone, or no tint can be made for it.
    Clear,
    /// The layer selects by image content, so the worker must measure its overlay (see
    /// `RenderCoordinator::render_mask_overlay`) and the tint follows a little later. The old
    /// tint stays until then, so dragging a range does not make it flicker. Install the result
    /// with [`TintSource::measured`].
    Measure { layer: u32 },
    /// Show `alpha` as the tint of `source`.
    Render {
        source: TintSource,
        alpha: MaskAlphaImage,
    },
}

/// Decides whether the tint `shown` is up to date with the recipe and the selected layer. A
/// gradient or brush is computed here, from the recipe, at a small size: a render would show
/// the mask only after the whole preview was done, so the tint would trail the handles while
/// they are dragged.
///
/// `revision` is the recipe's revision, and `info` the open photo's, `None` while it has no
/// frame to measure the mask against.
pub fn plan_tint(
    shown: Option<&TintSource>,
    session: &MaskToolSession,
    recipe: &EditRecipe,
    revision: u64,
    info: Option<&RawFileInfo>,
    policy: RawCropPolicy,
) -> TintPlan {
    let Some(layer) = session.overlay_layer() else {
        return TintPlan::Keep;
    };
    let Some(selected) = recipe.local.layer(layer) else {
        // The selection is stale (the layer was deleted): show nothing rather than its tint.
        return TintPlan::Clear;
    };
    let ranged = selected
        .components
        .iter()
        .any(|component| component.shape.is_range());
    let revision = ranged.then_some(revision);
    if shown.is_some_and(|tint| tint.matches(recipe, selected, policy, revision)) {
        return TintPlan::Keep;
    }
    if ranged {
        return TintPlan::Measure { layer };
    }
    let Some(info) = info else {
        return TintPlan::Keep;
    };
    // The developed frame (the sensor area the pipeline develops, before the user's crop and
    // orientation) is what masks are normalized over. The RAW's full sensor size is not it:
    // masked borders are cut off, which changes the aspect and with it every long-edge length.
    let Ok(developed) = rohditor_core::raw_crop_dimensions(info, policy) else {
        return TintPlan::Clear;
    };
    let alpha = rohditor_core::render_mask_overlay_preview(
        recipe,
        layer,
        developed,
        info.orientation,
        TINT_LONG_EDGE,
    );
    match alpha {
        Ok(Some(alpha)) => TintPlan::Render {
            source: TintSource::new(recipe, selected, policy, None),
            alpha,
        },
        // No such layer (or an unusable recipe): show nothing rather than a stale tint.
        Ok(None) | Err(_) => TintPlan::Clear,
    }
}

/// The tint as RGBA pixels (not premultiplied): red, as opaque as the mask is strong.
pub fn tint_pixels(alpha: &[u8]) -> Vec<[u8; 4]> {
    alpha
        .iter()
        .map(|&value| {
            let strength = (f32::from(value) * OVERLAY_TINT_OPACITY).round() as u8;
            [255, 0, 0, strength]
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::{LayerEdit, LocalSlider, MaskEditMode};
    use rohditor_edit::{
        LinearGradient, LocalSettings, LuminanceRange, MaskMode, MaskPoint, MaskShape,
    };
    use rohditor_image::Orientation;

    fn recipe_with_layer() -> EditRecipe {
        let mut recipe = EditRecipe::default();
        recipe.local.layers.push(MaskLayer {
            id: 0,
            name: "Mask 1".to_owned(),
            enabled: true,
            opacity: 1.0,
            components: vec![MaskComponent {
                mode: MaskMode::Add,
                invert: false,
                shape: MaskShape::Linear(LinearGradient {
                    start: MaskPoint { x: 0.1, y: 0.1 },
                    end: MaskPoint { x: 0.9, y: 0.9 },
                }),
            }],
            settings: LocalSettings::default(),
        });
        recipe
    }

    fn selecting(layer: u32) -> MaskToolSession {
        MaskToolSession {
            selected_layer: Some(layer),
            ..MaskToolSession::default()
        }
    }

    #[test]
    fn the_tint_depends_on_the_shape_and_frame_but_not_on_sliders() {
        let policy = RawCropPolicy::default();
        let recipe = recipe_with_layer();
        let source = TintSource::new(&recipe, &recipe.local.layers[0], policy, None);
        let still_matches = |recipe: &EditRecipe, policy| {
            source.matches(recipe, &recipe.local.layers[0], policy, None)
        };
        assert!(still_matches(&recipe, policy));

        let slider = LayerEdit::Slider(LocalSlider::Exposure, 1.0)
            .apply(&recipe, 0)
            .expect("changes the layer");
        assert!(still_matches(&slider, policy));
        let opacity = LayerEdit::Slider(LocalSlider::Opacity, 0.3)
            .apply(&recipe, 0)
            .expect("changes the layer");
        assert!(still_matches(&opacity, policy));

        let mut moved = recipe.clone();
        moved.local.layers[0].components[0].invert = true;
        assert!(!still_matches(&moved, policy));
        let mut turned = recipe.clone();
        turned.geometry.orientation_override = Some(Orientation::Rotate90);
        assert!(!still_matches(&turned, policy));
        assert!(!still_matches(&recipe, RawCropPolicy::ActiveArea));
    }

    #[test]
    fn the_tint_of_a_range_layer_is_tied_to_the_recipe_revision_it_was_measured_at() {
        let policy = RawCropPolicy::default();
        let recipe = recipe_with_layer();
        let layer = &recipe.local.layers[0];
        let measured = TintSource::measured(&recipe, 0, policy, 4).expect("layer exists");
        // The same recipe at another revision may look different (exposure, white balance), and a
        // tint of a layer the recipe alone determines is never the tint of a measured one.
        assert!(measured.matches(&recipe, layer, policy, Some(4)));
        assert!(!measured.matches(&recipe, layer, policy, Some(5)));
        assert!(!measured.matches(&recipe, layer, policy, None));
        let plain = TintSource::new(&recipe, layer, policy, None);
        assert!(!plain.matches(&recipe, layer, policy, Some(4)));
        assert_eq!(TintSource::measured(&recipe, 9, policy, 4), None);
    }

    #[test]
    fn nothing_is_planned_without_a_visible_selected_layer() {
        let policy = RawCropPolicy::default();
        let recipe = recipe_with_layer();
        let plan = |session: &MaskToolSession| plan_tint(None, session, &recipe, 1, None, policy);
        assert_eq!(plan(&MaskToolSession::default()), TintPlan::Keep);
        let mut hidden = selecting(0);
        hidden.show_overlay = false;
        assert_eq!(plan(&hidden), TintPlan::Keep);
        // Painting shows the tint whatever the setting says.
        hidden.mode = MaskEditMode::Paint { erase: false };
        assert_eq!(plan(&hidden), TintPlan::Keep, "no frame to measure against");
        // A stale selection shows nothing rather than the tint of a layer that is gone.
        assert_eq!(plan(&selecting(7)), TintPlan::Clear);
    }

    #[test]
    fn a_layer_selected_by_image_content_is_measured_by_the_worker() {
        let policy = RawCropPolicy::default();
        let mut recipe = recipe_with_layer();
        recipe.local.layers[0].components.push(MaskComponent {
            mode: MaskMode::Intersect,
            invert: false,
            shape: MaskShape::Luminance(LuminanceRange::default()),
        });
        let session = selecting(0);
        assert_eq!(
            plan_tint(None, &session, &recipe, 3, None, policy),
            TintPlan::Measure { layer: 0 }
        );
        // Once measured at this revision it stays, until the recipe moves on.
        let shown = TintSource::measured(&recipe, 0, policy, 3).expect("layer exists");
        assert_eq!(
            plan_tint(Some(&shown), &session, &recipe, 3, None, policy),
            TintPlan::Keep
        );
        assert_eq!(
            plan_tint(Some(&shown), &session, &recipe, 4, None, policy),
            TintPlan::Measure { layer: 0 }
        );
    }

    #[test]
    fn the_tint_is_red_and_as_opaque_as_the_mask_is_strong() {
        assert_eq!(
            tint_pixels(&[0, 100, 255]),
            [[255, 0, 0, 0], [255, 0, 0, 50], [255, 0, 0, 128]]
        );
    }
}
