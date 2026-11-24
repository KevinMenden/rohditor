//! Right dock: histogram and adjustments.
//!
//! Each adjustment is one row that is its own slider: dragging anywhere on it changes the value
//! relative to where it was, Shift drags finer, and a double click resets it. A click on the value
//! types it. With the focus, the arrow keys change it.

mod choices;
mod filter;
mod focus;
mod gestures;
mod mixer;
mod row;
mod section;
mod tone_curve;
mod wheels;

use std::collections::HashSet;
use std::sync::Arc;

use gpui::{
    App, Context, Div, Entity, IntoElement, Render, RenderImage, Role, Window, div, prelude::*,
};
use rohditor_core::Histogram;
use rohditor_edit::{EditRecipe, RecipeSection};
use rohditor_engine::open_document::NoiseSuggestion;
use rohditor_engine::tools::Picker;

use focus::PanelFocus;
use gestures::{Drag, Nudge, Typing, drag_tracker};

use super::adjustments::GROUPS;
use crate::actions::{AutoTone, ResetAdjustments};
use crate::session::{MixerColorPicked, Session};
use crate::theme::{DEVELOP_WIDTH, RADIUS, TAB_HEIGHT, theme};
use crate::ui::button::link;
use crate::ui::histogram::histogram;
use crate::ui::keys::{SelectNext, SelectPrevious};
use crate::ui::overlays::Overlays;
use crate::ui::text_input::{Changed, TextInput};

/// What the panel shows. Compared on every session change, so that the panel only redraws for
/// its own content.
#[derive(Clone, Default, PartialEq)]
struct Model {
    /// The recipe of the open photo and its defaults, once it can be edited.
    recipes: Option<(EditRecipe, EditRecipe)>,
    histogram: Option<Histogram>,
    picker: Option<Picker>,
    noise: Option<NoiseSuggestion>,
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
            picker: session.picker(),
            noise: Some(document.noise_suggestion()),
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
    search: Entity<TextInput>,
    /// Hide what is at its default.
    edited_only: bool,
    /// The colour whose mixer sliders are listed.
    mixer_channel: usize,
    wheel_picture: Arc<RenderImage>,
    focus: PanelFocus,
}

impl DevelopPanel {
    pub fn new(
        session: Entity<Session>,
        overlays: Entity<Overlays>,
        window: &mut Window,
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
        cx.subscribe_in(
            &session,
            window,
            |panel, _, picked: &MixerColorPicked, window, cx| {
                panel.show_mixer_channel(picked.0, window, cx);
            },
        )
        .detach();
        let search =
            cx.new(|cx| TextInput::new("Search adjustments", "Search…", String::new(), cx));
        cx.subscribe(&search, |_, _, _: &Changed, cx| cx.notify())
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
            // Used less often than what follows them.
            collapsed: HashSet::from([
                RecipeSection::ToneCurve,
                RecipeSection::HighlightReconstruction,
                RecipeSection::ColorMixer,
                RecipeSection::ColorGrading,
                RecipeSection::ColorProfile,
                RecipeSection::FalseColor,
                RecipeSection::Defringe,
            ]),
            search,
            edited_only: false,
            mixer_channel: 0,
            wheel_picture: wheels::wheel_picture(),
            focus: PanelFocus::new(cx),
        }
    }

    fn header(&self, enabled: bool) -> impl IntoElement + use<> {
        let theme = theme();
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
                    .child(link("auto", "Auto", AutoTone, enabled))
                    .child(link("reset", "Reset", ResetAdjustments, enabled)),
            )
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
            ..
        } = self.model.clone();
        let graph = histogram(bins.as_ref());
        let body = match &recipes {
            Some((recipe, defaults)) => {
                let groups: Vec<_> = GROUPS
                    .iter()
                    .filter_map(|group| self.group(group, recipe, defaults, cx))
                    .collect();
                let empty = groups.is_empty();
                let nothing = if self.searching(cx) {
                    "Nothing found."
                } else {
                    "Nothing is edited yet."
                };
                div()
                    .pt_1()
                    .children(groups)
                    .when(empty, |body| body.child(hint(nothing)))
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
