use std::hash::Hash;

use eframe::egui;

use super::icons::{self, Icon};
use super::theme::{self, colors, metrics};

#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) enum ValueScale {
    Raw,
    RawUnsigned,
    OffsetPercent,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SliderTrack {
    Standard,
    Hue,
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct AdjustmentSpec<'a> {
    pub label: &'a str,
    pub minimum: f32,
    pub maximum: f32,
    pub neutral: f32,
    pub decimals: usize,
    pub step: f64,
    pub suffix: &'a str,
    pub scale: ValueScale,
}

pub(crate) struct AdjustmentResponse {
    pub response: egui::Response,
    pub reset_clicked: bool,
}

pub(crate) fn adjustment_slider(
    ui: &mut egui::Ui,
    value: &mut f32,
    spec: AdjustmentSpec<'_>,
) -> AdjustmentResponse {
    adjustment_slider_with_track(ui, value, spec, SliderTrack::Standard)
}

pub(crate) fn hue_adjustment_slider(
    ui: &mut egui::Ui,
    value: &mut f32,
    spec: AdjustmentSpec<'_>,
) -> AdjustmentResponse {
    adjustment_slider_with_track(ui, value, spec, SliderTrack::Hue)
}

fn adjustment_slider_with_track(
    ui: &mut egui::Ui,
    value: &mut f32,
    spec: AdjustmentSpec<'_>,
    track: SliderTrack,
) -> AdjustmentResponse {
    let mut reset_clicked = false;
    let value_response = ui
        .horizontal(|ui| {
            ui.label(egui::RichText::new(spec.label).color(colors::TEXT));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let formatter_spec = spec;
                let parser_spec = spec;
                let response = ui.add_sized(
                    egui::vec2(74.0, 22.0),
                    egui::DragValue::new(value)
                        .range(spec.minimum..=spec.maximum)
                        .speed(spec.step)
                        .fixed_decimals(spec.decimals)
                        .suffix(spec.suffix)
                        .custom_formatter(move |number, _| {
                            format_adjustment_value(number, formatter_spec)
                        })
                        .custom_parser(move |text| parse_adjustment_value(text, parser_spec)),
                );
                let reset = if approximately_equal(*value, spec.neutral) {
                    let (_, response) =
                        ui.allocate_exact_size(egui::vec2(28.0, 28.0), egui::Sense::hover());
                    response.on_hover_text("Already at the neutral value")
                } else {
                    icon_button(ui, Icon::Reset, "Reset to the neutral value", false, true)
                };
                reset_clicked = reset.clicked();
                response
            })
            .inner
        })
        .inner;

    let available = ui.available_width();
    if track == SliderTrack::Hue {
        let slider_rect = egui::Rect::from_min_size(
            ui.cursor().min,
            egui::vec2(available, ui.spacing().interact_size.y),
        );
        paint_hue_slider_track(ui, slider_rect);
    }
    let slider_response = ui
        .scope(|ui| {
            ui.spacing_mut().slider_width = available;
            if track == SliderTrack::Hue {
                ui.spacing_mut().slider_rail_height = 0.0;
                ui.visuals_mut().widgets.inactive.bg_fill = colors::PANEL_RAISED;
            }
            ui.add(
                egui::Slider::new(value, spec.minimum..=spec.maximum)
                    .show_value(false)
                    .trailing_fill(track != SliderTrack::Hue)
                    .step_by(spec.step),
            )
        })
        .inner;
    paint_neutral_marker(ui, slider_response.rect, spec);
    let response = value_response
        .union(slider_response)
        .on_hover_text("Drag, use arrow keys, or click the value to type.");
    ui.add_space(4.0);

    AdjustmentResponse {
        response,
        reset_clicked,
    }
}

const HUE_RAMP: [egui::Color32; 7] = [
    egui::Color32::from_rgb(220, 55, 55),
    egui::Color32::from_rgb(235, 205, 45),
    egui::Color32::from_rgb(65, 185, 85),
    egui::Color32::from_rgb(45, 185, 185),
    egui::Color32::from_rgb(65, 105, 220),
    egui::Color32::from_rgb(190, 65, 190),
    egui::Color32::from_rgb(220, 55, 55),
];

fn paint_hue_slider_track(ui: &egui::Ui, rect: egui::Rect) {
    let rail = egui::Rect::from_center_size(
        rect.center(),
        egui::vec2(rect.width(), ui.spacing().slider_rail_height.max(6.0)),
    );
    let painter = ui.painter().with_clip_rect(rail);
    const SEGMENTS: usize = 48;
    for index in 0..SEGMENTS {
        let start = index as f32 / SEGMENTS as f32;
        let end = (index + 1) as f32 / SEGMENTS as f32;
        let x_start = egui::lerp(rail.left()..=rail.right(), start);
        let x_end = egui::lerp(rail.left()..=rail.right(), end);
        painter.rect_filled(
            egui::Rect::from_min_max(
                egui::pos2(x_start, rail.top()),
                egui::pos2(x_end + 1.0, rail.bottom()),
            ),
            0.0,
            hue_ramp_color((start + end) * 0.5),
        );
    }
    painter.rect_stroke(
        rail,
        3.0,
        egui::Stroke::new(1.0_f32, colors::TEXT.gamma_multiply(0.35)),
        egui::StrokeKind::Inside,
    );
}

fn hue_ramp_color(position: f32) -> egui::Color32 {
    let position = position.clamp(0.0, 1.0);
    let scaled = position * (HUE_RAMP.len() - 1) as f32;
    let index = (scaled.floor() as usize).min(HUE_RAMP.len() - 2);
    let fraction = scaled - index as f32;
    let start = HUE_RAMP[index];
    let end = HUE_RAMP[index + 1];
    egui::Color32::from_rgb(
        interpolate_channel(start.r(), end.r(), fraction),
        interpolate_channel(start.g(), end.g(), fraction),
        interpolate_channel(start.b(), end.b(), fraction),
    )
}

fn interpolate_channel(start: u8, end: u8, fraction: f32) -> u8 {
    (f32::from(start) + (f32::from(end) - f32::from(start)) * fraction).round() as u8
}

fn paint_neutral_marker(ui: &egui::Ui, rect: egui::Rect, spec: AdjustmentSpec<'_>) {
    let span = spec.maximum - spec.minimum;
    if span <= f32::EPSILON {
        return;
    }
    let normalized = ((spec.neutral - spec.minimum) / span).clamp(0.0, 1.0);
    let marker_x = egui::lerp(rect.left()..=rect.right(), normalized);
    let marker = egui::Stroke::new(1.0_f32, colors::TEXT_DISABLED);
    ui.painter().line_segment(
        [
            egui::pos2(marker_x, rect.center().y - 5.0),
            egui::pos2(marker_x, rect.center().y + 5.0),
        ],
        marker,
    );
}

pub(crate) fn section_header(ui: &mut egui::Ui, title: &str) {
    ui.add_space(4.0);
    ui.horizontal(|ui| {
        ui.label(
            egui::RichText::new(title.to_uppercase())
                .size(10.5)
                .strong()
                .color(colors::TEXT_MUTED),
        );
        ui.add(egui::Separator::default().horizontal().spacing(1.0));
    });
    ui.add_space(5.0);
}

pub(crate) fn subsection_header(ui: &mut egui::Ui, title: &str) {
    ui.add_space(4.0);
    ui.horizontal(|ui| {
        ui.label(egui::RichText::new(title).strong().color(colors::TEXT));
        ui.add(egui::Separator::default().horizontal().spacing(1.0));
    });
    ui.add_space(5.0);
}

pub(crate) fn adjustment_section(
    ui: &mut egui::Ui,
    title: &str,
    add_contents: impl FnOnce(&mut egui::Ui),
) {
    theme::adjustment_section_frame().show(ui, |ui| {
        ui.label(egui::RichText::new(title).size(14.0).strong());
        ui.add_space(7.0);
        add_contents(ui);
    });
    ui.add_space(metrics::ADJUSTMENT_SECTION_GAP);
}

pub(crate) fn icon_button(
    ui: &mut egui::Ui,
    icon: Icon,
    tooltip: &str,
    selected: bool,
    enabled: bool,
) -> egui::Response {
    let size = egui::vec2(28.0, 28.0);
    let sense = if enabled && ui.is_enabled() {
        egui::Sense::click()
    } else {
        egui::Sense::hover()
    };
    let (rect, response) = ui.allocate_exact_size(size, sense);
    let visuals = ui.style().interact_selectable(&response, selected);
    let fill = if selected {
        colors::ACCENT_MUTED
    } else {
        visuals.weak_bg_fill
    };
    ui.painter().rect(
        rect,
        metrics::RADIUS_SMALL,
        fill,
        visuals.bg_stroke,
        egui::StrokeKind::Inside,
    );
    let color = if enabled && ui.is_enabled() {
        if selected {
            colors::ACCENT_HOVER
        } else {
            visuals.fg_stroke.color
        }
    } else {
        colors::TEXT_DISABLED
    };
    icons::paint(ui.painter(), rect, icon, color);
    response.on_hover_text(tooltip)
}

/// A source action uses the same framed treatment at every toolbar width.
/// Narrow toolbars keep the icon and tooltip while dropping the text label.
pub(crate) fn icon_toolbar_button(
    ui: &mut egui::Ui,
    icon: Icon,
    label: &str,
    tooltip: &str,
    compact: bool,
) -> egui::Response {
    let font = egui::FontId::proportional(12.5);
    let text_width = ui
        .painter()
        .layout_no_wrap(label.to_owned(), font.clone(), colors::TEXT)
        .size()
        .x;
    let width = if compact {
        28.0
    } else {
        text_width + 15.0 + 7.0 + 18.0
    };
    let (rect, response) = ui.allocate_exact_size(egui::vec2(width, 28.0), egui::Sense::click());
    let visuals = ui.style().interact(&response);
    ui.painter().rect(
        rect,
        metrics::RADIUS_SMALL,
        visuals.bg_fill,
        visuals.bg_stroke,
        egui::StrokeKind::Inside,
    );
    let icon_rect = egui::Rect::from_center_size(
        egui::pos2(rect.left() + 14.0, rect.center().y),
        egui::Vec2::splat(15.0),
    );
    icons::paint(ui.painter(), icon_rect, icon, visuals.fg_stroke.color);
    if !compact {
        ui.painter().text(
            egui::pos2(icon_rect.right() + 7.0, rect.center().y),
            egui::Align2::LEFT_CENTER,
            label,
            font,
            visuals.fg_stroke.color,
        );
    }
    response.on_hover_text(tooltip)
}

pub(crate) fn toolbar_button(
    ui: &mut egui::Ui,
    label: &str,
    selected: bool,
    enabled: bool,
) -> egui::Response {
    ui.add_enabled(
        enabled,
        egui::Button::new(label)
            .selected(selected)
            .corner_radius(metrics::RADIUS_SMALL),
    )
}

pub(crate) fn primary_button(ui: &mut egui::Ui, label: &str, enabled: bool) -> egui::Response {
    ui.add_enabled(
        enabled,
        egui::Button::new(
            egui::RichText::new(label)
                .strong()
                .color(colors::APP_BACKGROUND),
        )
        .fill(colors::ACCENT)
        .stroke(egui::Stroke::new(1.0_f32, colors::ACCENT_ACTIVE))
        .corner_radius(metrics::RADIUS_SMALL),
    )
}

pub(crate) fn dropdown(
    ui: &mut egui::Ui,
    id_salt: impl Hash,
    label: &str,
    selected: &str,
    add_contents: impl FnOnce(&mut egui::Ui),
) {
    ui.label(egui::RichText::new(label).small().color(colors::TEXT_MUTED));
    let width = ui.available_width();
    let _ = egui::ComboBox::from_id_salt(id_salt)
        .selected_text(selected)
        .width(width)
        .truncate()
        .show_ui(ui, add_contents);
}

pub(crate) fn message_card(
    ui: &mut egui::Ui,
    message: &str,
    color: egui::Color32,
    dismiss_label: Option<&str>,
) -> bool {
    let mut dismissed = false;
    theme::card_frame().show(ui, |ui| {
        ui.colored_label(color, message);
        if let Some(label) = dismiss_label {
            dismissed = ui.small_button(label).clicked();
        }
    });
    dismissed
}

fn approximately_equal(left: f32, right: f32) -> bool {
    (left - right).abs() <= f32::EPSILON * 4.0
}

fn format_adjustment_value(value: f64, spec: AdjustmentSpec<'_>) -> String {
    let displayed = match spec.scale {
        ValueScale::Raw | ValueScale::RawUnsigned => value,
        ValueScale::OffsetPercent => (value - spec.neutral as f64) * 100.0,
    };
    if displayed > 0.0 && spec.scale != ValueScale::RawUnsigned {
        format!("+{displayed:.precision$}", precision = spec.decimals)
    } else {
        format!("{displayed:.precision$}", precision = spec.decimals)
    }
}

fn parse_adjustment_value(text: &str, spec: AdjustmentSpec<'_>) -> Option<f64> {
    let stripped = text
        .trim()
        .trim_end_matches(spec.suffix.trim())
        .trim_end_matches('%')
        .trim_end_matches('×')
        .trim();
    let displayed = stripped.parse::<f64>().ok()?;
    Some(match spec.scale {
        ValueScale::Raw | ValueScale::RawUnsigned => displayed,
        ValueScale::OffsetPercent => displayed / 100.0 + spec.neutral as f64,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn saturation_spec() -> AdjustmentSpec<'static> {
        AdjustmentSpec {
            label: "Saturation",
            minimum: 0.0,
            maximum: 2.0,
            neutral: 1.0,
            decimals: 0,
            step: 0.01,
            suffix: "%",
            scale: ValueScale::OffsetPercent,
        }
    }

    #[test]
    fn offset_percent_format_and_parser_share_the_same_units() {
        let spec = saturation_spec();
        assert_eq!(format_adjustment_value(1.24, spec), "+24");
        assert_eq!(format_adjustment_value(0.82, spec), "-18");
        let positive = parse_adjustment_value("+24%", spec).expect("valid positive percentage");
        let negative = parse_adjustment_value("-18", spec).expect("valid negative percentage");
        assert!((positive - 1.24).abs() < 1.0e-12);
        assert!((negative - 0.82).abs() < 1.0e-12);
    }

    #[test]
    fn raw_values_keep_sign_and_precision() {
        let spec = AdjustmentSpec {
            label: "Exposure",
            minimum: -5.0,
            maximum: 5.0,
            neutral: 0.0,
            decimals: 2,
            step: 0.01,
            suffix: " EV",
            scale: ValueScale::Raw,
        };
        assert_eq!(format_adjustment_value(0.35, spec), "+0.35");
        assert_eq!(parse_adjustment_value("+0.35 EV", spec), Some(0.35));
    }

    #[test]
    fn hue_ramp_wraps_back_to_red() {
        assert_eq!(hue_ramp_color(0.0), HUE_RAMP[0]);
        assert_eq!(hue_ramp_color(1.0), HUE_RAMP[HUE_RAMP.len() - 1]);
        assert_ne!(hue_ramp_color(0.25), HUE_RAMP[0]);
        assert_ne!(hue_ramp_color(0.75), HUE_RAMP[0]);
    }
}
