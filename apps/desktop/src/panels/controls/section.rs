//! The header of a section: its title, which collapses it, and whether it is edited.

use gpui::{
    App, Context, Div, KeyBinding, MouseButton, Role, SharedString, Stateful, div, prelude::*, px,
};

use super::row::ResetValue;
use super::{ControlPanel, FocusControls as _};
use crate::assets::Icon;
use crate::theme::{RADIUS, SMALL_FONT_SIZE, theme};
use crate::ui::button::{icon, icon_button};
use crate::ui::test_id::TestId as _;

const CONTEXT: &str = "SectionHeader";

pub(super) fn bind_keys(cx: &mut App) {
    let header = Some(CONTEXT);
    cx.bind_keys([
        KeyBinding::new("delete", ResetValue, header),
        KeyBinding::new("backspace", ResetValue, header),
    ]);
}

/// A title that collapses what is below it.
#[derive(Debug, Clone)]
pub struct Header<C> {
    pub control: C,
    pub title: &'static str,
    /// For the elements' names.
    pub id: SharedString,
    /// Whether what is below it is edited.
    pub changed: bool,
}

impl<C> Header<C> {
    /// Of what cannot be edited.
    pub fn plain(control: C, title: &'static str) -> Self {
        Self {
            control,
            title,
            id: title.into(),
            changed: false,
        }
    }
}

pub trait SectionHeaders: ControlPanel {
    fn toggle_section(&mut self, header: Self::Control, cx: &mut Context<Self>) {
        let collapsed = &mut self.controls_mut().collapsed;
        if !collapsed.remove(&header) {
            collapsed.insert(header);
        }
        cx.notify();
    }

    fn title(&self, header: Header<Self::Control>, cx: &mut Context<Self>) -> Stateful<Div> {
        let theme = theme();
        let Header {
            control,
            title,
            id,
            changed,
        } = header;
        let collapsed = !self.is_open(control, cx);
        let chevron = if collapsed {
            Icon::ChevronRight
        } else {
            Icon::ChevronDown
        };
        let dot = changed.then(|| {
            div()
                .test_id(format!("changed:{id}"))
                .relative()
                .ml_1()
                .size(px(5.0))
                .rounded_full()
                .bg(theme.accent)
        });
        let described = if changed {
            format!("{title}, edited")
        } else {
            title.to_owned()
        };
        div()
            .id(title)
            .role(Role::Button)
            .aria_label(described)
            .aria_expanded(!collapsed)
            .key_context(CONTEXT)
            .track_focus(self.controls().focus.control(control))
            .relative()
            .child(self.revealer(control, cx))
            .rounded(RADIUS)
            .border_1()
            .border_color(gpui::transparent_black())
            .focus_visible(move |style| style.border_color(theme.accent))
            .on_mouse_down(MouseButton::Left, |_, window, _| window.prevent_default())
            .test_id(format!("section:{id}"))
            .flex()
            .flex_1()
            .items_center()
            .gap_1()
            .h(px(28.0))
            .cursor_pointer()
            .text_size(SMALL_FONT_SIZE)
            .text_color(if changed { theme.text } else { theme.muted })
            .child(icon(chevron, theme.faint))
            .child(title)
            .children(dot)
            .on_click(cx.listener(move |this, _, _, cx| this.toggle_section(control, cx)))
    }

    /// Resets the section. By keyboard, Delete on its header does the same.
    fn reset_button(
        &self,
        header: &Header<Self::Control>,
        reset: impl Fn(&mut Self, &mut gpui::Window, &mut Context<Self>) + 'static,
        cx: &mut Context<Self>,
    ) -> Option<Stateful<Div>> {
        let Header { title, id, .. } = header;
        header.changed.then(|| {
            icon_button(
                format!("reset-{id}"),
                Icon::Reset,
                format!("Reset {title}"),
                true,
                cx.listener(move |this, _, window, cx| reset(this, window, cx)),
            )
            .tab_stop(false)
        })
    }
}

impl<P: ControlPanel> SectionHeaders for P {}
