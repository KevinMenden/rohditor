use std::time::Duration;

use eframe::egui;
use rohditor_core::DemosaicAlgorithm;

use super::theme::{self, colors};
use super::widgets;

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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PreviewSource {
    Embedded,
    FastCpu,
    FastGpu,
    HighQualityCpu,
    HighQualityGpu,
    OneToOneCpu,
}

impl PreviewSource {
    pub(crate) const fn developed(algorithm: DemosaicAlgorithm, gpu: bool) -> Self {
        match (algorithm, gpu) {
            (DemosaicAlgorithm::Bilinear, false) => Self::FastCpu,
            (DemosaicAlgorithm::Bilinear, true) => Self::FastGpu,
            (DemosaicAlgorithm::MalvarHeCutler, false) => Self::HighQualityCpu,
            (DemosaicAlgorithm::MalvarHeCutler, true) => Self::HighQualityGpu,
        }
    }

    pub(crate) const fn is_developed(self) -> bool {
        !matches!(self, Self::Embedded)
    }

    pub(crate) const fn label(self) -> &'static str {
        match self {
            Self::Embedded => "EMBEDDED PREVIEW · DEVELOPING RAW",
            Self::FastCpu => "FAST ANTIALIASED PREVIEW · CPU",
            Self::FastGpu => "FAST ANTIALIASED PREVIEW · GPU",
            Self::HighQualityCpu => "HIGH-QUALITY ANTIALIASED PREVIEW · CPU",
            Self::HighQualityGpu => "HIGH-QUALITY ANTIALIASED PREVIEW · GPU",
            Self::OneToOneCpu => "1:1 SOURCE-SCALE DEVELOPED · CPU",
        }
    }

    pub(crate) const fn short_label(self) -> &'static str {
        match self {
            Self::Embedded => "Embedded preview",
            Self::FastCpu => "Fast preview · CPU",
            Self::FastGpu => "Fast preview · GPU",
            Self::HighQualityCpu => "High-quality preview · CPU",
            Self::HighQualityGpu => "High-quality preview · GPU",
            Self::OneToOneCpu => "1:1 source-scale · CPU",
        }
    }
}

#[derive(Debug)]
pub(crate) struct ViewState {
    fit: bool,
    zoom: f32,
    pan: egui::Vec2,
    zoom_feedback_until: f64,
}

impl Default for ViewState {
    fn default() -> Self {
        Self {
            fit: true,
            zoom: 1.0,
            pan: egui::Vec2::ZERO,
            zoom_feedback_until: 0.0,
        }
    }
}

impl ViewState {
    pub(crate) fn fit(&mut self, now: f64) {
        self.fit = true;
        self.pan = egui::Vec2::ZERO;
        self.show_zoom_feedback(now);
    }

    pub(crate) fn actual_size(&mut self, now: f64) {
        self.fit = false;
        self.zoom = 1.0;
        self.pan = egui::Vec2::ZERO;
        self.show_zoom_feedback(now);
    }

    pub(crate) const fn is_fit(&self) -> bool {
        self.fit
    }

    pub(crate) fn is_actual_size(&self) -> bool {
        !self.fit && (self.zoom - 1.0).abs() <= f32::EPSILON * 4.0
    }

    pub(crate) fn zoom_label(&self) -> String {
        if self.fit {
            "FIT".to_owned()
        } else if self.is_actual_size() {
            "SOURCE 100%".to_owned()
        } else {
            format!("{:.0}%", self.zoom * 100.0)
        }
    }

    fn show_zoom_feedback(&mut self, now: f64) {
        self.zoom_feedback_until = now + 0.8;
    }

    fn pan_by(&mut self, fit_scale: f32, delta: egui::Vec2) {
        if self.fit {
            self.fit = false;
            self.zoom = fit_scale;
        }
        self.pan += delta;
    }

    fn zoom_by(&mut self, fit_scale: f32, scroll_delta: f32, now: f64) {
        let current = if self.fit { fit_scale } else { self.zoom };
        self.fit = false;
        self.zoom = (current * (scroll_delta * 0.002).exp()).clamp(0.03, 16.0);
        self.show_zoom_feedback(now);
    }
}

pub(crate) struct ViewportModel<'a> {
    pub has_document: bool,
    pub preparing: bool,
    pub texture: Option<&'a PreviewTexture>,
    pub source: Option<PreviewSource>,
    pub white_balance_picker_active: bool,
}

#[derive(Debug, Default)]
pub(crate) struct ViewportOutput {
    pub open: bool,
    pub white_balance_pick: Option<egui::Pos2>,
}

pub(crate) fn show(
    context: &egui::Context,
    model: ViewportModel<'_>,
    view: &mut ViewState,
) -> ViewportOutput {
    let mut output = ViewportOutput::default();
    egui::CentralPanel::default()
        .frame(theme::viewport_frame())
        .show(context, |ui| {
            let viewport = ui.max_rect();
            let response = ui.allocate_rect(viewport, egui::Sense::click_and_drag());
            let Some(texture) = model.texture else {
                show_empty_state(ui, model.has_document, model.preparing, &mut output);
                return;
            };

            let padded = viewport.shrink(18.0);
            let image_size = texture.size_vec2();
            let fit_scale = fit_scale(padded.size(), image_size);
            let now = context.input(|input| input.time);
            if !model.white_balance_picker_active
                && response.dragged_by(egui::PointerButton::Primary)
            {
                view.pan_by(fit_scale, response.drag_delta());
            }
            if response.hovered() {
                let scroll = context.input(|input| input.smooth_scroll_delta.y);
                if scroll.abs() > f32::EPSILON {
                    view.zoom_by(fit_scale, scroll, now);
                    context.request_repaint_after(Duration::from_millis(16));
                }
            }

            let scale = if view.fit { fit_scale } else { view.zoom };
            let size = image_size * scale;
            let image_rect = egui::Rect::from_center_size(viewport.center() + view.pan, size);
            if model.white_balance_picker_active {
                if response.clicked_by(egui::PointerButton::Primary)
                    && let Some(pointer) = response.interact_pointer_pos()
                    && image_rect.contains(pointer)
                {
                    output.white_balance_pick = Some(egui::pos2(
                        ((pointer.x - image_rect.left()) / image_rect.width()).clamp(0.0, 1.0),
                        ((pointer.y - image_rect.top()) / image_rect.height()).clamp(0.0, 1.0),
                    ));
                }
                paint_pill(
                    ui,
                    viewport.center_top() + egui::vec2(0.0, 16.0),
                    egui::Align2::CENTER_TOP,
                    "CLICK A NEUTRAL AREA",
                    colors::ACCENT_HOVER,
                );
            }
            ui.painter().rect_filled(
                image_rect.expand(8.0),
                2.0,
                egui::Color32::from_black_alpha(85),
            );
            texture.paint(ui, image_rect);
            ui.painter().rect_stroke(
                image_rect,
                0.0,
                egui::Stroke::new(1.0, egui::Color32::from_white_alpha(18)),
                egui::StrokeKind::Inside,
            );

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

            if now < view.zoom_feedback_until {
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

fn show_empty_state(
    ui: &mut egui::Ui,
    has_document: bool,
    preparing: bool,
    output: &mut ViewportOutput,
) {
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
                ui.add_space(12.0);
                output.open = widgets::primary_button(ui, "Open RAW…", true).clicked();
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

fn fit_scale(viewport: egui::Vec2, image: egui::Vec2) -> f32 {
    if image.x <= 0.0 || image.y <= 0.0 {
        return 1.0;
    }
    (viewport.x / image.x).min(viewport.y / image.y).max(0.01)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fit_scale_respects_both_axes_and_never_reaches_zero() {
        assert_eq!(
            fit_scale(egui::vec2(800.0, 600.0), egui::vec2(1600.0, 900.0)),
            0.5
        );
        assert_eq!(
            fit_scale(egui::vec2(600.0, 800.0), egui::vec2(900.0, 1600.0)),
            0.5
        );
        assert_eq!(fit_scale(egui::Vec2::ZERO, egui::vec2(100.0, 100.0)), 0.01);
    }

    #[test]
    fn view_commands_reset_pan_and_report_their_mode() {
        let mut state = ViewState {
            fit: false,
            zoom: 2.0,
            pan: egui::vec2(40.0, -12.0),
            zoom_feedback_until: 0.0,
        };
        state.fit(3.0);
        assert!(state.is_fit());
        assert_eq!(state.pan, egui::Vec2::ZERO);
        assert_eq!(state.zoom_label(), "FIT");
        state.actual_size(4.0);
        assert!(!state.is_fit());
        assert_eq!(state.zoom_label(), "SOURCE 100%");
    }

    #[test]
    fn pan_and_wheel_zoom_leave_fit_mode_without_touching_processing_state() {
        let mut state = ViewState::default();
        state.pan_by(0.5, egui::vec2(12.0, -8.0));
        assert!(!state.is_fit());
        assert_eq!(state.zoom, 0.5);
        assert_eq!(state.pan, egui::vec2(12.0, -8.0));

        state.zoom_by(0.5, 120.0, 5.0);
        assert!(state.zoom > 0.5);
        assert_eq!(state.pan, egui::vec2(12.0, -8.0));
        assert_eq!(state.zoom_feedback_until, 5.8);
    }
}
