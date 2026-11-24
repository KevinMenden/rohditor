//! Right dock: histogram and adjustments.
//!
//! Each adjustment is one row that is its own slider: dragging anywhere on it changes the value
//! relative to where it was, Shift drags finer, and a double click resets it. A click on the value
//! types it. With the focus, the arrow keys change it.

mod focus;
mod gestures;
mod row;
mod section;

use std::collections::HashSet;

use gpui::{
    App, Context, Div, Entity, IntoElement, MouseButton, Render, Role, Window, div, prelude::*,
};
use rohditor_core::Histogram;
use rohditor_edit::{EditRecipe, RecipeSection};

use focus::PanelFocus;
use gestures::{Drag, Nudge, Typing, drag_tracker};

use super::adjustments::{Adjustment, SECTIONS};
use crate::actions::{AutoTone, ResetAdjustments};
use crate::session::Session;
use crate::theme::{DEVELOP_WIDTH, RADIUS, SMALL_FONT_SIZE, TAB_HEIGHT, theme};
use crate::ui::histogram::histogram;
use crate::ui::keys::{SelectNext, SelectPrevious};
use crate::ui::overlays::Overlays;
use crate::ui::test_id::TestId as _;
use crate::ui::toggle::toggle;

/// What the panel shows. Compared on every session change, so that the panel only redraws for
/// its own content.
#[derive(Clone, Default, PartialEq)]
struct Model {
    /// The recipe of the open photo and its defaults, once it can be edited.
    recipes: Option<(EditRecipe, EditRecipe)>,
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
            recipes: Some((document.edits.recipe().clone(), document.default_recipe())),
            histogram: document.preview.histogram,
        }
    }
}

pub fn bind_keys(cx: &mut App) {
    focus::bind_keys(cx);
    row::bind_keys(cx);
    section::bind_keys(cx);
}

pub struct DevelopPanel {
    session: Entity<Session>,
    overlays: Entity<Overlays>,
    model: Model,
    #[cfg(test)]
    pub renders: usize,
    drag: Option<Drag>,
    nudge: Option<Nudge>,
    typing: Option<Typing>,
    collapsed: HashSet<RecipeSection>,
    /// Hide what is at its default.
    edited_only: bool,
    focus: PanelFocus,
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
            nudge: None,
            typing: None,
            collapsed: HashSet::new(),
            edited_only: false,
            focus: PanelFocus::new(cx),
        }
    }

    /// Whether the panel shows this adjustment.
    fn shows(&self, adjustment: &Adjustment, recipe: &EditRecipe, defaults: &EditRecipe) -> bool {
        // A slider must not vanish while it is changed.
        let parameter = adjustment.parameter;
        let active = [
            self.drag.as_ref().map(|drag| drag.adjustment.parameter),
            self.typing
                .as_ref()
                .map(|typing| typing.adjustment.parameter),
        ]
        .contains(&Some(parameter));
        !self.edited_only || active || parameter.is_changed(recipe, defaults)
    }

    fn header(&self, enabled: bool) -> impl IntoElement + use<> {
        let theme = theme();
        let link = |id, label, action: Box<dyn gpui::Action>| {
            let link = div()
                .id(id)
                .role(Role::Button)
                .aria_label(label)
                .test_id(format!("button:{id}"))
                .border_1()
                .border_color(gpui::transparent_black())
                .on_mouse_down(MouseButton::Left, |_, window, _| window.prevent_default())
                .px_1p5()
                .rounded(RADIUS)
                .text_size(SMALL_FONT_SIZE)
                .child(label);
            if enabled {
                link.tab_index(0)
                    .focus_visible(move |style| style.border_color(theme.accent))
                    .text_color(theme.muted)
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
}

fn hint(text: &'static str) -> Div {
    div().p_3().text_color(theme().faint).child(text)
}

impl Render for DevelopPanel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        #[cfg(test)]
        {
            self.renders += 1;
        }
        self.forget_lost_focus(window);
        let theme = theme();
        let Model {
            recipes,
            histogram: bins,
        } = self.model.clone();
        let graph = histogram(bins.as_ref());
        let body = match &recipes {
            Some((recipe, defaults)) => {
                let sections: Vec<_> = SECTIONS
                    .iter()
                    .filter_map(|section| self.section(section, recipe, defaults, cx))
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
            .child(self.header(recipes.is_some()))
            .child(
                div()
                    .flex_none()
                    .m_2()
                    .p_1()
                    .rounded(RADIUS)
                    .bg(theme.editor)
                    .child(graph),
            )
            .when(recipes.is_some(), |panel| panel.child(self.filter(cx)))
            .child(
                div()
                    .id("adjustments")
                    .role(Role::Group)
                    .aria_label("Adjustments")
                    .key_context(focus::CONTEXT)
                    .track_focus(&self.focus.entry)
                    .on_action(cx.listener(|this, _: &SelectNext, window, cx| {
                        this.focus.step(true, window, cx);
                    }))
                    .on_action(cx.listener(|this, _: &SelectPrevious, window, cx| {
                        this.focus.step(false, window, cx);
                    }))
                    .track_scroll(&self.focus.scroll)
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .child(body),
            )
    }
}
