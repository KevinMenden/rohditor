//! Right dock: histogram and adjustments.
//!
//! Each adjustment is one row that is its own slider: dragging anywhere on it changes the value
//! relative to where it was, Shift drags finer, and a double click resets it.

use std::collections::HashSet;

use gpui::{
    Bounds, Context, CursorStyle, DispatchPhase, Entity, IntoElement, MouseButton, MouseDownEvent,
    MouseMoveEvent, MouseUpEvent, Pixels, Render, Window, canvas, div, prelude::*, px, relative,
};
use rohditor_core::WhiteBalanceCoordinates;
use rohditor_edit::EditRecipe;

use super::adjustments::{Adjustment, SECTIONS, Section};
use crate::actions::{AutoTone, ResetAdjustments};
use crate::assets::Icon;
use crate::session::{EditPhase, Session};
use crate::theme::{DEVELOP_WIDTH, RADIUS, SMALL_FONT_SIZE, TAB_HEIGHT, theme};
use crate::ui::button::icon;
use crate::ui::histogram::histogram;
use crate::ui::slider::{dragged, fraction};

const FINE_DRAG: f32 = 0.1;

#[derive(Clone, Copy)]
struct Drag {
    adjustment: Adjustment,
    last_x: Pixels,
    value: f32,
    width: Pixels,
}

pub struct DevelopPanel {
    session: Entity<Session>,
    drag: Option<Drag>,
    collapsed: HashSet<&'static str>,
}

impl DevelopPanel {
    pub fn new(session: Entity<Session>, cx: &mut Context<Self>) -> Self {
        cx.observe(&session, |_, _, cx| cx.notify()).detach();
        Self {
            session,
            drag: None,
            collapsed: HashSet::new(),
        }
    }

    fn press(
        &mut self,
        adjustment: Adjustment,
        event: &MouseDownEvent,
        width: Pixels,
        cx: &mut Context<Self>,
    ) {
        if event.click_count == 2 {
            self.drag = None;
            self.session.update(cx, |session, cx| {
                let as_shot = session.as_shot();
                let value = adjustment.default_value(as_shot);
                session.edit(
                    |recipe| adjustment.set(recipe, as_shot, value),
                    EditPhase::Finished,
                    adjustment.is_white_balance(),
                    cx,
                );
            });
            return;
        }
        self.session
            .update(cx, |session, _| session.begin_gesture());
        let session = self.session.read(cx);
        if let Some(document) = session.document() {
            self.drag = Some(Drag {
                adjustment,
                last_x: event.position.x,
                value: adjustment.get(document.edits.recipe(), session.as_shot()),
                width,
            });
        }
        cx.notify();
    }

    fn drag_to(&mut self, event: &MouseMoveEvent, cx: &mut Context<Self>) {
        let Some(drag) = self.drag.as_mut() else {
            return;
        };
        if !event.dragging() {
            // The release happened outside the window.
            return self.release(cx);
        }
        let speed = if event.modifiers.shift {
            FINE_DRAG
        } else {
            1.0
        };
        let delta = (event.position.x - drag.last_x) / drag.width * speed;
        drag.last_x = event.position.x;
        let adjustment = drag.adjustment;
        drag.value = dragged(adjustment.format(), adjustment.range(), drag.value, delta);
        let value = drag.value;
        self.session.update(cx, |session, cx| {
            let as_shot = session.as_shot();
            session.edit(
                |recipe| adjustment.set(recipe, as_shot, value),
                EditPhase::Dragging,
                adjustment.is_white_balance(),
                cx,
            );
        });
    }

    fn release(&mut self, cx: &mut Context<Self>) {
        if let Some(drag) = self.drag.take() {
            self.session.update(cx, |session, cx| {
                session.finish_gesture(drag.adjustment.is_white_balance(), cx);
            });
            cx.notify();
        }
    }

    fn header(&self, enabled: bool) -> impl IntoElement + use<> {
        let theme = theme();
        let link = |id, label, action: Box<dyn gpui::Action>| {
            let link = div()
                .id(id)
                .px_1p5()
                .rounded(RADIUS)
                .text_size(SMALL_FONT_SIZE)
                .child(label);
            if enabled {
                link.text_color(theme.muted)
                    .cursor_pointer()
                    .hover(move |style| style.bg(theme.hover).text_color(theme.text))
                    .on_click(move |_, window, cx| window.dispatch_action(action.boxed_clone(), cx))
            } else {
                link.text_color(theme.faint)
            }
        };
        div()
            .flex()
            .flex_none()
            .items_center()
            .justify_between()
            .h(TAB_HEIGHT)
            .px_3()
            .border_b_1()
            .border_color(theme.border)
            .child("Develop")
            .child(
                div()
                    .flex()
                    .gap_1()
                    .child(link("auto", "Auto", Box::new(AutoTone)))
                    .child(link("reset", "Reset", Box::new(ResetAdjustments))),
            )
    }

    fn section(
        &self,
        section: &Section,
        recipe: &EditRecipe,
        as_shot: Option<WhiteBalanceCoordinates>,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        let theme = theme();
        let title = section.title;
        let collapsed = self.collapsed.contains(title);
        let chevron = if collapsed {
            Icon::ChevronRight
        } else {
            Icon::ChevronDown
        };
        let rows: Vec<_> = if collapsed {
            Vec::new()
        } else {
            section
                .adjustments
                .iter()
                .map(|&adjustment| self.row(adjustment, recipe, as_shot, cx).into_any_element())
                .collect()
        };
        div()
            .flex()
            .flex_col()
            .gap(px(3.0))
            .px_2()
            .pb_2()
            .child(
                div()
                    .id(title)
                    .flex()
                    .items_center()
                    .gap_1()
                    .h(px(28.0))
                    .cursor_pointer()
                    .text_size(SMALL_FONT_SIZE)
                    .text_color(theme.muted)
                    .child(icon(chevron, theme.faint))
                    .child(title)
                    .on_click(cx.listener(move |this, _, _, cx| {
                        if !this.collapsed.remove(title) {
                            this.collapsed.insert(title);
                        }
                        cx.notify();
                    })),
            )
            .children(rows)
    }

    fn row(
        &self,
        adjustment: Adjustment,
        recipe: &EditRecipe,
        as_shot: Option<WhiteBalanceCoordinates>,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        let theme = theme();
        let (range, format) = (adjustment.range(), adjustment.format());
        let value = adjustment.get(recipe, as_shot);
        let default = adjustment.default_value(as_shot);
        let at = |value| fraction(format, range, value);
        let (from, to) = (at(default).min(at(value)), at(default).max(at(value)));
        let edited = (at(value) - at(default)).abs() > 1e-4;
        let active = self.drag.is_some_and(|drag| drag.adjustment == adjustment);
        let panel = cx.entity();
        div()
            .id(adjustment.label())
            .relative()
            .flex()
            .items_center()
            .justify_between()
            .h(px(24.0))
            .px_2()
            .rounded(RADIUS)
            .overflow_hidden()
            .bg(if active { theme.hover } else { theme.element })
            .hover(move |style| style.bg(theme.hover))
            .cursor(CursorStyle::ResizeLeftRight)
            .child(
                div()
                    .absolute()
                    .top_0()
                    .bottom_0()
                    .left(relative(from))
                    .w(relative(to - from))
                    .bg(theme.fill),
            )
            // Marks the value, so that a slider at its default still shows its position.
            .child(
                div()
                    .absolute()
                    .top_0()
                    .bottom_0()
                    .left(relative(at(value).min(0.993)))
                    .w(px(2.0))
                    .bg(if edited || active {
                        theme.accent
                    } else {
                        theme.selected
                    }),
            )
            .child(
                div()
                    .relative()
                    .text_color(if edited { theme.text } else { theme.muted })
                    .child(adjustment.label()),
            )
            .child(
                div()
                    .relative()
                    .text_size(SMALL_FONT_SIZE)
                    .text_color(if edited { theme.text } else { theme.faint })
                    .child(format.label(value, range)),
            )
            .child(press_area(panel, adjustment))
    }
}

/// Covers a row and reports presses together with the row's width.
fn press_area(panel: Entity<DevelopPanel>, adjustment: Adjustment) -> impl IntoElement {
    canvas(
        |bounds, window, _| window.insert_hitbox(bounds, gpui::HitboxBehavior::Normal),
        move |bounds: Bounds<Pixels>, hitbox, window, _| {
            window.on_mouse_event(move |event: &MouseDownEvent, phase, window, cx| {
                if phase == DispatchPhase::Bubble
                    && event.button == MouseButton::Left
                    && hitbox.is_hovered(window)
                {
                    panel.update(cx, |panel, cx| {
                        panel.press(adjustment, event, bounds.size.width, cx);
                    });
                }
            });
        },
    )
    .absolute()
    .size_full()
    .top_0()
    .left_0()
}

/// Follows a drag wherever the pointer goes.
fn drag_tracker(panel: Entity<DevelopPanel>) -> impl IntoElement {
    canvas(
        |_, _, _| (),
        move |_, (), window, _| {
            let moved = panel.clone();
            window.on_mouse_event(move |event: &MouseMoveEvent, phase, _, cx| {
                if phase == DispatchPhase::Capture {
                    moved.update(cx, |panel, cx| panel.drag_to(event, cx));
                }
            });
            window.on_mouse_event(move |event: &MouseUpEvent, phase, _, cx| {
                if phase == DispatchPhase::Capture && event.button == MouseButton::Left {
                    panel.update(cx, |panel, cx| panel.release(cx));
                }
            });
        },
    )
    .absolute()
    .size_0()
}

impl Render for DevelopPanel {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = theme();
        let session = self.session.read(cx);
        let as_shot = session.as_shot();
        let document = session
            .document()
            .filter(|document| document.frame.is_some());
        let recipe = document.map(|document| document.edits.recipe().clone());
        let graph = histogram(document.and_then(|document| document.preview.histogram.as_ref()));
        let body = match &recipe {
            Some(recipe) => {
                let sections: Vec<_> = SECTIONS
                    .iter()
                    .map(|section| {
                        self.section(section, recipe, as_shot, cx)
                            .into_any_element()
                    })
                    .collect();
                div().pt_1().children(sections)
            }
            None => div()
                .p_4()
                .text_color(theme.faint)
                .child("Open a photo to adjust it."),
        };
        div()
            .flex()
            .flex_col()
            .flex_none()
            .w(DEVELOP_WIDTH)
            .h_full()
            .bg(theme.panel)
            .border_l_1()
            .border_color(theme.border)
            .child(drag_tracker(cx.entity()))
            .child(self.header(recipe.is_some()))
            .child(
                div()
                    .flex_none()
                    .m_2()
                    .p_1()
                    .rounded(RADIUS)
                    .bg(theme.editor)
                    .child(graph),
            )
            .child(
                div()
                    .id("adjustments")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .child(body),
            )
    }
}
