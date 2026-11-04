//! Masks tool wiring: panel actions, viewport gestures and the mask overlay texture.
//!
//! The tool's behaviour lives in `rohditor_engine::tools` (`MaskToolSession`, `LayerEdit`);
//! this module connects it to egui input, the edit history and preview scheduling. Every change
//! goes into the recipe as it happens, so leaving the tool has nothing to commit.

use eframe::egui;
use rohditor_core::{MaskAlphaImage, OutputGeometry};
use rohditor_edit::EditRecipe;
use rohditor_engine::document::EditSession;
use rohditor_engine::tools::{LayerEdit, MaskToolSession, MaskView, ToolKind};
use rohditor_raw::RawFileInfo;

use super::tools::tool_panel_model;
use super::{Document, RohditorApp};
use crate::ui::mask_overlay::{MaskOverlayModel, MaskOverlayOutput};
use crate::ui::masks::{self, MasksAction, MasksPanelModel, SliderPhase};
use crate::ui::tool_panel;

/// Peak opacity of the red mask tint, at alpha 1.
const OVERLAY_TINT_OPACITY: f32 = 0.5;

/// Orientation and crop of the displayed image over the developed frame. Only the aspect of
/// the frame matters to the Masks tool, so the RAW's own size stands in for the developed one.
fn output_geometry(info: &RawFileInfo, recipe: &EditRecipe) -> Option<OutputGeometry> {
    let orientation = recipe.geometry.orientation_override.unwrap_or(info.orientation);
    OutputGeometry::new(info.width, info.height, orientation, recipe.geometry.crop).ok()
}

/// What the viewport needs to draw and drive the Masks tool, or `None` while the photo has
/// no frame to map pointer positions onto.
pub(super) fn overlay_model<'a>(
    session: &'a MaskToolSession,
    document: &'a Document,
) -> Option<MaskOverlayModel<'a>> {
    let recipe = document.edits.recipe();
    let geometry = output_geometry(&document.frame.as_ref()?.info, recipe)?;
    Some(MaskOverlayModel {
        session,
        recipe,
        geometry,
        tint: document
            .mask_overlay
            .as_ref()
            .filter(|_| session.show_overlay),
    })
}

/// The overlay as an egui texture: red, `OVERLAY_TINT_OPACITY` strong where the mask is 1.
/// An existing texture is updated in place.
pub(super) fn install_overlay(
    context: &egui::Context,
    document: &mut Document,
    alpha: &MaskAlphaImage,
) {
    let pixels = alpha
        .data()
        .iter()
        .map(|&value| {
            let strength = (f32::from(value) * OVERLAY_TINT_OPACITY).round() as u8;
            // Premultiplied red.
            egui::Color32::from_rgba_premultiplied(strength, 0, 0, strength)
        })
        .collect();
    let image = egui::ColorImage::new([alpha.width(), alpha.height()], pixels);
    match document.mask_overlay.as_mut() {
        Some(texture) => texture.set(image, egui::TextureOptions::LINEAR),
        None => {
            document.mask_overlay = Some(context.load_texture(
                format!("document-{}-mask-overlay", document.id),
                image,
                egui::TextureOptions::LINEAR,
            ));
        }
    }
}

/// Installs a recipe the tool returned as part of one pointer gesture.
fn install_during_gesture(edits: &mut EditSession, next: Option<EditRecipe>) -> bool {
    next.is_some_and(|next| edits.set_during_gesture(next))
}

/// Installs a one-step edit of the panel (delete, rename, mode change, …).
fn install_discrete(edits: &mut EditSession, next: Option<EditRecipe>) -> bool {
    next.is_some_and(|next| edits.set_discrete(next))
}

/// A slider event as one edit. A drag is one undo step, as for the global sliders: it starts
/// a gesture, every change joins it, and it ends when the drag stops.
fn apply_slider(edits: &mut EditSession, next: Option<EditRecipe>, phase: SliderPhase) -> bool {
    if phase.drag_started {
        edits.begin_gesture();
    }
    let changed = match next {
        Some(next) if phase.reset => edits.set_discrete(next),
        Some(next) if phase.changed => {
            if phase.dragged || phase.drag_stopped || edits.gesture_active() {
                edits.set_during_gesture(next)
            } else {
                edits.set_discrete(next)
            }
        }
        _ => false,
    };
    if phase.drag_stopped {
        edits.finish_gesture();
    }
    changed
}

impl RohditorApp {
    pub(super) fn show_masks_controls(&mut self, context: &egui::Context) {
        let (Some(session), Some(document)) = (self.tool.masks(), self.document.as_ref()) else {
            return;
        };
        let model = MasksPanelModel {
            layers: &document.edits.recipe().local.layers,
            selected_layer: session.selected_layer,
            selected_component: session.selected_component,
            mode: session.mode,
            show_overlay: session.show_overlay,
            limit: session.limit_reached(),
        };
        let panel = tool_panel_model(Some(document), ToolKind::Masks);
        let actions = tool_panel::show(context, &panel, |ui| masks::show(ui, &model));
        let overlay_before = self.requested_overlay();
        let mut changed = false;
        for action in actions {
            changed |= self.apply_mask_action(action);
        }
        self.finish_mask_interaction(context, changed, overlay_before);
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
        let overlay_before = self.requested_overlay();
        let changed = self.forward_mask_pointer(output, &view);
        self.finish_mask_interaction(context, changed, overlay_before);
    }

    fn forward_mask_pointer(&mut self, output: &MaskOverlayOutput, view: &MaskView) -> bool {
        let (Some(document), Some(session)) = (self.document.as_mut(), self.tool.masks_mut())
        else {
            return false;
        };
        let edits = &mut document.edits;
        if let Some(layer) = output.select_layer {
            session.select_layer(edits.recipe(), layer);
        }
        let mut changed = false;
        if let Some(position) = output.press {
            edits.begin_gesture();
            let next = session.press(edits.recipe(), view, position);
            changed |= install_during_gesture(edits, next);
            if !session.is_dragging() {
                // Nothing was grabbed (or painting hit a limit): there is no gesture to end later.
                edits.finish_gesture();
            }
        }
        if let Some(position) = output.drag_to {
            let next = session.drag_to(edits.recipe(), view, position);
            changed |= install_during_gesture(edits, next);
        }
        if output.release {
            let next = session.release(edits.recipe());
            changed |= install_during_gesture(edits, next);
            edits.finish_gesture();
        }
        changed
    }

    /// Whether the document changed or a different overlay is now wanted: either needs a new
    /// preview, since the overlay is part of the render request.
    fn finish_mask_interaction(
        &mut self,
        context: &egui::Context,
        changed: bool,
        overlay_before: Option<u32>,
    ) {
        let overlay_changed = self.requested_overlay() != overlay_before;
        let Some(document) = self.document.as_mut() else {
            return;
        };
        if changed {
            document.notice = None;
        }
        if changed || overlay_changed {
            let id = document.id;
            self.queue_preview(context, id);
        }
    }

    fn requested_overlay(&self) -> Option<u32> {
        self.tool
            .masks()
            .and_then(MaskToolSession::overlay_layer)
    }

    /// One panel action. Returns whether the recipe changed.
    fn apply_mask_action(&mut self, action: MasksAction) -> bool {
        let (Some(document), Some(session)) = (self.document.as_mut(), self.tool.masks_mut())
        else {
            return false;
        };
        let edits = &mut document.edits;
        let recipe = edits.recipe().clone();
        match action {
            MasksAction::SelectLayer(layer) => session.select_layer(&recipe, layer),
            MasksAction::SelectComponent(component) => session.select_component(&recipe, component),
            MasksAction::NewLayer(shape) => session.create_layer(shape),
            MasksAction::NewComponent(shape, mode) => session.create_component(shape, mode),
            MasksAction::CancelCreate => {
                session.cancel_create();
            }
            MasksAction::SetOverlay(show) => session.show_overlay = show,
            MasksAction::DeleteLayer(layer) => {
                return install_discrete(edits, session.delete_layer(&recipe, layer));
            }
            MasksAction::DeleteComponent(component) => {
                let next = session
                    .selected_layer
                    .and_then(|layer| session.delete_component(&recipe, layer, component));
                return install_discrete(edits, next);
            }
            MasksAction::Edit(layer, edit) => {
                return install_discrete(edits, edit.apply(&recipe, layer));
            }
            MasksAction::Slider(layer, slider, value, phase) => {
                let next = LayerEdit::Slider(slider, value).apply(&recipe, layer);
                return apply_slider(edits, next, phase);
            }
        }
        false
    }

    /// M, O, Delete, Backspace and Esc inside the Masks tool. True when a key was used.
    pub(super) fn handle_mask_shortcuts(&mut self, context: &egui::Context) -> bool {
        if self.tool.masks().is_none() {
            return false;
        }
        let pressed =
            |key| context.input(|input| input.modifiers.is_none() && input.key_pressed(key));
        let overlay_before = self.requested_overlay();
        let mut changed = false;
        let mut used = true;
        if pressed(egui::Key::O) {
            if let Some(session) = self.tool.masks_mut() {
                session.show_overlay = !session.show_overlay;
            }
        } else if pressed(egui::Key::Delete) || pressed(egui::Key::Backspace) {
            if let (Some(document), Some(session)) =
                (self.document.as_mut(), self.tool.masks_mut())
            {
                let recipe = document.edits.recipe().clone();
                changed = install_discrete(&mut document.edits, session.delete_selected(&recipe));
            }
        } else if pressed(egui::Key::Escape) {
            used = self.tool.masks_mut().is_some_and(MaskToolSession::cancel_create);
        } else {
            used = false;
        }
        if used {
            self.finish_mask_interaction(context, changed, overlay_before);
        }
        used
    }

    /// Enters the Masks tool, selecting the first mask so there is something to show.
    pub(super) fn enter_masks_tool(&mut self, context: &egui::Context) {
        if self.tool.masks().is_some() {
            return;
        }
        let Some(document) = self.document.as_ref() else {
            return;
        };
        let mut session = MaskToolSession::default();
        if let Some(first) = document.edits.recipe().local.layers.first() {
            session.select_layer(document.edits.recipe(), first.id);
        }
        let document_id = document.id;
        self.pending_white_balance_pick = None;
        // Entering Masks drops any active picker.
        self.tool.enter_masks(session);
        // The render request now asks for the overlay.
        self.queue_preview(context, document_id);
    }

    /// Leaves the Masks tool for another tool. Nothing to commit; the next preview drops the
    /// overlay request and may return to the GPU.
    pub(super) fn exit_masks_tool(&mut self) -> bool {
        if self.tool.masks().is_none() {
            return false;
        }
        self.tool = rohditor_engine::tools::ActiveTool::default();
        if let Some(document) = self.document.as_mut() {
            document.mask_overlay = None;
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use rohditor_edit::{LinearGradient, MaskPoint, MaskShape};
    use rohditor_engine::tools::LocalSlider;
    use rohditor_image::Orientation;

    use super::*;

    fn recipe_with_layer() -> EditRecipe {
        let mut recipe = EditRecipe::default();
        recipe.local.layers.push(rohditor_edit::MaskLayer {
            id: 0,
            name: "Mask 1".to_owned(),
            enabled: true,
            opacity: 1.0,
            components: vec![rohditor_edit::MaskComponent {
                mode: rohditor_edit::MaskMode::Add,
                invert: false,
                shape: MaskShape::Linear(LinearGradient {
                    start: MaskPoint { x: 0.1, y: 0.1 },
                    end: MaskPoint { x: 0.9, y: 0.9 },
                }),
            }],
            settings: rohditor_edit::LocalSettings::default(),
        });
        recipe
    }

    fn drag(started: bool, stopped: bool) -> SliderPhase {
        SliderPhase {
            changed: true,
            drag_started: started,
            dragged: !stopped,
            drag_stopped: stopped,
            reset: false,
        }
    }

    #[test]
    fn a_slider_drag_is_one_undo_step_and_undo_restores_the_value() {
        let mut edits = EditSession::from_saved(recipe_with_layer());
        let step = |edits: &mut EditSession, value, phase| {
            let next = LayerEdit::Slider(LocalSlider::Exposure, value)
                .apply(&edits.recipe().clone(), 0);
            apply_slider(edits, next, phase)
        };
        assert!(step(&mut edits, 0.2, drag(true, false)));
        assert!(step(&mut edits, 0.5, drag(false, false)));
        // Releasing without moving ends the gesture without a further change.
        assert!(!step(&mut edits, 0.5, SliderPhase { drag_stopped: true, ..SliderPhase::default() }));
        assert_eq!(edits.recipe().local.layers[0].settings.exposure_ev, 0.5);
        assert!(edits.undo());
        assert_eq!(edits.recipe().local.layers[0].settings.exposure_ev, 0.0);
        assert!(!edits.can_undo());
    }

    #[test]
    fn a_typed_value_is_a_single_discrete_edit() {
        let mut edits = EditSession::from_saved(recipe_with_layer());
        let next = LayerEdit::Slider(LocalSlider::Opacity, 0.4).apply(edits.recipe(), 0);
        let typed = SliderPhase {
            changed: true,
            ..SliderPhase::default()
        };
        assert!(apply_slider(&mut edits, next, typed));
        assert!(edits.undo());
        assert_eq!(edits.recipe().local.layers[0].opacity, 1.0);
    }

    #[test]
    fn geometry_follows_the_orientation_override_and_the_crop() {
        let info = RawFileInfo {
            width: 600,
            height: 400,
            orientation: Orientation::Normal,
            ..RawFileInfo::default()
        };
        let mut recipe = EditRecipe::default();
        let upright = output_geometry(&info, &recipe).expect("geometry");
        assert_eq!(upright.output_dimensions(), (600, 400));
        recipe.geometry.orientation_override = Some(Orientation::Rotate90);
        let turned = output_geometry(&info, &recipe).expect("geometry");
        assert_eq!(turned.output_dimensions(), (400, 600));
    }
}
