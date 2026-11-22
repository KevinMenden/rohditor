//! Debounced readback of the displayed GPU frame for the histogram.
//!
//! After a GPU render the frame is read back asynchronously (after a short
//! debounce, so a drag does not queue one per frame) and turned into the
//! document's [`Histogram`]. A result is applied only while its ticket is
//! still the current one.

use std::time::{Duration, Instant};

use rohditor_core::Histogram;
use tracing::warn;

use super::gpu::PendingGpuHistogram;
use super::{DocumentPreview, PreviewRuntime};

/// Polling interval while waiting on a readback.
const POLL_INTERVAL: Duration = Duration::from_millis(16);

/// Pixels of a finished display readback, for overlays derived from them.
pub struct DisplayPixels {
    pub width: usize,
    pub height: usize,
    /// Packed RGBA8, row-major.
    pub rgba: Vec<u8>,
}

/// Progress of the histogram readback.
#[derive(Default)]
pub struct HistogramPoll {
    pub repaint: bool,
    /// Call again after this long.
    pub repaint_after: Option<Duration>,
    /// The finished readback, when the caller asked for pixels.
    pub pixels: Option<DisplayPixels>,
}

impl PreviewRuntime {
    /// Advance the debounced histogram readback of the displayed GPU frame.
    /// `want_pixels` returns the finished readback's pixels too.
    pub fn poll_histogram(
        &self,
        preview: &mut DocumentPreview,
        revision: u64,
        want_pixels: bool,
    ) -> HistogramPoll {
        if let Some(pending) = preview.pending_histogram.take() {
            return self.finish_histogram(preview, revision, pending, want_pixels);
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

        let current = preview.ticket(revision) == ticket
            && preview
                .gpu
                .as_ref()
                .is_some_and(|resident| resident.ticket == ticket);
        let readback = preview
            .gpu
            .as_ref()
            .filter(|_| current)
            .and_then(|resident| {
                self.gpu
                    .as_ref()
                    .map(|runtime| runtime.processor().begin_display_readback(&resident.frame))
            });
        preview.histogram_due = None;
        match readback {
            Some(Ok(readback)) => {
                preview.pending_histogram = Some(PendingGpuHistogram {
                    ticket,
                    readback,
                    started: Instant::now(),
                });
                HistogramPoll {
                    repaint_after: Some(POLL_INTERVAL),
                    ..HistogramPoll::default()
                }
            }
            Some(Err(error)) => {
                warn!(?ticket, %error, "could not start GPU histogram readback");
                HistogramPoll::default()
            }
            None => HistogramPoll::default(),
        }
    }

    fn finish_histogram(
        &self,
        preview: &mut DocumentPreview,
        revision: u64,
        mut pending: PendingGpuHistogram,
        want_pixels: bool,
    ) -> HistogramPoll {
        let latency = pending.started.elapsed();
        let still_current = |preview: &DocumentPreview| {
            preview.ticket(revision) == pending.ticket
                && preview
                    .gpu
                    .as_ref()
                    .is_some_and(|resident| resident.ticket == pending.ticket)
        };
        match pending.readback.try_finish() {
            Ok(Some(readback)) => {
                let size = usize::try_from(readback.width).ok().and_then(|width| {
                    usize::try_from(readback.height)
                        .ok()
                        .map(|height| (width, height))
                });
                let histogram = size.and_then(|(width, height)| {
                    Histogram::from_rgba8(width, height, &readback.rgba)
                });
                let mut poll = HistogramPoll {
                    repaint: true,
                    ..HistogramPoll::default()
                };
                if still_current(preview)
                    && let Some(histogram) = histogram
                {
                    preview.histogram = Some(histogram);
                    preview.histogram_revision = Some(pending.ticket.revision);
                    if want_pixels && let Some((width, height)) = size {
                        poll.pixels = Some(DisplayPixels {
                            width,
                            height,
                            rgba: readback.rgba,
                        });
                    }
                    if let Some(diagnostics) = preview.diagnostics.as_mut() {
                        diagnostics.gpu_histogram_readback = Some(latency);
                    }
                }
                poll
            }
            Ok(None) => {
                if still_current(preview) {
                    preview.pending_histogram = Some(pending);
                    HistogramPoll {
                        repaint_after: Some(POLL_INTERVAL),
                        ..HistogramPoll::default()
                    }
                } else {
                    HistogramPoll::default()
                }
            }
            Err(error) => {
                warn!(ticket = ?pending.ticket, %error, "GPU histogram readback failed");
                HistogramPoll::default()
            }
        }
    }
}
