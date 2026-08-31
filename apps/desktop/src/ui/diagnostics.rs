use std::time::Duration;

use eframe::egui;
use serde::Serialize;

#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct QueueModel {
    pub requested: u64,
    pub coalesced: u64,
    pub cancellation_requests: u64,
    pub cancelled: u64,
    pub completed: u64,
    pub failed: u64,
    pub active: bool,
    pub pending: bool,
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct CacheModel {
    pub decoded: bool,
    pub normalized: bool,
    pub demosaiced: bool,
    pub adjusted: bool,
    pub workspace_reused: bool,
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct TimingModel {
    pub metadata: Duration,
    pub normalization: Duration,
    pub demosaic: Duration,
    pub color_conversion: Duration,
    pub adjustments: Duration,
    pub output_conversion: Duration,
    pub total: Duration,
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct GpuModel {
    pub upload_preparation: Option<Duration>,
    pub submission: Option<Duration>,
    pub queue_completion: Option<Duration>,
    pub textures_reused: Option<bool>,
    pub resident_bytes: usize,
}

#[derive(Debug, Clone)]
pub(crate) struct PreviewModel {
    pub backend: String,
    pub cache: CacheModel,
    pub timings: TimingModel,
    pub cache_resident_bytes: usize,
    pub estimated_peak_bytes: usize,
    pub gpu: Option<GpuModel>,
}

#[derive(Debug, Clone)]
pub(crate) struct DiagnosticsModel {
    pub processor: String,
    pub ui_renderer: String,
    pub gpu_device: Option<String>,
    pub queue: QueueModel,
    pub preview: Option<PreviewModel>,
    pub messages: DiagnosticsMessages,
}

/// User-visible failures and fallback context safe to include in a support
/// report. Source paths and image data intentionally never appear here.
#[derive(Debug, Clone, Default)]
pub(crate) struct DiagnosticsMessages {
    pub processor_note: Option<String>,
    pub startup_error: Option<String>,
    pub warning: Option<String>,
    pub error: Option<String>,
}

#[derive(Debug, Default)]
pub(crate) struct DiagnosticsOutput {
    pub export_requested: bool,
}

pub(crate) fn show(
    context: &egui::Context,
    open: &mut bool,
    model: &DiagnosticsModel,
) -> DiagnosticsOutput {
    let mut output = DiagnosticsOutput::default();
    egui::Window::new("Developer diagnostics")
        .open(open)
        .default_width(390.0)
        .resizable(true)
        .show(context, |ui| {
            ui.horizontal(|ui| {
                ui.label(format!("Processor: {}", model.processor));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    output.export_requested = ui.button("Save report…").clicked();
                });
            });
            ui.weak(format!("{} UI", model.ui_renderer));
            if let Some(device) = &model.gpu_device {
                ui.weak(device);
            }

            show_messages(ui, &model.messages);

            ui.add_space(6.0);
            ui.strong("Preview queue");
            egui::Grid::new("preview_queue_diagnostics")
                .num_columns(2)
                .show(ui, |ui| {
                    diagnostic_row(ui, "Requested", model.queue.requested);
                    diagnostic_row(ui, "Coalesced", model.queue.coalesced);
                    diagnostic_row(
                        ui,
                        "Cancel requests",
                        model.queue.cancellation_requests,
                    );
                    diagnostic_row(ui, "Cancelled", model.queue.cancelled);
                    diagnostic_row(ui, "Completed", model.queue.completed);
                    diagnostic_row(ui, "Failed", model.queue.failed);
                    diagnostic_row(ui, "Active", model.queue.active);
                    diagnostic_row(ui, "Pending", model.queue.pending);
                });

            let Some(preview) = &model.preview else {
                ui.add_space(6.0);
                ui.weak("No developed preview diagnostics yet.");
                return;
            };
            ui.add_space(8.0);
            ui.strong(format!("Last {} preview", preview.backend));
            egui::Grid::new("preview_cache_diagnostics")
                .num_columns(2)
                .show(ui, |ui| {
                    diagnostic_row(ui, "DecodedRaw", cache_result(preview.cache.decoded));
                    diagnostic_row(
                        ui,
                        "NormalizedMosaic",
                        cache_result(preview.cache.normalized),
                    );
                    diagnostic_row(
                        ui,
                        "DemosaicedBase",
                        cache_result(preview.cache.demosaiced),
                    );
                    diagnostic_row(
                        ui,
                        "AdjustedPreview",
                        cache_result(preview.cache.adjusted),
                    );
                    diagnostic_row(
                        ui,
                        "CPU working buffer",
                        if preview.cache.workspace_reused {
                            "reused"
                        } else {
                            "allocated or unused"
                        },
                    );
                });

            ui.add_space(8.0);
            ui.strong("CPU stage wall times");
            egui::Grid::new("preview_stage_diagnostics")
                .num_columns(2)
                .show(ui, |ui| {
                    duration_row(ui, "Metadata", preview.timings.metadata);
                    duration_row(ui, "Normalization", preview.timings.normalization);
                    duration_row(ui, "Demosaic", preview.timings.demosaic);
                    duration_row(ui, "Color conversion", preview.timings.color_conversion);
                    duration_row(ui, "Adjustments", preview.timings.adjustments);
                    duration_row(ui, "Output conversion", preview.timings.output_conversion);
                    duration_row(ui, "Total", preview.timings.total);
                });
            ui.label(format!(
                "CPU cache: {} · estimated render peak: {}",
                format_bytes(preview.cache_resident_bytes),
                format_bytes(preview.estimated_peak_bytes)
            ));

            if let Some(gpu) = preview.gpu {
                ui.add_space(8.0);
                ui.strong("GPU preview");
                egui::Grid::new("gpu_preview_diagnostics")
                    .num_columns(2)
                    .show(ui, |ui| {
                        optional_duration_row(ui, "CPU upload packing", gpu.upload_preparation);
                        optional_duration_row(ui, "Encode + submit", gpu.submission);
                        optional_duration_row(ui, "Queue completion", gpu.queue_completion);
                        diagnostic_row(
                            ui,
                            "Output textures",
                            match gpu.textures_reused {
                                Some(true) => "reused",
                                Some(false) => "allocated",
                                None => "n/a",
                            },
                        );
                        diagnostic_row(
                            ui,
                            "Resident textures",
                            format_bytes(gpu.resident_bytes),
                        );
                    });
                ui.weak(
                    "Queue completion is conservative wall latency; it includes shared-queue delay when timestamp queries are unavailable.",
                );
            }
        });
    output
}

/// Serialize a support-safe snapshot. All duration fields use milliseconds so
/// the JSON is portable and readable without Rust-specific encodings.
pub(crate) fn report(model: &DiagnosticsModel) -> DiagnosticsReport<'_> {
    DiagnosticsReport {
        format_version: 1,
        application: ApplicationReport {
            name: "Rohditor",
            version: env!("CARGO_PKG_VERSION"),
            target_os: std::env::consts::OS,
            target_arch: std::env::consts::ARCH,
            ui_renderer: &model.ui_renderer,
            processor: &model.processor,
        },
        gpu_device: model.gpu_device.as_deref(),
        queue: QueueReport::from(model.queue),
        preview: model.preview.as_ref().map(PreviewReport::from),
        messages: MessageReport::from(&model.messages),
    }
}

#[derive(Debug, Serialize)]
pub(crate) struct DiagnosticsReport<'a> {
    pub format_version: u8,
    pub application: ApplicationReport<'a>,
    pub gpu_device: Option<&'a str>,
    pub queue: QueueReport,
    pub preview: Option<PreviewReport<'a>>,
    pub messages: MessageReport<'a>,
}

#[derive(Debug, Serialize)]
pub(crate) struct ApplicationReport<'a> {
    pub name: &'static str,
    pub version: &'static str,
    pub target_os: &'static str,
    pub target_arch: &'static str,
    pub ui_renderer: &'a str,
    pub processor: &'a str,
}

#[derive(Debug, Serialize)]
pub(crate) struct QueueReport {
    pub requested: u64,
    pub coalesced: u64,
    pub cancellation_requests: u64,
    pub cancelled: u64,
    pub completed: u64,
    pub failed: u64,
    pub active: bool,
    pub pending: bool,
}

impl From<QueueModel> for QueueReport {
    fn from(value: QueueModel) -> Self {
        Self {
            requested: value.requested,
            coalesced: value.coalesced,
            cancellation_requests: value.cancellation_requests,
            cancelled: value.cancelled,
            completed: value.completed,
            failed: value.failed,
            active: value.active,
            pending: value.pending,
        }
    }
}

#[derive(Debug, Serialize)]
pub(crate) struct PreviewReport<'a> {
    pub backend: &'a str,
    pub cache: CacheReport,
    pub timings_ms: TimingReport,
    pub cache_resident_bytes: usize,
    pub estimated_peak_bytes: usize,
    pub gpu: Option<GpuReport>,
}

impl<'a> From<&'a PreviewModel> for PreviewReport<'a> {
    fn from(value: &'a PreviewModel) -> Self {
        Self {
            backend: &value.backend,
            cache: CacheReport::from(value.cache),
            timings_ms: TimingReport::from(value.timings),
            cache_resident_bytes: value.cache_resident_bytes,
            estimated_peak_bytes: value.estimated_peak_bytes,
            gpu: value.gpu.map(GpuReport::from),
        }
    }
}

#[derive(Debug, Serialize)]
pub(crate) struct CacheReport {
    pub decoded: bool,
    pub normalized: bool,
    pub demosaiced: bool,
    pub adjusted: bool,
    pub workspace_reused: bool,
}

impl From<CacheModel> for CacheReport {
    fn from(value: CacheModel) -> Self {
        Self {
            decoded: value.decoded,
            normalized: value.normalized,
            demosaiced: value.demosaiced,
            adjusted: value.adjusted,
            workspace_reused: value.workspace_reused,
        }
    }
}

#[derive(Debug, Serialize)]
pub(crate) struct TimingReport {
    pub metadata: f64,
    pub normalization: f64,
    pub demosaic: f64,
    pub color_conversion: f64,
    pub adjustments: f64,
    pub output_conversion: f64,
    pub total: f64,
}

impl From<TimingModel> for TimingReport {
    fn from(value: TimingModel) -> Self {
        Self {
            metadata: duration_milliseconds(value.metadata),
            normalization: duration_milliseconds(value.normalization),
            demosaic: duration_milliseconds(value.demosaic),
            color_conversion: duration_milliseconds(value.color_conversion),
            adjustments: duration_milliseconds(value.adjustments),
            output_conversion: duration_milliseconds(value.output_conversion),
            total: duration_milliseconds(value.total),
        }
    }
}

#[derive(Debug, Serialize)]
pub(crate) struct GpuReport {
    pub upload_preparation_ms: Option<f64>,
    pub submission_ms: Option<f64>,
    pub queue_completion_ms: Option<f64>,
    pub textures_reused: Option<bool>,
    pub resident_bytes: usize,
}

impl From<GpuModel> for GpuReport {
    fn from(value: GpuModel) -> Self {
        Self {
            upload_preparation_ms: value.upload_preparation.map(duration_milliseconds),
            submission_ms: value.submission.map(duration_milliseconds),
            queue_completion_ms: value.queue_completion.map(duration_milliseconds),
            textures_reused: value.textures_reused,
            resident_bytes: value.resident_bytes,
        }
    }
}

#[derive(Debug, Serialize)]
pub(crate) struct MessageReport<'a> {
    pub processor_note: Option<&'a str>,
    pub startup_error: Option<&'a str>,
    pub warning: Option<&'a str>,
    pub error: Option<&'a str>,
}

impl<'a> From<&'a DiagnosticsMessages> for MessageReport<'a> {
    fn from(value: &'a DiagnosticsMessages) -> Self {
        Self {
            processor_note: value.processor_note.as_deref(),
            startup_error: value.startup_error.as_deref(),
            warning: value.warning.as_deref(),
            error: value.error.as_deref(),
        }
    }
}

fn show_messages(ui: &mut egui::Ui, messages: &DiagnosticsMessages) {
    for (label, message) in [
        ("Processor", messages.processor_note.as_deref()),
        ("Startup", messages.startup_error.as_deref()),
        ("Warning", messages.warning.as_deref()),
        ("Error", messages.error.as_deref()),
    ] {
        if let Some(message) = message {
            ui.colored_label(
                egui::Color32::from_rgb(220, 164, 74),
                format!("{label}: {message}"),
            );
        }
    }
}

fn duration_milliseconds(duration: Duration) -> f64 {
    duration.as_secs_f64() * 1_000.0
}

fn diagnostic_row(ui: &mut egui::Ui, label: &str, value: impl std::fmt::Display) {
    ui.label(label);
    ui.monospace(value.to_string());
    ui.end_row();
}

fn duration_row(ui: &mut egui::Ui, label: &str, duration: Duration) {
    diagnostic_row(ui, label, format_duration(duration));
}

fn optional_duration_row(ui: &mut egui::Ui, label: &str, duration: Option<Duration>) {
    diagnostic_row(
        ui,
        label,
        duration.map_or_else(|| "pending".to_owned(), format_duration),
    );
}

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

const fn cache_result(hit: bool) -> &'static str {
    if hit { "hit" } else { "miss" }
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
    fn support_report_omits_file_paths_and_image_contents() {
        let model = DiagnosticsModel {
            processor: "CPU (CPU)".to_owned(),
            ui_renderer: "glow".to_owned(),
            gpu_device: None,
            queue: QueueModel::default(),
            preview: None,
            messages: DiagnosticsMessages {
                error: Some("decoder rejected malformed input".to_owned()),
                ..DiagnosticsMessages::default()
            },
        };

        let json = serde_json::to_string(&report(&model)).expect("diagnostics serialize");
        assert!(json.contains("decoder rejected malformed input"));
        assert!(!json.contains("testdata"));
        assert!(!json.contains("pixels"));
        assert!(!json.contains("path"));
    }
}
