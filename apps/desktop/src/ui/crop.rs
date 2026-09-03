use eframe::egui;
use rohditor_edit::NormalizedCropRect;

use super::theme::colors;
use super::widgets;
use crate::app::crop::CropAspect;

const HANDLE_RADIUS: f32 = 5.0;
const HIT_RADIUS: f32 = 12.0;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CropHandle {
    NorthWest,
    North,
    NorthEast,
    East,
    SouthEast,
    South,
    SouthWest,
    West,
    Move,
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct CropOverlayModel {
    pub crop: NormalizedCropRect,
    pub active_handle: Option<CropHandle>,
}

#[derive(Debug, Default)]
pub(crate) struct CropOverlayOutput {
    pub drag_started: Option<(CropHandle, (f64, f64))>,
    pub dragged_to: Option<(f64, f64)>,
    pub drag_stopped: bool,
}

/// Paint and hit-test a crop overlay. It returns semantic normalized-pointer
/// events and deliberately has no recipe, worker, or cache knowledge.
pub(crate) fn interact_and_paint(
    context: &egui::Context,
    ui: &egui::Ui,
    response: &egui::Response,
    image_rect: egui::Rect,
    model: CropOverlayModel,
) -> CropOverlayOutput {
    let mut output = CropOverlayOutput::default();
    let crop_rect = normalized_rect(image_rect, model.crop);
    let pointer = response.interact_pointer_pos();
    if let Some(pointer) = pointer.filter(|pointer| image_rect.contains(*pointer)) {
        let hovered = hit_test(crop_rect, pointer);
        if let Some(handle) = model.active_handle.or(hovered) {
            context.set_cursor_icon(cursor_for(handle));
        }
        if response.drag_started_by(egui::PointerButton::Primary)
            && let Some(handle) = hovered
        {
            output.drag_started = Some((handle, normalized(image_rect, pointer)));
        }
        if response.dragged_by(egui::PointerButton::Primary) && model.active_handle.is_some() {
            output.dragged_to = Some(normalized(image_rect, pointer));
        }
        if response.drag_stopped_by(egui::PointerButton::Primary) && model.active_handle.is_some() {
            output.drag_stopped = true;
        }
    }

    let painter = ui.painter();
    let dark = egui::Color32::from_black_alpha(145);
    painter.rect_filled(
        egui::Rect::from_min_max(
            image_rect.min,
            egui::pos2(image_rect.max.x, crop_rect.top()),
        ),
        0.0,
        dark,
    );
    painter.rect_filled(
        egui::Rect::from_min_max(
            egui::pos2(image_rect.min.x, crop_rect.bottom()),
            image_rect.max,
        ),
        0.0,
        dark,
    );
    painter.rect_filled(
        egui::Rect::from_min_max(
            egui::pos2(image_rect.min.x, crop_rect.top()),
            egui::pos2(crop_rect.left(), crop_rect.bottom()),
        ),
        0.0,
        dark,
    );
    painter.rect_filled(
        egui::Rect::from_min_max(
            egui::pos2(crop_rect.right(), crop_rect.top()),
            egui::pos2(image_rect.max.x, crop_rect.bottom()),
        ),
        0.0,
        dark,
    );
    let guide = egui::Stroke::new(1.0, egui::Color32::from_white_alpha(95));
    for fraction in [1.0 / 3.0, 2.0 / 3.0] {
        let x = egui::lerp(crop_rect.left()..=crop_rect.right(), fraction);
        let y = egui::lerp(crop_rect.top()..=crop_rect.bottom(), fraction);
        painter.line_segment(
            [
                egui::pos2(x, crop_rect.top()),
                egui::pos2(x, crop_rect.bottom()),
            ],
            guide,
        );
        painter.line_segment(
            [
                egui::pos2(crop_rect.left(), y),
                egui::pos2(crop_rect.right(), y),
            ],
            guide,
        );
    }
    painter.rect_stroke(
        crop_rect,
        0.0,
        egui::Stroke::new(1.5, colors::ACCENT_HOVER),
        egui::StrokeKind::Inside,
    );
    for (_, point) in handles(crop_rect) {
        painter.circle_filled(point, HANDLE_RADIUS, colors::ACCENT_HOVER);
        painter.circle_stroke(
            point,
            HANDLE_RADIUS,
            egui::Stroke::new(1.0, egui::Color32::BLACK),
        );
    }
    output
}

#[derive(Debug, Clone)]
pub(crate) struct CropPanelModel {
    pub aspect: CropAspect,
    pub locked: bool,
    pub portrait: bool,
    pub dimensions: (usize, usize),
    pub ready: bool,
}

#[derive(Debug, Default)]
pub(crate) struct CropPanelOutput {
    pub aspect: Option<CropAspect>,
    pub locked: Option<bool>,
    pub toggle_orientation: bool,
    pub reset: bool,
    pub cancel: bool,
    pub apply: bool,
}

pub(crate) fn show_panel(context: &egui::Context, model: &CropPanelModel) -> CropPanelOutput {
    let mut output = CropPanelOutput::default();
    egui::Window::new("Geometry · Crop")
        .default_width(230.0)
        .resizable(false)
        .collapsible(false)
        .anchor(egui::Align2::RIGHT_TOP, egui::vec2(-18.0, 64.0))
        .show(context, |ui| {
            ui.label(
                egui::RichText::new("CROP")
                    .small()
                    .color(colors::TEXT_MUTED),
            );
            ui.horizontal_wrapped(|ui| {
                for aspect in CropAspect::ALL {
                    if ui
                        .selectable_label(model.aspect == aspect, aspect.label())
                        .clicked()
                    {
                        output.aspect = Some(aspect);
                    }
                }
            });
            ui.add_space(6.0);
            let mut locked = model.locked;
            if ui.checkbox(&mut locked, "Lock aspect ratio").changed() {
                output.locked = Some(locked);
            }
            if ui
                .button(if model.portrait {
                    "Landscape ratio"
                } else {
                    "Portrait ratio"
                })
                .clicked()
            {
                output.toggle_orientation = true;
            }
            ui.label(
                egui::RichText::new(format!(
                    "{} × {} px",
                    model.dimensions.0, model.dimensions.1
                ))
                .monospace()
                .color(colors::TEXT_MUTED),
            );
            if !model.ready {
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label(egui::RichText::new("Preparing full image…").small());
                });
            }
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                output.reset = ui.button("Reset").clicked();
                output.cancel = ui.button("Cancel").clicked();
                output.apply = widgets::primary_button(ui, "Apply", model.ready).clicked();
            });
        });
    output
}

fn normalized_rect(image: egui::Rect, crop: NormalizedCropRect) -> egui::Rect {
    egui::Rect::from_min_max(
        egui::pos2(
            image.left() + image.width() * crop.left as f32,
            image.top() + image.height() * crop.top as f32,
        ),
        egui::pos2(
            image.left() + image.width() * crop.right as f32,
            image.top() + image.height() * crop.bottom as f32,
        ),
    )
}

fn normalized(image: egui::Rect, point: egui::Pos2) -> (f64, f64) {
    (
        ((point.x - image.left()) / image.width()).clamp(0.0, 1.0) as f64,
        ((point.y - image.top()) / image.height()).clamp(0.0, 1.0) as f64,
    )
}

fn handles(rect: egui::Rect) -> [(CropHandle, egui::Pos2); 8] {
    [
        (CropHandle::NorthWest, rect.left_top()),
        (CropHandle::North, egui::pos2(rect.center().x, rect.top())),
        (CropHandle::NorthEast, rect.right_top()),
        (CropHandle::East, egui::pos2(rect.right(), rect.center().y)),
        (CropHandle::SouthEast, rect.right_bottom()),
        (
            CropHandle::South,
            egui::pos2(rect.center().x, rect.bottom()),
        ),
        (CropHandle::SouthWest, rect.left_bottom()),
        (CropHandle::West, egui::pos2(rect.left(), rect.center().y)),
    ]
}

fn hit_test(rect: egui::Rect, pointer: egui::Pos2) -> Option<CropHandle> {
    for (handle, point) in handles(rect) {
        if point.distance(pointer) <= HIT_RADIUS {
            return Some(handle);
        }
    }
    if pointer.x >= rect.left() - HIT_RADIUS
        && pointer.x <= rect.right() + HIT_RADIUS
        && pointer.y >= rect.top() - HIT_RADIUS
        && pointer.y <= rect.bottom() + HIT_RADIUS
    {
        let distances = [
            (CropHandle::North, (pointer.y - rect.top()).abs()),
            (CropHandle::East, (pointer.x - rect.right()).abs()),
            (CropHandle::South, (pointer.y - rect.bottom()).abs()),
            (CropHandle::West, (pointer.x - rect.left()).abs()),
        ];
        if let Some((handle, distance)) = distances.into_iter().min_by(|a, b| a.1.total_cmp(&b.1))
            && distance <= HIT_RADIUS
        {
            return Some(handle);
        }
    }
    rect.contains(pointer).then_some(CropHandle::Move)
}

fn cursor_for(handle: CropHandle) -> egui::CursorIcon {
    match handle {
        CropHandle::NorthWest | CropHandle::SouthEast => egui::CursorIcon::ResizeNwSe,
        CropHandle::NorthEast | CropHandle::SouthWest => egui::CursorIcon::ResizeNeSw,
        CropHandle::North | CropHandle::South => egui::CursorIcon::ResizeVertical,
        CropHandle::East | CropHandle::West => egui::CursorIcon::ResizeHorizontal,
        CropHandle::Move => egui::CursorIcon::Grab,
    }
}

#[cfg(test)]
mod tests {
    use super::{CropHandle, hit_test};

    #[test]
    fn corners_take_precedence_over_edges_and_interior() {
        let rect = eframe::egui::Rect::from_min_max(
            eframe::egui::pos2(10.0, 10.0),
            eframe::egui::pos2(90.0, 90.0),
        );
        assert_eq!(
            hit_test(rect, eframe::egui::pos2(11.0, 11.0)),
            Some(CropHandle::NorthWest)
        );
        assert_eq!(
            hit_test(rect, eframe::egui::pos2(50.0, 10.5)),
            Some(CropHandle::North)
        );
        assert_eq!(
            hit_test(rect, eframe::egui::pos2(50.0, 50.0)),
            Some(CropHandle::Move)
        );
    }
}
