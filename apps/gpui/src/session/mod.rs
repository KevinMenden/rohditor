//! The open photo and folder: connects the toolkit-free engine to GPUI.
//!
//! Owns no widgets. Views observe the [`Session`] entity and call its methods; worker wake-ups
//! and timers arrive through [`Session::tick`].

mod display;
mod events;
mod export;
mod library;
mod saving;

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{Result, anyhow};
use futures::StreamExt as _;
use futures::channel::mpsc;
use gpui::{Context, Task};
use rohditor_core::{PreviewOptions, WhiteBalanceCoordinates, auto_tone, default_recipe};
use rohditor_edit::EditRecipe;
use rohditor_engine::catalog::library::LibraryState;
use rohditor_engine::catalog::{CatalogCoordinator, CatalogState};
use rohditor_engine::coordinator::RenderCoordinator;
use rohditor_engine::document::EditSession;
use rohditor_engine::open_document::OpenDocument;
use rohditor_engine::persistence::{self, SaveWorker};
use rohditor_engine::preview::{
    DocumentPreview, PreviewOutcome, PreviewRequest, PreviewRuntime, ProcessorPreference,
    WgpuContext,
};
use rohditor_engine::settings::AppSettings;
use rohditor_engine::{WakeFn, session as engine_session};

use display::RetiredImages;
pub use display::{AtlasImage, Display};

/// Where the session stores settings and the last folder.
#[derive(Clone, Default)]
pub struct Directories {
    pub config: Option<PathBuf>,
}

pub struct Session {
    coordinator: RenderCoordinator,
    runtime: PreviewRuntime,
    settings: AppSettings,
    directories: Directories,
    document: Option<OpenDocument>,
    display: Option<Display>,
    synced_generation: u64,
    retired: RetiredImages,
    catalog_coordinator: CatalogCoordinator,
    pub catalog: CatalogState,
    pub library: LibraryState<AtlasImage>,
    saves: SaveWorker,
    /// Recipes handed to the save worker and not yet confirmed, by source file.
    queued_saves: HashMap<PathBuf, (u64, EditRecipe)>,
    next_document_id: u64,
    next_export_id: u64,
    started: Instant,
    _wake: Task<()>,
    timer: Option<Task<()>>,
}

/// Whether an edit comes from a drag still in progress.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EditPhase {
    Dragging,
    Finished,
}

impl Session {
    /// `gpu` is the device GPUI renders with; without one the CPU renders previews.
    pub fn new(
        gpu: Option<WgpuContext>,
        directories: Directories,
        cx: &mut Context<Self>,
    ) -> Result<Self> {
        let settings = rohditor_engine::settings::load(directories.config.as_deref()).settings;
        let preference = if settings.cpu_only() {
            ProcessorPreference::Cpu
        } else {
            ProcessorPreference::Auto
        };
        let runtime = PreviewRuntime::new(preference, gpu);

        let (wake, mut woken) = mpsc::unbounded::<()>();
        let wake: WakeFn = Arc::new(move || {
            // The receiver only goes away while the application quits.
            let _ = wake.unbounded_send(());
        });
        let wake_task = cx.spawn(async move |this, cx| {
            while woken.next().await.is_some() {
                // One tick handles everything queued so far.
                while woken.try_recv().is_ok() {}
                if this.update(cx, |session, cx| session.tick(cx)).is_err() {
                    break;
                }
            }
        });

        let coordinator = RenderCoordinator::new(Arc::clone(&wake)).map_err(|e| anyhow!(e))?;
        if let Some(gpu) = runtime.gpu() {
            let wgpu = gpu.context();
            coordinator
                .configure_capture_device(
                    wgpu.adapter.clone(),
                    wgpu.device.clone(),
                    wgpu.queue.clone(),
                    wgpu.target_format,
                )
                .map_err(|e| anyhow!(e))?;
        }
        Ok(Self {
            coordinator,
            runtime,
            settings,
            directories,
            document: None,
            display: None,
            synced_generation: 0,
            retired: RetiredImages::default(),
            catalog_coordinator: CatalogCoordinator::new(wake).map_err(|e| anyhow!(e))?,
            catalog: CatalogState::default(),
            library: LibraryState::default(),
            saves: SaveWorker::new()?,
            queued_saves: HashMap::new(),
            next_document_id: 1,
            next_export_id: 1,
            started: Instant::now(),
            _wake: wake_task,
            timer: None,
        })
    }

    pub fn document(&self) -> Option<&OpenDocument> {
        self.document.as_ref()
    }

    pub fn document_mut(&mut self) -> Option<&mut OpenDocument> {
        self.document.as_mut()
    }

    pub fn display(&self) -> Option<&Display> {
        self.display.as_ref()
    }

    /// Seconds since start, the clock of the view's zoom feedback.
    pub fn now(&self) -> f64 {
        self.started.elapsed().as_secs_f64()
    }

    pub fn as_shot(&self) -> Option<WhiteBalanceCoordinates> {
        self.document.as_ref()?.as_shot_coordinates()
    }

    /// The adapter previews render on, or that the CPU does.
    pub fn processor(&self) -> String {
        self.runtime.gpu().map_or_else(
            || "CPU".to_owned(),
            |gpu| gpu.processor().capabilities().adapter_name.clone(),
        )
    }

    pub fn open_folder(&mut self, folder: PathBuf, cx: &mut Context<Self>) {
        if let Some(config) = &self.directories.config {
            engine_session::store_last_folder(config, &folder);
        }
        if let Err(error) = self.catalog_coordinator.scan_folder(folder)
            && let Some(document) = self.document.as_mut()
        {
            document.error = Some(error);
        }
        cx.notify();
    }

    /// Open the folder browsed last time, if it is still there.
    pub fn restore_folder(&mut self, cx: &mut Context<Self>) {
        let folder = self
            .directories
            .config
            .as_deref()
            .and_then(engine_session::load_last_folder);
        if let Some(folder) = folder {
            self.open_folder(folder, cx);
        }
    }

    pub fn open_file(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        if self.document.as_ref().is_some_and(|open| open.path == path) {
            return;
        }
        if self.catalog.entry_index_for_path(&path).is_none()
            && let Some(folder) = path
                .parent()
                .filter(|folder| !folder.as_os_str().is_empty())
        {
            self.open_folder(folder.to_path_buf(), cx);
        }
        self.close_document(cx);
        let (recipe, warning, new_photo) = self.stored_recipe(&path);
        let id = self.next_document_id;
        self.next_document_id += 1;
        let mut document = OpenDocument::opening(id, path.clone(), recipe, warning);
        if new_photo {
            document.mark_new_photo(self.settings.auto_noise_reduction());
        }
        if let Err(error) = self.coordinator.open(id, path) {
            document.open_status = None;
            document.error = Some(error);
        }
        self.document = Some(document);
        cx.notify();
    }

    /// The recipe to open `path` with, a warning, and whether it has no saved edits.
    fn stored_recipe(&self, path: &Path) -> (EditRecipe, Option<String>, bool) {
        // Edits still on their way to disk are newer than the sidecar.
        if let Some((_, recipe)) = self.queued_saves.get(path) {
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
        self.display = None;
        if let Some(document) = self.document.take() {
            self.coordinator.abandon(document.id);
        }
        self.synced_generation = 0;
        self.drop_retired(cx);
        cx.notify();
    }

    /// Release everything that uses GPUI's device. Must run before GPUI shuts down: its wgpu
    /// instance may not outlive the display connection.
    pub fn shut_down(&mut self, cx: &mut Context<Self>) {
        self.close_document(cx);
        self.saves.shutdown();
        self.runtime = PreviewRuntime::new(ProcessorPreference::Cpu, None);
    }

    /// Open the photo after or before the current one in the folder.
    pub fn open_adjacent(&mut self, forward: bool, cx: &mut Context<Self>) {
        let active = self
            .document
            .as_ref()
            .and_then(|document| self.catalog.entry_index_for_path(&document.path));
        let target = match active {
            None => (!self.library.order().is_empty()).then_some(0),
            active => self.library.adjacent_grid_index(active, forward),
        };
        let path = target
            .and_then(|grid| self.library.catalog_index(grid))
            .and_then(|index| self.catalog.entry_path(index))
            .map(Path::to_path_buf);
        if let Some(path) = path {
            self.open_file(path, cx);
        }
    }

    pub fn begin_gesture(&mut self) {
        if let Some(document) = self.document.as_mut() {
            document.edits.begin_gesture();
        }
    }

    /// Replace the recipe as part of a gesture, or as one step of its own. `white_balance` edits
    /// are drafted on the resident GPU frame while dragging and rendered exactly at the end.
    pub fn edit(
        &mut self,
        change: impl FnOnce(&mut EditRecipe),
        phase: EditPhase,
        white_balance: bool,
        cx: &mut Context<Self>,
    ) {
        let Some(document) = self.document.as_mut() else {
            return;
        };
        let mut next = document.edits.recipe().clone();
        change(&mut next);
        let changed = match phase {
            EditPhase::Dragging => document.edits.set_during_gesture(next),
            EditPhase::Finished => document.edits.set_discrete(next),
        };
        if !changed {
            return;
        }
        let drafted = white_balance
            && phase == EditPhase::Dragging
            && self.with_preview(|runtime, coordinator, preview, request| {
                runtime.render_white_balance_draft(coordinator, preview, request)
            }) == Some(true);
        if !drafted {
            self.queue_preview();
        }
        self.tick(cx);
    }

    pub fn finish_gesture(&mut self, white_balance: bool, cx: &mut Context<Self>) {
        if let Some(document) = self.document.as_mut() {
            document.edits.finish_gesture();
            if white_balance {
                self.queue_preview();
            }
        }
        self.tick(cx);
    }

    fn change_history(
        &mut self,
        change: impl FnOnce(&mut EditSession) -> bool,
        cx: &mut Context<Self>,
    ) {
        if self.document.as_mut().is_some_and(|d| change(&mut d.edits)) {
            self.queue_preview();
            self.tick(cx);
        }
    }

    pub fn undo(&mut self, cx: &mut Context<Self>) {
        self.change_history(EditSession::undo, cx);
    }

    pub fn redo(&mut self, cx: &mut Context<Self>) {
        self.change_history(EditSession::redo, cx);
    }

    pub fn reset(&mut self, cx: &mut Context<Self>) {
        let Some(defaults) = self.document.as_ref().map(OpenDocument::default_recipe) else {
            return;
        };
        self.change_history(|edits| edits.reset_to(defaults), cx);
    }

    /// Auto Tone needs the histogram of exactly the recipe it starts from.
    pub fn auto_tone(&mut self, cx: &mut Context<Self>) {
        let next = self.document.as_ref().and_then(|document| {
            let preview = &document.preview;
            if preview.histogram_revision != Some(document.edits.revision()) {
                return None;
            }
            auto_tone(document.edits.recipe(), preview.histogram.as_ref()?)
        });
        if let Some(next) = next {
            self.change_history(|edits| edits.set_discrete(next), cx);
        }
    }

    fn queue_preview(&mut self) {
        self.with_preview(|runtime, coordinator, preview, request| {
            ((), runtime.queue_preview(coordinator, preview, request))
        });
    }

    /// Run an engine preview operation on the open document and apply its outcome.
    fn with_preview<R>(
        &mut self,
        operation: impl FnOnce(
            &mut PreviewRuntime,
            &RenderCoordinator,
            &mut DocumentPreview,
            &PreviewRequest,
        ) -> (R, PreviewOutcome),
    ) -> Option<R> {
        let now = self.now();
        let document = self.document.as_mut()?;
        let request = PreviewRequest {
            frame: document.frame.clone(),
            recipe: document.edits.recipe().clone(),
            revision: document.edits.revision(),
            options: PreviewOptions {
                render: self.settings.render_options(),
                ..PreviewOptions::default()
            },
            histogram_always: false,
            now,
        };
        let (result, outcome) = operation(
            &mut self.runtime,
            &self.coordinator,
            &mut document.preview,
            &request,
        );
        outcome.error.apply(&mut document.error);
        outcome.warning.apply(&mut document.warning);
        self.sync_display();
        Some(result)
    }

    /// Process everything that arrived since the last call and schedule the next one.
    pub fn tick(&mut self, cx: &mut Context<Self>) {
        self.process_worker_events();
        self.process_catalog_events();
        self.process_save_results();
        if let Some(document) = self.document.as_mut()
            && document.sync_camera_look(&self.coordinator)
        {
            self.queue_preview();
        }
        let delay = [
            self.poll_gpu(),
            self.autosave(),
            // The save worker and thumbnail decoding have no wake-up of their own.
            (!self.queued_saves.is_empty()).then_some(Duration::from_millis(100)),
            self.library
                .decode_pending()
                .then_some(Duration::from_millis(16)),
        ]
        .into_iter()
        .flatten()
        .min();
        self.timer = delay.map(|delay| {
            cx.spawn(async move |this, cx| {
                cx.background_executor().timer(delay).await;
                // Gone only while the application quits.
                let _ = this.update(cx, |session, cx| session.tick(cx));
            })
        });
        self.drop_retired(cx);
        cx.notify();
    }

    /// Advance the histogram readback and completion tracking of the displayed GPU frame.
    fn poll_gpu(&mut self) -> Option<Duration> {
        let document = self.document.as_mut()?;
        let completion = self.runtime.poll_completion(&mut document.preview);
        let histogram =
            self.runtime
                .poll_histogram(&mut document.preview, document.edits.revision(), false);
        [completion, histogram.repaint_after]
            .into_iter()
            .flatten()
            .min()
    }
}
