//! What a frontend asks of the Masks tool, and how it lands in the edit history.
//!
//! The rest of the tool is pure over `EditRecipe`. This module is the layer that knows about
//! [`EditSession`]: it turns one frame of pointer events ([`MaskPointerEvents`]) or one panel
//! or shortcut request ([`MaskAction`]) into recipe edits with the right undo grouping (one
//! drag, one stroke or one slider drag is one undo step). A frontend only has to translate its
//! toolkit's input into these types.

use crate::document::EditSession;
use rohditor_edit::{EditRecipe, MaskMode};

use super::{
    BrushSettings, LayerEdit, LocalSlider, MaskEditMode, MaskToolSession, MaskView, RangeSlider,
};

/// One frame of viewport pointer input for the Masks tool, in display positions (`0..1` over
/// the image on screen), for the [`MaskView`] the frame was shown with.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct MaskPointerEvents {
    /// The pointer pressed on something the tool grabs.
    pub press: Option<(f64, f64)>,
    /// Positions the pointer moved through during the gesture this frame, oldest first; the
    /// last is where it is now. A stroke follows all of them, so a fast one stays smooth.
    pub drag_to: Vec<(f64, f64)>,
    pub release: bool,
    /// The erase modifier is down. A stroke reads it at its press.
    pub erase_held: bool,
    /// A click on another mask's pin.
    pub select_layer: Option<u32>,
}

/// How a slider event relates to a drag, so a whole drag can be one undo step.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SliderPhase {
    pub changed: bool,
    pub drag_started: bool,
    pub dragged: bool,
    pub drag_stopped: bool,
    /// The value was reset to its default, which is always one edit of its own.
    pub reset: bool,
}

/// The two ways to select by what the image looks like instead of where things are.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RangeKind {
    Luminance,
    Color,
}

impl RangeKind {
    pub const ALL: [Self; 2] = [Self::Luminance, Self::Color];
}

/// Which bracket key was pressed while painting.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Bracket {
    /// `[`
    Open,
    /// `]`
    Close,
}

/// Net brush steps asked for by the bracket presses of one frame.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct BrushKeySteps {
    /// `]` grows the brush, `[` shrinks it.
    pub size: i32,
    /// Shift+`]` softens the edge, Shift+`[` hardens it.
    pub feather: i32,
}

impl BrushKeySteps {
    /// Counts one key press. Held keys repeat and several presses can land in one frame, so a
    /// frontend adds every press of the frame.
    pub fn press(&mut self, bracket: Bracket, shifted: bool) {
        let direction = match bracket {
            Bracket::Open => -1,
            Bracket::Close => 1,
        };
        if shifted {
            self.feather += direction;
        } else {
            self.size += direction;
        }
    }

    pub fn is_empty(&self) -> bool {
        *self == Self::default()
    }
}

/// A request from the tool panel or a shortcut.
#[derive(Debug, Clone)]
pub enum MaskAction {
    SelectLayer(u32),
    SelectComponent(usize),
    /// Start drawing a gradient (or painting a brush) into a new layer.
    NewLayer(MaskEditMode),
    /// Start drawing a gradient (or painting a brush) into the selected layer.
    NewComponent(MaskEditMode, MaskMode),
    /// Paint more strokes into the selected brush component.
    PaintSelected,
    /// Add a luminance or color range, as a new mask or as a component of the selected one.
    /// A color range starts by picking its colors on the image.
    NewRange {
        kind: RangeKind,
        new_layer: bool,
    },
    /// Pick the colors of the selected color range on the image.
    PickColor,
    /// A slider event of a range component (the luminance bar or the color tolerance).
    RangeSlider {
        layer: u32,
        component: usize,
        slider: RangeSlider,
        value: f32,
        phase: SliderPhase,
    },
    CancelCreate,
    /// New brush settings, from the brush sliders.
    SetBrush(BrushSettings),
    /// Paint (`false`) or erase (`true`) with the brush.
    SetErase(bool),
    DeleteLayer(u32),
    DeleteComponent(usize),
    Edit(u32, LayerEdit),
    /// A slider event with the new value.
    Slider(u32, LocalSlider, f32, SliderPhase),
    SetOverlay(bool),
    /// Shortcut: show or hide the tint.
    ToggleOverlay,
    /// Shortcut: switch between painting and erasing.
    ToggleErase,
    /// Shortcut: delete the selected component, or the layer when none is selected.
    DeleteSelected,
    /// Shortcut: the bracket keys of one frame, resizing or softening the brush.
    BrushKeys(BrushKeySteps),
}

impl MaskToolSession {
    /// Forwards one frame of viewport pointer events to the tool as edit gestures. Returns
    /// whether the recipe changed.
    ///
    /// A drag or a stroke is one gesture: opened by the press, closed by the release. A press
    /// that grabs nothing (or a brush press that hits a limit) opens none, so no release
    /// follows and the gesture must not stay open. `begin_gesture` and `finish_gesture` are
    /// no-ops when one is already open or none is, so a release without a press is harmless.
    pub fn drive_pointer(
        &mut self,
        edits: &mut EditSession,
        events: &MaskPointerEvents,
        view: &MaskView,
    ) -> bool {
        if let Some(layer) = events.select_layer {
            self.select_layer(edits.recipe(), layer);
        }
        // Read at the press, where a stroke takes it as its mode.
        self.erase_held = events.erase_held;
        let mut changed = false;
        if let Some(position) = events.press {
            edits.begin_gesture();
            let next = self.press(edits.recipe(), view, position);
            changed |= install_during_gesture(edits, next);
            if !self.is_dragging() {
                edits.finish_gesture();
            }
        }
        for position in &events.drag_to {
            let next = self.drag_to(edits.recipe(), view, *position);
            changed |= install_during_gesture(edits, next);
        }
        if events.release {
            let next = self.release(edits.recipe());
            changed |= install_during_gesture(edits, next);
            edits.finish_gesture();
        }
        changed
    }

    /// Carries out one panel or shortcut request. Returns whether the recipe changed, so the
    /// caller knows to schedule a preview; selection and overlay changes return false.
    pub fn apply_action(&mut self, edits: &mut EditSession, action: MaskAction) -> bool {
        let recipe = edits.recipe().clone();
        match action {
            MaskAction::SelectLayer(layer) => self.select_layer(&recipe, layer),
            MaskAction::SelectComponent(component) => self.select_component(&recipe, component),
            MaskAction::NewLayer(shape) => self.create_layer(shape),
            MaskAction::NewComponent(shape, mode) => self.create_component(shape, mode),
            MaskAction::PaintSelected => {
                self.paint_selected(&recipe);
            }
            MaskAction::NewRange { kind, new_layer } => {
                if new_layer {
                    // Clears the selection, so the range becomes a layer of its own. A color
                    // range then waits for its first click on the image.
                    self.create_layer(match kind {
                        RangeKind::Luminance => MaskEditMode::Select,
                        RangeKind::Color => MaskEditMode::PickColor,
                    });
                }
                match kind {
                    RangeKind::Luminance => {
                        return install_discrete(edits, self.add_luminance_range(&recipe));
                    }
                    RangeKind::Color => self.start_color_pick(),
                }
            }
            MaskAction::PickColor => self.start_color_pick(),
            MaskAction::RangeSlider {
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
            MaskAction::CancelCreate => {
                self.cancel_create();
            }
            MaskAction::SetBrush(brush) => self.brush = brush,
            MaskAction::SetErase(erase) => {
                if self.is_painting() {
                    self.mode = MaskEditMode::Paint { erase };
                }
            }
            MaskAction::SetOverlay(show) => self.show_overlay = show,
            MaskAction::DeleteLayer(layer) => {
                return install_discrete(edits, self.delete_layer(&recipe, layer));
            }
            MaskAction::DeleteComponent(component) => {
                let next = self
                    .selected_layer
                    .and_then(|layer| self.delete_component(&recipe, layer, component));
                return install_discrete(edits, next);
            }
            MaskAction::Edit(layer, edit) => {
                return install_discrete(edits, edit.apply(&recipe, layer));
            }
            MaskAction::Slider(layer, slider, value, phase) => {
                let next = LayerEdit::Slider(slider, value).apply(&recipe, layer);
                return apply_slider(edits, next, phase);
            }
            MaskAction::ToggleOverlay => self.show_overlay = !self.show_overlay,
            MaskAction::ToggleErase => self.toggle_erase(),
            MaskAction::DeleteSelected => {
                return install_discrete(edits, self.delete_selected(&recipe));
            }
            MaskAction::BrushKeys(steps) => {
                self.brush.step_radius(steps.size);
                self.brush.step_feather(steps.feather);
            }
        }
        false
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

#[cfg(test)]
mod tests;
