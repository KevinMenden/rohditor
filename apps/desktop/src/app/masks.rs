//! Masks tool wiring: panel actions, viewport gestures and the mask overlay texture.
//!
//! The tool's behaviour lives in `rohditor_engine::tools` (`MaskToolSession`, `LayerEdit`);
//! this module connects it to egui input, the edit history and preview scheduling. Every change
//! goes into the recipe as it happens, so leaving the tool has nothing to commit.

use eframe::egui;
use rohditor_core::OutputGeometry;
use rohditor_edit::EditRecipe;
use rohditor_engine::document::EditSession;
use rohditor_engine::tools::{LayerEdit, MaskToolSession, MaskView, ToolKind};
use rohditor_image::Orientation;
use rohditor_raw::{RawFileInfo, RawFrame};

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
    geometry_for((info.width, info.height), info.orientation, recipe)
}

fn geometry_for(
    (width, height): (usize, usize),
    source_orientation: Orientation,
    recipe: &EditRecipe,
) -> Option<OutputGeometry> {
    let orientation = recipe
        .geometry
        .orientation_override
        .unwrap_or(source_orientation);
    OutputGeometry::new(width, height, orientation, recipe.geometry.crop).ok()
}

/// What the viewport needs to draw and drive the Masks tool, or `None` while the photo has
/// no frame to map pointer positions onto.
pub(super) fn overlay_model<'a>(
    session: &'a MaskToolSession,
    edits: &'a EditSession,
    frame: Option<&RawFrame>,
    mask_overlay: Option<&'a MaskTint>,
) -> Option<MaskOverlayModel<'a>> {
    let recipe = edits.recipe();
    let geometry = output_geometry(&frame?.info, recipe)?;
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

/// Long edge, in pixels, of the tint image. It is a soft red wash, so a small image stretched
/// over the preview is enough, and it is cheap to recompute on every frame of a drag.
const TINT_LONG_EDGE: usize = 512;

/// The tint texture and what it was computed from.
pub(super) struct MaskTint {
    /// The recipe revision and layer the texture shows.
    key: (u64, u32),
    texture: egui::TextureHandle,
}

/// Brings the tint up to date with the recipe and the selected layer. It is computed here, from
/// the recipe, at a small size: a render would show the mask only after the whole preview was
/// done, so the tint would trail the handles while they are dragged.
pub(super) fn refresh_tint(
    context: &egui::Context,
    document: &mut Document,
    session: &MaskToolSession,
) {
    let Some(layer) = session.overlay_layer() else {
        return;
    };
    let key = (document.edits.revision(), layer);
    if document
        .mask_overlay
        .as_ref()
        .is_some_and(|tint| tint.key == key)
    {
        return;
    }
    let Some(info) = document.frame.as_ref().map(|frame| &frame.info) else {
        return;
    };
    let alpha = rohditor_core::render_mask_overlay_preview(
        document.edits.recipe(),
        layer,
        (info.width, info.height),
        info.orientation,
        TINT_LONG_EDGE,
    );
    let Ok(Some(alpha)) = alpha else {
        // No such layer (or an unusable recipe): show nothing rather than a stale tint.
        document.mask_overlay = None;
        return;
    };
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
        Some(tint) => {
            tint.texture.set(image, egui::TextureOptions::LINEAR);
            tint.key = key;
        }
        None => {
            document.mask_overlay = Some(MaskTint {
                key,
                texture: context.load_texture(
                    format!("document-{}-mask-tint", document.id),
                    image,
                    egui::TextureOptions::LINEAR,
                ),
            });
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

    /// A changed recipe needs a new preview. Selection and overlay changes do not: the tint is
    /// drawn from the recipe, not from a render.
    fn finish_mask_interaction(&mut self, context: &egui::Context, changed: bool) {
        if !changed {
            return;
        }
        let Some(document) = self.document.as_mut() else {
            return;
        };
        document.notice = None;
        let id = document.id;
        self.queue_preview(context, id);
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
        let mut changed = false;
        let mut used = true;
        if pressed(egui::Key::O) {
            if let Some(session) = self.tool.masks_mut() {
                session.show_overlay = !session.show_overlay;
            }
        } else if pressed(egui::Key::Delete) || pressed(egui::Key::Backspace) {
            if let (Some(document), Some(session)) = (self.document.as_mut(), self.tool.masks_mut())
            {
                let recipe = document.edits.recipe().clone();
                changed = install_discrete(&mut document.edits, session.delete_selected(&recipe));
            }
        } else if pressed(egui::Key::Escape) {
            used = self
                .tool
                .masks_mut()
                .is_some_and(MaskToolSession::cancel_create);
        } else {
            used = false;
        }
        if used {
            self.finish_mask_interaction(context, changed);
        }
        used
    }

    /// Enters the Masks tool, selecting the first mask so there is something to show.
    pub(super) fn enter_masks_tool(&mut self) {
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
        self.pending_white_balance_pick = None;
        // Entering Masks drops any active picker.
        self.tool.enter_masks(session);
    }

    /// Leaves the Masks tool for another tool. Nothing to commit.
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
    use super::*;
    use rohditor_edit::{LinearGradient, MaskPoint, MaskShape};
    use rohditor_engine::tools::LocalSlider;

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
            let next =
                LayerEdit::Slider(LocalSlider::Exposure, value).apply(&edits.recipe().clone(), 0);
            apply_slider(edits, next, phase)
        };
        assert!(step(&mut edits, 0.2, drag(true, false)));
        assert!(step(&mut edits, 0.5, drag(false, false)));
        // Releasing without moving ends the gesture without a further change.
        assert!(!step(
            &mut edits,
            0.5,
            SliderPhase {
                drag_stopped: true,
                ..SliderPhase::default()
            }
        ));
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
        let mut recipe = EditRecipe::default();
        let upright = geometry_for((600, 400), Orientation::Normal, &recipe).expect("geometry");
        assert_eq!(upright.output_dimensions(), (600, 400));
        recipe.geometry.orientation_override = Some(Orientation::Rotate90);
        let turned = geometry_for((600, 400), Orientation::Normal, &recipe).expect("geometry");
        assert_eq!(turned.output_dimensions(), (400, 600));
    }
}
