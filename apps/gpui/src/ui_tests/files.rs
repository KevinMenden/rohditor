//! The file tree. Most of it needs no real photos.

use super::*;

/// `Trip` with `a.arw`, `b.arw`, `c.arw` and the folder `Day` with `d.arw`, none of which are
/// photos.
pub(super) fn scratch_tree(name: &str) -> PathBuf {
    let folder = std::env::temp_dir().join(format!("rohditor-gpui-tree-{name}"));
    drop(std::fs::remove_dir_all(&folder));
    let trip = folder.join("Trip");
    std::fs::create_dir_all(trip.join("Day")).expect("scratch folders");
    for file in ["a.arw", "b.arw", "c.arw", "Day/d.arw"] {
        std::fs::write(trip.join(file), b"not a photo").expect("scratch file");
    }
    trip
}

pub(super) fn selected(harness: &mut Harness) -> Vec<String> {
    let library = harness.library.clone();
    let paths = harness.cx.update(|cx| library.read(cx).selected());
    let name = |path: &PathBuf| path.file_name().map(|name| name.to_string_lossy().into());
    paths.iter().filter_map(name).collect()
}

pub(super) fn open_photo(harness: &mut Harness) -> Option<PathBuf> {
    harness.with_session(|session, _| session.document().map(|open| open.path.clone()))
}

fn wait_for_row(harness: &mut Harness, path: &Path) {
    harness.wait_for_library("the row", |library| library.tree().row_of(path).is_some());
    harness.draw();
}

#[test]
#[ignore = "requires a wgpu adapter"]
fn folders_open_close_and_are_shown() {
    let trip = scratch_tree("folders");
    let mut harness = Harness::new();
    harness.find("button:open-folder");
    harness.open_folder(&trip);
    assert_eq!(harness.lookup("button:open-folder"), None);
    harness.find("file:a.arw");
    assert_eq!(harness.lookup("file:d.arw"), None);

    // The chevron opens without showing.
    let day = trip.join("Day");
    harness.click("chevron:Day");
    wait_for_row(&mut harness, &day.join("d.arw"));
    harness.find("file:d.arw");
    harness.cx.run_until_parked();
    let library = harness.library.clone();
    let shown = |harness: &mut Harness| {
        let library = library.clone();
        harness
            .cx
            .update(|cx| library.read(cx).folder().map(Path::to_path_buf))
    };
    assert_eq!(shown(&mut harness), Some(trip.clone()));
    // The first click shows it, the second closes it.
    harness.click("folder:Day");
    harness.wait_for_library("the other folder", |library| library.folder() == Some(&day));
    harness.draw();
    harness.find("file:d.arw");
    harness.click("folder:Day");
    assert_eq!(harness.lookup("file:d.arw"), None);
    harness.capture("file-tree");

    // A new file appears on a refresh.
    std::fs::write(trip.join("new.arw"), b"").expect("scratch file");
    harness.keys("f5");
    wait_for_row(&mut harness, &trip.join("new.arw"));
    harness.find("file:new.arw");

    harness.right_click("folder:Trip");
    harness.draw();
    harness.click("menu:Remove from Files");
    harness.find("button:open-folder");
    assert!(trip.join("a.arw").exists());
}

#[test]
#[ignore = "requires a wgpu adapter"]
fn photos_are_selected_with_ctrl_and_shift() {
    let trip = scratch_tree("selection");
    let mut harness = Harness::new();
    harness.open_folder(&trip);
    let ctrl = Modifiers::control();
    let shift = Modifiers::shift();
    harness.click_with("file:a.arw", ctrl);
    harness.click_with("file:c.arw", ctrl);
    assert_eq!(selected(&mut harness), ["a.arw", "c.arw"]);
    harness.click_with("file:a.arw", ctrl);
    assert_eq!(selected(&mut harness), ["c.arw"]);
    harness.click_with("file:c.arw", ctrl);
    harness.click_with("file:c.arw", ctrl);
    harness.click_with("file:a.arw", shift);
    assert_eq!(selected(&mut harness), ["a.arw", "b.arw", "c.arw"]);
    harness.capture("file-tree-selection");
    assert_eq!(open_photo(&mut harness), None, "nothing was opened");

    harness.click("file:b.arw");
    assert_eq!(selected(&mut harness), ["b.arw"]);
    assert_eq!(open_photo(&mut harness), Some(trip.join("b.arw")));
    // The click left the focus where it was.
    harness.keys("right");
    assert_eq!(open_photo(&mut harness), Some(trip.join("c.arw")));
    assert_eq!(selected(&mut harness), ["c.arw"]);
}

#[test]
#[ignore = "requires a wgpu adapter"]
fn the_tree_works_by_keyboard() {
    let trip = scratch_tree("keyboard");
    let mut harness = Harness::new();
    harness.open_folder(&trip);
    let library = harness.library.clone();
    // Tab goes through the title bar's buttons first.
    let mut tabs = 0;
    loop {
        harness.keys("tab home");
        let cursor = |cx: &mut gpui::App| library.read(cx).tree().cursor().map(Path::to_path_buf);
        harness.keys("down");
        if harness.cx.update(cursor) == Some(trip.join("Day")) {
            break;
        }
        tabs += 1;
        assert!(tabs < 20, "Tab reaches the tree");
    }
    harness.keys("right");
    wait_for_row(&mut harness, &trip.join("Day/d.arw"));
    harness.keys("right");
    harness.capture("file-tree-keyboard");
    harness.keys("enter");
    assert_eq!(open_photo(&mut harness), Some(trip.join("Day/d.arw")));
    harness.keys("left left");
    assert_eq!(harness.lookup("file:d.arw"), None, "closed");
    harness.keys("end enter");
    assert_eq!(open_photo(&mut harness), Some(trip.join("c.arw")));
}

#[test]
#[ignore = "requires a wgpu adapter"]
fn an_opened_photo_is_revealed_and_its_folder_shown() {
    let trip = scratch_tree("reveal");
    let mut harness = Harness::new();
    harness.open_folder(&trip);
    let photo = trip.join("Day/d.arw");
    let path = photo.clone();
    harness.with_session(|session, cx| session.open_file(path, cx));
    wait_for_row(&mut harness, &photo);
    harness.find("file:d.arw");
    harness.wait_for_library("its folder", |library| {
        library.folder() == Some(&trip.join("Day"))
    });

    // One outside of the roots shows its folder, and adds no root.
    let other = scratch_tree("reveal-other");
    let path = other.join("a.arw");
    harness.with_session(|session, cx| session.open_file(path, cx));
    harness.wait_for_library("the other folder", |library| {
        library.folder() == Some(&other)
    });
    let library = harness.library.clone();
    let roots = harness
        .cx
        .update(|cx| library.read(cx).tree().roots().to_vec());
    assert_eq!(roots, [trip]);
}

#[test]
#[ignore = "requires a wgpu adapter"]
fn roots_and_the_shown_folder_are_remembered() {
    let trip = scratch_tree("remember");
    let config = scratch_config();
    let start = |config: &Path| {
        let mut harness = Harness::new();
        let session = harness.session.clone();
        let config = Some(config.to_path_buf());
        harness.library = harness.cx.update(|cx| {
            cx.new(|cx| {
                let worker = LibraryWorker::start().expect("the catalog starts");
                Library::new(worker, config, &session, cx)
            })
        });
        harness
    };
    let mut harness = start(&config);
    harness.open_folder(&trip);
    let library = harness.library.clone();
    let day = trip.join("Day");
    let folder = day.clone();
    harness
        .cx
        .update(|cx| library.update(cx, |library, cx| library.click_folder(folder, cx)));
    drop(harness);

    let mut harness = start(&config);
    let library = harness.library.clone();
    harness.cx.update(|cx| library.update(cx, Library::restore));
    harness.wait_for_library("the folder again", |library| {
        library.folder() == Some(&day) && library.tree().row_of(&day.join("d.arw")).is_some()
    });
    let roots = harness
        .cx
        .update(|cx| library.read(cx).tree().roots().to_vec());
    assert_eq!(roots, [trip]);
}
