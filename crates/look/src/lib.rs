//! Matching Rohditor's rendering to the camera's own look.
//!
//! The building blocks here are pure functions over display-referred pixels.
//! The caller renders the RAW with a neutral base, decodes the camera's
//! embedded JPEG, and hands both over; nothing in this crate reads files or
//! knows about the pipeline.
//!
//! * [`fit_camera_tone`] fits a global tone curve by histogram matching, which
//!   needs no pixel alignment between the two images.
//! * [`fit_camera_chroma`] fits how colourful the camera renders each brightness,
//!   again from distributions only.
//! * [`Proxy`] and [`compare_proxies`] measure how far a rendering is from the
//!   camera JPEG, and serve as the acceptance harness for any fit.

mod chroma_fit;
mod crop;
mod histogram;
mod proxy;
mod tone_fit;

pub use chroma_fit::{ChromaFit, ChromaFitError, check_chroma_improvement, fit_camera_chroma};
pub use crop::{CropRect, common_aspect_crops};
pub use histogram::{LUMA_BINS, LuminanceHistogram, luminance_histogram};
pub use proxy::{PROXY_MAX_EDGE, Proxy, ProxyDifference, ProxyError, compare_proxies};
pub use tone_fit::{ToneFit, ToneFitError, fit_camera_tone};
