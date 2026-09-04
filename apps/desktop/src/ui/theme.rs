use std::collections::BTreeMap;

use eframe::egui;

pub(crate) mod colors {
    use eframe::egui::Color32;

    pub(crate) const APP_BACKGROUND: Color32 = Color32::from_rgb(14, 16, 19);
    pub(crate) const VIEWPORT: Color32 = Color32::from_rgb(9, 10, 12);
    pub(crate) const PANEL: Color32 = Color32::from_rgb(22, 25, 30);
    pub(crate) const PANEL_RAISED: Color32 = Color32::from_rgb(28, 32, 38);
    pub(crate) const FIELD: Color32 = Color32::from_rgb(16, 18, 22);
    pub(crate) const HOVER: Color32 = Color32::from_rgb(38, 43, 51);
    pub(crate) const ACTIVE: Color32 = Color32::from_rgb(47, 53, 62);
    pub(crate) const BORDER: Color32 = Color32::from_rgb(43, 49, 58);
    pub(crate) const BORDER_STRONG: Color32 = Color32::from_rgb(61, 69, 80);

    pub(crate) const TEXT: Color32 = Color32::from_rgb(230, 233, 238);
    pub(crate) const TEXT_MUTED: Color32 = Color32::from_rgb(145, 153, 165);
    pub(crate) const TEXT_DISABLED: Color32 = Color32::from_rgb(91, 97, 107);

    pub(crate) const ACCENT: Color32 = Color32::from_rgb(219, 157, 75);
    pub(crate) const ACCENT_HOVER: Color32 = Color32::from_rgb(237, 178, 96);
    pub(crate) const ACCENT_ACTIVE: Color32 = Color32::from_rgb(193, 127, 48);
    pub(crate) const ACCENT_MUTED: Color32 = Color32::from_rgb(73, 57, 38);

    pub(crate) const SUCCESS: Color32 = Color32::from_rgb(104, 186, 137);
    pub(crate) const WARNING: Color32 = Color32::from_rgb(226, 177, 96);
    pub(crate) const ERROR: Color32 = Color32::from_rgb(224, 111, 111);
}

pub(crate) mod metrics {
    pub(crate) const RADIUS_SMALL: u8 = 3;
    pub(crate) const RADIUS: u8 = 5;
    pub(crate) const RADIUS_LARGE: u8 = 8;
    pub(crate) const TOOLBAR_HEIGHT: f32 = 48.0;
    pub(crate) const STATUS_HEIGHT: f32 = 27.0;
    pub(crate) const ADJUSTMENT_PANEL_WIDTH: f32 = 312.0;
    pub(crate) const FILE_PANEL_WIDTH: f32 = 190.0;
    pub(crate) const FILE_PANEL_BREAKPOINT: f32 = 1_120.0;
    pub(crate) const NARROW_TOOLBAR_BREAKPOINT: f32 = 1_020.0;
    /// Horizontal and vertical inset around the Library browsing surface.
    pub(crate) const LIBRARY_CONTENT_PADDING: i8 = 22;
    pub(crate) const LIBRARY_HEADER_SPACING: f32 = 10.0;
    pub(crate) const LIBRARY_GRID_GAP: f32 = 12.0;
}

/// Install the complete Rohditor visual language on an egui context.
pub(crate) fn apply(context: &egui::Context) {
    let mut style = (*context.style()).clone();
    style.text_styles = BTreeMap::from([
        (
            egui::TextStyle::Heading,
            egui::FontId::new(20.0, egui::FontFamily::Proportional),
        ),
        (
            egui::TextStyle::Body,
            egui::FontId::new(13.0, egui::FontFamily::Proportional),
        ),
        (
            egui::TextStyle::Button,
            egui::FontId::new(12.5, egui::FontFamily::Proportional),
        ),
        (
            egui::TextStyle::Small,
            egui::FontId::new(11.0, egui::FontFamily::Proportional),
        ),
        (
            egui::TextStyle::Monospace,
            egui::FontId::new(12.0, egui::FontFamily::Monospace),
        ),
    ]);
    style.drag_value_text_style = egui::TextStyle::Monospace;
    style.spacing.item_spacing = egui::vec2(7.0, 6.0);
    style.spacing.window_margin = egui::Margin::same(12);
    style.spacing.menu_margin = egui::Margin::same(8);
    style.spacing.button_padding = egui::vec2(9.0, 5.0);
    style.spacing.interact_size = egui::vec2(34.0, 26.0);
    style.spacing.slider_width = 180.0;
    style.spacing.slider_rail_height = 3.0;
    style.spacing.combo_width = 140.0;
    style.spacing.scroll = egui::style::ScrollStyle::thin();
    style.interaction.tooltip_delay = 0.35;
    style.animation_time = 0.12;

    let corner = egui::CornerRadius::same(metrics::RADIUS);
    let small_corner = egui::CornerRadius::same(metrics::RADIUS_SMALL);
    let mut visuals = egui::Visuals::dark();
    visuals.override_text_color = Some(colors::TEXT);
    visuals.weak_text_color = Some(colors::TEXT_MUTED);
    visuals.panel_fill = colors::PANEL;
    visuals.window_fill = colors::PANEL_RAISED;
    visuals.extreme_bg_color = colors::FIELD;
    visuals.text_edit_bg_color = Some(colors::FIELD);
    visuals.faint_bg_color = colors::APP_BACKGROUND;
    visuals.code_bg_color = colors::FIELD;
    visuals.window_corner_radius = egui::CornerRadius::same(metrics::RADIUS_LARGE);
    visuals.menu_corner_radius = corner;
    visuals.window_stroke = egui::Stroke::new(1.0_f32, colors::BORDER_STRONG);
    visuals.window_shadow = egui::epaint::Shadow {
        offset: [0, 6],
        blur: 18,
        spread: 0,
        color: egui::Color32::from_black_alpha(110),
    };
    visuals.popup_shadow = visuals.window_shadow;
    visuals.selection.bg_fill = colors::ACCENT_MUTED;
    visuals.selection.stroke = egui::Stroke::new(1.0_f32, colors::ACCENT_HOVER);
    visuals.hyperlink_color = colors::ACCENT_HOVER;
    visuals.warn_fg_color = colors::WARNING;
    visuals.error_fg_color = colors::ERROR;
    visuals.slider_trailing_fill = true;
    visuals.button_frame = true;
    visuals.collapsing_header_frame = false;
    visuals.interact_cursor = Some(egui::CursorIcon::PointingHand);
    visuals.disabled_alpha = 0.48;

    visuals.widgets.noninteractive = egui::style::WidgetVisuals {
        bg_fill: colors::PANEL,
        weak_bg_fill: egui::Color32::TRANSPARENT,
        bg_stroke: egui::Stroke::new(1.0_f32, colors::BORDER),
        corner_radius: corner,
        fg_stroke: egui::Stroke::new(1.0_f32, colors::TEXT),
        expansion: 0.0,
    };
    visuals.widgets.inactive = egui::style::WidgetVisuals {
        bg_fill: colors::FIELD,
        weak_bg_fill: colors::PANEL_RAISED,
        bg_stroke: egui::Stroke::new(1.0_f32, colors::BORDER),
        corner_radius: small_corner,
        fg_stroke: egui::Stroke::new(1.0_f32, colors::TEXT_MUTED),
        expansion: 0.0,
    };
    visuals.widgets.hovered = egui::style::WidgetVisuals {
        bg_fill: colors::HOVER,
        weak_bg_fill: colors::HOVER,
        bg_stroke: egui::Stroke::new(1.0_f32, colors::BORDER_STRONG),
        corner_radius: small_corner,
        fg_stroke: egui::Stroke::new(1.25_f32, colors::TEXT),
        expansion: 0.0,
    };
    visuals.widgets.active = egui::style::WidgetVisuals {
        bg_fill: colors::ACTIVE,
        weak_bg_fill: colors::ACTIVE,
        bg_stroke: egui::Stroke::new(1.0_f32, colors::ACCENT_ACTIVE),
        corner_radius: small_corner,
        fg_stroke: egui::Stroke::new(1.25_f32, colors::ACCENT_HOVER),
        expansion: 0.0,
    };
    visuals.widgets.open = egui::style::WidgetVisuals {
        bg_fill: colors::ACTIVE,
        weak_bg_fill: colors::ACTIVE,
        bg_stroke: egui::Stroke::new(1.0_f32, colors::ACCENT),
        corner_radius: small_corner,
        fg_stroke: egui::Stroke::new(1.25_f32, colors::TEXT),
        expansion: 0.0,
    };
    style.visuals = visuals;
    context.set_style(style);
}

pub(crate) fn toolbar_frame() -> egui::Frame {
    egui::Frame::new()
        .fill(colors::PANEL)
        .inner_margin(egui::Margin::symmetric(12, 8))
        .stroke(egui::Stroke::new(1.0_f32, colors::BORDER))
}

pub(crate) fn side_panel_frame() -> egui::Frame {
    egui::Frame::new()
        .fill(colors::PANEL)
        .inner_margin(egui::Margin::same(12))
        .stroke(egui::Stroke::new(1.0_f32, colors::BORDER))
}

pub(crate) fn status_frame() -> egui::Frame {
    egui::Frame::new()
        .fill(colors::PANEL)
        .inner_margin(egui::Margin::symmetric(10, 5))
        .stroke(egui::Stroke::new(1.0_f32, colors::BORDER))
}

pub(crate) fn viewport_frame() -> egui::Frame {
    egui::Frame::new().fill(colors::VIEWPORT)
}

pub(crate) fn library_frame() -> egui::Frame {
    egui::Frame::new()
        .fill(colors::VIEWPORT)
        .inner_margin(egui::Margin::same(metrics::LIBRARY_CONTENT_PADDING))
}

pub(crate) fn card_frame() -> egui::Frame {
    egui::Frame::new()
        .fill(colors::PANEL_RAISED)
        .inner_margin(egui::Margin::same(10))
        .corner_radius(metrics::RADIUS)
        .stroke(egui::Stroke::new(1.0_f32, colors::BORDER))
}

pub(crate) fn overlay_frame() -> egui::Frame {
    egui::Frame::new()
        .fill(egui::Color32::from_black_alpha(205))
        .inner_margin(egui::Margin::symmetric(9, 5))
        .corner_radius(metrics::RADIUS)
        .stroke(egui::Stroke::new(
            1.0_f32,
            egui::Color32::from_white_alpha(24),
        ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn custom_theme_overrides_both_light_and_dark_starting_visuals() {
        let context = egui::Context::default();
        for starting_visuals in [egui::Visuals::light(), egui::Visuals::dark()] {
            context.set_visuals(starting_visuals);
            apply(&context);
            let style = context.style();

            assert!(style.visuals.dark_mode);
            assert_eq!(style.visuals.panel_fill, colors::PANEL);
            assert_eq!(style.visuals.selection.stroke.color, colors::ACCENT_HOVER);
            assert_eq!(
                style.visuals.widgets.inactive.corner_radius.nw,
                metrics::RADIUS_SMALL
            );
            assert!(style.spacing.item_spacing.y < style.spacing.interact_size.y);
        }
    }

    #[test]
    fn library_frame_has_a_consistent_content_inset() {
        let frame = library_frame();
        assert_eq!(
            frame.inner_margin,
            egui::Margin::same(metrics::LIBRARY_CONTENT_PADDING)
        );
    }
}
