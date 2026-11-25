//! The frontend's handle on the catalog worker.

use std::any::Any;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::PathBuf;
use std::sync::{Arc, Mutex, mpsc};
use std::thread::{self, JoinHandle};

use rohditor_catalog::{ThumbnailCache, ThumbnailGenerator, ThumbnailOptions};
use rohditor_raw::{RawDecoder, RawlerDecoder};
use tracing::warn;

use super::worker::{CatalogRequest, SharedDecoder, catalog_worker_loop, send_event};
use super::{CatalogEvent, CatalogSource};
use crate::wake::WakeFn;

pub struct CatalogCoordinator {
    requests: mpsc::Sender<CatalogRequest>,
    events: mpsc::Receiver<CatalogEvent>,
    /// Last pause state transmitted to the worker, used to skip duplicate
    /// per-frame pause updates.
    paused_sent: Mutex<bool>,
    worker: Option<JoinHandle<()>>,
}

impl CatalogCoordinator {
    pub fn new(wake: WakeFn) -> Result<Self, String> {
        let cache = match ThumbnailCache::from_default() {
            Ok(cache) => Some(cache),
            Err(error) => {
                warn!(%error, "catalog thumbnails will not be cached");
                None
            }
        };
        Self::new_with_parts(wake, Arc::new(RawlerDecoder::default()), cache)
    }

    fn new_with_parts(
        wake: WakeFn,
        decoder: Arc<dyn RawDecoder>,
        cache: Option<ThumbnailCache>,
    ) -> Result<Self, String> {
        let (request_sender, request_receiver) = mpsc::channel();
        let (event_sender, event_receiver) = mpsc::channel();
        let stopped_sender = event_sender.clone();
        let stopped_wake = wake.clone();
        let generator =
            ThumbnailGenerator::new(SharedDecoder(decoder), ThumbnailOptions::default());
        let worker = thread::Builder::new()
            .name("rohditor-catalog-worker".to_owned())
            .spawn(move || {
                let result = catch_unwind(AssertUnwindSafe(|| {
                    catalog_worker_loop(request_receiver, event_sender, wake, generator, cache);
                }));
                if let Err(payload) = result {
                    send_event(
                        &stopped_sender,
                        &stopped_wake,
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

    /// Show a folder or album, replacing what was scanned before.
    pub fn show(&self, source: CatalogSource) -> Result<(), String> {
        self.send(CatalogRequest::Show(source))
    }

    /// List a folder for the folder tree, also while paused.
    pub fn list_folder(&self, folder: PathBuf) -> Result<(), String> {
        self.send(CatalogRequest::ListFolder(folder))
    }

    fn send(&self, request: CatalogRequest) -> Result<(), String> {
        self.requests
            .send(request)
            .map_err(|_| "the background catalog worker stopped unexpectedly".to_owned())
    }

    /// Pause or resume background thumbnail generation. Called every frame;
    /// only state changes are transmitted.
    pub fn set_paused(&self, paused: bool) {
        let mut sent = match self.paused_sent.lock() {
            Ok(state) => state,
            Err(poisoned) => poisoned.into_inner(),
        };
        if *sent != paused {
            *sent = paused;
            drop(self.requests.send(CatalogRequest::SetPaused(paused)));
        }
    }

    pub fn try_events(&self) -> impl Iterator<Item = CatalogEvent> + '_ {
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
    use std::time::{Duration, Instant};

    use image::{Rgb, RgbImage};
    use rohditor_catalog::Thumbnail;

    use rohditor_catalog::scan_folder;

    use super::*;
    use crate::catalog::ThumbnailResult;
    use crate::catalog::test_directory::TestDirectory;

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
            crate::wake::noop(),
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
            .show(CatalogSource::Folder(directory.path().to_path_buf()))
            .expect("request catalog scan");

        let events = poll_events(&catalog, |events| thumbnail_ready_count(events) >= 2);
        let names = events
            .iter()
            .filter_map(|event| match event {
                CatalogEvent::ScanEntries { files, .. } => Some(
                    files
                        .iter()
                        .map(|file| file.file_name().to_owned())
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
            .show(CatalogSource::Folder(directory.path().to_path_buf()))
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
        catalog
            .show(CatalogSource::Folder(file))
            .expect("request catalog scan");
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
            .show(CatalogSource::Folder(directory.path().to_path_buf()))
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

    #[test]
    fn an_album_shows_files_of_any_folder_and_keeps_their_cached_thumbnails() {
        let directory = TestDirectory::new();
        let other = TestDirectory::new();
        let seeded = directory.create_arw("seeded.arw");
        directory.create_arw("not_in_the_album.arw");
        let second = other.create_arw("b.arw");
        let gone = other.path().join("gone.arw");
        let cache = ThumbnailCache::new(directory.path().join("thumbs"));
        let options = ThumbnailOptions::default();
        for entry in scan_folder(directory.path()).expect("scan test folder") {
            cache
                .store(&entry, options, &test_thumbnail(), None)
                .expect("seed the thumbnail cache");
        }
        let cached = || {
            fs::read_dir(directory.path().join("thumbs"))
                .expect("read the cache")
                .count()
        };
        let before = cached();

        let catalog = coordinator(Some(cache.clone()));
        catalog
            .show(CatalogSource::Album {
                name: "Best".to_owned(),
                photos: vec![seeded.clone(), gone, second],
            })
            .expect("request album");
        let events = poll_events(&catalog, |events| thumbnail_ready_count(events) >= 2);

        let files = events
            .iter()
            .find_map(|event| match event {
                CatalogEvent::ScanEntries { files, .. } => Some(files),
                _ => None,
            })
            .expect("scan entries event");
        let found: Vec<_> = files
            .iter()
            .map(|file| (file.file_name(), file.entry().is_some()))
            .collect();
        assert_eq!(
            found,
            [("b.arw", true), ("gone.arw", false), ("seeded.arw", true)]
        );
        assert!(events.iter().any(|event| matches!(
            event,
            CatalogEvent::ThumbnailReady { path, outcome: ThumbnailResult::Ready(_), .. }
                if *path == seeded
        )));
        assert_eq!(
            cached(),
            before,
            "an album never cleans up a folder's cache"
        );
    }

    #[test]
    fn folders_are_listed_while_paused() {
        let directory = TestDirectory::new();
        let photo = directory.create_arw("a.arw");
        fs::create_dir(directory.path().join("sub")).expect("create subfolder");
        let missing = directory.path().join("missing");

        let catalog = coordinator(None);
        catalog.set_paused(true);
        for folder in [directory.path(), &missing] {
            catalog
                .list_folder(folder.to_path_buf())
                .expect("request listing");
        }
        let events = poll_events(&catalog, |events| events.len() >= 2);

        let [
            CatalogEvent::FolderListed {
                result: Ok(listing),
                ..
            },
            CatalogEvent::FolderListed {
                folder,
                result: Err(reason),
            },
        ] = events.as_slice()
        else {
            panic!("expected two listings, got {events:?}");
        };
        assert_eq!(listing.files, [photo]);
        assert_eq!(listing.folders, [directory.path().join("sub")]);
        assert_eq!(*folder, missing);
        assert!(reason.contains("does not exist"));
    }
}
