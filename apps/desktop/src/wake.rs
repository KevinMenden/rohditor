//! Wakes an entity when a worker thread has something for it.

use std::sync::Arc;

use futures::StreamExt as _;
use futures::channel::mpsc;
use gpui::{Context, Task};
use rohditor_engine::WakeFn;

/// The receiving end of a [`WakeFn`].
pub struct Woken(mpsc::UnboundedReceiver<()>);

pub fn channel() -> (WakeFn, Woken) {
    let (sender, receiver) = mpsc::unbounded();
    let wake: WakeFn = Arc::new(move || {
        // The receiver only goes away while the application quits.
        let _ = sender.unbounded_send(());
    });
    (wake, Woken(receiver))
}

impl Woken {
    /// Call `tick` after wake-ups, once for everything queued so far.
    pub fn run<T: 'static>(
        mut self,
        cx: &Context<T>,
        tick: fn(&mut T, &mut Context<T>),
    ) -> Task<()> {
        cx.spawn(async move |this, cx| {
            while self.0.next().await.is_some() {
                while self.0.try_recv().is_ok() {}
                if this.update(cx, tick).is_err() {
                    break;
                }
            }
        })
    }
}
