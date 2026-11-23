//! The presets folder: one JSON file per preset.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use tracing::warn;

use super::preset::{Preset, PresetError, PresetFile, slug};
use crate::storage;

const PRESET_DIRECTORY: &str = "presets";
const PRESET_EXTENSION: &str = "json";
const MAX_PRESETS: usize = 256;
const MAX_PRESET_FILE_BYTES: u64 = 4 * 1024 * 1024;

/// The saved presets, loaded from `<config directory>/presets`.
#[derive(Debug, Default)]
pub struct PresetStore {
    /// `None` when the application has no configuration directory; the store
    /// is then empty and cannot save.
    directory: Option<PathBuf>,
    /// Sorted by name, ignoring case.
    presets: Vec<Preset>,
    /// One line per file that was skipped on load.
    warnings: Vec<String>,
}

impl PresetStore {
    /// Read every preset file. A file that cannot be read, parsed or
    /// validated, or whose name does not match its file name, is skipped and
    /// reported in [`Self::warnings`]; it never fails the whole list.
    pub fn load(config_directory: Option<&Path>) -> Self {
        let mut store = Self {
            directory: config_directory.map(|directory| directory.join(PRESET_DIRECTORY)),
            ..Self::default()
        };
        let Some(directory) = store.directory.clone() else {
            return store;
        };
        let entries = match fs::read_dir(&directory) {
            Ok(entries) => entries,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return store,
            Err(error) => {
                store.warn(format!("Preset folder could not be read: {error}"));
                return store;
            }
        };
        let mut paths: Vec<PathBuf> = entries
            .filter_map(Result::ok)
            .map(|entry| entry.path())
            .filter(|path| path.extension().is_some_and(|ext| ext == PRESET_EXTENSION))
            .collect();
        paths.sort();
        if paths.len() > MAX_PRESETS {
            store.warn(format!("Only the first {MAX_PRESETS} presets were loaded"));
            paths.truncate(MAX_PRESETS);
        }
        for path in paths {
            match read_preset(&path) {
                Ok(preset) => store.presets.push(preset),
                Err(reason) => store.warn(format!("Skipped preset {}: {reason}", path.display())),
            }
        }
        store.sort();
        store
    }

    pub fn presets(&self) -> &[Preset] {
        &self.presets
    }

    pub fn get(&self, name: &str) -> Option<&Preset> {
        self.presets
            .iter()
            .find(|preset| same_name(preset.name(), name))
    }

    /// Why files were skipped on load.
    pub fn warnings(&self) -> &[String] {
        &self.warnings
    }

    /// Store a new preset. Fails when a preset with the same name (ignoring
    /// case and punctuation) exists; delete or rename that one first.
    pub fn save(&mut self, preset: Preset) -> Result<(), PresetError> {
        if self.get(preset.name()).is_some() {
            return Err(PresetError::NameTaken(preset.name().to_owned()));
        }
        self.write(&preset)?;
        self.presets.push(preset);
        self.sort();
        Ok(())
    }

    pub fn delete(&mut self, name: &str) -> Result<(), PresetError> {
        let index = self.index_of(name)?;
        let path = self.path_for(self.presets[index].name())?;
        match fs::remove_file(&path) {
            Ok(()) => {}
            // Already gone: the list should still drop it.
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(source) => return Err(PresetError::Write { path, source }),
        }
        self.presets.remove(index);
        Ok(())
    }

    /// Give a preset another name. Fails like [`Self::save`] when the new name
    /// is taken by a different preset; changing only case or punctuation of
    /// its own name is fine.
    pub fn rename(&mut self, name: &str, new_name: &str) -> Result<(), PresetError> {
        let index = self.index_of(name)?;
        let renamed = self.presets[index].renamed(new_name)?;
        let taken = self
            .presets
            .iter()
            .enumerate()
            .any(|(other, preset)| other != index && same_name(preset.name(), renamed.name()));
        if taken {
            return Err(PresetError::NameTaken(renamed.name().to_owned()));
        }
        let old_path = self.path_for(self.presets[index].name())?;
        // Write the new file before removing the old one, so a failure never
        // loses the preset.
        self.write(&renamed)?;
        if self.path_for(renamed.name())? != old_path {
            let _ = fs::remove_file(&old_path);
        }
        self.presets[index] = renamed;
        self.sort();
        Ok(())
    }

    fn index_of(&self, name: &str) -> Result<usize, PresetError> {
        self.presets
            .iter()
            .position(|preset| same_name(preset.name(), name))
            .ok_or_else(|| PresetError::NotFound(name.to_owned()))
    }

    fn path_for(&self, name: &str) -> Result<PathBuf, PresetError> {
        let directory = self.directory.as_ref().ok_or(PresetError::NoDirectory)?;
        Ok(directory.join(format!("{}.{PRESET_EXTENSION}", slug(name))))
    }

    fn write(&self, preset: &Preset) -> Result<(), PresetError> {
        let path = self.path_for(preset.name())?;
        let mut json = serde_json::to_vec_pretty(&preset.to_file())?;
        json.push(b'\n');
        storage::write_transactionally(&path, &json)
            .map_err(|source| PresetError::Write { path, source })
    }

    fn sort(&mut self) {
        self.presets
            .sort_by_cached_key(|preset| preset.name().to_lowercase());
    }

    fn warn(&mut self, message: String) {
        warn!("{message}");
        self.warnings.push(message);
    }
}

fn same_name(a: &str, b: &str) -> bool {
    slug(a) == slug(b)
}

/// Parse and validate one file. The error text is shown to the user.
fn read_preset(path: &Path) -> Result<Preset, String> {
    let metadata = fs::metadata(path).map_err(|error| error.to_string())?;
    if metadata.len() > MAX_PRESET_FILE_BYTES {
        return Err(format!(
            "file exceeds the {MAX_PRESET_FILE_BYTES}-byte limit"
        ));
    }
    let bytes = fs::read(path).map_err(|error| error.to_string())?;
    let file: PresetFile = serde_json::from_slice(&bytes).map_err(|error| error.to_string())?;
    let preset = Preset::from_file(file).map_err(|error| error.to_string())?;
    // The file name is how the store finds the file again, so it must follow
    // from the name.
    if path.file_stem().and_then(|stem| stem.to_str()) != Some(slug(preset.name()).as_str()) {
        return Err("file name does not match the preset name".to_owned());
    }
    Ok(preset)
}
