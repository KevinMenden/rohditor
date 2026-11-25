//! The photo library behind a frontend: what it shows, the folder tree, the
//! selection, and the background worker that scans and fills thumbnails.
//!
//! The worker mirrors the render coordinator: the UI thread sends requests and
//! drains events while a dedicated thread scans folders and fills thumbnails.
//! The document/render pipeline stays untouched, and the catalog thread pauses
//! while the document worker is busy so editing work is never starved.

use std::path::PathBuf;

use rohditor_catalog::{FolderListing, PlaceholderReason, ScannedFile, Thumbnail};
use rohditor_raw::SourceIdentity;

mod coordinator;
pub mod folder_tree;
pub mod library;
mod selection;
mod source;
mod state;
#[cfg(test)]
mod test_directory;
mod worker;

pub use coordinator::CatalogCoordinator;
pub use selection::PhotoSelection;
pub use source::CatalogSource;
pub use state::{CatalogState, ThumbnailSlot};

/// What the worker reports. `generation` numbers the scans, so that results of
/// an earlier one are told apart even when it was of the same source.
#[derive(Debug)]
pub enum CatalogEvent {
    ScanStarted {
        generation: u64,
        source: CatalogSource,
    },
    ScanEntries {
        generation: u64,
        files: Vec<ScannedFile>,
    },
    ScanFailed {
        generation: u64,
        reason: String,
    },
    ThumbnailReady {
        generation: u64,
        path: PathBuf,
        identity: SourceIdentity,
        outcome: ThumbnailResult,
        captured_at: Option<String>,
    },
    /// For the [`folder_tree::FolderTree`]; unrelated to the scanned source.
    FolderListed {
        folder: PathBuf,
        result: Result<FolderListing, String>,
    },
    WorkerStopped {
        message: String,
    },
}

/// Outcome of one thumbnail attempt, ready for a UI slot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ThumbnailResult {
    Ready(Thumbnail),
    Placeholder(PlaceholderReason),
    Failed(String),
}
