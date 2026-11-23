//! Rohditor on GPUI: `rohditor-gpui [FILE|FOLDER]`.

mod actions;
mod assets;
mod device;
mod panels;
mod session;
mod theme;
mod ui;
mod viewport;
mod workspace;

use std::path::PathBuf;

use gpui::{
    App, AppContext, Bounds, TitlebarOptions, WindowBounds, WindowDecorations, WindowOptions, px,
    size,
};

use crate::session::{Directories, Session};
use crate::workspace::Workspace;

fn main() {
    env_logger::init();
    run(std::env::args_os().nth(1).map(PathBuf::from));
}

/// `$XDG_CONFIG_HOME/rohditor`, shared with the egui app.
fn directories() -> Directories {
    let config = std::env::var_os("XDG_CONFIG_HOME")
        .filter(|directory| !directory.is_empty())
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".config")));
    Directories {
        config: config.map(|base| base.join("rohditor")),
    }
}

fn run(open: Option<PathBuf>) {
    gpui_platform::application()
        .with_assets(assets::Assets)
        .run(move |cx: &mut App| {
            actions::bind_keys(cx);
            let bounds = Bounds::centered(None, size(px(1440.0), px(900.0)), cx);
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
                let session = cx.new(|cx| {
                    Session::new(device::shared_context(), directories(), cx)
                        .unwrap_or_else(|error| panic!("could not start the engine: {error:#}"))
                });
                session.update(cx, |session, cx| match open {
                    Some(path) if path.is_dir() => session.open_folder(path, cx),
                    Some(path) => session.open_file(path, cx),
                    None => session.restore_folder(cx),
                });
                let closing = session.clone();
                cx.on_app_quit(move |cx| {
                    closing.update(cx, |session, cx| session.shut_down(cx));
                    async {}
                })
                .detach();
                cx.new(|cx| Workspace::new(session, window, cx))
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
