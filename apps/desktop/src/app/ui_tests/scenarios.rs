//! Individual UI scenarios. Add new ones here, one `#[test]` per screen or
//! interaction, each ending in a named `snapshot` so the result can be viewed.
//!
//! They are `#[ignore]`d like the other GPU suites: they need a wgpu adapter
//! and compare PNGs, so they are run on demand (see the UI testing skill).

use egui_kittest::kittest::Queryable as _;

use super::{app_harness, scratch_config_dir};

/// The app keeps requesting repaints while workers run, so scenarios step a
/// fixed number of frames instead of waiting for the UI to go idle.
const SETTLE_FRAMES: usize = 10;

#[test]
#[ignore = "needs a wgpu adapter; run with --ignored"]
fn startup_without_a_photo() {
    let mut harness = app_harness(None, scratch_config_dir("startup"));
    harness.run_steps(SETTLE_FRAMES);
    harness.snapshot("startup_without_a_photo");
}

/// Widgets are found by accessible label, not by pixel position.
#[test]
#[ignore = "needs a wgpu adapter; run with --ignored"]
fn library_view_opens_from_the_top_bar() {
    let mut harness = app_harness(None, scratch_config_dir("library"));
    harness.run_steps(SETTLE_FRAMES);
    harness.get_by_label("Library").click();
    harness.run_steps(SETTLE_FRAMES);
    harness.snapshot("library_view_opens_from_the_top_bar");
}

/// Without an installed model (the scratch data directory has none), "Neural"
/// is listed but disabled, with the reason and how to install it.
#[test]
#[ignore = "needs a wgpu adapter; run with --ignored"]
fn settings_offer_neural_only_with_its_model() {
    let mut harness = app_harness(None, scratch_config_dir("settings-neural"));
    harness.run_steps(SETTLE_FRAMES);
    harness.get_by_label("Application menu").click();
    harness.run_steps(SETTLE_FRAMES);
    harness.get_by_label("Settings…").click();
    harness.run_steps(SETTLE_FRAMES);
    assert!(
        harness
            .query_by_label_contains("Neural is unavailable")
            .is_some()
    );
    harness.snapshot("settings_offer_neural_only_with_its_model");
}
