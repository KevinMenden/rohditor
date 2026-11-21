//! Headless UI scenarios built on `egui_kittest`.
//!
//! These run the real `RohditorApp` without a window: widgets are found by
//! accessible label (not pixel position), and `snapshot` renders the frame to
//! a PNG under `apps/desktop/tests/snapshots/` that agents can open to look at
//! the UI. See `.agents/skills/rohditor-ui-testing/SKILL.md` for the workflow.
//!
//! The app is built with the CPU processor and a scratch config directory, so
//! scenarios never touch the user's real settings or session.

use std::path::PathBuf;

use egui_kittest::Harness;
use rohditor_engine::settings::AppSettings;

use super::RohditorApp;
use crate::ProcessorPreference;

mod scenarios;

/// Window size used by all scenarios (logical points).
const WINDOW_SIZE: [f32; 2] = [1_400.0, 900.0];

/// Build the real app in a headless harness. `initial_path` opens a RAW at
/// startup, like the command-line argument does.
fn app_harness(
    initial_path: Option<PathBuf>,
    scratch_config: PathBuf,
) -> Harness<'static, RohditorApp> {
    Harness::builder()
        .with_size(WINDOW_SIZE)
        .wgpu()
        .build_eframe(move |context| {
            RohditorApp::new(
                context,
                initial_path,
                ProcessorPreference::Cpu,
                AppSettings::default(),
                None,
                false,
                Some(scratch_config),
            )
            .expect("the app should start under the test harness")
        })
}

/// A fresh, empty scratch config directory unique to one scenario.
fn scratch_config_dir(name: &str) -> PathBuf {
    let directory = std::env::temp_dir().join(format!("rohditor-ui-test-{name}"));
    drop(std::fs::remove_dir_all(&directory));
    std::fs::create_dir_all(&directory).expect("scratch config directory");
    directory
}
