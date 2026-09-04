mod app;
mod catalog;
mod coordinator;
mod document;
mod preview_cache;
mod session;
mod ui;

use clap::{Parser, ValueEnum};
use eframe::egui;
use rohditor_demosaic::DemosaicAlgorithm;
use std::path::PathBuf;
use tracing::warn;
use tracing_subscriber::EnvFilter;

use crate::app::RohditorApp;

/// Stable Freedesktop/Wayland identity. Keep this equal to the installed
/// desktop-file basename and icon name.
const APPLICATION_ID: &str = "io.github.kevin.rohditor";

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, ValueEnum)]
enum RendererPreference {
    /// Prefer wgpu and retry with glow if initialization fails.
    #[default]
    Auto,
    /// Require the wgpu UI renderer.
    Wgpu,
    /// Use the OpenGL glow UI fallback. Image processing remains on the CPU.
    Glow,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, ValueEnum)]
pub(crate) enum ProcessorPreference {
    /// Prefer GPU previews with automatic CPU fallback if they are unavailable.
    #[default]
    Auto,
    /// Require the shared wgpu device for interactive preview processing.
    Gpu,
    /// Use only the deterministic CPU preview processor.
    Cpu,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, ValueEnum)]
enum DemosaicPreference {
    Bilinear,
    #[value(name = "mhc")]
    #[default]
    MalvarHeCutler,
    #[value(name = "rcd")]
    Rcd,
    #[value(name = "amaze")]
    Amaze,
}

impl From<DemosaicPreference> for DemosaicAlgorithm {
    fn from(value: DemosaicPreference) -> Self {
        match value {
            DemosaicPreference::Bilinear => Self::Bilinear,
            DemosaicPreference::MalvarHeCutler => Self::MalvarHeCutler,
            DemosaicPreference::Rcd => Self::Rcd,
            DemosaicPreference::Amaze => Self::Amaze,
        }
    }
}

impl ProcessorPreference {
    pub(crate) const fn label(self) -> &'static str {
        match self {
            Self::Auto => "Auto",
            Self::Gpu => "GPU",
            Self::Cpu => "CPU",
        }
    }
}

#[derive(Debug, Parser)]
#[command(version, about = "Linux-first Sony RAW photo editor")]
struct Arguments {
    /// UI renderer. The wgpu renderer is required for GPU preview processing.
    #[arg(long, value_enum, default_value_t)]
    renderer: RendererPreference,

    /// Interactive preview processor. Full-resolution exports remain CPU-only.
    #[arg(long, value_enum, default_value_t)]
    processor: ProcessorPreference,

    /// Preview and export demosaic algorithm for development comparisons.
    #[arg(long, value_enum, default_value_t)]
    demosaic: DemosaicPreference,

    /// Sony RAW file to open at startup.
    file: Option<PathBuf>,

    /// Open the developer diagnostics window at startup.
    #[arg(long)]
    diagnostics: bool,
}

fn main() -> eframe::Result {
    init_tracing();
    let arguments = Arguments::parse();
    match arguments.renderer {
        RendererPreference::Auto => match launch(
            eframe::Renderer::Wgpu,
            arguments.file.clone(),
            arguments.processor,
            arguments.demosaic.into(),
            arguments.diagnostics,
        ) {
            Ok(()) => Ok(()),
            Err(error) => {
                warn!(%error, "wgpu UI initialization failed; retrying with glow");
                eprintln!("wgpu UI initialization failed ({error}); retrying with glow");
                launch(
                    eframe::Renderer::Glow,
                    arguments.file,
                    arguments.processor,
                    arguments.demosaic.into(),
                    arguments.diagnostics,
                )
            }
        },
        RendererPreference::Wgpu => launch(
            eframe::Renderer::Wgpu,
            arguments.file,
            arguments.processor,
            arguments.demosaic.into(),
            arguments.diagnostics,
        ),
        RendererPreference::Glow => launch(
            eframe::Renderer::Glow,
            arguments.file,
            arguments.processor,
            arguments.demosaic.into(),
            arguments.diagnostics,
        ),
    }
}

fn launch(
    renderer: eframe::Renderer,
    initial_path: Option<PathBuf>,
    processor: ProcessorPreference,
    demosaic: DemosaicAlgorithm,
    show_diagnostics: bool,
) -> eframe::Result {
    let mut options = eframe::NativeOptions {
        renderer,
        viewport: egui::ViewportBuilder::default()
            .with_title("Rohditor")
            .with_app_id(APPLICATION_ID)
            .with_inner_size([1_600.0, 1_000.0])
            .with_maximized(true)
            .with_min_inner_size([900.0, 600.0]),
        ..eframe::NativeOptions::default()
    };
    configure_glow_fallback_event_loop(&mut options, renderer);
    eframe::run_native(
        "rohditor",
        options,
        Box::new(move |context| {
            Ok(Box::new(RohditorApp::new(
                context,
                initial_path.clone(),
                processor,
                demosaic,
                show_diagnostics,
            )?))
        }),
    )
}

/// eframe's glow event loop did not reliably wake for worker repaint requests
/// on the reference Plasma Wayland session. The normal renderer is native
/// Wayland wgpu; make the legacy glow fallback use XWayland when it is already
/// available so CPU fallback stays responsive.
#[cfg(target_os = "linux")]
fn configure_glow_fallback_event_loop(
    options: &mut eframe::NativeOptions,
    renderer: eframe::Renderer,
) {
    if matches!(renderer, eframe::Renderer::Glow) && std::env::var_os("DISPLAY").is_some() {
        use winit::platform::x11::EventLoopBuilderExtX11 as _;

        options.event_loop_builder = Some(Box::new(|builder| {
            let _ = builder.with_x11();
        }));
    }
}

#[cfg(not(target_os = "linux"))]
fn configure_glow_fallback_event_loop(
    _options: &mut eframe::NativeOptions,
    _renderer: eframe::Renderer,
) {
}

fn init_tracing() {
    let filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new("warn,rohditor_desktop=info,rohditor_raw=info"));
    drop(
        tracing_subscriber::fmt()
            .with_env_filter(filter)
            .with_target(false)
            .try_init(),
    );
}

#[cfg(test)]
mod tests {
    use clap::Parser;

    use super::{
        APPLICATION_ID, Arguments, DemosaicAlgorithm, DemosaicPreference, ProcessorPreference,
        RendererPreference,
    };

    #[test]
    fn processor_and_renderer_preferences_are_exposed_as_cli_choices() {
        let arguments = Arguments::try_parse_from([
            "rohditor-desktop",
            "--renderer",
            "glow",
            "--processor",
            "cpu",
            "--demosaic",
            "mhc",
            "--diagnostics",
        ])
        .expect("valid desktop preferences should parse");
        assert_eq!(arguments.renderer, RendererPreference::Glow);
        assert_eq!(arguments.processor, ProcessorPreference::Cpu);
        assert_eq!(arguments.demosaic, DemosaicPreference::MalvarHeCutler);

        let defaults = Arguments::try_parse_from(["rohditor-desktop"])
            .expect("default desktop arguments parse");
        assert_eq!(defaults.demosaic, DemosaicPreference::MalvarHeCutler);
        assert!(arguments.diagnostics);
    }

    #[test]
    fn rcd_is_a_desktop_demosaic_choice() {
        let arguments = Arguments::try_parse_from(["rohditor-desktop", "--demosaic", "rcd"])
            .expect("rcd should be a supported desktop demosaic choice");
        assert_eq!(arguments.demosaic, DemosaicPreference::Rcd);
        assert_eq!(
            DemosaicAlgorithm::from(arguments.demosaic),
            DemosaicAlgorithm::Rcd
        );
    }

    #[test]
    fn amaze_is_a_desktop_demosaic_choice() {
        let arguments = Arguments::try_parse_from(["rohditor-desktop", "--demosaic", "amaze"])
            .expect("amaze should be a supported desktop demosaic choice");
        assert_eq!(arguments.demosaic, DemosaicPreference::Amaze);
        assert_eq!(
            DemosaicAlgorithm::from(arguments.demosaic),
            DemosaicAlgorithm::Amaze
        );
    }

    #[test]
    fn wayland_identity_matches_the_distributed_desktop_file() {
        assert_eq!(APPLICATION_ID, "io.github.kevin.rohditor");
        let desktop_entry = include_str!("../../../assets/io.github.kevin.rohditor.desktop");
        assert!(desktop_entry.contains("Icon=io.github.kevin.rohditor\n"));
        assert!(desktop_entry.contains("StartupWMClass=io.github.kevin.rohditor\n"));
    }
}
