//! The commands of the Masks tool.

use gpui::{Context, Div, ModifiersChangedEvent, Stateful, Window, prelude::*};
use rohditor_engine::tools::{Bracket, BrushKeySteps, LayerEdit, MaskAction, ToolKind};

use super::Workspace;
use crate::actions::{
    AddToMask, DeleteMask, GrowBrush, HardenBrush, MasksTool, MoveMaskDown, MoveMaskUp, NewMask,
    PaintMask, PickMaskColors, RenameMask, ShrinkBrush, SoftenBrush, ToggleErase, ToggleMask,
    ToggleMaskOverlay,
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
            this.use_on_photo(action.0.new_mask(), window, cx);
        }))
        .on_action(cx.listener(|this, action: &AddToMask, window, cx| {
            this.use_on_photo(action.shape.add(action.mode), window, cx);
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
        .on_action(cx.listener(|this, _: &ToggleMaskOverlay, _, cx| {
            this.act_on_masks(MaskAction::ToggleOverlay, cx);
        }))
        .on_action(cx.listener(|this, _: &PaintMask, window, cx| {
            this.use_on_photo(MaskAction::PaintSelected, window, cx);
        }))
        .on_action(cx.listener(|this, _: &PickMaskColors, window, cx| {
            this.use_on_photo(MaskAction::PickColor, window, cx);
        }))
        .on_action(cx.listener(|this, _: &ToggleErase, _, cx| {
            this.act_on_masks(MaskAction::ToggleErase, cx);
        }))
        .on_action(cx.listener(|this, _: &GrowBrush, _, cx| {
            this.step_brush(Bracket::Close, false, cx);
        }))
        .on_action(cx.listener(|this, _: &ShrinkBrush, _, cx| {
            this.step_brush(Bracket::Open, false, cx);
        }))
        .on_action(cx.listener(|this, _: &SoftenBrush, _, cx| {
            this.step_brush(Bracket::Close, true, cx);
        }))
        .on_action(cx.listener(|this, _: &HardenBrush, _, cx| {
            this.step_brush(Bracket::Open, true, cx);
        }))
        // Alt erases instead of painting, and the other way round.
        .on_modifiers_changed(cx.listener(|this, event: &ModifiersChangedEvent, _, cx| {
            let held = event.modifiers.alt;
            this.session
                .update(cx, |session, cx| session.hold_erase(held, cx));
        }))
    }

    fn step_brush(&mut self, bracket: Bracket, shifted: bool, cx: &mut Context<Self>) {
        let mut steps = BrushKeySteps::default();
        steps.press(bracket, shifted);
        self.act_on_masks(MaskAction::BrushKeys(steps), cx);
    }

    /// Do what goes on with the pointer on the photo, which gets the focus.
    fn use_on_photo(&mut self, action: MaskAction, window: &mut Window, cx: &mut Context<Self>) {
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
