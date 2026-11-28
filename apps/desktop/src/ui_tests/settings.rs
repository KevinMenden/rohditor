//! The settings dialog and the diagnostics panel.

use rohditor_demosaic::DemosaicAlgorithm;

use super::*;
use crate::session::ModelInstall;

fn demosaic(harness: &mut Harness) -> DemosaicAlgorithm {
    harness.with_session(|session, _| session.settings().demosaic())
}

/// The saved settings file.
fn saved(harness: &Harness) -> String {
    std::fs::read_to_string(harness.config.join("settings.json")).unwrap_or_default()
}

#[test]
#[ignore = "requires a wgpu adapter"]
fn settings_are_used_and_saved_at_once() {
    let mut harness = Harness::new();
    harness.keys("ctrl-,");
    harness.find("settings-dialog");
    harness.find("setting:Demosaic algorithm");

    // Without an installed model, Neural is shown but cannot be chosen.
    harness.click("choice:demosaic:Neural");
    assert_eq!(demosaic(&mut harness), DemosaicAlgorithm::Amaze);
    assert_eq!(saved(&harness), "");

    harness.click("choice:demosaic:RCD");
    assert_eq!(demosaic(&mut harness), DemosaicAlgorithm::Rcd);
    assert!(saved(&harness).contains("\"demosaic\": \"rcd\""));

    harness.capture("settings");

    // CPU only is below the fold; the search brings it up.
    harness.keys("c p u");
    harness.click("toggle:cpu-only");
    assert!(harness.with_session(|session, _| session.settings().cpu_only()));
    assert!(saved(&harness).contains("\"cpu_only\": true"));
    harness.keys("escape");

    // The search has the focus and narrows the list.
    harness.keys("n o i s e");
    harness.find("setting:Automatic noise reduction");
    assert_eq!(harness.lookup("setting:CPU only"), None);
    harness.keys("x y z");
    assert_eq!(harness.lookup("setting:Automatic noise reduction"), None);

    // Escape clears the search, then closes.
    harness.keys("escape");
    harness.find("setting:CPU only");
    harness.keys("escape");
    harness.draw();
    assert_eq!(harness.lookup("settings-dialog"), None);
}

/// The test model of `rohditor-neural`.
fn tiny_model() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../crates/neural/tests/data/tiny.onnx")
}

#[test]
#[ignore = "requires a wgpu adapter"]
fn a_neural_model_is_installed_for_the_next_start() {
    let mut harness = Harness::new();
    harness.keys("ctrl-,");
    harness.find("setting:Neural model");
    harness.find("button:install-neural-model");
    harness.capture("settings-model-missing");

    // A file that is not a model is refused and nothing is installed.
    let garbage = harness.config.join("garbage.onnx");
    std::fs::create_dir_all(&harness.config).expect("scratch");
    std::fs::write(&garbage, b"not a model").expect("write");
    harness.with_session(|session, cx| session.install_neural_model(garbage, cx));
    harness.wait_until("the refused install", |session| {
        session.settings_warning().is_some()
    });
    assert_eq!(
        harness.with_session(|session, _| session.model_install()),
        None
    );
    harness.find("settings-warning");

    // The file dialog cannot run headless; install the chosen file directly.
    harness.with_session(|session, cx| session.install_neural_model(tiny_model(), cx));
    harness.wait_until("the installed model", |session| {
        matches!(session.model_install(), Some(ModelInstall::Installed(_)))
    });
    let installed = rohditor_engine::models::ModelStore::new(&harness.config.join("data"));
    assert!(matches!(
        installed.status(),
        rohditor_engine::models::ModelStatus::Installed { .. }
    ));
    // Until the restart, the render worker has no model.
    harness.click("choice:demosaic:Neural");
    assert_eq!(demosaic(&mut harness), DemosaicAlgorithm::Amaze);
    assert_eq!(harness.lookup("settings-warning"), None);
    harness.find("button:install-neural-model");
    harness.capture("settings-model-installed");
}

#[test]
#[ignore = "requires a wgpu adapter"]
fn the_diagnostics_tab_comes_and_goes() {
    let mut harness = Harness::new();
    assert_eq!(harness.lookup("button:tab-diagnostics"), None);
    harness.keys("ctrl-shift-p d i a g n o s t i c s enter");
    harness.find("diagnostics-panel");
    harness.find("button:save-report");

    // Another tab hides the panel but keeps the tab; pressing the tab shows it again.
    harness.click("tab:Files");
    assert_eq!(harness.lookup("diagnostics-panel"), None);
    harness.click("button:tab-diagnostics");
    harness.find("diagnostics-panel");
    harness.capture("diagnostics");

    // Pressed while shown, it goes away.
    harness.click("button:tab-diagnostics");
    assert_eq!(harness.lookup("diagnostics-panel"), None);
    assert_eq!(harness.lookup("button:tab-diagnostics"), None);
}

#[test]
#[ignore = "requires a wgpu adapter and ROHDITOR_RAW"]
fn a_new_demosaic_renders_the_photo_again() {
    let Some((mut harness, ..)) = opened("settings-demosaic") else {
        return;
    };
    let rendered_with = |session: &Session| {
        let catalog = rohditor_engine::catalog::CatalogState::default();
        let diagnostics = session.diagnostics(&catalog);
        diagnostics.preview.map(|preview| preview.algorithm)
    };
    harness.wait_until("the AMaZE preview", |session| {
        settled(session) && rendered_with(session).as_deref() == Some("amaze")
    });
    harness.with_session(|session, cx| {
        session.change_settings(|settings| settings.set_demosaic(DemosaicAlgorithm::Rcd), cx);
    });
    harness.wait_until("the RCD preview", |session| {
        settled(session) && rendered_with(session).as_deref() == Some("rcd")
    });
    harness.keys("ctrl-shift-p d i a g n o s t i c s enter");
    harness.find("diagnostics-panel");
    harness.capture("diagnostics-preview");
}
