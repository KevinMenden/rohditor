//! One line of the tree.

use gpui::{
    ClickEvent, Context, Hsla, IntoElement, MouseButton, MouseDownEvent, Role, Window, div,
    prelude::*, px,
};
use rohditor_engine::catalog::folder_tree::TreeRowKind;

use super::FileTreePanel;
use crate::assets::Icon;
use crate::theme::{RADIUS, theme};
use crate::ui::button::icon;
use crate::ui::test_id::TestId as _;
use crate::ui::tooltip::tooltip;

const HEIGHT: f32 = 24.0;
const INDENT: f32 = 14.0;
const CHEVRON: f32 = 16.0;

impl FileTreePanel {
    pub(super) fn row(
        &self,
        index: usize,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> Option<impl IntoElement + use<>> {
        let theme = theme();
        let library = self.library.read(cx);
        let tree = library.tree();
        let row = tree.rows().get(index)?.clone();
        let at_cursor = self.focus.is_focused(window) && tree.cursor() == Some(&row.path);
        let selected = library.is_selected(&row.path);
        let open = self.open.as_ref() == Some(&row.path);
        let shown = library.folder() == Some(&row.path);
        let bright = open || shown || selected;
        let (name, path) = (row.name.clone(), row.path.clone());
        let (glyph, color, selector) = match &row.kind {
            TreeRowKind::Folder { failure, .. } => {
                let color = match failure {
                    Some(_) => theme.error,
                    None if shown => theme.accent,
                    None => theme.muted,
                };
                (Icon::Files, color, format!("folder:{name}"))
            }
            TreeRowKind::Photo => {
                let color = if open { theme.accent } else { theme.faint };
                (Icon::Image, color, format!("file:{name}"))
            }
        };
        let failure = match &row.kind {
            TreeRowKind::Folder { failure, .. } => failure.clone(),
            TreeRowKind::Photo => None,
        };
        let expanded = match row.kind {
            TreeRowKind::Folder { expanded, .. } => Some(expanded),
            TreeRowKind::Photo => None,
        };
        let element = div()
            .id(index)
            .role(Role::TreeItem)
            .aria_label(name.clone())
            .aria_selected(selected)
            .when_some(expanded, |row, expanded| row.aria_expanded(expanded))
            .relative()
            .flex()
            .items_center()
            .gap_1()
            .h(px(HEIGHT))
            .w_full()
            .pl(px(4.0 + row.depth as f32 * INDENT))
            .pr_2()
            .rounded(RADIUS)
            .border_1()
            .border_color(if at_cursor {
                theme.accent
            } else {
                gpui::transparent_black()
            })
            .cursor_pointer()
            .text_color(if bright { theme.text } else { theme.muted })
            .child(self.chevron(expanded, &row.path, theme.faint, cx))
            .child(icon(glyph, color))
            .child(div().min_w_0().truncate().child(name))
            .test_id(selector)
            .when_some(failure, |row, failure| row.tooltip(tooltip(failure, None)))
            .on_mouse_down(
                MouseButton::Right,
                cx.listener(move |this, event: &MouseDownEvent, window, cx| {
                    this.show_menu(&path, &row.kind, event.position, window, cx);
                }),
            );
        let path = row.path;
        let element = if expanded.is_some() {
            element.on_click(cx.listener(move |this, _, _, cx| {
                let folder = path.clone();
                this.library
                    .update(cx, |library, cx| library.click_folder(folder, cx));
            }))
        } else {
            element.on_click(cx.listener(move |this, event: &ClickEvent, _, cx| {
                let open = this.library.update(cx, |library, cx| {
                    library.click_photo(&path, event.modifiers(), cx)
                });
                if open {
                    this.open_photo(path.clone(), cx);
                }
            }))
        };
        let element = if selected {
            element.bg(theme.selected)
        } else {
            element.hover(move |style| style.bg(theme.hover))
        };
        Some(div().w_full().px_1().child(element))
    }

    /// Opens and closes a folder without showing it. Empty space for a photo.
    fn chevron(
        &self,
        expanded: Option<bool>,
        folder: &std::path::Path,
        color: Hsla,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        let space = div().id("chevron").size(px(CHEVRON)).flex_none();
        let Some(expanded) = expanded else {
            return space;
        };
        let folder = folder.to_path_buf();
        let glyph = if expanded {
            Icon::ChevronDown
        } else {
            Icon::ChevronRight
        };
        space
            .relative()
            .child(icon(glyph, color))
            .test_id(format!(
                "chevron:{}",
                folder.file_name().unwrap_or_default().to_string_lossy()
            ))
            .on_click(cx.listener(move |this, _, _, cx| {
                cx.stop_propagation();
                this.library.update(cx, |library, cx| {
                    library.change_tree(|tree| tree.toggle(&folder), cx);
                });
            }))
    }
}
