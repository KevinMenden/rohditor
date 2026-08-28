//! GPU preview backend.
//!
//! This crate intentionally stays free of graphics dependencies until the CPU
//! reference pipeline is established.

/// Reports whether this scaffold contains the future GPU implementation.
#[must_use]
pub const fn is_implemented() -> bool {
    false
}
