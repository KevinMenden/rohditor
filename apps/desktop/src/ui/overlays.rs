//! What floats above the window: one menu, one dialog and the toasts.
//!
//! The window has one [`Overlays`], which the panels share. A menu or dialog takes the focus and
//! gives it back when it closes.

use std::time::Duration;

use gpui::{
    AnyView, App, AppContext, Context, DismissEvent, Entity, FocusHandle, Focusable, IntoElement,
    ManagedView, MouseButton, Pixels, Point, Render, Role, SharedString, Subscription, Task,
    Window, anchored, deferred, div, prelude::*, px,
};

use super::keys::{Cancel, FocusNext, FocusPrevious};
use super::menu::{Menu, MenuEntry};
use crate::assets::Icon;
use crate::theme::{STATUS_BAR_HEIGHT, theme};
use crate::ui::button::icon_button;
use crate::ui::test_id::TestId as _;

pub const CONTEXT: &str = "Dialog";
const TOAST_TIME: Duration = Duration::from_secs(6);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToastKind {
    Success,
    Error,
}

struct Toast {
    id: usize,
    kind: ToastKind,
    text: SharedString,
    _expiry: Task<()>,
}

struct Open {
    view: AnyView,
    focus: FocusHandle,
    return_to: Option<FocusHandle>,
    /// Where a menu opens; a dialog has its fixed place.
    at: Option<Point<Pixels>>,
    _dismissed: Subscription,
}

#[derive(Default)]
pub struct Overlays {
    open: Option<Open>,
    toasts: Vec<Toast>,
    next_toast: usize,
}

impl Overlays {
    /// Whether a menu or dialog is open, which then gets the keys.
    pub const fn is_open(&self) -> bool {
        self.open.is_some()
    }

    /// The open dialog, if it is a `V`.
    pub fn dialog<V: 'static>(&self) -> Option<Entity<V>> {
        self.open.as_ref()?.view.clone().downcast().ok()
    }

    pub fn show_menu(
        &mut self,
        at: Point<Pixels>,
        entries: Vec<MenuEntry>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let menu = cx.new(|cx| Menu::new(entries, cx));
        self.show(menu, Some(at), window, cx);
    }

    /// Show a dialog until it emits [`DismissEvent`].
    pub fn show_dialog<V: ManagedView>(
        &mut self,
        view: Entity<V>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.show(view, None, window, cx);
    }

    fn show<V: ManagedView>(
        &mut self,
        view: Entity<V>,
        at: Option<Point<Pixels>>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.close(window, cx);
        let focus = view.focus_handle(cx);
        let return_to = window.focused(cx);
        focus.focus(window, cx);
        // Keep the press that opened it from moving the focus on.
        window.prevent_default();
        let dismissed = cx.subscribe_in(&view, window, |this, _, _: &DismissEvent, window, cx| {
            this.close(window, cx);
        });
        self.open = Some(Open {
            view: view.into(),
            focus,
            return_to,
            at,
            _dismissed: dismissed,
        });
        cx.notify();
    }

    pub fn close(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(open) = self.open.take() {
            if let Some(focus) = open.return_to {
                focus.focus(window, cx);
            }
            cx.notify();
        }
    }

    pub fn toast(
        &mut self,
        kind: ToastKind,
        text: impl Into<SharedString>,
        cx: &mut Context<Self>,
    ) {
        let id = self.next_toast;
        self.next_toast += 1;
        let expiry = cx.spawn(async move |this, cx| {
            cx.background_executor().timer(TOAST_TIME).await;
            // Gone only while the application quits.
            let _ = this.update(cx, |this, cx| this.dismiss_toast(id, cx));
        });
        self.toasts.push(Toast {
            id,
            kind,
            text: text.into(),
            _expiry: expiry,
        });
        cx.notify();
    }

    fn dismiss_toast(&mut self, id: usize, cx: &mut Context<Self>) {
        self.toasts.retain(|toast| toast.id != id);
        cx.notify();
    }

    fn toasts(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let theme = theme();
        let toasts = self.toasts.iter().map(|toast| {
            let id = toast.id;
            let color = match toast.kind {
                ToastKind::Success => theme.success,
                ToastKind::Error => theme.error,
            };
            div()
                .id(id)
                .role(Role::Alert)
                .aria_label(toast.text.clone())
                .test_id("toast")
                .occlude()
                .flex()
                .items_start()
                .gap_2()
                .w(px(340.0))
                .p_2()
                .rounded(px(6.0))
                .border_1()
                .border_color(theme.border)
                .bg(theme.elevated)
                .shadow_lg()
                .child(
                    div()
                        .flex_none()
                        .mt(px(5.0))
                        .size(px(7.0))
                        .rounded_full()
                        .bg(color),
                )
                .child(div().flex_1().min_w_0().child(toast.text.clone()))
                .child(icon_button(
                    "dismiss-toast",
                    Icon::Close,
                    "Dismiss",
                    true,
                    cx.listener(move |this, _, _, cx| this.dismiss_toast(id, cx)),
                ))
        });
        div()
            .absolute()
            .right_3()
            .bottom(STATUS_BAR_HEIGHT + px(12.0))
            .flex()
            .flex_col()
            .gap_2()
            .children(toasts.collect::<Vec<_>>())
    }

    /// Tab within the dialog only.
    fn move_focus(&self, forward: bool, window: &mut Window, cx: &mut App) {
        let Some(open) = self.open.as_ref().filter(|open| open.at.is_none()) else {
            return;
        };
        let start = window.focused(cx);
        loop {
            if forward {
                window.focus_next(cx);
            } else {
                window.focus_prev(cx);
            }
            let focused = window.focused(cx);
            if open.focus.contains_focused(window, cx) || focused == start {
                return;
            }
        }
    }

    fn open_layer(&self, open: &Open, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let content = match open.at {
            Some(at) => anchored()
                .position(at)
                .child(open.view.clone())
                .into_any_element(),
            None => div()
                .flex()
                .justify_center()
                .pt(px(90.0))
                // A press inside does not reach the backdrop.
                .child(div().occlude().child(open.view.clone()))
                .into_any_element(),
        };
        let close = |this: &mut Self, window: &mut Window, cx: &mut Context<Self>| {
            cx.stop_propagation();
            window.prevent_default();
            this.close(window, cx);
        };
        deferred(
            div()
                .absolute()
                .top_0()
                .left_0()
                .size_full()
                .occlude()
                .when(open.at.is_none(), |layer| {
                    layer.key_context(CONTEXT).bg(gpui::black().opacity(0.25))
                })
                .on_action(cx.listener(|this, _: &FocusNext, window, cx| {
                    this.move_focus(true, window, cx);
                }))
                .on_action(cx.listener(|this, _: &FocusPrevious, window, cx| {
                    this.move_focus(false, window, cx);
                }))
                .on_action(cx.listener(|this, _: &Cancel, window, cx| this.close(window, cx)))
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(move |this, _, window, cx| close(this, window, cx)),
                )
                .on_mouse_down(
                    MouseButton::Right,
                    cx.listener(move |this, _, window, cx| close(this, window, cx)),
                )
                .child(content),
        )
        .with_priority(1)
    }
}

impl Render for Overlays {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let open = self.open.as_ref().map(|open| self.open_layer(open, cx));
        div()
            .absolute()
            .top_0()
            .left_0()
            .size_full()
            .child(self.toasts(cx))
            .children(open)
    }
}
