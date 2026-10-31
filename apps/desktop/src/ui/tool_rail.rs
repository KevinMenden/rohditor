//! The narrow, always-visible tool rail on the far right edge of Develop.
//!
//! Presentation only: it reports which tool button was clicked and leaves the
//! tool state machine to `rohditor_engine::tools`.

use eframe::egui;
use rohditor_engine::tools::ToolKind;

use super::icons::Icon;
use super::theme::{colors, metrics};
use super::widgets;

#[derive(Debug, Clone, Copy)]
pub(crate) struct ToolRailModel {
    pub active: ToolKind,
    /// Tools need an open photo.
    pub enabled: bool,
}

#[derive(Debug, Default)]
pub(crate) struct ToolRailOutput {
    pub selected: Option<ToolKind>,
}

/// The rail's tools in display order, with icon and tooltip.
const TOOLS: [(ToolKind, Icon, &str); 2] = [
    (ToolKind::Edit, Icon::Sliders, "Edit (E)"),
    (ToolKind::Crop, Icon::Crop, "Crop (C)"),
];

pub(crate) fn show(context: &egui::Context, model: ToolRailModel) -> ToolRailOutput {
    let mut output = ToolRailOutput::default();
    egui::SidePanel::right("tool_rail")
        .exact_width(metrics::TOOL_RAIL_WIDTH)
        .resizable(false)
        .frame(
            egui::Frame::new()
                .fill(colors::PANEL)
                .inner_margin(egui::Margin::symmetric(8, 10))
                .stroke(egui::Stroke::new(1.0_f32, colors::BORDER)),
        )
        .show(context, |ui| {
            ui.vertical_centered(|ui| {
                ui.spacing_mut().item_spacing.y = 6.0;
                for (kind, icon, tooltip) in TOOLS {
                    if widgets::icon_button(ui, icon, tooltip, model.active == kind, model.enabled)
                        .clicked()
                    {
                        output.selected = Some(kind);
                    }
                }
            });
        });
    output
}
