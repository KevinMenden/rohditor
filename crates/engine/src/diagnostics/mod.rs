//! Developer diagnostics: what the preview pipeline did last, and a
//! support-safe report of it.
//!
//! [`build`] gathers a [`Diagnostics`] snapshot from the runtime state a
//! frontend holds; frontends only render it. [`save_report`] writes it as
//! JSON. Source paths and image data never appear in it.

mod build;
mod report;

use std::time::Duration;

use serde::{Serialize, Serializer};

use crate::coordinator::PreviewQueueStats;

pub use build::{DiagnosticsSources, build, processor_description};
pub use report::{REPORT_FORMAT_VERSION, report_json, save_report};

/// One snapshot of the developer diagnostics.
#[derive(Debug, Clone)]
pub struct Diagnostics {
    /// Which processor renders previews, and how the last one was routed.
    pub processor: String,
    /// The frontend's renderer, for example "GPUI".
    pub ui_renderer: String,
    /// Adapter, backend and timestamp-query support of the GPU, if one is in use.
    pub gpu_device: Option<String>,
    pub queue: PreviewQueueStats,
    /// `None` while no folder is shown.
    pub catalog: Option<CatalogDiagnostics>,
    /// `None` until a photo has a developed preview.
    pub preview: Option<PreviewDiagnostics>,
    pub messages: DiagnosticsMessages,
}

impl Diagnostics {
    /// The preview queue counters.
    pub fn queue_rows(&self) -> Vec<(&'static str, String)> {
        let queue = &self.queue;
        vec![
            ("Requested", queue.requested.to_string()),
            ("Coalesced", queue.coalesced.to_string()),
            ("Cancel requests", queue.cancellation_requests.to_string()),
            ("Cancelled", queue.cancelled.to_string()),
            ("Completed", queue.completed.to_string()),
            ("Failed", queue.failed.to_string()),
            ("Active", queue.active.to_string()),
            ("Pending", queue.pending.to_string()),
        ]
    }
}

#[derive(Debug, Clone, Copy, Default, Serialize)]
pub struct CatalogDiagnostics {
    pub photos: usize,
    pub thumbnails_ready: usize,
    pub thumbnails_placeholder: usize,
    pub thumbnails_pending: usize,
    pub thumbnails_failed: usize,
    pub resident_bytes: usize,
}

impl CatalogDiagnostics {
    pub fn rows(&self) -> Vec<(&'static str, String)> {
        vec![
            ("Photos", self.photos.to_string()),
            ("Thumbnails ready", self.thumbnails_ready.to_string()),
            (
                "Thumbnails placeholder",
                self.thumbnails_placeholder.to_string(),
            ),
            ("Thumbnails pending", self.thumbnails_pending.to_string()),
            ("Thumbnails failed", self.thumbnails_failed.to_string()),
            ("Encoded bytes", format_bytes(self.resident_bytes)),
        ]
    }
}

/// The last developed preview of the open photo.
#[derive(Debug, Clone, Serialize)]
pub struct PreviewDiagnostics {
    pub backend: String,
    pub algorithm: String,
    pub profile: String,
    pub false_color: String,
    pub noise_reduction: String,
    pub capture_sharpening: String,
    pub rendering: String,
    pub output_policy: String,
    pub output_gamut_statistics: Option<String>,
    pub source_state: String,
    pub cache: CacheDiagnostics,
    #[serde(rename = "timings_ms")]
    pub timings: TimingDiagnostics,
    pub highlight: HighlightDiagnosticsSummary,
    pub cache_resident_bytes: usize,
    pub estimated_peak_bytes: usize,
    pub gpu: Option<GpuDiagnostics>,
}

impl PreviewDiagnostics {
    /// How the preview was developed, as lines of text.
    pub fn summary(&self) -> Vec<String> {
        let mut lines = vec![
            format!("{} source · {} demosaic", self.source_state, self.algorithm),
            format!("Camera colour: {}", self.profile),
            format!("False colour: {}", self.false_color),
            format!("Noise reduction: {}", self.noise_reduction),
            format!("Capture sharpening: {}", self.capture_sharpening),
            format!("Rendering profile: {}", self.rendering),
            format!("Output gamut policy: {}", self.output_policy),
        ];
        lines.extend(
            self.output_gamut_statistics
                .as_ref()
                .map(|statistics| format!("Output gamut pixels: {statistics}")),
        );
        lines
    }

    /// Cached memory and the estimated peak of a render.
    pub fn memory(&self) -> String {
        format!(
            "CPU cache: {} · estimated render peak: {}",
            format_bytes(self.cache_resident_bytes),
            format_bytes(self.estimated_peak_bytes)
        )
    }
}

/// Which cached stages the preview reused.
#[derive(Debug, Clone, Copy, Serialize)]
pub struct CacheDiagnostics {
    pub decoded: bool,
    pub reconstructed: bool,
    pub demosaiced: bool,
    pub adjusted: bool,
    pub workspace_reused: bool,
}

impl CacheDiagnostics {
    pub fn rows(&self) -> Vec<(&'static str, String)> {
        let workspace = if self.workspace_reused {
            "reused"
        } else {
            "allocated or unused"
        };
        vec![
            ("DecodedRaw", cache_result(self.decoded).to_owned()),
            ("CameraRgbBase", cache_result(self.reconstructed).to_owned()),
            ("DemosaicedBase", cache_result(self.demosaiced).to_owned()),
            ("AdjustedPreview", cache_result(self.adjusted).to_owned()),
            ("CPU working buffer", workspace.to_owned()),
        ]
    }
}

/// CPU stage wall times, reported in milliseconds.
#[derive(Debug, Clone, Copy, Serialize)]
pub struct TimingDiagnostics {
    #[serde(serialize_with = "milliseconds")]
    pub metadata: Duration,
    #[serde(serialize_with = "milliseconds")]
    pub normalization: Duration,
    #[serde(serialize_with = "milliseconds")]
    pub highlight_processing: Duration,
    #[serde(serialize_with = "milliseconds")]
    pub demosaic: Duration,
    #[serde(serialize_with = "milliseconds")]
    pub false_color: Duration,
    #[serde(serialize_with = "milliseconds")]
    pub chroma_noise_reduction: Duration,
    #[serde(serialize_with = "milliseconds")]
    pub luminance_noise_reduction: Duration,
    #[serde(serialize_with = "milliseconds")]
    pub capture_sharpening: Duration,
    #[serde(serialize_with = "milliseconds")]
    pub optics: Duration,
    #[serde(serialize_with = "milliseconds")]
    pub defringe: Duration,
    #[serde(serialize_with = "milliseconds")]
    pub resampling: Duration,
    #[serde(serialize_with = "milliseconds")]
    pub color_conversion: Duration,
    #[serde(serialize_with = "milliseconds")]
    pub adjustments: Duration,
    #[serde(serialize_with = "milliseconds")]
    pub output_conversion: Duration,
    #[serde(serialize_with = "milliseconds")]
    pub total: Duration,
}

impl TimingDiagnostics {
    /// Every stage with its time, in pipeline order, the total last.
    pub fn rows(&self) -> Vec<(&'static str, String)> {
        [
            ("Metadata", self.metadata),
            ("Normalization", self.normalization),
            ("Highlight processing", self.highlight_processing),
            ("Demosaic", self.demosaic),
            ("False colour suppression", self.false_color),
            ("Chroma noise reduction", self.chroma_noise_reduction),
            ("Luminance noise reduction", self.luminance_noise_reduction),
            ("Capture sharpening", self.capture_sharpening),
            ("Optics", self.optics),
            ("Defringe", self.defringe),
            ("Area reduction", self.resampling),
            ("Color conversion", self.color_conversion),
            ("Adjustments", self.adjustments),
            ("Output conversion", self.output_conversion),
            ("Total", self.total),
        ]
        .into_iter()
        .map(|(label, duration)| (label, format_duration(duration)))
        .collect()
    }
}

/// What highlight reconstruction did, summed over the CFA sites.
#[derive(Debug, Clone, Copy, Default, Serialize)]
#[serde(tag = "method", rename_all = "snake_case")]
pub enum HighlightDiagnosticsSummary {
    #[default]
    Off,
    Clip {
        affected_sites: usize,
        changed_sites: usize,
        nominal_over_white_sites: usize,
        affected_by_channel: [usize; 3],
    },
    Opposed {
        suspected_clipped_sites: usize,
        reconstructed_sites: usize,
        changed_sites: usize,
        fallback_sites: usize,
        fully_unsupported_sites: usize,
        neutralised_sites: usize,
        suspected_by_channel: [usize; 3],
    },
}

impl HighlightDiagnosticsSummary {
    /// The summary as lines of text.
    pub fn lines(&self) -> Vec<String> {
        match *self {
            Self::Off => vec!["Highlight reconstruction: off".to_owned()],
            Self::Clip {
                affected_sites,
                changed_sites,
                nominal_over_white_sites,
                affected_by_channel: [red, green, blue],
            } => vec![
                format!(
                    "Highlight Clip: {affected_sites} affected · {changed_sites} changed · \
                     {nominal_over_white_sites} nominally over-white"
                ),
                format!("Affected CFA sites: R {red} · G {green} · B {blue}"),
            ],
            Self::Opposed {
                suspected_clipped_sites,
                reconstructed_sites,
                changed_sites,
                fallback_sites,
                fully_unsupported_sites,
                neutralised_sites,
                suspected_by_channel: [red, green, blue],
            } => vec![
                format!(
                    "Highlight Opposed: {suspected_clipped_sites} suspected · \
                     {reconstructed_sites} reconstructed · {changed_sites} changed"
                ),
                format!(
                    "Fallback: {fallback_sites} · fully unsupported: {fully_unsupported_sites} · \
                     suspected CFA sites: R {red} · G {green} · B {blue}"
                ),
                format!("Neutralised to the as-shot WB ceiling: {neutralised_sites}"),
            ],
        }
    }
}

/// GPU timings of the preview; `None` timings are still pending.
#[derive(Debug, Clone, Copy, Serialize)]
pub struct GpuDiagnostics {
    #[serde(
        rename = "upload_preparation_ms",
        serialize_with = "optional_milliseconds"
    )]
    pub upload_preparation: Option<Duration>,
    #[serde(rename = "submission_ms", serialize_with = "optional_milliseconds")]
    pub submission: Option<Duration>,
    #[serde(
        rename = "queue_completion_ms",
        serialize_with = "optional_milliseconds"
    )]
    pub queue_completion: Option<Duration>,
    #[serde(
        rename = "histogram_readback_ms",
        serialize_with = "optional_milliseconds"
    )]
    pub histogram_readback: Option<Duration>,
    pub textures_reused: Option<bool>,
    pub resident_bytes: usize,
}

impl GpuDiagnostics {
    /// Said under the GPU rows.
    pub const NOTE: &str = "Queue completion is conservative wall latency; it includes \
        shared-queue delay when timestamp queries are unavailable.";

    pub fn rows(&self) -> Vec<(&'static str, String)> {
        let pending = |duration: Option<Duration>| {
            duration.map_or_else(|| "pending".to_owned(), format_duration)
        };
        let textures = match self.textures_reused {
            Some(true) => "reused",
            Some(false) => "allocated",
            None => "n/a",
        };
        vec![
            ("CPU upload packing", pending(self.upload_preparation)),
            ("Encode + submit", pending(self.submission)),
            ("Queue completion", pending(self.queue_completion)),
            ("Histogram readback", pending(self.histogram_readback)),
            ("Output textures", textures.to_owned()),
            ("Resident textures", format_bytes(self.resident_bytes)),
        ]
    }
}

/// User-visible failures and fallback context safe to include in a support
/// report. Source paths and image data intentionally never appear here.
#[derive(Debug, Clone, Default, Serialize)]
pub struct DiagnosticsMessages {
    pub processor_note: Option<String>,
    pub startup_error: Option<String>,
    pub warning: Option<String>,
    pub error: Option<String>,
}

impl DiagnosticsMessages {
    /// The messages that are set, each with its label.
    pub fn labelled(&self) -> impl Iterator<Item = (&'static str, &str)> {
        [
            ("Processor", self.processor_note.as_deref()),
            ("Startup", self.startup_error.as_deref()),
            ("Warning", self.warning.as_deref()),
            ("Error", self.error.as_deref()),
        ]
        .into_iter()
        .filter_map(|(label, message)| Some((label, message?)))
    }
}

/// `250 µs` below a millisecond, `2.00 ms` from there.
fn format_duration(duration: Duration) -> String {
    if duration >= Duration::from_millis(1) {
        format!("{:.2} ms", duration.as_secs_f64() * 1_000.0)
    } else {
        format!("{} µs", duration.as_micros())
    }
}

fn format_bytes(bytes: usize) -> String {
    const MEBIBYTE: f64 = 1_048_576.0;
    format!("{:.1} MiB", bytes as f64 / MEBIBYTE)
}

/// Whether a cached stage was reused.
const fn cache_result(hit: bool) -> &'static str {
    if hit { "hit" } else { "miss" }
}

fn duration_milliseconds(duration: Duration) -> f64 {
    duration.as_secs_f64() * 1_000.0
}

fn milliseconds<S: Serializer>(duration: &Duration, serializer: S) -> Result<S::Ok, S::Error> {
    serializer.serialize_f64(duration_milliseconds(*duration))
}

fn optional_milliseconds<S: Serializer>(
    duration: &Option<Duration>,
    serializer: S,
) -> Result<S::Ok, S::Error> {
    duration.map(duration_milliseconds).serialize(serializer)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn diagnostic_units_stay_compact_and_explicit() {
        assert_eq!(format_duration(Duration::from_micros(250)), "250 µs");
        assert_eq!(format_duration(Duration::from_millis(2)), "2.00 ms");
        assert_eq!(format_bytes(3 * 1_048_576), "3.0 MiB");
    }

    #[test]
    fn only_set_messages_are_listed() {
        let messages = DiagnosticsMessages {
            warning: Some("slow".to_owned()),
            ..DiagnosticsMessages::default()
        };
        assert_eq!(
            messages.labelled().collect::<Vec<_>>(),
            [("Warning", "slow")]
        );
    }
}
