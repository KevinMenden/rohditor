//! The support-safe JSON report of a [`Diagnostics`] snapshot.
//!
//! Durations are milliseconds, so the JSON is portable and readable without
//! Rust-specific encodings.

use std::path::Path;

use serde::Serialize;

use super::{CatalogDiagnostics, Diagnostics, DiagnosticsMessages, PreviewDiagnostics};
use crate::coordinator::PreviewQueueStats;
use crate::storage;

pub const REPORT_FORMAT_VERSION: u8 = 5;

#[derive(Serialize)]
struct Report<'a> {
    format_version: u8,
    application: Application<'a>,
    gpu_device: Option<&'a str>,
    queue: &'a PreviewQueueStats,
    catalog: Option<&'a CatalogDiagnostics>,
    preview: Option<&'a PreviewDiagnostics>,
    messages: &'a DiagnosticsMessages,
}

#[derive(Serialize)]
struct Application<'a> {
    name: &'static str,
    version: &'static str,
    target_os: &'static str,
    target_arch: &'static str,
    ui_renderer: &'a str,
    processor: &'a str,
}

/// The report as pretty-printed JSON.
pub fn report_json(diagnostics: &Diagnostics) -> Result<Vec<u8>, String> {
    let report = Report {
        format_version: REPORT_FORMAT_VERSION,
        application: Application {
            name: "Rohditor",
            version: env!("CARGO_PKG_VERSION"),
            target_os: std::env::consts::OS,
            target_arch: std::env::consts::ARCH,
            ui_renderer: &diagnostics.ui_renderer,
            processor: &diagnostics.processor,
        },
        gpu_device: diagnostics.gpu_device.as_deref(),
        queue: &diagnostics.queue,
        catalog: diagnostics.catalog.as_ref(),
        preview: diagnostics.preview.as_ref(),
        messages: &diagnostics.messages,
    };
    let mut bytes = serde_json::to_vec_pretty(&report)
        .map_err(|error| format!("Could not encode the diagnostics report: {error}"))?;
    bytes.push(b'\n');
    Ok(bytes)
}

/// Write the report to `destination`, replacing it whole or not at all.
pub fn save_report(diagnostics: &Diagnostics, destination: &Path) -> Result<(), String> {
    let bytes = report_json(diagnostics)?;
    storage::write_transactionally(destination, &bytes).map_err(|error| {
        format!(
            "Could not save the diagnostics report {}: {error}",
            destination.display()
        )
    })
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;
    use crate::diagnostics::{
        CacheDiagnostics, GpuDiagnostics, HighlightDiagnosticsSummary, TimingDiagnostics,
    };

    fn diagnostics() -> Diagnostics {
        Diagnostics {
            processor: "CPU (CPU)".to_owned(),
            ui_renderer: "GPUI".to_owned(),
            gpu_device: None,
            queue: PreviewQueueStats::default(),
            catalog: None,
            preview: None,
            messages: DiagnosticsMessages {
                error: Some("decoder rejected malformed input".to_owned()),
                ..DiagnosticsMessages::default()
            },
        }
    }

    #[test]
    fn support_report_omits_file_paths_and_image_contents() {
        let json = String::from_utf8(report_json(&diagnostics()).expect("encodes")).expect("utf-8");
        assert!(json.contains("decoder rejected malformed input"));
        assert!(json.contains("\"ui_renderer\": \"GPUI\""));
        assert!(!json.contains("testdata"));
        assert!(!json.contains("pixels"));
        assert!(!json.contains("path"));
    }

    #[test]
    fn durations_are_milliseconds_and_highlights_are_tagged() {
        let millis = Duration::from_micros(1_500);
        let mut diagnostics = diagnostics();
        diagnostics.preview = Some(PreviewDiagnostics {
            backend: "CPU".to_owned(),
            algorithm: "amaze".to_owned(),
            profile: String::new(),
            false_color: "Off".to_owned(),
            noise_reduction: "Off".to_owned(),
            capture_sharpening: "Off".to_owned(),
            rendering: String::new(),
            output_policy: String::new(),
            output_gamut_statistics: None,
            source_state: "1:1".to_owned(),
            cache: CacheDiagnostics {
                decoded: true,
                reconstructed: false,
                demosaiced: false,
                adjusted: false,
                workspace_reused: false,
            },
            timings: TimingDiagnostics {
                metadata: millis,
                normalization: millis,
                highlight_processing: millis,
                demosaic: Duration::from_millis(40),
                false_color: millis,
                chroma_noise_reduction: millis,
                luminance_noise_reduction: millis,
                capture_sharpening: millis,
                optics: millis,
                defringe: millis,
                resampling: millis,
                color_conversion: millis,
                adjustments: millis,
                output_conversion: millis,
                total: millis,
            },
            highlight: HighlightDiagnosticsSummary::Clip {
                affected_sites: 3,
                changed_sites: 2,
                nominal_over_white_sites: 1,
                affected_by_channel: [1, 1, 1],
            },
            cache_resident_bytes: 0,
            estimated_peak_bytes: 0,
            gpu: Some(GpuDiagnostics {
                upload_preparation: None,
                submission: Some(Duration::from_millis(2)),
                queue_completion: None,
                histogram_readback: None,
                textures_reused: Some(true),
                resident_bytes: 0,
            }),
        });
        let json: serde_json::Value =
            serde_json::from_slice(&report_json(&diagnostics).expect("encodes")).expect("JSON");
        let preview = &json["preview"];
        assert_eq!(preview["timings_ms"]["demosaic"], 40.0);
        assert_eq!(preview["timings_ms"]["metadata"], 1.5);
        assert_eq!(preview["highlight"]["method"], "clip");
        assert_eq!(preview["highlight"]["changed_sites"], 2);
        assert_eq!(preview["gpu"]["submission_ms"], 2.0);
        assert!(preview["gpu"]["queue_completion_ms"].is_null());
        assert_eq!(json["format_version"], REPORT_FORMAT_VERSION);
    }

    #[test]
    fn saving_replaces_the_report_whole() {
        let directory = std::env::temp_dir().join(format!(
            "rohditor-diagnostics-report-{}",
            std::process::id()
        ));
        let path = directory.join("report.json");
        save_report(&diagnostics(), &path).expect("first save");
        save_report(&diagnostics(), &path).expect("second save replaces it");
        let saved: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&path).expect("read")).expect("JSON");
        assert_eq!(saved["application"]["name"], "Rohditor");
        drop(std::fs::remove_dir_all(&directory));
    }
}
