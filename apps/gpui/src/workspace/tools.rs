//! The right dock: the tools, and the panel of the active one.

use gpui::{Context, IntoElement, Role, StyleRefinement, div, prelude::*};
use rohditor_engine::tools::ToolKind;

use super::Workspace;
use crate::actions::{CropTool, EditTool};
use crate::assets::Icon;
use crate::theme::{DEVELOP_WIDTH, TAB_HEIGHT, theme};
use crate::ui::button::command_button;

impl Workspace {
    /// Change to the tool and show its panel.
    pub(super) fn show_tool(&mut self, tool: ToolKind, cx: &mut Context<Self>) {
        self.session
            .update(cx, |session, cx| session.select_tool(tool, cx));
        self.develop_open = true;
        cx.notify();
    }

    /// As [`Self::show_tool`], or leave the tool if it is active.
    pub(super) fn toggle_tool(&mut self, tool: ToolKind, cx: &mut Context<Self>) {
        let target = self.session.read(cx).tool().toggle_target(tool);
        self.show_tool(target, cx);
    }

    pub(super) fn tool_dock(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let session = self.session.read(cx);
        let ready = session.document().is_some_and(|d| d.frame.is_some());
        let active = session.tool().kind();
        let fill = StyleRefinement::default().flex_1().min_h_0().w_full();
        let panel = match active {
            ToolKind::Crop => self.crop.clone().cached(fill).into_any_element(),
            ToolKind::Edit | ToolKind::Masks => {
                self.develop.clone().cached(fill).into_any_element()
            }
        };
        let theme = theme();
        let selected = |tool| tool == active;
        div()
            .flex()
            .flex_col()
            .flex_none()
            .w(DEVELOP_WIDTH)
            .h_full()
            .child(
                div()
                    .id("tools")
                    .role(Role::Toolbar)
                    .aria_label("Tools")
                    .flex()
                    .flex_none()
                    .items_center()
                    .gap_1()
                    .h(TAB_HEIGHT)
                    .px_2()
                    .bg(theme.panel)
                    .border_l_1()
                    .border_b_1()
                    .border_color(theme.border)
                    .child(
                        command_button("tool-edit", Icon::Sliders, EditTool, ready)
                            .when(selected(ToolKind::Edit), |button| button.bg(theme.selected)),
                    )
                    .child(
                        command_button("tool-crop", Icon::Crop, CropTool, ready)
                            .when(selected(ToolKind::Crop), |button| button.bg(theme.selected)),
                    ),
            )
            .child(panel)
    }
}
