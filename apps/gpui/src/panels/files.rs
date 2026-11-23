//! Left dock: the photos of the open folder.

use std::path::{Path, PathBuf};

use gpui::{
    ClipboardItem, Context, Corners, Entity, IntoElement, MouseButton, MouseDownEvent, Pixels,
    Point, Render, Role, Window, canvas, div, prelude::*, px, uniform_list,
};

use crate::actions::OpenFolder;
use crate::assets::Icon;
use crate::library::Library;
use crate::session::Session;
use crate::theme::{FILES_WIDTH, RADIUS, TAB_HEIGHT, theme};
use crate::ui::button::icon;
use crate::ui::menu::MenuEntry;
use crate::ui::overlays::Overlays;
use crate::ui::test_id::TestId as _;

pub struct FilesPanel {
    session: Entity<Session>,
    library: Entity<Library>,
    overlays: Entity<Overlays>,
    /// The open photo, which is highlighted.
    open: Option<PathBuf>,
    #[cfg(test)]
    pub renders: usize,
}

impl FilesPanel {
    pub fn new(
        session: Entity<Session>,
        library: Entity<Library>,
        overlays: Entity<Overlays>,
        cx: &mut Context<Self>,
    ) -> Self {
        cx.observe(&session, |panel, session, cx| {
            let open = session.read(cx).document().map(|document| &document.path);
            if panel.open.as_ref() != open {
                panel.open = open.cloned();
                cx.notify();
            }
        })
        .detach();
        cx.observe(&library, |_, _, cx| cx.notify()).detach();
        Self {
            session,
            library,
            overlays,
            open: None,
            #[cfg(test)]
            renders: 0,
        }
    }

    fn show_menu(
        &self,
        path: &Path,
        at: Point<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let (reveal, copy) = (path.to_path_buf(), path.display().to_string());
        let entries = vec![
            MenuEntry::new("Show in File Manager", move |_, cx| cx.reveal_path(&reveal)),
            MenuEntry::new("Copy Path", move |_, cx| {
                cx.write_to_clipboard(ClipboardItem::new_string(copy.clone()));
            }),
        ];
        self.overlays.update(cx, |overlays, cx| {
            overlays.show_menu(at, entries, window, cx);
        });
    }

    fn row(&self, position: usize, cx: &mut Context<Self>) -> Option<impl IntoElement + use<>> {
        let theme = theme();
        let library = self.library.read(cx);
        let path = library.path(position)?.to_path_buf();
        let name = library.name(position)?.to_owned();
        let open = self.open.as_ref() == Some(&path);
        let thumbnail = library.thumbnail(&path).map(|t| t.image().clone());
        let thumbnail = match thumbnail {
            Some(image) => canvas(
                |_, _, _| (),
                move |bounds, (), window, _| {
                    let pixels = image.size(0);
                    let (width, height) = (pixels.width.0 as f32, pixels.height.0 as f32);
                    let scale = (bounds.size.width.as_f32() / width)
                        .min(bounds.size.height.as_f32() / height);
                    let target = gpui::Bounds::centered_at(
                        bounds.center(),
                        gpui::size(px(width * scale), px(height * scale)),
                    );
                    let corners = Corners::all(px(2.0));
                    // Fails only when the atlas cannot grow; the row stays usable.
                    let _ = window.paint_image(target, target, corners, image, 0, false);
                },
            )
            .w(px(44.0))
            .h(px(30.0))
            .flex_none()
            .into_any_element(),
            None => div()
                .w(px(44.0))
                .h(px(30.0))
                .flex()
                .flex_none()
                .items_center()
                .justify_center()
                .child(icon(Icon::Image, theme.faint))
                .into_any_element(),
        };
        let row = div()
            .id(position)
            .role(Role::ListItem)
            .aria_label(name.clone())
            .aria_selected(open)
            .flex()
            .items_center()
            .gap_2()
            .h(px(36.0))
            .w_full()
            .px_2()
            .rounded(RADIUS)
            .cursor_pointer()
            .text_color(if open { theme.text } else { theme.muted })
            .child(thumbnail)
            .test_id(format!("file:{name}"))
            .child(div().min_w_0().truncate().child(name))
            .on_mouse_down(
                MouseButton::Right,
                cx.listener({
                    let path = path.clone();
                    move |this, event: &MouseDownEvent, window, cx| {
                        this.show_menu(&path, event.position, window, cx);
                    }
                }),
            )
            .on_click(cx.listener(move |this, _, _, cx| {
                let path = path.clone();
                this.session
                    .update(cx, |session, cx| session.open_file(path, cx));
            }));
        let row = if open {
            row.bg(theme.selected)
        } else {
            row.hover(move |style| style.bg(theme.hover))
        };
        Some(div().w_full().px_1().child(row))
    }
}

impl Render for FilesPanel {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        #[cfg(test)]
        {
            self.renders += 1;
        }
        let theme = theme();
        let library = self.library.read(cx);
        let count = library.len();
        let folder = library.folder_name();
        let empty = match (&folder, library.failure()) {
            (_, Some(failure)) => Some(failure.to_owned()),
            (Some(_), None) if count == 0 && !library.scanning() => {
                Some("No RAW files in this folder.".to_owned())
            }
            _ => None,
        };
        let list = uniform_list(
            "files",
            count,
            cx.processor(|this: &mut Self, range: std::ops::Range<usize>, _, cx| {
                // Deferred: the library must not change while the panel renders.
                let library = this.library.clone();
                let (start, end) = (range.start, range.end);
                // The list also asks for its first row alone, to measure it.
                if range != (0..1) {
                    cx.defer(move |cx| {
                        library.update(cx, |library, cx| library.set_visible(start, end, cx));
                    });
                }
                range
                    .filter_map(|index| this.row(index, cx))
                    .collect::<Vec<_>>()
            }),
        )
        .flex_1()
        .py_1();
        div()
            .flex()
            .flex_col()
            .flex_none()
            .w(FILES_WIDTH)
            .h_full()
            .bg(theme.panel)
            .border_r_1()
            .border_color(theme.border)
            .child(
                div()
                    .id("folder")
                    .flex()
                    .flex_none()
                    .items_center()
                    .gap_2()
                    .h(TAB_HEIGHT)
                    .px_3()
                    .border_b_1()
                    .border_color(theme.border)
                    .cursor_pointer()
                    .hover(move |style| style.bg(theme.hover))
                    .child(icon(Icon::Files, theme.muted))
                    .child(
                        div()
                            .min_w_0()
                            .flex_1()
                            .truncate()
                            .child(folder.clone().unwrap_or_else(|| "Open Folder…".to_owned())),
                    )
                    .when(count > 0, |header| {
                        header.child(div().text_color(theme.faint).child(count.to_string()))
                    })
                    .on_click(|_, window, cx| window.dispatch_action(Box::new(OpenFolder), cx)),
            )
            .children(empty.map(|text| div().p_3().text_color(theme.faint).child(text)))
            .child(list)
    }
}
