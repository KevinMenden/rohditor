//! Toolkit-free preview state of an open document.
//!
//! What a frontend needs to track about the image it is showing: where the
//! displayed frame came from, whether a resident GPU base can serve a new
//! request, and the diagnostics of the last render.

mod diagnostics;
mod routing;
mod source;

pub use diagnostics::DocumentPreviewDiagnostics;
pub use routing::{
    gpu_base_algorithm_matches, gpu_supports_recipe, preview_intent_matches_resolution,
};
pub use source::PreviewSource;
