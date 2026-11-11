use eframe::egui;
use rohditor_core::evaluate_tone_curve;
use rohditor_edit::{HighlightMethod, ToneCurve};

use super::{
    AdjustmentInteraction, AdjustmentPanelOutput, AdjustmentTarget, DocumentPanelModel,
    record_slider,
};
use crate::ui::theme::{colors, metrics};
use crate::ui::widgets::{self, AdjustmentSpec, ValueScale};

pub(super) fn show_highlight_controls(
    ui: &mut egui::Ui,
    document: &mut DocumentPanelModel,
    output: &mut AdjustmentPanelOutput,
) {
    let mut highlight_method = document.values.highlight_method;
    widgets::dropdown(
        ui,
        "highlight_reconstruction_method",
        "Method",
        match highlight_method {
            HighlightMethod::Off => "Off",
            HighlightMethod::Clip => "Clip",
            HighlightMethod::LocalRatios => "Local ratios",
            HighlightMethod::Opposed => "Opposed / local inpainting",
        },
        |ui| {
            ui.selectable_value(&mut highlight_method, HighlightMethod::Off, "Off");
            ui.selectable_value(&mut highlight_method, HighlightMethod::Clip, "Clip");
            ui.selectable_value(
                &mut highlight_method,
                HighlightMethod::LocalRatios,
                "Local ratios",
            );
            ui.selectable_value(
                &mut highlight_method,
                HighlightMethod::Opposed,
                "Opposed / local inpainting",
            );
        },
    );
    if highlight_method != document.values.highlight_method {
        document.values.highlight_method = highlight_method;
        output.highlight_method = Some(highlight_method);
    }
    if !matches!(highlight_method, HighlightMethod::Off) {
        let (label, tooltip, suffix) = match highlight_method {
            HighlightMethod::Clip => (
                "Effective threshold",
                "Clip caps channel values using a white-balance-dependent treatment ceiling.",
                "× white",
            ),
            HighlightMethod::LocalRatios => (
                "Detection threshold",
                "Lower values classify more RAW samples as potentially clipped; too low can reconstruct legitimate over-range color.",
                "× normalized white",
            ),
            HighlightMethod::Opposed => (
                "Detection threshold",
                "Uses nearby opposing-channel color evidence to inpaint clipped RAW sites; unsupported sites remain unchanged.",
                "× normalized white",
            ),
            HighlightMethod::Off => unreachable!("Off is excluded above"),
        };
        let threshold_response = record_slider(
            ui,
            &mut output.interactions,
            AdjustmentTarget::HighlightThreshold,
            &mut document.values.highlight_threshold,
            AdjustmentSpec {
                label,
                minimum: document.ranges.highlight_threshold.minimum,
                maximum: document.ranges.highlight_threshold.maximum,
                neutral: document.ranges.highlight_threshold.neutral,
                decimals: 2,
                step: 0.01,
                suffix,
                scale: ValueScale::Raw,
            },
        );
        let _ = threshold_response.on_hover_text(tooltip);
    }
}

pub(super) fn show_tone_controls(
    ui: &mut egui::Ui,
    document: &mut DocumentPanelModel,
    output: &mut AdjustmentPanelOutput,
) {
    let auto_tone_response = widgets::full_width_primary_button(
        ui,
        "Auto tone",
        document.auto_tone_available,
    )
    .on_hover_text(if document.auto_tone_available {
        "Refine exposure, shadows, highlights and blacks from the current histogram, keeping the photo's overall brightness character"
    } else {
        "Auto tone becomes available when the current preview histogram is ready"
    });
    output.auto_tone = auto_tone_response.clicked();
    record_slider(
        ui,
        &mut output.interactions,
        AdjustmentTarget::Exposure,
        &mut document.values.exposure,
        AdjustmentSpec {
            label: "Exposure",
            minimum: document.ranges.exposure.minimum,
            maximum: document.ranges.exposure.maximum,
            neutral: document.ranges.exposure.neutral,
            decimals: 2,
            step: 0.01,
            suffix: " EV",
            scale: ValueScale::Raw,
        },
    );
    record_slider(
        ui,
        &mut output.interactions,
        AdjustmentTarget::Contrast,
        &mut document.values.contrast,
        AdjustmentSpec {
            label: "Contrast",
            minimum: document.ranges.contrast.minimum,
            maximum: document.ranges.contrast.maximum,
            neutral: document.ranges.contrast.neutral,
            decimals: 0,
            step: 0.01,
            suffix: "%",
            scale: ValueScale::OffsetPercent,
        },
    );
    for (label, target, value, range) in [
        (
            "Highlights",
            AdjustmentTarget::Highlights,
            &mut document.values.highlights,
            document.ranges.highlights,
        ),
        (
            "Shadows",
            AdjustmentTarget::Shadows,
            &mut document.values.shadows,
            document.ranges.shadows,
        ),
        (
            "Whites",
            AdjustmentTarget::Whites,
            &mut document.values.whites,
            document.ranges.whites,
        ),
        (
            "Blacks",
            AdjustmentTarget::Blacks,
            &mut document.values.blacks,
            document.ranges.blacks,
        ),
    ] {
        record_slider(
            ui,
            &mut output.interactions,
            target,
            value,
            AdjustmentSpec {
                label,
                minimum: range.minimum,
                maximum: range.maximum,
                neutral: range.neutral,
                decimals: 0,
                step: 0.01,
                suffix: "%",
                scale: ValueScale::OffsetPercent,
            },
        );
    }
}

pub(super) fn show_tone_curve_controls(
    ui: &mut egui::Ui,
    document: &mut DocumentPanelModel,
    output: &mut AdjustmentPanelOutput,
) {
    let mut values = [
        document.values.tone_curve_shadows,
        document.values.tone_curve_darks,
        document.values.tone_curve_lights,
        document.values.tone_curve_highlights,
    ];
    let neutral = document.ranges.tone_curve.neutral;
    let minimum = document.ranges.tone_curve.minimum;
    let maximum = document.ranges.tone_curve.maximum;
    let graph_size = egui::vec2(ui.available_width(), 156.0);
    let (outer_rect, _) = ui.allocate_exact_size(graph_size, egui::Sense::hover());
    let rect = outer_rect.shrink(1.0);
    let painter = ui.painter();
    painter.rect(
        rect,
        metrics::RADIUS,
        colors::FIELD,
        egui::Stroke::new(1.0_f32, colors::BORDER),
        egui::StrokeKind::Inside,
    );
    for fraction in [0.25_f32, 0.5, 0.75] {
        let x = egui::lerp(rect.left()..=rect.right(), fraction);
        let y = egui::lerp(rect.bottom()..=rect.top(), fraction);
        painter.vline(
            x,
            rect.top()..=rect.bottom(),
            egui::Stroke::new(1.0_f32, colors::BORDER.gamma_multiply(0.55)),
        );
        painter.hline(
            rect.left()..=rect.right(),
            y,
            egui::Stroke::new(1.0_f32, colors::BORDER.gamma_multiply(0.55)),
        );
    }
    painter.line_segment(
        [rect.left_bottom(), rect.right_top()],
        egui::Stroke::new(1.0_f32, colors::TEXT_DISABLED),
    );
    let curve_points = (0..=64)
        .map(|index| {
            let input = index as f32 / 64.0;
            curve_graph_position(rect, input, tone_curve_graph_value(input, values))
        })
        .collect::<Vec<_>>();
    painter.add(egui::Shape::line(
        curve_points,
        egui::Stroke::new(2.0_f32, colors::ACCENT_HOVER),
    ));

    const CONTROL_INPUTS: [f32; 4] = [0.12, 0.35, 0.65, 0.88];
    let targets = [
        AdjustmentTarget::ToneCurveShadows,
        AdjustmentTarget::ToneCurveDarks,
        AdjustmentTarget::ToneCurveLights,
        AdjustmentTarget::ToneCurveHighlights,
    ];
    for index in 0..CONTROL_INPUTS.len() {
        let input = CONTROL_INPUTS[index];
        let point = curve_graph_position(rect, input, tone_curve_graph_value(input, values));
        let handle_rect = egui::Rect::from_center_size(point, egui::vec2(18.0, 18.0));
        let response = ui.interact(
            handle_rect,
            ui.make_persistent_id(("tone_curve_point", index)),
            egui::Sense::drag(),
        );
        let was = values[index];
        let reset = response.double_clicked();
        if reset {
            values[index] = neutral;
        } else if response.dragged()
            && let Some(pointer) = response.interact_pointer_pos()
        {
            let output_value = ((rect.bottom() - pointer.y) / rect.height()).clamp(0.0, 1.0);
            values[index] = (output_value - input).clamp(minimum, maximum);
        }
        let value_changed = (values[index] - was).abs() > f32::EPSILON;
        if response.drag_started() || value_changed || response.drag_stopped() || reset {
            output.interactions.push(AdjustmentInteraction {
                target: targets[index],
                value: values[index],
                changed: value_changed,
                drag_started: response.drag_started(),
                dragged: response.dragged(),
                drag_stopped: response.drag_stopped(),
                reset,
            });
        }
        let visuals = ui.style().interact(&response);
        painter.circle_filled(point, 5.0, visuals.fg_stroke.color);
        painter.circle_stroke(point, 6.0, egui::Stroke::new(1.0_f32, colors::FIELD));
    }
    ui.label(
        egui::RichText::new("Drag points · double-click a point to reset")
            .small()
            .color(colors::TEXT_MUTED),
    );
    ui.label(
        egui::RichText::new("Shadows · Darks · Lights · Highlights")
            .small()
            .color(colors::TEXT_MUTED),
    );
    document.values.tone_curve_shadows = values[0];
    document.values.tone_curve_darks = values[1];
    document.values.tone_curve_lights = values[2];
    document.values.tone_curve_highlights = values[3];
}

fn tone_curve_graph_value(input: f32, values: [f32; 4]) -> f32 {
    evaluate_tone_curve(
        &ToneCurve {
            shadows: values[0],
            darks: values[1],
            lights: values[2],
            highlights: values[3],
        },
        input,
    )
}

fn curve_graph_position(rect: egui::Rect, input: f32, output: f32) -> egui::Pos2 {
    egui::pos2(
        egui::lerp(rect.left()..=rect.right(), input),
        egui::lerp(rect.bottom()..=rect.top(), output),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tone_curve_graph_is_identity_at_neutral() {
        for input in [0.0, 0.12, 0.5, 0.88, 1.0] {
            assert!((tone_curve_graph_value(input, [0.0; 4]) - input).abs() < 1.0e-6);
        }
    }

    #[test]
    fn tone_curve_graph_moves_the_selected_tonal_region() {
        let lifted = tone_curve_graph_value(0.15, [0.1, 0.0, 0.0, 0.0]);
        let lowered = tone_curve_graph_value(0.85, [0.0, 0.0, 0.0, -0.1]);
        assert!(lifted > 0.15);
        assert!(lowered < 0.85);
    }
}
