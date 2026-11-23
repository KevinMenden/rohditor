// Tests unwrap freely; a failure panics with a useful backtrace anyway.
#![allow(clippy::unwrap_used)]

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use rohditor_edit::{EditRecipe, RecipeSection, WhiteBalance};

use super::*;

static NEXT_DIRECTORY: AtomicU64 = AtomicU64::new(0);

struct TestDirectory(PathBuf);

impl TestDirectory {
    fn new() -> Self {
        let id = NEXT_DIRECTORY.fetch_add(1, Ordering::Relaxed);
        let path =
            std::env::temp_dir().join(format!("rohditor-presets-{}-{id}", std::process::id()));
        fs::create_dir(&path).expect("create test directory");
        Self(path)
    }

    fn path(&self) -> &Path {
        &self.0
    }

    fn preset_directory(&self) -> PathBuf {
        self.0.join("presets")
    }
}

impl Drop for TestDirectory {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

/// A recipe with asymmetric values in Tone, Presence and White balance.
fn edited() -> EditRecipe {
    let mut recipe = EditRecipe::default();
    recipe.light.exposure_ev = 1.0;
    recipe.light.contrast = 0.2;
    recipe.presence.clarity = 0.4;
    recipe.color.white_balance = WhiteBalance::TemperatureTint {
        temperature: 4800.0,
        tint: 0.12,
    };
    recipe
}

fn warm_look() -> Preset {
    Preset::new(
        "Warm look",
        &edited(),
        &[RecipeSection::Tone, RecipeSection::Presence],
    )
    .expect("valid preset")
}

#[test]
fn a_preset_keeps_only_the_chosen_sections() {
    let preset = warm_look();
    assert_eq!(
        preset.sections(),
        [RecipeSection::Tone, RecipeSection::Presence]
    );

    let applied = preset.apply(&EditRecipe::default(), &ApplyOptions::default());
    assert_eq!(applied.light.exposure_ev, 1.0);
    assert_eq!(applied.presence.clarity, 0.4);
    // White balance was in the source recipe but not in the preset.
    assert_eq!(applied.color.white_balance, WhiteBalance::default());
}

#[test]
fn sections_are_ordered_and_deduplicated_and_must_not_be_empty() {
    let preset = Preset::new(
        "x",
        &edited(),
        &[
            RecipeSection::Presence,
            RecipeSection::Tone,
            RecipeSection::Presence,
        ],
    )
    .unwrap();
    assert_eq!(
        preset.sections(),
        [RecipeSection::Tone, RecipeSection::Presence]
    );
    assert!(matches!(
        Preset::new("x", &edited(), &[]),
        Err(PresetError::NoSections)
    ));
}

#[test]
fn applying_leaves_other_sections_of_the_photo_alone() {
    let mut photo = EditRecipe::default();
    photo.color.saturation = 0.3;
    photo.light.exposure_ev = -0.5;

    let applied = warm_look().apply(&photo, &ApplyOptions::default());
    assert_eq!(applied.light.exposure_ev, 1.0);
    assert_eq!(applied.color.saturation, 0.3);
}

#[test]
fn applying_twice_at_full_amount_changes_nothing_more() {
    let preset = warm_look();
    let mut photo = EditRecipe::default();
    photo.light.exposure_ev = -0.5;
    photo.color.saturation = 0.3;

    let once = preset.apply(&photo, &ApplyOptions::default());
    let twice = preset.apply(&once, &ApplyOptions::default());
    assert_eq!(once, twice);
}

#[test]
fn amount_blends_towards_the_preset() {
    let preset = warm_look();
    let options = |amount| ApplyOptions {
        amount,
        ..ApplyOptions::default()
    };
    let photo = EditRecipe::default();

    assert_eq!(preset.apply(&photo, &options(0.0)), photo);
    let half = preset.apply(&photo, &options(0.5));
    assert!((half.light.exposure_ev - 0.5).abs() < 1e-6);
    assert!((half.presence.clarity - 0.2).abs() < 1e-6);
}

#[test]
fn chosen_sections_narrow_what_is_applied() {
    let preset = warm_look();
    let applied = preset.apply(
        &EditRecipe::default(),
        &ApplyOptions {
            // Presence is covered; Saturation is not and must be ignored.
            sections: Some(vec![RecipeSection::Presence, RecipeSection::Saturation]),
            ..ApplyOptions::default()
        },
    );
    assert_eq!(applied.presence.clarity, 0.4);
    assert_eq!(applied.light.exposure_ev, 0.0);
    assert_eq!(
        applied.color.saturation,
        EditRecipe::default().color.saturation
    );
}

#[test]
fn sections_the_photo_cannot_take_are_skipped() {
    // For example white balance on a photo that is already demosaiced.
    let preset = Preset::new(
        "Cool",
        &edited(),
        &[RecipeSection::Tone, RecipeSection::WhiteBalance],
    )
    .unwrap();
    let photo = EditRecipe::default();
    let applied = preset.apply(
        &photo,
        &ApplyOptions {
            unavailable: vec![RecipeSection::WhiteBalance],
            ..ApplyOptions::default()
        },
    );
    assert_eq!(applied.light.exposure_ev, 1.0);
    assert_eq!(applied.color.white_balance, photo.color.white_balance);
    assert!(applied.validate().is_ok());
}

#[test]
fn names_are_trimmed_and_checked() {
    let recipe = edited();
    let section = [RecipeSection::Tone];
    assert_eq!(
        Preset::new("  Moody  ", &recipe, &section).unwrap().name(),
        "Moody"
    );
    // 64 characters of 4 bytes each: within the character limit, but too long
    // for a file name.
    let wide = "𝒜".repeat(64);
    for bad in ["", "   ", "---", "a\nb", &"x".repeat(65), wide.as_str()] {
        assert!(
            matches!(
                Preset::new(bad, &recipe, &section),
                Err(PresetError::InvalidName(_))
            ),
            "{bad:?} should be rejected"
        );
    }
}

#[test]
fn an_invalid_recipe_cannot_become_a_preset() {
    let mut recipe = edited();
    recipe.light.exposure_ev = f32::NAN;
    assert!(matches!(
        Preset::new("x", &recipe, &[RecipeSection::Tone]),
        Err(PresetError::InvalidRecipe(_))
    ));
}

#[test]
fn saved_presets_survive_a_reload() {
    let directory = TestDirectory::new();
    let mut store = PresetStore::load(Some(directory.path()));
    assert!(store.presets().is_empty());

    store.save(warm_look()).unwrap();
    store
        .save(Preset::new("another", &edited(), &[RecipeSection::Saturation]).unwrap())
        .unwrap();

    let reloaded = PresetStore::load(Some(directory.path()));
    assert!(reloaded.warnings().is_empty());
    let names: Vec<_> = reloaded.presets().iter().map(Preset::name).collect();
    assert_eq!(names, ["another", "Warm look"]);
    assert_eq!(reloaded.get("warm LOOK"), Some(&warm_look()));
}

#[test]
fn a_duplicate_name_is_refused_and_leaves_the_first_preset() {
    let directory = TestDirectory::new();
    let mut store = PresetStore::load(Some(directory.path()));
    store.save(warm_look()).unwrap();

    let other = Preset::new("warm-look", &EditRecipe::default(), &[RecipeSection::Tone]).unwrap();
    assert!(matches!(store.save(other), Err(PresetError::NameTaken(_))));
    assert_eq!(store.presets().len(), 1);
    assert_eq!(store.get("Warm look"), Some(&warm_look()));
}

#[test]
fn delete_removes_the_file() {
    let directory = TestDirectory::new();
    let mut store = PresetStore::load(Some(directory.path()));
    store.save(warm_look()).unwrap();

    store.delete("Warm look").unwrap();
    assert!(store.presets().is_empty());
    assert!(
        PresetStore::load(Some(directory.path()))
            .presets()
            .is_empty()
    );
    assert!(matches!(
        store.delete("Warm look"),
        Err(PresetError::NotFound(_))
    ));
}

#[test]
fn rename_moves_the_file_and_refuses_taken_names() {
    let directory = TestDirectory::new();
    let mut store = PresetStore::load(Some(directory.path()));
    store.save(warm_look()).unwrap();
    store
        .save(Preset::new("Other", &edited(), &[RecipeSection::Tone]).unwrap())
        .unwrap();

    assert!(matches!(
        store.rename("Warm look", "other"),
        Err(PresetError::NameTaken(_))
    ));

    store.rename("Warm look", "Golden hour").unwrap();
    // Changing only the case keeps the same file.
    store.rename("Golden hour", "golden HOUR").unwrap();

    let reloaded = PresetStore::load(Some(directory.path()));
    assert!(reloaded.warnings().is_empty());
    let names: Vec<_> = reloaded.presets().iter().map(Preset::name).collect();
    assert_eq!(names, ["golden HOUR", "Other"]);
    assert_eq!(
        fs::read_dir(directory.preset_directory()).unwrap().count(),
        2
    );
}

#[test]
fn corrupt_files_are_reported_and_skipped() {
    let directory = TestDirectory::new();
    let mut store = PresetStore::load(Some(directory.path()));
    store.save(warm_look()).unwrap();
    let folder = directory.preset_directory();

    fs::write(folder.join("garbage.json"), b"{ not json").unwrap();
    // Valid JSON, but the file name does not follow from the name.
    let good = fs::read(folder.join("warm-look.json")).unwrap();
    fs::write(folder.join("copy.json"), &good).unwrap();
    // A recipe value outside its range.
    let invalid = String::from_utf8(good.clone())
        .unwrap()
        .replace("\"exposure_ev\": 1.0", "\"exposure_ev\": 99.0");
    assert_ne!(invalid.as_bytes(), good.as_slice());
    fs::write(folder.join("bad-value.json"), invalid).unwrap();
    // A format version this build does not know.
    let future = String::from_utf8(good)
        .unwrap()
        .replace("\"format_version\": 1", "\"format_version\": 99");
    fs::write(folder.join("future.json"), future).unwrap();
    // Not a preset file at all.
    fs::write(folder.join("notes.txt"), b"hello").unwrap();

    let reloaded = PresetStore::load(Some(directory.path()));
    let names: Vec<_> = reloaded.presets().iter().map(Preset::name).collect();
    assert_eq!(names, ["Warm look"]);
    assert_eq!(reloaded.warnings().len(), 4, "{:?}", reloaded.warnings());
}

#[test]
fn without_a_config_directory_the_store_is_empty_and_cannot_save() {
    let mut store = PresetStore::load(None);
    assert!(store.presets().is_empty());
    assert!(matches!(
        store.save(warm_look()),
        Err(PresetError::NoDirectory)
    ));
    assert!(store.presets().is_empty());
}

#[test]
fn a_failed_rename_keeps_the_old_name_and_leaves_one_file() {
    let directory = TestDirectory::new();
    let mut store = PresetStore::load(Some(directory.path()));
    store.save(warm_look()).unwrap();
    let folder = directory.preset_directory();

    // Make removing the old file fail: a non-empty directory cannot be
    // removed with `remove_file`, whoever runs the test.
    let old = folder.join("warm-look.json");
    fs::remove_file(&old).unwrap();
    fs::create_dir(&old).unwrap();
    fs::write(old.join("keep"), b"x").unwrap();

    assert!(matches!(
        store.rename("Warm look", "Golden hour"),
        Err(PresetError::Write { .. })
    ));
    assert!(store.get("Warm look").is_some());
    assert!(store.get("Golden hour").is_none());
    assert!(!folder.join("golden-hour.json").exists());
}
