//! Versioned desktop preferences and their tolerant JSON persistence.

use std::fs;
use std::io;
use std::path::Path;

use rohditor_core::{OutputPolicy, RenderOptions};
use rohditor_demosaic::DemosaicAlgorithm;
use serde::{Deserialize, Serialize};
use tracing::warn;

use crate::storage;

const SCHEMA_VERSION: u32 = 1;
const SETTINGS_FILE_NAME: &str = "settings.json";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AppSettings {
    schema_version: u32,
    demosaic: DemosaicAlgorithm,
    output_policy: OutputPolicy,
    /// Disables all GPU image processing. Read once at startup, so a change
    /// takes effect on the next launch.
    cpu_only: bool,
    /// Estimate each newly opened photo's noise and start it with colour noise
    /// reduction when it is noisy enough to matter. Photos with saved edits
    /// keep what they have.
    auto_noise_reduction: bool,
}

impl Default for AppSettings {
    fn default() -> Self {
        Self {
            schema_version: SCHEMA_VERSION,
            demosaic: DemosaicAlgorithm::Amaze,
            output_policy: OutputPolicy::ClipToSrgb,
            cpu_only: false,
            auto_noise_reduction: true,
        }
    }
}

impl AppSettings {
    pub const fn demosaic(self) -> DemosaicAlgorithm {
        self.demosaic
    }

    pub fn set_demosaic(&mut self, demosaic: DemosaicAlgorithm) {
        debug_assert!(demosaic != DemosaicAlgorithm::Bilinear);
        self.demosaic = demosaic;
    }

    pub const fn output_policy(self) -> OutputPolicy {
        self.output_policy
    }

    pub fn set_output_policy(&mut self, output_policy: OutputPolicy) {
        self.output_policy = output_policy;
    }

    pub const fn cpu_only(self) -> bool {
        self.cpu_only
    }

    pub fn set_cpu_only(&mut self, cpu_only: bool) {
        self.cpu_only = cpu_only;
    }

    pub const fn auto_noise_reduction(self) -> bool {
        self.auto_noise_reduction
    }

    pub fn set_auto_noise_reduction(&mut self, enabled: bool) {
        self.auto_noise_reduction = enabled;
    }

    pub fn render_options(self) -> RenderOptions {
        RenderOptions {
            demosaic: self.demosaic,
            output_policy: self.output_policy,
            ..RenderOptions::default()
        }
    }
}

#[derive(Debug)]
pub struct SettingsLoad {
    pub settings: AppSettings,
    pub warning: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
struct StoredSettings {
    schema_version: u32,
    #[serde(default)]
    demosaic: Option<String>,
    #[serde(default)]
    output_policy: Option<String>,
    #[serde(default)]
    cpu_only: Option<bool>,
    #[serde(default)]
    auto_noise_reduction: Option<bool>,
}

/// Load settings from `config_directory`; `None` means the application could
/// not determine one, which is reported as a warning with default settings.
pub fn load(config_directory: Option<&Path>) -> SettingsLoad {
    let Some(directory) = config_directory else {
        return load_failure("Could not determine the Rohditor configuration directory");
    };
    load_from_path(&directory.join(SETTINGS_FILE_NAME))
}

pub fn save(config_directory: Option<&Path>, settings: AppSettings) -> io::Result<()> {
    let directory = config_directory.ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::NotFound,
            "could not determine the Rohditor configuration directory",
        )
    })?;
    save_to_path(&directory.join(SETTINGS_FILE_NAME), settings)
}

pub fn resolve_startup_settings(
    persisted: AppSettings,
    command_line_override: Option<DemosaicAlgorithm>,
) -> AppSettings {
    let mut effective = persisted;
    if let Some(demosaic) = command_line_override {
        effective.demosaic = demosaic;
    }
    effective
}

fn load_from_path(path: &Path) -> SettingsLoad {
    let bytes = match fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            return SettingsLoad {
                settings: AppSettings::default(),
                warning: None,
            };
        }
        Err(error) => {
            return load_failure(format!("Could not read {}: {error}", path.display()));
        }
    };
    match decode(&bytes) {
        Ok(settings) => SettingsLoad {
            settings,
            warning: None,
        },
        Err(error) => load_failure(format!(
            "Could not load {}: {error}. Defaults are active; the file was not changed.",
            path.display()
        )),
    }
}

fn load_failure(message: impl Into<String>) -> SettingsLoad {
    let message = message.into();
    warn!(message, "desktop settings could not be loaded");
    SettingsLoad {
        settings: AppSettings::default(),
        warning: Some(message),
    }
}

fn decode(bytes: &[u8]) -> Result<AppSettings, String> {
    let stored: StoredSettings =
        serde_json::from_slice(bytes).map_err(|error| format!("malformed JSON ({error})"))?;
    if stored.schema_version != SCHEMA_VERSION {
        return Err(format!(
            "unsupported schema version {}",
            stored.schema_version
        ));
    }
    let demosaic = match stored.demosaic.as_deref().unwrap_or("amaze") {
        "mhc" => DemosaicAlgorithm::MalvarHeCutler,
        "rcd" => DemosaicAlgorithm::Rcd,
        "amaze" => DemosaicAlgorithm::Amaze,
        value => return Err(format!("unknown demosaic algorithm {value:?}")),
    };
    let output_policy = match stored.output_policy.as_deref().unwrap_or("clip_to_srgb") {
        "clip_to_srgb" => OutputPolicy::ClipToSrgb,
        "chroma_compress_to_srgb" => OutputPolicy::ChromaCompressToSrgb,
        value => return Err(format!("unknown output gamut policy {value:?}")),
    };
    Ok(AppSettings {
        schema_version: SCHEMA_VERSION,
        demosaic,
        output_policy,
        cpu_only: stored.cpu_only.unwrap_or(false),
        auto_noise_reduction: stored.auto_noise_reduction.unwrap_or(true),
    })
}

fn save_to_path(path: &Path, settings: AppSettings) -> io::Result<()> {
    if settings.demosaic == DemosaicAlgorithm::Bilinear {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "bilinear is a command-line-only demosaic override",
        ));
    }
    let stored = StoredSettings {
        schema_version: settings.schema_version,
        demosaic: Some(settings.demosaic.stable_name().to_owned()),
        output_policy: Some(
            match settings.output_policy {
                OutputPolicy::ClipToSrgb => "clip_to_srgb",
                OutputPolicy::ChromaCompressToSrgb => "chroma_compress_to_srgb",
            }
            .to_owned(),
        ),
        cpu_only: Some(settings.cpu_only),
        auto_noise_reduction: Some(settings.auto_noise_reduction),
    };
    let mut bytes = serde_json::to_vec_pretty(&stored).map_err(io::Error::other)?;
    bytes.push(b'\n');
    storage::write_transactionally(path, &bytes)
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicU64, Ordering};

    use super::*;

    static NEXT_DIRECTORY: AtomicU64 = AtomicU64::new(0);

    struct TestDirectory(PathBuf);

    impl TestDirectory {
        fn new() -> Self {
            let id = NEXT_DIRECTORY.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!(
                "rohditor-desktop-settings-{}-{id}",
                std::process::id()
            ));
            fs::create_dir(&path).expect("create settings test directory");
            Self(path)
        }

        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for TestDirectory {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn defaults_are_schema_one_and_amaze() {
        let settings = AppSettings::default();
        assert_eq!(settings.schema_version, 1);
        assert_eq!(settings.demosaic(), DemosaicAlgorithm::Amaze);
    }

    #[test]
    fn supported_algorithms_round_trip_with_stable_names() {
        for (algorithm, stable_name) in [
            (DemosaicAlgorithm::MalvarHeCutler, "mhc"),
            (DemosaicAlgorithm::Rcd, "rcd"),
            (DemosaicAlgorithm::Amaze, "amaze"),
        ] {
            let directory = TestDirectory::new();
            let path = directory.path().join("settings.json");
            let mut settings = AppSettings::default();
            settings.set_demosaic(algorithm);
            save_to_path(&path, settings).expect("save settings");
            let text = fs::read_to_string(&path).expect("read saved settings");
            assert!(text.contains(&format!("\"demosaic\": \"{stable_name}\"")));
            assert_eq!(load_from_path(&path).settings, settings);
        }
    }

    #[test]
    fn output_gamut_policy_round_trips_and_defaults_to_clip() {
        let defaults = decode(br#"{"schema_version":1}"#).expect("missing policy uses default");
        assert_eq!(defaults.output_policy(), OutputPolicy::ClipToSrgb);

        let directory = TestDirectory::new();
        let path = directory.path().join("settings.json");
        let mut settings = AppSettings::default();
        settings.set_output_policy(OutputPolicy::ChromaCompressToSrgb);
        save_to_path(&path, settings).expect("save output gamut setting");
        let text = fs::read_to_string(&path).expect("read saved settings");
        assert!(text.contains("\"output_policy\": \"chroma_compress_to_srgb\""));
        assert_eq!(load_from_path(&path).settings, settings);
    }

    #[test]
    fn cpu_only_defaults_to_false_and_round_trips() {
        let defaults = decode(br#"{"schema_version":1}"#).expect("missing field uses default");
        assert!(!defaults.cpu_only());

        let directory = TestDirectory::new();
        let path = directory.path().join("settings.json");
        let mut settings = AppSettings::default();
        settings.set_cpu_only(true);
        save_to_path(&path, settings).expect("save cpu-only setting");
        let text = fs::read_to_string(&path).expect("read saved settings");
        assert!(text.contains("\"cpu_only\": true"));
        assert_eq!(load_from_path(&path).settings, settings);
    }

    #[test]
    fn auto_noise_reduction_defaults_to_on_and_round_trips() {
        let defaults = decode(br#"{"schema_version":1}"#).expect("missing field uses default");
        assert!(defaults.auto_noise_reduction());

        let directory = TestDirectory::new();
        let path = directory.path().join("settings.json");
        let mut settings = AppSettings::default();
        settings.set_auto_noise_reduction(false);
        save_to_path(&path, settings).expect("save auto noise reduction setting");
        let text = fs::read_to_string(&path).expect("read saved settings");
        assert!(text.contains("\"auto_noise_reduction\": false"));
        assert_eq!(load_from_path(&path).settings, settings);
    }

    #[test]
    fn missing_fields_default_and_unknown_fields_are_ignored() {
        let settings = decode(br#"{"schema_version":1,"future":true}"#)
            .expect("supported schema remains forward compatible");
        assert_eq!(settings, AppSettings::default());
    }

    #[test]
    fn missing_file_is_silent_but_invalid_files_warn_and_use_defaults() {
        let directory = TestDirectory::new();
        let path = directory.path().join("settings.json");
        let missing = load_from_path(&path);
        assert_eq!(missing.settings, AppSettings::default());
        assert!(missing.warning.is_none());

        for invalid in [
            "not json",
            r#"{"schema_version":1,"demosaic":"bilinear"}"#,
            r#"{"schema_version":2,"demosaic":"rcd"}"#,
        ] {
            fs::write(&path, invalid).expect("write invalid settings");
            let loaded = load_from_path(&path);
            assert_eq!(loaded.settings, AppSettings::default());
            assert!(loaded.warning.is_some());
            assert_eq!(fs::read_to_string(&path).expect("file remains"), invalid);
        }
    }

    #[test]
    fn save_creates_parent_directories_and_replaces_old_file() {
        let directory = TestDirectory::new();
        let path = directory.path().join("nested/rohditor/settings.json");
        let mut settings = AppSettings::default();
        settings.set_demosaic(DemosaicAlgorithm::Rcd);
        save_to_path(&path, settings).expect("first save creates directories");
        settings.set_demosaic(DemosaicAlgorithm::Amaze);
        save_to_path(&path, settings).expect("second save replaces file");
        assert_eq!(load_from_path(&path).settings, settings);
    }

    #[test]
    fn bilinear_remains_a_non_persisted_development_override() {
        let directory = TestDirectory::new();
        let path = directory.path().join("settings.json");
        let settings =
            resolve_startup_settings(AppSettings::default(), Some(DemosaicAlgorithm::Bilinear));
        let error = save_to_path(&path, settings).expect_err("bilinear must not be persisted");
        assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
        assert!(!path.exists());
    }

    #[test]
    fn command_line_override_has_documented_precedence() {
        let mut persisted = AppSettings::default();
        persisted.set_demosaic(DemosaicAlgorithm::Rcd);
        assert_eq!(resolve_startup_settings(persisted, None), persisted);
        assert_eq!(
            resolve_startup_settings(persisted, Some(DemosaicAlgorithm::Amaze)).demosaic(),
            DemosaicAlgorithm::Amaze
        );
        assert_eq!(
            resolve_startup_settings(AppSettings::default(), None),
            AppSettings::default()
        );
    }
}
