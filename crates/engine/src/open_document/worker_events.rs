//! How worker results change an [`OpenDocument`].
//!
//! The frontend routes a `WorkerEvent` to the open document when its id
//! matches, then calls the matching method here. Methods return what the
//! frontend must do next instead of touching any UI state.

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use rohditor_core::{
    DatabaseProvenance, ExportReport, LensProfileSummary, ProfileMatch,
    white_balance_from_camera_sample,
};
use rohditor_raw::{RawFileInfo, RawFrame};

use super::{ExportActivity, OpenDocument};
use crate::coordinator::JobKind;
use crate::document::PreviewTicket;

impl OpenDocument {
    /// A worker reported what a running job is doing.
    pub fn job_progress(
        &mut self,
        job: JobKind,
        revision: Option<u64>,
        preview_ticket: Option<PreviewTicket>,
        export_id: Option<u64>,
        detail: String,
    ) {
        match job {
            JobKind::Open => self.open_status = Some(detail),
            JobKind::Preview => {
                if preview_ticket.is_some_and(|ticket| self.ticket() == ticket)
                    || (preview_ticket.is_none() && revision == Some(self.edits.revision()))
                {
                    self.preview.status = Some((self.edits.revision(), detail));
                }
            }
            JobKind::Export => {
                if let Some(activity) = self.export_status.as_mut()
                    && export_id == Some(activity.id)
                {
                    activity.detail = detail;
                }
            }
        }
    }

    /// The file's metadata was read. A photo without saved edits rebuilds its
    /// default recipe from it, as long as nothing was edited meanwhile.
    pub fn metadata_ready(&mut self, info: RawFileInfo) {
        self.info = Some(info);
        if self.initialize_white_balance_from_metadata {
            self.initialize_white_balance_from_metadata = false;
            if !self.edits.is_dirty() {
                let recipe = self.default_recipe();
                self.edits.replace_clean_baseline(recipe);
            }
        }
    }

    pub fn optics_match_ready(
        &mut self,
        result: Option<ProfileMatch>,
        candidates: Vec<LensProfileSummary>,
        database: Option<DatabaseProvenance>,
        error: Option<String>,
    ) {
        self.optics_match = result;
        self.optics_candidates = candidates;
        self.optics_database = database;
        self.optics_error = error;
    }

    /// The RAW frame is decoded; the document can be rendered.
    pub fn raw_ready(&mut self, frame: Arc<RawFrame>) {
        self.info = Some(frame.info.clone());
        self.frame = Some(frame);
        self.open_status = None;
    }

    /// An export finished. Returns false for an export the document no longer
    /// tracks.
    pub fn export_ready(
        &mut self,
        export_id: u64,
        recipe_revision: u64,
        destination: &Path,
        report: &ExportReport,
        elapsed: Duration,
    ) -> bool {
        if self.export_status.as_ref().map(|job| job.id) != Some(export_id) {
            return false;
        }
        self.export_status = None;
        self.error = None;
        self.notice = Some(format!(
            "Exported revision {recipe_revision} as {}-bit {}×{} ({} bytes) to {} in {:.2} s.",
            report.bit_depth.bits(),
            report.width,
            report.height,
            report.bytes_written,
            destination.display(),
            elapsed.as_secs_f64()
        ));
        true
    }

    /// Remember an export that was handed to the worker.
    pub fn export_started(&mut self, activity: ExportActivity) {
        self.export_status = Some(activity);
    }

    /// A job failed. A failure of a superseded preview or export is dropped.
    pub fn job_failed(
        &mut self,
        job: JobKind,
        revision: Option<u64>,
        export_id: Option<u64>,
        message: String,
    ) {
        let current = match job {
            JobKind::Open => {
                self.open_status = None;
                true
            }
            JobKind::Preview => {
                let current = revision == Some(self.edits.revision());
                if current {
                    self.preview.status = None;
                }
                current
            }
            JobKind::Export => {
                let current = self.export_status.as_ref().map(|job| job.id) == export_id;
                if current {
                    self.export_status = None;
                }
                current
            }
        };
        if current {
            self.error = Some(message);
        }
    }

    /// The worker thread died: every job of the document is gone.
    pub fn worker_stopped(&mut self, message: String) {
        self.open_status = None;
        self.preview.status = None;
        self.export_status = None;
        self.error = Some(message);
    }

    /// Whether a white-balance sample for `ticket` still belongs to this
    /// document's current recipe. A stale one drops the status of the preview
    /// work it was waiting on; the caller then forgets the pick.
    pub fn white_balance_pick_is_current(&mut self, ticket: PreviewTicket) -> bool {
        if self.id != ticket.document_id {
            return false;
        }
        if self.ticket() != ticket {
            if self
                .preview
                .status
                .as_ref()
                .is_some_and(|(revision, _)| *revision == ticket.revision)
            {
                self.preview.status = None;
            }
            return false;
        }
        true
    }

    /// Turn the sampled camera RGB into the document's white balance. Returns
    /// whether the recipe changed, so the caller must queue a preview.
    pub fn apply_white_balance_sample(&mut self, sample: [f32; 3]) -> bool {
        self.preview.status = None;
        let Some(resolved) = self.as_shot_colour() else {
            self.error =
                Some("The RAW file did not provide a usable camera colour calibration".to_owned());
            return false;
        };
        let as_shot = self
            .frame
            .as_ref()
            .map(|frame| frame.info.as_shot_white_balance)
            .or_else(|| self.info.as_ref().map(|info| info.as_shot_white_balance));
        let Some(balance) =
            white_balance_from_camera_sample(sample, resolved.unbalanced_camera_to_xyz, as_shot)
        else {
            self.error = Some(
                "That sample could not be represented by the available white-balance range"
                    .to_owned(),
            );
            return false;
        };
        let mut next = self.edits.recipe().clone();
        next.color.white_balance = balance;
        let changed = self.edits.set_discrete(next);
        self.notice = None;
        self.error = None;
        changed
    }

    /// The white-balance sample could not be taken.
    pub fn white_balance_sample_failed(&mut self, message: String) {
        self.preview.status = None;
        self.error = Some(message);
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use rohditor_core::default_recipe;

    use super::*;
    use crate::document::PreviewIntent;

    fn document() -> OpenDocument {
        OpenDocument::opening(3, PathBuf::from("fixture.arw"), default_recipe(None), None)
    }

    #[test]
    fn a_failure_of_a_superseded_preview_is_dropped() {
        let mut document = document();
        let stale = Some(document.edits.revision() + 1);
        document.preview.status = Some((document.edits.revision(), "rendering".to_owned()));
        document.job_failed(JobKind::Preview, stale, None, "late".to_owned());
        assert!(document.error.is_none());
        assert!(document.preview.status.is_some());
        document.job_failed(
            JobKind::Preview,
            Some(document.edits.revision()),
            None,
            "current".to_owned(),
        );
        assert_eq!(document.error.as_deref(), Some("current"));
        assert!(document.preview.status.is_none());
    }

    #[test]
    fn a_failed_open_always_reports_and_clears_the_status() {
        let mut document = document();
        assert!(document.open_status.is_some());
        document.job_failed(JobKind::Open, None, None, "unreadable".to_owned());
        assert!(document.open_status.is_none());
        assert_eq!(document.error.as_deref(), Some("unreadable"));
    }

    #[test]
    fn only_the_tracked_export_settles_the_document() {
        let mut document = document();
        document.export_started(ExportActivity {
            id: 5,
            recipe_revision: 0,
            detail: String::new(),
        });
        document.job_progress(JobKind::Export, None, None, Some(4), "other".to_owned());
        assert!(
            document
                .export_status
                .as_ref()
                .is_some_and(|job| job.detail.is_empty())
        );
        document.job_progress(JobKind::Export, None, None, Some(5), "encoding".to_owned());
        assert!(
            document
                .export_status
                .as_ref()
                .is_some_and(|job| job.detail == "encoding")
        );
        document.job_failed(JobKind::Export, None, Some(4), "other".to_owned());
        assert!(document.export_status.is_some() && document.error.is_none());
        document.job_failed(JobKind::Export, None, Some(5), "disk full".to_owned());
        assert!(document.export_status.is_none());
        assert_eq!(document.error.as_deref(), Some("disk full"));
    }

    #[test]
    fn a_stopped_worker_clears_every_running_job() {
        let mut document = document();
        document.preview.status = Some((0, "rendering".to_owned()));
        document.export_started(ExportActivity {
            id: 1,
            recipe_revision: 0,
            detail: String::new(),
        });
        document.worker_stopped("gone".to_owned());
        assert!(document.open_status.is_none());
        assert!(document.preview.status.is_none());
        assert!(document.export_status.is_none());
        assert_eq!(document.error.as_deref(), Some("gone"));
    }

    #[test]
    fn a_stale_white_balance_pick_is_rejected_and_frees_its_status() {
        let mut document = document();
        let ticket = document.begin_preview_request(PreviewIntent::CommittedFit);
        assert!(document.white_balance_pick_is_current(ticket));
        // An edit moves the revision on: the sample no longer matches.
        let mut next = document.edits.recipe().clone();
        next.light.exposure_ev = 0.5;
        assert!(document.edits.set_discrete(next));
        document.preview.status = Some((ticket.revision, "sampling".to_owned()));
        assert!(!document.white_balance_pick_is_current(ticket));
        assert!(document.preview.status.is_none());
        // A ticket of another document never matches.
        let mut foreign = ticket;
        foreign.document_id += 1;
        assert!(!document.white_balance_pick_is_current(foreign));
    }
}
