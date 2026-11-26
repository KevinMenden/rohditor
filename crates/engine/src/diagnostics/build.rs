//! Gathering a [`Diagnostics`] snapshot from the runtime state of a frontend.

use rohditor_core::{
    CameraCalibration, GamutMappingDiagnostics, HighlightDiagnostics, OutputPolicy,
    resolve_camera_colour,
};
use rohditor_demosaic::DemosaicAlgorithm;
use rohditor_edit::RenderingProfileSelection;

use super::{
    CacheDiagnostics, CatalogDiagnostics, Diagnostics, DiagnosticsMessages, GpuDiagnostics,
    HighlightDiagnosticsSummary, PreviewDiagnostics, TimingDiagnostics,
};
use crate::catalog::CatalogState;
use crate::coordinator::{PreviewBackend, PreviewQueueStats, PreviewResolution};
use crate::open_document::OpenDocument;
use crate::preview::{DocumentPreviewDiagnostics, PreviewRuntime};

/// What a frontend holds that the diagnostics describe.
pub struct DiagnosticsSources<'a> {
    /// The frontend's renderer, for example "GPUI".
    pub ui_renderer: &'a str,
    pub runtime: &'a PreviewRuntime,
    /// From [`crate::coordinator::RenderCoordinator::preview_queue_stats`].
    pub queue: PreviewQueueStats,
    pub document: Option<&'a OpenDocument>,
    pub catalog: Option<&'a CatalogState>,
    /// The output gamut policy of the app settings.
    pub output_policy: OutputPolicy,
}

pub fn build(sources: &DiagnosticsSources<'_>) -> Diagnostics {
    let document = sources.document;
    let gpu_device = sources.runtime.gpu().map(|gpu| {
        let capabilities = gpu.processor().capabilities();
        format!(
            "{} · {} · timestamp queries: {}",
            capabilities.adapter_name, capabilities.backend, capabilities.timestamp_queries
        )
    });
    let catalog = sources
        .catalog
        .filter(|catalog| catalog.entry_count() > 0 || catalog.failure().is_some())
        .map(|catalog| CatalogDiagnostics {
            photos: catalog.entry_count(),
            thumbnails_ready: catalog.ready_count(),
            thumbnails_placeholder: catalog.placeholder_count(),
            thumbnails_pending: catalog.pending_count(),
            thumbnails_failed: catalog.failure_count(),
            resident_bytes: catalog.resident_thumbnail_bytes(),
        });
    let preview = document.and_then(|document| {
        let diagnostics = document.preview.diagnostics?;
        Some(preview(document, &diagnostics, sources.output_policy))
    });
    Diagnostics {
        processor: processor_description(sources.runtime, document),
        ui_renderer: sources.ui_renderer.to_owned(),
        gpu_device,
        queue: sources.queue,
        catalog,
        preview,
        messages: DiagnosticsMessages {
            processor_note: sources.runtime.note().map(str::to_owned),
            startup_error: sources.runtime.startup_error().map(str::to_owned),
            warning: document.and_then(|document| document.warning.clone()),
            error: document.and_then(|document| document.error.clone()),
        },
    }
}

/// Which processor renders previews, and where the last one of `document` ran.
pub fn processor_description(runtime: &PreviewRuntime, document: Option<&OpenDocument>) -> String {
    let Some(gpu) = runtime.gpu() else {
        return format!("CPU ({})", runtime.preference().label());
    };
    let capabilities = gpu.processor().capabilities();
    let hardware = format!("{} · {}", capabilities.adapter_name, capabilities.backend);
    let last = document.and_then(|document| document.preview.diagnostics);
    match last.map(|diagnostics| diagnostics.worker) {
        Some(worker) if worker.capture_cpu_recovery => {
            format!("CPU sensor recovery · GPU spatial/color · {hardware}")
        }
        Some(worker) if worker.backend == PreviewBackend::Cpu => {
            format!("CPU fallback · GPU available · {hardware}")
        }
        Some(worker) if worker.sensor_gpu => format!("GPU sensor/spatial/color · {hardware}"),
        Some(_) => format!("CPU sensor · GPU spatial/color · {hardware}"),
        None => format!("GPU available · {hardware}"),
    }
}

fn preview(
    document: &OpenDocument,
    preview: &DocumentPreviewDiagnostics,
    output_policy: OutputPolicy,
) -> PreviewDiagnostics {
    let worker = &preview.worker;
    let detail = worker.detail;
    let backend = match worker.backend {
        PreviewBackend::GpuBase if worker.sensor_gpu => "GPU sensor/spatial/color",
        PreviewBackend::GpuBase => "CPU sensor / GPU spatial/color",
        backend => backend.label(),
    };
    let source_state = match (worker.resolution, worker.algorithm) {
        (PreviewResolution::SourceScale, _) => "1:1",
        (PreviewResolution::Fit, DemosaicAlgorithm::Bilinear) => "fast",
        (PreviewResolution::Fit, DemosaicAlgorithm::Neural) => "neural",
        (PreviewResolution::Fit, _) => "high-quality",
    };
    let off = || "Off".to_owned();
    let timings = &worker.timings;
    PreviewDiagnostics {
        backend: backend.to_owned(),
        algorithm: worker.algorithm.stable_name().to_owned(),
        profile: camera_profile(document),
        false_color: detail
            .and_then(|p| p.false_color.map(|f| (p, f)))
            .map_or_else(off, |(p, f)| {
                format!("v{}, {} steps", p.false_color_version, f.steps)
            }),
        noise_reduction: detail
            .and_then(|p| p.noise_reduction.map(|n| (p, n)))
            .map_or_else(off, |(p, n)| {
                format!(
                    "v{}, chroma {:.2}, luminance {:.2} (detail {:.2})",
                    p.noise_reduction_version, n.chroma, n.luminance, n.luminance_detail
                )
            }),
        capture_sharpening: detail
            .and_then(|p| p.capture_sharpening.map(|s| (p, s)))
            .map_or_else(off, |(p, s)| {
                format!(
                    "RL v{}, amount {:.2}, sigma {:.2} px, noise protection {:.2}",
                    p.capture_sharpening_version, s.amount, s.radius, s.noise_protection
                )
            }),
        rendering: rendering_profile(document.edits.recipe().rendering.profile),
        output_policy: versioned_policy(output_policy),
        output_gamut_statistics: worker.output_gamut_diagnostics.map(gamut_statistics),
        source_state: source_state.to_owned(),
        cache: CacheDiagnostics {
            decoded: worker.cache_hits.decoded,
            reconstructed: worker.cache_hits.reconstructed,
            demosaiced: worker.cache_hits.demosaiced,
            adjusted: worker.cache_hits.adjusted,
            workspace_reused: worker.workspace_reused,
        },
        timings: TimingDiagnostics {
            metadata: timings.metadata,
            normalization: timings.normalization,
            highlight_processing: timings.highlight_processing,
            demosaic: timings.demosaic,
            false_color: timings.false_color,
            chroma_noise_reduction: timings.chroma_noise_reduction,
            luminance_noise_reduction: timings.luminance_noise_reduction,
            capture_sharpening: timings.capture_sharpening,
            optics: timings.optics,
            defringe: timings.defringe,
            resampling: timings.resampling,
            color_conversion: timings.color_conversion,
            adjustments: timings.adjustments,
            output_conversion: timings.output_conversion,
            total: timings.total,
        },
        highlight: highlight(worker.highlight_diagnostics),
        cache_resident_bytes: worker.cache_resident_bytes,
        estimated_peak_bytes: worker.memory.estimated_peak_bytes,
        gpu: preview.gpu_submission.map(|submission| GpuDiagnostics {
            upload_preparation: preview.gpu_upload_preparation,
            submission: Some(submission),
            queue_completion: preview.gpu_queue_completion,
            histogram_readback: preview.gpu_histogram_readback,
            textures_reused: preview.gpu_textures_reused,
            resident_bytes: preview.gpu_resident_bytes,
        }),
    }
}

fn highlight(diagnostics: HighlightDiagnostics) -> HighlightDiagnosticsSummary {
    match diagnostics {
        HighlightDiagnostics::Off => HighlightDiagnosticsSummary::Off,
        HighlightDiagnostics::Clip(stats) => HighlightDiagnosticsSummary::Clip {
            affected_sites: stats.affected_sites,
            changed_sites: stats.changed_sites,
            nominal_over_white_sites: stats.nominal_over_white_sites,
            affected_by_channel: stats.affected_by_channel,
        },
        HighlightDiagnostics::Opposed(stats) => HighlightDiagnosticsSummary::Opposed {
            suspected_clipped_sites: stats.suspected_clipped_sites,
            reconstructed_sites: stats.reconstructed_sites,
            changed_sites: stats.changed_sites,
            fallback_sites: stats.fallback_sites,
            fully_unsupported_sites: stats.fully_unsupported_sites,
            neutralised_sites: stats.neutralised_sites,
            suspected_by_channel: stats.suspected_by_channel,
        },
    }
}

/// Where the camera colour of the open photo comes from.
fn camera_profile(document: &OpenDocument) -> String {
    let Some(info) = document.info.as_ref() else {
        return "unavailable (RAW metadata pending)".to_owned();
    };
    let calibration = CameraCalibration::from_raw_info(info);
    let color = &document.edits.recipe().color;
    resolve_camera_colour(&calibration, &color.camera_profile, color.white_balance)
        .map(|resolved| resolved.provenance.to_string())
        .unwrap_or_else(|error| format!("unavailable ({error})"))
}

fn rendering_profile(profile: RenderingProfileSelection) -> String {
    match profile.process_version() {
        Some(version) => format!("{} v{version}", profile.display_name()),
        None => profile.display_name().to_owned(),
    }
}

fn versioned_policy(policy: OutputPolicy) -> String {
    match policy.algorithm_version() {
        Some(version) => format!("{} v{version}", policy.display_name()),
        None => policy.display_name().to_owned(),
    }
}

fn gamut_statistics(diagnostics: GamutMappingDiagnostics) -> String {
    format!(
        "{} in gamut, {} compressed, {} limited, {} clipped fallback, {} invalid",
        diagnostics.in_gamut_pixels,
        diagnostics.compressed_pixels,
        diagnostics.limited_pixels,
        diagnostics.clipped_fallback_pixels,
        diagnostics.invalid_pixels,
    )
}
