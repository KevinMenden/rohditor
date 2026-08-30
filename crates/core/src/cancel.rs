use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use crate::PipelineError;

/// Cheap, cloneable cancellation shared between a preview requester and the
/// CPU stages doing its work.
///
/// Cancellation is cooperative: image stages check the token at row and stage
/// boundaries, then return [`PipelineError::Cancelled`]. A token is single-use;
/// create a new token for each preview job.
#[derive(Debug, Clone, Default)]
pub struct CancellationToken {
    cancelled: Arc<AtomicBool>,
}

impl CancellationToken {
    /// Create an active token.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Request cancellation. Repeated calls are harmless.
    pub fn cancel(&self) {
        self.cancelled.store(true, Ordering::Release);
    }

    /// Whether cancellation has been requested.
    #[must_use]
    pub fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::Acquire)
    }

    /// Return the typed cancellation error when cancellation was requested.
    pub fn checkpoint(&self) -> Result<(), PipelineError> {
        if self.is_cancelled() {
            Err(PipelineError::Cancelled)
        } else {
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::CancellationToken;

    #[test]
    fn clones_observe_the_same_one_way_cancellation() {
        let token = CancellationToken::new();
        let worker = token.clone();
        assert!(!worker.is_cancelled());

        token.cancel();

        assert!(worker.is_cancelled());
        assert!(worker.checkpoint().is_err());
    }
}
