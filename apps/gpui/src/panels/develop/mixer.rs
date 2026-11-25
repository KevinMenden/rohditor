//! The colours of the mixer. One is selected, and its sliders are listed below.

use gpui::{Context, IntoElement, MouseButton, Role, Window, div, prelude::*, px, rgb};
use rohditor_edit::{EditRecipe, HSL_CHANNEL_COUNT, Parameter, RecipeSection};

use super::DevelopPanel;
use super::control::Control;
use crate::panels::adjustments::{MIXER_COLORS, MIXER_LABELS};
use crate::panels::controls::{Decrease, FocusControls as _, Increase, SLIDER_CONTEXT};
use crate::theme::{RADIUS, theme};
use crate::ui::test_id::TestId as _;
use crate::ui::tooltip::tooltip;

/// The colour a parameter of the mixer belongs to.
pub(super) const fn channel_of(parameter: Parameter) -> Option<usize> {
    match parameter {
        Parameter::HslHue(channel)
        | Parameter::HslSaturation(channel)
        | Parameter::HslLuminance(channel) => Some(channel),
        _ => None,
    }
}

impl DevelopPanel {
    /// Show the sliders of a colour picked on the photo.
    pub(super) fn show_mixer_channel(
        &mut self,
        channel: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // What would hide the colours.
        self.controls
            .collapsed
            .remove(&Control::Section(RecipeSection::ColorMixer));
        self.edited_only = false;
        self.search.update(cx, |search, cx| search.clear(cx));
        self.scroll_to(Control::MixerChannels, window, cx);
        self.select_mixer_channel(channel, cx);
    }

    pub fn select_mixer_channel(&mut self, channel: usize, cx: &mut Context<Self>) {
        self.mixer_channel = channel;
        cx.notify();
    }

    fn step_mixer_channel(&mut self, steps: usize, cx: &mut Context<Self>) {
        self.select_mixer_channel((self.mixer_channel + steps) % HSL_CHANNEL_COUNT, cx);
    }

    pub(super) fn mixer_channels(
        &self,
        recipe: &EditRecipe,
        defaults: &EditRecipe,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        let theme = theme();
        let swatches: Vec<_> = (0..HSL_CHANNEL_COUNT)
            .map(|channel| {
                let selected = channel == self.mixer_channel;
                let changed = [
                    Parameter::HslHue(channel),
                    Parameter::HslSaturation(channel),
                    Parameter::HslLuminance(channel),
                ]
                .iter()
                .any(|parameter| parameter.is_changed(recipe, defaults));
                let label = MIXER_LABELS[channel];
                div()
                    .id(label)
                    .role(Role::RadioButton)
                    .aria_label(label)
                    .aria_selected(selected)
                    .tooltip(tooltip(label, None))
                    .test_id(format!("mixer:{label}"))
                    .relative()
                    .flex()
                    .flex_col()
                    .items_center()
                    .justify_center()
                    .gap(px(3.0))
                    .flex_1()
                    .h(px(32.0))
                    .rounded(RADIUS)
                    .cursor_pointer()
                    .when(selected, |swatch| swatch.bg(theme.selected))
                    .hover(move |style| style.bg(theme.hover))
                    .child(
                        div()
                            .size(px(14.0))
                            .rounded_full()
                            .bg(rgb(MIXER_COLORS[channel])),
                    )
                    .child(
                        div()
                            .size(px(4.0))
                            .rounded_full()
                            .when(changed, |dot| dot.bg(theme.accent)),
                    )
                    .on_mouse_down(MouseButton::Left, |_, window, _| window.prevent_default())
                    .on_click(cx.listener(move |this, _, window, cx| {
                        let colors = this.controls.focus.control(Control::MixerChannels).clone();
                        colors.focus(window, cx);
                        this.select_mixer_channel(channel, cx);
                    }))
            })
            .collect();
        let control = Control::MixerChannels;
        div()
            .id("mixer-channels")
            .role(Role::RadioGroup)
            .aria_label("Color")
            .key_context(SLIDER_CONTEXT)
            .track_focus(self.controls.focus.control(control))
            .relative()
            .child(self.revealer(control, cx))
            .flex()
            .rounded(RADIUS)
            .border_1()
            .border_color(gpui::transparent_black())
            .focus_visible(move |style| style.border_color(theme.accent))
            .on_action(cx.listener(|this, _: &Decrease, _, cx| {
                this.step_mixer_channel(HSL_CHANNEL_COUNT - 1, cx);
            }))
            .on_action(cx.listener(|this, _: &Increase, _, cx| this.step_mixer_channel(1, cx)))
            .children(swatches)
    }
}
