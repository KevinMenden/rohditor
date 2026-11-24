//! The active tool: changing it, and applying or discarding what it drafted.

use gpui::Context;
use rohditor_engine::open_document::OpenDocument;
use rohditor_engine::tools::{ActiveTool, ToolKind};

use super::Session;

impl Session {
    pub fn tool(&self) -> &ActiveTool {
        &self.tool
    }

    /// Change tools, applying what the current one drafted.
    pub fn select_tool(&mut self, target: ToolKind, cx: &mut Context<Self>) {
        let policy = self.renderer.settings.render_options().raw_crop_policy;
        let now = self.now();
        self.change_tool(
            |tool, document| tool.switch_to(target, document, policy, now),
            cx,
        );
    }

    /// Return to Edit, applying what the tool drafted.
    pub fn apply_tool(&mut self, cx: &mut Context<Self>) {
        let now = self.now();
        self.change_tool(|tool, document| tool.apply(document, now), cx);
    }

    /// Return to Edit, discarding what the tool drafted.
    pub fn cancel_tool(&mut self, cx: &mut Context<Self>) {
        let now = self.now();
        self.change_tool(|tool, document| tool.cancel(document, now), cx);
    }

    /// Whether the active tool drafts something to apply or cancel.
    pub fn drafting(&self) -> bool {
        self.tool.kind() == ToolKind::Crop
    }

    fn change_tool(
        &mut self,
        change: impl FnOnce(&mut ActiveTool, &mut OpenDocument) -> bool,
        cx: &mut Context<Self>,
    ) {
        if let Some(document) = self.document.as_mut()
            && change(&mut self.tool, document)
        {
            self.queue_preview();
        }
        self.tick(cx);
    }
}

#[cfg(test)]
impl Session {
    pub fn draft_aspect(
        &mut self,
        aspect: rohditor_engine::tools::CropAspect,
        cx: &mut Context<Self>,
    ) {
        if let Some(crop) = self.tool.crop_mut() {
            crop.set_aspect(aspect);
            cx.notify();
        }
    }
}
