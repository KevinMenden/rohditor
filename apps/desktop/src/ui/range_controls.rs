//! Controls of the range components in the Masks panel: the luminance bar and the colour
//! samples.
//!
//! Presentation only, like the rest of the panel. Edits are reported as
//! [`MasksAction::RangeSlider`] with the engine's [`RangeSlider`] ids, so the values are
//! validated and combined by the engine, not here.

use eframe::egui;
use rohditor_core::{linear_srgb_to_srgb, oklab_to_linear_srgb};
use rohditor_edit::{COLOR_TOLERANCE_RANGE, ColorRange, ColorSample, LuminanceRange};
use rohditor_engine::tools::RangeSlider;

use super::masks::{MasksAction, SliderPhase};
use super::theme::colors;
use super::widgets::{self, AdjustmentSpec, ValueScale};

const BAR_HEIGHT: f32 = 34.0;
const HANDLE_RADIUS: f32 = 5.0;
/// Width of the swatch of one colour sample.
const SWATCH_SIZE: f32 = 20.0;

/// Where the component lives, so an edit can address it.
#[derive(Clone, Copy)]
pub(crate) struct ComponentRef {
    pub layer: u32,
    pub component: usize,
}

impl ComponentRef {
    fn action(self, slider: RangeSlider, value: f32, phase: SliderPhase) -> MasksAction {
        MasksAction::RangeSlider {
            layer: self.layer,
            component: self.component,
            slider,
            value,
            phase,
        }
    }
}

/// Phase of a drag on `response`, for making a whole drag one undo step.
fn drag_phase(response: &egui::Response) -> SliderPhase {
    SliderPhase {
        changed: response.dragged(),
        drag_started: response.drag_started(),
        dragged: response.dragged(),
        drag_stopped: response.drag_stopped(),
        reset: false,
    }
}

/// The luminance range as a black-to-white bar with the selected band over it. The two round
/// handles are the ends of the range; the small ones below are where the feathers reach zero.
pub(crate) fn show_luminance(
    ui: &mut egui::Ui,
    at: ComponentRef,
    range: &LuminanceRange,
    actions: &mut Vec<MasksAction>,
) {
    let width = ui.available_width().max(120.0);
    let (rect, _) = ui.allocate_exact_size(egui::vec2(width, BAR_HEIGHT), egui::Sense::hover());
    // Leave room at both ends so a handle at 0 or 1 is fully visible and grabbable.
    let track = rect.shrink2(egui::vec2(HANDLE_RADIUS + 1.0, 0.0));
    let x_of = |value: f32| track.left() + value.clamp(0.0, 1.0) * track.width();
    let value_of = |x: f32| ((x - track.left()) / track.width()).clamp(0.0, 1.0);

    let painter = ui.painter();
    let bar = egui::Rect::from_min_max(track.min, egui::pos2(track.right(), track.center().y));
    paint_gradient(painter, bar);
    // The selected band: full strength between the ends, falling to nothing over the feathers.
    let (low, high) = (range.low, range.high);
    let outer_low = (low - range.feather_low).max(0.0);
    let outer_high = (high + range.feather_high).min(1.0);
    painter.add(egui::Shape::convex_polygon(
        vec![
            egui::pos2(x_of(outer_low), bar.bottom()),
            egui::pos2(x_of(low), bar.top()),
            egui::pos2(x_of(high), bar.top()),
            egui::pos2(x_of(outer_high), bar.bottom()),
        ],
        colors::ACCENT.gamma_multiply(0.45),
        egui::Stroke::new(1.0_f32, colors::ACCENT),
    ));

    let handles = [
        (RangeSlider::LuminanceLow, "low", low, bar.top()),
        (RangeSlider::LuminanceHigh, "high", high, bar.top()),
        (
            RangeSlider::LuminanceFeatherLow,
            "feather_low",
            outer_low,
            bar.bottom() + 8.0,
        ),
        (
            RangeSlider::LuminanceFeatherHigh,
            "feather_high",
            outer_high,
            bar.bottom() + 8.0,
        ),
    ];
    for (slider, name, value, y) in handles {
        let center = egui::pos2(x_of(value), y);
        let hit = egui::Rect::from_center_size(center, egui::Vec2::splat(HANDLE_RADIUS * 3.0));
        let response = ui
            .interact(
                hit,
                ui.id().with(("luminance_handle", at.layer, name)),
                egui::Sense::drag(),
            )
            .on_hover_cursor(egui::CursorIcon::ResizeHorizontal);
        let radius = if matches!(
            slider,
            RangeSlider::LuminanceLow | RangeSlider::LuminanceHigh
        ) {
            HANDLE_RADIUS
        } else {
            HANDLE_RADIUS - 1.5
        };
        let fill = if response.dragged() || response.hovered() {
            colors::ACCENT_HOVER
        } else {
            colors::ACCENT
        };
        ui.painter().circle(
            center,
            radius,
            fill,
            egui::Stroke::new(1.0_f32, egui::Color32::BLACK),
        );

        if !(response.drag_started() || response.dragged() || response.drag_stopped()) {
            continue;
        }
        let current = handle_value(slider, range);
        let new_value = response.interact_pointer_pos().map_or(current, |pointer| {
            let target = value_of(pointer.x);
            // The ends stay in order; a feather handle sets the distance from its end.
            match slider {
                RangeSlider::LuminanceLow => target.min(high),
                RangeSlider::LuminanceHigh => target.max(low),
                RangeSlider::LuminanceFeatherLow => (low - target).max(0.0),
                RangeSlider::LuminanceFeatherHigh => (target - high).max(0.0),
                RangeSlider::ColorTolerance => current,
            }
        });
        actions.push(at.action(slider, new_value, drag_phase(&response)));
    }
    ui.add_space(8.0);
    ui.label(
        egui::RichText::new(format!(
            "Range {:.0}%–{:.0}% · feather {:.0}% / {:.0}%",
            low * 100.0,
            high * 100.0,
            range.feather_low * 100.0,
            range.feather_high * 100.0
        ))
        .small()
        .color(colors::TEXT_MUTED),
    );
}

/// The value `slider` currently has in `range`.
const fn handle_value(slider: RangeSlider, range: &LuminanceRange) -> f32 {
    match slider {
        RangeSlider::LuminanceLow => range.low,
        RangeSlider::LuminanceHigh => range.high,
        RangeSlider::LuminanceFeatherLow => range.feather_low,
        RangeSlider::LuminanceFeatherHigh => range.feather_high,
        RangeSlider::ColorTolerance => 0.0,
    }
}

/// Black to white, in steps small enough to look smooth. The bar is in sRGB-encoded luminance,
/// which is what the range selects on, so the gradient is linear in the encoded value.
fn paint_gradient(painter: &egui::Painter, bar: egui::Rect) {
    const STEPS: usize = 48;
    for step in 0..STEPS {
        let (a, b) = (step as f32 / STEPS as f32, (step + 1) as f32 / STEPS as f32);
        let shade = (a * 255.0).round() as u8;
        painter.rect_filled(
            egui::Rect::from_min_max(
                egui::pos2(bar.left() + a * bar.width(), bar.top()),
                egui::pos2(bar.left() + b * bar.width() + 0.5, bar.bottom()),
            ),
            0.0,
            egui::Color32::from_gray(shade),
        );
    }
}

/// The colour samples as swatches, the tolerance slider, and the picker.
pub(crate) fn show_color(
    ui: &mut egui::Ui,
    at: ComponentRef,
    range: &ColorRange,
    picking: bool,
    actions: &mut Vec<MasksAction>,
) {
    ui.horizontal_wrapped(|ui| {
        for sample in &range.samples {
            let (rect, response) =
                ui.allocate_exact_size(egui::Vec2::splat(SWATCH_SIZE), egui::Sense::hover());
            ui.painter().rect_filled(rect, 3.0, swatch_color(sample));
            ui.painter().rect_stroke(
                rect,
                3.0,
                egui::Stroke::new(1.0_f32, colors::TEXT_MUTED),
                egui::StrokeKind::Inside,
            );
            response.on_hover_text(format!(
                "L {:.2}  a {:+.2}  b {:+.2}",
                sample.lightness, sample.a, sample.b
            ));
        }
        if range.samples.is_empty() {
            ui.label(
                egui::RichText::new("No colors picked yet")
                    .small()
                    .color(colors::WARNING),
            );
        }
    });
    if !picking
        && ui
            .button("Pick colors")
            .on_hover_text("Click the image to pick the colors this range selects")
            .clicked()
    {
        actions.push(MasksAction::PickColor);
    }
    ui.label(
        egui::RichText::new("Click replaces · Shift-click adds · Alt-click removes the nearest")
            .small()
            .color(colors::TEXT_MUTED),
    );

    let spec = AdjustmentSpec {
        label: "Tolerance",
        minimum: COLOR_TOLERANCE_RANGE.minimum,
        maximum: COLOR_TOLERANCE_RANGE.maximum,
        neutral: COLOR_TOLERANCE_RANGE.neutral,
        decimals: 0,
        step: 0.01,
        suffix: "%",
        scale: ValueScale::Percent,
    };
    let mut value = range.tolerance;
    let response = widgets::adjustment_slider(ui, &mut value, spec);
    if response.reset_clicked {
        value = spec.neutral;
    }
    let widget = &response.response;
    if widget.changed() || widget.drag_started() || widget.drag_stopped() || response.reset_clicked
    {
        actions.push(at.action(
            RangeSlider::ColorTolerance,
            value,
            SliderPhase {
                changed: widget.changed(),
                drag_started: widget.drag_started(),
                dragged: widget.dragged(),
                drag_stopped: widget.drag_stopped(),
                reset: response.reset_clicked,
            },
        ));
    }
}

/// The colour of a sample on screen: OkLab to linear sRGB, clipped, then encoded.
fn swatch_color(sample: &ColorSample) -> egui::Color32 {
    let linear = oklab_to_linear_srgb([sample.lightness, sample.a, sample.b]);
    let [red, green, blue] =
        linear.map(|value| (linear_srgb_to_srgb(value.clamp(0.0, 1.0)) * 255.0).round() as u8);
    egui::Color32::from_rgb(red, green, blue)
}
