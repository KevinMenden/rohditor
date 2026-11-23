//! Left dock: the photos of the open folder.

use gpui::{
    Context, Corners, Entity, IntoElement, Render, Window, canvas, div, prelude::*, px,
    uniform_list,
};

use crate::actions::OpenFolder;
use crate::assets::Icon;
use crate::session::Session;
use crate::theme::{FILES_WIDTH, RADIUS, TAB_HEIGHT, theme};
use crate::ui::button::icon;

pub struct FilesPanel {
    session: Entity<Session>,
}

impl FilesPanel {
    pub fn new(session: Entity<Session>, cx: &mut Context<Self>) -> Self {
        cx.observe(&session, |_, _, cx| cx.notify()).detach();
        Self { session }
    }

    fn row(&self, grid_index: usize, cx: &mut Context<Self>) -> Option<impl IntoElement + use<>> {
        let theme = theme();
        let session = self.session.read(cx);
        let index = session.library.catalog_index(grid_index)?;
        let path = session.catalog.entry_path(index)?.to_path_buf();
        let name = session.catalog.entry_name(index)?.to_owned();
        let open = session
            .document()
            .is_some_and(|document| document.path == path);
        let thumbnail = session.library.texture(&path).map(|t| t.image().clone());
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
            .id(grid_index)
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
            .child(div().min_w_0().truncate().child(name))
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
        let theme = theme();
        let session = self.session.read(cx);
        let count = session.library.order().len();
        let folder = session.catalog.folder_name();
        let empty = match (&folder, session.catalog.failure()) {
            (_, Some(failure)) => Some(failure.to_owned()),
            (Some(_), None) if count == 0 && !session.catalog.scanning() => {
                Some("No RAW files in this folder.".to_owned())
            }
            _ => None,
        };
        let list = uniform_list(
            "files",
            count,
            cx.processor(|this: &mut Self, range: std::ops::Range<usize>, _, cx| {
                // Deferred: the session must not change while the panel renders.
                let session = this.session.clone();
                let (start, end) = (range.start, range.end);
                // The list also asks for its first row alone, to measure it.
                if range != (0..1) {
                    cx.defer(move |cx| {
                        session.update(cx, |session, cx| session.set_visible_files(start, end, cx));
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
