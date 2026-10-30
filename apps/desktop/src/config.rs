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
}
