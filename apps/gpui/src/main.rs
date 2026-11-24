//! Rohditor on GPUI: `rohditor-gpui [FILE|FOLDER]`.

mod actions;
mod assets;
mod atlas_image;
mod device;
mod library;
mod panels;
mod session;
mod theme;
mod ui;
mod viewport;
mod wake;
mod workspace;

use std::path::PathBuf;

use anyhow::Result;
use gpui::{
    App, AppContext, Bounds, TitlebarOptions, WindowBounds, WindowDecorations, WindowOptions, px,
    size,
};

use crate::library::{Library, LibraryWorker};
use crate::session::{Session, SessionWorkers};
use crate::workspace::Workspace;

fn main() -> Result<()> {
    env_logger::init();
    let workers = Workers {
        session: SessionWorkers::start()?,
        library: LibraryWorker::start()?,
    };
    run(std::env::args_os().nth(1).map(PathBuf::from), workers);
    Ok(())
}

struct Workers {
    session: SessionWorkers,
    library: LibraryWorker,
}

/// `$XDG_CONFIG_HOME/rohditor`, shared with the egui app.
fn config_directory() -> Option<PathBuf> {
    let config = std::env::var_os("XDG_CONFIG_HOME")
        .filter(|directory| !directory.is_empty())
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".config")));
    config.map(|base| base.join("rohditor"))
}

fn run(open: Option<PathBuf>, workers: Workers) {
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
                app_id: Some("rohditor".to_owned()),
                ..WindowOptions::default()
            };
            let opened = cx.open_window(options, |window, cx| {
                // The window's renderer exists by now, and with it GPUI's device.
                let config = config_directory();
                let settings = rohditor_engine::settings::load(config.as_deref()).settings;
                let session = cx.new(|cx| {
                    Session::new(workers.session, settings, device::shared_context(), cx)
                });
                let library = cx.new(|cx| Library::new(workers.library, config, &session, cx));
                match open {
                    Some(path) if path.is_dir() => {
                        library.update(cx, |library, cx| library.open_folder(path, cx));
                    }
                    Some(path) => session.update(cx, |session, cx| session.open_file(path, cx)),
                    None => library.update(cx, Library::restore_folder),
                }
                let closing = session.clone();
                cx.on_app_quit(move |cx| {
                    closing.update(cx, |session, cx| session.shut_down(cx));
                    async {}
                })
                .detach();
                cx.new(|cx| Workspace::new(session, library, window, cx))
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
