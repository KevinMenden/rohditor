//! The red tint over the preview for the selected mask: which one the frontend shows, and the one
//! being measured.

use rohditor_core::{MaskAlphaImage, PreviewOptions, RawCropPolicy};

use super::OpenDocument;
use crate::coordinator::RenderCoordinator;
use crate::document::PreviewTicket;
use crate::tools::{MaskToolSession, TintPlan, TintSource, plan_tint};

#[derive(Default)]
pub(super) struct MaskTint {
    /// What the tint that the frontend shows was made from.
    shown: Option<TintSource>,
    /// The (recipe revision, layer) whose overlay was asked of the worker. It stays set after a
    /// failure, so that a failing request is not repeated every frame.
    requested: Option<(u64, u32)>,
}

/// What the frontend does with the tint it shows.
#[derive(Debug, PartialEq)]
#[must_use]
pub enum TintUpdate {
    Keep,
    Clear,
    /// Replace it with this one, as [`tint_pixels`](crate::tools::tint_pixels).
    Show(MaskAlphaImage),
}

impl OpenDocument {
    /// Bring the tint up to date with the recipe and the selected mask. A mask with a luminance or
    /// color range depends on the photo, so the worker measures it and the tint follows with
    /// [`Self::mask_overlay_ready`]. The old one stays until then, so that dragging a range does
    /// not make it flicker.
    pub fn refresh_mask_tint(
        &mut self,
        coordinator: &RenderCoordinator,
        session: &MaskToolSession,
        options: PreviewOptions,
    ) -> TintUpdate {
        let plan = plan_tint(
            self.mask_tint.shown.as_ref(),
            session,
            self.edits.recipe(),
            self.edits.revision(),
            self.frame.as_ref().map(|frame| &frame.info),
            options.render.raw_crop_policy,
        );
        match plan {
            TintPlan::Keep => TintUpdate::Keep,
            TintPlan::Clear => self.clear_mask_tint(),
            TintPlan::Measure { layer } => self.measure_mask(coordinator, layer, options),
            TintPlan::Render { source, alpha } => {
                self.mask_tint.shown = Some(source);
                TintUpdate::Show(alpha)
            }
        }
    }

    fn measure_mask(
        &mut self,
        coordinator: &RenderCoordinator,
        layer: u32,
        options: PreviewOptions,
    ) -> TintUpdate {
        let request = (self.edits.revision(), layer);
        if self.mask_tint.requested == Some(request) || self.open_status.is_some() {
            return TintUpdate::Keep;
        }
        let Some(frame) = self.frame.clone() else {
            return TintUpdate::Keep;
        };
        self.mask_tint.requested = Some(request);
        let recipe = self.edits.recipe().clone();
        match coordinator.render_mask_overlay(self.ticket(), frame, recipe, layer, options) {
            Ok(()) => TintUpdate::Keep,
            Err(_) => self.clear_mask_tint(),
        }
    }

    /// The worker measured a mask. It is shown if the recipe is still the one it was measured
    /// for; otherwise a newer one is already on its way.
    pub fn mask_overlay_ready(
        &mut self,
        ticket: PreviewTicket,
        layer: u32,
        overlay: MaskAlphaImage,
        policy: RawCropPolicy,
    ) -> TintUpdate {
        if self.mask_tint.requested == Some((ticket.revision, layer)) {
            self.mask_tint.requested = None;
        }
        if self.edits.revision() != ticket.revision {
            return TintUpdate::Keep;
        }
        match TintSource::measured(self.edits.recipe(), layer, policy, ticket.revision) {
            Some(source) => {
                self.mask_tint.shown = Some(source);
                TintUpdate::Show(overlay)
            }
            None => TintUpdate::Keep,
        }
    }

    /// A mask could not be measured: show no tint rather than one that no longer matches. A
    /// failure for a recipe that was edited since says nothing about the current one.
    pub fn mask_overlay_failed(&mut self, ticket: PreviewTicket) -> TintUpdate {
        if self.edits.revision() == ticket.revision {
            self.clear_mask_tint()
        } else {
            TintUpdate::Keep
        }
    }

    /// The frontend no longer shows a tint.
    pub fn forget_mask_tint(&mut self) {
        self.mask_tint = MaskTint::default();
    }

    /// Whether the tint that the frontend has belongs over the photo: it is the one of the mask
    /// that `session` shows.
    pub fn shows_mask_tint(&self, session: &MaskToolSession) -> bool {
        session.overlay_layer().is_some_and(|layer| {
            let shown = self.mask_tint.shown.as_ref();
            shown.is_some_and(|source| source.layer() == layer)
                && self.edits.recipe().local.layer(layer).is_some()
        })
    }

    fn clear_mask_tint(&mut self) -> TintUpdate {
        self.mask_tint.shown = None;
        TintUpdate::Clear
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use rohditor_core::render_mask_overlay_preview;
    use rohditor_edit::{
        EditRecipe, LinearGradient, LocalSettings, LuminanceRange, MaskComponent, MaskLayer,
        MaskMode, MaskPoint, MaskShape,
    };
    use rohditor_image::Orientation;

    use super::*;

    const POLICY: RawCropPolicy = RawCropPolicy::Recommended;

    fn layer(shape: MaskShape) -> MaskLayer {
        MaskLayer {
            id: 3,
            name: "Mask".to_owned(),
            enabled: true,
            opacity: 1.0,
            components: vec![MaskComponent {
                mode: MaskMode::Add,
                invert: false,
                shape,
            }],
            settings: LocalSettings::default(),
        }
    }

    /// A document with a mask that the worker has to measure, and that was asked to.
    fn measuring() -> (OpenDocument, PreviewTicket) {
        let mut recipe = EditRecipe::default();
        recipe.local.layers = vec![layer(MaskShape::Luminance(LuminanceRange::default()))];
        let mut document = OpenDocument::opening(1, PathBuf::from("photo.arw"), recipe, None);
        let ticket = document.ticket();
        document.mask_tint.requested = Some((ticket.revision, 3));
        (document, ticket)
    }

    fn overlay() -> MaskAlphaImage {
        let mut recipe = EditRecipe::default();
        recipe.local.layers = vec![layer(MaskShape::Linear(LinearGradient {
            start: MaskPoint { x: 0.1, y: 0.5 },
            end: MaskPoint { x: 0.9, y: 0.5 },
        }))];
        render_mask_overlay_preview(&recipe, 3, (8, 4), Orientation::Normal, 8)
            .expect("it renders")
            .expect("the layer exists")
    }

    fn selecting(layer: u32) -> MaskToolSession {
        let mut session = MaskToolSession::default();
        session.selected_layer = Some(layer);
        session
    }

    fn expose(document: &mut OpenDocument) {
        let mut next = document.edits.recipe().clone();
        next.light.exposure_ev = 0.5;
        assert!(document.edits.set_discrete(next));
    }

    #[test]
    fn a_measured_mask_is_shown_for_its_layer_only() {
        let (mut document, ticket) = measuring();
        assert!(!document.shows_mask_tint(&selecting(3)), "nothing to show");
        let update = document.mask_overlay_ready(ticket, 3, overlay(), POLICY);
        assert_eq!(update, TintUpdate::Show(overlay()));
        assert_eq!(document.mask_tint.requested, None);
        assert!(document.shows_mask_tint(&selecting(3)));
        assert!(!document.shows_mask_tint(&selecting(4)));
        let mut hidden = selecting(3);
        hidden.show_overlay = false;
        assert!(!document.shows_mask_tint(&hidden));

        assert!(document.edits.set_discrete(EditRecipe::default()));
        assert!(
            !document.shows_mask_tint(&selecting(3)),
            "the layer is gone"
        );
    }

    #[test]
    fn the_overlay_of_an_older_recipe_is_not_shown() {
        let (mut document, ticket) = measuring();
        expose(&mut document);
        let update = document.mask_overlay_ready(ticket, 3, overlay(), POLICY);
        assert_eq!(update, TintUpdate::Keep);
        assert_eq!(document.mask_overlay_failed(ticket), TintUpdate::Keep);
    }

    #[test]
    fn a_failure_clears_the_tint_and_is_not_repeated() {
        let (mut document, ticket) = measuring();
        assert_eq!(
            document.mask_overlay_ready(ticket, 3, overlay(), POLICY),
            TintUpdate::Show(overlay())
        );
        document.mask_tint.requested = Some((ticket.revision, 3));
        assert_eq!(document.mask_overlay_failed(ticket), TintUpdate::Clear);
        assert!(!document.shows_mask_tint(&selecting(3)));
        assert_eq!(document.mask_tint.requested, Some((ticket.revision, 3)));
        document.forget_mask_tint();
        assert_eq!(document.mask_tint.requested, None);
    }
}
