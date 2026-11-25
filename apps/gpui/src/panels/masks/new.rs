//! Starting new masks and adding shapes to the selected one.

use gpui::{Context, IntoElement, prelude::*};
use rohditor_edit::MaskMode;
use rohditor_engine::tools::{MaskAction, MaskEditMode, RangeKind};

use super::MasksPanel;
use crate::actions::{AddToMask, NewMask};
use crate::theme::{SMALL_FONT_SIZE, theme};
use crate::ui::dropdown::dropdown;
use crate::ui::menu::MenuEntry;

/// What a mask can be made of.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Shape {
    Linear,
    Radial,
    Brush,
    Range(RangeKind),
}

impl Shape {
    pub const ALL: [Self; 5] = [
        Self::Linear,
        Self::Radial,
        Self::Brush,
        Self::Range(RangeKind::Luminance),
        Self::Range(RangeKind::Color),
    ];

    pub const fn name(self) -> &'static str {
        match self {
            Self::Linear => "linear gradient",
            Self::Radial => "radial gradient",
            Self::Brush => "brush",
            Self::Range(RangeKind::Luminance) => "luminance range",
            Self::Range(RangeKind::Color) => "color range",
        }
    }

    /// How it can join a mask. The mode can be changed afterwards.
    pub const fn modes(self) -> &'static [MaskMode] {
        match self {
            Self::Linear | Self::Radial | Self::Brush => &[MaskMode::Add, MaskMode::Subtract],
            // Refines what is selected already.
            Self::Range(_) => &[MaskMode::Intersect],
        }
    }

    pub const fn new_mask(self) -> MaskAction {
        match self {
            Self::Range(kind) => MaskAction::NewRange {
                kind,
                new_layer: true,
            },
            Self::Linear => MaskAction::NewLayer(MaskEditMode::CreateLinear),
            Self::Radial => MaskAction::NewLayer(MaskEditMode::CreateRadial),
            Self::Brush => MaskAction::NewLayer(MaskEditMode::Paint { erase: false }),
        }
    }

    /// Add it to the selected mask, by one of its [`Self::modes`].
    pub const fn add(self, mode: MaskMode) -> MaskAction {
        match self {
            Self::Range(kind) => MaskAction::NewRange {
                kind,
                new_layer: false,
            },
            Self::Linear => MaskAction::NewComponent(MaskEditMode::CreateLinear, mode),
            Self::Radial => MaskAction::NewComponent(MaskEditMode::CreateRadial, mode),
            Self::Brush => MaskAction::NewComponent(MaskEditMode::Paint { erase: false }, mode),
        }
    }
}

impl MasksPanel {
    pub(super) fn new_mask(&self, _: &mut Context<Self>) -> impl IntoElement + use<> {
        let ready = self.model.ready;
        dropdown(
            "new-mask",
            "New mask",
            "New",
            self.overlays.clone(),
            move |_, _| {
                let shapes = Shape::ALL.into_iter().filter(|_| ready);
                shapes
                    .map(|shape| MenuEntry::command(NewMask(shape)))
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
            "Add, subtract or intersect",
            self.overlays.clone(),
            |_, _| {
                [MaskMode::Add, MaskMode::Subtract, MaskMode::Intersect]
                    .into_iter()
                    .flat_map(|mode| {
                        let joining = Shape::ALL
                            .into_iter()
                            .filter(move |shape| shape.modes().contains(&mode));
                        joining.map(move |shape| MenuEntry::command(AddToMask { shape, mode }))
                    })
                    .collect()
            },
        )
        .text_size(SMALL_FONT_SIZE)
        .text_color(theme().muted)
    }
}
