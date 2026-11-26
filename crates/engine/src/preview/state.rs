//! Per-document preview state: request identity, resident GPU frame, histogram.
//!
//! A frontend keeps one [`DocumentPreview`] per open document. It owns what
//! guards asynchronous results (ticket sequence and intent), the resident GPU
//! source and frame, and the histogram, so every frontend applies the same
//! revision rules. Rendering is driven through [`super::PreviewRuntime`].

use std::borrow::Cow;
use std::time::{Duration, Instant};

use rohditor_core::Histogram;
use rohditor_edit::EditRecipe;

use super::gpu::GpuDocumentPreview;
use super::histogram::PendingGpuHistogram;
use super::routing::{gpu_supports_recipe, preview_intent_matches_resolution};
use super::{DocumentPreviewDiagnostics, PreviewRequest, PreviewSource};
use crate::coordinator::PreviewResolution;
use crate::document::{PreviewIntent, PreviewTicket};
use crate::view::ViewState;

/// Least time between the starts of two histogram readbacks, so a drag does
/// not read back every frame.
const GPU_HISTOGRAM_INTERVAL: Duration = Duration::from_millis(33);

/// The GPU display texture a frontend should be showing.
pub struct GpuDisplay<'a> {
    /// Oriented display texture; the frontend registers it with its toolkit.
    pub view: &'a wgpu::TextureView,
    pub width: u32,
    pub height: u32,
}

pub struct DocumentPreview {
    document_id: u64,
    sequence: u64,
    intent: PreviewIntent,
    pub(super) gpu: Option<GpuDocumentPreview>,
    /// Bumped whenever the GPU display texture changed or went away, so a
    /// frontend re-registers it only when needed.
    display_generation: u64,
    pub(super) pending_histogram: Option<PendingGpuHistogram>,
    pub(super) histogram_due: Option<(PreviewTicket, Instant)>,
    /// When the last histogram readback started.
    pub(super) histogram_started: Option<Instant>,
    /// Source 1:1 inspection is requested instead of the fitted preview.
    pub source_scale_requested: bool,
    /// The photo is to be shown without its crop, for a tool that frames it.
    full_frame_requested: bool,
    /// The displayed frame was rendered that way.
    full_frame_shown: bool,
    /// The background neural demosaic has finished, so a classic stand-in
    /// base no longer counts as current.
    pub neural_ready: bool,
    /// While editing this may be a few frames older than the displayed one.
    pub histogram: Option<Histogram>,
    /// The recipe revision `histogram` belongs to. Cleared while a newer
    /// revision renders, so an old graph is never used as Auto Tone input.
    pub histogram_revision: Option<u64>,
    /// Revision and description of the preview work in flight.
    pub status: Option<(u64, String)>,
    pub last_time: Option<Duration>,
    pub diagnostics: Option<DocumentPreviewDiagnostics>,
    /// Where the displayed frame came from.
    pub source: Option<PreviewSource>,
    pub view: ViewState,
}

impl DocumentPreview {
    pub fn new(document_id: u64) -> Self {
        Self {
            document_id,
            sequence: 0,
            intent: PreviewIntent::CommittedFit,
            gpu: None,
            display_generation: 0,
            pending_histogram: None,
            histogram_due: None,
            histogram_started: None,
            source_scale_requested: false,
            full_frame_requested: false,
            full_frame_shown: false,
            neural_ready: false,
            histogram: None,
            histogram_revision: None,
            status: None,
            last_time: None,
            diagnostics: None,
            source: None,
            view: ViewState::default(),
        }
    }

    pub const fn document_id(&self) -> u64 {
        self.document_id
    }

    pub const fn intent(&self) -> PreviewIntent {
        self.intent
    }

    /// The identity of the presentation expected right now at `revision`.
    pub const fn ticket(&self, revision: u64) -> PreviewTicket {
        PreviewTicket {
            document_id: self.document_id,
            revision,
            sequence: self.sequence,
        }
    }

    /// Start a new presentation request, invalidating every older result.
    pub fn begin_request(&mut self, revision: u64, intent: PreviewIntent) -> PreviewTicket {
        self.sequence = self.sequence.saturating_add(1);
        self.intent = intent;
        self.ticket(revision)
    }

    /// Whether a result carrying `ticket` still answers the current request.
    pub fn accepts(&self, revision: u64, ticket: PreviewTicket, intent: PreviewIntent) -> bool {
        ticket.is_current(self.document_id, revision, self.sequence) && self.intent == intent
    }

    /// Whether a prepared GPU source for `ticket` may still be adopted: it must
    /// be the current request, answer the fit/1:1 mode, and the GPU must be able
    /// to render the current recipe.
    pub fn gpu_upload_matches(
        &self,
        revision: u64,
        ticket: PreviewTicket,
        recipe: &EditRecipe,
    ) -> bool {
        ticket.document_id == self.document_id
            && self.ticket(revision) == ticket
            && preview_intent_matches_resolution(
                self.intent,
                if self.source_scale_requested {
                    PreviewResolution::SourceScale
                } else {
                    PreviewResolution::Fit
                },
            )
            && gpu_supports_recipe(recipe)
    }

    /// Show the whole photo in the viewport. Returns whether Source 1:1 was
    /// on, in which case the caller must queue a fitted preview.
    #[must_use = "leaving Source 1:1 needs a new preview"]
    pub fn request_fit(&mut self) -> bool {
        let was_source_scale = std::mem::take(&mut self.source_scale_requested);
        self.view.fit();
        was_source_scale
    }

    /// Show the photo without its crop, whatever the recipe says, or stop doing
    /// so. The caller must queue a preview when it changes; until that arrives
    /// [`Self::full_frame_shown`] does not follow.
    pub fn request_full_frame(&mut self, full_frame: bool) {
        self.full_frame_requested = full_frame;
        if full_frame {
            self.source_scale_requested = false;
            self.view.fit();
        }
    }

    /// Whether the displayed frame is the whole photo, without its crop.
    pub const fn full_frame_shown(&self) -> bool {
        self.full_frame_shown
    }

    /// The request as it is to be rendered.
    pub(super) fn presented<'a>(&self, request: &'a PreviewRequest) -> Cow<'a, PreviewRequest> {
        if !self.full_frame_requested || request.recipe.geometry.crop.is_none() {
            return Cow::Borrowed(request);
        }
        let mut request = request.clone();
        request.recipe.geometry.crop = None;
        Cow::Owned(request)
    }

    /// The frontend shows a CPU frame that answers the current request.
    pub fn cpu_frame_installed(&mut self, histogram: Histogram, revision: u64) {
        self.full_frame_shown = self.full_frame_requested;
        // Auto Tone and the graph are about the cropped photo.
        if !self.full_frame_shown {
            self.histogram = Some(histogram);
            self.histogram_revision = Some(revision);
        }
    }

    /// Ask for Source 1:1: one photo pixel per device pixel. Returns whether
    /// the mode changed, in which case the caller must queue a source-scale
    /// preview. The view only jumps to 100% when a source-scale frame is
    /// already on screen; otherwise it follows when that frame arrives (see
    /// [`ViewState::source_scale_frame_ready`]). Not while the whole photo is
    /// being framed.
    #[must_use = "entering Source 1:1 needs a new preview"]
    pub fn request_actual_size(&mut self) -> bool {
        if self.full_frame_requested {
            return false;
        }
        let changed = !std::mem::replace(&mut self.source_scale_requested, true);
        if self.source.is_some_and(PreviewSource::is_source_scale) {
            self.view.actual_size();
        }
        changed
    }

    pub const fn gpu(&self) -> Option<&GpuDocumentPreview> {
        self.gpu.as_ref()
    }

    pub const fn display_generation(&self) -> u64 {
        self.display_generation
    }

    /// The GPU display texture, when the displayed frame is a GPU frame.
    pub fn gpu_display(&self) -> Option<GpuDisplay<'_>> {
        let frame = &self.gpu.as_ref()?.frame;
        let (width, height) = frame.output_dimensions();
        Some(GpuDisplay {
            view: frame.display_view(),
            width,
            height,
        })
    }

    /// Drop the resident GPU source and frame. The displayed frame goes with
    /// them when it was a GPU frame; a CPU frame stays.
    pub fn release_gpu(&mut self) {
        self.pending_histogram = None;
        self.histogram_due = None;
        let had_gpu_frame = self.gpu.take().is_some();
        let displayed_gpu = self.source.is_some_and(PreviewSource::is_gpu);
        if displayed_gpu {
            self.full_frame_shown = false;
            self.source = None;
        }
        // A GPU frame that was on screen needs its texture released, even when
        // the resident source was already taken by a failed render.
        if had_gpu_frame || displayed_gpu {
            self.mark_display_changed();
        }
    }

    /// Record completion times of the GPU work once the queue reports them.
    /// Returns `true` while the completion is still outstanding.
    pub fn refresh_queue_completion(&mut self) -> bool {
        let Some(preview) = self.gpu.as_ref() else {
            return false;
        };
        let Some(diagnostics) = self.diagnostics.as_mut() else {
            return false;
        };
        if diagnostics.gpu_queue_completion.is_some() {
            return false;
        }
        if let Some(completion) = preview.frame.queue_completion_time() {
            diagnostics.gpu_queue_completion = Some(completion);
            self.last_time = Some(completion);
            false
        } else {
            true
        }
    }

    pub(super) fn mark_display_changed(&mut self) {
        self.display_generation = self.display_generation.wrapping_add(1);
    }

    /// Install a GPU frame as the displayed preview. The histogram readback is
    /// scheduled for fit frames, or whenever `histogram_always` is set.
    pub(super) fn install_gpu(&mut self, preview: GpuDocumentPreview, histogram_always: bool) {
        let ticket = preview.ticket;
        let resolution = preview.resolution;
        let algorithm = preview.algorithm;
        self.gpu = Some(preview);
        self.mark_display_changed();
        let due = next_histogram(Instant::now(), self.histogram_started);
        self.full_frame_shown = self.full_frame_requested;
        let wanted = resolution == PreviewResolution::Fit && !self.full_frame_shown;
        self.histogram_due = (wanted || histogram_always).then_some((ticket, due));
        self.source = Some(if resolution == PreviewResolution::SourceScale {
            PreviewSource::OneToOneGpu
        } else {
            PreviewSource::developed(algorithm, true)
        });
        self.status = None;
    }
}

/// When the readback of a frame rendered `now` starts. Deliberately not a
/// debounce: a drag renders frames faster than any pause would end.
fn next_histogram(now: Instant, last_started: Option<Instant>) -> Instant {
    last_started.map_or(now, |started| now.max(started + GPU_HISTOGRAM_INTERVAL))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::document::EditSession;

    #[test]
    fn gpu_upload_guard_requires_the_current_revision_and_supported_recipe() {
        let mut edits = EditSession::from_saved(EditRecipe::default());
        let preview = DocumentPreview::new(7);
        let ticket = preview.ticket(edits.revision());
        assert!(preview.gpu_upload_matches(edits.revision(), ticket, edits.recipe()));

        let mut recipe = edits.recipe().clone();
        recipe.color.hsl.channels[0].saturation = 0.2;
        assert!(edits.set_discrete(recipe));
        assert!(!preview.gpu_upload_matches(edits.revision(), ticket, edits.recipe()));
        let current = preview.ticket(edits.revision());
        assert!(preview.gpu_upload_matches(edits.revision(), current, edits.recipe()));

        let mut recipe = edits.recipe().clone();
        recipe.color.hsl = Default::default();
        assert!(edits.set_discrete(recipe));
        let latest = preview.ticket(edits.revision());
        assert!(preview.gpu_upload_matches(edits.revision(), latest, edits.recipe()));
        assert!(!preview.gpu_upload_matches(edits.revision(), current, edits.recipe()));
    }

    #[test]
    fn actual_size_waits_for_the_source_scale_frame_before_zooming() {
        let mut preview = DocumentPreview::new(1);
        preview.source = Some(PreviewSource::HighQualityCpu);
        preview
            .view
            .pan_by(0.5, [800.0, 600.0], [1_000.0, 1_000.0], [12.0, 8.0]);

        assert!(preview.request_actual_size(), "the mode changed");
        assert!(preview.source_scale_requested);
        assert!(
            !preview.view.is_actual_size(),
            "the fitted frame is still shown"
        );
        assert!(
            !preview.request_actual_size(),
            "asking again changes nothing"
        );

        // The frame arrives: the view enters 100% once.
        let previous = preview.source.replace(PreviewSource::OneToOneCpu);
        preview.view.source_scale_frame_ready(previous);
        assert!(preview.view.is_actual_size());
    }

    #[test]
    fn actual_size_again_recentres_an_already_shown_source_scale_frame() {
        let mut preview = DocumentPreview::new(1);
        preview.source = Some(PreviewSource::OneToOneGpu);
        preview.source_scale_requested = true;
        preview
            .view
            .pan_by(1.0, [800.0, 600.0], [2_000.0, 1_000.0], [90.0, -40.0]);

        assert!(!preview.request_actual_size());
        assert!(preview.view.is_actual_size());
        assert_eq!(preview.view.pan(), [0.0, 0.0]);
    }

    #[test]
    fn fit_leaves_source_scale_and_reports_that_a_new_preview_is_needed() {
        let mut preview = DocumentPreview::new(1);
        assert!(!preview.request_fit(), "already fitted: nothing to render");

        preview.source_scale_requested = true;
        assert!(preview.request_fit());
        assert!(!preview.source_scale_requested);
        assert!(preview.view.is_fit());
    }

    #[test]
    fn a_new_request_rejects_results_of_the_previous_one() {
        let mut preview = DocumentPreview::new(3);
        let first = preview.begin_request(5, PreviewIntent::CommittedFit);
        assert!(preview.accepts(5, first, PreviewIntent::CommittedFit));
        assert!(!preview.accepts(5, first, PreviewIntent::SourceScale));

        // Same revision, new presentation: a full frame must not be answered
        // by the earlier, cropped result.
        preview.request_full_frame(true);
        let second = preview.begin_request(5, PreviewIntent::CommittedFit);
        assert!(!preview.accepts(5, first, PreviewIntent::CommittedFit));
        assert!(preview.accepts(5, second, PreviewIntent::CommittedFit));
        assert!(!preview.accepts(6, second, PreviewIntent::CommittedFit));
    }

    #[test]
    fn a_full_frame_request_drops_the_crop_and_excludes_source_scale() {
        let mut recipe = EditRecipe::default();
        recipe.geometry.crop = Some(rohditor_edit::NormalizedCropRect {
            left: 0.1,
            top: 0.2,
            right: 0.7,
            bottom: 0.9,
        });
        let request = PreviewRequest {
            frame: None,
            recipe,
            revision: 4,
            options: Default::default(),
            histogram_always: false,
        };
        let mut preview = DocumentPreview::new(1);
        preview.source_scale_requested = true;
        assert_eq!(preview.presented(&request).recipe, request.recipe);

        preview.request_full_frame(true);
        assert!(!preview.source_scale_requested && preview.view.is_fit());
        assert!(!preview.request_actual_size());
        assert!(!preview.source_scale_requested);
        assert_eq!(preview.presented(&request).recipe.geometry.crop, None);
        assert!(!preview.full_frame_shown(), "the old frame is still shown");
        preview.cpu_frame_installed(Histogram::default(), 4);
        assert!(preview.full_frame_shown());
        assert_eq!(preview.histogram_revision, None);

        preview.request_full_frame(false);
        assert_eq!(preview.presented(&request).recipe, request.recipe);
        assert!(
            preview.full_frame_shown(),
            "until the cropped frame arrives"
        );
        preview.cpu_frame_installed(Histogram::default(), 4);
        assert!(!preview.full_frame_shown());
        assert_eq!(preview.histogram_revision, Some(4));
    }

    #[test]
    fn frames_of_a_drag_do_not_postpone_the_histogram() {
        let start = Instant::now();
        assert_eq!(next_histogram(start, None), start);
        let due = start + GPU_HISTOGRAM_INTERVAL;
        for frame in [1, 16, 32] {
            let now = start + Duration::from_millis(frame);
            assert_eq!(next_histogram(now, Some(start)), due);
        }
        let later = start + Duration::from_secs(1);
        assert_eq!(next_histogram(later, Some(start)), later);
    }

    #[test]
    fn releasing_a_cpu_frame_keeps_the_displayed_source() {
        let mut preview = DocumentPreview::new(1);
        preview.source = Some(PreviewSource::FastCpu);
        preview.release_gpu();
        assert_eq!(preview.source, Some(PreviewSource::FastCpu));
        assert_eq!(preview.display_generation(), 0);

        preview.source = Some(PreviewSource::HighQualityGpu);
        preview.release_gpu();
        assert_eq!(preview.source, None);
    }
}
