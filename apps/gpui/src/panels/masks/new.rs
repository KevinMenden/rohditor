//! Starting new masks and adding shapes to the selected one.

use gpui::{Context, IntoElement, prelude::*};
use rohditor_edit::MaskMode;
use rohditor_engine::tools::MaskEditMode;

use super::MasksPanel;
use crate::actions::{AddToMask, NewMask};
use crate::theme::{SMALL_FONT_SIZE, theme};
use crate::ui::dropdown::dropdown;
use crate::ui::menu::MenuEntry;

/// What can be drawn, and how it is called.
pub const SHAPES: [(MaskEditMode, &str); 2] = [
    (MaskEditMode::CreateLinear, "linear gradient"),
    (MaskEditMode::CreateRadial, "radial gradient"),
];

/// How a drawn shape can join a mask. `Intersect` is chosen afterwards.
pub const COMPONENT_MODES: [MaskMode; 2] = [MaskMode::Add, MaskMode::Subtract];

impl MasksPanel {
    pub(super) fn new_mask(&self, _: &mut Context<Self>) -> impl IntoElement + use<> {
        let ready = self.model.ready;
        dropdown(
            "new-mask",
            "New mask",
            "New",
            self.overlays.clone(),
            move |_, _| {
                let shapes = SHAPES.iter().filter(|_| ready);
                shapes
                    .map(|(shape, _)| MenuEntry::command(NewMask(*shape)))
                    .collect()
            },
        )
        .text_size(SMALL_FONT_SIZE)
        .text_color(if ready { theme().muted } else { theme().faint })
    }

    /// Adds to the selected mask.
    pub(super) fn new_component(&self) -> impl IntoElement + use<> {
        dropdown(
            "new-component",
            "Change the shape of the mask",
            "Add or subtract",
            self.overlays.clone(),
            |_, _| {
                let modes = COMPONENT_MODES.into_iter();
                modes
                    .flat_map(|mode| {
                        SHAPES.map(|(shape, _)| MenuEntry::command(AddToMask { shape, mode }))
                    })
                    .collect()
            },
        )
        .text_size(SMALL_FONT_SIZE)
        .text_color(theme().muted)
    }
}
