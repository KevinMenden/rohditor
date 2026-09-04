//! Library grid: the folder browsing view.
//!
//! Presentation-only, like every `ui` module. The app builds a
//! [`LibraryModel`] from catalog state and owned textures, and interprets the
//! returned actions. The grid reports which entries were visible so the app
//! can decode textures lazily.

use eframe::egui;
use std::ops::Range;

use super::icons::{self, Icon};
use super::theme::{self, colors, metrics};
use super::widgets;

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

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) enum LibrarySort {
    #[default]
    Filename,
    CaptureDate,
}

impl LibrarySort {
    const fn label(self) -> &'static str {
        match self {
            Self::Filename => "Filename",
            Self::CaptureDate => "Capture date",
        }
    }

    const ALL: [Self; 2] = [Self::Filename, Self::CaptureDate];
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
    pub folder_path: Option<String>,
    pub entries: Vec<LibraryEntryModel>,
    pub selection: Option<usize>,
    /// Scroll the selected entry into view once, after keyboard navigation.
    pub scroll_to_selection: bool,
    pub sort: LibrarySort,
    pub failure: Option<String>,
    pub scanning: bool,
    pub thumbnail_progress: Option<(usize, usize)>,
}

#[derive(Debug, Default)]
pub(crate) struct LibraryOutput {
    pub selected_entry: Option<usize>,
    pub open_entry: Option<usize>,
    pub sort_changed: Option<LibrarySort>,
    /// Index range whose cells intersected the visible viewport.
    pub visible_range: Range<usize>,
    /// Column count of the laid-out grid, for keyboard navigation.
    pub columns: usize,
}

pub(crate) fn show(context: &egui::Context, model: &LibraryModel) -> LibraryOutput {
    let mut output = LibraryOutput::default();
    egui::CentralPanel::default()
        .frame(theme::library_frame())
        .show(context, |ui| {
            show_header(ui, model, &mut output);
            ui.add_space(metrics::LIBRARY_HEADER_SPACING);
            if let Some(failure) = &model.failure {
                theme::card_frame().show(ui, |ui| {
                    ui.colored_label(colors::ERROR, format!("Catalog scan failed: {failure}"));
                    ui.label(
                        egui::RichText::new(
                            "Choose another folder with Open Folder… in the toolbar.",
                        )
                        .small()
                        .color(colors::TEXT_MUTED),
                    );
                });
                return;
            }
            if model.entries.is_empty() {
                show_empty_state(ui, model);
                return;
            }
            show_grid(ui, model, &mut output);
        });
    output
}

fn show_header(ui: &mut egui::Ui, model: &LibraryModel, output: &mut LibraryOutput) {
    egui::Sides::new().shrink_left().truncate().show(
        ui,
        |ui| {
            ui.vertical(|ui| {
                let title = model.folder_name.as_deref().unwrap_or("No folder open");
                let title_response = ui.add(
                    egui::Label::new(
                        egui::RichText::new(title)
                            .strong()
                            .size(15.0)
                            .color(colors::TEXT),
                    )
                    .truncate(),
                );
                if let Some(path) = &model.folder_path {
                    let _ = title_response.on_hover_text(path);
                }
                if model.folder_name.is_some() {
                    ui.horizontal(|ui| {
                        let mut summary = format!("{} photos", model.entries.len());
                        let loading = if model.scanning {
                            summary.push_str(" · Scanning folder…");
                            true
                        } else if let Some((resolved, total)) = model.thumbnail_progress
                            && resolved < total
                        {
                            summary
                                .push_str(&format!(" · Loading thumbnails… ({resolved}/{total})"));
                            true
                        } else {
                            false
                        };
                        ui.label(
                            egui::RichText::new(summary)
                                .small()
                                .color(colors::TEXT_MUTED),
                        );
                        if loading {
                            ui.spinner();
                        }
                    });
                }
            });
        },
        |ui| {
            if model.folder_name.is_some() {
                output.sort_changed = show_sort_selector(ui, model.sort);
            }
        },
    );
}

fn show_sort_selector(ui: &mut egui::Ui, current: LibrarySort) -> Option<LibrarySort> {
    let mut selected = current;
    widgets::dropdown(ui, "library_sort", "Sort", current.label(), |ui| {
        for option in LibrarySort::ALL {
            ui.selectable_value(&mut selected, option, option.label());
        }
    });
    (selected != current).then_some(selected)
}

fn show_empty_state(ui: &mut egui::Ui, model: &LibraryModel) {
    ui.centered_and_justified(|ui| {
        ui.vertical_centered(|ui| {
            if model.scanning {
                ui.spinner();
                ui.add_space(6.0);
            }
            let message = if model.scanning {
                "Scanning folder for supported Sony ARW photos…"
            } else if model.folder_name.is_some() {
                "No supported Sony ARW photos were found in this folder."
            } else {
                "Use Open Folder… in the toolbar to browse Sony ARW photos."
            };
            ui.label(
                egui::RichText::new(message)
                    .small()
                    .color(colors::TEXT_MUTED),
            );
        });
    });
}

/// Cell size in the Develop-mode filmstrip.
pub(crate) const FILMSTRIP_CELL: egui::Vec2 = egui::vec2(96.0, 84.0);

#[derive(Default)]
pub(crate) struct FilmstripModel {
    pub entries: Vec<LibraryEntryModel>,
    /// Grid-space index of the photo open in the develop viewport.
    pub active: Option<usize>,
    /// Scroll the active entry into view once, when the active photo changes.
    pub scroll_to_active: bool,
}

#[derive(Debug, Default)]
pub(crate) struct FilmstripOutput {
    pub open_entry: Option<usize>,
    /// Index range whose cells intersected the visible viewport.
    pub visible_range: Range<usize>,
}

/// Horizontal thumbnail strip below the develop viewport.
pub(crate) fn show_filmstrip(context: &egui::Context, model: &FilmstripModel) -> FilmstripOutput {
    let mut output = FilmstripOutput::default();
    egui::TopBottomPanel::bottom("filmstrip")
        .exact_height(FILMSTRIP_CELL.y + 24.0)
        .frame(theme::side_panel_frame())
        .show(context, |ui| {
            let mut next_index = 0_usize;
            let mut visible = Range { start: 0, end: 0 };
            egui::ScrollArea::horizontal()
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    ui.horizontal(|ui| {
                        ui.spacing_mut().item_spacing.x = 8.0;
                        while next_index < model.entries.len() {
                            let index = next_index;
                            next_index += 1;
                            let entry = &model.entries[index];
                            let (rect, response) =
                                ui.allocate_exact_size(FILMSTRIP_CELL, egui::Sense::click());
                            let active = model.active == Some(index);
                            paint_entry_card(ui, rect, entry, active);
                            let response = if response.hovered() {
                                response.on_hover_text(&entry.name)
                            } else {
                                response
                            };
                            if response.clicked() {
                                output.open_entry = Some(index);
                            }
                            if active && model.scroll_to_active {
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
                });
            output.visible_range = visible;
        });
    output
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
                    ui.spacing_mut().item_spacing.x = metrics::LIBRARY_GRID_GAP;
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
                ui.add_space(metrics::LIBRARY_GRID_GAP);
            }
        });
    output.visible_range = visible;
}

fn paint_cell(ui: &mut egui::Ui, rect: egui::Rect, entry: &LibraryEntryModel, selected: bool) {
    paint_entry_card(ui, rect, entry, selected);

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

/// Card background plus the thumbnail area content, shared by the library
/// grid and the develop filmstrip.
fn paint_entry_card(
    ui: &mut egui::Ui,
    rect: egui::Rect,
    entry: &LibraryEntryModel,
    selected: bool,
) {
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
        rect.right_bottom() - egui::vec2(CELL_PADDING, CELL_PADDING),
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
            let icon_rect = egui::Rect::from_center_size(
                image_rect.center() - egui::vec2(0.0, 7.0),
                egui::vec2(22.0, 22.0),
            );
            icons::paint(ui.painter(), icon_rect, Icon::Photo, colors::TEXT_DISABLED);
            ui.painter().text(
                image_rect.center() + egui::vec2(0.0, 12.0),
                egui::Align2::CENTER_CENTER,
                "No preview",
                egui::FontId::proportional(10.0),
                colors::TEXT_DISABLED,
            );
        }
        (LibraryEntryState::Failed, _) => {
            paint_centered_hint(ui, image_rect, "Unreadable", colors::ERROR);
        }
        (LibraryEntryState::Pending, _) => {
            paint_centered_hint(ui, image_rect, "Loading…", colors::TEXT_DISABLED);
        }
    }
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

/// Column count derived from the available width, minimum cell size, and the
/// gutter between cells.
pub(crate) fn column_count(available_width: f32) -> usize {
    ((available_width.max(0.0) + metrics::LIBRARY_GRID_GAP)
        / (MIN_CELL_WIDTH + metrics::LIBRARY_GRID_GAP))
        .floor()
        .max(1.0) as usize
}

/// Cell size for a given available width. Gaps are removed before the
/// remaining width is shared, so the last column ends at the content frame.
pub(crate) fn cell_size(available_width: f32) -> egui::Vec2 {
    let columns = column_count(available_width);
    let gaps = metrics::LIBRARY_GRID_GAP * columns.saturating_sub(1) as f32;
    let width = ((available_width.max(0.0) - gaps) / columns as f32).max(0.0);
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
        assert_eq!(column_count(311.0), 1);
        assert_eq!(column_count(312.0), 2);
        assert_eq!(column_count(1_500.0), 9);
    }

    #[test]
    fn cells_include_gutters_and_fill_the_content_width() {
        let narrow = cell_size(160.0);
        assert_eq!(narrow, egui::vec2(160.0, 160.0 + CAPTION_HEIGHT));
        let stretched = cell_size(450.0);
        assert_eq!(stretched.x, (450.0 - metrics::LIBRARY_GRID_GAP) / 2.0);
        let capped = cell_size(299.0);
        assert_eq!(capped.x, 299.0);
        assert_eq!(capped.y, 299.0 + CAPTION_HEIGHT);
        let columns = column_count(1_500.0);
        let cell = cell_size(1_500.0);
        assert!(
            (cell.x * columns as f32
                + metrics::LIBRARY_GRID_GAP * columns.saturating_sub(1) as f32
                - 1_500.0)
                .abs()
                < f32::EPSILON
        );
    }
}
