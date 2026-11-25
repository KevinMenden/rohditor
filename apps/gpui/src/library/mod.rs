//! The folder tree, the folder shown, the selection and the thumbnails.
//!
//! Follows the [`Session`]: opening a photo shows its folder and reveals it in the tree, and thumbnails wait while the photo
//! needs the CPU. The session does not know about the library.

mod shown;
mod tree;

use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Result, anyhow};
use gpui::{Context, Entity, Task};
use rohditor_engine::catalog::folder_tree::FolderTree;
use rohditor_engine::catalog::library::LibraryState;
use rohditor_engine::catalog::{CatalogCoordinator, CatalogEvent, CatalogSource, CatalogState};
use rohditor_engine::library_file::{self, LibraryFile, Shown};

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
    tree: FolderTree,
    /// What is remembered, and where.
    file: LibraryFile,
    saved: LibraryFile,
    config: Option<PathBuf>,
    /// The positions whose thumbnails are shown; none while nothing shows them.
    visible: Option<(usize, usize)>,
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
            library.photo_opened(path, cx);
        })
        .detach();
        cx.observe(session, |library, session, cx| {
            let session = session.read(cx);
            library.coordinator.set_paused(session.busy());
            if session.document().is_none() && library.state.selection.open().is_some() {
                library.state.selection.clear_open();
                cx.notify();
            }
        })
        .detach();
        let file = config.as_deref().map(library_file::load);
        let file = file.unwrap_or_default();
        Self {
            coordinator: worker.coordinator,
            tree: FolderTree::new(file.roots.iter().cloned()),
            saved: file.clone(),
            file,
            visible: None,
            catalog: CatalogState::default(),
            state: LibraryState::default(),
            retired: RetiredImages::default(),
            config,
            error: None,
            _wake: worker.woken.run(cx, Self::tick),
            timer: None,
        }
    }

    /// The folder shown.
    pub fn folder(&self) -> Option<&Path> {
        self.catalog.folder()
    }

    /// What went wrong last, for example why the folder shown could not be read.
    pub fn failure(&self) -> Option<&str> {
        self.error.as_deref().or(self.catalog.failure())
    }

    /// Add a folder to the tree and show it.
    pub fn open_folder(&mut self, folder: PathBuf, cx: &mut Context<Self>) {
        self.change_tree(|tree| tree.add_root(folder.clone()), cx);
        self.show_folder(folder, cx);
    }

    pub fn show_folder(&mut self, folder: PathBuf, cx: &mut Context<Self>) {
        self.error = self
            .coordinator
            .show(CatalogSource::Folder(folder.clone()))
            .err();
        self.file.shown = Some(Shown::Folder(folder));
        self.save();
        cx.notify();
    }

    /// Show what was shown last time.
    pub fn restore(&mut self, cx: &mut Context<Self>) {
        if let Some(Shown::Folder(folder)) = self.file.shown.clone() {
            let expand = |tree: &mut FolderTree| {
                if tree.reveal(&folder) {
                    tree.expand(&folder);
                }
            };
            self.change_tree(expand, cx);
            self.show_folder(folder, cx);
        }
    }

    fn photo_opened(&mut self, photo: &Path, cx: &mut Context<Self>) {
        self.state.selection.set_open(photo);
        self.change_tree(
            |tree| {
                tree.reveal(photo);
                tree.set_cursor(photo);
            },
            cx,
        );
        let shown = self
            .catalog
            .source()
            .is_some_and(|source| source.contains(photo));
        let parent = photo.parent();
        if !shown && let Some(folder) = parent.filter(|folder| !folder.as_os_str().is_empty()) {
            self.show_folder(folder.to_path_buf(), cx);
        }
    }

    /// Remember the roots and what is shown, if that changed.
    fn save(&mut self) {
        let Some(config) = &self.config else {
            return;
        };
        self.file.roots = self.tree.roots().to_vec();
        if self.file == self.saved {
            return;
        }
        match library_file::store(config, &self.file) {
            Ok(()) => self.saved = self.file.clone(),
            Err(error) => {
                self.error = Some(format!("The folders could not be remembered: {error}"));
            }
        }
    }

    /// Process what arrived since the last call and decode thumbnails that are due.
    pub fn tick(&mut self, cx: &mut Context<Self>) {
        for event in self.coordinator.try_events() {
            match event {
                CatalogEvent::FolderListed { folder, result } => {
                    self.tree.folder_listed(folder, result);
                }
                event => self.catalog.apply_event(event),
            }
        }
        self.state.begin_frame();
        self.state.sync(&self.catalog);
        if let Some(visible) = self.visible {
            let retired = &self.retired;
            self.state
                .fill_textures(&self.catalog, visible, |_, thumbnail| {
                    AtlasImage::from_rgb(thumbnail.width, thumbnail.height, &thumbnail.rgb, retired)
                        .expect("a decoded thumbnail's pixels match its dimensions")
                });
        }
        // Decoding is spread over ticks and has no wake-up of its own.
        let pending = self.visible.is_some() && self.state.decode_pending();
        self.timer = pending.then(|| {
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
