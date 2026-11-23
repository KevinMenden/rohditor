use std::time::Duration;

use eframe::egui;
use rohditor_engine::preview::PreviewSource;
use rohditor_engine::tools::Picker;
use rohditor_engine::view::{ViewState, fit_scale};

use super::crop::{CropOverlayModel, CropOverlayOutput, interact_and_paint};
use super::mask_overlay::{self, MaskOverlayModel, MaskOverlayOutput};
use super::theme::{self, colors};

#[derive(Clone)]
pub(crate) enum PreviewTexture {
    Cpu(egui::TextureHandle),
    Gpu {
        id: egui::TextureId,
        size: egui::Vec2,
    },
}

impl PreviewTexture {
    pub(crate) fn size_vec2(&self) -> egui::Vec2 {
        match self {
            Self::Cpu(texture) => texture.size_vec2(),
            Self::Gpu { size, .. } => *size,
        }
    }

    pub(crate) fn dimensions(&self) -> (usize, usize) {
        let size = self.size_vec2();
        (size.x.round() as usize, size.y.round() as usize)
    }

    fn paint(&self, ui: &mut egui::Ui, rect: egui::Rect) {
        match self {
            Self::Cpu(texture) => {
                egui::Image::new(texture)
                    .fit_to_exact_size(rect.size())
                    .texture_options(egui::TextureOptions::LINEAR)
                    .paint_at(ui, rect);
            }
            Self::Gpu { id, size } => {
                egui::Image::from_texture((*id, *size))
                    .fit_to_exact_size(rect.size())
                    .texture_options(egui::TextureOptions::LINEAR)
                    .paint_at(ui, rect);
            }
        }
    }
}

pub(crate) struct ViewportModel<'a> {
    pub has_document: bool,
    pub preparing: bool,
    pub texture: Option<&'a PreviewTexture>,
    /// Clipped-pixel tint over the whole preview; `None` when the overlay is off.
    pub clipping: Option<&'a egui::TextureHandle>,
    pub source: Option<PreviewSource>,
    pub picker_mode: Option<Picker>,
    pub crop: Option<CropOverlayModel>,
    pub masks: Option<MaskOverlayModel<'a>>,
}

#[derive(Debug, Default)]
pub(crate) struct ViewportOutput {
    pub picker_sample: Option<(Picker, egui::Pos2)>,
    pub crop: CropOverlayOutput,
    pub masks: MaskOverlayOutput,
}

pub(crate) fn show(
    root: &mut egui::Ui,
    model: ViewportModel<'_>,
    view: &mut ViewState,
) -> ViewportOutput {
    let mut output = ViewportOutput::default();
    let context = root.ctx().clone();
    egui::CentralPanel::default()
        .frame(theme::viewport_frame())
        .show(root, |ui| {
            let viewport = ui.max_rect();
            let response = ui.allocate_rect(viewport, egui::Sense::click_and_drag());
            let Some(texture) = model.texture else {
                show_empty_state(ui, model.has_document, model.preparing);
                return;
            };

            let padded = viewport.shrink(18.0);
            let image_size = texture.size_vec2();
            let fit_scale = fit_scale(to_pair(padded.size()), to_pair(image_size));
            let now = ui.input(|input| input.time);
            // A mask gesture owns the drag it starts on a handle; any other drag pans. The grab
            // is judged against the image as it is on screen before this frame's pan.
            let masks_capture = model.masks.as_ref().is_some_and(|masks| {
                let size = image_size * view.scale(fit_scale);
                let rect =
                    egui::Rect::from_center_size(viewport.center() + from_pair(view.pan()), size);
                mask_overlay::captures_drag(ui, &response, rect, masks)
            });
            if model.picker_mode.is_none()
                && model.crop.is_none()
                && !masks_capture
                && response.dragged_by(egui::PointerButton::Primary)
            {
                view.pan_by(
                    fit_scale,
                    to_pair(viewport.size()),
                    to_pair(image_size),
                    to_pair(response.drag_delta()),
                );
            }
            if response.hovered() {
                let scroll = ui.input(|input| input.smooth_scroll_delta.y);
                if scroll.abs() > f32::EPSILON {
                    view.zoom_by(
                        fit_scale,
                        to_pair(viewport.size()),
                        to_pair(image_size),
                        response
                            .hover_pos()
                            .map(|pointer| to_pair(pointer - viewport.center())),
                        scroll,
                        now,
                    );
                    context.request_repaint_after(Duration::from_millis(16));
                }
            }

            let scale = view.scale(fit_scale);
            let size = image_size * scale;
            let image_rect =
                egui::Rect::from_center_size(viewport.center() + from_pair(view.pan()), size);
            if let Some(picker_mode) = model.picker_mode {
                if response
                    .hover_pos()
                    .is_some_and(|pointer| image_rect.contains(pointer))
                {
                    context.set_cursor_icon(egui::CursorIcon::Crosshair);
                }
                if response.clicked_by(egui::PointerButton::Primary)
                    && let Some(pointer) = response.interact_pointer_pos()
                    && image_rect.contains(pointer)
                {
                    output.picker_sample = Some((
                        picker_mode,
                        egui::pos2(
                            ((pointer.x - image_rect.left()) / image_rect.width()).clamp(0.0, 1.0),
                            ((pointer.y - image_rect.top()) / image_rect.height()).clamp(0.0, 1.0),
                        ),
                    ));
                }
            }
            ui.painter().rect_filled(
                image_rect.expand(8.0),
                2.0,
                egui::Color32::from_black_alpha(85),
            );
            texture.paint(ui, image_rect);
            if let Some(clipping) = model.clipping {
                egui::Image::new(clipping)
                    .fit_to_exact_size(image_rect.size())
                    .paint_at(ui, image_rect);
            }
            ui.painter().rect_stroke(
                image_rect,
                0.0,
                egui::Stroke::new(1.0_f32, egui::Color32::from_white_alpha(18)),
                egui::StrokeKind::Inside,
            );

            if let Some(crop) = model.crop {
                output.crop = interact_and_paint(&context, ui, &response, image_rect, crop);
            }

            if let Some(masks) = model.masks.as_ref() {
                output.masks =
                    mask_overlay::interact_and_paint(&context, ui, &response, image_rect, masks);
            }

            if let Some(source) = model.source
                && (source == PreviewSource::Embedded || response.hovered())
            {
                paint_pill(
                    ui,
                    viewport.left_top() + egui::vec2(16.0, 16.0),
                    egui::Align2::LEFT_TOP,
                    source.label(),
                    if source == PreviewSource::Embedded {
                        colors::ACCENT_HOVER
                    } else {
                        colors::TEXT_MUTED
                    },
                );
            }

            if view.zoom_feedback_active(now) {
                paint_pill(
                    ui,
                    viewport.right_bottom() - egui::vec2(16.0, 16.0),
                    egui::Align2::RIGHT_BOTTOM,
                    &format!("{:.0}%", scale * 100.0),
                    colors::TEXT,
                );
                context.request_repaint_after(Duration::from_millis(50));
            }
        });
    output
}

fn show_empty_state(ui: &mut egui::Ui, has_document: bool, preparing: bool) {
    ui.centered_and_justified(|ui| {
        ui.vertical_centered(|ui| {
            if preparing {
                ui.spinner();
                ui.add_space(8.0);
                ui.label("Preparing developed preview…");
                ui.label(
                    egui::RichText::new("The editor remains responsive while RAW work runs")
                        .small()
                        .color(colors::TEXT_MUTED),
                );
            } else if has_document {
                ui.label(egui::RichText::new("Preview unavailable").strong());
                ui.label(
                    egui::RichText::new("Check the adjustment panel for a decoding error")
                        .color(colors::TEXT_MUTED),
                );
            } else {
                ui.label(egui::RichText::new("Develop a Sony RAW photo").heading());
                ui.label(
                    egui::RichText::new("Non-destructive edits · GPU-resident previews")
                        .color(colors::TEXT_MUTED),
                );
            }
        });
    });
}

fn paint_pill(
    ui: &egui::Ui,
    anchor: egui::Pos2,
    alignment: egui::Align2,
    text: &str,
    text_color: egui::Color32,
) {
    let font = egui::FontId::monospace(10.5);
    let galley = ui
        .painter()
        .layout_no_wrap(text.to_owned(), font.clone(), text_color);
    let size = galley.size() + egui::vec2(18.0, 10.0);
    let rect = alignment.anchor_size(anchor, size);
    let frame = theme::overlay_frame();
    ui.painter().rect(
        rect,
        frame.corner_radius,
        frame.fill,
        frame.stroke,
        egui::StrokeKind::Inside,
    );
    ui.painter()
        .galley(rect.center() - galley.size() * 0.5, galley, text_color);
}

/// The engine's view math is toolkit-neutral; these convert egui geometry to and from it.
fn to_pair(vector: egui::Vec2) -> [f32; 2] {
    [vector.x, vector.y]
}

fn from_pair(pair: [f32; 2]) -> egui::Vec2 {
    egui::vec2(pair[0], pair[1])
}
