use std::time::Duration;

use eframe::egui;

#[derive(Debug, Clone, Copy)]
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
    pub gpu_device: Option<String>,
    pub queue: QueueModel,
    pub preview: Option<PreviewModel>,
}

pub(crate) fn show(context: &egui::Context, open: &mut bool, model: &DiagnosticsModel) {
    egui::Window::new("Developer diagnostics")
        .open(open)
        .default_width(390.0)
        .resizable(true)
        .show(context, |ui| {
            ui.label(format!("Processor: {}", model.processor));
            if let Some(device) = &model.gpu_device {
                ui.weak(device);
            }

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
}
