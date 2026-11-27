//! Right dock: histogram and adjustments.
//!
//! Each adjustment is one row that is its own slider: dragging anywhere on it changes the value
//! relative to where it was, Shift drags finer, and a double click resets it. A click on the value
//! types it. With the focus, the arrow keys change it.

mod choices;
mod control;
mod filter;
mod info;
mod mixer;
mod optics;
mod preset_form;
mod presets;
mod section;
mod tone_curve;
mod wheels;

use std::collections::HashMap;
use std::sync::Arc;

use gpui::{
    App, Context, Div, Entity, FocusHandle, IntoElement, Render, RenderImage, SharedString, Window,
    div, prelude::*,
};
use rohditor_core::Histogram;
use rohditor_edit::{EditRecipe, RecipeSection};
use rohditor_engine::open_document::{LensProfiles, NoiseSuggestion};
use rohditor_engine::tools::Picker;

use control::Control;
use info::PhotoInfo;

use super::adjustments::GROUPS;
use super::controls::{Controls, FocusControls as _, drag_tracker};
use crate::actions::{ApplyPreset, AutoTone, ResetAdjustments};
use crate::session::{MixerColorPicked, PresetEntry, Session};
use crate::theme::{DEVELOP_WIDTH, RADIUS, TAB_HEIGHT, theme};
use crate::ui::button::link;
use crate::ui::histogram::histogram;
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
    lens_profiles: Arc<LensProfiles>,
    /// The lens the photo names.
    lens: Option<String>,
    /// The vignetting correction lacks the focus distance.
    infinity_vignetting: bool,
    info: Option<Arc<PhotoInfo>>,
    presets: Arc<[PresetEntry]>,
    /// The name and amount of the preset that was applied last.
    applied_preset: Option<(SharedString, f32)>,
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
            lens_profiles: Arc::clone(&document.lens_profiles),
            lens: document
                .info
                .as_ref()
                .and_then(|info| info.capture.lens_label()),
            infinity_vignetting: document
                .preview
                .diagnostics
                .is_some_and(|shown| shown.worker.optics_used_infinity_distance_fallback),
            info: document
                .info
                .as_ref()
                .map(|info| Arc::new(PhotoInfo::of(info))),
            presets: session.presets(),
            applied_preset: document
                .applied_preset()
                .map(|(name, amount)| (name.to_owned().into(), amount)),
        }
    }
}

pub fn bind_keys(cx: &mut App) {
    presets::bind_keys(cx);
}

pub struct DevelopPanel {
    session: Entity<Session>,
    overlays: Entity<Overlays>,
    model: Model,
    #[cfg(test)]
    pub renders: usize,
    controls: Controls<Control>,
    search: Entity<TextInput>,
    /// Hide what is at its default.
    edited_only: bool,
    /// The colour whose mixer sliders are listed.
    mixer_channel: usize,
    /// The selected point of the tone curve, by index.
    curve_point: Option<usize>,
    wheel_picture: Arc<RenderImage>,
    /// The focus of each listed preset.
    preset_focus: HashMap<SharedString, FocusHandle>,
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
                panel.list_preset_focus(cx);
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
        let mut controls = Controls::new(Control::focusable(), cx);
        // Used less often than what follows them.
        controls.collapsed.extend([
            Control::Section(RecipeSection::ToneCurve),
            Control::Section(RecipeSection::HighlightReconstruction),
            Control::Section(RecipeSection::ColorMixer),
            Control::Section(RecipeSection::ColorGrading),
            Control::Section(RecipeSection::ColorProfile),
            Control::Section(RecipeSection::FalseColor),
            Control::Section(RecipeSection::Defringe),
            Control::Presets,
            Control::Info,
        ]);
        Self {
            session,
            overlays,
            model: Model::default(),
            #[cfg(test)]
            renders: 0,
            controls,
            search,
            edited_only: false,
            mixer_channel: 0,
            curve_point: None,
            wheel_picture: wheels::wheel_picture(),
            preset_focus: HashMap::new(),
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
                    .child(link("presets", "Presets", ApplyPreset, enabled))
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
        self.controls.focus.forget_lost(window);
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
                let presets = self.presets(cx);
                let info = self.info(cx);
                let empty = groups.is_empty() && presets.is_none() && info.is_none();
                let nothing = if self.searching(cx) {
                    "Nothing found."
                } else {
                    "Nothing is edited yet."
                };
                div()
                    .pt_1()
                    .children(groups)
                    .children(presets)
                    .children(info)
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
                    .child(graph)
                    .children(self.exposure_summary()),
            )
            .when(recipes.is_some(), |panel| panel.child(self.filter(cx)))
            .child(
                self.control_list("adjustments", "Adjustments", cx)
                    .child(body),
            )
    }
}
