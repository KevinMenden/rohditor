mod app;
mod config;
mod ui;

use clap::{Parser, ValueEnum};
use eframe::egui;
use rohditor_demosaic::DemosaicAlgorithm;
use std::path::PathBuf;
use tracing::warn;
use tracing_subscriber::EnvFilter;

use crate::app::RohditorApp;
use rohditor_engine::settings::{self, AppSettings, SettingsLoad, resolve_startup_settings};

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
    /// Prefer GPU previews and export color processing with CPU fallback.
    #[default]
    Auto,
    /// Require GPU previews; prefer GPU export with recovery to CPU if needed.
    Gpu,
    /// Use the CPU reference for previews and exports.
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

    /// Image processor for this run, overriding the persisted "CPU only"
    /// setting. Without it, the setting decides between `auto` and `cpu`.
    #[arg(long, value_enum)]
    processor: Option<ProcessorPreference>,

    /// Preview and export demosaic algorithm for development comparisons.
    #[arg(long, value_enum)]
    demosaic: Option<DemosaicPreference>,

    /// Sony RAW file to open at startup.
    file: Option<PathBuf>,

    /// Open the developer diagnostics window at startup.
    #[arg(long)]
    diagnostics: bool,
}

fn main() -> eframe::Result {
    init_tracing();
    let arguments = Arguments::parse();
    let SettingsLoad {
        settings: persisted_settings,
        warning: settings_warning,
    } = settings::load(config::config_directory().as_deref());
    let processor = resolve_processor(persisted_settings, arguments.processor);
    let effective_settings = resolve_startup_settings(
        persisted_settings,
        arguments.demosaic.map(DemosaicAlgorithm::from),
    );
    match arguments.renderer {
        RendererPreference::Auto => match launch(
            eframe::Renderer::Wgpu,
            arguments.file.clone(),
            processor,
            effective_settings,
            settings_warning.clone(),
            arguments.diagnostics,
        ) {
            Ok(()) => Ok(()),
            Err(error) => {
                warn!(%error, "wgpu UI initialization failed; retrying with glow");
                eprintln!("wgpu UI initialization failed ({error}); retrying with glow");
                launch(
                    eframe::Renderer::Glow,
                    arguments.file,
                    processor,
                    effective_settings,
                    settings_warning,
                    arguments.diagnostics,
                )
            }
        },
        RendererPreference::Wgpu => launch(
            eframe::Renderer::Wgpu,
            arguments.file,
            processor,
            effective_settings,
            settings_warning,
            arguments.diagnostics,
        ),
        RendererPreference::Glow => launch(
            eframe::Renderer::Glow,
            arguments.file,
            processor,
            effective_settings,
            settings_warning,
            arguments.diagnostics,
        ),
    }
}

/// The processor is fixed for the whole run: the "CPU only" setting is read
/// here once, and the command-line flag takes precedence over it.
fn resolve_processor(
    settings: AppSettings,
    command_line_override: Option<ProcessorPreference>,
) -> ProcessorPreference {
    command_line_override.unwrap_or(if settings.cpu_only() {
        ProcessorPreference::Cpu
    } else {
        ProcessorPreference::Auto
    })
}

fn launch(
    renderer: eframe::Renderer,
    initial_path: Option<PathBuf>,
    processor: ProcessorPreference,
    settings: AppSettings,
    settings_warning: Option<String>,
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
                settings,
                settings_warning.clone(),
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
        APPLICATION_ID, AppSettings, Arguments, DemosaicAlgorithm, DemosaicPreference,
        ProcessorPreference, RendererPreference, resolve_processor,
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
        assert_eq!(arguments.processor, Some(ProcessorPreference::Cpu));
        assert_eq!(arguments.demosaic, Some(DemosaicPreference::MalvarHeCutler));

        let defaults = Arguments::try_parse_from(["rohditor-desktop"])
            .expect("default desktop arguments parse");
        assert_eq!(defaults.demosaic, None);
        assert_eq!(defaults.processor, None);
        assert!(arguments.diagnostics);
    }

    #[test]
    fn cpu_only_setting_selects_the_processor_unless_overridden() {
        let mut cpu_only = AppSettings::default();
        cpu_only.set_cpu_only(true);
        assert_eq!(
            resolve_processor(AppSettings::default(), None),
            ProcessorPreference::Auto
        );
        assert_eq!(resolve_processor(cpu_only, None), ProcessorPreference::Cpu);
        assert_eq!(
            resolve_processor(cpu_only, Some(ProcessorPreference::Gpu)),
            ProcessorPreference::Gpu
        );
        assert_eq!(
            resolve_processor(AppSettings::default(), Some(ProcessorPreference::Cpu)),
            ProcessorPreference::Cpu
        );
    }

    #[test]
    fn rcd_is_a_desktop_demosaic_choice() {
        let arguments = Arguments::try_parse_from(["rohditor-desktop", "--demosaic", "rcd"])
            .expect("rcd should be a supported desktop demosaic choice");
        assert_eq!(arguments.demosaic, Some(DemosaicPreference::Rcd));
        assert_eq!(
            arguments.demosaic.map(DemosaicAlgorithm::from),
            Some(DemosaicAlgorithm::Rcd)
        );
    }

    #[test]
    fn amaze_is_a_desktop_demosaic_choice() {
        let arguments = Arguments::try_parse_from(["rohditor-desktop", "--demosaic", "amaze"])
            .expect("amaze should be a supported desktop demosaic choice");
        assert_eq!(arguments.demosaic, Some(DemosaicPreference::Amaze));
        assert_eq!(
            arguments.demosaic.map(DemosaicAlgorithm::from),
            Some(DemosaicAlgorithm::Amaze)
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
