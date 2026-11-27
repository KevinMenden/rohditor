//! Throttled GPU histogram of the displayed frame.
//!
//! After a GPU render the frame's histogram is counted on the GPU and its bins
//! are read back asynchronously into the document's [`Histogram`]. There is
//! one count at a time. While editing, its result is shown although newer
//! frames exist, but only the current frame's counts as the recipe's histogram
//! ([`DocumentPreview::histogram_revision`]).

use std::time::{Duration, Instant};

use rohditor_core::Histogram;
use rohditor_gpu::GpuHistogramPending;
use tracing::warn;

use super::{DocumentPreview, PreviewRuntime};
use crate::document::PreviewTicket;

/// Polling interval while waiting on the GPU.
const POLL_INTERVAL: Duration = Duration::from_millis(8);

/// A histogram count on its way back from the GPU.
pub(super) struct PendingGpuHistogram {
    ticket: PreviewTicket,
    started: Instant,
    bins: GpuHistogramPending,
}

/// Progress of the GPU histogram.
#[derive(Default)]
pub struct HistogramPoll {
    pub repaint: bool,
    /// Call again after this long.
    pub repaint_after: Option<Duration>,
}

impl PreviewRuntime {
    /// Advance the throttled histogram count of the displayed GPU frame.
    pub fn poll_histogram(&self, preview: &mut DocumentPreview, revision: u64) -> HistogramPoll {
        if let Some(pending) = preview.pending_histogram.take() {
            let poll = advance(preview, revision, pending);
            if preview.pending_histogram.is_some() || preview.histogram_due.is_none() {
                return poll;
            }
            // A newer frame waits for its turn.
            return HistogramPoll {
                repaint_after: Some(Duration::ZERO),
                ..poll
            };
        }
        let Some((ticket, due)) = preview.histogram_due else {
            return HistogramPoll::default();
        };
        let now = Instant::now();
        if due > now {
            return HistogramPoll {
                repaint_after: Some(due.duration_since(now)),
                ..HistogramPoll::default()
            };
        }

        preview.histogram_due = None;
        let bins = preview
            .gpu
            .as_ref()
            .filter(|_| is_current(preview, revision, ticket))
            .zip(self.gpu.as_ref())
            .map(|(resident, runtime)| runtime.processor().begin_histogram(&resident.frame));
        let Some(bins) = bins else {
            return HistogramPoll::default();
        };
        preview.histogram_started = Some(now);
        preview.pending_histogram = Some(PendingGpuHistogram {
            ticket,
            started: now,
            bins,
        });
        HistogramPoll {
            repaint_after: Some(POLL_INTERVAL),
            ..HistogramPoll::default()
        }
    }
}

/// Whether `ticket` is the frame on screen and answers the current recipe.
fn is_current(preview: &DocumentPreview, revision: u64, ticket: PreviewTicket) -> bool {
    preview.ticket(revision) == ticket
        && preview
            .gpu
            .as_ref()
            .is_some_and(|resident| resident.ticket == ticket)
}

/// Install the histogram of a pending count once the GPU has finished it.
fn advance(
    preview: &mut DocumentPreview,
    revision: u64,
    mut pending: PendingGpuHistogram,
) -> HistogramPoll {
    match pending.bins.poll_ready() {
        Ok(false) => {
            preview.pending_histogram = Some(pending);
            HistogramPoll {
                repaint_after: Some(POLL_INTERVAL),
                ..HistogramPoll::default()
            }
        }
        Ok(true) => {
            let latency = pending.started.elapsed();
            install(
                preview,
                revision,
                pending.ticket,
                latency,
                pending.bins.finish(),
            )
        }
        Err(error) => {
            warn!(ticket = ?pending.ticket, %error, "GPU histogram readback failed");
            HistogramPoll::default()
        }
    }
}

/// Show a counted histogram. Only the one of the current frame is the input
/// for Auto Tone.
fn install(
    preview: &mut DocumentPreview,
    revision: u64,
    ticket: PreviewTicket,
    latency: Duration,
    histogram: Histogram,
) -> HistogramPoll {
    let poll = HistogramPoll {
        repaint: true,
        ..HistogramPoll::default()
    };
    // Auto Tone and the graph are about the cropped photo.
    if !preview.full_frame_shown() {
        preview.histogram = Some(histogram);
    }
    if !is_current(preview, revision, ticket) {
        return poll;
    }
    if !preview.full_frame_shown() {
        preview.histogram_revision = Some(ticket.revision);
    }
    if let Some(diagnostics) = preview.diagnostics.as_mut() {
        diagnostics.gpu_histogram_readback = Some(latency);
    }
    poll
}
