//! Bounded newest-wins preview scheduling, independent of RAW decoding and
//! export execution. Keeping this state machine isolated makes cancellation
//! and coalescing behavior testable without growing the worker implementation.

use std::sync::{Mutex, MutexGuard, mpsc};

use rohditor_core::CancellationToken;

use super::{PreviewJob, PreviewQueueStats, WorkerRequest};
use crate::document::PreviewTicket;

#[derive(Debug)]
struct ActivePreview {
    ticket: PreviewTicket,
    cancellation: CancellationToken,
}

#[derive(Debug, Default)]
struct PreviewMailboxState {
    pending: Option<PreviewJob>,
    active: Option<ActivePreview>,
    wake_queued: bool,
    stats: PreviewQueueStats,
}

#[derive(Debug, Default)]
pub(super) struct PreviewMailbox {
    state: Mutex<PreviewMailboxState>,
}

pub(super) struct ScheduledPreview {
    pub(super) job: PreviewJob,
    pub(super) cancellation: CancellationToken,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum PreviewCompletion {
    Completed,
    Cancelled,
    Failed,
}

impl PreviewMailbox {
    pub(super) fn queue(
        &self,
        job: PreviewJob,
        requests: &mpsc::Sender<WorkerRequest>,
    ) -> Result<(), String> {
        let mut state = self.lock();
        state.stats.requested = state.stats.requested.saturating_add(1);

        if let Some(active) = &state.active
            && (active.ticket.document_id != job.ticket.document_id
                || should_replace_preview(active.ticket, job.ticket))
            && !active.cancellation.is_cancelled()
        {
            active.cancellation.cancel();
            state.stats.cancellation_requests = state.stats.cancellation_requests.saturating_add(1);
        }

        let replace_pending = state.pending.as_ref().is_none_or(|pending| {
            pending.ticket.document_id != job.ticket.document_id
                || should_replace_preview(pending.ticket, job.ticket)
        });
        if replace_pending {
            if state.pending.replace(job).is_some() {
                state.stats.coalesced = state.stats.coalesced.saturating_add(1);
            }
        } else {
            state.stats.coalesced = state.stats.coalesced.saturating_add(1);
            return Ok(());
        }

        if state.wake_queued {
            return Ok(());
        }
        state.wake_queued = true;
        drop(state);

        if requests.send(WorkerRequest::PreviewAvailable).is_err() {
            let mut state = self.lock();
            state.pending = None;
            state.wake_queued = false;
            return Err("the background CPU worker stopped unexpectedly".to_owned());
        }
        Ok(())
    }

    pub(super) fn take(&self) -> Option<ScheduledPreview> {
        let mut state = self.lock();
        state.wake_queued = false;
        let job = state.pending.take()?;
        let cancellation = CancellationToken::new();
        state.active = Some(ActivePreview {
            ticket: job.ticket,
            cancellation: cancellation.clone(),
        });
        Some(ScheduledPreview { job, cancellation })
    }

    pub(super) fn finish(&self, ticket: PreviewTicket, completion: PreviewCompletion) {
        let mut state = self.lock();
        if state
            .active
            .as_ref()
            .is_some_and(|active| active.ticket == ticket)
        {
            state.active = None;
        }
        match completion {
            PreviewCompletion::Completed => {
                state.stats.completed = state.stats.completed.saturating_add(1);
            }
            PreviewCompletion::Cancelled => {
                state.stats.cancelled = state.stats.cancelled.saturating_add(1);
            }
            PreviewCompletion::Failed => {
                state.stats.failed = state.stats.failed.saturating_add(1);
            }
        }
    }

    pub(super) fn abandon(&self, document_id: u64) {
        let mut state = self.lock();
        if state
            .pending
            .as_ref()
            .is_some_and(|job| job.ticket.document_id == document_id)
        {
            state.pending = None;
            state.stats.coalesced = state.stats.coalesced.saturating_add(1);
        }
        if let Some(active) = &state.active
            && active.ticket.document_id == document_id
            && !active.cancellation.is_cancelled()
        {
            active.cancellation.cancel();
            state.stats.cancellation_requests = state.stats.cancellation_requests.saturating_add(1);
        }
    }

    pub(super) fn cancel_all(&self) {
        let mut state = self.lock();
        state.pending = None;
        if let Some(active) = &state.active
            && !active.cancellation.is_cancelled()
        {
            active.cancellation.cancel();
            state.stats.cancellation_requests = state.stats.cancellation_requests.saturating_add(1);
        }
    }

    pub(super) fn stats(&self) -> PreviewQueueStats {
        let state = self.lock();
        PreviewQueueStats {
            pending: state.pending.is_some(),
            active: state.active.is_some(),
            ..state.stats
        }
    }

    fn lock(&self) -> MutexGuard<'_, PreviewMailboxState> {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

pub(crate) fn should_replace_preview(current: PreviewTicket, candidate: PreviewTicket) -> bool {
    current.document_id == candidate.document_id
        && (candidate.revision > current.revision
            || (candidate.revision == current.revision && candidate.sequence >= current.sequence))
}
