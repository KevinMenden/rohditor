//! Where the desktop app keeps its configuration. The engine takes this
//! directory as an argument instead of resolving it itself.

use std::ffi::OsStr;
use std::path::PathBuf;

/// Resolve Rohditor's application configuration directory without coupling
/// callers to process-global environment access.
pub(crate) fn config_directory_from(
    xdg_config_home: Option<&OsStr>,
    home: Option<&OsStr>,
) -> Option<PathBuf> {
    xdg_config_home
        .filter(|directory| !directory.is_empty())
        .map(PathBuf::from)
        .or_else(|| {
            home.filter(|directory| !directory.is_empty())
                .map(|directory| PathBuf::from(directory).join(".config"))
        })
        .map(|base| base.join("rohditor"))
}

/// Where downloaded data such as the neural demosaic model lives
/// (`$XDG_DATA_HOME/rohditor`, default `~/.local/share/rohditor`).
pub(crate) fn data_directory_from(
    xdg_data_home: Option<&OsStr>,
    home: Option<&OsStr>,
) -> Option<PathBuf> {
    xdg_data_home
        .filter(|directory| !directory.is_empty())
        .map(PathBuf::from)
        .or_else(|| {
            home.filter(|directory| !directory.is_empty())
                .map(|directory| PathBuf::from(directory).join(".local/share"))
        })
        .map(|base| base.join("rohditor"))
}

pub(crate) fn data_directory() -> Option<PathBuf> {
    data_directory_from(
        std::env::var_os("XDG_DATA_HOME").as_deref(),
        std::env::var_os("HOME").as_deref(),
    )
}

/// The directories the app persists to. Injected so tests can use scratch ones.
#[derive(Debug, Clone, Default)]
pub(crate) struct AppDirectories {
    /// Settings, session and camera profiles.
    pub config: Option<PathBuf>,
    /// Downloaded data such as the neural demosaic model.
    pub data: Option<PathBuf>,
}

impl AppDirectories {
    pub(crate) fn resolve() -> Self {
        Self {
            config: config_directory(),
            data: data_directory(),
        }
    }
}

pub(crate) fn config_directory() -> Option<PathBuf> {
    config_directory_from(
        std::env::var_os("XDG_CONFIG_HOME").as_deref(),
        std::env::var_os("HOME").as_deref(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn xdg_directory_precedes_the_home_fallback() {
        assert_eq!(
            config_directory_from(Some(OsStr::new("/xdg")), Some(OsStr::new("/home/me"))),
            Some(PathBuf::from("/xdg/rohditor"))
        );
        assert_eq!(
            config_directory_from(None, Some(OsStr::new("/home/me"))),
            Some(PathBuf::from("/home/me/.config/rohditor"))
        );
        assert_eq!(config_directory_from(None, None), None);
    }

    #[test]
    fn xdg_data_directory_precedes_the_home_fallback() {
        assert_eq!(
            data_directory_from(Some(OsStr::new("/data")), Some(OsStr::new("/home/me"))),
            Some(PathBuf::from("/data/rohditor"))
        );
        assert_eq!(
            data_directory_from(None, Some(OsStr::new("/home/me"))),
            Some(PathBuf::from("/home/me/.local/share/rohditor"))
        );
    }
}
