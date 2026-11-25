//! The open folder and its thumbnails.
//!
//! Follows the [`Session`]: opening a photo shows its folder, and thumbnails wait while the photo
//! needs the CPU. The session does not know about the library.

use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Result, anyhow};
use gpui::{Context, Entity, Task};
use rohditor_engine::catalog::library::LibraryState;
use rohditor_engine::catalog::{CatalogCoordinator, CatalogSource, CatalogState};
use rohditor_engine::session as engine_session;

use crate::atlas_image::{AtlasImage, RetiredImages};
use crate::session::{PhotoOpened, Session};
use crate::wake::{self, Woken};

/// The catalog's background worker.
pub struct LibraryWorker {
    coordinator: CatalogCoordinator,
    woken: Woken,
}

impl LibraryWorker {
    pub fn start() -> Result<Self> {
        let (wake, woken) = wake::channel();
        Ok(Self {
            coordinator: CatalogCoordinator::new(wake).map_err(|e| anyhow!(e))?,
            woken,
        })
    }
}

pub struct Library {
    coordinator: CatalogCoordinator,
    catalog: CatalogState,
    state: LibraryState<AtlasImage>,
    retired: RetiredImages,
    /// Where the last folder is remembered.
    config: Option<PathBuf>,
    error: Option<String>,
    _wake: Task<()>,
    timer: Option<Task<()>>,
}

impl Library {
    pub fn new(
        worker: LibraryWorker,
        config: Option<PathBuf>,
        session: &Entity<Session>,
        cx: &mut Context<Self>,
    ) -> Self {
        cx.subscribe(session, |library, _, PhotoOpened(path), cx| {
            library.show_folder_of(path, cx);
        })
        .detach();
        cx.observe(session, |library, session, cx| {
            library.coordinator.set_paused(session.read(cx).busy());
        })
        .detach();
        Self {
            coordinator: worker.coordinator,
            catalog: CatalogState::default(),
            state: LibraryState::default(),
            retired: RetiredImages::default(),
            config,
            error: None,
            _wake: worker.woken.run(cx, Self::tick),
            timer: None,
        }
    }

    pub fn folder_name(&self) -> Option<String> {
        self.catalog.title()
    }

    /// How many photos there are. Positions below count in display order.
    pub fn len(&self) -> usize {
        self.state.order().len()
    }

    pub fn path(&self, position: usize) -> Option<&Path> {
        self.catalog.entry_path(self.state.catalog_index(position)?)
    }

    pub fn name(&self, position: usize) -> Option<&str> {
        self.catalog.entry_name(self.state.catalog_index(position)?)
    }

    pub fn thumbnail(&self, path: &Path) -> Option<&AtlasImage> {
        self.state.texture(path)
    }

    pub fn scanning(&self) -> bool {
        self.catalog.scanning()
    }

    /// Why the folder could not be read.
    pub fn failure(&self) -> Option<&str> {
        self.error.as_deref().or(self.catalog.failure())
    }

    /// The photo after or before `current`, or the first one if `current` is not shown.
    pub fn adjacent(&self, current: Option<&Path>, forward: bool) -> Option<PathBuf> {
        let path = self.state.adjacent_path(&self.catalog, current, forward);
        path.map(Path::to_path_buf)
    }

    pub fn open_folder(&mut self, folder: PathBuf, cx: &mut Context<Self>) {
        if let Some(config) = &self.config {
            engine_session::store_last_folder(config, &folder);
        }
        self.error = self.coordinator.show(CatalogSource::Folder(folder)).err();
        cx.notify();
    }

    /// Open the folder browsed last time, if it is still there.
    pub fn restore_folder(&mut self, cx: &mut Context<Self>) {
        let folder = self
            .config
            .as_deref()
            .and_then(engine_session::load_last_folder);
        if let Some(folder) = folder {
            self.open_folder(folder, cx);
        }
    }

    fn show_folder_of(&mut self, photo: &Path, cx: &mut Context<Self>) {
        if self.catalog.entry_index_for_path(photo).is_none()
            && let Some(folder) = photo
                .parent()
                .filter(|folder| !folder.as_os_str().is_empty())
        {
            self.open_folder(folder.to_path_buf(), cx);
        }
    }

    /// The list shows these rows now; have their thumbnails ready.
    pub fn set_visible(&mut self, start: usize, end: usize, cx: &mut Context<Self>) {
        if self.state.visible != (start, end) {
            self.state.visible = (start, end);
            self.tick(cx);
        }
    }

    /// Process what arrived since the last call and decode thumbnails that are due.
    pub fn tick(&mut self, cx: &mut Context<Self>) {
        for event in self.coordinator.try_events() {
            self.catalog.apply_event(event);
        }
        self.state.begin_frame();
        self.state.sync(&self.catalog);
        let visible = self.state.visible;
        let retired = &self.retired;
        self.state
            .fill_textures(&self.catalog, visible, |_, thumbnail| {
                AtlasImage::from_rgb(thumbnail.width, thumbnail.height, &thumbnail.rgb, retired)
                    .expect("a decoded thumbnail's pixels match its dimensions")
            });
        // Decoding is spread over ticks and has no wake-up of its own.
        self.timer = self.state.decode_pending().then(|| {
            cx.spawn(async move |this, cx| {
                cx.background_executor()
                    .timer(Duration::from_millis(16))
                    .await;
                // Gone only while the application quits.
                let _ = this.update(cx, Self::tick);
            })
        });
        self.retired.remove_from_atlas(cx);
        cx.notify();
    }
}
