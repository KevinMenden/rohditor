//! Right dock of the Masks tool: the list of the masks, and the sliders of the selected one. Its
//! shapes are changed on the photo.

mod control;
mod list;
mod model;
mod new;
mod rename;
mod sliders;

use gpui::{Context, Entity, IntoElement, Render, Role, Window, div, prelude::*, px};
use rohditor_edit::MAX_MASK_COMPONENTS;
use rohditor_engine::tools::{LayerEdit, LocalSection, LocalSlider, MaskAction, MaskEditMode};

use super::controls::{Controls, FocusControls as _, Gestures as _, drag_tracker};
use crate::actions::CancelTool;
use crate::session::Session;
use crate::theme::{SMALL_FONT_SIZE, TAB_HEIGHT, theme};
use crate::ui::button::link;
use crate::ui::overlays::Overlays;
use crate::ui::test_id::TestId as _;
use control::Control;
use model::Model;
use rename::Rename;

pub use new::{COMPONENT_MODES, SHAPES};

pub struct MasksPanel {
    session: Entity<Session>,
    overlays: Entity<Overlays>,
    model: Model,
    controls: Controls<Control>,
    rename: Option<Rename>,
}

impl MasksPanel {
    pub fn new(
        session: Entity<Session>,
        overlays: Entity<Overlays>,
        cx: &mut Context<Self>,
    ) -> Self {
        cx.observe(&session, |panel, session, cx| {
            let model = Model::of(session.read(cx));
            if panel.model != model {
                panel.follow(model, cx);
            }
        })
        .detach();
        let sections = LocalSection::ALL.map(Control::Section);
        let sliders = LocalSlider::ALL.map(Control::Slider);
        let components = (0..MAX_MASK_COMPONENTS).map(Control::Component);
        let focusable = sections.into_iter().chain(sliders).chain(components);
        let mut controls = Controls::new(focusable, cx);
        controls
            .collapsed
            .insert(Control::Section(LocalSection::Detail));
        Self {
            session,
            overlays,
            model: Model::default(),
            controls,
            rename: None,
        }
    }

    fn follow(&mut self, model: Model, cx: &mut Context<Self>) {
        if self.model.selected_mask() != model.selected_mask() {
            // What was being changed belongs to the mask that was selected.
            self.abandon_gestures(cx);
        }
        let listed = model.masks.iter().map(|mask| mask.id);
        self.controls.focus.add(listed.map(Control::Mask), cx);
        if let Some(rename) = &self.rename
            && !model.masks.iter().any(|mask| mask.id == rename.mask)
        {
            self.rename = None;
        }
        self.model = model;
        cx.notify();
    }

    /// Carry out a request, after what was being typed.
    pub fn act(&mut self, action: MaskAction, cx: &mut Context<Self>) {
        self.finish_nudge(cx);
        self.commit_typing(cx);
        self.commit_rename(cx);
        self.session
            .update(cx, |session, cx| session.edit_masks(action, cx));
    }

    /// As [`Self::act`], for a request that depends on the selected mask.
    pub fn act_on_selected(
        &mut self,
        action: impl FnOnce(u32) -> MaskAction,
        cx: &mut Context<Self>,
    ) {
        if let Some(mask) = self.model.selected_mask() {
            self.act(action(mask), cx);
        }
    }

    /// Enable or disable the selected mask.
    pub fn toggle_selected(&mut self, cx: &mut Context<Self>) {
        let selected = self
            .model
            .selected_mask()
            .and_then(|id| self.model.mask(id));
        if let Some(mask) = selected {
            let edit = LayerEdit::Enabled(!mask.enabled);
            self.act(MaskAction::Edit(mask.id, edit), cx);
        }
    }

    fn header(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        div()
            .flex()
            .flex_none()
            .items_center()
            .justify_between()
            .h(TAB_HEIGHT)
            .px_3()
            .border_b_1()
            .border_color(theme().border)
            .child("Masks")
            .child(self.new_mask(cx))
    }

    /// What the next drag on the photo does, while it draws a shape.
    fn creation(&self) -> Option<impl IntoElement + use<>> {
        let hint = match self.model.mode {
            MaskEditMode::CreateLinear => "Drag on the photo from full to no effect.",
            MaskEditMode::CreateRadial => "Drag on the photo from the center outwards.",
            _ => return None,
        };
        let theme = theme();
        Some(
            div()
                .test_id("mask-creation")
                .relative()
                .flex()
                .flex_none()
                .items_center()
                .justify_between()
                .gap_2()
                .px_3()
                .py_1()
                .bg(theme.element)
                .text_size(SMALL_FONT_SIZE)
                .text_color(theme.accent)
                .child(div().min_w_0().child(hint))
                .child(link("cancel-mask", "Cancel", CancelTool, true).flex_none()),
        )
    }

    fn body(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let theme = theme();
        let note = |text: String| {
            div()
                .px_1()
                .py_1()
                .text_size(SMALL_FONT_SIZE)
                .text_color(theme.faint)
                .child(text)
        };
        let masks = self.mask_list(cx);
        let empty = masks.is_empty().then(|| {
            note(if self.model.ready {
                "No masks yet. A mask limits adjustments to a part of the photo.".to_owned()
            } else {
                "Open a photo to mask it.".to_owned()
            })
        });
        let limit = self
            .model
            .limit
            .map(|limit| note(limit.to_string()).text_color(theme.warning));
        let sliders = self
            .model
            .selected
            .as_ref()
            .map(|selected| self.sliders(selected, cx));
        div()
            .flex()
            .flex_col()
            .gap(px(3.0))
            .p_2()
            .children(empty)
            .children(masks)
            .children(limit)
            .children(sliders)
    }
}

impl Render for MasksPanel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.controls.focus.forget_lost(window);
        let theme = theme();
        let body = self.body(cx);
        div()
            .id("masks-panel")
            .role(Role::Group)
            .aria_label("Masks")
            .test_id("masks-panel")
            .flex()
            .flex_col()
            .size_full()
            .bg(theme.panel)
            .border_l_1()
            .border_color(theme.border)
            .child(drag_tracker(cx.entity()))
            .child(self.header(cx))
            .children(self.creation())
            .child(
                self.control_list("mask-controls", "Masks and their adjustments", cx)
                    .child(body),
            )
    }
}
