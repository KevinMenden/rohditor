//! GPUI's wgpu device, which the engine's GPU preview shares.

use rohditor_engine::preview::WgpuContext;

/// The device GPUI renders with, once it has a window.
pub fn shared_context() -> Option<WgpuContext> {
    let context = gpui_wgpu::current_context()?;
    Some(WgpuContext {
        adapter: context.adapter.clone(),
        device: (*context.device).clone(),
        queue: (*context.queue).clone(),
        // Only recorded for diagnostics; GPUI prefers this surface format.
        target_format: wgpu::TextureFormat::Bgra8Unorm,
    })
}
