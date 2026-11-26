//! The open photo: connects the toolkit-free engine to GPUI.
//!
//! Owns no widgets. Views observe the [`Session`] entity and call its methods; worker wake-ups
//! and timers arrive through [`Session::tick`].

mod clipping;
mod display;
mod editing;
mod events;
mod export;
mod mask_tint;
mod masks;
mod optics;
mod picking;
mod presets;
mod profiles;
mod renderer;
mod saving;
mod settings;
mod tools;
mod upload;
mod zoom;

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Result, anyhow};
use gpui::{Context, EventEmitter, Task};
use rohditor_core::default_recipe;
use rohditor_edit::EditRecipe;
use rohditor_engine::camera_profiles::CameraProfileRegistry;
use rohditor_engine::catalog::CatalogState;
use rohditor_engine::coordinator::RenderCoordinator;
use rohditor_engine::diagnostics::{Diagnostics, DiagnosticsSources};
use rohditor_engine::export_options::ExportOptions;
use rohditor_engine::open_document::OpenDocument;
use rohditor_engine::persistence;
use rohditor_engine::presets::PresetStore;
use rohditor_engine::preview::WgpuContext;
use rohditor_engine::tools::ActiveTool;
use rohditor_neural::NeuralDemosaicer;

pub use display::Display;
use display::DisplaySlot;
pub use editing::EditPhase;
pub use presets::PresetEntry;
use presets::Presets;
use renderer::Renderer;
use saving::Saves;
use settings::SettingsFile;
pub use settings::{StartupChoices, StartupSettings};

use crate::wake::{self, Woken};

/// The session's background workers. Started before there is a window, so that a failure can be
/// reported.
pub struct SessionWorkers {
    coordinator: RenderCoordinator,
    saves: Saves,
    woken: Woken,
}

impl SessionWorkers {
    /// `neural` is the model of the neural demosaic, if one is installed.
    pub fn start(neural: Option<Arc<NeuralDemosaicer>>) -> Result<Self> {
        let (wake, woken) = wake::channel();
        Ok(Self {
            coordinator: RenderCoordinator::with_neural(wake, neural).map_err(|e| anyhow!(e))?,
            saves: Saves::start()?,
            woken,
        })
    }
}

pub struct Session {
    renderer: Renderer,
    settings_file: SettingsFile,
    document: Option<OpenDocument>,
    display: DisplaySlot,
    saves: Saves,
    profiles: CameraProfileRegistry,
    presets: Presets,
    tool: ActiveTool,
    /// A picked mixer colour that is not announced yet.
    picked_channel: Option<usize>,
    mask_tint: mask_tint::MaskTint,
    next_document_id: u64,
    next_export_id: u64,
    /// What the export dialog chose last.
    export_options: ExportOptions,
    _wake: Task<()>,
    timer: Option<Task<()>>,
}

/// A photo was opened.
pub struct PhotoOpened(pub PathBuf);

impl EventEmitter<PhotoOpened> for Session {}

/// A colour of the mixer was picked on the photo.
pub struct MixerColorPicked(pub usize);

impl EventEmitter<MixerColorPicked> for Session {}

impl Session {
    /// `gpu` is the device GPUI renders with; without one the CPU renders previews.
    pub fn new(
        workers: SessionWorkers,
        settings: StartupSettings,
        profiles: CameraProfileRegistry,
        presets: PresetStore,
        gpu: Option<WgpuContext>,
        cx: &mut Context<Self>,
    ) -> Self {
        let (settings, processor, settings_file) = SettingsFile::new(settings);
        Self {
            renderer: Renderer::new(workers.coordinator, settings, processor, gpu),
            settings_file,
            document: None,
            display: DisplaySlot::default(),
            saves: workers.saves,
            profiles,
            presets: Presets::new(presets),
            tool: ActiveTool::default(),
            picked_channel: None,
            mask_tint: mask_tint::MaskTint::default(),
            next_document_id: 1,
            next_export_id: 1,
            export_options: export::default_options(),
            _wake: workers.woken.run(cx, Self::tick),
            timer: None,
        }
    }

    pub fn document(&self) -> Option<&OpenDocument> {
        self.document.as_ref()
    }

    pub fn document_mut(&mut self) -> Option<&mut OpenDocument> {
        self.document.as_mut()
    }

    pub fn display(&self) -> Option<&Display> {
        self.display.get()
    }

    /// The adapter previews render on, or that the CPU does.
    pub fn processor(&self) -> String {
        self.renderer.processor()
    }

    /// What the preview pipeline did last, for the diagnostics panel. `catalog` is the shown
    /// folder's.
    pub fn diagnostics(&self, catalog: &CatalogState) -> Diagnostics {
        rohditor_engine::diagnostics::build(&DiagnosticsSources {
            ui_renderer: "GPUI",
            runtime: self.renderer.runtime(),
            queue: self.renderer.coordinator.preview_queue_stats(),
            document: self.document.as_ref(),
            catalog: Some(catalog),
            output_policy: self.renderer.settings.output_policy(),
        })
    }

    /// Whether the open photo needs the CPU.
    pub fn busy(&self) -> bool {
        self.document.as_ref().is_some_and(|document| {
            document.open_status.is_some()
                || document.preview.status.is_some()
                || document.export_status.is_some()
        })
    }

    /// Whether the picture is final for now: no photo, a failed one, or a settled preview.
    pub fn preview_settled(&self) -> bool {
        self.document
            .as_ref()
            .is_none_or(|document| document.error.is_some() || document.is_settled())
    }

    pub fn open_file(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        if self.document.as_ref().is_some_and(|open| open.path == path) {
            return;
        }
        self.close_document(cx);
        let (recipe, warning, new_photo) = self.stored_recipe(&path);
        let id = self.next_document_id;
        self.next_document_id += 1;
        let mut document = OpenDocument::opening(id, path.clone(), recipe, warning);
        if new_photo {
            document.mark_new_photo(self.renderer.settings.auto_noise_reduction());
        }
        if let Err(error) = self.renderer.coordinator.open(id, path.clone()) {
            document.open_status = None;
            document.error = Some(error);
        }
        self.document = Some(document);
        cx.emit(PhotoOpened(path));
        cx.notify();
    }

    /// The recipe to open `path` with, a warning, and whether it has no saved edits.
    fn stored_recipe(&self, path: &Path) -> (EditRecipe, Option<String>, bool) {
        // Edits still on their way to disk are newer than the sidecar.
        if let Some(recipe) = self.saves.queued_recipe(path) {
            return (recipe.clone(), None, false);
        }
        match persistence::load_recipe(path) {
            Ok(Some(recipe)) => (recipe, None, false),
            Ok(None) => (default_recipe(None), None, true),
            Err(error) => (
                default_recipe(None),
                Some(format!(
                    "Could not load saved edits: {error}; using the defaults."
                )),
                true,
            ),
        }
    }

    pub fn close_document(&mut self, cx: &mut Context<Self>) {
        if let Some(document) = self.document.as_mut() {
            // Nothing is shown any more, so nothing has to be rendered.
            let _ = self.tool.apply(document);
        }
        self.tool = ActiveTool::default();
        self.save_now();
        self.display.clear();
        self.mask_tint.clear();
        if let Some(document) = self.document.take() {
            self.renderer.coordinator.abandon(document.id);
        }
        cx.notify();
    }

    /// Save without waiting for the autosave.
    pub fn save_now(&mut self) {
        if let Some(document) = self.document.as_mut() {
            self.saves.save(document);
        }
    }

    /// Release everything that uses GPUI's device. Must run before GPUI shuts down: its wgpu
    /// instance may not outlive the display connection.
    pub fn shut_down(&mut self, cx: &mut Context<Self>) {
        self.close_document(cx);
        self.saves.shut_down();
        self.renderer.release_gpu();
    }

    fn queue_preview(&mut self) {
        if let Some(document) = self.document.as_mut() {
            self.renderer.queue_preview(document, &mut self.display);
        }
    }

    /// Process everything that arrived since the last call and schedule the next one.
    pub fn tick(&mut self, cx: &mut Context<Self>) {
        self.process_worker_events();
        self.announce_pick(cx);
        self.saves.process_results(self.document.as_mut());
        let mut delays = Vec::new();
        if let Some(document) = self.document.as_mut() {
            self.tool.sync(document.edits.recipe());
            if document.sync_camera_look(&self.renderer.coordinator) {
                self.renderer.queue_preview(document, &mut self.display);
            }
            delays.extend(self.renderer.poll(document));
            delays.extend(self.saves.autosave(document));
        }
        self.refresh_mask_tint();
        if self.saves.pending() {
            // The save worker has no wake-up of its own.
            delays.push(Duration::from_millis(100));
        }
        self.timer = delays.into_iter().min().map(|delay| {
            cx.spawn(async move |this, cx| {
                cx.background_executor().timer(delay).await;
                // Gone only while the application quits.
                let _ = this.update(cx, Self::tick);
            })
        });
        cx.notify();
    }
}

#[cfg(test)]
impl Session {
    /// Show these pixels as if the CPU had rendered them.
    pub fn show_test_image(
        &mut self,
        width: usize,
        height: usize,
        rgb: &[u8],
        cx: &mut Context<Self>,
    ) {
        self.display
            .show_cpu_image(display::FrameKind::Cpu, width, height, rgb);
        cx.notify();
    }
}
