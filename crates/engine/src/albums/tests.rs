use std::fs;
use std::sync::atomic::{AtomicU64, Ordering};

use super::*;

struct TestDirectory(PathBuf);

impl TestDirectory {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let id = NEXT.fetch_add(1, Ordering::Relaxed);
        let name = format!("rohditor-albums-{}-{id}", std::process::id());
        let path = std::env::temp_dir().join(name);
        fs::create_dir(&path).expect("create test directory");
        Self(path)
    }

    fn load(&self) -> AlbumStore {
        AlbumStore::load(Some(&self.0))
    }

    fn file(&self, name: &str) -> PathBuf {
        self.0.join("albums").join(name)
    }
}

impl Drop for TestDirectory {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn paths(names: &[&str]) -> Vec<PathBuf> {
    names.iter().map(PathBuf::from).collect()
}

fn album(name: &str, photos: &[&str]) -> Album {
    Album::new(name, &paths(photos)).expect("valid album")
}

fn photos(store: &AlbumStore, name: &str) -> Vec<PathBuf> {
    store.get(name).expect("the album").photos().to_vec()
}

#[test]
fn photos_keep_their_order_and_are_held_once() {
    let album = album(" Best ", &["/b/2.arw", "/a/1.arw", "/b/2.arw"]);
    assert_eq!(album.name(), "Best");
    assert_eq!(album.photos(), paths(&["/b/2.arw", "/a/1.arw"]));
    let source = album.source();
    assert_eq!(source.title(), "Best");
    assert!(source.contains(Path::new("/a/1.arw")));
}

#[test]
fn changes_survive_a_reload() {
    let directory = TestDirectory::new();
    let mut store = directory.load();
    store.save(album("Best", &["/b/2.arw"])).expect("save");
    store.save(album("Empty", &[])).expect("save");

    let more = paths(&["/a/1.arw", "/b/2.arw", "/c/3.arw"]);
    assert_eq!(store.add_photos("Best", &more).expect("add"), 2);
    assert_eq!(store.add_photos("Best", &more).expect("add"), 0);
    let expected = paths(&["/b/2.arw", "/a/1.arw", "/c/3.arw"]);
    assert_eq!(photos(&store, "Best"), expected);
    assert_eq!(photos(&directory.load(), "Best"), expected);

    let gone = paths(&["/a/1.arw", "/not/there.arw"]);
    assert_eq!(store.remove_photos("Best", &gone).expect("remove"), 1);
    assert_eq!(store.remove_photos("Best", &gone).expect("remove"), 0);
    let store = directory.load();
    assert_eq!(photos(&store, "Best"), paths(&["/b/2.arw", "/c/3.arw"]));
    let names: Vec<_> = store.albums().iter().map(Album::name).collect();
    assert_eq!(names, ["Best", "Empty"]);
    assert!(store.warnings().is_empty());
}

#[test]
fn renaming_and_deleting_never_touch_photos_or_their_edits() {
    let directory = TestDirectory::new();
    let photo = directory.0.join("a.arw");
    let edits = directory.0.join("a.arw.rohditor.json");
    for file in [&photo, &edits] {
        fs::write(file, b"kept").expect("write");
    }
    let mut store = directory.load();
    let best = Album::new("Best", std::slice::from_ref(&photo)).expect("valid album");
    store.save(best).expect("save");

    store.rename("Best", "Très bon").expect("rename");
    assert!(!directory.file("best.json").exists());
    assert_eq!(
        photos(&directory.load(), "Très bon"),
        std::slice::from_ref(&photo)
    );
    store
        .remove_photos("Très bon", std::slice::from_ref(&photo))
        .expect("remove");
    store.delete("Très bon").expect("delete");
    assert!(directory.load().albums().is_empty());
    for file in [&photo, &edits] {
        assert_eq!(fs::read(file).expect("still there"), b"kept");
    }
}

#[test]
fn names_are_unique_and_unknown_albums_are_errors() {
    let directory = TestDirectory::new();
    let mut store = directory.load();
    store.save(album("Best", &["/a/1.arw"])).expect("save");
    assert!(matches!(
        store.save(album("best!", &[])),
        Err(NamedFileError::NameTaken(_))
    ));
    assert!(matches!(
        store.add_photos("Other", &paths(&["/a/2.arw"])),
        Err(AlbumError::File(NamedFileError::NotFound(_)))
    ));
    assert!(matches!(
        Album::new("  ", &[]),
        Err(AlbumError::File(NamedFileError::InvalidName(_)))
    ));
    assert_eq!(photos(&directory.load(), "Best"), paths(&["/a/1.arw"]));
}

#[test]
fn a_photo_that_cannot_be_stored_changes_nothing() {
    let directory = TestDirectory::new();
    let mut store = directory.load();
    store.save(album("Best", &["/a/1.arw"])).expect("save");
    assert!(matches!(
        store.add_photos("Best", &paths(&["/a/2.arw", "relative.arw"])),
        Err(AlbumError::NotAbsolute(_))
    ));
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStringExt as _;
        let odd = PathBuf::from(std::ffi::OsString::from_vec(b"/\xff.arw".to_vec()));
        assert!(matches!(
            store.add_photos("Best", &[odd]),
            Err(AlbumError::NotUtf8(_))
        ));
    }
    assert_eq!(photos(&store, "Best"), paths(&["/a/1.arw"]));
    assert_eq!(photos(&directory.load(), "Best"), paths(&["/a/1.arw"]));
}

#[test]
fn a_failed_write_leaves_the_album_as_it_was() {
    let directory = TestDirectory::new();
    let mut store = directory.load();
    store.save(album("Best", &["/a/1.arw"])).expect("save");
    // A folder in the way of the file.
    fs::remove_file(directory.file("best.json")).expect("remove");
    fs::create_dir(directory.file("best.json")).expect("block");
    assert!(matches!(
        store.add_photos("Best", &paths(&["/a/2.arw"])),
        Err(AlbumError::File(NamedFileError::Write { .. }))
    ));
    assert_eq!(photos(&store, "Best"), paths(&["/a/1.arw"]));
}

#[test]
fn files_that_do_not_fit_are_reported_and_skipped() {
    let directory = TestDirectory::new();
    fs::create_dir(directory.0.join("albums")).expect("create folder");
    for (name, contents) in [
        ("broken.json", "not json"),
        (
            "newer.json",
            r#"{"format_version": 2, "name": "Newer", "photos": []}"#,
        ),
        (
            "relative.json",
            r#"{"format_version": 1, "name": "Relative", "photos": ["a.arw"]}"#,
        ),
        (
            "moved.json",
            r#"{"format_version": 1, "name": "Other", "photos": []}"#,
        ),
        (
            "good.json",
            r#"{"format_version": 1, "name": "Good", "photos": ["/a.arw"]}"#,
        ),
    ] {
        fs::write(directory.file(name), contents).expect("write");
    }
    let mut store = directory.load();
    let names: Vec<_> = store.albums().iter().map(Album::name).collect();
    assert_eq!(names, ["Good"]);
    assert_eq!(store.warnings().len(), 4);
    // A file that was skipped is not overwritten.
    assert!(matches!(
        store.save(album("Newer", &[])),
        Err(NamedFileError::FileExists(_))
    ));
}
