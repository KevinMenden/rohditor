//! Tool-rail wiring: tool switching, the crop lifecycle and tool shortcuts.
//!
//! The tool state machine lives in `rohditor_engine::tools`; this module only
//! connects it to egui input, the edit history and preview scheduling.

use std::sync::Arc;

use eframe::egui;
use rohditor_engine::document::PreviewIntent;
use rohditor_engine::tools::{ActiveTool, CropToolSession, ToolKind};

use super::{Document, RohditorApp};
use crate::ui::crop::{self, CropPanelModel};
use crate::ui::tool_panel::{self, ToolPanelModel};
use crate::ui::tool_rail::{self, ToolRailModel};
use crate::ui::viewport::PreviewTexture;

/// Heading, histogram and document presence for the tool panel chrome.
pub(super) fn tool_panel_model(document: Option<&Document>, tool: ToolKind) -> ToolPanelModel<'_> {
    let (title, subtitle) = match tool {
        ToolKind::Edit => ("Develop", "NON-DESTRUCTIVE RAW ADJUSTMENTS"),
        ToolKind::Crop => ("Crop", "NON-DESTRUCTIVE GEOMETRY"),
    };
    ToolPanelModel {
        title,
        subtitle,
        has_document: document.is_some(),
        histogram: document.and_then(|document| document.histogram.as_ref()),
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
    pub(super) fn show_tool_rail(&mut self, context: &egui::Context) {
        let model = rail_model(self.document.is_some(), &self.tool);
        if let Some(kind) = tool_rail::show(context, model).selected {
            self.select_tool(context, kind);
        }
    }

    pub(super) fn show_tool_panel(&mut self, context: &egui::Context) {
        self.handle_tool_shortcuts(context);
        match self.tool.kind() {
            ToolKind::Edit => self.show_adjustment_panel(context),
            ToolKind::Crop => self.show_crop_controls(context),
        }
    }

    /// Rail or shortcut request. The active tool's own button returns to Edit;
    /// leaving Crop commits its draft.
    fn select_tool(&mut self, context: &egui::Context, requested: ToolKind) {
        let target = self.tool.toggle_target(requested);
        if target == self.tool.kind() {
            return;
        }
        self.leave_tool(context, true);
        if target == ToolKind::Crop {
            self.enter_crop_tool(context);
        }
    }

    /// E, C, Esc and Enter, unless a text field is being edited.
    fn handle_tool_shortcuts(&mut self, context: &egui::Context) {
        if self.document.is_none() || context.wants_keyboard_input() {
            return;
        }
        let pressed =
            |key| context.input(|input| input.modifiers.is_none() && input.key_pressed(key));
        if pressed(egui::Key::E) {
            self.select_tool(context, ToolKind::Edit);
        } else if pressed(egui::Key::C) {
            self.select_tool(context, ToolKind::Crop);
        } else if pressed(egui::Key::Escape) {
            self.leave_tool(context, false);
        } else if pressed(egui::Key::Enter) {
            self.leave_tool(context, true);
        }
    }

    fn enter_crop_tool(&mut self, context: &egui::Context) {
        if self.tool.crop().is_some() {
            return;
        }
        let request = self.document.as_mut().and_then(|document| {
            let frame = document.frame.as_ref().map(Arc::clone)?;
            document.source_scale_requested = false;
            document.view.fit(context.input(|input| input.time));
            let (width, height) = document
                .texture
                .as_ref()
                .map(PreviewTexture::dimensions)
                .unwrap_or((frame.info.width, frame.info.height));
            let session =
                CropToolSession::new(document.edits.recipe().geometry.crop, width, height);
            let ticket = document.begin_preview_request(PreviewIntent::CropToolFullFrame);
            let recipe = document.edits.recipe().clone();
            document.preview_status =
                Some((ticket.revision, "Preparing full image for crop".to_owned()));
            Some((document.id, ticket, frame, recipe, session))
        });
        let Some((document_id, ticket, frame, recipe, session)) = request else {
            return;
        };
        self.pending_white_balance_pick = None;
        // Entering Crop drops any active picker.
        self.tool.enter_crop(session);
        let options = self.preview_options();
        if let Err(error) = self
            .coordinator
            .crop_tool_preview(ticket, frame, recipe, options)
        {
            self.tool = ActiveTool::default();
            if let Some(document) = self
                .document
                .as_mut()
                .filter(|document| document.id == document_id)
            {
                document.preview_status = None;
                document.error = Some(error);
            }
        }
    }

    /// Leaves the Crop tool for Edit. With `commit` the draft becomes one undo
    /// step; without it the draft is discarded. No-op in other tools.
    pub(super) fn leave_tool(&mut self, context: &egui::Context, commit: bool) {
        if self.tool.crop().is_none() {
            return;
        }
        let tool = std::mem::take(&mut self.tool);
        let mut changed_document = None;
        if let Some(document) = self.document.as_mut()
            && let Some(next) = tool.leave(document.edits.recipe(), commit)
            && document.edits.set_discrete(next)
        {
            document.notice = None;
            changed_document = Some(document.id);
        }
        let document_id =
            changed_document.or_else(|| self.document.as_ref().map(|document| document.id));
        if let Some(document_id) = document_id {
            self.queue_preview(context, document_id);
        }
    }

    fn show_crop_controls(&mut self, context: &egui::Context) {
        let Some(session) = self.tool.crop() else {
            return;
        };
        let model = CropPanelModel {
            aspect: session.aspect(),
            locked: session.locked(),
            portrait: session.portrait(),
            dimensions: session.output_dimensions(),
            ready: session.full_frame_ready(),
        };
        let panel = tool_panel_model(self.document.as_ref(), ToolKind::Crop);
        let output = tool_panel::show(context, &panel, |ui| crop::show_controls(ui, &model));
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
    use rohditor_edit::EditRecipe;
    use rohditor_engine::document::EditSession;

    use super::*;

    #[test]
    fn rail_is_disabled_without_a_document() {
        let tool = ActiveTool::default();
        assert!(!rail_model(false, &tool).enabled);
        assert!(rail_model(true, &tool).enabled);
        assert_eq!(rail_model(true, &tool).active, ToolKind::Edit);
    }

    #[test]
    fn a_committed_crop_session_is_one_undo_step() {
        let mut edits = EditSession::from_saved(EditRecipe::default());
        let mut session = CropToolSession::new(None, 6000, 4000);
        session.set_full_dimensions(6000, 4000);
        // Several draft changes before leaving the tool.
        session.set_aspect(rohditor_engine::tools::CropAspect::Square);
        session.set_aspect(rohditor_engine::tools::CropAspect::ThreeTwo);

        let next = ActiveTool::Crop(session)
            .leave(edits.recipe(), true)
            .expect("modified crop commits");
        assert!(edits.set_discrete(next));
        assert!(edits.recipe().geometry.crop.is_some());
        assert!(edits.undo());
        assert_eq!(edits.recipe().geometry.crop, None);
        assert!(!edits.can_undo());
    }
}
