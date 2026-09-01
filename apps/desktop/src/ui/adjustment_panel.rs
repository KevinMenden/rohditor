use eframe::egui;
use rohditor_core::{Histogram, ToneCurve, evaluate_tone_curve};

use super::theme::{self, colors, metrics};
use super::widgets::{self, AdjustmentSpec, ValueScale};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AdjustmentTarget {
    WhiteBalanceRed,
    WhiteBalanceGreen,
    WhiteBalanceBlue,
    WhiteBalanceTemperature,
    WhiteBalanceTint,
    Exposure,
    Contrast,
    Highlights,
    Shadows,
    Whites,
    Blacks,
    ToneCurveShadows,
    ToneCurveDarks,
    ToneCurveLights,
    ToneCurveHighlights,
    Saturation,
    Vibrance,
    HslHue(usize),
    HslSaturation(usize),
    HslLuminance(usize),
    GradingShadows(usize),
    GradingMidtones(usize),
    GradingHighlights(usize),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum WhiteBalanceMode {
    AsShot,
    TemperatureTint,
    ManualMultipliers,
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct AdjustmentRange {
    pub minimum: f32,
    pub maximum: f32,
    pub neutral: f32,
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct AdjustmentRanges {
    pub white_balance: AdjustmentRange,
    pub temperature: AdjustmentRange,
    pub tint: AdjustmentRange,
    pub exposure: AdjustmentRange,
    pub contrast: AdjustmentRange,
    pub highlights: AdjustmentRange,
    pub shadows: AdjustmentRange,
    pub whites: AdjustmentRange,
    pub blacks: AdjustmentRange,
    pub tone_curve: AdjustmentRange,
    pub saturation: AdjustmentRange,
    pub vibrance: AdjustmentRange,
    pub hsl_hue: AdjustmentRange,
    pub hsl_saturation: AdjustmentRange,
    pub hsl_luminance: AdjustmentRange,
    pub grading: AdjustmentRange,
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct AdjustmentValues {
    pub white_balance_mode: WhiteBalanceMode,
    pub white_balance_red: f32,
    pub white_balance_green: f32,
    pub white_balance_blue: f32,
    pub white_balance_temperature: f32,
    pub white_balance_tint: f32,
    pub exposure: f32,
    pub contrast: f32,
    pub highlights: f32,
    pub shadows: f32,
    pub whites: f32,
    pub blacks: f32,
    pub tone_curve_shadows: f32,
    pub tone_curve_darks: f32,
    pub tone_curve_lights: f32,
    pub tone_curve_highlights: f32,
    pub saturation: f32,
    pub vibrance: f32,
    pub hsl: [[f32; 3]; 8],
    pub grading: [[f32; 3]; 3],
}

#[derive(Debug, Clone)]
pub(crate) struct DocumentPanelModel {
    pub file_name: String,
    pub camera: Option<String>,
    pub sensor_dimensions: Option<(usize, usize)>,
    pub revision: u64,
    pub has_adjustments: bool,
    pub values: AdjustmentValues,
    pub ranges: AdjustmentRanges,
    pub export_ready: bool,
    pub export_in_progress: bool,
    pub error: Option<String>,
    pub warning: Option<String>,
    pub notice: Option<String>,
    pub histogram: Option<Histogram>,
    pub auto_tone_available: bool,
    pub white_balance_picker_active: bool,
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct AdjustmentInteraction {
    pub target: AdjustmentTarget,
    pub value: f32,
    pub changed: bool,
    pub drag_started: bool,
    pub dragged: bool,
    pub drag_stopped: bool,
    pub reset: bool,
}

#[derive(Debug, Default)]
pub(crate) struct AdjustmentPanelOutput {
    pub white_balance_mode: Option<WhiteBalanceMode>,
    pub white_balance_picker_active: Option<bool>,
    pub interactions: Vec<AdjustmentInteraction>,
    pub auto_tone: bool,
    pub reset_all: bool,
    pub export: bool,
    pub dismiss_error: bool,
    pub dismiss_warning: bool,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) enum ExportKind {
    #[default]
    Jpeg,
    Png,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) enum PngDepth {
    #[default]
    Eight,
    Sixteen,
}

#[derive(Debug, Clone)]
pub(crate) struct ExportUiSettings {
    pub kind: ExportKind,
    pub jpeg_quality: u8,
    pub png_depth: PngDepth,
    pub dither: bool,
    pub safe_metadata: bool,
    pub overwrite: bool,
}

impl ExportUiSettings {
    pub(crate) const fn with_jpeg_quality(jpeg_quality: u8) -> Self {
        Self {
            kind: ExportKind::Jpeg,
            jpeg_quality,
            png_depth: PngDepth::Eight,
            dither: false,
            safe_metadata: true,
            overwrite: false,
        }
    }
}

pub(crate) fn show(
    context: &egui::Context,
    document: Option<DocumentPanelModel>,
    export_settings: &mut ExportUiSettings,
) -> AdjustmentPanelOutput {
    let mut output = AdjustmentPanelOutput::default();
    egui::SidePanel::right("adjustments")
        .default_width(metrics::ADJUSTMENT_PANEL_WIDTH)
        .min_width(280.0)
        .max_width(350.0)
        .resizable(true)
        .frame(theme::side_panel_frame())
        .show(context, |ui| {
            egui::ScrollArea::vertical()
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    ui.heading("Develop");
                    ui.label(
                        egui::RichText::new("NON-DESTRUCTIVE RAW ADJUSTMENTS")
                            .size(10.0)
                            .color(colors::TEXT_MUTED),
                    );
                    ui.add_space(7.0);

                    let Some(mut document) = document else {
                        empty_panel(ui);
                        return;
                    };

                    document_summary(ui, &document);
                    show_messages(ui, &document, &mut output);
                    histogram_panel(ui, document.histogram.as_ref());
                    show_light_controls(ui, &mut document, &mut output);
                    show_tone_curve_controls(ui, &mut document, &mut output);
                    show_color_controls(ui, &mut document, &mut output);
                    show_color_mixer_controls(ui, &mut document, &mut output);
                    show_color_grading_controls(ui, &mut document, &mut output);

                    widgets::section_header(ui, "Export");
                    show_export_settings(ui, export_settings);
                    let export_label = if document.export_in_progress {
                        "Exporting…"
                    } else {
                        "Export image…"
                    };
                    output.export = widgets::primary_button(
                        ui,
                        export_label,
                        document.export_ready && !document.export_in_progress,
                    )
                    .on_hover_text("Export the full-resolution CPU-developed image")
                    .clicked();
                    if !document.export_ready && !document.export_in_progress {
                        ui.label(
                            egui::RichText::new("Available after RAW decoding completes")
                                .small()
                                .color(colors::TEXT_MUTED),
                        );
                    }
                });
        });
    output
}

fn show_tone_curve_controls(
    ui: &mut egui::Ui,
    document: &mut DocumentPanelModel,
    output: &mut AdjustmentPanelOutput,
) {
    widgets::section_header(ui, "Tone curve");
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
        egui::Stroke::new(1.0, colors::BORDER),
        egui::StrokeKind::Inside,
    );
    for fraction in [0.25_f32, 0.5, 0.75] {
        let x = egui::lerp(rect.left()..=rect.right(), fraction);
        let y = egui::lerp(rect.bottom()..=rect.top(), fraction);
        painter.vline(
            x,
            rect.top()..=rect.bottom(),
            egui::Stroke::new(1.0, colors::BORDER.gamma_multiply(0.55)),
        );
        painter.hline(
            rect.left()..=rect.right(),
            y,
            egui::Stroke::new(1.0, colors::BORDER.gamma_multiply(0.55)),
        );
    }
    painter.line_segment(
        [rect.left_bottom(), rect.right_top()],
        egui::Stroke::new(1.0, colors::TEXT_DISABLED),
    );
    let curve_points = (0..=64)
        .map(|index| {
            let input = index as f32 / 64.0;
            curve_graph_position(rect, input, tone_curve_graph_value(input, values))
        })
        .collect::<Vec<_>>();
    painter.add(egui::Shape::line(
        curve_points,
        egui::Stroke::new(2.0, colors::ACCENT_HOVER),
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
        painter.circle_stroke(point, 6.0, egui::Stroke::new(1.0, colors::FIELD));
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

fn empty_panel(ui: &mut egui::Ui) {
    theme::card_frame().show(ui, |ui| {
        ui.label(egui::RichText::new("No photo open").strong());
        ui.label(
            egui::RichText::new("Open a Sony ARW file to reveal the develop controls.")
                .color(colors::TEXT_MUTED),
        );
    });
    widgets::section_header(ui, "Histogram");
    histogram_canvas(ui, "WAITING FOR A PHOTO");
}

fn document_summary(ui: &mut egui::Ui, document: &DocumentPanelModel) {
    ui.add(
        egui::Label::new(egui::RichText::new(&document.file_name).strong().size(14.0)).truncate(),
    );
    if let Some(camera) = &document.camera {
        ui.label(
            egui::RichText::new(camera)
                .small()
                .color(colors::TEXT_MUTED),
        );
    }
    if let Some((width, height)) = document.sensor_dimensions {
        ui.label(
            egui::RichText::new(format!("{width} × {height}  ·  REV {}", document.revision))
                .monospace()
                .small()
                .color(colors::TEXT_MUTED),
        );
    }
}

fn histogram_panel(ui: &mut egui::Ui, histogram: Option<&Histogram>) {
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

fn histogram_canvas(ui: &mut egui::Ui, caption: &str) {
    let (rect, _) =
        ui.allocate_exact_size(egui::vec2(ui.available_width(), 74.0), egui::Sense::hover());
    ui.painter().rect(
        rect,
        metrics::RADIUS,
        colors::FIELD,
        egui::Stroke::new(1.0, colors::BORDER),
        egui::StrokeKind::Inside,
    );
    for fraction in [0.25_f32, 0.5, 0.75] {
        let x = egui::lerp(rect.left()..=rect.right(), fraction);
        ui.painter().vline(
            x,
            rect.top() + 1.0..=rect.bottom() - 1.0,
            egui::Stroke::new(1.0, colors::BORDER.gamma_multiply(0.45)),
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
        egui::Stroke::new(1.0, colors::BORDER),
        egui::StrokeKind::Inside,
    );
    for fraction in [0.25_f32, 0.5, 0.75] {
        let x = egui::lerp(rect.left()..=rect.right(), fraction);
        ui.painter().vline(
            x,
            rect.top() + 1.0..=rect.bottom() - 1.0,
            egui::Stroke::new(1.0, colors::BORDER.gamma_multiply(0.45)),
        );
    }
    let maximum = histogram
        .red
        .iter()
        .chain(histogram.green.iter())
        .chain(histogram.blue.iter())
        .chain(histogram.luminance.iter())
        .copied()
        .max()
        .unwrap_or(0)
        .max(1) as f32;
    let width = rect.width() / 256.0;
    for index in 0..256 {
        let x0 = rect.left() + index as f32 * width;
        let x1 = x0 + width.max(1.0);
        let luminance = histogram.luminance[index] as f32 / maximum;
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
                let y = egui::lerp(rect.bottom()..=rect.top(), *count as f32 / maximum);
                egui::pos2(x, y)
            })
            .collect::<Vec<_>>();
        ui.painter().add(egui::Shape::line(
            points,
            egui::Stroke::new(1.0, color.gamma_multiply(0.8)),
        ));
    }
}

fn show_light_controls(
    ui: &mut egui::Ui,
    document: &mut DocumentPanelModel,
    output: &mut AdjustmentPanelOutput,
) {
    widgets::section_header(ui, "Light");
    let auto_tone_response = ui
        .add_enabled_ui(document.auto_tone_available, |ui| {
            ui.small_button("Auto tone")
        })
        .inner
        .on_hover_text(if document.auto_tone_available {
            "Display-referred heuristic: set exposure and clipping guards from the current histogram"
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

fn show_color_controls(
    ui: &mut egui::Ui,
    document: &mut DocumentPanelModel,
    output: &mut AdjustmentPanelOutput,
) {
    widgets::section_header(ui, "Color");
    let mut mode = document.values.white_balance_mode;
    widgets::dropdown(
        ui,
        "white_balance_mode",
        "White balance",
        match mode {
            WhiteBalanceMode::AsShot => "As shot",
            WhiteBalanceMode::TemperatureTint => "Temperature / tint",
            WhiteBalanceMode::ManualMultipliers => "Manual multipliers",
        },
        |ui| {
            ui.selectable_value(&mut mode, WhiteBalanceMode::AsShot, "As shot");
            ui.selectable_value(
                &mut mode,
                WhiteBalanceMode::TemperatureTint,
                "Temperature / tint",
            );
            ui.selectable_value(
                &mut mode,
                WhiteBalanceMode::ManualMultipliers,
                "Manual multipliers",
            );
        },
    );
    if mode != document.values.white_balance_mode {
        output.white_balance_mode = Some(mode);
        document.values.white_balance_mode = mode;
    }
    let white_balance_hint = match mode {
        WhiteBalanceMode::AsShot => "Using the camera's as-shot white balance",
        WhiteBalanceMode::TemperatureTint => "Temperature is in Kelvin; tint shifts green/magenta",
        WhiteBalanceMode::ManualMultipliers => {
            "Manual values are relative to the camera multipliers"
        }
    };
    ui.label(
        egui::RichText::new(white_balance_hint)
            .small()
            .color(colors::TEXT_MUTED),
    );
    let picker_label = if document.white_balance_picker_active {
        "Cancel picker"
    } else {
        "Pick neutral"
    };
    if ui
        .small_button(picker_label)
        .on_hover_text("Click a neutral or gray area in the image")
        .clicked()
    {
        output.white_balance_picker_active = Some(!document.white_balance_picker_active);
    }
    if document.white_balance_picker_active {
        ui.label(
            egui::RichText::new("Click a neutral area in the image")
                .small()
                .color(colors::ACCENT_HOVER),
        );
    }
    ui.add_space(4.0);

    if document.values.white_balance_mode == WhiteBalanceMode::TemperatureTint {
        record_slider(
            ui,
            &mut output.interactions,
            AdjustmentTarget::WhiteBalanceTemperature,
            &mut document.values.white_balance_temperature,
            AdjustmentSpec {
                label: "Temperature",
                minimum: document.ranges.temperature.minimum,
                maximum: document.ranges.temperature.maximum,
                neutral: document.ranges.temperature.neutral,
                decimals: 0,
                step: 10.0,
                suffix: " K",
                scale: ValueScale::Raw,
            },
        );
        record_slider(
            ui,
            &mut output.interactions,
            AdjustmentTarget::WhiteBalanceTint,
            &mut document.values.white_balance_tint,
            AdjustmentSpec {
                label: "Tint",
                minimum: document.ranges.tint.minimum,
                maximum: document.ranges.tint.maximum,
                neutral: document.ranges.tint.neutral,
                decimals: 0,
                step: 0.01,
                suffix: "%",
                scale: ValueScale::OffsetPercent,
            },
        );
    } else if document.values.white_balance_mode == WhiteBalanceMode::ManualMultipliers {
        for (label, target, value) in [
            (
                "Red multiplier",
                AdjustmentTarget::WhiteBalanceRed,
                &mut document.values.white_balance_red,
            ),
            (
                "Green multiplier",
                AdjustmentTarget::WhiteBalanceGreen,
                &mut document.values.white_balance_green,
            ),
            (
                "Blue multiplier",
                AdjustmentTarget::WhiteBalanceBlue,
                &mut document.values.white_balance_blue,
            ),
        ] {
            record_slider(
                ui,
                &mut output.interactions,
                target,
                value,
                AdjustmentSpec {
                    label,
                    minimum: document.ranges.white_balance.minimum,
                    maximum: document.ranges.white_balance.maximum,
                    neutral: document.ranges.white_balance.neutral,
                    decimals: 2,
                    step: 0.01,
                    suffix: "×",
                    scale: ValueScale::RawUnsigned,
                },
            );
        }
    }

    record_slider(
        ui,
        &mut output.interactions,
        AdjustmentTarget::Saturation,
        &mut document.values.saturation,
        AdjustmentSpec {
            label: "Saturation",
            minimum: document.ranges.saturation.minimum,
            maximum: document.ranges.saturation.maximum,
            neutral: document.ranges.saturation.neutral,
            decimals: 0,
            step: 0.01,
            suffix: "%",
            scale: ValueScale::OffsetPercent,
        },
    );
    record_slider(
        ui,
        &mut output.interactions,
        AdjustmentTarget::Vibrance,
        &mut document.values.vibrance,
        AdjustmentSpec {
            label: "Vibrance",
            minimum: document.ranges.vibrance.minimum,
            maximum: document.ranges.vibrance.maximum,
            neutral: document.ranges.vibrance.neutral,
            decimals: 0,
            step: 0.01,
            suffix: "%",
            scale: ValueScale::OffsetPercent,
        },
    );

    output.reset_all = ui
        .add_enabled(
            document.has_adjustments,
            egui::Button::new("Reset all adjustments").frame(false),
        )
        .on_hover_text("Restore the neutral recipe")
        .clicked();
}

fn show_color_mixer_controls(
    ui: &mut egui::Ui,
    document: &mut DocumentPanelModel,
    output: &mut AdjustmentPanelOutput,
) {
    egui::CollapsingHeader::new("Color mixer")
        .default_open(false)
        .show(ui, |ui| {
            for (index, label) in [
                "Red", "Orange", "Yellow", "Green", "Aqua", "Blue", "Purple", "Magenta",
            ]
            .into_iter()
            .enumerate()
            {
                ui.label(egui::RichText::new(label).small().color(colors::TEXT_MUTED));
                record_slider(
                    ui,
                    &mut output.interactions,
                    AdjustmentTarget::HslHue(index),
                    &mut document.values.hsl[index][0],
                    AdjustmentSpec {
                        label: "Hue",
                        minimum: document.ranges.hsl_hue.minimum,
                        maximum: document.ranges.hsl_hue.maximum,
                        neutral: document.ranges.hsl_hue.neutral,
                        decimals: 0,
                        step: 0.01,
                        suffix: "%",
                        scale: ValueScale::OffsetPercent,
                    },
                );
                record_slider(
                    ui,
                    &mut output.interactions,
                    AdjustmentTarget::HslSaturation(index),
                    &mut document.values.hsl[index][1],
                    AdjustmentSpec {
                        label: "Saturation",
                        minimum: document.ranges.hsl_saturation.minimum,
                        maximum: document.ranges.hsl_saturation.maximum,
                        neutral: document.ranges.hsl_saturation.neutral,
                        decimals: 0,
                        step: 0.01,
                        suffix: "%",
                        scale: ValueScale::OffsetPercent,
                    },
                );
                record_slider(
                    ui,
                    &mut output.interactions,
                    AdjustmentTarget::HslLuminance(index),
                    &mut document.values.hsl[index][2],
                    AdjustmentSpec {
                        label: "Luminance",
                        minimum: document.ranges.hsl_luminance.minimum,
                        maximum: document.ranges.hsl_luminance.maximum,
                        neutral: document.ranges.hsl_luminance.neutral,
                        decimals: 0,
                        step: 0.01,
                        suffix: "%",
                        scale: ValueScale::OffsetPercent,
                    },
                );
            }
        });
}

fn show_color_grading_controls(
    ui: &mut egui::Ui,
    document: &mut DocumentPanelModel,
    output: &mut AdjustmentPanelOutput,
) {
    egui::CollapsingHeader::new("Three-way RGB tint")
        .default_open(false)
        .show(ui, |ui| {
            ui.label(
                egui::RichText::new("Luminance-preserving tint; values are not lift/gamma/gain")
                    .small()
                    .color(colors::TEXT_MUTED),
            );
            for (group, label, target) in [
                (0, "Shadows", AdjustmentTarget::GradingShadows(0)),
                (1, "Midtones", AdjustmentTarget::GradingMidtones(0)),
                (2, "Highlights", AdjustmentTarget::GradingHighlights(0)),
            ] {
                ui.label(egui::RichText::new(label).small().color(colors::TEXT_MUTED));
                for channel in 0..3 {
                    let target = match target {
                        AdjustmentTarget::GradingShadows(_) => {
                            AdjustmentTarget::GradingShadows(channel)
                        }
                        AdjustmentTarget::GradingMidtones(_) => {
                            AdjustmentTarget::GradingMidtones(channel)
                        }
                        AdjustmentTarget::GradingHighlights(_) => {
                            AdjustmentTarget::GradingHighlights(channel)
                        }
                        _ => unreachable!("color grading group is fixed"),
                    };
                    let label = ["Red", "Green", "Blue"][channel];
                    record_slider(
                        ui,
                        &mut output.interactions,
                        target,
                        &mut document.values.grading[group][channel],
                        AdjustmentSpec {
                            label,
                            minimum: document.ranges.grading.minimum,
                            maximum: document.ranges.grading.maximum,
                            neutral: document.ranges.grading.neutral,
                            decimals: 0,
                            step: 0.01,
                            suffix: "%",
                            scale: ValueScale::OffsetPercent,
                        },
                    );
                }
            }
        });
}

fn record_slider(
    ui: &mut egui::Ui,
    interactions: &mut Vec<AdjustmentInteraction>,
    target: AdjustmentTarget,
    value: &mut f32,
    spec: AdjustmentSpec<'_>,
) {
    let neutral = spec.neutral;
    let response = widgets::adjustment_slider(ui, value, spec);
    if response.reset_clicked {
        *value = neutral;
    }
    if response.response.changed()
        || response.response.drag_started()
        || response.response.drag_stopped()
        || response.reset_clicked
    {
        interactions.push(AdjustmentInteraction {
            target,
            value: *value,
            changed: response.response.changed(),
            drag_started: response.response.drag_started(),
            dragged: response.response.dragged(),
            drag_stopped: response.response.drag_stopped(),
            reset: response.reset_clicked,
        });
    }
}

fn show_export_settings(ui: &mut egui::Ui, settings: &mut ExportUiSettings) {
    let mut kind = settings.kind;
    widgets::dropdown(
        ui,
        "export_format",
        "Format",
        match kind {
            ExportKind::Jpeg => "JPEG",
            ExportKind::Png => "PNG",
        },
        |ui| {
            ui.selectable_value(&mut kind, ExportKind::Jpeg, "JPEG");
            ui.selectable_value(&mut kind, ExportKind::Png, "PNG");
        },
    );
    settings.kind = kind;

    match settings.kind {
        ExportKind::Jpeg => {
            ui.label(
                egui::RichText::new("JPEG quality")
                    .small()
                    .color(colors::TEXT_MUTED),
            );
            let available = ui.available_width();
            ui.scope(|ui| {
                ui.spacing_mut().slider_width = available;
                ui.add(
                    egui::Slider::new(&mut settings.jpeg_quality, 1..=100)
                        .show_value(true)
                        .trailing_fill(true),
                );
            });
        }
        ExportKind::Png => {
            let mut depth = settings.png_depth;
            widgets::dropdown(
                ui,
                "png_depth",
                "Bit depth",
                match depth {
                    PngDepth::Eight => "8-bit",
                    PngDepth::Sixteen => "16-bit",
                },
                |ui| {
                    ui.selectable_value(&mut depth, PngDepth::Eight, "8-bit");
                    ui.selectable_value(&mut depth, PngDepth::Sixteen, "16-bit");
                },
            );
            settings.png_depth = depth;
        }
    }
    ui.checkbox(&mut settings.dither, "Ordered output dithering");
    ui.checkbox(&mut settings.safe_metadata, "Include safe EXIF metadata");
    ui.checkbox(&mut settings.overwrite, "Allow replacing an existing file");
    ui.add_space(4.0);
}

fn show_messages(
    ui: &mut egui::Ui,
    document: &DocumentPanelModel,
    output: &mut AdjustmentPanelOutput,
) {
    if document.error.is_some() || document.warning.is_some() || document.notice.is_some() {
        widgets::section_header(ui, "Messages");
    }
    if let Some(message) = &document.error {
        output.dismiss_error =
            widgets::message_card(ui, message, colors::ERROR, Some("Dismiss error"));
    }
    if let Some(message) = &document.warning {
        output.dismiss_warning =
            widgets::message_card(ui, message, colors::WARNING, Some("Dismiss warning"));
    }
    if let Some(message) = &document.notice {
        let _ = widgets::message_card(ui, message, colors::SUCCESS, None);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn export_ui_defaults_are_stable_and_non_destructive() {
        let settings = ExportUiSettings::with_jpeg_quality(87);
        assert_eq!(settings.kind, ExportKind::Jpeg);
        assert_eq!(settings.jpeg_quality, 87);
        assert_eq!(settings.png_depth, PngDepth::Eight);
        assert!(settings.safe_metadata);
        assert!(!settings.overwrite);
    }

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
