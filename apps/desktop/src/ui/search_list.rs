//! A dialog that lists things to choose from: type to filter, Enter to choose.

use std::ops::Range;
use std::rc::Rc;

use gpui::{
    App, Context, DismissEvent, Entity, EventEmitter, FocusHandle, Focusable, IntoElement, Render,
    Role, ScrollStrategy, SharedString, UniformListScrollHandle, Window, div, prelude::*, px,
    uniform_list,
};

use crate::assets::Icon;
use crate::theme::{RADIUS, SMALL_FONT_SIZE, theme};
use crate::ui::button::icon;
use crate::ui::keys::{Confirm, SelectNext, SelectPrevious};
use crate::ui::test_id::TestId as _;
use crate::ui::text_input::{Changed, TextInput};

const ROW: f32 = 28.0;
const MAX_HEIGHT: f32 = 400.0;

type Run = Rc<dyn Fn(&mut Window, &mut App)>;

pub struct SearchEntry {
    name: SharedString,
    /// Shown at the right end.
    note: Option<SharedString>,
    run: Run,
}

impl SearchEntry {
    pub fn new(
        name: impl Into<SharedString>,
        note: Option<SharedString>,
        run: impl Fn(&mut Window, &mut App) + 'static,
    ) -> Self {
        Self {
            name: name.into(),
            note,
            run: Rc::new(run),
        }
    }
}

pub struct SearchList {
    title: &'static str,
    entries: Vec<SearchEntry>,
    /// Whether an entry's name matches a query.
    matches: fn(&str, &str) -> bool,
    query: Entity<TextInput>,
    /// The entries that match the query.
    visible: Vec<usize>,
    selected: usize,
    scroll: UniformListScrollHandle,
}

impl EventEmitter<DismissEvent> for SearchList {}

impl Focusable for SearchList {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.query.focus_handle(cx)
    }
}

impl SearchList {
    pub fn new(
        title: &'static str,
        placeholder: &'static str,
        entries: Vec<SearchEntry>,
        matches: fn(&str, &str) -> bool,
        cx: &mut Context<Self>,
    ) -> Self {
        let query = cx.new(|cx| TextInput::new(title, placeholder, String::new(), cx));
        cx.subscribe(&query, |this, query, _: &Changed, cx| {
            let text = query.read(cx).text();
            this.visible = (0..this.entries.len())
                .filter(|&index| (this.matches)(&this.entries[index].name, text))
                .collect();
            this.select(0, cx);
        })
        .detach();
        Self {
            title,
            visible: (0..entries.len()).collect(),
            entries,
            matches,
            query,
            selected: 0,
            scroll: UniformListScrollHandle::new(),
        }
    }

    /// Start at this entry.
    pub fn at(mut self, entry: usize) -> Self {
        self.selected = entry;
        self.scroll.scroll_to_item(entry, ScrollStrategy::Center);
        self
    }

    fn select(&mut self, row: usize, cx: &mut Context<Self>) {
        self.selected = row;
        self.scroll.scroll_to_item(row, ScrollStrategy::Nearest);
        cx.notify();
    }

    fn step(&mut self, steps: usize, cx: &mut Context<Self>) {
        let count = self.visible.len().max(1);
        self.select((self.selected + steps) % count, cx);
    }

    /// Close first, so that the entry runs with the focus back where it was.
    fn run(&mut self, row: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(&index) = self.visible.get(row) else {
            return;
        };
        let run = Rc::clone(&self.entries[index].run);
        cx.emit(DismissEvent);
        window.defer(cx, move |window, cx| run(window, cx));
    }

    fn rows(&self, rows: Range<usize>, cx: &mut Context<Self>) -> Vec<impl IntoElement + use<>> {
        rows.filter_map(|row| {
            let entry = &self.entries[*self.visible.get(row)?];
            Some(Self::row(entry, row, row == self.selected, cx))
        })
        .collect()
    }

    fn row(
        entry: &SearchEntry,
        row: usize,
        selected: bool,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        let theme = theme();
        let element = div()
            .id(row)
            .role(Role::ListBoxOption)
            .aria_label(entry.name.clone())
            .aria_selected(selected)
            .test_id(format!("entry:{}", entry.name))
            .relative()
            .w_full()
            .flex()
            .items_center()
            .justify_between()
            .gap_4()
            .h(px(ROW))
            .px_2()
            .rounded(RADIUS)
            .cursor_pointer()
            .child(div().min_w_0().truncate().child(entry.name.clone()))
            .children(entry.note.clone().map(|note| {
                div()
                    .flex_none()
                    .text_size(SMALL_FONT_SIZE)
                    .text_color(theme.faint)
                    .child(note)
            }))
            .on_click(cx.listener(move |this, _, window, cx| this.run(row, window, cx)));
        if selected {
            element.bg(theme.selected)
        } else {
            element.hover(move |style| style.bg(theme.hover))
        }
    }
}

impl Render for SearchList {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = theme();
        let count = self.visible.len();
        let list = uniform_list(
            "entries",
            count,
            cx.processor(|this: &mut Self, rows, _, cx| this.rows(rows, cx)),
        )
        .track_scroll(&self.scroll)
        .h(px((count as f32 * ROW).min(MAX_HEIGHT)));
        div()
            .id("search-list")
            .role(Role::Dialog)
            .aria_label(self.title)
            .on_action(cx.listener(|this, _: &Confirm, window, cx| {
                this.run(this.selected, window, cx);
            }))
            .on_action(cx.listener(|this, _: &SelectNext, _, cx| this.step(1, cx)))
            .on_action(cx.listener(|this, _: &SelectPrevious, _, cx| {
                this.step(this.visible.len().max(1) - 1, cx);
            }))
            .w(px(520.0))
            .flex()
            .flex_col()
            .rounded(px(8.0))
            .border_1()
            .border_color(theme.border)
            .bg(theme.elevated)
            .shadow_lg()
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .h(px(40.0))
                    .px_3()
                    .border_b_1()
                    .border_color(theme.border)
                    .child(icon(Icon::Search, theme.muted))
                    .child(self.query.clone()),
            )
            .child(
                div()
                    .id("list")
                    .role(Role::ListBox)
                    .p_1()
                    .child(list)
                    .when(count == 0, |list| {
                        list.child(div().p_2().text_color(theme.faint).child("No matches"))
                    }),
            )
    }
}
