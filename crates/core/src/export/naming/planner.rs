//! Resolving photos to destination paths, one batch at a time.

use std::collections::HashSet;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use super::NamingError;
use super::sanitize::sanitize_component;
use super::template::{FileNameTemplate, PhotoNameInfo};
use crate::export::{CollisionPolicy, ExportFormat};

/// How many `-1`, `-2`, ... suffixes to try before giving up.
const MAX_RENAME_ATTEMPTS: u32 = 10_000;

/// The folder exports go to.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum ExportFolder {
    /// The folder of each source photo.
    #[default]
    NextToSource,
    Fixed(PathBuf),
}

/// A folder, optionally with a subfolder below it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ExportDestination {
    pub folder: ExportFolder,
    /// One folder name (no separators), created by whoever writes the files.
    pub subfolder: Option<String>,
}

/// Everything that decides an export's file name and location.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExportNaming {
    pub template: FileNameTemplate,
    pub destination: ExportDestination,
    pub collision: CollisionPolicy,
    /// Value of `{seq}` for the first photo of a batch.
    pub sequence_start: u64,
}

impl Default for ExportNaming {
    fn default() -> Self {
        Self {
            template: FileNameTemplate::default(),
            destination: ExportDestination::default(),
            collision: CollisionPolicy::default(),
            sequence_start: 1,
        }
    }
}

/// What to do with one photo of a batch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PlannedExport {
    /// Export to this path; it is free, or may be replaced under
    /// [`CollisionPolicy::Overwrite`].
    Write(PathBuf),
    /// The name is taken and the policy is [`CollisionPolicy::Skip`].
    Skip(PathBuf),
}

/// Plans the output paths of a batch, in the order the photos are given.
///
/// The planner remembers every path it handed out. Two photos that render to
/// the same name therefore never collide, even though nothing has been
/// written yet: the second one is renamed (or skipped, under
/// [`CollisionPolicy::Skip`]). Names are compared case-insensitively, since
/// the destination may be a case-insensitive filesystem.
///
/// The sequence number advances for every photo, including skipped ones, so a
/// photo's number depends only on its position in the batch.
#[derive(Debug)]
pub struct BatchPlanner {
    naming: ExportNaming,
    subfolder: Option<String>,
    extension: &'static str,
    next_sequence: u64,
    claimed: HashSet<String>,
}

impl BatchPlanner {
    /// Fails when the subfolder is not a single plain folder name.
    pub fn new(naming: ExportNaming, format: ExportFormat) -> Result<Self, NamingError> {
        let subfolder = naming
            .destination
            .subfolder
            .as_deref()
            .map(validate_subfolder)
            .transpose()?;
        Ok(Self {
            next_sequence: naming.sequence_start,
            naming,
            subfolder,
            extension: format.file_extension(),
            claimed: HashSet::new(),
        })
    }

    pub fn plan(&mut self, photo: &PhotoNameInfo) -> Result<PlannedExport, NamingError> {
        let sequence = self.next_sequence;
        self.next_sequence = sequence.saturating_add(1);

        let folder = self.folder_for(photo);
        let stem = self.naming.template.render(photo, sequence);
        let first_choice = self.path_for(&folder, &stem, None);

        match self.naming.collision {
            CollisionPolicy::Skip => {
                if self.is_taken(&first_choice, true)? {
                    Ok(PlannedExport::Skip(first_choice))
                } else {
                    Ok(self.claim(first_choice))
                }
            }
            // Existing files may be replaced, but never this batch's own output.
            CollisionPolicy::Overwrite => self.first_free(&folder, &stem, false),
            CollisionPolicy::AutoRename => self.first_free(&folder, &stem, true),
        }
    }

    fn folder_for(&self, photo: &PhotoNameInfo) -> PathBuf {
        let mut folder = match &self.naming.destination.folder {
            ExportFolder::NextToSource => photo
                .source
                .parent()
                .map(Path::to_path_buf)
                .unwrap_or_default(),
            ExportFolder::Fixed(path) => path.clone(),
        };
        if let Some(subfolder) = &self.subfolder {
            folder.push(subfolder);
        }
        folder
    }

    fn path_for(&self, folder: &Path, stem: &str, suffix: Option<u32>) -> PathBuf {
        let name = match suffix {
            None => format!("{stem}.{}", self.extension),
            Some(number) => format!("{stem}-{number}.{}", self.extension),
        };
        folder.join(name)
    }

    /// The first of `stem`, `stem-1`, `stem-2`, ... that is not taken.
    fn first_free(
        &mut self,
        folder: &Path,
        stem: &str,
        check_disk: bool,
    ) -> Result<PlannedExport, NamingError> {
        let first = self.path_for(folder, stem, None);
        if !self.is_taken(&first, check_disk)? {
            return Ok(self.claim(first));
        }
        for number in 1..=MAX_RENAME_ATTEMPTS {
            let candidate = self.path_for(folder, stem, Some(number));
            if !self.is_taken(&candidate, check_disk)? {
                return Ok(self.claim(candidate));
            }
        }
        Err(NamingError::NoFreeName {
            path: first,
            attempts: MAX_RENAME_ATTEMPTS,
        })
    }

    fn is_taken(&self, path: &Path, check_disk: bool) -> Result<bool, NamingError> {
        if self.claimed.contains(&claim_key(path)) {
            return Ok(true);
        }
        if !check_disk {
            return Ok(false);
        }
        match fs::symlink_metadata(path) {
            Ok(_) => Ok(true),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
            Err(source) => Err(NamingError::Inspect {
                path: path.to_owned(),
                source,
            }),
        }
    }

    fn claim(&mut self, path: PathBuf) -> PlannedExport {
        self.claimed.insert(claim_key(&path));
        PlannedExport::Write(path)
    }
}

fn claim_key(path: &Path) -> String {
    path.to_string_lossy().to_lowercase()
}

fn validate_subfolder(raw: &str) -> Result<String, NamingError> {
    let name = raw.trim();
    if name.is_empty() {
        return Err(NamingError::InvalidSubfolder {
            reason: "the name is empty",
        });
    }
    if name.contains(['/', '\\']) {
        return Err(NamingError::InvalidSubfolder {
            reason: "it must be a single folder name, not a path",
        });
    }
    if name.chars().all(|c| c == '.') {
        return Err(NamingError::InvalidSubfolder {
            reason: "'.' and '..' are not folder names",
        });
    }
    Ok(sanitize_component(name))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::export::PngBitDepth;

    const PNG: ExportFormat = ExportFormat::Png {
        bit_depth: PngBitDepth::Eight,
    };

    fn naming(template: &str, collision: CollisionPolicy) -> ExportNaming {
        ExportNaming {
            template: FileNameTemplate::parse(template).expect("template"),
            destination: ExportDestination {
                folder: ExportFolder::Fixed(PathBuf::from("/out")),
                subfolder: None,
            },
            collision,
            sequence_start: 1,
        }
    }

    fn photo(path: &str) -> PhotoNameInfo {
        PhotoNameInfo::from_path(Path::new(path))
    }

    fn plan_all(planner: &mut BatchPlanner, photos: &[&str]) -> Vec<PlannedExport> {
        photos
            .iter()
            .map(|path| planner.plan(&photo(path)).expect("plan"))
            .collect()
    }

    fn write(path: &str) -> PlannedExport {
        PlannedExport::Write(PathBuf::from(path))
    }

    fn skip(path: &str) -> PlannedExport {
        PlannedExport::Skip(PathBuf::from(path))
    }

    /// A directory under the system temp dir, removed on drop.
    struct TempDir(PathBuf);

    impl TempDir {
        fn new(label: &str) -> Self {
            let path = std::env::temp_dir()
                .join(format!("rohditor-naming-{label}-{}", std::process::id()));
            let _ = fs::remove_dir_all(&path);
            fs::create_dir_all(&path).expect("temp dir");
            Self(path)
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn sequence_counts_from_the_start_value_in_batch_order() {
        let mut settings = naming("{seq:3}-{name}", CollisionPolicy::Skip);
        settings.sequence_start = 40;
        let mut planner = BatchPlanner::new(settings, PNG).expect("planner");
        assert_eq!(
            plan_all(&mut planner, &["/a/one.ARW", "/b/two.ARW"]),
            [write("/out/040-one.png"), write("/out/041-two.png")]
        );
    }

    #[test]
    fn duplicates_in_a_batch_are_renamed_before_any_file_exists() {
        let mut planner =
            BatchPlanner::new(naming("{name}", CollisionPolicy::AutoRename), PNG).expect("planner");
        assert_eq!(
            plan_all(&mut planner, &["/a/IMG.ARW", "/b/IMG.ARW", "/c/img.ARW"]),
            [
                write("/out/IMG.png"),
                write("/out/IMG-1.png"),
                write("/out/img-2.png")
            ]
        );
    }

    #[test]
    fn overwrite_still_keeps_a_batch_from_replacing_its_own_output() {
        let mut planner =
            BatchPlanner::new(naming("{name}", CollisionPolicy::Overwrite), PNG).expect("planner");
        assert_eq!(
            plan_all(&mut planner, &["/a/IMG.ARW", "/b/IMG.ARW"]),
            [write("/out/IMG.png"), write("/out/IMG-1.png")]
        );
    }

    #[test]
    fn skip_drops_a_later_duplicate_but_keeps_counting() {
        let mut planner =
            BatchPlanner::new(naming("{name}", CollisionPolicy::Skip), PNG).expect("planner");
        assert_eq!(
            plan_all(&mut planner, &["/a/IMG.ARW", "/b/IMG.ARW", "/c/OTHER.ARW"]),
            [
                write("/out/IMG.png"),
                skip("/out/IMG.png"),
                write("/out/OTHER.png")
            ]
        );
    }

    #[test]
    fn existing_files_follow_the_policy() {
        let dir = TempDir::new("existing");
        fs::write(dir.0.join("IMG.png"), b"old").expect("fixture");
        fs::write(dir.0.join("IMG-1.png"), b"old").expect("fixture");
        let in_dir = |collision| ExportNaming {
            destination: ExportDestination {
                folder: ExportFolder::Fixed(dir.0.clone()),
                subfolder: None,
            },
            ..naming("{name}", collision)
        };
        let target = |name: &str| dir.0.join(name);

        let mut skip_planner =
            BatchPlanner::new(in_dir(CollisionPolicy::Skip), PNG).expect("planner");
        assert_eq!(
            plan_all(&mut skip_planner, &["/a/IMG.ARW"]),
            [PlannedExport::Skip(target("IMG.png"))]
        );

        let mut overwrite =
            BatchPlanner::new(in_dir(CollisionPolicy::Overwrite), PNG).expect("planner");
        assert_eq!(
            plan_all(&mut overwrite, &["/a/IMG.ARW"]),
            [PlannedExport::Write(target("IMG.png"))]
        );

        let mut rename =
            BatchPlanner::new(in_dir(CollisionPolicy::AutoRename), PNG).expect("planner");
        assert_eq!(
            plan_all(&mut rename, &["/a/IMG.ARW", "/a/NEW.ARW"]),
            [
                PlannedExport::Write(target("IMG-2.png")),
                PlannedExport::Write(target("NEW.png"))
            ]
        );
    }

    #[test]
    fn next_to_source_uses_each_photos_folder_and_the_format_extension() {
        let settings = ExportNaming {
            destination: ExportDestination::default(),
            ..naming("{name}-edited", CollisionPolicy::Skip)
        };
        let jpeg = ExportFormat::Jpeg { quality: 90 };
        let mut planner = BatchPlanner::new(settings, jpeg).expect("planner");
        assert_eq!(
            plan_all(&mut planner, &["/a/one.ARW", "/b/two.ARW", "three.ARW"]),
            [
                write("/a/one-edited.jpg"),
                write("/b/two-edited.jpg"),
                write("three-edited.jpg")
            ]
        );
    }

    #[test]
    fn subfolders_nest_below_the_destination() {
        let mut settings = naming("{name}", CollisionPolicy::Skip);
        settings.destination.subfolder = Some(" web: small ".to_owned());
        let mut planner = BatchPlanner::new(settings, PNG).expect("planner");
        assert_eq!(
            plan_all(&mut planner, &["/a/one.ARW"]),
            [write("/out/web_ small/one.png")]
        );
    }

    #[test]
    fn subfolders_must_be_a_single_plain_name() {
        for bad in ["", "  ", "..", ".", "a/b", "a\\b", "../up"] {
            let mut settings = naming("{name}", CollisionPolicy::Skip);
            settings.destination.subfolder = Some(bad.to_owned());
            assert!(
                matches!(
                    BatchPlanner::new(settings, PNG),
                    Err(NamingError::InvalidSubfolder { .. })
                ),
                "{bad:?}"
            );
        }
    }

    #[test]
    fn exhausting_the_rename_budget_is_an_error() {
        let mut planner =
            BatchPlanner::new(naming("same", CollisionPolicy::AutoRename), PNG).expect("planner");
        // Pretend every candidate was handed out already.
        planner
            .claimed
            .insert(claim_key(Path::new("/out/same.png")));
        for number in 1..=MAX_RENAME_ATTEMPTS {
            planner
                .claimed
                .insert(claim_key(Path::new(&format!("/out/same-{number}.png"))));
        }
        assert!(matches!(
            planner.plan(&photo("/a/x.ARW")),
            Err(NamingError::NoFreeName { .. })
        ));
    }
}
