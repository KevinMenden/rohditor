//! The dialog that saves or renames a preset.

use gpui::{
    App, Context, DismissEvent, Entity, EventEmitter, FocusHandle, Focusable, IntoElement, Render,
    Role, SharedString, Window, div, prelude::*, px, relative,
};
use rohditor_edit::RecipeSection;

use crate::session::Session;
use crate::theme::{RADIUS, SMALL_FONT_SIZE, theme};
use crate::ui::button::button;
use crate::ui::keys::{Cancel, Confirm};
use crate::ui::test_id::TestId as _;
use crate::ui::text_input::{Changed, TextInput};
use crate::ui::toggle::toggle;

enum Purpose {
    /// Every section, and whether it goes into the preset.
    Save(Vec<(RecipeSection, bool)>),
    Rename(SharedString),
}

pub(super) struct PresetForm {
    session: Entity<Session>,
    purpose: Purpose,
    name: Entity<TextInput>,
    error: Option<String>,
}

impl EventEmitter<DismissEvent> for PresetForm {}

impl Focusable for PresetForm {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.name.focus_handle(cx)
    }
}

impl PresetForm {
    pub fn save(
        session: Entity<Session>,
        suggested: &[RecipeSection],
        cx: &mut Context<Self>,
    ) -> Self {
        let sections = RecipeSection::ALL
            .into_iter()
            .map(|section| (section, suggested.contains(&section)));
        Self::new(
            session,
            Purpose::Save(sections.collect()),
            String::new(),
            cx,
        )
    }

    pub fn rename(session: Entity<Session>, name: SharedString, cx: &mut Context<Self>) -> Self {
        let text = name.to_string();
        Self::new(session, Purpose::Rename(name), text, cx)
    }

    fn new(
        session: Entity<Session>,
        purpose: Purpose,
        name: String,
        cx: &mut Context<Self>,
    ) -> Self {
        let name = cx.new(|cx| TextInput::new("Preset name", "Name…", name, cx));
        cx.subscribe(&name, |this, _, _: &Changed, cx| {
            this.error = None;
            cx.notify();
        })
        .detach();
        Self {
            session,
            purpose,
            name,
            error: None,
        }
    }

    /// Stays open if it fails, so that it can be put right.
    fn accept(&mut self, cx: &mut Context<Self>) {
        let name = self.name.read(cx).text().to_owned();
        let result = self.session.update(cx, |session, cx| match &self.purpose {
            Purpose::Save(sections) => {
                let included = sections.iter().filter(|(_, included)| *included);
                let sections: Vec<_> = included.map(|(section, _)| *section).collect();
                session.save_preset(&name, &sections, cx)
            }
            Purpose::Rename(old) => session.rename_preset(old, &name, cx),
        });
        match result {
            Ok(()) => cx.emit(DismissEvent),
            Err(error) => self.error = Some(error),
        }
        cx.notify();
    }

    fn toggle(&mut self, index: usize, cx: &mut Context<Self>) {
        if let Purpose::Save(sections) = &mut self.purpose {
            sections[index].1 ^= true;
            self.error = None;
            cx.notify();
        }
    }

    fn sections(&self, cx: &mut Context<Self>) -> Option<impl IntoElement + use<>> {
        let Purpose::Save(sections) = &self.purpose else {
            return None;
        };
        let form = cx.entity();
        let toggles = sections.iter().enumerate().map(|(index, (section, on))| {
            let form = form.clone();
            let toggle = toggle(section.label(), section.label(), *on, move |_, cx| {
                form.update(cx, |form, cx| form.toggle(index, cx));
            });
            div().w(relative(0.5)).child(toggle)
        });
        Some(
            div()
                .flex()
                .flex_wrap()
                .children(toggles.collect::<Vec<_>>()),
        )
    }
}

impl Render for PresetForm {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = theme();
        let (title, accept) = match self.purpose {
            Purpose::Save(_) => ("Save Preset", "Save"),
            Purpose::Rename(_) => ("Rename Preset", "Rename"),
        };
        div()
            .id("preset-form")
            .role(Role::Dialog)
            .aria_label(title)
            .on_action(cx.listener(|this, _: &Confirm, _, cx| this.accept(cx)))
            .on_action(cx.listener(|_, _: &Cancel, _, cx| cx.emit(DismissEvent)))
            .w(px(420.0))
            .flex()
            .flex_col()
            .gap_3()
            .p_4()
            .rounded(px(8.0))
            .border_1()
            .border_color(theme.border)
            .bg(theme.elevated)
            .shadow_lg()
            .child(title)
            .child(
                div()
                    .flex()
                    .items_center()
                    .h(px(28.0))
                    .px_2()
                    .rounded(RADIUS)
                    .bg(theme.editor)
                    .child(self.name.clone()),
            )
            .children(self.sections(cx))
            .children(self.error.clone().map(|error| {
                div()
                    .test_id("preset-error")
                    .relative()
                    .text_size(SMALL_FONT_SIZE)
                    .text_color(theme.error)
                    .child(error)
            }))
            .child(
                div()
                    .flex()
                    .justify_end()
                    .gap_2()
                    .child(button(
                        "cancel",
                        "Cancel",
                        cx.listener(|_, _, _, cx| cx.emit(DismissEvent)),
                    ))
                    .child(button(
                        "accept",
                        accept,
                        cx.listener(|this, _, _, cx| this.accept(cx)),
                    )),
            )
    }
}
