//! One thumbnail of the strip.

use std::path::Path;
use std::sync::Arc;

use gpui::{
    AnyElement, Bounds, ClickEvent, Context, Corners, IntoElement, MouseButton, MouseDownEvent,
    Pixels, Point, RenderImage, Role, SharedString, Window, canvas, div, prelude::*, px, size,
};

use super::FilmstripPanel;
use crate::assets::Icon;
use crate::panels::photo_menu::path_entries;
use crate::theme::{RADIUS, SMALL_FONT_SIZE, theme};
use crate::ui::button::icon;
use crate::ui::test_id::TestId as _;
use crate::ui::tooltip::tooltip;

const MISSING_OPACITY: f32 = 0.4;

impl FilmstripPanel {
    pub(super) fn thumbnail(&self, position: usize, cx: &mut Context<Self>) -> Option<AnyElement> {
        let theme = theme();
        let photo = self.library.read(cx).photo(position)?;
        let path = photo.path.to_path_buf();
        let name = SharedString::from(photo.name.to_owned());
        let (missing, selected) = (photo.missing, photo.selected);
        let open = self.open.as_deref() == Some(photo.path);
        let content = match photo.thumbnail {
            Some(thumbnail) => picture(Arc::clone(thumbnail.image())).into_any_element(),
            None => placeholder(name.clone(), !photo.pending).into_any_element(),
        };
        let hint = if missing {
            SharedString::from(format!("{name} is missing"))
        } else {
            name.clone()
        };
        let menu_path = path.clone();
        let cell = div()
            .id(position)
            .role(Role::ListItem)
            .aria_label(hint.clone())
            .aria_selected(selected)
            .relative()
            .size_full()
            .p_1()
            .rounded(RADIUS)
            .border_1()
            .border_color(if open {
                theme.accent
            } else {
                gpui::transparent_black()
            })
            .cursor_pointer()
            .when(missing, |cell| cell.opacity(MISSING_OPACITY))
            .child(content)
            .test_id(format!("thumbnail:{name}"))
            .tooltip(tooltip(hint, None))
            .on_mouse_down(
                MouseButton::Right,
                cx.listener(move |this, event: &MouseDownEvent, window, cx| {
                    this.show_menu(&menu_path, event.position, window, cx);
                }),
            )
            .on_click(cx.listener(move |this, event: &ClickEvent, _, cx| {
                let open = this.library.update(cx, |library, cx| {
                    library.click_shown_photo(&path, event.modifiers(), cx)
                });
                if open {
                    this.open_photo(path.clone(), cx);
                }
            }));
        let cell = if selected {
            cell.bg(theme.selected)
        } else {
            cell.hover(move |style| style.bg(theme.hover))
        };
        let padded = div().size_full().px(px(2.0)).pb_1().child(cell);
        Some(padded.into_any_element())
    }

    fn show_menu(
        &self,
        path: &Path,
        at: Point<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.overlays.update(cx, |overlays, cx| {
            overlays.show_menu(at, path_entries(path), window, cx);
        });
    }
}

/// The image as large as fits.
fn picture(image: Arc<RenderImage>) -> impl IntoElement {
    canvas(
        |_, _, _| (),
        move |bounds, (), window, _| {
            let pixels = image.size(0);
            let (width, height) = (pixels.width.0 as f32, pixels.height.0 as f32);
            let scale =
                (bounds.size.width.as_f32() / width).min(bounds.size.height.as_f32() / height);
            let target =
                Bounds::centered_at(bounds.center(), size(px(width * scale), px(height * scale)));
            let corners = Corners::all(px(2.0));
            // Fails only when the atlas cannot grow; the cell stays usable.
            let _ = window.paint_image(target, target, corners, image, 0, false);
        },
    )
    .size_full()
}

/// Instead of a thumbnail that is not ready, or that there is none of, which then has the name.
fn placeholder(name: SharedString, named: bool) -> impl IntoElement {
    let theme = theme();
    div()
        .size_full()
        .flex()
        .flex_col()
        .items_center()
        .justify_center()
        .gap_1()
        .child(icon(Icon::Image, theme.faint))
        .when(named, |cell| {
            cell.child(
                div()
                    .max_w_full()
                    .truncate()
                    .text_size(SMALL_FONT_SIZE)
                    .text_color(theme.muted)
                    .child(name),
            )
        })
}
