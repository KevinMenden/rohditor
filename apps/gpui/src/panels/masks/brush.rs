//! The brush, while painting.

use gpui::{AnyElement, Context, IntoElement, div, prelude::*, px};
use rohditor_engine::tools::{BrushSlider, MaskAction};

use super::MasksPanel;
use super::control::Control;
use super::model::Brush;
use crate::panels::controls::{RowState, Slider, SliderRows as _};
use crate::theme::{SMALL_FONT_SIZE, theme};
use crate::ui::slider::Format;
use crate::ui::toggle::toggle;

pub(super) fn brush_slider(slider: BrushSlider) -> Slider {
    Slider {
        label: slider.label().into(),
        format: match slider {
            BrushSlider::Size => Format::Size,
            BrushSlider::Feather | BrushSlider::Flow => Format::Amount,
        },
        range: slider.range(),
        ramp: None,
    }
}

impl MasksPanel {
    pub(super) fn brush(&self, brush: Brush, cx: &mut Context<Self>) -> AnyElement {
        let rows: Vec<_> = BrushSlider::ALL
            .into_iter()
            .map(|slider| {
                let range = slider.range();
                let value = slider.get(brush.settings);
                let state = RowState {
                    value,
                    origin: range.minimum,
                    edited: value != range.neutral,
                    effective: true,
                };
                let id = format!("brush-{slider:?}");
                self.slider_row(Control::Brush(slider), &id, brush_slider(slider), state, cx)
            })
            .collect();
        let panel = cx.entity();
        div()
            .flex()
            .flex_col()
            .gap(px(3.0))
            .pt_2()
            .child(toggle(
                "erase",
                "Erase (E, or hold Alt)",
                brush.erase,
                move |_, cx| {
                    panel.update(cx, |panel, cx| panel.act(MaskAction::ToggleErase, cx));
                },
            ))
            .children(rows)
            .child(
                div()
                    .px_1()
                    .text_size(SMALL_FONT_SIZE)
                    .text_color(theme().faint)
                    .child("[ and ] change the size, { and } the feather."),
            )
            .into_any_element()
    }
}
