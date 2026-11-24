//! Tool-rail wiring: tool switching, the crop lifecycle and tool shortcuts.
//!
//! The tool state machine lives in `rohditor_engine::tools`; this module only
//! connects it to egui input, the edit history and preview scheduling.

use eframe::egui;
use rohditor_engine::tools::{ActiveTool, ToolKind};

use super::{Document, RohditorApp};
use crate::ui::crop::{self, CropPanelModel};
use crate::ui::tool_panel::{self, ToolPanelModel};
use crate::ui::tool_rail::{self, ToolRailModel};

/// Heading, histogram and document presence for the tool panel chrome.
pub(super) fn tool_panel_model(document: Option<&Document>, tool: ToolKind) -> ToolPanelModel<'_> {
    let (title, subtitle) = match tool {
        ToolKind::Edit => ("Develop", "NON-DESTRUCTIVE RAW ADJUSTMENTS"),
        ToolKind::Crop => ("Crop", "NON-DESTRUCTIVE GEOMETRY"),
        ToolKind::Masks => ("Masks", "LOCAL ADJUSTMENTS"),
    };
    ToolPanelModel {
        title,
        subtitle,
        has_document: document.is_some(),
        histogram: document.and_then(|document| document.core.preview.histogram.as_ref()),
    }
}

/// Tools need an open photo, so the rail is disabled without one.
fn rail_model(document_open: bool, tool: &ActiveTool) -> ToolRailModel {
    ToolRailModel {
        active: tool.kind(),
        enabled: document_open,
    }
}

impl RohditorApp {
    pub(super) fn show_tool_rail(&mut self, root: &mut egui::Ui) {
        let context = &root.ctx().clone();
        let model = rail_model(self.document.is_some(), &self.tool);
        if let Some(kind) = tool_rail::show(root, model).selected {
            self.select_tool(context, kind);
        }
    }

    pub(super) fn show_tool_panel(&mut self, root: &mut egui::Ui) {
        let context = &root.ctx().clone();
        self.handle_tool_shortcuts(context);
        self.handle_clipping_shortcut(context);
        match self.tool.kind() {
            ToolKind::Edit => self.show_adjustment_panel(root),
            ToolKind::Crop => self.show_crop_controls(root),
            ToolKind::Masks => self.show_masks_controls(root),
        }
    }

    /// Rail or shortcut request. The active tool's own button returns to Edit;
    /// changing tools applies what the old one drafted.
    fn select_tool(&mut self, context: &egui::Context, requested: ToolKind) {
        let target = self.tool.toggle_target(requested);
        let policy = self.settings.render_options().raw_crop_policy;
        let now = context.input(|input| input.time);
        let Some(document) = self.document.as_mut() else {
            return;
        };
        let changed = self.tool.switch_to(target, &mut document.core, policy, now);
        if self.tool.masks().is_none() {
            document.mask_overlay = None;
        }
        let id = document.core.id;
        if changed {
            self.queue_preview(context, id);
        }
    }

    /// E, C, M, Esc and Enter, plus the Masks tool's own keys, unless a text field is being
    /// edited.
    fn handle_tool_shortcuts(&mut self, context: &egui::Context) {
        if self.document.is_none() || context.egui_wants_keyboard_input() {
            return;
        }
        if self.handle_mask_shortcuts(context) {
            return;
        }
        let pressed =
            |key| context.input(|input| input.modifiers.is_none() && input.key_pressed(key));
        if pressed(egui::Key::E) {
            self.select_tool(context, ToolKind::Edit);
        } else if pressed(egui::Key::C) {
            self.select_tool(context, ToolKind::Crop);
        } else if pressed(egui::Key::M) {
            self.select_tool(context, ToolKind::Masks);
        } else if pressed(egui::Key::Escape) {
            self.leave_tool(context, false);
        } else if pressed(egui::Key::Enter) {
            self.leave_tool(context, true);
        }
    }

    /// Leaves the Crop tool for Edit. With `commit` the draft becomes one undo
    /// step; without it the draft is discarded. No-op in other tools.
    pub(super) fn leave_tool(&mut self, context: &egui::Context, commit: bool) {
        if self.tool.crop().is_none() {
            return;
        }
        let now = context.input(|input| input.time);
        let Some(document) = self.document.as_mut() else {
            return;
        };
        let changed = if commit {
            self.tool.apply(&mut document.core, now)
        } else {
            self.tool.cancel(&mut document.core, now)
        };
        let id = document.core.id;
        if changed {
            self.queue_preview(context, id);
        }
    }

    fn show_crop_controls(&mut self, root: &mut egui::Ui) {
        let context = &root.ctx().clone();
        let Some(session) = self.tool.crop() else {
            return;
        };
        let model = CropPanelModel {
            aspect: session.aspect(),
            locked: session.locked(),
            portrait: session.portrait(),
            dimensions: session.output_dimensions(),
            ready: self
                .document
                .as_ref()
                .is_some_and(|document| document.core.preview.full_frame_shown()),
        };
        let panel = tool_panel_model(self.document.as_ref(), ToolKind::Crop);
        let output = tool_panel::show(root, &panel, |ui| crop::show_controls(ui, &model));
        if let Some(session) = self.tool.crop_mut() {
            if let Some(aspect) = output.aspect {
                session.set_aspect(aspect);
            }
            if let Some(locked) = output.locked {
                session.set_locked(locked);
            }
            if output.toggle_orientation {
                session.toggle_orientation();
            }
            if output.reset {
                session.reset();
            }
        }
        if output.cancel {
            self.leave_tool(context, false);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rail_is_disabled_without_a_document() {
        let tool = ActiveTool::default();
        assert!(!rail_model(false, &tool).enabled);
        assert!(rail_model(true, &tool).enabled);
        assert_eq!(rail_model(true, &tool).active, ToolKind::Edit);
    }
}
