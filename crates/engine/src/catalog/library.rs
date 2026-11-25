//! Toolkit-free state of the photos the library shows.
//!
//! A frontend owns the widgets; this holds what a second frontend would
//! otherwise duplicate: the display order of the catalog, stepping through it,
//! the selection, and the policy of which thumbnails to decode, how
//! many per frame and how many to keep resident. The decoded thumbnail is
//! handed to the frontend to turn into its own texture type `T`.

use std::cmp::Ordering;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

use rohditor_raw::SourceIdentity;
use tracing::warn;

use super::{CatalogSource, CatalogState, PhotoSelection, ThumbnailSlot};

/// Longest texture edge; thumbnails decode down to this size.
pub const TEXTURE_LONG_EDGE: u32 = 256;
/// Maximum resident textures before the least recently used are evicted.
pub const TEXTURE_CAP: usize = 256;
/// Textures decoded per frame, so a folder fill never blocks a frame.
pub const DECODE_BUDGET: usize = 8;
/// Extra entries around the visible range kept warm for smooth scrolling.
pub const VISIBLE_MARGIN: usize = 24;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum LibrarySort {
    /// Scan order, which is natural order by filename.
    #[default]
    Filename,
    CaptureDate,
}

impl LibrarySort {
    pub const ALL: [Self; 2] = [Self::Filename, Self::CaptureDate];
}

/// A thumbnail decoded and reduced to at most [`TEXTURE_LONG_EDGE`].
pub struct DecodedThumbnail {
    pub width: usize,
    pub height: usize,
    /// Packed RGB8, row-major.
    pub rgb: Vec<u8>,
}

/// Selection, display order and resident thumbnail textures.
pub struct LibraryState<T> {
    /// The source and its photos as of the last [`Self::sync`].
    source: Option<CatalogSource>,
    revision: u64,
    photos: Vec<PathBuf>,
    pub selection: PhotoSelection,
    /// Display order for the current sort, mapping positions to catalog
    /// entry indices.
    order: Vec<usize>,
    pub sort: LibrarySort,
    textures: HashMap<PathBuf, Texture<T>>,
    /// Versions of photos whose thumbnail bytes could not be decoded, so
    /// failures are not retried every frame.
    failures: HashMap<PathBuf, SourceIdentity>,
    /// Positions that intersected the viewport last frame.
    pub visible: (usize, usize),
    frame: u64,
    decode_pending: bool,
}

struct Texture<T> {
    texture: T,
    /// The version of the photo it shows.
    identity: SourceIdentity,
    last_used: u64,
}

impl<T> Default for LibraryState<T> {
    fn default() -> Self {
        Self {
            source: None,
            revision: 0,
            photos: Vec::new(),
            selection: PhotoSelection::default(),
            order: Vec::new(),
            sort: LibrarySort::default(),
            textures: HashMap::new(),
            failures: HashMap::new(),
            visible: (0, VISIBLE_MARGIN),
            frame: 0,
            decode_pending: false,
        }
    }
}

impl<T> LibraryState<T> {
    /// Count a frame; texture recency is measured in frames.
    pub fn begin_frame(&mut self) {
        self.frame = self.frame.wrapping_add(1);
    }

    /// Follow the catalog: another source resets the selection to the open
    /// photo, photos that went away leave it, and the display order is
    /// recomputed.
    pub fn sync(&mut self, catalog: &CatalogState) {
        let source = catalog.source();
        let same = match (&self.source, source) {
            (Some(before), Some(now)) => before.is_same_place(now),
            (before, now) => before.is_none() && now.is_none(),
        };
        if !same {
            self.source = source.cloned();
            self.photos.clear();
            self.selection.reset();
            self.textures.clear();
            self.failures.clear();
        }
        if catalog.revision() != self.revision {
            self.revision = catalog.revision();
            for photo in &self.photos {
                let gone = catalog.entry_index_for_path(photo).is_none();
                if gone && self.selection.open() != Some(photo) {
                    self.selection.remove(photo);
                }
            }
            self.photos = catalog.paths().map(Path::to_path_buf).collect();
        }
        self.refresh_order(catalog);
    }

    /// Recompute the display order for the current sort.
    pub fn refresh_order(&mut self, catalog: &CatalogState) {
        self.order = sorted_indices(catalog.entry_count(), self.sort, |index| {
            catalog.capture_date(index)
        });
    }

    /// Catalog entry indices in display order.
    pub fn order(&self) -> &[usize] {
        &self.order
    }

    /// The catalog entry shown at a position.
    pub fn catalog_index(&self, position: usize) -> Option<usize> {
        self.order.get(position).copied()
    }

    /// The position of a catalog entry.
    pub fn position(&self, catalog_index: usize) -> Option<usize> {
        self.order.iter().position(|&index| index == catalog_index)
    }

    /// The photos in display order.
    pub fn paths<'a>(&'a self, catalog: &'a CatalogState) -> impl Iterator<Item = &'a Path> {
        self.order
            .iter()
            .filter_map(|&index| catalog.entry_path(index))
    }

    /// The position of the photo shown before or after the one at
    /// `active_catalog_index`, stepping in display order (which differs from
    /// catalog order when sorted by capture date) and over missing files.
    /// `None` at the ends or when nothing is active.
    pub fn adjacent_position(
        &self,
        catalog: &CatalogState,
        active_catalog_index: Option<usize>,
        forward: bool,
    ) -> Option<usize> {
        let active = self.position(active_catalog_index?)?;
        let is_there = |&position: &usize| !catalog.is_missing(self.order[position]);
        if forward {
            (active + 1..self.order.len()).find(is_there)
        } else {
            (0..active).rev().find(is_there)
        }
    }

    /// The photo after or before `current`, or the first one if `current` is
    /// not shown. Missing files are skipped.
    pub fn adjacent_path<'a>(
        &self,
        catalog: &'a CatalogState,
        current: Option<&Path>,
        forward: bool,
    ) -> Option<&'a Path> {
        let active = current.and_then(|path| catalog.entry_index_for_path(path));
        let index = match active {
            None => self
                .order
                .iter()
                .copied()
                .find(|&index| !catalog.is_missing(index))?,
            active => self.catalog_index(self.adjacent_position(catalog, active, forward)?)?,
        };
        catalog.entry_path(index)
    }

    /// Select every photo shown.
    pub fn select_all(&mut self, catalog: &CatalogState) {
        self.selection.select_all(catalog.paths());
    }

    /// Select the photos shown between the anchor and `photo`.
    pub fn select_range(&mut self, catalog: &CatalogState, photo: &Path) {
        let order = self
            .order
            .iter()
            .filter_map(|&index| catalog.entry_path(index));
        self.selection.select_range(photo, order);
    }

    /// Whether more thumbnails are waiting for a later frame's decode budget.
    pub const fn decode_pending(&self) -> bool {
        self.decode_pending
    }

    pub fn texture(&self, path: &Path) -> Option<&T> {
        self.textures.get(path).map(|entry| &entry.texture)
    }

    /// Decode thumbnails for positions around `range`, at most
    /// [`DECODE_BUDGET`] per call, and keep at most [`TEXTURE_CAP`] resident.
    /// `upload` turns a decoded thumbnail into the frontend's texture.
    pub fn fill_textures(
        &mut self,
        catalog: &CatalogState,
        (start, end): (usize, usize),
        mut upload: impl FnMut(&Path, DecodedThumbnail) -> T,
    ) {
        self.decode_pending = false;
        let count = self.order.len();
        if count == 0 {
            return;
        }
        let start = start.saturating_sub(VISIBLE_MARGIN).min(count);
        let end = end.saturating_add(VISIBLE_MARGIN).min(count);
        let mut budget = DECODE_BUDGET;
        for position in start..end {
            let Some(catalog_index) = self.order.get(position).copied() else {
                continue;
            };
            let (Some(path), Some(identity), Some(ThumbnailSlot::Ready(thumbnail))) = (
                catalog.entry_path(catalog_index),
                catalog.identity(catalog_index),
                catalog.slot(catalog_index),
            ) else {
                continue;
            };
            if let Some(entry) = self.textures.get_mut(path)
                && entry.identity == identity
            {
                entry.last_used = self.frame;
                continue;
            }
            if budget == 0 {
                self.decode_pending = true;
                continue;
            }
            if self.failures.get(path) == Some(&identity) {
                continue;
            }
            match decode_thumbnail(thumbnail.bytes()) {
                Ok(decoded) => {
                    let entry = Texture {
                        texture: upload(path, decoded),
                        identity,
                        last_used: self.frame,
                    };
                    self.textures.insert(path.to_path_buf(), entry);
                    budget -= 1;
                }
                // A failed decode spends no budget: it is remembered, so each
                // corrupt thumbnail costs one attempt in total, not one per frame.
                Err(error) => {
                    warn!(
                        %error,
                        path = %path.display(),
                        "library thumbnail decoding failed"
                    );
                    self.failures.insert(path.to_path_buf(), identity);
                }
            }
        }
        evict_least_recently_used(&mut self.textures, TEXTURE_CAP);
    }
}

/// Display order of catalog entries: scan order for filename sort, or
/// capture-date order with undated entries last and ties in scan order.
pub fn sorted_indices<'a>(
    count: usize,
    sort: LibrarySort,
    capture_date: impl Fn(usize) -> Option<&'a str>,
) -> Vec<usize> {
    let mut order: Vec<usize> = (0..count).collect();
    if sort == LibrarySort::CaptureDate {
        // Stable, so ties keep the scan order.
        order.sort_by(
            |&left, &right| match (capture_date(left), capture_date(right)) {
                (Some(left), Some(right)) => left.cmp(right),
                (Some(_), None) => Ordering::Less,
                (None, Some(_)) => Ordering::Greater,
                (None, None) => Ordering::Equal,
            },
        );
    }
    order
}

/// Decode encoded thumbnail bytes into a bounded texture image.
pub fn decode_thumbnail(bytes: &[u8]) -> Result<DecodedThumbnail, String> {
    let decoded = image::load_from_memory_with_format(bytes, image::ImageFormat::Jpeg)
        .map_err(|error| format!("thumbnail JPEG decoding failed: {error}"))?
        .to_rgb8();
    let (width, height) = decoded.dimensions();
    let long_edge = width.max(height);
    let resized = if long_edge <= TEXTURE_LONG_EDGE {
        decoded
    } else {
        let scale = f64::from(TEXTURE_LONG_EDGE) / f64::from(long_edge);
        let target_width = ((f64::from(width) * scale).round() as u32).max(1);
        let target_height = ((f64::from(height) * scale).round() as u32).max(1);
        image::imageops::resize(
            &decoded,
            target_width,
            target_height,
            image::imageops::FilterType::Triangle,
        )
    };
    let (width, height) = resized.dimensions();
    Ok(DecodedThumbnail {
        width: width as usize,
        height: height as usize,
        rgb: resized.into_raw(),
    })
}

/// Evict least-recently-used textures until the map fits within `cap`.
fn evict_least_recently_used<T>(textures: &mut HashMap<PathBuf, Texture<T>>, cap: usize) {
    if textures.len() <= cap {
        return;
    }
    let mut order: Vec<(u64, PathBuf)> = textures
        .iter()
        .map(|(path, entry)| (entry.last_used, path.clone()))
        .collect();
    order.sort_unstable();
    let evict = order.len() - cap;
    for (_, path) in order.into_iter().take(evict) {
        textures.remove(&path);
    }
}

#[cfg(test)]
mod tests {
    use image::{Rgb, RgbImage};
    use rohditor_catalog::{ScannedFile, Thumbnail, scan_files, scan_folder};

    use super::*;
    use crate::catalog::test_directory::TestDirectory;
    use crate::catalog::{CatalogEvent, ThumbnailResult};

    const IDENTITY: SourceIdentity = SourceIdentity {
        size_bytes: 1,
        modified_unix_nanos: None,
        filesystem_device: None,
        filesystem_inode: None,
    };

    /// Shows files as they are on disk.
    fn show(
        catalog: &mut CatalogState,
        library: &mut LibraryState<u32>,
        generation: u64,
        source: CatalogSource,
    ) {
        let files = match &source {
            CatalogSource::Folder(folder) => scan_folder(folder)
                .expect("scan test folder")
                .into_iter()
                .map(ScannedFile::Found)
                .collect(),
            CatalogSource::Album { photos, .. } => scan_files(photos),
        };
        catalog.apply_event(CatalogEvent::ScanStarted { generation, source });
        library.sync(catalog);
        catalog.apply_event(CatalogEvent::ScanEntries { generation, files });
        library.sync(catalog);
    }

    fn resolve(
        catalog: &mut CatalogState,
        generation: u64,
        path: &Path,
        outcome: ThumbnailResult,
        date: Option<&str>,
    ) {
        let index = catalog.entry_index_for_path(path).expect("shown");
        catalog.apply_event(CatalogEvent::ThumbnailReady {
            generation,
            path: path.to_path_buf(),
            identity: catalog.identity(index).expect("not missing"),
            outcome,
            captured_at: date.map(str::to_owned),
        });
    }

    fn failed() -> ThumbnailResult {
        ThumbnailResult::Failed("not a photo".to_owned())
    }

    fn album(photos: &[&Path]) -> CatalogSource {
        CatalogSource::Album {
            name: "Best".to_owned(),
            photos: photos.iter().map(|path| path.to_path_buf()).collect(),
        }
    }

    fn folder(directory: &TestDirectory) -> CatalogSource {
        CatalogSource::Folder(directory.path().to_path_buf())
    }

    #[test]
    fn stepping_follows_display_order_and_stops_at_the_ends() {
        let directory = TestDirectory::new();
        let [a, b, c] = ["a.arw", "b.arw", "c.arw"].map(|name| directory.create_arw(name));
        let (mut catalog, mut library) = (CatalogState::default(), LibraryState::default());
        show(&mut catalog, &mut library, 1, folder(&directory));
        for (path, date) in [(&a, "2024:02"), (&b, "2024:03"), (&c, "2024:01")] {
            resolve(&mut catalog, 1, path, failed(), Some(date));
        }
        library.sort = LibrarySort::CaptureDate;
        library.sync(&catalog);

        let step = |from: Option<&Path>, forward| library.adjacent_path(&catalog, from, forward);
        assert_eq!(library.order(), [2, 0, 1]);
        assert_eq!(step(Some(&c), true), Some(a.as_path()));
        assert_eq!(step(Some(&b), false), Some(a.as_path()));
        assert_eq!(step(Some(&c), false), None);
        assert_eq!(step(Some(&b), true), None);
        assert_eq!(step(None, true), Some(c.as_path()));
        assert_eq!(
            step(Some(Path::new("/other.arw")), false),
            Some(c.as_path())
        );
        assert_eq!(library.adjacent_position(&catalog, Some(0), true), Some(2));
        assert_eq!(library.adjacent_position(&catalog, None, true), None);
    }

    #[test]
    fn stepping_skips_missing_photos() {
        let directory = TestDirectory::new();
        let gone = |name: &str| directory.path().join(name);
        let (b, d) = (directory.create_arw("b.arw"), directory.create_arw("d.arw"));
        let (a, c, e) = (gone("a.arw"), gone("c.arw"), gone("e.arw"));
        let (mut catalog, mut library) = (CatalogState::default(), LibraryState::default());
        show(&mut catalog, &mut library, 1, album(&[&a, &b, &c, &d, &e]));

        let step = |from: Option<&Path>, forward| library.adjacent_path(&catalog, from, forward);
        assert_eq!(step(None, true), Some(b.as_path()));
        assert_eq!(step(Some(&b), true), Some(d.as_path()));
        assert_eq!(step(Some(&d), false), Some(b.as_path()));
        assert_eq!(step(Some(&d), true), None);
        assert_eq!(step(Some(&b), false), None);
    }

    #[test]
    fn the_selection_survives_sorting_thumbnails_and_rescans() {
        let directory = TestDirectory::new();
        let [a, b, c] = ["a.arw", "b.arw", "c.arw"].map(|name| directory.create_arw(name));
        let (mut catalog, mut library) = (CatalogState::default(), LibraryState::default());
        show(&mut catalog, &mut library, 1, folder(&directory));
        library.selection.select_only(&b);
        library.selection.toggle(&c);

        resolve(&mut catalog, 1, &c, failed(), Some("2024:01"));
        library.sort = LibrarySort::CaptureDate;
        library.sync(&catalog);
        directory.create_arw("0.arw");
        show(&mut catalog, &mut library, 2, folder(&directory));
        assert_eq!(
            library.selection.in_order(library.paths(&catalog)),
            [c.clone(), b.clone()]
        );

        // A photo that is gone is not selected any more.
        std::fs::remove_file(&b).expect("delete a photo");
        show(&mut catalog, &mut library, 3, folder(&directory));
        assert_eq!(library.selection.in_order(library.paths(&catalog)), [c]);

        library.select_all(&catalog);
        assert_eq!(library.selection.len(), 3);
        assert!(library.selection.contains(&a));
    }

    #[test]
    fn ranges_follow_the_display_order() {
        let directory = TestDirectory::new();
        let [a, b, c] = ["a.arw", "b.arw", "c.arw"].map(|name| directory.create_arw(name));
        let (mut catalog, mut library) = (CatalogState::default(), LibraryState::default());
        show(&mut catalog, &mut library, 1, folder(&directory));
        resolve(&mut catalog, 1, &c, failed(), Some("2024:01"));
        library.sort = LibrarySort::CaptureDate;
        library.sync(&catalog);

        library.selection.select_only(&c);
        library.select_range(&catalog, &a);
        assert_eq!(library.selection.in_order(library.paths(&catalog)), [c, a]);
        assert!(!library.selection.contains(&b));
    }

    #[test]
    fn another_source_resets_the_selection_to_the_open_photo() {
        let directory = TestDirectory::new();
        let [a, b] = ["a.arw", "b.arw"].map(|name| directory.create_arw(name));
        let (mut catalog, mut library) = (CatalogState::default(), LibraryState::default());
        show(&mut catalog, &mut library, 1, folder(&directory));
        library.selection.set_open(&a);
        library.select_all(&catalog);

        show(&mut catalog, &mut library, 2, album(&[&b]));
        assert_eq!(library.selection.in_order([]), std::slice::from_ref(&a));

        // Photos of other folders may be selected too, as in the folder tree.
        let elsewhere = Path::new("/elsewhere.arw");
        library.selection.toggle(elsewhere);
        library.selection.toggle(&b);
        show(&mut catalog, &mut library, 3, album(&[]));
        assert!(library.selection.contains(elsewhere));
        assert!(library.selection.contains(&a));
        assert!(!library.selection.contains(&b), "removed from the album");
    }

    #[test]
    fn a_changed_photo_gets_a_new_texture() {
        let directory = TestDirectory::new();
        let photo = directory.create_arw("a.arw");
        let (mut catalog, mut library) = (CatalogState::default(), LibraryState::default());
        let thumbnail = |color| {
            let bytes = jpeg(&RgbImage::from_pixel(2, 2, Rgb([color, 0, 0])));
            ThumbnailResult::Ready(Thumbnail::new(2, 2, bytes))
        };
        let mut uploads = 0;
        let mut fill = |catalog: &CatalogState, library: &mut LibraryState<u32>| {
            library.fill_textures(catalog, (0, 1), |_, _| {
                uploads += 1;
                uploads
            });
            library.texture(&photo).copied()
        };
        show(&mut catalog, &mut library, 1, folder(&directory));
        resolve(&mut catalog, 1, &photo, thumbnail(10), None);
        assert_eq!(fill(&catalog, &mut library), Some(1));

        // Nothing changed.
        show(&mut catalog, &mut library, 2, folder(&directory));
        assert_eq!(fill(&catalog, &mut library), Some(1));

        std::fs::write(&photo, b"longer now").expect("change the photo");
        show(&mut catalog, &mut library, 3, folder(&directory));
        assert_eq!(fill(&catalog, &mut library), Some(1), "until it is known");
        resolve(&mut catalog, 3, &photo, thumbnail(200), None);
        assert_eq!(fill(&catalog, &mut library), Some(2));
    }

    #[test]
    fn capture_date_order_puts_undated_entries_last_and_keeps_ties_in_scan_order() {
        let dates = [
            Some("2024:05:02"),
            None,
            Some("2024:05:01"),
            Some("2024:05:02"),
            None,
        ];
        let order = sorted_indices(dates.len(), LibrarySort::CaptureDate, |index| dates[index]);
        assert_eq!(order, [2, 0, 3, 1, 4]);

        let by_name = sorted_indices(dates.len(), LibrarySort::Filename, |index| dates[index]);
        assert_eq!(by_name, [0, 1, 2, 3, 4]);
    }

    fn jpeg(image: &RgbImage) -> Vec<u8> {
        let mut bytes = Vec::new();
        image::codecs::jpeg::JpegEncoder::new_with_quality(&mut bytes, 85)
            .encode_image(image)
            .expect("encode a thumbnail");
        bytes
    }

    #[test]
    fn thumbnail_decoding_reduces_long_edges() {
        let large = decode_thumbnail(&jpeg(&RgbImage::from_pixel(400, 300, Rgb([200, 10, 10]))))
            .expect("decode texture");
        assert_eq!((large.width, large.height), (256, 192));
        assert_eq!(large.rgb.len(), 256 * 192 * 3);

        let small = decode_thumbnail(&jpeg(&RgbImage::from_pixel(64, 64, Rgb([10, 200, 10]))))
            .expect("decode small texture");
        assert_eq!((small.width, small.height), (64, 64));
    }

    #[test]
    fn textures_evict_least_recently_used_beyond_the_cap() {
        let mut textures = HashMap::new();
        for (last_used, path) in [(1_u64, "a"), (2, "b"), (3, "c")] {
            let entry = Texture {
                texture: (),
                identity: IDENTITY,
                last_used,
            };
            textures.insert(PathBuf::from(path), entry);
        }

        evict_least_recently_used(&mut textures, 2);

        assert_eq!(textures.len(), 2);
        assert!(!textures.contains_key(&PathBuf::from("a")));
        assert!(textures.contains_key(&PathBuf::from("c")));
    }
}
