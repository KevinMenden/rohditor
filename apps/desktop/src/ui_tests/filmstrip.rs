//! The filmstrip, most of it with files that are not photos.

use super::files::{open_photo, scratch_tree, selected};
use super::*;

/// A folder of many files.
fn scratch_many(name: &str, count: usize) -> PathBuf {
    let folder = std::env::temp_dir().join(format!("rohditor-desktop-filmstrip-{name}"));
    drop(std::fs::remove_dir_all(&folder));
    std::fs::create_dir_all(&folder).expect("scratch folder");
    for index in 0..count {
        std::fs::write(folder.join(format!("{index}.arw")), b"not a photo").expect("file");
    }
    folder
}

fn thumbnails(harness: &mut Harness) -> usize {
    let visible = harness.visible();
    let is_thumbnail = |(name, _): &&(String, Bounds<Pixels>)| name.starts_with("thumbnail:");
    visible.iter().filter(is_thumbnail).count()
}

/// Whether the whole thumbnail is inside the strip.
fn in_view(harness: &mut Harness, name: &str) -> bool {
    let strip = harness.find("filmstrip");
    harness.lookup(name).is_some_and(|thumbnail| {
        thumbnail.left() >= strip.left() && thumbnail.right() <= strip.right()
    })
}

#[test]
#[ignore = "requires a wgpu adapter"]
fn the_filmstrip_is_hidden_until_toggled_and_stepping_works_without_it() {
    let trip = scratch_tree("filmstrip-toggle");
    let mut harness = Harness::new();
    harness.open_folder(&trip);
    assert_eq!(harness.lookup("filmstrip"), None);
    harness.keys("right right");
    assert_eq!(open_photo(&mut harness), Some(trip.join("b.arw")));

    harness.keys("ctrl-j");
    for name in ["a", "b", "c"] {
        harness.find(&format!("thumbnail:{name}.arw"));
    }
    harness.keys("right");
    assert_eq!(open_photo(&mut harness), Some(trip.join("c.arw")));
    harness.click("thumbnail:a.arw");
    assert_eq!(open_photo(&mut harness), Some(trip.join("a.arw")));
    // The click left the focus where it was.
    harness.keys("right");
    assert_eq!(open_photo(&mut harness), Some(trip.join("b.arw")));

    harness.click("button:toggle-filmstrip");
    assert_eq!(harness.lookup("filmstrip"), None);
    harness.click("button:toggle-filmstrip");
    harness.click("button:close-filmstrip");
    assert_eq!(harness.lookup("filmstrip"), None);
}

#[test]
#[ignore = "requires a wgpu adapter"]
fn the_selection_is_shared_with_the_tree() {
    let trip = scratch_tree("filmstrip-selection");
    let mut harness = Harness::new();
    harness.open_folder(&trip);
    harness.keys("ctrl-j");
    harness.click_with("thumbnail:a.arw", Modifiers::control());
    assert_eq!(harness.lookup("selected:1"), None, "nothing to count");
    harness.click_with("thumbnail:c.arw", Modifiers::shift());
    assert_eq!(selected(&mut harness), ["a.arw", "b.arw", "c.arw"]);
    harness.find("selected:3");
    harness.click_with("file:b.arw", Modifiers::control());
    harness.find("selected:2");
    assert_eq!(open_photo(&mut harness), None, "nothing was opened");
    harness.keys("ctrl-a");
    harness.find("selected:3");
    harness.capture("filmstrip-selection");

    // Text inputs keep the key.
    harness.click_with("thumbnail:b.arw", Modifiers::control());
    harness.keys("ctrl-shift-p a ctrl-a escape");
    harness.find("selected:2");
    harness.right_click("thumbnail:a.arw");
    harness.draw();
    harness.find("menu:Copy Path");
}

#[test]
#[ignore = "requires a wgpu adapter"]
fn the_order_is_chosen_in_the_header_and_the_palette() {
    let trip = scratch_tree("filmstrip-sort");
    let mut harness = Harness::new();
    harness.open_folder(&trip);
    harness.keys("ctrl-j");
    let library = harness.library.clone();
    let sort = |harness: &mut Harness| harness.cx.update(|cx| library.read(cx).sort());
    harness.click("dropdown:sort");
    harness.draw();
    harness.click("menu:Capture date");
    assert_eq!(sort(&mut harness), LibrarySort::CaptureDate);
    harness.keys("ctrl-shift-p");
    for key in "sortbyfile".chars() {
        harness.keys(&key.to_string());
    }
    harness.keys("enter");
    assert_eq!(sort(&mut harness), LibrarySort::Filename);
}

#[test]
#[ignore = "requires a wgpu adapter"]
fn missing_photos_are_shown_but_not_opened() {
    let trip = scratch_tree("filmstrip-missing");
    let mut harness = Harness::new();
    let photos = vec![trip.join("gone.arw"), trip.join("a.arw")];
    let library = harness.library.clone();
    harness.cx.update(|cx| {
        library.update(cx, |library, cx| {
            library
                .create_album("Best", &photos, cx)
                .expect("a new album");
            library.show_album("Best", cx);
        })
    });
    harness.wait_for_library("the album", |library| library.len() == 2);
    harness.keys("ctrl-j");
    harness.click("thumbnail:gone.arw");
    assert_eq!(open_photo(&mut harness), None);
    harness.capture("filmstrip-missing");
    // Stepping skips it.
    harness.keys("right");
    assert_eq!(open_photo(&mut harness), Some(trip.join("a.arw")));
}

#[test]
#[ignore = "requires a wgpu adapter"]
fn an_empty_folder_says_so() {
    let folder = scratch_many("empty", 0);
    let mut harness = Harness::new();
    harness.keys("ctrl-j");
    harness.capture("filmstrip-nothing");
    let library = harness.library.clone();
    let path = folder.clone();
    harness
        .cx
        .update(|cx| library.update(cx, |library, cx| library.open_folder(path, cx)));
    harness.wait_for_library("the folder", |library| {
        library.folder() == Some(&folder) && !library.scanning()
    });
    harness.capture("filmstrip-empty");
    assert_eq!(thumbnails(&mut harness), 0);
}

#[test]
#[ignore = "requires a wgpu adapter"]
fn thousands_of_photos_scroll_and_the_open_one_is_followed() {
    let folder = scratch_many("many", 3000);
    let mut harness = Harness::new();
    harness.open_folder(&folder);
    harness.wait_for_library("the folder", |library| library.len() == 3000);
    let photo = folder.join("1500.arw");
    harness.with_session(|session, cx| session.open_file(photo, cx));
    harness.cx.run_until_parked();

    // Opening the strip scrolls to the open photo.
    harness.keys("ctrl-j");
    harness.draw();
    assert!(in_view(&mut harness, "thumbnail:1500.arw"));
    assert!(thumbnails(&mut harness) < 20, "only those on screen");

    // The wheel scrolls, and stays where it is while thumbnails arrive.
    let strip = harness.find("filmstrip").center();
    harness.scroll(strip, point(px(0.0), px(-126_000.0)));
    assert!(in_view(&mut harness, "thumbnail:2500.arw"));
    harness.scroll(strip, point(px(0.0), px(-9_000_000.0)));
    assert!(in_view(&mut harness, "thumbnail:2999.arw"));
    let library = harness.library.clone();
    harness.cx.update(|cx| library.update(cx, Library::tick));
    assert!(in_view(&mut harness, "thumbnail:2999.arw"));

    // Stepping scrolls back, with the next photo in view too.
    harness.keys("right");
    assert!(in_view(&mut harness, "thumbnail:1501.arw"));
    assert!(in_view(&mut harness, "thumbnail:1502.arw"));
    for _ in 0..15 {
        harness.keys("right");
    }
    assert!(in_view(&mut harness, "thumbnail:1517.arw"));

    // Another folder starts at its first photo.
    let start = Instant::now();
    for _ in 0..20 {
        harness.scroll(strip, point(px(0.0), px(-500.0)));
        harness.draw();
    }
    eprintln!("a scroll step takes {:?}", start.elapsed() / 20);
    let other = scratch_many("other", 100);
    harness.with_session(|session, cx| session.close_document(cx));
    harness.open_folder(&other);
    harness.draw();
    assert!(in_view(&mut harness, "thumbnail:0.arw"));
    harness.capture("filmstrip-many");
}
