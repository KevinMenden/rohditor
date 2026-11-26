//! The active tool: changing it, and applying or discarding what it drafted.

use gpui::Context;
use rohditor_engine::open_document::OpenDocument;
use rohditor_engine::tools::{ActiveTool, CropToolSession, ToolKind};

use super::Session;

impl Session {
    pub fn tool(&self) -> &ActiveTool {
        &self.tool
    }

    /// Change tools, applying what the current one drafted.
    pub fn select_tool(&mut self, target: ToolKind, cx: &mut Context<Self>) {
        let policy = self.renderer.settings.render_options().raw_crop_policy;
        self.change_tool(
            |tool, document| tool.switch_to(target, document, policy),
            cx,
        );
    }

    /// Return to Edit, applying what the tool drafted.
    pub fn apply_tool(&mut self, cx: &mut Context<Self>) {
        self.change_tool(|tool, document| tool.apply(document), cx);
    }

    /// Return to Edit, discarding what the tool drafted.
    pub fn cancel_tool(&mut self, cx: &mut Context<Self>) {
        self.change_tool(|tool, document| tool.cancel(document), cx);
    }

    /// Whether the active tool drafts something to apply or cancel.
    pub fn drafting(&self) -> bool {
        self.tool.kind() == ToolKind::Crop
    }

    /// The crop being drafted, once the photo is shown without its crop.
    pub fn crop_draft(&self) -> Option<&CropToolSession> {
        let document = self.document.as_ref()?;
        self.tool
            .crop()
            .filter(|_| document.preview.full_frame_shown())
    }

    /// Change the crop being drafted, if the Crop tool is active.
    pub fn edit_crop(&mut self, change: impl FnOnce(&mut CropToolSession), cx: &mut Context<Self>) {
        if let Some(crop) = self.tool.crop_mut() {
            change(crop);
            cx.notify();
        }
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
