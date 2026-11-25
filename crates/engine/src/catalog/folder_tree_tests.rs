use super::*;

/// `/p` holds `a/` (with `deep/` and `1.arw`), `b/` and `2.arw`.
fn listing(folder: &Path) -> FolderListing {
    let paths = |names: &[&str]| names.iter().map(|name| folder.join(name)).collect();
    let (folders, files): (&[&str], &[&str]) = match folder.to_str().expect("a plain path") {
        "/p" => (&["a", "b"], &["2.arw"]),
        "/p/a" => (&["deep"], &["1.arw"]),
        "/p/a/deep" => (&[], &["x.arw", "y.arw"]),
        _ => (&[], &[]),
    };
    FolderListing {
        folders: paths(folders),
        files: paths(files),
    }
}

/// Answer requests until there are none, and return what was asked for.
fn answer(tree: &mut FolderTree) -> Vec<PathBuf> {
    let mut answered = Vec::new();
    loop {
        let requests = tree.take_requests();
        if requests.is_empty() {
            return answered;
        }
        for folder in requests {
            tree.folder_listed(folder.clone(), Ok(listing(&folder)));
            answered.push(folder);
        }
    }
}

fn lines(tree: &FolderTree) -> Vec<String> {
    let line = |row: &TreeRow| format!("{}{}", " ".repeat(row.depth), row.name);
    tree.rows().iter().map(line).collect()
}

fn opened() -> FolderTree {
    let mut tree = FolderTree::default();
    tree.add_root("/p".into());
    answer(&mut tree);
    tree
}

#[test]
fn folders_are_listed_when_they_are_expanded() {
    let mut tree = FolderTree::new(["/p".into(), "/q".into()]);
    assert_eq!(answer(&mut tree), [] as [PathBuf; 0]);
    assert_eq!(lines(&tree), ["p", "q"]);

    tree.toggle(Path::new("/p"));
    assert_eq!(lines(&tree), ["p", "q"], "not listed yet");
    assert_eq!(answer(&mut tree), [PathBuf::from("/p")]);
    assert_eq!(lines(&tree), ["p", " a", " b", " 2.arw", "q"]);

    tree.expand(Path::new("/p/a"));
    answer(&mut tree);
    let photos: Vec<_> = tree.photos().collect();
    assert_eq!(photos, [Path::new("/p/a/1.arw"), Path::new("/p/2.arw")]);

    tree.toggle(Path::new("/p"));
    assert_eq!(lines(&tree), ["p", "q"]);
    // Subfolders stay expanded inside, and the folder is listed again.
    tree.toggle(Path::new("/p"));
    assert_eq!(
        lines(&tree),
        ["p", " a", "  deep", "  1.arw", " b", " 2.arw", "q"]
    );
    assert_eq!(answer(&mut tree), [PathBuf::from("/p")]);
}

#[test]
fn a_root_inside_another_is_shown_there() {
    let mut tree = opened();
    tree.add_root("/p/a".into());
    answer(&mut tree);
    assert_eq!(tree.roots(), [PathBuf::from("/p")]);
    assert!(tree.is_expanded(Path::new("/p/a")));
    assert_eq!(tree.cursor(), Some(Path::new("/p/a")));
    assert_eq!(tree.take_revealed(), Some(1));

    // Adding it again changes nothing.
    tree.add_root("/p".into());
    assert_eq!(tree.roots(), [PathBuf::from("/p")]);
    // A name that merely starts the same is another folder.
    tree.add_root("/p2".into());
    assert_eq!(tree.roots(), [PathBuf::from("/p"), PathBuf::from("/p2")]);
}

#[test]
fn a_root_around_others_takes_them_in() {
    let mut tree = FolderTree::new(["/p/a".into(), "/q".into(), "/p/b".into()]);
    tree.expand(Path::new("/p/a"));
    answer(&mut tree);
    tree.add_root("/p".into());
    answer(&mut tree);
    assert_eq!(tree.roots(), [PathBuf::from("/q"), PathBuf::from("/p")]);
    assert_eq!(
        lines(&tree),
        ["q", "p", " a", "  deep", "  1.arw", " b", " 2.arw"]
    );
    // Remembered roots may have been edited by hand.
    let tree = FolderTree::new(["/p/a".into(), "/p".into(), "/p".into()]);
    assert_eq!(tree.roots(), [PathBuf::from("/p")]);
}

#[test]
fn a_removed_root_is_forgotten_and_its_late_listing_ignored() {
    let mut tree = opened();
    tree.expand(Path::new("/p/a"));
    tree.remove_root(Path::new("/p"));
    assert!(tree.rows().is_empty());
    assert!(tree.take_requests().is_empty());
    assert_eq!(tree.cursor(), None);
    tree.folder_listed("/p/a".into(), Ok(listing(Path::new("/p/a"))));
    assert!(tree.rows().is_empty());

    tree.add_root("/p".into());
    answer(&mut tree);
    assert!(!tree.is_expanded(Path::new("/p/a")));
}

#[test]
fn revealing_lists_the_folders_on_the_way() {
    let mut tree = FolderTree::new(["/p".into()]);
    let photo = Path::new("/p/a/deep/y.arw");
    assert!(tree.reveal(photo));
    assert_eq!(tree.take_revealed(), None, "no row yet");
    answer(&mut tree);
    let row = tree.take_revealed().expect("the row exists");
    assert_eq!(tree.rows()[row].path, photo);
    assert_eq!(tree.take_revealed(), None, "scrolled to once");

    // Known folders are not listed again.
    assert!(tree.reveal(Path::new("/p/a/1.arw")));
    assert!(tree.take_requests().is_empty());
    assert!(tree.take_revealed().is_some());
    assert!(!tree.reveal(Path::new("/elsewhere/1.arw")));
}

#[test]
fn listings_may_arrive_in_any_order() {
    let mut tree = FolderTree::new(["/p".into()]);
    let photo = Path::new("/p/a/deep/y.arw");
    tree.reveal(photo);
    for folder in ["/p/a/deep", "/p/a"] {
        tree.folder_listed(folder.into(), Ok(listing(Path::new(folder))));
        // Not absent: the folders above are still to come.
        assert_eq!(tree.take_revealed(), None);
    }
    tree.folder_listed("/p".into(), Ok(listing(Path::new("/p"))));
    assert!(tree.take_revealed().is_some());
}

#[test]
fn a_new_photo_is_found_and_an_absent_one_given_up() {
    let mut tree = opened();
    let photo = Path::new("/p/new.arw");
    tree.reveal(photo);
    assert_eq!(tree.take_requests(), [PathBuf::from("/p")]);
    let mut newer = listing(Path::new("/p"));
    newer.files.push(photo.to_path_buf());
    tree.folder_listed("/p".into(), Ok(newer.clone()));
    assert!(tree.take_revealed().is_some());

    let absent = Path::new("/p/absent.dng");
    tree.reveal(absent);
    tree.folder_listed("/p".into(), Ok(newer.clone()));
    newer.files.push(absent.to_path_buf());
    tree.folder_listed("/p".into(), Ok(newer));
    assert_eq!(tree.take_revealed(), None, "does not scroll later");
}

#[test]
fn a_failed_listing_is_shown_until_it_works() {
    let mut tree = opened();
    tree.expand(Path::new("/p/a"));
    answer(&mut tree);
    let failure = |tree: &FolderTree| match &tree.rows()[0].kind {
        TreeRowKind::Folder { failure, .. } => failure.clone(),
        TreeRowKind::Photo => None,
    };
    tree.refresh();
    assert_eq!(
        tree.take_requests(),
        [PathBuf::from("/p"), PathBuf::from("/p/a")]
    );
    tree.folder_listed("/p".into(), Err("unplugged".to_owned()));
    assert_eq!(failure(&tree).as_deref(), Some("unplugged"));
    assert_eq!(lines(&tree), ["p"], "the root stays");

    tree.refresh();
    answer(&mut tree);
    assert_eq!(failure(&tree), None);
    assert_eq!(
        lines(&tree),
        ["p", " a", "  deep", "  1.arw", " b", " 2.arw"]
    );
}

#[test]
fn the_keyboard_walks_the_tree() {
    use TreeNavigation::{Down, End, Home, Left, Right, Up};
    let mut tree = FolderTree::new(["/p".into(), "/q".into()]);
    assert_eq!(FolderTree::default().navigate(Down), None);
    assert_eq!(tree.navigate(Down), Some(0));
    assert_eq!(tree.navigate(Up), Some(0));
    assert_eq!(tree.navigate(Right), Some(0), "expands");
    assert_eq!(tree.navigate(Right), Some(0), "nothing inside yet");
    answer(&mut tree);
    assert_eq!(tree.navigate(Right), Some(1), "goes inside");
    assert_eq!(tree.navigate(Left), Some(0), "back to the parent");
    assert_eq!(tree.navigate(End), Some(4));
    assert_eq!(tree.navigate(Down), Some(4));
    assert_eq!(tree.navigate(Up), Some(3));
    assert_eq!(tree.cursor(), Some(Path::new("/p/2.arw")));
    assert_eq!(tree.navigate(Right), Some(3), "a photo does not expand");
    assert_eq!(tree.navigate(Home), Some(0));
    assert_eq!(tree.navigate(Left), Some(0), "collapses");
    assert_eq!(lines(&tree), ["p", "q"]);
}

#[test]
fn the_cursor_moves_out_of_a_collapsed_folder() {
    let mut tree = opened();
    tree.reveal(Path::new("/p/a/deep/y.arw"));
    answer(&mut tree);
    tree.take_revealed();
    tree.set_cursor(Path::new("/p/a/deep/y.arw"));
    tree.collapse(Path::new("/p/a"));
    assert_eq!(tree.cursor(), Some(Path::new("/p/a")));
}
