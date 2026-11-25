//! Left dock: the albums.

mod commands;

use gpui::{
    App, Context, FocusHandle, IntoElement, KeyBinding, MouseButton, MouseDownEvent, Pixels, Point,
    Render, Role, ScrollStrategy, SharedString, UniformListScrollHandle, Window, actions, div,
    prelude::*, px, uniform_list,
};

pub use commands::AlbumCommands;

use crate::actions::NewAlbum;
use crate::assets::Icon;
use crate::theme::{RADIUS, SMALL_FONT_SIZE, theme};
use crate::ui::button::{button, icon};
use crate::ui::keys::{Confirm, SelectNext, SelectPrevious};
use crate::ui::menu::MenuEntry;
use crate::ui::test_id::TestId as _;

const CONTEXT: &str = "Albums";
const ROW_HEIGHT: Pixels = px(24.0);

actions!(albums, [RenameAlbum, DeleteAlbum]);

pub fn bind_keys(cx: &mut App) {
    let context = Some(CONTEXT);
    cx.bind_keys([
        KeyBinding::new("down", SelectNext, context),
        KeyBinding::new("up", SelectPrevious, context),
        KeyBinding::new("enter", Confirm, context),
        KeyBinding::new("f2", RenameAlbum, context),
        KeyBinding::new("delete", DeleteAlbum, context),
    ]);
}

pub struct AlbumsPanel {
    commands: AlbumCommands,
    /// The album the keys are about.
    cursor: Option<String>,
    focus: FocusHandle,
    scroll: UniformListScrollHandle,
}

impl AlbumsPanel {
    pub fn new(commands: AlbumCommands, cx: &mut Context<Self>) -> Self {
        cx.observe(&commands.library, |_, _, cx| cx.notify())
            .detach();
        Self {
            commands,
            cursor: None,
            focus: cx.focus_handle().tab_stop(true),
            scroll: UniformListScrollHandle::new(),
        }
    }

    fn show(&mut self, album: &str, cx: &mut Context<Self>) {
        self.cursor = Some(album.to_owned());
        self.commands
            .library
            .update(cx, |library, cx| library.show_album(album, cx));
        cx.notify();
    }

    /// Where the cursor is, unless its album is gone.
    fn cursor_row(&self, cx: &App) -> Option<usize> {
        let albums = self.commands.library.read(cx).albums();
        let cursor = self.cursor.as_deref()?;
        albums.iter().position(|album| album.name() == cursor)
    }

    fn step(&mut self, forward: bool, cx: &mut Context<Self>) {
        let row = match self.cursor_row(cx) {
            None => 0,
            Some(row) if forward => row + 1,
            Some(row) => row.saturating_sub(1),
        };
        let albums = self.commands.library.read(cx).albums();
        let Some(album) = albums.get(row) else {
            return;
        };
        self.cursor = Some(album.name().to_owned());
        self.scroll.scroll_to_item(row, ScrollStrategy::Nearest);
        cx.notify();
    }

    /// The album at the cursor.
    fn current(&self, cx: &App) -> Option<String> {
        self.cursor_row(cx).and_then(|_| self.cursor.clone())
    }

    fn show_menu(
        &self,
        album: &str,
        at: Point<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let entry = |label, run: fn(&AlbumCommands, String, &mut Window, &mut App)| {
            let (commands, album) = (self.commands.clone(), album.to_owned());
            MenuEntry::new(label, move |window, cx| {
                run(&commands, album.clone(), window, cx);
            })
        };
        let entries = vec![
            entry("Rename…", AlbumCommands::rename).key(Some("F2".into())),
            entry("Delete…", AlbumCommands::delete).key(Some("Delete".into())),
        ];
        self.commands.overlays.update(cx, |overlays, cx| {
            overlays.show_menu(at, entries, window, cx);
        });
    }

    fn row(
        &self,
        index: usize,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> Option<impl IntoElement + use<>> {
        let theme = theme();
        let library = self.commands.library.read(cx);
        let album = library.albums().get(index)?;
        let name = SharedString::from(album.name().to_owned());
        let count = album.photos().len();
        let shown = library.shown_album() == Some(album.name());
        let at_cursor =
            self.focus.is_focused(window) && self.cursor.as_deref() == Some(album.name());
        let (clicked, opened) = (name.clone(), name.clone());
        let row = div()
            .id(index)
            .role(Role::ListItem)
            .aria_label(name.clone())
            .aria_selected(shown)
            .relative()
            .flex()
            .items_center()
            .gap_2()
            .h(ROW_HEIGHT)
            .w_full()
            .px_2()
            .rounded(RADIUS)
            .border_1()
            .border_color(if at_cursor {
                theme.accent
            } else {
                gpui::transparent_black()
            })
            .cursor_pointer()
            .text_color(if shown { theme.text } else { theme.muted })
            .child(icon(
                Icon::Album,
                if shown { theme.accent } else { theme.muted },
            ))
            .child(div().flex_1().min_w_0().truncate().child(name.clone()))
            .child(
                div()
                    .flex_none()
                    .text_size(SMALL_FONT_SIZE)
                    .text_color(theme.faint)
                    .child(count.to_string()),
            )
            .test_id(format!("album:{name}"))
            .on_click(cx.listener(move |this, _, _, cx| this.show(&clicked, cx)))
            .on_mouse_down(
                MouseButton::Right,
                cx.listener(move |this, event: &MouseDownEvent, window, cx| {
                    this.show_menu(&opened, event.position, window, cx);
                }),
            );
        let row = if shown {
            row.bg(theme.selected)
        } else {
            row.hover(move |style| style.bg(theme.hover))
        };
        Some(div().w_full().px_1().child(row))
    }

    /// Instead of the list while there are no albums.
    fn empty(&self) -> impl IntoElement + use<> {
        div()
            .flex()
            .flex_col()
            .items_start()
            .gap_2()
            .p_3()
            .text_color(theme().faint)
            .child("No albums yet.")
            .child(button("new-album", "New Album…", |_, window, cx| {
                window.dispatch_action(Box::new(NewAlbum), cx);
            }))
    }

    fn list(&self, count: usize, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let list = uniform_list(
            "albums",
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
            .id("album-list")
            .role(Role::List)
            .aria_label("Albums")
            .key_context(CONTEXT)
            .track_focus(&self.focus)
            // A click leaves the arrow keys to the photo.
            .on_mouse_down(MouseButton::Left, |_, window, _| window.prevent_default())
            .on_action(cx.listener(|this, _: &SelectNext, _, cx| this.step(true, cx)))
            .on_action(cx.listener(|this, _: &SelectPrevious, _, cx| this.step(false, cx)))
            .on_action(cx.listener(|this, _: &Confirm, _, cx| {
                if let Some(album) = this.current(cx) {
                    this.show(&album, cx);
                }
            }))
            .on_action(cx.listener(|this, _: &RenameAlbum, window, cx| {
                if let Some(album) = this.current(cx) {
                    this.commands.rename(album, window, cx);
                }
            }))
            .on_action(cx.listener(|this, _: &DeleteAlbum, window, cx| {
                if let Some(album) = this.current(cx) {
                    this.commands.delete(album, window, cx);
                }
            }))
            .flex_1()
            .min_h_0()
            .child(list)
    }
}

impl Render for AlbumsPanel {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = theme();
        let library = self.commands.library.read(cx);
        let count = library.albums().len();
        let warnings = library.album_warnings().to_vec();
        div()
            .flex()
            .flex_col()
            .size_full()
            .child(if count == 0 {
                self.empty().into_any_element()
            } else {
                self.list(count, cx).into_any_element()
            })
            .children(warnings.into_iter().map(|text| {
                div()
                    .flex_none()
                    .p_2()
                    .border_t_1()
                    .border_color(theme.border)
                    .text_size(SMALL_FONT_SIZE)
                    .text_color(theme.warning)
                    .child(text)
            }))
    }
}
