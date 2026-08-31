mod app;
mod coordinator;
mod document;
mod preview_cache;
mod ui;

use clap::{Parser, ValueEnum};
use eframe::egui;
use std::path::PathBuf;
use tracing::warn;
use tracing_subscriber::EnvFilter;

use crate::app::RohditorApp;

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
                    arguments.diagnostics,
                )
            }
        },
        RendererPreference::Wgpu => launch(
            eframe::Renderer::Wgpu,
            arguments.file,
            arguments.processor,
            arguments.diagnostics,
        ),
        RendererPreference::Glow => launch(
            eframe::Renderer::Glow,
            arguments.file,
            arguments.processor,
            arguments.diagnostics,
        ),
    }
}

fn launch(
    renderer: eframe::Renderer,
    initial_path: Option<PathBuf>,
    processor: ProcessorPreference,
    show_diagnostics: bool,
) -> eframe::Result {
    let options = eframe::NativeOptions {
        renderer,
        viewport: egui::ViewportBuilder::default()
            .with_title("Rohditor")
            .with_inner_size([1_280.0, 800.0])
            .with_min_inner_size([900.0, 600.0]),
        ..eframe::NativeOptions::default()
    };
    eframe::run_native(
        "rohditor",
        options,
        Box::new(move |context| {
            Ok(Box::new(RohditorApp::new(
                context,
                initial_path.clone(),
                processor,
                show_diagnostics,
            )?))
        }),
    )
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

    use super::{Arguments, ProcessorPreference, RendererPreference};

    #[test]
    fn processor_and_renderer_preferences_are_exposed_as_cli_choices() {
        let arguments = Arguments::try_parse_from([
            "rohditor-desktop",
            "--renderer",
            "glow",
            "--processor",
            "cpu",
            "--diagnostics",
        ])
        .expect("valid desktop preferences should parse");
        assert_eq!(arguments.renderer, RendererPreference::Glow);
        assert_eq!(arguments.processor, ProcessorPreference::Cpu);
        assert!(arguments.diagnostics);
    }
}
