//! Background photo-catalog worker.
//!
//! Mirrors the render coordinator: the UI thread sends requests and drains
//! events while a dedicated thread scans folders and fills thumbnails. The
//! document/render pipeline stays untouched, and the catalog thread pauses
//! while the document worker is busy so editing work is never starved.

use std::any::Any;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, mpsc};
use std::thread::{self, JoinHandle};

use eframe::egui;
use rohditor_catalog::{
    CachedThumbnail, CatalogEntry, GeneratedThumbnail, PlaceholderReason, Thumbnail,
    ThumbnailCache, ThumbnailGenerator, ThumbnailOptions, ThumbnailOutcome, scan_folder,
};
use rohditor_raw::{RawDecoder, RawError, RawSession, RawlerDecoder, SourceIdentity};
use tracing::{info, warn};

#[path = "catalog/state.rs"]
mod state;

pub(crate) use state::{CatalogState, ThumbnailSlot};

#[derive(Debug)]
pub(crate) enum CatalogEvent {
    ScanStarted {
        folder: PathBuf,
    },
    ScanEntries {
        folder: PathBuf,
        entries: Vec<CatalogEntry>,
    },
    ScanFailed {
        folder: PathBuf,
        reason: String,
    },
    ThumbnailReady {
        folder: PathBuf,
        path: PathBuf,
        identity: SourceIdentity,
        outcome: ThumbnailResult,
        captured_at: Option<String>,
    },
    WorkerStopped {
        message: String,
    },
}

/// Outcome of one thumbnail attempt, ready for a UI slot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ThumbnailResult {
    Ready(Thumbnail),
    Placeholder(PlaceholderReason),
    Failed(String),
}

#[derive(Debug)]
enum CatalogRequest {
    ScanFolder(PathBuf),
    SetPaused(bool),
    Shutdown,
}

/// `ThumbnailGenerator` owns its decoder, so the shared worker decoder is
/// wrapped in a sized newtype that forwards to the trait object.
struct SharedDecoder(Arc<dyn RawDecoder>);

impl RawDecoder for SharedDecoder {
    fn open(&self, path: &Path) -> Result<Box<dyn RawSession>, RawError> {
        self.0.open(path)
    }
}

pub(crate) struct CatalogCoordinator {
    requests: mpsc::Sender<CatalogRequest>,
    events: mpsc::Receiver<CatalogEvent>,
    /// Last pause state transmitted to the worker, used to skip duplicate
    /// per-frame pause updates.
    paused_sent: Mutex<bool>,
    worker: Option<JoinHandle<()>>,
}

impl CatalogCoordinator {
    pub(crate) fn new(context: egui::Context) -> Result<Self, String> {
        let cache = match ThumbnailCache::from_default() {
            Ok(cache) => Some(cache),
            Err(error) => {
                warn!(%error, "catalog thumbnails will not be cached");
                None
            }
        };
        Self::new_with_parts(context, Arc::new(RawlerDecoder::default()), cache)
    }

    fn new_with_parts(
        context: egui::Context,
        decoder: Arc<dyn RawDecoder>,
        cache: Option<ThumbnailCache>,
    ) -> Result<Self, String> {
        let (request_sender, request_receiver) = mpsc::channel();
        let (event_sender, event_receiver) = mpsc::channel();
        let stopped_sender = event_sender.clone();
        let stopped_context = context.clone();
        let generator =
            ThumbnailGenerator::new(SharedDecoder(decoder), ThumbnailOptions::default());
        let worker = thread::Builder::new()
            .name("rohditor-catalog-worker".to_owned())
            .spawn(move || {
                let result = catch_unwind(AssertUnwindSafe(|| {
                    catalog_worker_loop(request_receiver, event_sender, context, generator, cache);
                }));
                if let Err(payload) = result {
                    send_event(
                        &stopped_sender,
                        &stopped_context,
                        CatalogEvent::WorkerStopped {
                            message: format!(
                                "The background catalog worker stopped unexpectedly: {}",
                                panic_message(payload.as_ref())
                            ),
                        },
                    );
                }
            })
            .map_err(|error| format!("could not start the catalog worker: {error}"))?;
        Ok(Self {
            requests: request_sender,
            events: event_receiver,
            paused_sent: Mutex::new(false),
            worker: Some(worker),
        })
    }

    /// Start scanning one folder, replacing any previous catalog fill.
    pub(crate) fn scan_folder(&self, folder: PathBuf) -> Result<(), String> {
        self.requests
            .send(CatalogRequest::ScanFolder(folder))
            .map_err(|_| "the background catalog worker stopped unexpectedly".to_owned())
    }

    /// Pause or resume background thumbnail generation. Called every frame;
    /// only state changes are transmitted.
    pub(crate) fn set_paused(&self, paused: bool) {
        let mut sent = match self.paused_sent.lock() {
            Ok(state) => state,
            Err(poisoned) => poisoned.into_inner(),
        };
        if *sent != paused {
            *sent = paused;
            drop(self.requests.send(CatalogRequest::SetPaused(paused)));
        }
    }

    pub(crate) fn try_events(&self) -> impl Iterator<Item = CatalogEvent> + '_ {
        self.events.try_iter()
    }
}

impl Drop for CatalogCoordinator {
    fn drop(&mut self) {
        // Like the render coordinator: request shutdown without joining an
        // active worker, then reap the thread once it has finished.
        drop(self.requests.send(CatalogRequest::Shutdown));
        if self.worker.as_ref().is_some_and(JoinHandle::is_finished)
            && let Some(worker) = self.worker.take()
        {
            drop(worker.join());
        }
    }
}

struct CatalogWorker {
    sender: mpsc::Sender<CatalogEvent>,
    context: egui::Context,
    generator: ThumbnailGenerator<SharedDecoder>,
    cache: Option<ThumbnailCache>,
    options: ThumbnailOptions,
    folder: Option<PathBuf>,
    entries: Vec<CatalogEntry>,
    /// Next index to fill; entries are visited once per folder scan.
    cursor: usize,
    paused: bool,
}

fn catalog_worker_loop(
    requests: mpsc::Receiver<CatalogRequest>,
    events: mpsc::Sender<CatalogEvent>,
    context: egui::Context,
    generator: ThumbnailGenerator<SharedDecoder>,
    cache: Option<ThumbnailCache>,
) {
    let mut worker = CatalogWorker {
        sender: events,
        context,
        generator,
        cache,
        options: ThumbnailOptions::default(),
        folder: None,
        entries: Vec::new(),
        cursor: 0,
        paused: false,
    };
    while let Ok(request) = requests.recv() {
        if !worker.handle_request(request) {
            break;
        }
        while let Ok(request) = requests.try_recv() {
            if !worker.handle_request(request) {
                return;
            }
        }
        if !worker.fill(&requests) {
            return;
        }
    }
}

impl CatalogWorker {
    /// Returns `false` once the worker should stop.
    fn handle_request(&mut self, request: CatalogRequest) -> bool {
        match request {
            CatalogRequest::ScanFolder(folder) => {
                self.start_scan(folder);
                true
            }
            CatalogRequest::SetPaused(paused) => {
                self.paused = paused;
                true
            }
            CatalogRequest::Shutdown => false,
        }
    }

    fn start_scan(&mut self, folder: PathBuf) {
        self.folder = Some(folder.clone());
        self.entries.clear();
        self.cursor = 0;
        send_event(
            &self.sender,
            &self.context,
            CatalogEvent::ScanStarted {
                folder: folder.clone(),
            },
        );
        match scan_folder(&folder) {
            Ok(entries) => {
                self.entries = entries;
                if let Some(cache) = &self.cache {
                    let removed = cache.cleanup_folder(&folder, &self.entries);
                    if removed > 0 {
                        info!(
                            removed,
                            folder = %folder.display(),
                            "removed stale catalog thumbnails"
                        );
                    }
                }
                send_event(
                    &self.sender,
                    &self.context,
                    CatalogEvent::ScanEntries {
                        folder,
                        entries: self.entries.clone(),
                    },
                );
            }
            Err(error) => {
                send_event(
                    &self.sender,
                    &self.context,
                    CatalogEvent::ScanFailed {
                        folder,
                        reason: error.to_string(),
                    },
                );
            }
        }
    }

    /// Generate thumbnails one at a time while unpaused, interleaving request
    /// handling so scans and pause changes take effect between files.
    fn fill(&mut self, requests: &mpsc::Receiver<CatalogRequest>) -> bool {
        while !self.paused {
            let Some(index) = self.next_pending() else {
                return true;
            };
            self.emit_thumbnail(index);
            while let Ok(request) = requests.try_recv() {
                if !self.handle_request(request) {
                    return false;
                }
            }
        }
        true
    }

    fn next_pending(&mut self) -> Option<usize> {
        if self.cursor < self.entries.len() {
            let index = self.cursor;
            self.cursor += 1;
            Some(index)
        } else {
            None
        }
    }

    fn emit_thumbnail(&self, index: usize) {
        let Some(folder) = self.folder.clone() else {
            return;
        };
        let entry = &self.entries[index];
        let (outcome, captured_at) = self.generate(entry);
        send_event(
            &self.sender,
            &self.context,
            CatalogEvent::ThumbnailReady {
                folder,
                path: entry.path().to_path_buf(),
                identity: entry.source_identity(),
                outcome,
                captured_at,
            },
        );
    }

    /// Cache-first thumbnail generation; cache failures degrade to direct
    /// generation instead of blocking the catalog. Capture dates travel with
    /// the thumbnail, from either the cache or the fresh probe.
    fn generate(&self, entry: &CatalogEntry) -> (ThumbnailResult, Option<String>) {
        if let Some(cache) = &self.cache {
            match cache.load(entry, self.options) {
                Ok(Some(CachedThumbnail {
                    thumbnail,
                    captured_at,
                })) => return (ThumbnailResult::Ready(thumbnail), captured_at),
                Ok(None) => {}
                Err(error) => {
                    warn!(
                        %error,
                        path = %entry.path().display(),
                        "catalog thumbnail cache read failed"
                    );
                }
            }
        }
        match self.generator.generate(entry.path()) {
            Ok(GeneratedThumbnail {
                outcome: ThumbnailOutcome::Ready(thumbnail),
                captured_at,
            }) => {
                if let Some(cache) = &self.cache
                    && let Err(error) =
                        cache.store(entry, self.options, &thumbnail, captured_at.as_deref())
                {
                    warn!(
                        %error,
                        path = %entry.path().display(),
                        "catalog thumbnail cache write failed"
                    );
                }
                (ThumbnailResult::Ready(thumbnail), captured_at)
            }
            Ok(GeneratedThumbnail {
                outcome: ThumbnailOutcome::Placeholder(reason),
                captured_at,
            }) => (ThumbnailResult::Placeholder(reason), captured_at),
            Err(error) => (ThumbnailResult::Failed(error.to_string()), None),
        }
    }
}

fn send_event(sender: &mpsc::Sender<CatalogEvent>, context: &egui::Context, event: CatalogEvent) {
    drop(sender.send(event));
    context.request_repaint();
}

fn panic_message(payload: &(dyn Any + Send)) -> String {
    if let Some(message) = payload.downcast_ref::<&str>() {
        (*message).to_owned()
    } else if let Some(message) = payload.downcast_ref::<String>() {
        message.clone()
    } else {
        "non-string panic payload".to_owned()
    }
}

#[cfg(test)]
mod tests {
    use std::fs::{self, File};
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicU64, Ordering as AtomicOrdering};
    use std::time::{Duration, Instant};

    use image::{Rgb, RgbImage};
    use rohditor_catalog::Thumbnail;

    use super::*;

    static NEXT_TEST_DIRECTORY: AtomicU64 = AtomicU64::new(0);

    struct TestDirectory(PathBuf);

    impl TestDirectory {
        fn new() -> Self {
            let id = NEXT_TEST_DIRECTORY.fetch_add(1, AtomicOrdering::Relaxed);
            let path = std::env::temp_dir().join(format!(
                "rohditor-desktop-catalog-{}-{id}",
                std::process::id()
            ));
            fs::create_dir(&path).expect("create catalog test directory");
            Self(path)
        }

        fn path(&self) -> &Path {
            &self.0
        }

        fn create_arw(&self, name: &str) -> PathBuf {
            let path = self.path().join(name);
            File::create(&path).expect("create catalog test file");
            path
        }
    }

    impl Drop for TestDirectory {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    /// Collect catalog events until `done` matches, with a hard deadline.
    fn poll_events<F>(coordinator: &CatalogCoordinator, done: F) -> Vec<CatalogEvent>
    where
        F: Fn(&[CatalogEvent]) -> bool,
    {
        let mut events = Vec::new();
        let deadline = Instant::now() + Duration::from_secs(20);
        while Instant::now() < deadline {
            events.extend(coordinator.try_events());
            if done(&events) {
                break;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        events
    }

    fn thumbnail_ready_count(events: &[CatalogEvent]) -> usize {
        events
            .iter()
            .filter(|event| matches!(event, CatalogEvent::ThumbnailReady { .. }))
            .count()
    }

    fn test_thumbnail() -> Thumbnail {
        let image = RgbImage::from_pixel(2, 1, Rgb([120, 140, 160]));
        let mut bytes = Vec::new();
        image::codecs::jpeg::JpegEncoder::new_with_quality(&mut bytes, 85)
            .encode_image(&image)
            .expect("encode test thumbnail");
        Thumbnail::new(2, 1, bytes)
    }

    fn coordinator(cache: Option<ThumbnailCache>) -> CatalogCoordinator {
        CatalogCoordinator::new_with_parts(
            egui::Context::default(),
            Arc::new(RawlerDecoder::default()),
            cache,
        )
        .expect("start catalog coordinator")
    }

    #[test]
    fn catalog_scan_streams_entries_and_reports_unreadable_sources() {
        let directory = TestDirectory::new();
        directory.create_arw("b.arw");
        directory.create_arw("a.arw");
        File::create(directory.path().join("notes.txt")).expect("create unrelated file");

        let catalog = coordinator(Some(ThumbnailCache::new(directory.path().join("thumbs"))));
        catalog
            .scan_folder(directory.path().to_path_buf())
            .expect("request catalog scan");

        let events = poll_events(&catalog, |events| thumbnail_ready_count(events) >= 2);
        let names = events
            .iter()
            .filter_map(|event| match event {
                CatalogEvent::ScanEntries { entries, .. } => Some(
                    entries
                        .iter()
                        .map(|entry| entry.file_name().to_owned())
                        .collect::<Vec<_>>(),
                ),
                _ => None,
            })
            .next()
            .expect("scan entries event");
        assert_eq!(names, ["a.arw", "b.arw"]);
        // Exactly one scan start, one entries batch, and one result per file.
        assert_eq!(events.len(), 4);
        for event in &events {
            if let CatalogEvent::ThumbnailReady {
                outcome: ThumbnailResult::Failed(reason),
                ..
            } = event
            {
                assert!(!reason.is_empty(), "fake RAW files must fail readably");
            }
        }
        assert_eq!(
            events
                .iter()
                .filter(|event| matches!(event, CatalogEvent::ThumbnailReady { .. }))
                .count(),
            2
        );
    }

    #[test]
    fn paused_catalog_defers_thumbnail_generation_until_resumed() {
        let directory = TestDirectory::new();
        directory.create_arw("a.arw");
        directory.create_arw("b.arw");

        let catalog = coordinator(Some(ThumbnailCache::new(directory.path().join("thumbs"))));
        catalog.set_paused(true);
        catalog
            .scan_folder(directory.path().to_path_buf())
            .expect("request catalog scan");

        poll_events(&catalog, |events| {
            events
                .iter()
                .any(|event| matches!(event, CatalogEvent::ScanEntries { .. }))
        });
        std::thread::sleep(Duration::from_millis(150));
        let paused_events = poll_events(&catalog, |_| true);
        assert_eq!(thumbnail_ready_count(&paused_events), 0);

        catalog.set_paused(false);
        let events = poll_events(&catalog, |events| thumbnail_ready_count(events) >= 2);
        assert_eq!(thumbnail_ready_count(&events), 2);
    }

    #[test]
    fn scan_failure_reports_the_reason_for_the_current_folder() {
        let directory = TestDirectory::new();
        let file = directory.create_arw("single.arw");

        let catalog = coordinator(None);
        catalog.scan_folder(file).expect("request catalog scan");
        let events = poll_events(&catalog, |events| {
            events
                .iter()
                .any(|event| matches!(event, CatalogEvent::ScanFailed { .. }))
        });
        let reason = events
            .iter()
            .find_map(|event| match event {
                CatalogEvent::ScanFailed { reason, .. } => Some(reason.clone()),
                _ => None,
            })
            .expect("scan failure event");
        assert!(reason.contains("not a directory"));
    }

    #[test]
    fn cached_thumbnails_skip_decoding_and_uncached_sources_generate() {
        let directory = TestDirectory::new();
        directory.create_arw("seeded.arw");
        directory.create_arw("fresh.arw");
        let cache = ThumbnailCache::new(directory.path().join("thumbs"));
        let options = ThumbnailOptions::default();
        let entries = scan_folder(directory.path()).expect("scan test folder");
        let seeded = entries
            .iter()
            .find(|entry| entry.file_name() == "seeded.arw")
            .expect("find seeded entry");
        let expected = test_thumbnail();
        cache
            .store(seeded, options, &expected, None)
            .expect("seed the thumbnail cache");

        let catalog = coordinator(Some(cache));
        catalog
            .scan_folder(directory.path().to_path_buf())
            .expect("request catalog scan");
        let events = poll_events(&catalog, |events| thumbnail_ready_count(events) >= 2);

        let mut ready_bytes = None;
        let mut failed = 0;
        for event in &events {
            if let CatalogEvent::ThumbnailReady { path, outcome, .. } = event {
                if path.file_name().is_some_and(|name| name == "seeded.arw") {
                    if let ThumbnailResult::Ready(thumbnail) = outcome {
                        ready_bytes = Some(thumbnail.bytes().to_vec());
                    }
                } else if matches!(outcome, ThumbnailResult::Failed(_)) {
                    failed += 1;
                }
            }
        }
        assert_eq!(ready_bytes, Some(expected.bytes().to_vec()));
        assert_eq!(failed, 1);
    }
}
