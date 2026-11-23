//! ROHDITOR: lets the application reach the device GPUI renders with. Upstream keeps it private
//! to the platform client.

use std::cell::RefCell;
use std::rc::{Rc, Weak};

use crate::{GpuContext, WgpuContext};

thread_local! {
    // GPUI creates its renderers, and runs the application, on the main thread. Weak, because the
    // wgpu instance must be dropped before GPUI closes its Wayland connection: a strong handle
    // would outlive it until thread exit and crash in EGL teardown.
    static CURRENT: RefCell<Weak<RefCell<Option<WgpuContext>>>> = const { RefCell::new(Weak::new()) };
}

pub(crate) fn publish(context: &GpuContext) {
    CURRENT.with(|current| *current.borrow_mut() = Rc::downgrade(context));
}

/// The context of the most recently created renderer, once a window exists. Callers must drop it,
/// and everything created from it, before the application quits.
pub fn current_context() -> Option<WgpuContext> {
    let context = CURRENT.with(|current| current.borrow().upgrade())?;
    let context = context.borrow();
    context.clone()
}

/// Headless renderers own their context; keep it reachable for the rest of the test process.
#[cfg(any(feature = "bench-support", feature = "test-support"))]
pub(crate) fn publish_headless(context: &WgpuContext) {
    let shared = GpuContext::new(RefCell::new(Some(context.clone())));
    publish(&shared);
    std::mem::forget(shared);
}
