//! The read-only Info section: capture settings and file facts of the open photo.

use eframe::egui;
use rohditor_raw::InfoRow;

use super::theme::colors;

/// Shown for any value the file does not provide.
const MISSING: &str = "–";

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct InfoPanelModel {
    pub capture: Vec<InfoRow>,
    pub file: Vec<InfoRow>,
}

pub(crate) fn show(ui: &mut egui::Ui, model: &InfoPanelModel) {
    rows(ui, "info_capture", &model.capture);
    ui.add_space(6.0);
    rows(ui, "info_file", &model.file);
}

fn rows(ui: &mut egui::Ui, id: &str, rows: &[InfoRow]) {
    egui::Grid::new(id)
        .num_columns(2)
        .spacing(egui::vec2(12.0, 3.0))
        .show(ui, |ui| {
            for row in rows {
                ui.label(
                    egui::RichText::new(row.label)
                        .small()
                        .color(colors::TEXT_MUTED),
                );
                let value = row.value.as_deref().unwrap_or(MISSING);
                ui.add(egui::Label::new(egui::RichText::new(value).small()).truncate())
                    .on_hover_text(value);
                ui.end_row();
            }
        });
}
