//! The commands of the Masks tool.

use gpui::{Context, Div, Stateful, Window, prelude::*};
use rohditor_engine::tools::{LayerEdit, MaskAction, ToolKind};

use super::Workspace;
use crate::actions::{
    AddToMask, DeleteMask, MasksTool, MoveMaskDown, MoveMaskUp, NewMask, RenameMask, ToggleMask,
};

impl Workspace {
    pub(super) fn with_mask_actions(
        &self,
        root: Stateful<Div>,
        cx: &mut Context<Self>,
    ) -> Stateful<Div> {
        root.on_action(
            cx.listener(|this, _: &MasksTool, _, cx| this.toggle_tool(ToolKind::Masks, cx)),
        )
        .on_action(cx.listener(|this, action: &NewMask, window, cx| {
            this.draw_mask(MaskAction::NewLayer(action.0), window, cx);
        }))
        .on_action(cx.listener(|this, action: &AddToMask, window, cx| {
            let AddToMask { shape, mode } = *action;
            this.draw_mask(MaskAction::NewComponent(shape, mode), window, cx);
        }))
        .on_action(cx.listener(|this, _: &DeleteMask, _, cx| {
            this.act_on_masks(MaskAction::DeleteSelected, cx);
        }))
        .on_action(cx.listener(|this, _: &RenameMask, window, cx| {
            this.masks
                .update(cx, |panel, cx| panel.rename_selected(window, cx));
        }))
        .on_action(cx.listener(|this, _: &ToggleMask, _, cx| {
            this.masks.update(cx, |panel, cx| panel.toggle_selected(cx));
        }))
        .on_action(cx.listener(|this, _: &MoveMaskUp, _, cx| {
            this.edit_selected_mask(LayerEdit::MoveUp, cx);
        }))
        .on_action(cx.listener(|this, _: &MoveMaskDown, _, cx| {
            this.edit_selected_mask(LayerEdit::MoveDown, cx);
        }))
    }

    /// Start to draw a shape, with the focus on the photo.
    fn draw_mask(&mut self, action: MaskAction, window: &mut Window, cx: &mut Context<Self>) {
        self.show_tool(ToolKind::Masks, cx);
        self.act_on_masks(action, cx);
        self.focus.focus(window, cx);
    }

    fn act_on_masks(&mut self, action: MaskAction, cx: &mut Context<Self>) {
        self.masks.update(cx, |panel, cx| panel.act(action, cx));
    }

    fn edit_selected_mask(&mut self, edit: LayerEdit, cx: &mut Context<Self>) {
        self.masks.update(cx, |panel, cx| {
            panel.act_on_selected(|mask| MaskAction::Edit(mask, edit), cx);
        });
    }
}
