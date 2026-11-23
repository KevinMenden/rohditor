//! Right dock: histogram and adjustments.
//!
//! Each adjustment is one row that is its own slider: dragging anywhere on it changes the value
//! relative to where it was, Shift drags finer, and a double click resets it.

use std::collections::HashSet;

use gpui::{
    Bounds, Context, CursorStyle, DispatchPhase, Div, Entity, IntoElement, MouseButton,
    MouseDownEvent, MouseMoveEvent, MouseUpEvent, Pixels, Point, Render, Window, canvas, div,
    prelude::*, px, relative,
};
use rohditor_core::{Histogram, WhiteBalanceCoordinates};
use rohditor_edit::EditRecipe;

use super::adjustments::{Adjustment, SECTIONS, Section};
use crate::actions::{AutoTone, ResetAdjustments};
use crate::assets::Icon;
use crate::session::{EditPhase, Session};
use crate::theme::{DEVELOP_WIDTH, RADIUS, SMALL_FONT_SIZE, TAB_HEIGHT, theme};
use crate::ui::button::icon;
use crate::ui::histogram::histogram;
use crate::ui::menu::MenuEntry;
use crate::ui::overlays::Overlays;
use crate::ui::slider::{dragged, fraction};
use crate::ui::test_id::TestId as _;
use crate::ui::toggle::toggle;

const FINE_DRAG: f32 = 0.1;

#[derive(Clone, Copy)]
struct Drag {
    adjustment: Adjustment,
    last_x: Pixels,
    value: f32,
    width: Pixels,
}

/// What the panel shows. Compared on every session change, so that the panel only redraws for
/// its own content.
#[derive(Clone, Default, PartialEq)]
struct Model {
    /// The recipe of the open photo, once it can be edited.
    recipe: Option<EditRecipe>,
    as_shot: Option<WhiteBalanceCoordinates>,
    histogram: Option<Histogram>,
}

impl Model {
    fn of(session: &Session) -> Self {
        let Some(document) = session
            .document()
            .filter(|document| document.frame.is_some())
        else {
            return Self::default();
        };
        Self {
            recipe: Some(document.edits.recipe().clone()),
            as_shot: session.as_shot(),
            histogram: document.preview.histogram,
        }
    }
}

pub struct DevelopPanel {
    session: Entity<Session>,
    overlays: Entity<Overlays>,
    model: Model,
    #[cfg(test)]
    pub renders: usize,
    drag: Option<Drag>,
    collapsed: HashSet<&'static str>,
    /// Hide what is at its default.
    edited_only: bool,
}

impl DevelopPanel {
    pub fn new(
        session: Entity<Session>,
        overlays: Entity<Overlays>,
        cx: &mut Context<Self>,
    ) -> Self {
        cx.observe(&session, |panel, session, cx| {
            let model = Model::of(session.read(cx));
            if panel.model != model {
                panel.model = model;
                cx.notify();
            }
        })
        .detach();
        Self {
            session,
            overlays,
            model: Model::default(),
            #[cfg(test)]
            renders: 0,
            drag: None,
            collapsed: HashSet::new(),
            edited_only: false,
        }
    }

    fn reset(&self, adjustment: Adjustment, cx: &mut Context<Self>) {
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
    }

    fn show_menu(
        &self,
        adjustment: Adjustment,
        at: Point<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let panel = cx.entity();
        let entries = vec![MenuEntry::new("Reset", move |_, cx| {
            panel.update(cx, |panel, cx| panel.reset(adjustment, cx));
        })];
        self.overlays.update(cx, |overlays, cx| {
            overlays.show_menu(at, entries, window, cx);
        });
    }

    /// Whether the panel shows this adjustment.
    fn shows(
        &self,
        adjustment: Adjustment,
        recipe: &EditRecipe,
        as_shot: Option<WhiteBalanceCoordinates>,
    ) -> bool {
        // A slider must not vanish while it is dragged over its default.
        let dragged = self.drag.is_some_and(|drag| drag.adjustment == adjustment);
        !self.edited_only || dragged || adjustment.is_edited(recipe, as_shot)
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
            return self.reset(adjustment, cx);
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

    fn filter(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let panel = cx.entity();
        div().flex().flex_none().px_2().child(toggle(
            "edited-only",
            "Edited only",
            self.edited_only,
            move |_, cx| {
                panel.update(cx, |panel, cx| {
                    panel.edited_only = !panel.edited_only;
                    cx.notify();
                });
            },
        ))
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
                .filter(|&&adjustment| self.shows(adjustment, recipe, as_shot))
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
                    .test_id(format!("section:{title}"))
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
        let edited = adjustment.is_edited(recipe, as_shot);
        let active = self.drag.is_some_and(|drag| drag.adjustment == adjustment);
        let panel = cx.entity();
        div()
            .id(adjustment.label())
            .test_id(format!("adjustment:{}", adjustment.label()))
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
            .on_mouse_down(
                MouseButton::Right,
                cx.listener(move |this, event: &MouseDownEvent, window, cx| {
                    this.show_menu(adjustment, event.position, window, cx);
                }),
            )
            .child(press_area(panel, adjustment))
    }
}

fn hint(text: &'static str) -> Div {
    div().p_3().text_color(theme().faint).child(text)
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
        #[cfg(test)]
        {
            self.renders += 1;
        }
        let theme = theme();
        let Model {
            recipe,
            as_shot,
            histogram: bins,
        } = self.model.clone();
        let graph = histogram(bins.as_ref());
        let body = match &recipe {
            Some(recipe) => {
                let sections: Vec<_> = SECTIONS
                    .iter()
                    .filter(|section| {
                        section
                            .adjustments
                            .iter()
                            .any(|&adjustment| self.shows(adjustment, recipe, as_shot))
                    })
                    .map(|section| {
                        self.section(section, recipe, as_shot, cx)
                            .into_any_element()
                    })
                    .collect();
                let empty = sections.is_empty();
                div()
                    .pt_1()
                    .children(sections)
                    .when(empty, |body| body.child(hint("Nothing is edited yet.")))
            }
            None => hint("Open a photo to adjust it."),
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
            .when(recipe.is_some(), |panel| panel.child(self.filter(cx)))
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
