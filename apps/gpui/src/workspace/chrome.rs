//! The bars around the content, and the window controls where the desktop leaves them to us.

use gpui::{
    Context, CursorStyle, Decorations, Div, Hsla, IntoElement, MouseButton, ResizeEdge, Window,
    div, prelude::*, px,
};
use rohditor_engine::view::ViewState;

use super::Workspace;
use crate::actions::{
    ActualPixels, CloseFile, Command, Export, Quit, Redo, ToggleCommandPalette, ToggleDevelop,
    ToggleFiles, Undo, ZoomToFit,
};
use crate::assets::Icon;
use crate::theme::{SMALL_FONT_SIZE, STATUS_BAR_HEIGHT, TAB_HEIGHT, TITLE_BAR_HEIGHT, theme};
use crate::ui::button::{command_button, icon_button};
use crate::ui::dropdown::dropdown;
use crate::ui::menu::MenuEntry;

const RESIZE_MARGIN: gpui::Pixels = px(5.0);

impl Workspace {
    pub(super) fn title_bar(
        &self,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        let theme = theme();
        let session = self.session.read(cx);
        let folder = self.library.read(cx).folder_name();
        let edits = session.document().map(|document| &document.edits);
        let can_undo = edits.is_some_and(|edits| edits.can_undo());
        let can_redo = edits.is_some_and(|edits| edits.can_redo());
        let own_controls = matches!(window.window_decorations(), Decorations::Client { .. });
        div()
            .id("title-bar")
            .flex()
            .flex_none()
            .items_center()
            .justify_between()
            .h(TITLE_BAR_HEIGHT)
            .px_2()
            .bg(theme.title_bar)
            .border_b_1()
            .border_color(theme.border)
            .on_mouse_down(MouseButton::Left, |event, window, _| {
                if event.click_count == 2 {
                    window.zoom_window();
                } else {
                    window.start_window_move();
                }
            })
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .pl_1()
                    .child("Rohditor")
                    .children(folder.map(|name| div().text_color(theme.muted).child(name))),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_1()
                    .child(command_button(
                        "search",
                        Icon::Search,
                        ToggleCommandPalette,
                        true,
                    ))
                    .child(command_button("undo", Icon::Undo, Undo, can_undo))
                    .child(command_button("redo", Icon::Redo, Redo, can_redo))
                    .child(command_button(
                        "export",
                        Icon::Export,
                        Export,
                        session.export_ready(),
                    ))
                    .when(own_controls, |bar| bar.child(window_controls())),
            )
    }

    /// The open photo's tab.
    pub(super) fn tab(&self, cx: &mut Context<Self>) -> Option<impl IntoElement + use<>> {
        let theme = theme();
        let document = self.session.read(cx).document()?;
        let dirty = document.is_dirty();
        Some(
            div()
                .flex()
                .flex_none()
                .h(TAB_HEIGHT)
                .bg(theme.panel)
                .border_b_1()
                .border_color(theme.border)
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap_2()
                        .h_full()
                        .pl_3()
                        .pr_1()
                        .bg(theme.editor)
                        .border_r_1()
                        .border_color(theme.border)
                        .child(
                            div()
                                .size(px(6.0))
                                .rounded_full()
                                .when(dirty, |dot| dot.bg(theme.accent)),
                        )
                        .child(document.file_name())
                        .child(command_button("close-tab", Icon::Close, CloseFile, true)),
                ),
        )
    }

    pub(super) fn status_bar(
        &self,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        let theme = theme();
        let session = self.session.read(cx);
        let (message, color) = self.status_message(cx);
        let details = session.document().and_then(|document| {
            let info = document.info.as_ref()?;
            Some(format!("{} × {}", info.width, info.height))
        });
        let zoom = self.viewport.read(cx).zoom_label(window, cx);
        let text = |text: String| div().text_color(theme.muted).child(text);
        div()
            .flex()
            .flex_none()
            .items_center()
            .justify_between()
            .gap_4()
            .h(STATUS_BAR_HEIGHT)
            .px_2()
            .bg(theme.title_bar)
            .border_t_1()
            .border_color(theme.border)
            .text_size(SMALL_FONT_SIZE)
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .min_w_0()
                    .child(
                        command_button("toggle-files", Icon::Files, ToggleFiles, true)
                            .when(self.files_open, |button| button.bg(theme.selected)),
                    )
                    .child(div().min_w_0().truncate().text_color(color).child(message)),
            )
            .child(
                div()
                    .flex()
                    .flex_none()
                    .items_center()
                    .gap_3()
                    .children(details.map(text))
                    .children(zoom.map(|zoom| self.zoom_dropdown(zoom)))
                    .child(text(session.processor()))
                    .child(
                        command_button("toggle-develop", Icon::Sliders, ToggleDevelop, true)
                            .when(self.develop_open, |button| button.bg(theme.selected)),
                    ),
            )
    }

    fn zoom_dropdown(&self, zoom: String) -> impl IntoElement + use<> {
        let viewport = self.viewport.clone();
        dropdown("zoom", "Zoom", zoom, self.overlays.clone(), move |_, cx| {
            let view = viewport.read(cx).view(cx);
            let entry = |action: Box<dyn gpui::Action>, checked| {
                let command = Command::of(action.as_ref());
                MenuEntry::new(command.title(), move |window, cx| {
                    window.dispatch_action(action.boxed_clone(), cx);
                })
                .key(command.key_label().map(Into::into))
                .checked(checked)
            };
            vec![
                entry(Box::new(ZoomToFit), view.is_some_and(ViewState::is_fit)),
                entry(
                    Box::new(ActualPixels),
                    view.is_some_and(ViewState::is_actual_size),
                ),
            ]
        })
    }

    /// The most important thing to say about the open photo.
    fn status_message(&self, cx: &Context<Self>) -> (String, Hsla) {
        let theme = theme();
        let Some(document) = self.session.read(cx).document() else {
            return (String::new(), theme.muted);
        };
        let busy = document
            .export_status
            .as_ref()
            .map(|export| &export.detail)
            .or(document.open_status.as_ref())
            .or(document.preview.status.as_ref().map(|(_, detail)| detail));
        if let Some(error) = &document.error {
            (error.clone(), theme.error)
        } else if let Some(busy) = busy {
            (format!("{busy}…"), theme.muted)
        } else if let Some(warning) = &document.warning {
            (warning.clone(), theme.warning)
        } else {
            (String::new(), theme.muted)
        }
    }
}

fn window_controls() -> impl IntoElement {
    div()
        .flex()
        .items_center()
        .gap_1()
        .pl_2()
        .child(icon_button(
            "minimize",
            Icon::Minimize,
            "Minimize",
            true,
            |_, window, _| {
                window.minimize_window();
            },
        ))
        .child(icon_button(
            "maximize",
            Icon::Maximize,
            "Maximize",
            true,
            |_, window, _| {
                window.zoom_window();
            },
        ))
        .child(command_button("close-window", Icon::Close, Quit, true))
}

/// Grab areas along the window border, where the desktop does not provide them.
pub(super) fn resize_edges(window: &Window) -> Vec<Div> {
    let client = matches!(window.window_decorations(), Decorations::Client { .. });
    if !client || window.is_maximized() || window.is_fullscreen() {
        return Vec::new();
    }
    let edge = |edge: ResizeEdge, cursor: CursorStyle| {
        div()
            .absolute()
            .cursor(cursor)
            .on_mouse_down(MouseButton::Left, move |_, window, cx| {
                window.start_window_resize(edge);
                cx.stop_propagation();
            })
    };
    let m = RESIZE_MARGIN;
    vec![
        edge(ResizeEdge::Top, CursorStyle::ResizeUpDown)
            .top_0()
            .left_0()
            .right_0()
            .h(m),
        edge(ResizeEdge::Bottom, CursorStyle::ResizeUpDown)
            .bottom_0()
            .left_0()
            .right_0()
            .h(m),
        edge(ResizeEdge::Left, CursorStyle::ResizeLeftRight)
            .left_0()
            .top_0()
            .bottom_0()
            .w(m),
        edge(ResizeEdge::Right, CursorStyle::ResizeLeftRight)
            .right_0()
            .top_0()
            .bottom_0()
            .w(m),
        edge(ResizeEdge::BottomRight, CursorStyle::ResizeUpLeftDownRight)
            .right_0()
            .bottom_0()
            .size(m * 2.0),
    ]
}
