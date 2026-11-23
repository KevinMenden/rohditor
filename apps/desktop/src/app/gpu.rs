//! egui side of the GPU preview: registering the engine's display texture.
//!
//! The engine renders into a plain `wgpu::TextureView`
//! ([`DocumentPreview::gpu_display`]); this module hands it to egui's wgpu
//! renderer and keeps the document's [`PreviewTexture`] in step with it.

use eframe::egui;
use rohditor_engine::preview::WgpuContext;

use super::Document;
use crate::ui::viewport::PreviewTexture;

/// eframe's shared wgpu state, as the engine and egui each need it.
pub(super) struct EguiGpu {
    render_state: eframe::egui_wgpu::RenderState,
}

impl EguiGpu {
    /// The renderer's wgpu state, or `None` for a non-wgpu (glow) renderer.
    pub(super) fn new(context: &eframe::CreationContext<'_>) -> Option<Self> {
        context
            .wgpu_render_state
            .clone()
            .map(|render_state| Self { render_state })
    }

    /// The device the engine's GPU runtime must share with the UI.
    pub(super) fn wgpu_context(&self) -> WgpuContext {
        WgpuContext {
            adapter: self.render_state.adapter.clone(),
            device: self.render_state.device.clone(),
            queue: self.render_state.queue.clone(),
            target_format: self.render_state.target_format,
        }
    }

    /// Make the document's displayed texture match its preview state: register
    /// or update the engine's GPU frame, or release the texture when that frame
    /// is gone. Cheap when nothing changed.
    pub(super) fn sync_display(&self, document: &mut Document) {
        let generation = document.core.preview.display_generation();
        if document.synced_display_generation == generation {
            return;
        }
        document.synced_display_generation = generation;
        let Some(display) = document.core.preview.gpu_display() else {
            // The GPU frame is gone: release its texture, and the viewport's
            // reference to it if that is what it shows.
            if let Some(id) = document.gpu_texture_id.take() {
                self.render_state.renderer.write().free_texture(&id);
                if matches!(document.texture, Some(PreviewTexture::Gpu { id: shown, .. }) if shown == id)
                {
                    document.texture = None;
                }
            }
            return;
        };
        let size = egui::vec2(display.width as f32, display.height as f32);
        let mut renderer = self.render_state.renderer.write();
        let id = match document.gpu_texture_id {
            Some(id) => {
                renderer.update_egui_texture_from_wgpu_texture(
                    &self.render_state.device,
                    display.view,
                    wgpu::FilterMode::Linear,
                    id,
                );
                id
            }
            None => renderer.register_native_texture(
                &self.render_state.device,
                display.view,
                wgpu::FilterMode::Linear,
            ),
        };
        drop(renderer);
        document.gpu_texture_id = Some(id);
        document.texture = Some(PreviewTexture::Gpu { id, size });
        document.preview_pixels = None;
    }
}
