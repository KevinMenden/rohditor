//! Throttled readback of the displayed GPU frame for the histogram.
//!
//! After a GPU render the frame is read back asynchronously and counted on a
//! thread of its own into the document's [`Histogram`]. There is one readback
//! at a time. While editing, its result is shown although newer frames exist,
//! but only the current frame's counts as the recipe's histogram
//! ([`DocumentPreview::histogram_revision`]).

use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use rohditor_core::Histogram;
use rohditor_gpu::{GpuDisplayReadback, GpuDisplayReadbackPending, GpuPreviewError};
use tracing::warn;

use super::{DocumentPreview, PreviewRuntime};
use crate::document::PreviewTicket;

/// Polling interval while waiting on a readback.
const POLL_INTERVAL: Duration = Duration::from_millis(8);

/// A display readback on its way to becoming the histogram.
pub(super) struct PendingGpuHistogram {
    ticket: PreviewTicket,
    started: Instant,
    stage: Stage,
}

enum Stage {
    /// The GPU has not finished the copy.
    Reading(GpuDisplayReadbackPending),
    Counting(CountTask),
}

type Counted = Result<(Option<Histogram>, GpuDisplayReadback), GpuPreviewError>;

/// Copies and counts the pixels on a thread, which takes too long for a UI.
///
/// Dropping it waits for the thread: that holds a GPU buffer, and a frontend
/// must be able to let go of its device.
struct CountTask(Option<JoinHandle<Counted>>);

impl CountTask {
    fn start(readback: GpuDisplayReadbackPending) -> std::io::Result<Self> {
        std::thread::Builder::new()
            .name("rohditor-histogram".to_owned())
            .spawn(move || {
                let readback = readback.finish()?;
                let histogram = Histogram::from_rgba8(
                    readback.width as usize,
                    readback.height as usize,
                    &readback.rgba,
                );
                Ok((histogram, readback))
            })
            .map(|thread| Self(Some(thread)))
    }

    fn is_finished(&self) -> bool {
        self.0.as_ref().is_none_or(JoinHandle::is_finished)
    }

    /// The result of a finished task; `None` if its thread panicked.
    fn take(mut self) -> Option<Counted> {
        self.0.take()?.join().ok()
    }
}

impl Drop for CountTask {
    fn drop(&mut self) {
        if let Some(thread) = self.0.take() {
            drop(thread.join());
        }
    }
}

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
    /// Advance the throttled histogram readback of the displayed GPU frame.
    /// `want_pixels` returns the finished readback's pixels too.
    pub fn poll_histogram(
        &self,
        preview: &mut DocumentPreview,
        revision: u64,
        want_pixels: bool,
    ) -> HistogramPoll {
        if let Some(pending) = preview.pending_histogram.take() {
            let poll = advance(preview, revision, pending, want_pixels);
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
                preview.histogram_started = Some(now);
                preview.pending_histogram = Some(PendingGpuHistogram {
                    ticket,
                    started: now,
                    stage: Stage::Reading(readback),
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
}

/// Take a pending readback one stage further, or install its histogram.
fn advance(
    preview: &mut DocumentPreview,
    revision: u64,
    mut pending: PendingGpuHistogram,
    want_pixels: bool,
) -> HistogramPoll {
    pending.stage = match pending.stage {
        Stage::Reading(mut readback) => match readback.poll_ready() {
            Ok(false) => Stage::Reading(readback),
            Ok(true) => match CountTask::start(readback) {
                Ok(task) => Stage::Counting(task),
                Err(error) => {
                    warn!(ticket = ?pending.ticket, %error, "could not start counting");
                    return HistogramPoll::default();
                }
            },
            Err(error) => {
                warn!(ticket = ?pending.ticket, %error, "GPU histogram readback failed");
                return HistogramPoll::default();
            }
        },
        Stage::Counting(task) if task.is_finished() => {
            return match task.take() {
                Some(Ok((histogram, readback))) => install(
                    preview,
                    revision,
                    pending.ticket,
                    pending.started.elapsed(),
                    histogram,
                    readback,
                    want_pixels,
                ),
                Some(Err(error)) => {
                    warn!(ticket = ?pending.ticket, %error, "GPU histogram readback failed");
                    HistogramPoll::default()
                }
                None => HistogramPoll::default(),
            };
        }
        counting => counting,
    };
    preview.pending_histogram = Some(pending);
    HistogramPoll {
        repaint_after: Some(POLL_INTERVAL),
        ..HistogramPoll::default()
    }
}

/// Show a counted histogram. Only the one of the current frame is the input
/// for Auto Tone, and only its pixels are handed out.
fn install(
    preview: &mut DocumentPreview,
    revision: u64,
    ticket: PreviewTicket,
    latency: Duration,
    histogram: Option<Histogram>,
    readback: GpuDisplayReadback,
    want_pixels: bool,
) -> HistogramPoll {
    let mut poll = HistogramPoll {
        repaint: true,
        ..HistogramPoll::default()
    };
    let Some(histogram) = histogram else {
        return poll;
    };
    let current = preview.ticket(revision) == ticket
        && preview
            .gpu
            .as_ref()
            .is_some_and(|resident| resident.ticket == ticket);
    preview.histogram = Some(histogram);
    if !current {
        return poll;
    }
    preview.histogram_revision = Some(ticket.revision);
    if let Some(diagnostics) = preview.diagnostics.as_mut() {
        diagnostics.gpu_histogram_readback = Some(latency);
    }
    if want_pixels {
        poll.pixels = Some(DisplayPixels {
            width: readback.width as usize,
            height: readback.height as usize,
            rgba: readback.rgba,
        });
    }
    poll
}
