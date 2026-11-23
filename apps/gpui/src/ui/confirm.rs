//! Asks before doing something that cannot be taken back.

use gpui::{
    App, Context, DismissEvent, EventEmitter, FocusHandle, Focusable, IntoElement, Render, Role,
    SharedString, Window, div, prelude::*, px,
};

use crate::theme::theme;
use crate::ui::button::button;

type Accept = Box<dyn Fn(&mut Window, &mut App)>;

pub struct Confirm {
    focus: FocusHandle,
    title: SharedString,
    message: SharedString,
    accept: &'static str,
    on_accept: Accept,
}

impl EventEmitter<DismissEvent> for Confirm {}

impl Focusable for Confirm {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Confirm {
    pub fn new(
        title: impl Into<SharedString>,
        message: impl Into<SharedString>,
        accept: &'static str,
        on_accept: impl Fn(&mut Window, &mut App) + 'static,
        cx: &mut Context<Self>,
    ) -> Self {
        Self {
            focus: cx.focus_handle(),
            title: title.into(),
            message: message.into(),
            accept,
            on_accept: Box::new(on_accept),
        }
    }
}

impl Render for Confirm {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = theme();
        div()
            .id("confirm")
            .role(Role::AlertDialog)
            .aria_label(self.title.clone())
            .aria_description(self.message.clone())
            .track_focus(&self.focus)
            .w(px(400.0))
            .flex()
            .flex_col()
            .gap_3()
            .p_4()
            .rounded(px(8.0))
            .border_1()
            .border_color(theme.border)
            .bg(theme.elevated)
            .shadow_lg()
            .child(self.title.clone())
            .child(div().text_color(theme.muted).child(self.message.clone()))
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
                        cx.listener(|this, _, window, cx| {
                            cx.emit(DismissEvent);
                            (this.on_accept)(window, cx);
                        }),
                    )),
            )
    }
}
