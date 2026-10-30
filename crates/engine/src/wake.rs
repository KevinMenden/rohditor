//! Toolkit-neutral "something happened" signal for background workers.
use std::sync::Arc;

/// Called by workers after they queue an event so the frontend can schedule a
/// redraw or poll its receiver. Must be cheap and callable from any thread.
pub type WakeFn = Arc<dyn Fn() + Send + Sync>;

#[cfg(test)]
pub fn noop() -> WakeFn {
    Arc::new(|| {})
}
