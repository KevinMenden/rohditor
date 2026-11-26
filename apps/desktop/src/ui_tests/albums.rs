//! The albums, with files that are not photos.

use super::files::{open_photo, scratch_tree, with_config};
use super::presets::type_text;
use super::*;

fn library<R>(harness: &mut Harness, read: impl FnOnce(&Library) -> R) -> R {
    let library = harness.library.clone();
    harness.cx.update(|cx| read(library.read(cx)))
}

fn photos_of(harness: &mut Harness, album: &str) -> Vec<String> {
    library(harness, |library| {
        let albums = library.albums();
        let album = albums.iter().find(|one| one.name() == album);
        let name = |photo: &PathBuf| {
            photo
                .file_name()
                .expect("a file name")
                .to_string_lossy()
                .into_owned()
        };
        album
            .expect("the album")
            .photos()
            .iter()
            .map(name)
            .collect()
    })
}

fn wait_for_count(harness: &mut Harness, count: usize) {
    harness.wait_for_library("the photos", |library| {
        !library.scanning() && library.len() == count
    });
    harness.draw();
}

#[test]
#[ignore = "requires a wgpu adapter"]
fn albums_are_filled_from_the_tree_and_the_filmstrip() {
    let trip = scratch_tree("albums");
    let mut harness = Harness::new();
    harness.open_folder(&trip);
    harness.keys("ctrl-j");

    // From the tree, several at once, into a new album.
    harness.click_with("file:a.arw", Modifiers::control());
    harness.click_with("file:c.arw", Modifiers::control());
    harness.right_click("file:c.arw");
    harness.click("menu:Add to Album…");
    harness.click("entry:New Album…");
    harness.keys("enter");
    harness.find("name-error");
    type_text(&mut harness, "best");
    harness.keys("enter");
    assert_eq!(photos_of(&mut harness, "best"), ["a.arw", "c.arw"]);

    // From the filmstrip. A photo outside of the selection replaces it.
    harness.right_click("thumbnail:b.arw");
    assert_eq!(library(&mut harness, Library::selected_count), 1);
    assert_eq!(harness.lookup("menu:Remove from Album"), None);
    harness.click("menu:Add to Album…");
    harness.keys("enter");
    harness.find("toast");
    assert_eq!(photos_of(&mut harness, "best"), ["a.arw", "c.arw", "b.arw"]);

    harness.click("tab:Albums");
    harness.click("album:best");
    harness.wait_for_library("the album", |library| {
        library.folder().is_none() && library.len() == 3
    });
    harness.draw();
    harness.capture("albums");
    harness.click_with("thumbnail:a.arw", Modifiers::control());
    harness.click_with("thumbnail:b.arw", Modifiers::control());
    harness.right_click("thumbnail:b.arw");
    harness.click("menu:Remove from Album");
    wait_for_count(&mut harness, 1);
    assert_eq!(photos_of(&mut harness, "best"), ["c.arw"]);
    assert_eq!(library(&mut harness, Library::selected_count), 0);
    assert!(trip.join("a.arw").exists());

    // Opening a photo of the album keeps showing it.
    harness.keys("right");
    assert_eq!(open_photo(&mut harness), Some(trip.join("c.arw")));
    assert!(library(&mut harness, |library| library
        .shown_album()
        .is_some()));
}

#[test]
#[ignore = "requires a wgpu adapter"]
fn albums_are_created_renamed_and_deleted() {
    let trip = scratch_tree("albums-manage");
    let mut harness = Harness::new();
    harness.click("tab:Albums");
    harness.capture("albums-none");
    harness.click("button:new-album");
    type_text(&mut harness, "one");
    harness.keys("enter");
    harness.click("button:add-album");
    type_text(&mut harness, "one");
    harness.keys("enter");
    harness.find("name-error");
    harness.capture("albums-form");
    type_text(&mut harness, "2");
    harness.keys("enter");
    harness.find("album:one2");

    harness.click("album:one");
    harness.right_click("album:one");
    harness.click("menu:Rename…");
    type_text(&mut harness, "first");
    harness.keys("enter");
    harness.find("album:first");
    let shown = |library: &Library| library.shown_album().map(str::to_owned);
    assert_eq!(library(&mut harness, shown).as_deref(), Some("first"));

    // Nothing is shown when the shown album goes and no photo is open.
    harness.right_click("album:first");
    harness.click("menu:Delete…");
    harness.click("button:accept");
    assert_eq!(harness.lookup("album:first"), None);
    assert_eq!(library(&mut harness, shown), None);
    assert!(library(&mut harness, |library| library.source().is_none()));

    // With one open it is its folder.
    let photo = trip.join("a.arw");
    let path = photo.clone();
    harness.click("album:one2");
    harness.with_session(|session, cx| session.open_file(path, cx));
    harness.click("album:one2");
    // By keyboard.
    let mut tabs = 0;
    while harness.lookup("button:accept").is_none() {
        harness.keys("tab down delete");
        tabs += 1;
        assert!(tabs < 30, "Tab reaches the albums");
    }
    harness.click("button:accept");
    harness.wait_for_library("the folder", |library| {
        library.folder() == photo.parent() && library.albums().is_empty()
    });
    assert!(photo.exists());
}

#[test]
#[ignore = "requires a wgpu adapter"]
fn the_shown_album_is_remembered() {
    let trip = scratch_tree("albums-remember");
    let config = scratch_config();
    let mut harness = with_config(&config);
    let entity = harness.library.clone();
    let photos = [trip.join("b.arw")];
    harness.cx.update(|cx| {
        entity.update(cx, |library, cx| {
            library
                .create_album("best", &photos, cx)
                .expect("a new album");
            library.show_album("best", cx);
        });
    });
    drop(harness);

    let mut harness = with_config(&config);
    let entity = harness.library.clone();
    harness.cx.update(|cx| entity.update(cx, Library::restore));
    harness.wait_for_library("the album", |library| {
        library.shown_album() == Some("best") && library.len() == 1
    });
}
