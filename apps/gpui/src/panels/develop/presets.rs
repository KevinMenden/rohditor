//! The saved presets: click one to apply it, then set how much of it.

use gpui::{
    App, ClickEvent, Context, IntoElement, KeyBinding, MouseButton, MouseDownEvent, Role,
    SharedString, Window, actions, div, prelude::*, px, relative,
};

use super::DevelopPanel;
use super::focus::Control;
use super::gestures::{Grip, NudgeBy};
use super::preset_form::PresetForm;
use super::row::{
    CONTEXT as SLIDER_CONTEXT, Decrease, DecreaseMore, Increase, IncreaseMore, ResetValue,
    press_area,
};
use crate::actions::SavePreset;
use crate::assets::Icon;
use crate::session::PresetEntry;
use crate::theme::{RADIUS, SMALL_FONT_SIZE, theme};
use crate::ui::button::command_button;
use crate::ui::confirm::Confirm;
use crate::ui::matching;
use crate::ui::menu::MenuEntry;
use crate::ui::overlays::ToastKind;
use crate::ui::search_list::{SearchEntry, SearchList};
use crate::ui::test_id::TestId as _;
use crate::ui::tooltip::tooltip;

actions!(presets, [RenamePreset, DeletePreset]);

const CONTEXT: &str = "Preset";
/// What an arrow key changes.
const AMOUNT_STEP: f32 = 0.01;

pub(super) fn bind_keys(cx: &mut App) {
    let preset = Some(CONTEXT);
    cx.bind_keys([
        KeyBinding::new("f2", RenamePreset, preset),
        KeyBinding::new("delete", DeletePreset, preset),
        KeyBinding::new("backspace", DeletePreset, preset),
    ]);
}

impl DevelopPanel {
    /// Every listed preset has a focus, which it keeps.
    pub(super) fn list_preset_focus(&mut self, cx: &mut Context<Self>) {
        let presets = &self.model.presets;
        self.preset_focus
            .retain(|name, _| presets.iter().any(|preset| preset.name == *name));
        for preset in presets.iter() {
            self.preset_focus
                .entry(preset.name.clone())
                .or_insert_with(|| cx.focus_handle().tab_stop(true));
        }
    }

    fn apply_preset(&mut self, name: &str, cx: &mut Context<Self>) {
        self.choose(|session, cx| session.apply_preset(name, cx), cx);
        // Its amount is there.
        self.collapsed.remove(&Control::Presets);
        cx.notify();
    }

    /// Search the presets for one to apply.
    pub fn choose_preset(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        // Nothing to apply one to.
        if self.model.recipes.is_none() {
            return;
        }
        let panel = cx.entity();
        let presets = self.model.presets.iter().map(|preset| {
            let (panel, name) = (panel.clone(), preset.name.clone());
            let note = match preset.sections.as_slice() {
                [section] => section.label().to_owned(),
                sections => format!("{} sections", sections.len()),
            };
            SearchEntry::new(preset.name.clone(), Some(note.into()), move |_, cx| {
                panel.update(cx, |panel, cx| panel.apply_preset(&name, cx));
            })
        });
        let save = SearchEntry::new("Save as Preset…", None, |window, cx| {
            window.dispatch_action(Box::new(SavePreset), cx);
        });
        let entries = presets.chain([save]).collect();
        let list = cx.new(|cx| {
            SearchList::new(
                "Apply preset",
                "Search presets…",
                entries,
                matching::words,
                cx,
            )
        });
        self.overlays.update(cx, |overlays, cx| {
            overlays.show_dialog(list, window, cx);
        });
    }

    pub fn save_preset(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let session = self.session.read(cx);
        let Some(document) = session.document().filter(|open| open.frame.is_some()) else {
            return;
        };
        let suggested = document.suggested_preset_sections();
        let session = self.session.clone();
        let form = cx.new(|cx| PresetForm::save(session, &suggested, cx));
        self.overlays.update(cx, |overlays, cx| {
            overlays.show_dialog(form, window, cx);
        });
    }

    fn rename_preset(&mut self, name: &SharedString, window: &mut Window, cx: &mut Context<Self>) {
        let session = self.session.clone();
        let form = cx.new(|cx| PresetForm::rename(session, name.clone(), cx));
        self.overlays.update(cx, |overlays, cx| {
            overlays.show_dialog(form, window, cx);
        });
    }

    fn delete_preset(&mut self, name: &SharedString, window: &mut Window, cx: &mut Context<Self>) {
        let (session, toasts) = (self.session.clone(), self.overlays.clone());
        let name = name.clone();
        let title = format!("Delete the preset \"{name}\"?");
        let question = cx.new(|cx| {
            Confirm::new(
                title,
                "Photos it was applied to keep their edits.",
                "Delete",
                move |_, cx| {
                    let result = session.update(cx, |session, cx| session.delete_preset(&name, cx));
                    if let Err(error) = result {
                        toasts.update(cx, |toasts, cx| toasts.toast(ToastKind::Error, error, cx));
                    }
                },
                cx,
            )
        });
        self.overlays.update(cx, |overlays, cx| {
            overlays.show_dialog(question, window, cx);
        });
    }

    fn preset_menu(
        &self,
        name: &SharedString,
        event: &MouseDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let entry =
            |label,
             key: &'static str,
             run: fn(&mut Self, &SharedString, &mut Window, &mut Context<Self>)| {
                let (panel, name) = (cx.entity(), name.clone());
                MenuEntry::new(label, move |window, cx| {
                    panel.update(cx, |panel, cx| run(panel, &name, window, cx));
                })
                .key(Some(key.into()))
            };
        let entries = vec![
            entry("Apply", "Enter", |panel, name, _, cx| {
                panel.apply_preset(name, cx)
            }),
            entry("Rename…", "F2", Self::rename_preset),
            entry("Delete…", "Delete", Self::delete_preset),
        ];
        self.overlays.update(cx, |overlays, cx| {
            overlays.show_menu(event.position, entries, window, cx);
        });
    }

    pub(super) fn set_preset_amount(&self, amount: f32, cx: &mut Context<Self>) {
        self.session
            .update(cx, |session, cx| session.set_preset_amount(amount, cx));
    }

    /// Back to all of the preset.
    pub(super) fn reset_preset_amount(&mut self, cx: &mut Context<Self>) {
        self.finish_nudge(cx);
        self.set_preset_amount(1.0, cx);
        self.session
            .update(cx, |session, cx| session.finish_gesture(None, cx));
    }

    pub(super) fn presets(&self, cx: &mut Context<Self>) -> Option<impl IntoElement + use<>> {
        let query = self.query(cx);
        let found = |name: &str| matching::words(&format!("Presets {name}"), &query);
        let listed: Vec<_> = self
            .model
            .presets
            .iter()
            .filter(|preset| found(&preset.name))
            .collect();
        if self.edited_only || (listed.is_empty() && !found("")) {
            return None;
        }
        let theme = theme();
        let open = self.is_open(Control::Presets, cx);
        let note = |text: String| {
            div()
                .pl_2()
                .text_size(SMALL_FONT_SIZE)
                .text_color(theme.faint)
                .child(text)
        };
        let mut notes = Vec::new();
        if open && listed.is_empty() {
            notes.push(note("No presets yet".to_owned()));
        }
        if open {
            let warnings = self.session.read(cx).preset_warnings();
            notes.extend(warnings.iter().cloned().map(note));
        }
        let amount = self
            .model
            .applied_preset
            .as_ref()
            .filter(|_| open)
            .map(|(name, amount)| self.preset_amount(name, *amount, cx));
        let rows: Vec<_> = listed
            .into_iter()
            .filter(|_| open)
            .map(|preset| self.preset(preset, cx))
            .collect();
        let save = command_button("save-preset", Icon::Plus, SavePreset, true);
        Some(
            div()
                .flex()
                .flex_col()
                .gap(px(3.0))
                .px_2()
                .pb_2()
                .child(
                    div()
                        .flex()
                        .items_center()
                        .child(self.title(Control::Presets, "Presets", "Presets", cx))
                        .child(save),
                )
                .children(notes)
                .children(rows)
                .children(amount),
        )
    }

    fn preset(&self, preset: &PresetEntry, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let theme = theme();
        let name = preset.name.clone();
        let sections = preset.sections.iter().map(|section| section.label());
        let sections = sections.collect::<Vec<_>>().join(", ");
        let applied = self.model.applied_preset.as_ref().map(|applied| &applied.0) == Some(&name);
        // Listed together with its focus.
        let focus = self.preset_focus.get(&name).cloned();
        let revealer = focus
            .clone()
            .map(|focus| self.focus_revealer(focus, false, cx));
        let [clicked, opened, renamed, deleted] = std::array::from_fn(|_| name.clone());
        div()
            .id(SharedString::from(format!("preset-{name}")))
            .role(Role::Button)
            .aria_label(format!("Apply preset {name}"))
            .key_context(CONTEXT)
            .when_some(focus, |row, focus| row.track_focus(&focus))
            .test_id(format!("preset:{name}"))
            .relative()
            .children(revealer)
            .flex()
            .items_center()
            .h(px(24.0))
            .px_2()
            .rounded(RADIUS)
            .border_1()
            .border_color(gpui::transparent_black())
            .focus_visible(move |style| style.border_color(theme.accent))
            .bg(theme.element)
            .hover(move |style| style.bg(theme.hover))
            .cursor_pointer()
            .text_color(if applied { theme.text } else { theme.muted })
            .tooltip(tooltip(sections, None))
            .child(div().min_w_0().truncate().child(name))
            .on_mouse_down(MouseButton::Left, |_, window, _| window.prevent_default())
            .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                this.apply_preset(&clicked, cx);
            }))
            .on_mouse_down(
                MouseButton::Right,
                cx.listener(move |this, event, window, cx| {
                    this.preset_menu(&opened, event, window, cx);
                }),
            )
            .on_action(cx.listener(move |this, _: &RenamePreset, window, cx| {
                this.rename_preset(&renamed, window, cx);
            }))
            .on_action(cx.listener(move |this, _: &DeletePreset, window, cx| {
                this.delete_preset(&deleted, window, cx);
            }))
    }

    /// How much of the preset applied last is used: a slider as the rows of the sections.
    fn preset_amount(
        &self,
        name: &SharedString,
        amount: f32,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        let theme = theme();
        let control = Control::PresetAmount;
        let label = format!("{:.0}%", amount * 100.0);
        let step = |steps: f32| NudgeBy::PresetAmount(steps * AMOUNT_STEP);
        div()
            .id("preset-amount")
            .role(Role::Slider)
            .aria_label(format!("Amount of {name}"))
            .aria_value(label.clone())
            .key_context(SLIDER_CONTEXT)
            .track_focus(self.focus.control(control))
            .test_id("preset-amount")
            .relative()
            .child(self.revealer(control, cx))
            .flex()
            .items_center()
            .justify_between()
            .h(px(24.0))
            .px_2()
            .rounded(RADIUS)
            .overflow_hidden()
            .border_1()
            .border_color(gpui::transparent_black())
            .focus(move |style| style.border_color(theme.accent))
            .bg(theme.element)
            .hover(move |style| style.bg(theme.hover))
            .cursor(gpui::CursorStyle::ResizeLeftRight)
            .on_action(cx.listener(move |this, _: &Decrease, _, cx| this.nudge(step(-1.0), cx)))
            .on_action(cx.listener(move |this, _: &Increase, _, cx| this.nudge(step(1.0), cx)))
            .on_action(cx.listener(move |this, _: &DecreaseMore, _, cx| {
                this.nudge(step(-10.0), cx);
            }))
            .on_action(cx.listener(move |this, _: &IncreaseMore, _, cx| {
                this.nudge(step(10.0), cx);
            }))
            .on_action(cx.listener(|this, _: &ResetValue, _, cx| this.reset_preset_amount(cx)))
            .child(
                div()
                    .absolute()
                    .top_0()
                    .bottom_0()
                    .left_0()
                    .w(relative(amount))
                    .bg(theme.fill),
            )
            .child(
                div()
                    .relative()
                    .min_w_0()
                    .truncate()
                    .child(format!("Amount of {name}")),
            )
            .child(
                div()
                    .relative()
                    .flex_none()
                    .text_size(SMALL_FONT_SIZE)
                    .child(label),
            )
            .child(press_area(cx.entity(), Grip::PresetAmount))
    }
}
