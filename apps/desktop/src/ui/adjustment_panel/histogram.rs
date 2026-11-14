use eframe::egui;
use rohditor_core::Histogram;

use crate::ui::theme::{colors, metrics};
use crate::ui::widgets;

/// Histogram header shared by every tool; `None` means analysis is pending.
pub(crate) fn histogram_panel(ui: &mut egui::Ui, histogram: Option<&Histogram>) {
    widgets::section_header(ui, "Histogram");
    let Some(histogram) = histogram else {
        histogram_canvas(ui, "ANALYZING");
        return;
    };
    histogram_canvas_data(ui, histogram);
    ui.horizontal(|ui| {
        let shadow_count = histogram.shadow_clipped.into_iter().sum::<u64>();
        let highlight_count = histogram.highlight_clipped.into_iter().sum::<u64>();
        ui.label(
            egui::RichText::new(format!("Shadows {shadow_count}"))
                .small()
                .color(colors::TEXT_MUTED),
        );
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            ui.label(
                egui::RichText::new(format!("Highlights {highlight_count}"))
                    .small()
                    .color(colors::TEXT_MUTED),
            );
        });
    });
}

pub(crate) fn histogram_canvas(ui: &mut egui::Ui, caption: &str) {
    let (rect, _) =
        ui.allocate_exact_size(egui::vec2(ui.available_width(), 74.0), egui::Sense::hover());
    ui.painter().rect(
        rect,
        metrics::RADIUS,
        colors::FIELD,
        egui::Stroke::new(1.0_f32, colors::BORDER),
        egui::StrokeKind::Inside,
    );
    for fraction in [0.25_f32, 0.5, 0.75] {
        let x = egui::lerp(rect.left()..=rect.right(), fraction);
        ui.painter().vline(
            x,
            rect.top() + 1.0..=rect.bottom() - 1.0,
            egui::Stroke::new(1.0_f32, colors::BORDER.gamma_multiply(0.45)),
        );
    }
    ui.painter().text(
        rect.center(),
        egui::Align2::CENTER_CENTER,
        caption,
        egui::FontId::monospace(9.5),
        colors::TEXT_DISABLED,
    );
}

fn histogram_canvas_data(ui: &mut egui::Ui, histogram: &Histogram) {
    let (rect, _) =
        ui.allocate_exact_size(egui::vec2(ui.available_width(), 74.0), egui::Sense::hover());
    ui.painter().rect(
        rect,
        metrics::RADIUS,
        colors::FIELD,
        egui::Stroke::new(1.0_f32, colors::BORDER),
        egui::StrokeKind::Inside,
    );
    for fraction in [0.25_f32, 0.5, 0.75] {
        let x = egui::lerp(rect.left()..=rect.right(), fraction);
        ui.painter().vline(
            x,
            rect.top() + 1.0..=rect.bottom() - 1.0,
            egui::Stroke::new(1.0_f32, colors::BORDER.gamma_multiply(0.45)),
        );
    }
    let vertical_scale = histogram_vertical_scale(histogram);
    let width = rect.width() / 256.0;
    for index in 0..256 {
        let x0 = rect.left() + index as f32 * width;
        let x1 = x0 + width.max(1.0);
        let luminance = histogram_bin_height(histogram.luminance[index], vertical_scale);
        if luminance > 0.0 {
            let y = egui::lerp(rect.bottom()..=rect.top(), luminance);
            ui.painter().rect_filled(
                egui::Rect::from_min_max(egui::pos2(x0, y), egui::pos2(x1, rect.bottom())),
                0.0,
                colors::TEXT.gamma_multiply(0.16),
            );
        }
    }
    for (bins, color) in [
        (&histogram.red, colors::ACCENT),
        (&histogram.green, egui::Color32::from_rgb(105, 190, 125)),
        (&histogram.blue, egui::Color32::from_rgb(105, 145, 220)),
    ] {
        let points = bins
            .iter()
            .enumerate()
            .map(|(index, count)| {
                let x = rect.left() + (index as f32 + 0.5) * width;
                let height = histogram_bin_height(*count, vertical_scale);
                let y = egui::lerp(rect.bottom()..=rect.top(), height);
                egui::pos2(x, y)
            })
            .collect::<Vec<_>>();
        ui.painter().add(egui::Shape::line(
            points,
            egui::Stroke::new(1.0_f32, color.gamma_multiply(0.8)),
        ));
    }
}

/// Use a robust interior-bin scale. Endpoint bins are clipping indicators and
/// can be orders of magnitude larger than the useful distribution, so they do
/// not participate in the graph's vertical normalization.
fn histogram_vertical_scale(histogram: &Histogram) -> f32 {
    let mut interior_counts = Vec::with_capacity(4 * 254);
    for bins in [
        &histogram.red,
        &histogram.green,
        &histogram.blue,
        &histogram.luminance,
    ] {
        interior_counts.extend(bins[1..255].iter().copied().filter(|count| *count > 0));
    }
    if interior_counts.is_empty() {
        return 1.0;
    }
    interior_counts.sort_unstable();
    let percentile_99 = (interior_counts.len() - 1) * 99 / 100;
    interior_counts[percentile_99].max(1) as f32
}

fn histogram_bin_height(count: u64, vertical_scale: f32) -> f32 {
    if count == 0 {
        return 0.0;
    }
    ((count as f32).ln_1p() / vertical_scale.max(1.0).ln_1p()).clamp(0.0, 1.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn histogram_scale_ignores_dominant_clipping_spikes() {
        let mut histogram = Histogram::default();
        for bins in [
            &mut histogram.red,
            &mut histogram.green,
            &mut histogram.blue,
            &mut histogram.luminance,
        ] {
            bins[0] = 1_000_000;
            bins[64] = 10;
            bins[128] = 20;
            bins[255] = 2_000_000;
        }

        let scale = histogram_vertical_scale(&histogram);
        assert_eq!(scale, 20.0);
        assert!(histogram_bin_height(10, scale) > 0.5);
        assert_eq!(histogram_bin_height(1_000_000, scale), 1.0);
    }
}
