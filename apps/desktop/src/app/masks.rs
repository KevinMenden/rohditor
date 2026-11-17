//! Masks tool wiring: panel actions, viewport gestures, shortcuts and the mask overlay texture.
//!
//! The tool's behaviour lives in `rohditor_engine::tools` (`MaskToolSession`, `LayerEdit`);
//! this module connects it to egui input, the edit history and preview scheduling. Every change
//! goes into the recipe as it happens, so leaving the tool has nothing to commit.

use eframe::egui;
use rohditor_core::{MaskAlphaImage, OutputGeometry, PreviewOptions, RawCropPolicy};
use rohditor_edit::{ColorSample, EditRecipe, GeometryAdjustments, MaskComponent, MaskLayer};
use rohditor_engine::coordinator::RenderCoordinator;
use rohditor_engine::document::{EditSession, PreviewTicket};
use rohditor_engine::tools::{
    ColorPick, LayerEdit, MaskEditMode, MaskToolSession, MaskView, ToolKind,
};
use rohditor_image::Orientation;
use rohditor_raw::{RawFileInfo, RawFrame};

use super::tools::tool_panel_model;
use super::{Document, RohditorApp};
use crate::ui::mask_overlay::{MaskOverlayModel, MaskOverlayOutput};
use crate::ui::masks::{self, MasksAction, MasksPanelModel, RangeKind, SliderPhase};
use crate::ui::tool_panel;

/// Peak opacity of the red mask tint, at alpha 1.
const OVERLAY_TINT_OPACITY: f32 = 0.5;

/// Size of the developed frame (the sensor area the pipeline develops, before the user's crop
/// and orientation) that masks are normalized over. The RAW's full sensor size is not it: masked
/// borders are cut off, which changes the aspect and with it every long-edge length.
fn developed_size(info: &RawFileInfo, policy: RawCropPolicy) -> Option<(usize, usize)> {
    rohditor_core::raw_crop_dimensions(info, policy).ok()
}

/// Orientation and crop of the displayed image over the developed frame.
fn output_geometry(
    info: &RawFileInfo,
    policy: RawCropPolicy,
    recipe: &EditRecipe,
) -> Option<OutputGeometry> {
    geometry_for(developed_size(info, policy)?, info.orientation, recipe)
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
    policy: RawCropPolicy,
    mask_overlay: Option<&'a MaskTint>,
) -> Option<MaskOverlayModel<'a>> {
    let recipe = edits.recipe();
    let geometry = output_geometry(&frame?.info, policy, recipe)?;
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
/// over the preview is enough, and it is recomputed on every frame of a handle drag.
const TINT_LONG_EDGE: usize = 512;

/// What a tint shows: the shape of one layer in the displayed frame. Opacity, sliders and
/// everything else in the recipe do not change it, so a slider drag never recomputes it.
struct TintSource {
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
    fn new(
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

    /// Whether the tint of this source is still right, without copying the layer.
    fn matches(
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

/// The tint texture and what it was computed from.
pub(super) struct MaskTint {
    source: TintSource,
    texture: egui::TextureHandle,
}

/// Brings the tint up to date with the recipe and the selected layer. A gradient or brush is
/// computed here, from the recipe, at a small size: a render would show the mask only after the
/// whole preview was done, so the tint would trail the handles while they are dragged. A layer
/// with a luminance or color range depends on the image, so the worker measures it (see
/// [`request_range_overlay`]) and the tint follows the edits a little later.
pub(super) fn refresh_tint(
    context: &egui::Context,
    document: &mut Document,
    session: &MaskToolSession,
    policy: RawCropPolicy,
    coordinator: &RenderCoordinator,
    options: PreviewOptions,
) {
    let Some(layer) = session.overlay_layer() else {
        return;
    };
    let recipe = document.edits.recipe();
    let Some(shown) = recipe.local.layer(layer) else {
        // The selection is stale (the layer was deleted): show nothing rather than its tint.
        document.mask_overlay = None;
        return;
    };
    let ranged = shown
        .components
        .iter()
        .any(|component| component.shape.is_range());
    let revision = ranged.then(|| document.edits.revision());
    if document
        .mask_overlay
        .as_ref()
        .is_some_and(|tint| tint.source.matches(recipe, shown, policy, revision))
    {
        return;
    }
    if ranged {
        request_range_overlay(document, layer, coordinator, options);
        return;
    }
    let source = TintSource::new(recipe, shown, policy, None);
    let Some(info) = document.frame.as_ref().map(|frame| &frame.info) else {
        return;
    };
    let Some(developed) = developed_size(info, policy) else {
        document.mask_overlay = None;
        return;
    };
    let alpha = rohditor_core::render_mask_overlay_preview(
        document.edits.recipe(),
        layer,
        developed,
        info.orientation,
        TINT_LONG_EDGE,
    );
    let Ok(Some(alpha)) = alpha else {
        // No such layer (or an unusable recipe): show nothing rather than a stale tint.
        document.mask_overlay = None;
        return;
    };
    install_tint(context, document, source, &alpha);
}

/// Asks the worker for the overlay of a layer that selects by image content. The old tint stays
/// until the new one arrives, so dragging a range does not make it flicker.
fn request_range_overlay(
    document: &mut Document,
    layer: u32,
    coordinator: &RenderCoordinator,
    options: PreviewOptions,
) {
    let revision = document.edits.revision();
    if document.range_overlay_requested == Some((revision, layer)) || document.open_status.is_some()
    {
        return;
    }
    let Some(frame) = document.frame.clone() else {
        return;
    };
    let ticket = document.ticket();
    let recipe = document.edits.recipe().clone();
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
            tint.source = source;
        }
        None => {
            document.mask_overlay = Some(MaskTint {
                source,
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

/// Forwards one frame of viewport pointer events to the session as edit gestures. Returns
/// whether the recipe changed.
///
/// A drag or a stroke is one gesture: opened by the press, closed by the release. A press that
/// grabs nothing (or a brush press that hits a limit) opens none, so no release follows and the
/// gesture must not stay open. `begin_gesture` and `finish_gesture` are no-ops when one is already open or
/// none is, so a release without a press is harmless.
fn drive_pointer(
    edits: &mut EditSession,
    session: &mut MaskToolSession,
    output: &MaskOverlayOutput,
    view: &MaskView,
) -> bool {
    if let Some(layer) = output.select_layer {
        session.select_layer(edits.recipe(), layer);
    }
    // Read at the press, where a stroke takes it as its mode.
    session.erase_held = output.erase_held;
    let mut changed = false;
    if let Some(position) = output.press {
        edits.begin_gesture();
        let next = session.press(edits.recipe(), view, position);
        changed |= install_during_gesture(edits, next);
        if !session.is_dragging() {
            edits.finish_gesture();
        }
    }
    for position in &output.drag_to {
        let next = session.drag_to(edits.recipe(), view, *position);
        changed |= install_during_gesture(edits, next);
    }
    if output.release {
        let next = session.release(edits.recipe());
        changed |= install_during_gesture(edits, next);
        edits.finish_gesture();
    }
    changed
}

/// Net brush steps asked for by the `[` and `]` presses of one frame.
#[derive(Debug, Default, PartialEq, Eq)]
struct BrushKeySteps {
    /// `]` grows the brush, `[` shrinks it.
    size: i32,
    /// Shift+`]` softens the edge, Shift+`[` hardens it.
    feather: i32,
}

/// Reads the brush keys from the frame's key events, so held keys (which repeat) and several
/// presses in one frame all count. `{` and `}` are what Shift+`[` and Shift+`]` produce on many
/// layouts, so they mean the same as the shifted brackets.
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
        let (direction, curly) = match key {
            egui::Key::CloseBracket => (1, false),
            egui::Key::OpenBracket => (-1, false),
            egui::Key::CloseCurlyBracket => (1, true),
            egui::Key::OpenCurlyBracket => (-1, true),
            _ => continue,
        };
        if modifiers.shift || curly {
            steps.feather += direction;
        } else {
            steps.size += direction;
        }
    }
    steps
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
            brush: session.brush,
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
        let Some(frame) = document.frame.clone() else {
            document.error = Some("Open a developed RAW preview before picking colors".to_owned());
            return;
        };
        let ticket = document.ticket();
        let recipe = document.edits.recipe().clone();
        match self
            .coordinator
            .sample_color_range(ticket, frame, recipe, position, pick, options)
        {
            Ok(()) => self.pending_color_pick = Some(ticket),
            Err(error) => document.error = Some(error),
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
        if document.id != ticket.document_id || document.edits.revision() != ticket.revision {
            return;
        }
        let recipe = document.edits.recipe().clone();
        let changed = install_discrete(
            &mut document.edits,
            session.pick_color(&recipe, sample, pick),
        );
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
            .filter(|document| document.id == ticket.document_id)
        {
            document.error = Some(message);
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
            .filter(|document| document.id == ticket.document_id)
        else {
            return;
        };
        if document.range_overlay_requested == Some((ticket.revision, layer)) {
            document.range_overlay_requested = None;
        }
        if document.edits.revision() != ticket.revision {
            return;
        }
        let recipe = document.edits.recipe();
        let Some(shown) = recipe.local.layer(layer) else {
            return;
        };
        let source = TintSource::new(recipe, shown, policy, Some(ticket.revision));
        install_tint(context, document, source, overlay);
        context.request_repaint();
    }

    /// The overlay could not be measured: show no tint rather than one that no longer matches.
    /// A failure for a recipe the user has since edited says nothing about the current one.
    pub(super) fn fail_range_overlay(&mut self, ticket: PreviewTicket) {
        if let Some(document) = self.document.as_mut().filter(|document| {
            document.id == ticket.document_id && document.edits.revision() == ticket.revision
        }) {
            document.mask_overlay = None;
        }
    }

    fn forward_mask_pointer(&mut self, output: &MaskOverlayOutput, view: &MaskView) -> bool {
        let (Some(document), Some(session)) = (self.document.as_mut(), self.tool.masks_mut())
        else {
            return false;
        };
        drive_pointer(&mut document.edits, session, output, view)
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
            MasksAction::PaintSelected => {
                session.paint_selected(&recipe);
            }
            MasksAction::NewRange { kind, new_layer } => {
                if new_layer {
                    // Clears the selection, so the range becomes a layer of its own. A color
                    // range then waits for its first click on the image.
                    session.create_layer(match kind {
                        RangeKind::Luminance => MaskEditMode::Select,
                        RangeKind::Color => MaskEditMode::PickColor,
                    });
                }
                match kind {
                    RangeKind::Luminance => {
                        return install_discrete(edits, session.add_luminance_range(&recipe));
                    }
                    RangeKind::Color => session.start_color_pick(),
                }
            }
            MasksAction::PickColor => session.start_color_pick(),
            MasksAction::RangeSlider {
                layer,
                component,
                slider,
                value,
                phase,
            } => {
                let next = LayerEdit::RangeSlider {
                    component,
                    slider,
                    value,
                }
                .apply(&recipe, layer);
                return apply_slider(edits, next, phase);
            }
            MasksAction::CancelCreate => {
                session.cancel_create();
            }
            MasksAction::SetBrush(brush) => session.brush = brush,
            MasksAction::SetErase(erase) => {
                if session.is_painting() {
                    session.mode = MaskEditMode::Paint { erase };
                }
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
        if painting && brush_steps != BrushKeySteps::default() {
            if let Some(session) = self.tool.masks_mut() {
                session.brush.step_radius(brush_steps.size);
                session.brush.step_feather(brush_steps.feather);
            }
        } else if painting && pressed(egui::Key::E) {
            if let Some(session) = self.tool.masks_mut() {
                session.toggle_erase();
            }
        } else if pressed(egui::Key::O) {
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
    use rohditor_engine::tools::{LocalSlider, RangeSlider};

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
    fn dragging_a_luminance_handle_is_one_undo_step() {
        let mut recipe = recipe_with_layer();
        recipe.local.layers[0]
            .components
            .push(rohditor_edit::MaskComponent {
                mode: rohditor_edit::MaskMode::Intersect,
                invert: false,
                shape: MaskShape::Luminance(rohditor_edit::LuminanceRange::default()),
            });
        let mut edits = EditSession::from_saved(recipe);
        let step = |edits: &mut EditSession, value, phase| {
            let next = LayerEdit::RangeSlider {
                component: 1,
                slider: RangeSlider::LuminanceLow,
                value,
            }
            .apply(&edits.recipe().clone(), 0);
            apply_slider(edits, next, phase)
        };
        let low = |edits: &EditSession| match &edits.recipe().local.layers[0].components[1].shape {
            MaskShape::Luminance(range) => range.low,
            other => panic!("not a luminance range: {other:?}"),
        };
        assert!(step(&mut edits, 0.2, drag(true, false)));
        assert!(step(&mut edits, 0.4, drag(false, false)));
        assert!(!step(
            &mut edits,
            0.4,
            SliderPhase {
                drag_stopped: true,
                ..SliderPhase::default()
            }
        ));
        assert_eq!(low(&edits), 0.4);
        assert!(edits.undo());
        assert_eq!(low(&edits), 0.0);
        assert!(!edits.can_undo());
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

    fn view() -> MaskView {
        MaskView {
            geometry: OutputGeometry::new(600, 400, Orientation::Normal, None)
                .expect("valid geometry"),
            screen_scale: 1.0,
        }
    }

    #[test]
    fn a_press_that_grabs_nothing_leaves_no_gesture_open() {
        let mut edits = EditSession::from_saved(recipe_with_layer());
        let mut session = MaskToolSession::default();
        // Select mode with nothing selected: nothing to grab, and the drag that follows moves
        // nothing.
        let output = MaskOverlayOutput {
            press: Some((0.5, 0.5)),
            drag_to: vec![(0.6, 0.6)],
            ..MaskOverlayOutput::default()
        };
        assert!(!drive_pointer(&mut edits, &mut session, &output, &view()));
        assert!(!edits.gesture_active());
        assert!(!session.is_dragging());
        // A release without a press is harmless too.
        let release = MaskOverlayOutput {
            release: true,
            ..MaskOverlayOutput::default()
        };
        assert!(!drive_pointer(&mut edits, &mut session, &release, &view()));
        assert!(!edits.can_undo());
    }

    #[test]
    fn drawing_a_gradient_is_one_undo_step() {
        let mut edits = EditSession::from_saved(EditRecipe::default());
        let mut session = MaskToolSession::default();
        session.create_layer(MaskEditMode::CreateLinear);
        let press = MaskOverlayOutput {
            press: Some((0.2, 0.5)),
            drag_to: vec![(0.4, 0.5)],
            ..MaskOverlayOutput::default()
        };
        assert!(drive_pointer(&mut edits, &mut session, &press, &view()));
        assert!(edits.gesture_active());
        let drag = MaskOverlayOutput {
            drag_to: vec![(0.8, 0.5)],
            ..MaskOverlayOutput::default()
        };
        assert!(drive_pointer(&mut edits, &mut session, &drag, &view()));
        let release = MaskOverlayOutput {
            release: true,
            ..MaskOverlayOutput::default()
        };
        drive_pointer(&mut edits, &mut session, &release, &view());
        assert!(!edits.gesture_active());
        assert_eq!(edits.recipe().local.layers.len(), 1);
        assert!(edits.undo());
        assert!(edits.recipe().local.layers.is_empty());
        assert!(!edits.can_undo());
    }

    fn paint_session() -> MaskToolSession {
        let mut session = MaskToolSession::default();
        session.create_layer(MaskEditMode::Paint { erase: false });
        session
    }

    #[test]
    fn a_stroke_is_one_undo_step_through_the_pointer() {
        let mut edits = EditSession::from_saved(EditRecipe::default());
        let mut session = paint_session();
        let press = MaskOverlayOutput {
            press: Some((0.2, 0.5)),
            drag_to: vec![(0.3, 0.5), (0.4, 0.5)],
            ..MaskOverlayOutput::default()
        };
        assert!(drive_pointer(&mut edits, &mut session, &press, &view()));
        assert!(edits.gesture_active());
        let release = MaskOverlayOutput {
            drag_to: vec![(0.6, 0.5)],
            release: true,
            ..MaskOverlayOutput::default()
        };
        assert!(drive_pointer(&mut edits, &mut session, &release, &view()));
        assert!(!edits.gesture_active());
        assert_eq!(edits.recipe().local.layers.len(), 1);
        // The tool stays in painting for the next stroke.
        assert!(session.is_painting());

        assert!(edits.undo());
        assert_eq!(*edits.recipe(), EditRecipe::default());
        assert!(!edits.can_undo());
    }

    #[test]
    fn a_click_that_is_pressed_and_released_in_one_frame_paints_a_dab() {
        let mut edits = EditSession::from_saved(EditRecipe::default());
        let mut session = paint_session();
        let click = MaskOverlayOutput {
            press: Some((0.5, 0.5)),
            release: true,
            ..MaskOverlayOutput::default()
        };
        assert!(drive_pointer(&mut edits, &mut session, &click, &view()));
        assert!(!edits.gesture_active());
        assert!(!session.is_dragging());
        let MaskShape::Brush(brush) = &edits.recipe().local.layers[0].components[0].shape else {
            panic!("a brush layer");
        };
        assert_eq!(brush.strokes[0].points.len(), 1);
    }

    #[test]
    fn holding_alt_at_the_press_erases_from_the_selected_brush() {
        let mut edits = EditSession::from_saved(EditRecipe::default());
        let mut session = paint_session();
        let paint = MaskOverlayOutput {
            press: Some((0.2, 0.5)),
            drag_to: vec![(0.6, 0.5)],
            release: true,
            ..MaskOverlayOutput::default()
        };
        drive_pointer(&mut edits, &mut session, &paint, &view());
        let erase = MaskOverlayOutput {
            press: Some((0.3, 0.5)),
            drag_to: vec![(0.4, 0.5)],
            release: true,
            erase_held: true,
            ..MaskOverlayOutput::default()
        };
        assert!(drive_pointer(&mut edits, &mut session, &erase, &view()));
        let MaskShape::Brush(brush) = &edits.recipe().local.layers[0].components[0].shape else {
            panic!("a brush layer");
        };
        let erases: Vec<bool> = brush.strokes.iter().map(|stroke| stroke.erase).collect();
        assert_eq!(erases, [false, true]);
        // Letting go of Alt is back to painting; the mode was never changed.
        assert_eq!(session.mode, MaskEditMode::Paint { erase: false });
    }

    #[test]
    fn a_brush_press_that_hits_a_limit_leaves_no_gesture_open() {
        let mut recipe = EditRecipe::default();
        recipe.local.layers.push(rohditor_edit::MaskLayer {
            components: vec![rohditor_edit::MaskComponent {
                mode: rohditor_edit::MaskMode::Add,
                invert: false,
                shape: MaskShape::Brush(rohditor_edit::BrushMask {
                    strokes: vec![rohditor_edit::BrushStroke {
                        points: vec![MaskPoint { x: 0.5, y: 0.5 }; rohditor_edit::MAX_BRUSH_POINTS],
                        radius: 0.05,
                        feather: 0.5,
                        flow: 1.0,
                        erase: false,
                    }],
                }),
            }],
            ..recipe_with_layer().local.layers.remove(0)
        });
        let mut edits = EditSession::from_saved(recipe);
        let mut session = paint_session();
        let press = MaskOverlayOutput {
            press: Some((0.5, 0.5)),
            drag_to: vec![(0.6, 0.5)],
            ..MaskOverlayOutput::default()
        };
        assert!(!drive_pointer(&mut edits, &mut session, &press, &view()));
        assert!(!edits.gesture_active());
        assert!(session.limit_reached().is_some());
        assert!(!edits.can_undo());
    }

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
        let measured = TintSource::new(&recipe, layer, policy, Some(4));
        // The same recipe at another revision may look different (exposure, white balance), and a
        // tint of a layer the recipe alone determines is never the tint of a measured one.
        assert!(measured.matches(&recipe, layer, policy, Some(4)));
        assert!(!measured.matches(&recipe, layer, policy, Some(5)));
        assert!(!measured.matches(&recipe, layer, policy, None));
        let plain = TintSource::new(&recipe, layer, policy, None);
        assert!(!plain.matches(&recipe, layer, policy, Some(4)));
    }
}
