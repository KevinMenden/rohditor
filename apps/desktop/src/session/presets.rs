//! The saved presets: applying and managing them.

use std::sync::Arc;

use gpui::{Context, SharedString};
use rohditor_edit::RecipeSection;
use rohditor_engine::presets::{Preset, PresetStore};

use super::Session;

/// A preset as it is listed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PresetEntry {
    pub name: SharedString,
    pub sections: Vec<RecipeSection>,
}

pub(super) struct Presets {
    store: PresetStore,
    entries: Arc<[PresetEntry]>,
}

impl Presets {
    pub fn new(store: PresetStore) -> Self {
        let mut presets = Self {
            store,
            entries: Arc::default(),
        };
        presets.list();
        presets
    }

    fn list(&mut self) {
        let entries = self.store.presets().iter().map(|preset| PresetEntry {
            name: preset.name().to_owned().into(),
            sections: preset.sections().to_vec(),
        });
        self.entries = entries.collect();
    }
}

impl Session {
    pub fn presets(&self) -> Arc<[PresetEntry]> {
        Arc::clone(&self.presets.entries)
    }

    /// Why preset files could not be loaded.
    pub fn preset_warnings(&self) -> &[String] {
        self.presets.store.warnings()
    }

    pub fn apply_preset(&mut self, name: &str, cx: &mut Context<Self>) {
        if let Some(preset) = self.presets.store.get(name)
            && let Some(document) = self.document.as_mut()
            && document.apply_preset(preset, 1.0)
        {
            self.queue_preview();
        }
        self.tick(cx);
    }

    /// Change the amount of the preset applied last, as part of a gesture.
    pub fn set_preset_amount(&mut self, amount: f32, cx: &mut Context<Self>) {
        if self
            .document
            .as_mut()
            .is_some_and(|document| document.set_preset_amount(amount))
        {
            self.queue_preview();
            self.tick(cx);
        }
    }

    /// Save these sections of the open photo's recipe.
    pub fn save_preset(
        &mut self,
        name: &str,
        sections: &[RecipeSection],
        cx: &mut Context<Self>,
    ) -> Result<(), String> {
        let Some(document) = self.document.as_ref() else {
            return Err("There is no open photo".to_owned());
        };
        let result = Preset::new(name, document.edits.recipe(), sections)
            .and_then(|preset| Ok(self.presets.store.save(preset)?));
        self.presets_changed(result, cx)
    }

    pub fn rename_preset(
        &mut self,
        name: &str,
        new_name: &str,
        cx: &mut Context<Self>,
    ) -> Result<(), String> {
        let result = self.presets.store.rename(name, new_name);
        self.presets_changed(result, cx)
    }

    pub fn delete_preset(&mut self, name: &str, cx: &mut Context<Self>) -> Result<(), String> {
        let result = self.presets.store.delete(name);
        self.presets_changed(result, cx)
    }

    fn presets_changed(
        &mut self,
        result: Result<(), impl std::fmt::Display>,
        cx: &mut Context<Self>,
    ) -> Result<(), String> {
        self.presets.list();
        cx.notify();
        result.map_err(|error| error.to_string())
    }
}
