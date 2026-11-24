use eframe::egui;
use rohditor_edit::NormalizedCropRect;
use rohditor_engine::tools::{CropAspect, CropHandle};

use super::theme::colors;
use super::viewport::placement;

const HANDLE_RADIUS: f32 = 5.0;

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
    if let Some(pointer) = pointer {
        let hovered = CropHandle::at(placement(image_rect), model.crop, pointer.into());
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
    let guide = egui::Stroke::new(1.0_f32, egui::Color32::from_white_alpha(95));
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
        egui::Stroke::new(1.5_f32, colors::ACCENT_HOVER),
        egui::StrokeKind::Inside,
    );
    let handles = CropHandle::OUTLINE.into_iter();
    for position in handles.filter_map(|handle| handle.position(model.crop)) {
        let point = egui::Pos2::from(placement(image_rect).to_screen(position));
        painter.circle_filled(point, HANDLE_RADIUS, colors::ACCENT_HOVER);
        painter.circle_stroke(
            point,
            HANDLE_RADIUS,
            egui::Stroke::new(1.0_f32, egui::Color32::BLACK),
        );
    }
    output
}

#[derive(Debug, Clone)]
pub(crate) struct CropPanelModel {
    pub aspect: CropAspect,
    pub locked: bool,
    pub dimensions: (usize, usize),
    pub ready: bool,
}

#[derive(Debug, Default)]
pub(crate) struct CropPanelOutput {
    pub aspect: Option<CropAspect>,
    pub locked: Option<bool>,
    pub flip_ratio: bool,
    pub reset: bool,
    pub cancel: bool,
}

/// Crop controls for the tool panel body. Leaving the tool commits the draft,
/// so there is no Apply button; Cancel (also Esc) discards it.
pub(crate) fn show_controls(ui: &mut egui::Ui, model: &CropPanelModel) -> CropPanelOutput {
    let mut output = CropPanelOutput::default();
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
        .add_enabled(model.locked, egui::Button::new("Flip ratio"))
        .clicked()
    {
        output.flip_ratio = true;
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
    });
    ui.label(
        egui::RichText::new("Enter or switching tools applies · Esc cancels")
            .small()
            .color(colors::TEXT_MUTED),
    );
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
    placement(image).to_image(point.into())
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
