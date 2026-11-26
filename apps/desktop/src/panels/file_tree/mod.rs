//! Left dock: the root folders, their subfolders and photos.

mod row;

use std::path::{Path, PathBuf};

use gpui::{
    App, Context, Entity, FocusHandle, IntoElement, KeyBinding, MouseButton, Pixels, Point, Render,
    Role, ScrollStrategy, UniformListScrollHandle, Window, actions, div, prelude::*, uniform_list,
};
use rohditor_engine::catalog::folder_tree::{TreeNavigation, TreeRowKind};

use crate::actions::{OpenFolder, RefreshFolders};
use crate::library::Library;
use crate::panels::photo_menu::{add_to_album, path_entries};
use crate::session::Session;
use crate::theme::{SMALL_FONT_SIZE, theme};
use crate::ui::button::button;
use crate::ui::keys::{Confirm, SelectNext, SelectPrevious};
use crate::ui::menu::MenuEntry;
use crate::ui::overlays::Overlays;
use crate::ui::test_id::TestId as _;

const CONTEXT: &str = "FileTree";

actions!(file_tree, [Leave, Enter, First, Last]);

pub fn bind_keys(cx: &mut App) {
    let context = Some(CONTEXT);
    cx.bind_keys([
        KeyBinding::new("down", SelectNext, context),
        KeyBinding::new("up", SelectPrevious, context),
        KeyBinding::new("left", Leave, context),
        KeyBinding::new("right", Enter, context),
        KeyBinding::new("home", First, context),
        KeyBinding::new("end", Last, context),
        KeyBinding::new("enter", Confirm, context),
    ]);
}

pub struct FileTreePanel {
    session: Entity<Session>,
    library: Entity<Library>,
    overlays: Entity<Overlays>,
    /// The open photo, which stands out.
    open: Option<PathBuf>,
    focus: FocusHandle,
    scroll: UniformListScrollHandle,
    #[cfg(test)]
    pub renders: usize,
}

impl FileTreePanel {
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
        cx.observe(&library, |panel, library, cx| {
            let revealed = library.update(cx, |library, _| library.take_revealed());
            if let Some(row) = revealed {
                panel.scroll.scroll_to_item(row, ScrollStrategy::Center);
            }
            cx.notify();
        })
        .detach();
        Self {
            session,
            library,
            overlays,
            open: None,
            focus: cx.focus_handle().tab_stop(true),
            scroll: UniformListScrollHandle::new(),
            #[cfg(test)]
            renders: 0,
        }
    }

    fn open_photo(&self, photo: PathBuf, cx: &mut Context<Self>) {
        self.session
            .update(cx, |session, cx| session.open_file(photo, cx));
    }

    fn navigate(&mut self, step: TreeNavigation, cx: &mut Context<Self>) {
        let row = self.library.update(cx, |library, cx| {
            library.change_tree(|tree| tree.navigate(step), cx)
        });
        if let Some(row) = row {
            self.scroll.scroll_to_item(row, ScrollStrategy::Nearest);
        }
    }

    /// What a click on the cursor's row does.
    fn confirm(&mut self, cx: &mut Context<Self>) {
        let tree = self.library.read(cx).tree();
        let row = tree.cursor().and_then(|path| tree.row_of(path));
        let Some(row) = row.map(|row| tree.rows()[row].clone()) else {
            return;
        };
        if row.is_photo() {
            self.library.update(cx, |library, cx| {
                library.click_photo(&row.path, gpui::Modifiers::default(), cx)
            });
            self.open_photo(row.path, cx);
        } else {
            self.library
                .update(cx, |library, cx| library.click_folder(row.path, cx));
        }
    }

    fn show_menu(
        &self,
        path: &Path,
        kind: &TreeRowKind,
        at: Point<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let mut entries = Vec::new();
        if matches!(kind, TreeRowKind::Photo) {
            self.library
                .update(cx, |library, cx| library.select_for_menu(path, cx));
            entries.push(add_to_album());
        }
        entries.extend(path_entries(path));
        if matches!(kind, TreeRowKind::Folder { .. }) {
            entries.push(MenuEntry::command(RefreshFolders));
        }
        let roots = self.library.read(cx).tree().roots();
        if let Some(root) = roots.iter().find(|root| *root == path).cloned() {
            let library = self.library.clone();
            entries.push(MenuEntry::new("Remove from Files", move |_, cx| {
                library.update(cx, |library, cx| library.remove_root(&root, cx));
            }));
        }
        self.overlays.update(cx, |overlays, cx| {
            overlays.show_menu(at, entries, window, cx);
        });
    }

    /// Instead of the tree while there are no roots.
    fn empty(&self) -> impl IntoElement + use<> {
        div()
            .flex()
            .flex_col()
            .items_start()
            .gap_2()
            .p_3()
            .text_color(theme().faint)
            .child("No folders yet.")
            .child(button("open-folder", "Open Folder…", |_, window, cx| {
                window.dispatch_action(Box::new(OpenFolder), cx);
            }))
    }

    fn tree(&self, count: usize, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let list = uniform_list(
            "rows",
            count,
            cx.processor(
                |this: &mut Self, range: std::ops::Range<usize>, window, cx| {
                    range
                        .filter_map(|index| this.row(index, window, cx))
                        .collect::<Vec<_>>()
                },
            ),
        )
        .track_scroll(&self.scroll)
        .size_full()
        .py_1();
        div()
            .id("file-tree")
            .role(Role::Tree)
            .aria_label("Files")
            .key_context(CONTEXT)
            .track_focus(&self.focus)
            // A click leaves the arrow keys to the photo.
            .on_mouse_down(MouseButton::Left, |_, window, _| window.prevent_default())
            .on_action(cx.listener(|this, _: &SelectNext, _, cx| {
                this.navigate(TreeNavigation::Down, cx);
            }))
            .on_action(cx.listener(|this, _: &SelectPrevious, _, cx| {
                this.navigate(TreeNavigation::Up, cx);
            }))
            .on_action(cx.listener(|this, _: &Leave, _, cx| {
                this.navigate(TreeNavigation::Left, cx);
            }))
            .on_action(cx.listener(|this, _: &Enter, _, cx| {
                this.navigate(TreeNavigation::Right, cx);
            }))
            .on_action(cx.listener(|this, _: &First, _, cx| {
                this.navigate(TreeNavigation::Home, cx);
            }))
            .on_action(cx.listener(|this, _: &Last, _, cx| {
                this.navigate(TreeNavigation::End, cx);
            }))
            .on_action(cx.listener(|this, _: &Confirm, _, cx| this.confirm(cx)))
            .relative()
            .flex_1()
            .min_h_0()
            .child(list)
            .test_id("file-tree")
    }
}

impl Render for FileTreePanel {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        #[cfg(test)]
        {
            self.renders += 1;
        }
        let theme = theme();
        let library = self.library.read(cx);
        let count = library.tree().rows().len();
        let failure = library.failure().map(str::to_owned);
        div()
            .flex()
            .flex_col()
            .size_full()
            .child(if count == 0 {
                self.empty().into_any_element()
            } else {
                self.tree(count, cx).into_any_element()
            })
            .children(failure.map(|text| {
                div()
                    .flex_none()
                    .p_2()
                    .border_t_1()
                    .border_color(theme.border)
                    .text_size(SMALL_FONT_SIZE)
                    .text_color(theme.error)
                    .child(text)
            }))
    }
}
