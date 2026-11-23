//! The open photo: connects the toolkit-free engine to GPUI.
//!
//! Owns no widgets. Views observe the [`Session`] entity and call its methods; worker wake-ups
//! and timers arrive through [`Session::tick`].

mod display;
mod editing;
mod events;
mod export;
mod renderer;
mod saving;

use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Result, anyhow};
use gpui::{Context, EventEmitter, Task};
use rohditor_core::{WhiteBalanceCoordinates, default_recipe};
use rohditor_edit::EditRecipe;
use rohditor_engine::coordinator::RenderCoordinator;
use rohditor_engine::open_document::OpenDocument;
use rohditor_engine::persistence;
use rohditor_engine::preview::WgpuContext;
use rohditor_engine::settings::AppSettings;

pub use display::Display;
use display::DisplaySlot;
pub use editing::EditPhase;
use renderer::Renderer;
use saving::Saves;

use crate::wake::{self, Woken};

/// The session's background workers. Started before there is a window, so that a failure can be
/// reported.
pub struct SessionWorkers {
    coordinator: RenderCoordinator,
    saves: Saves,
    woken: Woken,
}

impl SessionWorkers {
    pub fn start() -> Result<Self> {
        let (wake, woken) = wake::channel();
        Ok(Self {
            coordinator: RenderCoordinator::new(wake).map_err(|e| anyhow!(e))?,
            saves: Saves::start()?,
            woken,
        })
    }
}

pub struct Session {
    renderer: Renderer,
    document: Option<OpenDocument>,
    display: DisplaySlot,
    saves: Saves,
    next_document_id: u64,
    next_export_id: u64,
    _wake: Task<()>,
    timer: Option<Task<()>>,
}

/// A photo was opened.
pub struct PhotoOpened(pub PathBuf);

impl EventEmitter<PhotoOpened> for Session {}

impl Session {
    /// `gpu` is the device GPUI renders with; without one the CPU renders previews.
    pub fn new(
        workers: SessionWorkers,
        settings: AppSettings,
        gpu: Option<WgpuContext>,
        cx: &mut Context<Self>,
    ) -> Self {
        Self {
            renderer: Renderer::new(workers.coordinator, settings, gpu),
            document: None,
            display: DisplaySlot::default(),
            saves: workers.saves,
            next_document_id: 1,
            next_export_id: 1,
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

    /// Seconds since start, the clock of the view's zoom feedback.
    pub fn now(&self) -> f64 {
        self.renderer.now()
    }

    pub fn as_shot(&self) -> Option<WhiteBalanceCoordinates> {
        self.document.as_ref()?.as_shot_coordinates()
    }

    /// The adapter previews render on, or that the CPU does.
    pub fn processor(&self) -> String {
        self.renderer.processor()
    }

    /// Whether the open photo needs the CPU.
    pub fn busy(&self) -> bool {
        self.document.as_ref().is_some_and(|document| {
            document.open_status.is_some()
                || document.preview.status.is_some()
                || document.export_status.is_some()
        })
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
        self.save_now();
        self.display.clear();
        if let Some(document) = self.document.take() {
            self.renderer.coordinator.abandon(document.id);
        }
        self.display.remove_retired(cx);
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
        self.saves.process_results(self.document.as_mut());
        let mut delays = Vec::new();
        if let Some(document) = self.document.as_mut() {
            if document.sync_camera_look(&self.renderer.coordinator) {
                self.renderer.queue_preview(document, &mut self.display);
            }
            delays.extend(self.renderer.poll(document));
            delays.extend(self.saves.autosave(document));
        }
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
        self.display.remove_retired(cx);
        cx.notify();
    }
}
