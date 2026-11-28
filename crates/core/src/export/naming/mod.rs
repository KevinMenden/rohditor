//! Where an export lands: file name templates, destination folders and
//! collision handling. UI-free and filesystem-light, so a single-file dialog,
//! a batch queue and the CLI all derive their output paths the same way.
//!
//! * [`FileNameTemplate`] turns a photo's metadata into a sanitised file stem.
//! * [`ExportNaming`] adds the destination folder and the [`CollisionPolicy`].
//! * [`BatchPlanner`] resolves a sequence of photos to concrete paths, keeping
//!   the batch's own earlier choices in mind so duplicates never overwrite
//!   each other even though no file exists yet.

mod planner;
mod sanitize;
mod template;

use std::io;
use std::path::PathBuf;

use thiserror::Error;

pub use planner::{BatchPlanner, ExportDestination, ExportFolder, ExportNaming, PlannedExport};
pub use template::{FileNameTemplate, PhotoNameInfo};

#[derive(Debug, Error)]
pub enum NamingError {
    #[error("file name template is empty")]
    EmptyTemplate,

    #[error(
        "unknown token {{{token}}} in file name template (use name, seq, seq:<digits>, date or camera)"
    )]
    UnknownToken { token: String },

    #[error("file name template has a '{{' without a closing '}}'")]
    UnclosedToken,

    #[error("file name template has a '}}' without an opening '{{'")]
    UnmatchedClosingBrace,

    #[error("sequence padding in {{{token}}} must be a number from 1 to {max}")]
    InvalidPadding { token: String, max: usize },

    #[error("invalid export subfolder: {reason}")]
    InvalidSubfolder { reason: &'static str },

    #[error("no free name for {path} after {attempts} attempts")]
    NoFreeName { path: PathBuf, attempts: u32 },

    #[error("could not inspect export path {path}: {source}")]
    Inspect {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
}
