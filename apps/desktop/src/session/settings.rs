//! The app settings: loaded at startup with the neural model, changed by the settings dialog,
//! and saved at once.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use gpui::Context;
use rohditor_demosaic::DemosaicAlgorithm;
use rohditor_engine::preview::ProcessorPreference;
use rohditor_engine::settings::{self, AppSettings};
use rohditor_neural::NeuralDemosaicer;

use super::Session;

/// The settings to start with, and what the session needs to change them later.
pub struct StartupSettings {
    pub settings: AppSettings,
    /// The processor for the whole run.
    pub processor: ProcessorPreference,
    /// Where they are saved; `None` when there is no configuration directory.
    pub config: Option<PathBuf>,
    /// Why the neural demosaic cannot be chosen; `None` when its model is loaded.
    pub neural_unavailable: Option<String>,
    /// Why the settings in use are not the saved ones.
    pub warning: Option<String>,
}

/// What the command line asks for.
#[derive(Debug, Default)]
pub struct StartupChoices {
    /// Chosen like in the settings dialog, so it is saved.
    pub demosaic: Option<DemosaicAlgorithm>,
    /// For this run only.
    pub processor: Option<ProcessorPreference>,
}

impl StartupSettings {
    /// Load the settings from `config` and the neural model from `data`, and apply `choices`.
    /// The model is for the render worker, which starts before the session.
    pub fn load(
        config: Option<PathBuf>,
        data: Option<&Path>,
        choices: StartupChoices,
    ) -> (Self, Option<Arc<NeuralDemosaicer>>) {
        let loaded = settings::load(config.as_deref());
        let mut chosen = loaded.settings;
        if let Some(demosaic) = choices.demosaic {
            chosen.set_demosaic(demosaic);
        }
        let processor = ProcessorPreference::for_run(chosen, choices.processor);
        let model = rohditor_engine::models::load_installed(data, chosen.cpu_only());
        let neural_unavailable = model.as_ref().err().cloned();
        let (settings, neural_warning) = neural_unavailable
            .as_deref()
            .map_or((chosen, None), |reason| {
                settings::without_neural(chosen, reason)
            });
        // A choice the run cannot use (neural without its model) is not saved.
        let save_warning = (settings != loaded.settings && settings == chosen)
            .then(|| save(config.as_deref(), settings))
            .flatten();
        let startup = Self {
            settings,
            processor,
            config,
            neural_unavailable,
            warning: neural_warning.or(save_warning).or(loaded.warning),
        };
        (startup, model.ok())
    }
}

/// Save `settings`; the warning to show when that fails.
fn save(config: Option<&Path>, settings: AppSettings) -> Option<String> {
    settings::save(config, settings).err().map(|error| {
        log::warn!("the settings could not be saved: {error}");
        format!("The setting is used now, but could not be saved: {error}")
    })
}

/// What the session keeps besides the settings in use.
pub(super) struct SettingsFile {
    config: Option<PathBuf>,
    neural_unavailable: Option<String>,
    warning: Option<String>,
}

impl SettingsFile {
    pub(super) fn new(startup: StartupSettings) -> (AppSettings, ProcessorPreference, Self) {
        let file = Self {
            config: startup.config,
            neural_unavailable: startup.neural_unavailable,
            warning: startup.warning,
        };
        (startup.settings, startup.processor, file)
    }
}

impl Session {
    pub fn settings(&self) -> AppSettings {
        self.renderer.settings
    }

    /// Why the neural demosaic cannot be chosen; `None` when its model is loaded.
    pub fn neural_unavailable(&self) -> Option<&str> {
        self.settings_file.neural_unavailable.as_deref()
    }

    /// Why the settings in use are not the saved ones: they did not load, or did not save.
    pub fn settings_warning(&self) -> Option<&str> {
        self.settings_file.warning.as_deref()
    }

    /// Change a setting, use it at once and save it.
    pub fn change_settings(
        &mut self,
        change: impl FnOnce(&mut AppSettings),
        cx: &mut Context<Self>,
    ) {
        let mut next = self.renderer.settings;
        change(&mut next);
        if next == self.renderer.settings {
            return;
        }
        let changes_pixels = self.renderer.settings.changes_pixels(next);
        self.renderer.settings = next;
        if changes_pixels {
            // A pick samples the pixels it was started on.
            self.stop_picking(cx);
            self.queue_preview();
        }
        self.settings_file.warning = save(self.settings_file.config.as_deref(), next);
        cx.notify();
    }
}
