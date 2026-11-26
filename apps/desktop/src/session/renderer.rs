//! Runs the engine's preview operations for the open photo.

use std::time::Duration;

use rohditor_core::PreviewOptions;
use rohditor_engine::coordinator::RenderCoordinator;
use rohditor_engine::open_document::OpenDocument;
use rohditor_engine::preview::{
    DocumentPreview, PreviewOutcome, PreviewRequest, PreviewRuntime, ProcessorPreference,
    WgpuContext,
};
use rohditor_engine::settings::AppSettings;

use super::display::DisplaySlot;

pub struct Renderer {
    pub coordinator: RenderCoordinator,
    pub settings: AppSettings,
    runtime: PreviewRuntime,
}

impl Renderer {
    /// `gpu` is the device GPUI renders with; without one, or with the CPU `processor`, the
    /// CPU renders previews.
    pub fn new(
        coordinator: RenderCoordinator,
        settings: AppSettings,
        processor: ProcessorPreference,
        gpu: Option<WgpuContext>,
    ) -> Self {
        let runtime = PreviewRuntime::new(processor, gpu);
        if let Some(gpu) = runtime.gpu() {
            let wgpu = gpu.context();
            let configured = coordinator.configure_capture_device(
                wgpu.adapter.clone(),
                wgpu.device.clone(),
                wgpu.queue.clone(),
                wgpu.target_format,
            );
            if let Err(error) = configured {
                log::error!("the render worker did not take the GPU: {error}");
            }
        }
        Self {
            coordinator,
            settings,
            runtime,
        }
    }

    /// The adapter previews render on, or that the CPU does.
    pub fn processor(&self) -> String {
        self.runtime.gpu().map_or_else(
            || "CPU".to_owned(),
            |gpu| gpu.processor().capabilities().adapter_name.clone(),
        )
    }

    pub const fn runtime(&self) -> &PreviewRuntime {
        &self.runtime
    }

    pub fn gpu_allowed(&self) -> bool {
        self.runtime.preference() != ProcessorPreference::Cpu
    }

    pub fn preview_options(&self) -> PreviewOptions {
        PreviewOptions {
            render: self.settings.render_options(),
            ..PreviewOptions::default()
        }
    }

    pub fn queue_preview(&mut self, document: &mut OpenDocument, display: &mut DisplaySlot) {
        self.run(
            document,
            display,
            |runtime, coordinator, preview, request| {
                ((), runtime.queue_preview(coordinator, preview, request))
            },
        );
    }

    /// Run an engine preview operation and apply its outcome.
    pub fn run<R>(
        &mut self,
        document: &mut OpenDocument,
        display: &mut DisplaySlot,
        operation: impl FnOnce(
            &mut PreviewRuntime,
            &RenderCoordinator,
            &mut DocumentPreview,
            &PreviewRequest,
        ) -> (R, PreviewOutcome),
    ) -> R {
        let request = PreviewRequest {
            frame: document.frame.clone(),
            recipe: document.edits.recipe().clone(),
            revision: document.edits.revision(),
            options: self.preview_options(),
            histogram_always: false,
        };
        let (result, outcome) = operation(
            &mut self.runtime,
            &self.coordinator,
            &mut document.preview,
            &request,
        );
        outcome.error.apply(&mut document.error);
        outcome.warning.apply(&mut document.warning);
        display.sync(&document.preview);
        result
    }

    /// Advance the histogram readback and completion tracking of the displayed GPU frame; returns
    /// when to ask again.
    pub fn poll(&mut self, document: &mut OpenDocument) -> Option<Duration> {
        let completion = self.runtime.poll_completion(&mut document.preview);
        let histogram =
            self.runtime
                .poll_histogram(&mut document.preview, document.edits.revision(), false);
        [completion, histogram.repaint_after]
            .into_iter()
            .flatten()
            .min()
    }

    /// Let go of the GPU.
    pub fn release_gpu(&mut self) {
        self.runtime = PreviewRuntime::new(ProcessorPreference::Cpu, None);
    }
}
