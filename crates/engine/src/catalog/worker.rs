//! The catalog thread: scans, folder listings and thumbnails.

use std::path::{Path, PathBuf};
use std::sync::{Arc, mpsc};

use rohditor_catalog::{
    CachedThumbnail, CatalogEntry, GeneratedThumbnail, ScanError, ScannedFile, ThumbnailCache,
    ThumbnailGenerator, ThumbnailOptions, ThumbnailOutcome, list_folder, scan_files, scan_folder,
};
use rohditor_raw::{RawDecoder, RawError, RawSession};
use tracing::{info, warn};

use super::{CatalogEvent, CatalogSource, ThumbnailResult};
use crate::wake::WakeFn;

#[derive(Debug)]
pub(super) enum CatalogRequest {
    Show(CatalogSource),
    ListFolder(PathBuf),
    SetPaused(bool),
    Shutdown,
}

/// `ThumbnailGenerator` owns its decoder, so the shared worker decoder is
/// wrapped in a sized newtype that forwards to the trait object.
pub(super) struct SharedDecoder(pub(super) Arc<dyn RawDecoder>);

impl RawDecoder for SharedDecoder {
    fn open(&self, path: &Path) -> Result<Box<dyn RawSession>, RawError> {
        self.0.open(path)
    }
}

struct CatalogWorker {
    sender: mpsc::Sender<CatalogEvent>,
    wake: WakeFn,
    generator: ThumbnailGenerator<SharedDecoder>,
    cache: Option<ThumbnailCache>,
    options: ThumbnailOptions,
    generation: u64,
    entries: Vec<CatalogEntry>,
    /// Next index to fill; entries are visited once per folder scan.
    cursor: usize,
    paused: bool,
}

pub(super) fn catalog_worker_loop(
    requests: mpsc::Receiver<CatalogRequest>,
    events: mpsc::Sender<CatalogEvent>,
    wake: WakeFn,
    generator: ThumbnailGenerator<SharedDecoder>,
    cache: Option<ThumbnailCache>,
) {
    let mut worker = CatalogWorker {
        sender: events,
        wake,
        generator,
        cache,
        options: ThumbnailOptions::default(),
        generation: 0,
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
            CatalogRequest::Show(source) => {
                self.start_scan(source);
                true
            }
            CatalogRequest::ListFolder(folder) => {
                let result = list_folder(&folder).map_err(|error| error.to_string());
                self.send(CatalogEvent::FolderListed { folder, result });
                true
            }
            CatalogRequest::SetPaused(paused) => {
                self.paused = paused;
                true
            }
            CatalogRequest::Shutdown => false,
        }
    }

    fn start_scan(&mut self, source: CatalogSource) {
        self.generation += 1;
        self.entries.clear();
        self.cursor = 0;
        let generation = self.generation;
        self.send(CatalogEvent::ScanStarted {
            generation,
            source: source.clone(),
        });
        let files = match &source {
            CatalogSource::Folder(folder) => self.scan_folder(folder),
            CatalogSource::Album { photos, .. } => Ok(scan_files(photos)),
        };
        match files {
            Ok(files) => {
                self.entries = files
                    .iter()
                    .filter_map(ScannedFile::entry)
                    .cloned()
                    .collect();
                self.send(CatalogEvent::ScanEntries { generation, files });
            }
            Err(error) => self.send(CatalogEvent::ScanFailed {
                generation,
                reason: error.to_string(),
            }),
        }
    }

    fn scan_folder(&self, folder: &Path) -> Result<Vec<ScannedFile>, ScanError> {
        let entries = scan_folder(folder)?;
        // Only a folder scan knows all of a folder's photos. An album does not,
        // so it must not clean up.
        if let Some(cache) = &self.cache {
            let removed = cache.cleanup_folder(folder, &entries);
            if removed > 0 {
                info!(
                    removed,
                    folder = %folder.display(),
                    "removed stale catalog thumbnails"
                );
            }
        }
        Ok(entries.into_iter().map(ScannedFile::Found).collect())
    }

    fn send(&self, event: CatalogEvent) {
        send_event(&self.sender, &self.wake, event);
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
        let entry = &self.entries[index];
        let (outcome, captured_at) = self.generate(entry);
        self.send(CatalogEvent::ThumbnailReady {
            generation: self.generation,
            path: entry.path().to_path_buf(),
            identity: entry.source_identity(),
            outcome,
            captured_at,
        });
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

pub(super) fn send_event(sender: &mpsc::Sender<CatalogEvent>, wake: &WakeFn, event: CatalogEvent) {
    drop(sender.send(event));
    wake();
}
