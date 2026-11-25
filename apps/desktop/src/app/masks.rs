//! Masks tool wiring: panel actions, viewport gestures, shortcuts and the mask overlay texture.
//!
//! The tool's behaviour lives in `rohditor_engine::tools` (`MaskToolSession`, `LayerEdit`);
//! this module connects it to egui input, the edit history and preview scheduling. Every change
//! goes into the recipe as it happens, so leaving the tool has nothing to commit.

use eframe::egui;
use rohditor_core::{MaskAlphaImage, OutputGeometry, PreviewOptions, RawCropPolicy};
use rohditor_edit::ColorSample;
use rohditor_engine::coordinator::RenderCoordinator;
use rohditor_engine::document::{EditSession, PreviewTicket};
use rohditor_engine::tools::{
    Bracket, BrushKeySteps, ColorPick, MaskAction, MaskToolSession, MaskView, TintPlan, TintSource,
    ToolKind, plan_tint, tint_pixels,
};
use rohditor_raw::RawFrame;

use super::tools::tool_panel_model;
use super::{Document, RohditorApp};
use crate::ui::mask_overlay::{MaskOverlayModel, MaskOverlayOutput};
use crate::ui::masks::{self, MasksPanelModel};
use crate::ui::tool_panel;

/// What the viewport needs to draw and drive the Masks tool, or `None` while the photo has
/// no frame to map pointer positions onto.
pub(super) fn overlay_model<'a>(
    session: &'a MaskToolSession,
    edits: &'a EditSession,
    frame: Option<&RawFrame>,
    policy: RawCropPolicy,
    mask_overlay: Option<&'a MaskTint>,
) -> Option<MaskOverlayModel<'a>> {
    let recipe = edits.recipe();
    let geometry = OutputGeometry::of_photo(&frame?.info, policy, &recipe.geometry).ok()?;
    Some(MaskOverlayModel {
        session,
        recipe,
        geometry,
        // A tint belongs to the layer it was rendered for: not drawn once that layer is gone.
        tint: session
            .overlay_layer()
            .filter(|id| recipe.local.layer(*id).is_some())
            .and(mask_overlay.map(|tint| &tint.texture)),
    })
}

/// The tint texture and what it was computed from.
pub(super) struct MaskTint {
    source: TintSource,
    texture: egui::TextureHandle,
}

/// Brings the tint up to date with the recipe and the selected layer, as `plan_tint` decides. A
/// layer with a luminance or color range depends on the image, so the worker measures it (see
/// [`request_range_overlay`]) and the tint follows the edits a little later.
pub(super) fn refresh_tint(
    context: &egui::Context,
    document: &mut Document,
    session: &MaskToolSession,
    policy: RawCropPolicy,
    coordinator: &RenderCoordinator,
    options: PreviewOptions,
) {
    let plan = plan_tint(
        document.mask_overlay.as_ref().map(|tint| &tint.source),
        session,
        document.core.edits.recipe(),
        document.core.edits.revision(),
        document.core.frame.as_ref().map(|frame| &frame.info),
        policy,
    );
    match plan {
        TintPlan::Keep => {}
        TintPlan::Clear => document.mask_overlay = None,
        TintPlan::Measure { layer } => request_range_overlay(document, layer, coordinator, options),
        TintPlan::Render { source, alpha } => install_tint(context, document, source, &alpha),
    }
}

/// Asks the worker for the overlay of a layer that selects by image content. The old tint stays
/// until the new one arrives, so dragging a range does not make it flicker.
fn request_range_overlay(
    document: &mut Document,
    layer: u32,
    coordinator: &RenderCoordinator,
    options: PreviewOptions,
) {
    let revision = document.core.edits.revision();
    if document.range_overlay_requested == Some((revision, layer))
        || document.core.open_status.is_some()
    {
        return;
    }
    let Some(frame) = document.core.frame.clone() else {
        return;
    };
    let ticket = document.core.ticket();
    let recipe = document.core.edits.recipe().clone();
    // A failed request is remembered too, so it is not repeated every frame.
    document.range_overlay_requested = Some((revision, layer));
    if coordinator
        .render_mask_overlay(ticket, frame, recipe, layer, options)
        .is_err()
    {
        document.mask_overlay = None;
    }
}

/// Puts `alpha` on screen as the red tint of the layer `source` describes.
fn install_tint(
    context: &egui::Context,
    document: &mut Document,
    source: TintSource,
    alpha: &MaskAlphaImage,
) {
    let pixels = tint_pixels(alpha.data())
        .into_iter()
        .map(|[r, g, b, a]| egui::Color32::from_rgba_premultiplied(r, g, b, a))
        .collect();
    let image = egui::ColorImage::new([alpha.width(), alpha.height()], pixels);
    match document.mask_overlay.as_mut() {
        Some(tint) => {
            tint.texture.set(image, egui::TextureOptions::LINEAR);
            tint.source = source;
        }
        None => {
            document.mask_overlay = Some(MaskTint {
                source,
                texture: context.load_texture(
                    format!("document-{}-mask-tint", document.core.id),
                    image,
                    egui::TextureOptions::LINEAR,
                ),
            });
        }
    }
}

/// Reads the brush keys from the frame's key events, so held keys (which repeat) and several
/// presses in one frame all count. `{` and `}` are what Shift+`[` and Shift+`]` produce on many
/// layouts, so they mean the same as the shifted brackets. What the steps do is the engine's.
fn brush_key_steps(input: &egui::InputState) -> BrushKeySteps {
    let mut steps = BrushKeySteps::default();
    for event in &input.events {
        let egui::Event::Key {
            key,
            pressed: true,
            modifiers,
            ..
        } = event
        else {
            continue;
        };
        if modifiers.ctrl || modifiers.command {
            continue;
        }
        let (bracket, curly) = match key {
            egui::Key::CloseBracket => (Bracket::Close, false),
            egui::Key::OpenBracket => (Bracket::Open, false),
            egui::Key::CloseCurlyBracket => (Bracket::Close, true),
            egui::Key::OpenCurlyBracket => (Bracket::Open, true),
            _ => continue,
        };
        steps.press(bracket, modifiers.shift || curly);
    }
    steps
}

impl RohditorApp {
    pub(super) fn show_masks_controls(&mut self, root: &mut egui::Ui) {
        let context = &root.ctx().clone();
        let (Some(session), Some(document)) = (self.tool.masks(), self.document.as_ref()) else {
            return;
        };
        let model = MasksPanelModel {
            layers: &document.core.edits.recipe().local.layers,
            selected_layer: session.selected_layer,
            selected_component: session.selected_component,
            mode: session.mode,
            brush: session.brush,
            show_overlay: session.show_overlay,
            limit: session.limit_reached(),
        };
        let panel = tool_panel_model(Some(document), ToolKind::Masks);
        let actions = tool_panel::show(root, &panel, |ui| masks::show(ui, &model));
        let mut changed = false;
        for action in actions {
            changed |= self.apply_mask_action(action);
        }
        self.finish_mask_interaction(context, changed);
    }

    /// Pointer events of the viewport: forwarded to the session as one gesture per drag.
    pub(super) fn apply_mask_pointer(
        &mut self,
        context: &egui::Context,
        output: &MaskOverlayOutput,
    ) {
        let Some(view) = output.view else {
            return;
        };
        let changed = self.forward_mask_pointer(output, &view);
        self.finish_mask_interaction(context, changed);
        if let Some((position, pick)) = output.pick_color {
            self.request_color_sample(position, pick);
        }
    }

    /// Asks the worker for the colour under a click of the color-range picker. The sample is
    /// measured on the developed image (after the global exposure and base rendering), the same
    /// reference the mask selects on, so it comes back as an event.
    fn request_color_sample(&mut self, position: (f64, f64), pick: ColorPick) {
        if self.pending_color_pick.is_some() {
            return;
        }
        let options = self.preview_options();
        let Some(document) = self.document.as_mut() else {
            return;
        };
        let Some(frame) = document.core.frame.clone() else {
            document.core.error =
                Some("Open a developed RAW preview before picking colors".to_owned());
            return;
        };
        let ticket = document.core.ticket();
        let recipe = document.core.edits.recipe().clone();
        match self
            .coordinator
            .sample_color_range(ticket, frame, recipe, position, pick, options)
        {
            Ok(()) => self.pending_color_pick = Some(ticket),
            Err(error) => document.core.error = Some(error),
        }
    }

    /// A picked colour arrived: it edits the color range, unless the recipe moved on meanwhile.
    pub(super) fn apply_color_sample(
        &mut self,
        context: &egui::Context,
        ticket: PreviewTicket,
        pick: ColorPick,
        sample: ColorSample,
    ) {
        if self.pending_color_pick != Some(ticket) {
            return;
        }
        self.pending_color_pick = None;
        let (Some(document), Some(session)) = (self.document.as_mut(), self.tool.masks_mut())
        else {
            return;
        };
        if document.core.id != ticket.document_id
            || document.core.edits.revision() != ticket.revision
        {
            return;
        }
        let recipe = document.core.edits.recipe().clone();
        let changed = session
            .pick_color(&recipe, sample, pick)
            .is_some_and(|next| document.core.edits.set_discrete(next));
        self.finish_mask_interaction(context, changed);
    }

    pub(super) fn fail_color_sample(&mut self, ticket: PreviewTicket, message: String) {
        if self.pending_color_pick != Some(ticket) {
            return;
        }
        self.pending_color_pick = None;
        if let Some(document) = self
            .document
            .as_mut()
            .filter(|document| document.core.id == ticket.document_id)
        {
            document.core.error = Some(message);
        }
    }

    /// The worker measured the overlay of a range layer. It is shown if the recipe is still the
    /// one it was measured for; otherwise `refresh_tint` has already asked for a newer one.
    pub(super) fn install_range_overlay(
        &mut self,
        context: &egui::Context,
        ticket: PreviewTicket,
        layer: u32,
        overlay: &MaskAlphaImage,
    ) {
        let policy = self.settings.render_options().raw_crop_policy;
        let Some(document) = self
            .document
            .as_mut()
            .filter(|document| document.core.id == ticket.document_id)
        else {
            return;
        };
        if document.range_overlay_requested == Some((ticket.revision, layer)) {
            document.range_overlay_requested = None;
        }
        if document.core.edits.revision() != ticket.revision {
            return;
        }
        let Some(source) =
            TintSource::measured(document.core.edits.recipe(), layer, policy, ticket.revision)
        else {
            return;
        };
        install_tint(context, document, source, overlay);
        context.request_repaint();
    }

    /// The overlay could not be measured: show no tint rather than one that no longer matches.
    /// A failure for a recipe the user has since edited says nothing about the current one.
    pub(super) fn fail_range_overlay(&mut self, ticket: PreviewTicket) {
        if let Some(document) = self.document.as_mut().filter(|document| {
            document.core.id == ticket.document_id
                && document.core.edits.revision() == ticket.revision
        }) {
            document.mask_overlay = None;
        }
    }

    fn forward_mask_pointer(&mut self, output: &MaskOverlayOutput, view: &MaskView) -> bool {
        let (Some(document), Some(session)) = (self.document.as_mut(), self.tool.masks_mut())
        else {
            return false;
        };
        session.drive_pointer(&mut document.core.edits, &output.events, view)
    }

    /// A changed recipe needs a new preview. Selection and overlay changes do not: the tint is
    /// drawn from the recipe, not from a render.
    fn finish_mask_interaction(&mut self, context: &egui::Context, changed: bool) {
        if !changed {
            return;
        }
        let Some(document) = self.document.as_mut() else {
            return;
        };
        document.core.notice = None;
        let id = document.core.id;
        self.queue_preview(context, id);
    }

    /// One panel action. Returns whether the recipe changed.
    fn apply_mask_action(&mut self, action: MaskAction) -> bool {
        let (Some(document), Some(session)) = (self.document.as_mut(), self.tool.masks_mut())
        else {
            return false;
        };
        session.apply_action(&mut document.core.edits, action)
    }

    /// O, Delete, Backspace and Esc inside the Masks tool, and while painting `[`, `]` (size),
    /// Shift+`[`, Shift+`]` (feather) and E (erase). The tool's own key, M, is handled with the
    /// other tool shortcuts. True when a key was used. A running drag owns the keyboard: the keys
    /// only count once it ends.
    ///
    /// E is the Edit tool's key everywhere else. Painting takes it for the eraser, so Esc (leave
    /// painting) comes first there; the rail's Edit button works at any time.
    pub(super) fn handle_mask_shortcuts(&mut self, context: &egui::Context) -> bool {
        if self.tool.masks().is_none_or(MaskToolSession::is_dragging) {
            return false;
        }
        let pressed =
            |key| context.input(|input| input.modifiers.is_none() && input.key_pressed(key));
        let painting = self.tool.masks().is_some_and(MaskToolSession::is_painting);
        let brush_steps = context.input(brush_key_steps);
        let mut changed = false;
        let mut used = true;
        let action = if painting && !brush_steps.is_empty() {
            Some(MaskAction::BrushKeys(brush_steps))
        } else if painting && pressed(egui::Key::E) {
            Some(MaskAction::ToggleErase)
        } else if pressed(egui::Key::O) {
            Some(MaskAction::ToggleOverlay)
        } else if pressed(egui::Key::Delete) || pressed(egui::Key::Backspace) {
            Some(MaskAction::DeleteSelected)
        } else if pressed(egui::Key::Escape) {
            used = self
                .tool
                .masks_mut()
                .is_some_and(MaskToolSession::cancel_create);
            None
        } else {
            used = false;
            None
        };
        if let Some(action) = action {
            changed = self.apply_mask_action(action);
        }
        if used {
            self.finish_mask_interaction(context, changed);
        }
        used
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn keys(events: Vec<(egui::Key, egui::Modifiers)>) -> BrushKeySteps {
        let events = events
            .into_iter()
            .map(|(key, modifiers)| egui::Event::Key {
                key,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers,
            })
            .collect();
        let mut input = egui::InputState::default();
        input.events = events;
        brush_key_steps(&input)
    }

    #[test]
    fn brackets_resize_and_shifted_brackets_change_the_feather() {
        let none = egui::Modifiers::NONE;
        let shift = egui::Modifiers::SHIFT;
        assert_eq!(
            keys(vec![
                (egui::Key::CloseBracket, none),
                (egui::Key::CloseBracket, none),
                (egui::Key::OpenBracket, none),
            ]),
            BrushKeySteps {
                size: 1,
                feather: 0
            }
        );
        assert_eq!(
            keys(vec![(egui::Key::OpenBracket, shift)]),
            BrushKeySteps {
                size: 0,
                feather: -1
            }
        );
        // Layouts that report Shift+] as `}` (and may not set the shift modifier on it).
        assert_eq!(
            keys(vec![(egui::Key::CloseCurlyBracket, none)]),
            BrushKeySteps {
                size: 0,
                feather: 1
            }
        );
        // Shortcuts with Ctrl are not ours.
        assert_eq!(
            keys(vec![(egui::Key::CloseBracket, egui::Modifiers::CTRL)]),
            BrushKeySteps::default()
        );
    }
}
