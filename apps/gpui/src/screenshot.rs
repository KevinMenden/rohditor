//! `--screenshot <PATH>`: a developer/agent aid that lets the real window (real GPU, real
//! compositor) render, saves one frame as a PNG, and quits. Look-only; for interaction use the
//! headless scenarios in `ui_tests`.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::TryRecvError;
use std::time::{Duration, Instant};

use anyhow::{Context as _, Result};
use gpui::{AnyWindowHandle, App, AppContext as _, AsyncApp, Entity};

use crate::cli::Screenshot;
use crate::session::Session;

/// How long to wait for the preview to settle after the delay, before capturing anyway.
const SETTLE_LIMIT: Duration = Duration::from_secs(60);
/// How long the window may take to draw the captured frame.
const DRAW_LIMIT: Duration = Duration::from_secs(10);
const POLL: Duration = Duration::from_millis(50);

/// Capture `window` once `screenshot.delay` has passed and the preview has settled, then quit.
/// `failed` is set when no screenshot was written.
pub fn arm(
    screenshot: Screenshot,
    window: AnyWindowHandle,
    session: Entity<Session>,
    failed: Arc<AtomicBool>,
    cx: &mut App,
) {
    cx.spawn(async move |cx| {
        let saved = capture(&screenshot, window, &session, cx).await;
        match saved {
            Ok(()) => log::info!(
                "saved the window screenshot to {}",
                screenshot.path.display()
            ),
            Err(error) => {
                log::error!("no screenshot: {error:#}");
                eprintln!("could not save the screenshot: {error:#}");
                failed.store(true, Ordering::Relaxed);
            }
        }
        cx.update(|cx| cx.quit());
    })
    .detach();
}

async fn capture(
    screenshot: &Screenshot,
    window: AnyWindowHandle,
    session: &Entity<Session>,
    cx: &mut AsyncApp,
) -> Result<()> {
    cx.background_executor().timer(screenshot.delay).await;
    let deadline = Instant::now() + SETTLE_LIMIT;
    while !session.read_with(cx, |session, _| session.preview_settled()) {
        if Instant::now() >= deadline {
            log::warn!("the preview did not settle; capturing it as it is");
            break;
        }
        cx.background_executor().timer(POLL).await;
    }
    // The next drawn frame answers; make the window draw one.
    let frame = gpui_wgpu::capture_next_frame();
    cx.update_window(window, |_, window, _| window.refresh())?;
    let deadline = Instant::now() + DRAW_LIMIT;
    let image = loop {
        anyhow::ensure!(Instant::now() < deadline, "the window drew no frame");
        match frame.try_recv() {
            Ok(image) => break image?,
            Err(TryRecvError::Empty) => cx.background_executor().timer(POLL).await,
            Err(TryRecvError::Disconnected) => anyhow::bail!("the frame was not captured"),
        }
    };
    image
        .save(&screenshot.path)
        .with_context(|| format!("writing {}", screenshot.path.display()))
}
