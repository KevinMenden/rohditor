mod app;
mod coordinator;
mod document;

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

#[derive(Debug, Parser)]
#[command(version, about = "Linux-first Sony RAW photo editor")]
struct Arguments {
    /// UI renderer. Image processing is CPU-only in Phase 4.
    #[arg(long, value_enum, default_value_t)]
    renderer: RendererPreference,

    /// Sony RAW file to open at startup.
    file: Option<PathBuf>,
}

fn main() -> eframe::Result {
    init_tracing();
    let arguments = Arguments::parse();
    match arguments.renderer {
        RendererPreference::Auto => match launch(eframe::Renderer::Wgpu, arguments.file.clone()) {
            Ok(()) => Ok(()),
            Err(error) => {
                warn!(%error, "wgpu UI initialization failed; retrying with glow");
                eprintln!("wgpu UI initialization failed ({error}); retrying with glow");
                launch(eframe::Renderer::Glow, arguments.file)
            }
        },
        RendererPreference::Wgpu => launch(eframe::Renderer::Wgpu, arguments.file),
        RendererPreference::Glow => launch(eframe::Renderer::Glow, arguments.file),
    }
}

fn launch(renderer: eframe::Renderer, initial_path: Option<PathBuf>) -> eframe::Result {
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
        Box::new(move |context| Ok(Box::new(RohditorApp::new(context, initial_path.clone())?))),
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
