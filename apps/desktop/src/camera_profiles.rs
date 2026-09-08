//! Installed matrix-only DCP profiles for the desktop application.

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

use rohditor_camera_profile::{DCP_MAX_FILE_BYTES, MatrixCameraProfile, parse_dcp_bytes};
use rohditor_raw::RawFileInfo;
use tracing::warn;

use crate::storage;

const PROFILE_DIRECTORY: &str = "camera-profiles";
const MAX_INSTALLED_PROFILES: usize = 256;
const MAX_SCAN_WARNINGS: usize = 4;

/// Validated profiles available to the desktop's current session.
#[derive(Debug, Default)]
pub(crate) struct CameraProfileRegistry {
    profiles: Vec<MatrixCameraProfile>,
    warning: Option<String>,
    warning_count: usize,
}

impl CameraProfileRegistry {
    pub(crate) fn load() -> Self {
        let mut registry = Self::default();
        let Some(directory) = profile_directory() else {
            return registry;
        };
        let mut paths = BTreeSet::new();
        let mut scan_was_truncated = false;
        let entries = match fs::read_dir(&directory) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return registry,
            Err(error) => {
                registry.add_warning(format!(
                    "Camera profile directory could not be scanned: {error}"
                ));
                return registry;
            }
        };
        for entry in entries {
            match entry {
                Ok(entry) if is_dcp_path(&entry.path()) => {
                    paths.insert(entry.path());
                    if paths.len() > MAX_INSTALLED_PROFILES {
                        let last = paths
                            .iter()
                            .next_back()
                            .cloned()
                            .expect("the set contains the inserted profile path");
                        paths.remove(&last);
                        scan_was_truncated = true;
                    }
                }
                Ok(_) => {}
                Err(error) => registry.add_warning(format!(
                    "A camera profile directory entry could not be read: {error}"
                )),
            }
        }
        if scan_was_truncated {
            registry.add_warning(format!(
                "Only the first {MAX_INSTALLED_PROFILES} sorted camera profiles were scanned"
            ));
        }
        for path in paths {
            match fs::metadata(&path).and_then(|metadata| {
                if metadata.len() > DCP_MAX_FILE_BYTES as u64 {
                    Err(std::io::Error::new(
                        std::io::ErrorKind::InvalidData,
                        format!("file exceeds the {DCP_MAX_FILE_BYTES}-byte limit"),
                    ))
                } else {
                    fs::read(&path)
                }
            }) {
                Ok(bytes) => match parse_dcp_bytes(&bytes) {
                    Ok(profile) => {
                        if path.file_stem().and_then(|stem| stem.to_str())
                            != Some(profile.source_sha256.as_str())
                        {
                            registry.add_warning(format!(
                                "Skipped {}: installed filename does not match its SHA-256",
                                path.display()
                            ));
                        } else if !registry
                            .profiles
                            .iter()
                            .any(|installed| installed.source_sha256 == profile.source_sha256)
                        {
                            registry.profiles.push(profile);
                        }
                    }
                    Err(error) => registry.add_warning(format!(
                        "Skipped camera profile {}: {error}",
                        path.display()
                    )),
                },
                Err(error) => registry.add_warning(format!(
                    "Skipped camera profile {}: {error}",
                    path.display()
                )),
            }
        }
        registry.sort_profiles();
        registry
    }

    pub(crate) fn warning(&self) -> Option<&str> {
        self.warning.as_deref()
    }

    pub(crate) fn compatible_profiles(&self, info: &RawFileInfo) -> Vec<MatrixCameraProfile> {
        self.profiles
            .iter()
            .filter(|profile| {
                profile.matches_camera(&info.make, &info.model, &info.clean_make, &info.clean_model)
            })
            .cloned()
            .collect()
    }

    /// Parse and, when possible, install a profile. A write failure is a
    /// warning rather than an import failure because the validated payload can
    /// still be embedded in the current recipe.
    pub(crate) fn import(
        &mut self,
        path: &Path,
        info: &RawFileInfo,
    ) -> Result<MatrixCameraProfile, String> {
        let bytes = read_profile_bytes(path)?;
        let profile = parse_dcp_bytes(&bytes)
            .map_err(|error| format!("Could not import {}: {error}", path.display()))?;
        if !profile.matches_camera(&info.make, &info.model, &info.clean_make, &info.clean_model) {
            return Err(format!(
                "Could not import {}: profile model {:?} does not match the open camera {} {}",
                path.display(),
                profile.camera_model,
                info.clean_make,
                info.clean_model
            ));
        }

        let already_installed = self
            .profiles
            .iter()
            .any(|installed| installed.source_sha256 == profile.source_sha256);
        if !already_installed {
            if self.profiles.len() >= MAX_INSTALLED_PROFILES {
                self.add_warning(format!(
                    "The camera profile registry is full; {} was used for this document but not installed",
                    profile.name
                ));
            } else {
                match profile_directory() {
                    Some(directory) => {
                        let destination = directory.join(format!("{}.dcp", profile.source_sha256));
                        if let Err(error) = storage::write_transactionally(&destination, &bytes) {
                            self.add_warning(format!(
                                "{} was imported for this document but could not be installed: {error}",
                                profile.name
                            ));
                        }
                    }
                    None => self.add_warning(format!(
                        "{} was imported for this document but no XDG configuration directory is available",
                        profile.name
                    )),
                }
                self.profiles.push(profile.clone());
                self.sort_profiles();
            }
        }
        Ok(profile)
    }

    fn sort_profiles(&mut self) {
        self.profiles.sort_unstable_by(|left, right| {
            left.name
                .cmp(&right.name)
                .then_with(|| left.source_sha256.cmp(&right.source_sha256))
        });
    }

    fn add_warning(&mut self, warning: String) {
        if self.warning_count >= MAX_SCAN_WARNINGS {
            return;
        }
        warn!(message = %warning, "camera profile registry warning");
        self.warning_count += 1;
        match &mut self.warning {
            Some(existing) => {
                existing.push_str("; ");
                existing.push_str(&warning);
            }
            None => self.warning = Some(warning),
        }
    }
}

fn profile_directory() -> Option<PathBuf> {
    storage::config_directory().map(|directory| directory.join(PROFILE_DIRECTORY))
}

fn is_dcp_path(path: &Path) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| extension.eq_ignore_ascii_case("dcp"))
}

fn read_profile_bytes(path: &Path) -> Result<Vec<u8>, String> {
    let metadata = fs::metadata(path)
        .map_err(|error| format!("Could not import {}: {error}", path.display()))?;
    if metadata.len() > DCP_MAX_FILE_BYTES as u64 {
        return Err(format!(
            "Could not import {}: file exceeds the {DCP_MAX_FILE_BYTES}-byte limit",
            path.display()
        ));
    }
    fs::read(path).map_err(|error| format!("Could not import {}: {error}", path.display()))
}
