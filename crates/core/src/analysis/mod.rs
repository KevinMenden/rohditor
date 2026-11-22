//! Display-referred analysis of a rendered preview: the histogram and the
//! clipping mask. Both read the same final 8-bit display buffer and share one
//! clipping rule (see [`clipping`]), so counts and overlays always agree.

mod clipping;
mod histogram;

pub use clipping::{ClipFlags, ClippingMask};
pub use histogram::Histogram;
