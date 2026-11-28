//! Rohditor on GPUI: `rohditor-desktop [FILE|FOLDER]`; `--help` lists the options.

mod actions;
mod assets;
mod atlas_image;
mod cli;
mod device;
mod library;
mod panels;
mod screenshot;
mod session;
mod theme;
mod ui;
mod viewport;
mod wake;
mod workspace;

use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use anyhow::Result;
use clap::Parser as _;
use gpui::{
    App, AppContext, Bounds, TitlebarOptions, WindowBounds, WindowDecorations, WindowOptions, px,
    size,
};

use rohditor_engine::camera_profiles::CameraProfileRegistry;
use rohditor_engine::presets::PresetStore;

use crate::library::{Library, LibraryWorker};
use crate::session::{Session, SessionWorkers, StartupSettings};
use crate::workspace::Workspace;

/// The Wayland app id. It must match the installed desktop file and icon
/// (`assets/io.github.kevin.rohditor.*`), or the compositor shows neither for the window.
const APPLICATION_ID: &str = "io.github.kevin.rohditor";

fn main() -> Result<ExitCode> {
    env_logger::init();
    let arguments = cli::Arguments::parse();
    // Before the workers: the render worker takes the neural model.
    let (settings, neural) = StartupSettings::load(
        config_directory(),
        data_directory(),
        arguments.startup_choices(),
    );
    let workers = Workers {
        session: SessionWorkers::start(neural)?,
        library: LibraryWorker::start()?,
    };
    let startup = Startup {
        // Relative paths would not match the folder tree's.
        open: (arguments.open.as_ref())
            .map(|path| std::path::absolute(path).unwrap_or_else(|_| path.clone())),
        diagnostics: arguments.diagnostics,
        screenshot: arguments.screenshot(),
    };
    let screenshot_failed = Arc::new(AtomicBool::new(false));
    run(startup, settings, workers, screenshot_failed.clone());
    Ok(if screenshot_failed.load(Ordering::Relaxed) {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    })
}

/// What the command line asks the window to do.
struct Startup {
    open: Option<PathBuf>,
    diagnostics: bool,
    screenshot: Option<cli::Screenshot>,
}

struct Workers {
    session: SessionWorkers,
    library: LibraryWorker,
}

/// `$XDG_CONFIG_HOME/rohditor`.
fn config_directory() -> Option<PathBuf> {
    xdg_directory("XDG_CONFIG_HOME", ".config")
}

/// `$XDG_DATA_HOME/rohditor`, where the neural model is installed; shared with the CLI.
fn data_directory() -> Option<PathBuf> {
    xdg_directory("XDG_DATA_HOME", ".local/share")
}

/// `$variable/rohditor`, or `$HOME/fallback/rohditor` when it is not set.
fn xdg_directory(variable: &str, fallback: &str) -> Option<PathBuf> {
    let base = std::env::var_os(variable)
        .filter(|directory| !directory.is_empty())
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(fallback)));
    base.map(|base| base.join("rohditor"))
}

fn run(
    startup: Startup,
    settings: StartupSettings,
    workers: Workers,
    screenshot_failed: Arc<AtomicBool>,
) {
    gpui_platform::application()
        .with_assets(assets::Assets)
        .run(move |cx: &mut App| {
            actions::bind_keys(cx);
            let bounds = Bounds::centered(None, size(px(1800.0), px(1200.0)), cx);
            let options = WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                titlebar: Some(TitlebarOptions {
                    title: Some("Rohditor".into()),
                    appears_transparent: true,
                    traffic_light_position: None,
                }),
                window_decorations: Some(WindowDecorations::Client),
                app_id: Some(APPLICATION_ID.to_owned()),
                ..WindowOptions::default()
            };
            let opened = cx.open_window(options, |window, cx| {
                // The window's renderer exists by now, and with it GPUI's device.
                let config = settings.config.clone();
                let session = cx.new(|cx| {
                    let profiles = CameraProfileRegistry::load(config.as_deref());
                    Session::new(
                        workers.session,
                        settings,
                        profiles,
                        PresetStore::load(config.as_deref()),
                        device::shared_context(),
                        cx,
                    )
                });
                let library = cx.new(|cx| Library::new(workers.library, config, &session, cx));
                match startup.open {
                    Some(path) if path.is_dir() => {
                        library.update(cx, |library, cx| library.open_folder(path, cx));
                    }
                    Some(path) => session.update(cx, |session, cx| session.open_file(path, cx)),
                    None => library.update(cx, Library::restore),
                }
                let closing = session.clone();
                cx.on_app_quit(move |cx| {
                    closing.update(cx, |session, cx| session.shut_down(cx));
                    async {}
                })
                .detach();
                if let Some(screenshot) = startup.screenshot {
                    let handle = window.window_handle();
                    screenshot::arm(screenshot, handle, session.clone(), screenshot_failed, cx);
                }
                cx.new(|cx| {
                    let mut workspace = Workspace::new(session, library, window, cx);
                    if startup.diagnostics {
                        workspace.show_diagnostics(cx);
                    }
                    workspace
                })
            });
            if let Err(error) = opened {
                eprintln!("could not open the window: {error:#}");
                cx.spawn(async move |cx| cx.update(|cx| cx.quit())).detach();
            }
            cx.on_window_closed(|cx, _| {
                if cx.windows().is_empty() {
                    cx.quit();
                }
            })
            .detach();
            cx.activate(true);
        });
}

#[cfg(test)]
mod ui_tests;

#[cfg(test)]
mod tests {
    use super::APPLICATION_ID;

    #[test]
    fn application_id_matches_the_desktop_file() {
        let desktop_entry = include_str!("../../../assets/io.github.kevin.rohditor.desktop");
        assert!(desktop_entry.contains(&format!("Icon={APPLICATION_ID}\n")));
        assert!(desktop_entry.contains(&format!("StartupWMClass={APPLICATION_ID}\n")));
    }
}
