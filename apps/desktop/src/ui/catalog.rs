//! Library grid: the folder browsing view.
//!
//! Presentation-only, like every `ui` module. The app builds a
//! [`LibraryModel`] from catalog state and owned textures, and interprets the
//! returned actions. The grid reports which entries were visible so the app
//! can decode textures lazily.

use eframe::egui;
use std::ops::Range;

use super::theme::{self, colors, metrics};
use super::widgets;

/// Widest allowed grid cell before columns stop stretching.
const MAX_CELL_WIDTH: f32 = 240.0;
/// Narrowest cell the grid will use; columns are derived from this.
const MIN_CELL_WIDTH: f32 = 150.0;
/// Caption band under each thumbnail.
const CAPTION_HEIGHT: f32 = 26.0;
/// Inner padding between a cell's card and its thumbnail.
const CELL_PADDING: f32 = 6.0;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum LibraryEntryState {
    /// Thumbnail generation has not finished yet.
    Pending,
    /// A decoded thumbnail is available (the texture may still be loading).
    Ready,
    /// The RAW has no embedded preview.
    Placeholder,
    /// Thumbnail generation failed.
    Failed,
}

#[derive(Clone)]
pub(crate) struct LibraryEntryModel {
    pub name: String,
    pub state: LibraryEntryState,
    pub texture: Option<egui::TextureHandle>,
}

#[derive(Default)]
pub(crate) struct LibraryModel {
    pub folder_name: Option<String>,
    pub entries: Vec<LibraryEntryModel>,
    pub selection: Option<usize>,
    /// Scroll the selected entry into view once, after keyboard navigation.
    pub scroll_to_selection: bool,
    pub failure: Option<String>,
    pub loading_thumbnails: bool,
}

#[derive(Debug, Default)]
pub(crate) struct LibraryOutput {
    pub open_folder: bool,
    pub selected_entry: Option<usize>,
    pub open_entry: Option<usize>,
    /// Index range whose cells intersected the visible viewport.
    pub visible_range: Range<usize>,
    /// Column count of the laid-out grid, for keyboard navigation.
    pub columns: usize,
}

pub(crate) fn show(context: &egui::Context, model: &LibraryModel) -> LibraryOutput {
    let mut output = LibraryOutput::default();
    egui::CentralPanel::default()
        .frame(theme::viewport_frame())
        .show(context, |ui| {
            show_header(ui, model, &mut output);
            ui.add_space(6.0);
            if let Some(failure) = &model.failure {
                ui.colored_label(colors::ERROR, format!("Catalog: {failure}"));
                return;
            }
            if model.entries.is_empty() {
                show_empty_state(ui, model, &mut output);
                return;
            }
            show_grid(ui, model, &mut output);
        });
    output
}

fn show_header(ui: &mut egui::Ui, model: &LibraryModel, output: &mut LibraryOutput) {
    ui.horizontal(|ui| {
        let title = model
            .folder_name
            .clone()
            .unwrap_or_else(|| "Library".to_owned());
        ui.label(
            egui::RichText::new(title)
                .strong()
                .size(15.0)
                .color(colors::TEXT),
        );
        if model.folder_name.is_some() {
            ui.label(
                egui::RichText::new(format!("{} photos", model.entries.len()))
                    .small()
                    .color(colors::TEXT_MUTED),
            );
            if model.loading_thumbnails {
                ui.spinner();
                ui.label(
                    egui::RichText::new("Loading thumbnails…")
                        .small()
                        .color(colors::TEXT_MUTED),
                );
            }
        }
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            output.open_folder = widgets::toolbar_button(ui, "Open Folder…", false, true)
                .on_hover_text("Choose a folder of Sony ARW photos to browse")
                .clicked();
        });
    });
}

fn show_empty_state(ui: &mut egui::Ui, model: &LibraryModel, output: &mut LibraryOutput) {
    ui.centered_and_justified(|ui| {
        ui.vertical(|ui| {
            ui.label(
                egui::RichText::new("Library")
                    .strong()
                    .size(22.0)
                    .color(colors::TEXT),
            );
            let message = if model.folder_name.is_some() {
                "This folder contains no supported photos (Sony ARW)."
            } else {
                "Open a folder with Sony ARW photos to browse them here."
            };
            ui.label(
                egui::RichText::new(message)
                    .small()
                    .color(colors::TEXT_MUTED),
            );
            ui.add_space(8.0);
            if widgets::primary_button(ui, "Open Folder…", true)
                .on_hover_text("Choose a folder of Sony ARW photos to browse")
                .clicked()
            {
                output.open_folder = true;
            }
        });
    });
}

fn show_grid(ui: &mut egui::Ui, model: &LibraryModel, output: &mut LibraryOutput) {
    let columns = column_count(ui.available_width());
    let cell = cell_size(ui.available_width());
    output.columns = columns;

    let mut next_index = 0_usize;
    let mut visible = Range { start: 0, end: 0 };
    egui::ScrollArea::vertical()
        .auto_shrink([false, false])
        .show_viewport(ui, |ui, _viewport| {
            while next_index < model.entries.len() {
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 8.0;
                    for _ in 0..columns {
                        if next_index >= model.entries.len() {
                            break;
                        }
                        let index = next_index;
                        next_index += 1;
                        let entry = &model.entries[index];
                        let (rect, response) = ui.allocate_exact_size(cell, egui::Sense::click());
                        let selected = model.selection == Some(index);
                        paint_cell(ui, rect, entry, selected);
                        let response = if response.hovered() {
                            response.on_hover_text(&entry.name)
                        } else {
                            response
                        };
                        if response.clicked() {
                            output.selected_entry = Some(index);
                        }
                        if response.double_clicked() {
                            output.open_entry = Some(index);
                        }
                        if selected && model.scroll_to_selection {
                            response.scroll_to_me(Some(egui::Align::Center));
                        }
                        if rect.intersects(ui.clip_rect()) {
                            if visible.is_empty() || index < visible.start {
                                visible.start = index;
                            }
                            visible.end = index + 1;
                        }
                    }
                });
                ui.add_space(8.0);
            }
        });
    output.visible_range = visible;
}

fn paint_cell(ui: &mut egui::Ui, rect: egui::Rect, entry: &LibraryEntryModel, selected: bool) {
    let hovered = ui.rect_contains_pointer(rect);
    let fill = if selected {
        colors::ACCENT_MUTED
    } else if hovered {
        colors::HOVER
    } else {
        colors::PANEL_RAISED
    };
    let stroke_color = if selected {
        colors::ACCENT
    } else {
        colors::BORDER
    };
    ui.painter().rect(
        rect,
        metrics::RADIUS,
        fill,
        egui::Stroke::new(1.0_f32, stroke_color),
        egui::StrokeKind::Inside,
    );

    let image_rect = egui::Rect::from_min_max(
        rect.left_top() + egui::vec2(CELL_PADDING, CELL_PADDING),
        rect.right_bottom() - egui::vec2(CELL_PADDING, CAPTION_HEIGHT),
    );
    match (&entry.state, &entry.texture) {
        (LibraryEntryState::Ready, Some(texture)) => {
            let size = texture.size_vec2();
            let scale = (image_rect.width() / size.x).min(image_rect.height() / size.y);
            let target_size = size * scale;
            let target_rect = egui::Rect::from_center_size(image_rect.center(), target_size);
            egui::Image::new(texture)
                .fit_to_exact_size(target_size)
                .texture_options(egui::TextureOptions::LINEAR)
                .paint_at(ui, target_rect);
        }
        (LibraryEntryState::Ready, None) => {
            paint_centered_hint(ui, image_rect, "Decoding…", colors::TEXT_DISABLED);
        }
        (LibraryEntryState::Placeholder, _) => {
            paint_centered_hint(ui, image_rect, "No preview", colors::TEXT_DISABLED);
        }
        (LibraryEntryState::Failed, _) => {
            paint_centered_hint(ui, image_rect, "Unreadable", colors::ERROR);
        }
        (LibraryEntryState::Pending, _) => {
            paint_centered_hint(ui, image_rect, "Loading…", colors::TEXT_DISABLED);
        }
    }

    let caption_rect = egui::Rect::from_min_max(
        rect.left_bottom() + egui::vec2(CELL_PADDING, -(CAPTION_HEIGHT - CELL_PADDING)),
        rect.right_bottom() - egui::vec2(CELL_PADDING, CELL_PADDING),
    );
    let _ = ui.scope_builder(egui::UiBuilder::new().max_rect(caption_rect), |ui| {
        ui.add(
            egui::Label::new(egui::RichText::new(&entry.name).small().color(if selected {
                colors::TEXT
            } else {
                colors::TEXT_MUTED
            }))
            .truncate(),
        );
    });
}

fn paint_centered_hint(ui: &egui::Ui, rect: egui::Rect, text: &str, color: egui::Color32) {
    ui.painter()
        .rect_filled(rect, metrics::RADIUS_SMALL, colors::FIELD);
    ui.painter().text(
        rect.center(),
        egui::Align2::CENTER_CENTER,
        text,
        egui::FontId::proportional(11.0),
        color,
    );
}

/// Column count derived from the available width and the minimum cell size.
pub(crate) fn column_count(available_width: f32) -> usize {
    (available_width / MIN_CELL_WIDTH).floor().max(1.0) as usize
}

/// Cell size for a given available width: columns stretch but never above
/// [`MAX_CELL_WIDTH`], and the caption band sits below the thumbnail.
pub(crate) fn cell_size(available_width: f32) -> egui::Vec2 {
    let columns = column_count(available_width);
    let width = (available_width / columns as f32).min(MAX_CELL_WIDTH);
    egui::vec2(width, width + CAPTION_HEIGHT)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn columns_adapt_to_width_and_never_drop_below_one() {
        assert_eq!(column_count(0.0), 1);
        assert_eq!(column_count(-30.0), 1);
        assert_eq!(column_count(150.0), 1);
        assert_eq!(column_count(299.0), 1);
        assert_eq!(column_count(300.0), 2);
        assert_eq!(column_count(1_500.0), 10);
    }

    #[test]
    fn cells_stretch_to_columns_but_are_capped() {
        let narrow = cell_size(160.0);
        assert_eq!(narrow, egui::vec2(160.0, 160.0 + CAPTION_HEIGHT));
        let stretched = cell_size(450.0);
        assert_eq!(stretched.x, 150.0);
        let capped = cell_size(299.0);
        assert_eq!(capped.x, MAX_CELL_WIDTH);
        assert_eq!(capped.y, MAX_CELL_WIDTH + CAPTION_HEIGHT);
    }
}
