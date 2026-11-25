//! Bottom dock: the thumbnails of the folder or album shown.

mod thumbnail;

use std::path::PathBuf;

use gpui::{
    Context, Entity, IntoElement, MouseButton, Pixels, Render, Role, Window, div, prelude::*, px,
};
use rohditor_engine::catalog::CatalogSource;
use rohditor_engine::catalog::library::LibrarySort;

use crate::actions::{SortPhotos, ToggleFilmstrip};
use crate::assets::Icon;
use crate::library::Library;
use crate::session::Session;
use crate::theme::{FILMSTRIP_HEIGHT, SMALL_FONT_SIZE, theme};
use crate::ui::button::command_button;
use crate::ui::dropdown::dropdown;
use crate::ui::menu::MenuEntry;
use crate::ui::overlays::Overlays;
use crate::ui::test_id::TestId as _;
use crate::ui::virtual_row::{VirtualRowScroll, virtual_row};

const HEADER_HEIGHT: Pixels = px(28.0);
/// The distance from one thumbnail to the next.
const CELL_WIDTH: Pixels = px(126.0);

pub struct FilmstripPanel {
    session: Entity<Session>,
    library: Entity<Library>,
    overlays: Entity<Overlays>,
    /// The open photo, which stands out.
    open: Option<PathBuf>,
    scroll: VirtualRowScroll,
    /// What the row was last scrolled for.
    revealed: Option<Revealed>,
}

/// The row scrolls to the open photo when any of these changes.
struct Revealed {
    open: Option<PathBuf>,
    sort: LibrarySort,
    source: CatalogSource,
}

impl FilmstripPanel {
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
            scroll: VirtualRowScroll::default(),
            revealed: None,
        }
    }

    /// The panel is shown from now on.
    pub fn opened(&mut self, cx: &mut Context<Self>) {
        self.revealed = None;
        cx.notify();
    }

    /// Scroll to the open photo if it, the order or what is shown changed.
    fn follow(&mut self, cx: &mut Context<Self>) {
        let library = self.library.read(cx);
        let Some(source) = library.source().filter(|_| !library.scanning()) else {
            return;
        };
        let last = self.revealed.as_ref();
        let same_source = last.is_some_and(|last| last.source.is_same_place(source));
        if same_source
            && last.is_some_and(|last| last.open == self.open && last.sort == library.sort())
        {
            return;
        }
        let open = self.open.as_deref();
        let done = match open.and_then(|open| library.position(open)) {
            Some(position) => self.scroll.reveal(position, library.len(), CELL_WIDTH),
            None => {
                if !same_source {
                    self.scroll.to_start();
                }
                true
            }
        };
        if done {
            self.revealed = Some(Revealed {
                open: self.open.clone(),
                sort: library.sort(),
                source: source.clone(),
            });
        } else {
            // Again once the row was laid out.
            let this = cx.entity();
            cx.defer(move |cx| this.update(cx, |_, cx| cx.notify()));
        }
    }

    fn open_photo(&self, photo: PathBuf, cx: &mut Context<Self>) {
        self.session
            .update(cx, |session, cx| session.open_file(photo, cx));
    }

    fn header(&self, cx: &Context<Self>) -> impl IntoElement + use<> {
        let theme = theme();
        let library = self.library.read(cx);
        let count = library.len();
        let selected = library.selected_count();
        let sort = library.sort();
        let title = library.source().map(CatalogSource::title);
        let photos = if count == 1 { "photo" } else { "photos" };
        let faint = |text: String| div().flex_none().text_color(theme.faint).child(text);
        div()
            .flex()
            .flex_none()
            .items_center()
            .gap_2()
            .h(HEADER_HEIGHT)
            .pl_3()
            .pr_1()
            .text_size(SMALL_FONT_SIZE)
            .child(div().min_w_0().truncate().children(title))
            .when(count > 0, |header| {
                header.child(faint(format!("{count} {photos}")))
            })
            .when(selected > 1, |header| {
                let label = format!("{selected} selected");
                header.child(
                    div()
                        .relative()
                        .flex_none()
                        .text_color(theme.accent)
                        .child(label)
                        .test_id(format!("selected:{selected}")),
                )
            })
            .child(div().flex_1())
            .child(faint("Sort by".to_owned()))
            .child(dropdown(
                "sort",
                "Sort by",
                sort.label(),
                self.overlays.clone(),
                move |_, _| {
                    let entry = |order: LibrarySort| {
                        MenuEntry::new(order.label(), move |window, cx| {
                            window.dispatch_action(Box::new(SortPhotos(order)), cx);
                        })
                        .checked(order == sort)
                    };
                    LibrarySort::ALL.into_iter().map(entry).collect()
                },
            ))
            .child(command_button(
                "close-filmstrip",
                Icon::Close,
                ToggleFilmstrip,
                true,
            ))
    }

    /// What to say instead of thumbnails while there are none.
    fn message(&self, cx: &Context<Self>) -> String {
        let library = self.library.read(cx);
        if let Some(failure) = library.failure() {
            return failure.to_owned();
        }
        let text = match library.source() {
            None => "Open a folder to see its photos here.",
            Some(_) if library.scanning() => "Looking for photos…",
            Some(CatalogSource::Folder(_)) => "No RAW files in this folder.",
            Some(CatalogSource::Album { .. }) => "No photos in this album.",
        };
        text.to_owned()
    }

    fn strip(
        &self,
        count: usize,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        let visible = self.scroll.visible(count, CELL_WIDTH, window);
        // Deferred: the library must not change while the panel renders.
        let library = self.library.clone();
        cx.defer(move |cx| {
            library.update(cx, |library, cx| {
                library.set_visible(Some((visible.start, visible.end)), cx);
            });
        });
        let cells: Vec<_> = visible
            .filter_map(|position| Some((position, self.thumbnail(position, cx)?)))
            .collect();
        virtual_row("thumbnails", &self.scroll, count, CELL_WIDTH, cells)
    }
}

impl Render for FilmstripPanel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = theme();
        let count = self.library.read(cx).len();
        self.follow(cx);
        div()
            .id("filmstrip")
            .role(Role::List)
            .aria_label("Filmstrip")
            // A click leaves the arrow keys to the photo.
            .on_mouse_down(MouseButton::Left, |_, window, _| window.prevent_default())
            .relative()
            .flex()
            .flex_col()
            .w_full()
            .h(FILMSTRIP_HEIGHT)
            .bg(theme.panel)
            .border_t_1()
            .border_color(theme.border)
            .child(self.header(cx))
            .child(div().flex_1().min_h_0().child(if count == 0 {
                div()
                    .px_3()
                    .py_1()
                    .text_color(theme.faint)
                    .child(self.message(cx))
                    .into_any_element()
            } else {
                self.strip(count, window, cx).into_any_element()
            }))
            .test_id("filmstrip")
    }
}
