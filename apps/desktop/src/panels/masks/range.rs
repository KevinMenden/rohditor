//! The selected luminance or color range.
//!
//! A luminance range is a bar from black to white with the selected band on it. A press takes the
//! closest handle: in the upper part an end of the band, in the lower part an end of its feathers.
//! Shift drags finer, and a double click resets it. The rows below have the values.

use gpui::{
    AnyElement, Bounds, Context, CursorStyle, DispatchPhase, Entity, IntoElement, MouseButton,
    MouseDownEvent, PathBuilder, Pixels, Role, Window, canvas, div, linear_color_stop,
    linear_gradient, point, prelude::*, px, relative, rgb,
};
use rohditor_core::color_sample_srgb;
use rohditor_edit::{ColorRange, LuminanceRange, MaskShape};
use rohditor_engine::tools::RangeSlider;

use super::MasksPanel;
use super::control::Control;
use crate::actions::PickMaskColors;
use crate::panels::controls::{Gestures as _, RowState, Slider, SliderRows as _};
use crate::theme::{RADIUS, SMALL_FONT_SIZE, theme};
use crate::ui::button::link;
use crate::ui::slider::Format;
use crate::ui::test_id::TestId as _;

/// Of the bar itself. Below it are the handles of the feathers.
const BAR: Pixels = px(28.0);
const FEATHERS: Pixels = px(14.0);
const HANDLE: Pixels = px(9.0);
const SWATCH: Pixels = px(20.0);

const ENDS: [RangeSlider; 2] = [RangeSlider::LuminanceLow, RangeSlider::LuminanceHigh];
const FEATHER_ENDS: [RangeSlider; 2] = [
    RangeSlider::LuminanceFeatherLow,
    RangeSlider::LuminanceFeatherHigh,
];

pub(super) fn range_slider(slider: RangeSlider) -> Slider {
    Slider {
        label: slider.label().into(),
        format: Format::Amount,
        range: slider.range(),
        ramp: None,
    }
}

impl MasksPanel {
    /// The controls of the shape, if it is a range.
    pub(super) fn range(&self, shape: &MaskShape, cx: &mut Context<Self>) -> Option<AnyElement> {
        let graphic = match shape {
            MaskShape::Luminance(range) => self.luminance(range, cx).into_any_element(),
            MaskShape::Color(range) => self.colors(range).into_any_element(),
            MaskShape::Linear(_) | MaskShape::Radial(_) | MaskShape::Brush(_) => return None,
        };
        let rows: Vec<_> = RangeSlider::ALL
            .into_iter()
            .filter_map(|slider| {
                let value = slider.get(shape)?;
                let state = RowState {
                    value,
                    origin: slider.range().minimum,
                    edited: value != slider.default_value(),
                    effective: true,
                };
                let id = format!("range-{slider:?}");
                Some(self.slider_row(Control::Range(slider), &id, range_slider(slider), state, cx))
            })
            .collect();
        Some(
            div()
                .flex()
                .flex_col()
                .gap(px(3.0))
                .pt_2()
                .child(graphic)
                .children(rows)
                .into_any_element(),
        )
    }

    fn luminance(
        &self,
        range: &LuminanceRange,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        let theme = theme();
        let handles = RangeSlider::LUMINANCE.map(|slider| {
            let active = self.controls.dragged() == Some(Control::Handle(slider));
            let middle = if ENDS.contains(&slider) {
                BAR / 2.0
            } else {
                BAR + FEATHERS / 2.0
            };
            div()
                .test_id(format!("handle:{slider:?}"))
                .absolute()
                .left(relative(slider.handle_position(range)))
                .top(middle)
                .ml(-HANDLE / 2.0)
                .mt(-HANDLE / 2.0)
                .size(HANDLE)
                .rounded_full()
                .border_1()
                .border_color(theme.editor)
                .bg(if active { theme.accent } else { theme.text })
        });
        let gradient = linear_gradient(
            90.0,
            linear_color_stop(rgb(0x00_0000), 0.0),
            linear_color_stop(rgb(0xff_ffff), 1.0),
        );
        // The handles at the ends stick out.
        div().px(HANDLE / 2.0).child(
            div()
                .id("luminance")
                .role(Role::Image)
                .aria_label("Luminance range")
                .test_id("luminance")
                .relative()
                .h(BAR + FEATHERS)
                .cursor(CursorStyle::ResizeLeftRight)
                .child(div().h(BAR).rounded(RADIUS).bg(gradient))
                .child(band(*range))
                .children(handles)
                .child(press_area(cx.entity())),
        )
    }

    fn press_luminance(
        &mut self,
        event: &MouseDownEvent,
        bounds: Bounds<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(MaskShape::Luminance(range)) = self.model.range() else {
            return;
        };
        let handles = if event.position.y - bounds.top() < BAR {
            ENDS
        } else {
            FEATHER_ENDS
        };
        let position = (event.position.x - bounds.left()) / bounds.size.width;
        let slider = RangeSlider::closest_handle(handles, range, position);
        // For the arrow keys. Not on the panel, which would get it now.
        window.prevent_default();
        self.controls
            .focus
            .control(Control::Range(slider))
            .focus(window, cx);
        self.press(Control::Handle(slider), event, bounds, cx);
    }

    fn colors(&self, range: &ColorRange) -> impl IntoElement + use<> {
        let theme = theme();
        let swatches = range.samples.iter().enumerate().map(|(index, sample)| {
            let [red, green, blue] = color_sample_srgb(sample).map(u32::from);
            div()
                .test_id(format!("swatch:{index}"))
                .relative()
                .size(SWATCH)
                .rounded(RADIUS)
                .border_1()
                .border_color(theme.border)
                .bg(rgb(red << 16 | green << 8 | blue))
        });
        let swatches: Vec<_> = swatches.collect();
        let none = swatches.is_empty().then(|| {
            div()
                .text_color(theme.warning)
                .child("No colors picked yet.")
        });
        div()
            .flex()
            .items_start()
            .justify_between()
            .gap_2()
            .px_1()
            .text_size(SMALL_FONT_SIZE)
            .child(
                div()
                    .flex()
                    .flex_wrap()
                    .min_w_0()
                    .gap_1()
                    .children(swatches)
                    .children(none),
            )
            .child(link("pick-mask-colors", "Pick", PickMaskColors, true).flex_none())
    }
}

/// The selected band: full strength between the ends, fading out over the feathers.
fn band(range: LuminanceRange) -> impl IntoElement {
    canvas(
        |_, _, _| (),
        move |bounds, (), window, _| {
            let at = |slider: RangeSlider, y: Pixels| {
                let x = bounds.size.width * slider.handle_position(&range);
                bounds.origin + point(x, y)
            };
            let corners = [
                at(RangeSlider::LuminanceFeatherLow, BAR),
                at(RangeSlider::LuminanceLow, px(0.0)),
                at(RangeSlider::LuminanceHigh, px(0.0)),
                at(RangeSlider::LuminanceFeatherHigh, BAR),
            ];
            let accent = theme().accent;
            for (mut path, color) in [
                (PathBuilder::fill(), accent.opacity(0.45)),
                (PathBuilder::stroke(px(1.0)), accent),
            ] {
                path.add_polygon(&corners, true);
                if let Ok(path) = path.build() {
                    window.paint_path(path, color);
                }
            }
        },
    )
    .absolute()
    .top_0()
    .left_0()
    .size_full()
}

/// Covers the bar and reports presses together with its bounds.
fn press_area(panel: Entity<MasksPanel>) -> impl IntoElement {
    canvas(
        |bounds, window, _| window.insert_hitbox(bounds, gpui::HitboxBehavior::Normal),
        move |bounds, hitbox, window, _| {
            window.on_mouse_event(move |event: &MouseDownEvent, phase, window, cx| {
                if phase == DispatchPhase::Bubble
                    && event.button == MouseButton::Left
                    && hitbox.is_hovered(window)
                {
                    panel.update(cx, |panel, cx| {
                        panel.press_luminance(event, bounds, window, cx);
                    });
                }
            });
        },
    )
    .absolute()
    .top_0()
    .left_0()
    .size_full()
}
