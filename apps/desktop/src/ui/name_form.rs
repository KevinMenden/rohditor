//! A dialog that asks for a name.

use gpui::{
    App, Context, DismissEvent, Entity, EventEmitter, FocusHandle, Focusable, IntoElement, Render,
    Role, Window, div, prelude::*, px,
};

use crate::theme::{RADIUS, SMALL_FONT_SIZE, theme};
use crate::ui::button::button;
use crate::ui::keys::{Cancel, Confirm};
use crate::ui::test_id::TestId as _;
use crate::ui::text_input::{Changed, TextInput};

type Accept = Box<dyn Fn(&str, &mut App) -> Result<(), String>>;

pub struct NameForm {
    title: &'static str,
    accept: &'static str,
    name: Entity<TextInput>,
    error: Option<String>,
    on_accept: Accept,
}

impl EventEmitter<DismissEvent> for NameForm {}

impl Focusable for NameForm {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.name.focus_handle(cx)
    }
}

impl NameForm {
    /// Stays open with the error while `on_accept` fails, so that it can be put right.
    pub fn new(
        title: &'static str,
        accept: &'static str,
        name: String,
        on_accept: impl Fn(&str, &mut App) -> Result<(), String> + 'static,
        cx: &mut Context<Self>,
    ) -> Self {
        let name = cx.new(|cx| TextInput::new("Name", "Name…", name, cx));
        cx.subscribe(&name, |this, _, _: &Changed, cx| {
            this.error = None;
            cx.notify();
        })
        .detach();
        Self {
            title,
            accept,
            name,
            error: None,
            on_accept: Box::new(on_accept),
        }
    }

    fn accept(&mut self, cx: &mut Context<Self>) {
        let name = self.name.read(cx).text().to_owned();
        match (self.on_accept)(&name, cx) {
            Ok(()) => cx.emit(DismissEvent),
            Err(error) => self.error = Some(error),
        }
        cx.notify();
    }
}

impl Render for NameForm {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = theme();
        div()
            .id("name-form")
            .role(Role::Dialog)
            .aria_label(self.title)
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
            .child(self.title)
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
            .children(self.error.clone().map(|error| {
                div()
                    .test_id("name-error")
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
                        self.accept,
                        cx.listener(|this, _, _, cx| this.accept(cx)),
                    )),
            )
    }
}
