//! `--screenshot <PATH>`: a developer/agent aid that lets the real window
//! (real GPU, real compositor) render for a while, saves one frame as a PNG,
//! and closes the app. Look-only; for interaction use the headless scenarios
//! in `ui_tests`.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use eframe::egui;
use tracing::{error, info};

use super::RohditorApp;

/// A pending self-screenshot, armed at startup.
pub(super) struct ScreenshotRequest {
    path: PathBuf,
    /// Capture once this moment passes, giving the preview time to settle.
    capture_at: Instant,
    requested: bool,
}

impl ScreenshotRequest {
    pub(super) fn new(path: PathBuf, delay: Duration) -> Self {
        Self {
            path,
            capture_at: Instant::now() + delay,
            requested: false,
        }
    }
}

impl RohditorApp {
    /// Drive the screenshot state machine once per frame: ask for the capture
    /// when the delay has elapsed, then save it when egui delivers the event.
    pub(super) fn update_screenshot(&mut self, context: &egui::Context) {
        let Some(request) = &mut self.screenshot else {
            return;
        };
        if !request.requested {
            if Instant::now() >= request.capture_at {
                request.requested = true;
                context.send_viewport_cmd(egui::ViewportCommand::Screenshot(
                    egui::UserData::default(),
                ));
            }
            // Keep frames flowing so the delay is observed even when idle.
            context.request_repaint_after(Duration::from_millis(100));
            return;
        }
        let captured = context.input(|input| {
            input.events.iter().find_map(|event| match event {
                egui::Event::Screenshot { image, .. } => Some(image.clone()),
                _ => None,
            })
        });
        let Some(image) = captured else {
            context.request_repaint_after(Duration::from_millis(100));
            return;
        };
        let [width, height] = image.size;
        match ::image::save_buffer(
            &request.path,
            image.as_raw(),
            width as u32,
            height as u32,
            ::image::ColorType::Rgba8,
        ) {
            Ok(()) => info!(path = %request.path.display(), "saved window screenshot"),
            Err(error) => {
                error!(%error, path = %request.path.display(), "could not save screenshot")
            }
        }
        self.screenshot = None;
        context.send_viewport_cmd(egui::ViewportCommand::Close);
    }
}
