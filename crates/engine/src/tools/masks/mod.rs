//! The Masks tool: selection, handle dragging, gradient creation and brush painting.
//!
//! A frontend only draws and forwards pointer input. It builds a [`MaskView`] from how the
//! image is shown, and for one pointer gesture does:
//!
//! ```text
//! press:   edits.begin_gesture();        apply(session.press(recipe, view, pos))
//! move:                                  apply(session.drag_to(recipe, view, pos))
//! release:                               apply(session.release(recipe));  edits.finish_gesture()
//! ```
//!
//! where `apply` hands a returned recipe to `EditSession::set_during_gesture`. Every change is
//! already in the recipe, so leaving the tool needs no commit, and one handle drag or one
//! stroke is one undo step. All methods are pure over `EditRecipe`: the caller owns undo and
//! revisions.
//!
//! Pointer positions are display positions (`0..1` over the image on screen). The tool turns
//! them into the developed frame the recipe stores via `OutputGeometry`; see `view`.
//!
//! Every recipe the tool returns passes `EditRecipe::validate`. When a limit of the recipe
//! model stops an edit, the tool does not exceed it and reports it through
//! [`MaskToolSession::limit_reached`].

mod brush;
mod edit;
mod gesture;
mod handles;
mod view;

#[cfg(test)]
mod tests;

pub use brush::BrushSettings;
pub use handles::MaskHandle;
pub use view::MaskView;

use rohditor_edit::{MAX_BRUSH_POINTS, MAX_BRUSH_STROKES, MAX_MASK_COMPONENTS, MAX_MASK_LAYERS};
use thiserror::Error;

use gesture::MaskDrag;

/// What a pointer press does.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum MaskEditMode {
    /// Grab a handle of the selected component.
    #[default]
    Select,
    /// Press-drag-release draws a linear gradient from its start to its end, then returns to
    /// `Select`.
    CreateLinear,
    /// Press-drag-release draws a radial gradient with the press as its centre and the
    /// release as a point on its outline, then returns to `Select`.
    CreateRadial,
    /// Paint a brush stroke, or erase from the selected brush component.
    Paint { erase: bool },
}

/// A limit of the recipe model that stopped an edit.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum MaskLimit {
    #[error("A photo can have at most {MAX_MASK_LAYERS} masks")]
    Layers,
    #[error("A mask can have at most {MAX_MASK_COMPONENTS} components")]
    Components,
    #[error("A brush can have at most {MAX_BRUSH_STROKES} strokes")]
    Strokes,
    #[error("A photo can have at most {MAX_BRUSH_POINTS} brush points")]
    BrushPoints,
}

/// A point handle to draw, at a display position (`0..1` over the image on screen).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct HandleMarker {
    pub handle: MaskHandle,
    pub position: (f64, f64),
}

/// Masks tool state that is not part of the recipe: what is selected, what a press does, and
/// the gesture in progress.
#[derive(Debug, Clone)]
pub struct MaskToolSession {
    /// Layer id. A stale id (the layer was deleted or undone away) selects nothing.
    pub selected_layer: Option<u32>,
    /// Index into the selected layer's components; its handles are the ones shown.
    pub selected_component: Option<usize>,
    pub mode: MaskEditMode,
    /// Authoring state, copied into each new stroke.
    pub brush: BrushSettings,
    /// Whether the selected layer's alpha is drawn over the preview.
    pub show_overlay: bool,
    drag: Option<MaskDrag>,
    limit: Option<MaskLimit>,
}

impl Default for MaskToolSession {
    fn default() -> Self {
        Self {
            selected_layer: None,
            selected_component: None,
            mode: MaskEditMode::Select,
            brush: BrushSettings::default(),
            show_overlay: true,
            drag: None,
            limit: None,
        }
    }
}

impl MaskToolSession {
    /// The layer id for `RenderOptions::mask_overlay`: the selected layer while the overlay
    /// is on. The render ignores an id that no longer exists.
    pub const fn overlay_layer(&self) -> Option<u32> {
        if self.show_overlay {
            self.selected_layer
        } else {
            None
        }
    }

    /// Whether a pointer gesture is in progress (between a press that grabbed or started
    /// something and its release).
    pub const fn is_dragging(&self) -> bool {
        self.drag.is_some()
    }

    /// The limit that stopped the last press or move, until the next press.
    pub const fn limit_reached(&self) -> Option<MaskLimit> {
        self.limit
    }
}
