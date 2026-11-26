//! The adjustments of the selected mask.

use gpui::{AnyElement, Context, IntoElement, Role, div, prelude::*, px};
use rohditor_edit::MAX_LOCAL_DETAIL_VARIANTS;
use rohditor_engine::tools::{LayerEdit, LocalSection, LocalSlider, MaskAction};

use super::MasksPanel;
use super::control::Control;
use super::model::Selected;
use crate::panels::controls::{
    ControlPanel as _, Header, ResetValue, RowState, SectionHeaders as _, Slider, SliderRows as _,
};
use crate::theme::{SMALL_FONT_SIZE, theme};
use crate::ui::slider::{Format, TEMPERATURE_RAMP, TINT_RAMP};
use crate::ui::tooltip::tooltip;

pub(super) fn local_slider(slider: LocalSlider) -> Slider {
    let format = match slider {
        LocalSlider::Opacity => Format::Amount,
        LocalSlider::Exposure => Format::Stops,
        LocalSlider::FalseColor => Format::Count,
        _ => Format::Percent,
    };
    let ramp = match slider {
        LocalSlider::Temperature => Some(TEMPERATURE_RAMP),
        LocalSlider::Tint => Some(TINT_RAMP),
        _ => None,
    };
    Slider {
        label: slider.label().into(),
        format,
        range: slider.range(),
        ramp,
    }
}

impl MasksPanel {
    pub(super) fn sliders(&self, selected: &Selected, cx: &mut Context<Self>) -> AnyElement {
        let sections: Vec<_> = LocalSection::ALL
            .into_iter()
            .map(|section| self.section(section, selected, cx))
            .collect();
        div()
            .flex()
            .flex_col()
            .gap(px(3.0))
            .pt_2()
            .child(self.slider_of(LocalSlider::Opacity, selected, cx))
            .children(sections)
            .into_any_element()
    }

    fn section(
        &self,
        section: LocalSection,
        selected: &Selected,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let control = Control::Section(section);
        let header = Header {
            control,
            title: section.label(),
            id: format!("local-{section:?}").into(),
            changed: section
                .sliders()
                .iter()
                .any(|slider| selected.value(*slider) != slider.range().neutral),
        };
        let reset = self.reset_button(
            &header,
            move |this, _, cx| this.reset_section(section, cx),
            cx,
        );
        let title = self
            .title(header, cx)
            .when(section == LocalSection::Detail, |title| {
                title.tooltip(tooltip(
                    format!(
                        "Offsets on the global detail settings. They use the CPU and develop the \
                         photo once more for each different setting, up to \
                         {MAX_LOCAL_DETAIL_VARIANTS} per photo."
                    ),
                    None,
                ))
            })
            .on_action(cx.listener(move |this, _: &ResetValue, _, cx| {
                this.reset_section(section, cx);
            }));
        let open = self.is_open(control, cx);
        let full =
            (open && section == LocalSection::Detail && selected.detail_is_full).then(|| {
                div()
                .pl_2()
                .text_size(SMALL_FONT_SIZE)
                .text_color(theme().faint)
                .child(format!(
                    "The photo has {MAX_LOCAL_DETAIL_VARIANTS} different settings already. Masks \
                     with equal settings count once."
                ))
            });
        let rows: Vec<_> = section
            .sliders()
            .iter()
            .filter(|_| open)
            .map(|slider| self.slider_of(*slider, selected, cx))
            .collect();
        div()
            .id(section.label())
            .role(Role::Group)
            .aria_label(section.label())
            .flex()
            .flex_col()
            .gap(px(3.0))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_1()
                    .child(title)
                    .children(reset),
            )
            .children(full)
            .children(rows)
            .into_any_element()
    }

    fn reset_section(&mut self, section: LocalSection, cx: &mut Context<Self>) {
        self.act_on_selected(
            |mask| MaskAction::Edit(mask, LayerEdit::ResetSection(section)),
            cx,
        );
    }

    fn slider_of(
        &self,
        slider: LocalSlider,
        selected: &Selected,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        let value = selected.value(slider);
        let neutral = slider.range().neutral;
        let state = RowState {
            value,
            // Opacity is an amount.
            origin: if slider == LocalSlider::Opacity {
                slider.range().minimum
            } else {
                neutral
            },
            edited: value != neutral,
            effective: !(slider.is_detail() && selected.detail_is_full),
        };
        let id = format!("local-{slider:?}");
        self.slider_row(
            Control::Slider(slider),
            &id,
            local_slider(slider),
            state,
            cx,
        )
    }
}
