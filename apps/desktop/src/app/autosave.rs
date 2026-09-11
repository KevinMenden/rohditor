//! Observe recipe revisions without coupling persistence to preview rendering.
use std::time::{Duration, Instant};

const SAVE_DELAY: Duration = Duration::from_secs(1);

#[derive(Default)]
pub(super) struct Autosave {
    revision: u64,
    due: Option<Instant>,
    dragging: bool,
}

impl Autosave {
    pub(super) fn observe(&mut self, revision: u64, dragging: bool, now: Instant) {
        if revision != self.revision || (self.dragging && !dragging) {
            self.revision = revision;
            self.due = Some(now + SAVE_DELAY);
        }
        self.dragging = dragging;
    }

    pub(super) fn take_due(&mut self, now: Instant) -> bool {
        if !self.dragging && self.due.is_some_and(|due| now >= due) {
            self.due = None;
            return true;
        }
        false
    }

    pub(super) fn delay(&self, now: Instant) -> Option<Duration> {
        (!self.dragging)
            .then_some(self.due)
            .flatten()
            .map(|due| due.saturating_duration_since(now))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn edits_restart_delay_but_unchanged_frames_do_not() {
        let now = Instant::now();
        let mut save = Autosave::default();
        save.observe(1, false, now);
        save.observe(2, false, now + Duration::from_millis(800));
        save.observe(2, false, now + Duration::from_millis(1500));
        assert!(!save.take_due(now + SAVE_DELAY));
        assert!(save.take_due(now + Duration::from_millis(1800)));
        assert!(!save.take_due(now + Duration::from_secs(3)));
    }

    #[test]
    fn paused_drag_waits_until_one_second_after_release() {
        let now = Instant::now();
        let mut save = Autosave::default();
        save.observe(1, true, now);
        assert!(!save.take_due(now + Duration::from_secs(5)));
        save.observe(1, false, now + Duration::from_secs(5));
        assert!(!save.take_due(now + Duration::from_millis(5999)));
        assert!(save.take_due(now + Duration::from_secs(6)));
    }
}
