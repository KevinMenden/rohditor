//! The right-hand panel that hosts the active tool's body.
//!
//! It owns the panel chrome (width, scrolling, heading) and the histogram,
//! which stays on top for every tool.

use eframe::egui;
use rohditor_core::Histogram;

use super::adjustment_panel;
use super::theme::{self, colors, metrics};

pub(crate) struct ToolPanelModel<'a> {
    pub title: &'a str,
    pub subtitle: &'a str,
    pub has_document: bool,
    pub histogram: Option<&'a Histogram>,
}

/// Shows the panel and runs `body` for the tool's controls.
pub(crate) fn show<R>(
    root: &mut egui::Ui,
    model: &ToolPanelModel<'_>,
    body: impl FnOnce(&mut egui::Ui) -> R,
) -> R {
    egui::Panel::right("tool_panel")
        .default_size(metrics::ADJUSTMENT_PANEL_WIDTH)
        .min_size(280.0)
        .max_size(350.0)
        .resizable(true)
        .frame(theme::side_panel_frame())
        .show(root, |ui| {
            egui::ScrollArea::vertical()
                .auto_shrink([false, false])
                .wheel_scroll_multiplier(egui::vec2(1.0, metrics::TOOL_PANEL_SCROLL_MULTIPLIER))
                .show(ui, |ui| {
                    ui.heading(model.title);
                    ui.label(
                        egui::RichText::new(model.subtitle)
                            .size(10.0)
                            .color(colors::TEXT_MUTED),
                    );
                    ui.add_space(7.0);
                    if model.has_document {
                        adjustment_panel::histogram_panel(ui, model.histogram);
                    } else {
                        super::widgets::section_header(ui, "Histogram");
                        adjustment_panel::histogram_canvas(ui, "WAITING FOR A PHOTO");
                    }
                    body(ui)
                })
                .inner
        })
        .inner
}
