//! Renaming a mask in the list.

use gpui::{Context, Entity, Focusable as _, Subscription, Window, prelude::*};
use rohditor_engine::tools::{LayerEdit, MaskAction};

use super::MasksPanel;
use super::control::Control;
use crate::panels::controls::Gestures as _;
use crate::ui::text_input::TextInput;

pub(super) struct Rename {
    pub mask: u32,
    pub input: Entity<TextInput>,
    _blurred: Subscription,
}

impl MasksPanel {
    pub fn rename_selected(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(mask) = self.model.selected_mask() {
            self.start_rename(mask, window, cx);
        }
    }

    pub(super) fn start_rename(&mut self, mask: u32, window: &mut Window, cx: &mut Context<Self>) {
        self.finish_nudge(cx);
        let Some(name) = self.model.mask(mask).map(|mask| mask.name.to_string()) else {
            return;
        };
        let input = cx.new(|cx| TextInput::new("Mask name", "Name…", name, cx));
        let focus = input.read(cx).focus_handle(cx);
        focus.focus(window, cx);
        let blurred = cx.on_blur(&focus, window, |panel, _, cx| panel.commit_rename(cx));
        self.rename = Some(Rename {
            mask,
            input,
            _blurred: blurred,
        });
        cx.notify();
    }

    /// Stop typing the name, and focus the mask.
    pub(super) fn stop_rename(
        &mut self,
        commit: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(mask) = self.rename.as_ref().map(|rename| rename.mask) else {
            return;
        };
        if commit {
            self.commit_rename(cx);
        }
        self.rename = None;
        self.controls
            .focus
            .control(Control::Mask(mask))
            .focus(window, cx);
        cx.notify();
    }

    /// Apply the name that was typed, unless it is empty.
    pub(super) fn commit_rename(&mut self, cx: &mut Context<Self>) {
        let Some(rename) = self.rename.take() else {
            return;
        };
        cx.notify();
        let name = rename.input.read(cx).text().trim().to_owned();
        if !name.is_empty() {
            let edit = MaskAction::Edit(rename.mask, LayerEdit::Rename(name));
            self.session
                .update(cx, |session, cx| session.edit_masks(edit, cx));
        }
    }
}
