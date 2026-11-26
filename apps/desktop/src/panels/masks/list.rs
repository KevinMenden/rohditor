//! The list of the masks, with the shapes of the selected one below it.

use gpui::{
    AnyElement, Context, Div, IntoElement, MouseButton, MouseDownEvent, Role, SharedString,
    Stateful, Window, div, prelude::*, px,
};
use rohditor_edit::MaskMode;
use rohditor_engine::tools::{LayerEdit, MaskAction};

use super::MasksPanel;
use super::control::Control;
use super::model::{Component, Mask, Selected};
use crate::actions::{MoveMaskDown, MoveMaskUp, PaintMask, RenameMask, ToggleMask};
use crate::assets::Icon;
use crate::panels::controls::FocusControls as _;
use crate::theme::{RADIUS, SMALL_FONT_SIZE, theme};
use crate::ui::button::icon_button;
use crate::ui::dropdown::dropdown;
use crate::ui::keys::{Cancel, Confirm};
use crate::ui::menu::MenuEntry;
use crate::ui::test_id::TestId as _;

impl MasksPanel {
    pub(super) fn mask_list(&self, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let mut rows = Vec::new();
        for mask in &self.model.masks {
            rows.push(self.mask_row(mask, cx));
            let selected = self.model.selected.as_ref();
            if let Some(selected) = selected.filter(|selected| selected.mask == mask.id) {
                rows.extend(self.component_rows(selected, cx));
            }
        }
        rows
    }

    /// A row of the list, which is also a button.
    fn list_row(
        &self,
        control: Control,
        id: String,
        label: SharedString,
        selected: bool,
        cx: &mut Context<Self>,
    ) -> Stateful<Div> {
        let theme = theme();
        div()
            .id(SharedString::from(id.clone()))
            .role(Role::Button)
            .aria_label(label)
            .aria_selected(selected)
            .track_focus(self.controls.focus.control(control))
            .test_id(id)
            .relative()
            .child(self.revealer(control, cx))
            .flex()
            .items_center()
            .gap_1()
            .h(px(26.0))
            .pr_1()
            .rounded(RADIUS)
            .border_1()
            .border_color(gpui::transparent_black())
            .focus_visible(move |style| style.border_color(theme.accent))
            .cursor_pointer()
            .text_color(if selected { theme.text } else { theme.muted })
            .when(selected, |row| row.bg(theme.selected))
            .when(!selected, |row| {
                row.hover(move |style| style.bg(theme.hover))
            })
    }

    fn mask_row(&self, mask: &Mask, cx: &mut Context<Self>) -> AnyElement {
        let theme = theme();
        let Mask { id, enabled, .. } = *mask;
        let selected = self.model.selected_mask() == Some(id);
        let renaming = self
            .rename
            .as_ref()
            .filter(|rename| rename.mask == id)
            .map(|rename| rename.input.clone());
        let (eye, hint) = if enabled {
            (Icon::Eye, "Disable the mask")
        } else {
            (Icon::EyeOff, "Enable the mask")
        };
        let toggle = icon_button(
            format!("toggle-mask-{id}"),
            eye,
            hint,
            true,
            cx.listener(move |this, _, _, cx| {
                this.act(MaskAction::Edit(id, LayerEdit::Enabled(!enabled)), cx);
            }),
        );
        let delete = icon_button(
            format!("delete-mask-{id}"),
            Icon::Close,
            "Delete the mask",
            true,
            cx.listener(move |this, _, _, cx| this.act(MaskAction::DeleteLayer(id), cx)),
        );
        let row = self
            .list_row(
                Control::Mask(id),
                format!("mask:{id}"),
                mask.name.clone(),
                selected,
                cx,
            )
            .child(toggle.tab_stop(false));
        let row = match renaming {
            Some(input) => row
                .on_action(cx.listener(|this, _: &Confirm, window, cx| {
                    this.stop_rename(true, window, cx);
                }))
                .on_action(cx.listener(|this, _: &Cancel, window, cx| {
                    this.stop_rename(false, window, cx);
                }))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .px_1()
                        .rounded(px(2.0))
                        .bg(theme.editor)
                        .child(input),
                ),
            None => row
                .on_mouse_down(MouseButton::Left, |_, window, _| window.prevent_default())
                .on_click(
                    cx.listener(move |this, event: &gpui::ClickEvent, window, cx| {
                        if event.click_count() == 2 {
                            this.start_rename(id, window, cx);
                        } else {
                            this.act(MaskAction::SelectLayer(id), cx);
                        }
                    }),
                )
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .when(!enabled, |name| name.text_color(theme.faint))
                        .child(mask.name.clone()),
                ),
        };
        row.when(selected, |row| row.child(delete.tab_stop(false)))
            .on_mouse_down(
                MouseButton::Right,
                cx.listener(move |this, event, window, cx| this.mask_menu(id, event, window, cx)),
            )
            .into_any_element()
    }

    fn mask_menu(
        &mut self,
        mask: u32,
        event: &MouseDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // What the commands act on.
        if self.model.selected_mask() != Some(mask) {
            self.act(MaskAction::SelectLayer(mask), cx);
        }
        let panel = cx.entity();
        let entries = vec![
            MenuEntry::command(RenameMask),
            MenuEntry::command(ToggleMask),
            MenuEntry::command(MoveMaskUp),
            MenuEntry::command(MoveMaskDown),
            MenuEntry::new("Delete", move |_, cx| {
                panel.update(cx, |panel, cx| {
                    panel.act(MaskAction::DeleteLayer(mask), cx);
                });
            }),
        ];
        self.overlays.update(cx, |overlays, cx| {
            overlays.show_menu(event.position, entries, window, cx);
        });
    }

    fn component_rows(&self, selected: &Selected, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let mut rows: Vec<_> = std::iter::zip(0.., &selected.components)
            .map(|(index, component)| self.component_row(index, component, selected, cx))
            .collect();
        let add = div().flex().pl_6().pb_1().child(self.new_component());
        rows.push(add.into_any_element());
        rows
    }

    fn component_row(
        &self,
        index: usize,
        component: &Component,
        selected: &Selected,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = theme();
        let mask = selected.mask;
        let chosen = selected.component == Some(index);
        let inverted = component.inverted;
        let invert = icon_button(
            format!("invert-component-{index}"),
            Icon::Invert,
            "Invert: select what is outside the shape",
            true,
            cx.listener(move |this, _, _, cx| {
                let edit = LayerEdit::ComponentInvert {
                    component: index,
                    invert: !inverted,
                };
                this.act(MaskAction::Edit(mask, edit), cx);
            }),
        )
        .aria_toggled(if inverted {
            gpui::Toggled::True
        } else {
            gpui::Toggled::False
        })
        .when(inverted, |button| button.bg(theme.fill));
        let delete = icon_button(
            format!("delete-component-{index}"),
            Icon::Close,
            "Delete the shape",
            true,
            cx.listener(move |this, _, _, cx| this.act(MaskAction::DeleteComponent(index), cx)),
        );
        let paint = component.brush.then(|| {
            icon_button(
                format!("paint-component-{index}"),
                Icon::Brush,
                "Paint into it",
                true,
                cx.listener(move |this, _, window, cx| {
                    this.act(MaskAction::SelectComponent(index), cx);
                    window.dispatch_action(Box::new(PaintMask), cx);
                }),
            )
            .tab_stop(false)
        });
        // The first has nothing to combine with, and is the mask when it is the only one.
        let mode = (index > 0).then(|| self.component_mode(index, component.mode, mask, cx));
        let only = selected.components.len() == 1;
        self.list_row(
            Control::Component(index),
            format!("component:{index}"),
            component.label.into(),
            chosen && !only,
            cx,
        )
        .ml_5()
        .pl_1()
        .text_size(SMALL_FONT_SIZE)
        .on_mouse_down(MouseButton::Left, |_, window, _| window.prevent_default())
        .on_click(cx.listener(move |this, _, _, cx| {
            this.act(MaskAction::SelectComponent(index), cx);
        }))
        .child(div().flex_1().min_w_0().truncate().child(component.label))
        .children(paint)
        .children(mode)
        .child(invert.tab_stop(false))
        .when(!only, |row| row.child(delete.tab_stop(false)))
        .into_any_element()
    }

    /// How the component combines with the ones before it.
    fn component_mode(
        &self,
        index: usize,
        mode: MaskMode,
        mask: u32,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        let panel = cx.entity();
        dropdown(
            format!("component-mode-{index}"),
            "How it combines with the shapes above",
            mode.label(),
            self.overlays.clone(),
            move |_, _| {
                let modes = MaskMode::ALL.into_iter().map(|choice| {
                    let panel = panel.clone();
                    let edit = MaskAction::Edit(
                        mask,
                        LayerEdit::ComponentMode {
                            component: index,
                            mode: choice,
                        },
                    );
                    MenuEntry::new(choice.label(), move |_, cx| {
                        panel.update(cx, |panel, cx| panel.act(edit.clone(), cx));
                    })
                    .checked(choice == mode)
                });
                modes.collect()
            },
        )
        // Not the row's.
        .on_mouse_down(MouseButton::Left, |_, window, cx| {
            window.prevent_default();
            cx.stop_propagation();
        })
    }
}
