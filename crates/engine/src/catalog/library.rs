//! Toolkit-free state of the library grid and filmstrip.
//!
//! A frontend owns the widgets; this holds what a second frontend would
//! otherwise duplicate: the display order of the catalog, the selection and
//! its keyboard navigation, and the policy of which thumbnails to decode, how
//! many per frame and how many to keep resident. The decoded thumbnail is
//! handed to the frontend to turn into its own texture type `T`.

use std::cmp::Ordering;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use tracing::warn;

use super::{CatalogState, ThumbnailSlot};

/// Longest grid texture edge; thumbnails decode down to this size.
pub const TEXTURE_LONG_EDGE: u32 = 256;
/// Maximum resident grid textures before the least recently used are evicted.
pub const TEXTURE_CAP: usize = 256;
/// Grid textures decoded per frame, so a folder fill never blocks a frame.
pub const DECODE_BUDGET: usize = 8;
/// Extra entries around the visible range kept warm for smooth scrolling.
pub const VISIBLE_MARGIN: usize = 24;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum LibrarySort {
    /// Scan order, which is by filename.
    #[default]
    Filename,
    CaptureDate,
}

impl LibrarySort {
    pub const ALL: [Self; 2] = [Self::Filename, Self::CaptureDate];
}

/// Keyboard navigation steps in the library grid.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LibraryNavigation {
    Left,
    Right,
    Up,
    Down,
    Home,
    End,
}

/// A thumbnail decoded and reduced to at most [`TEXTURE_LONG_EDGE`].
pub struct DecodedThumbnail {
    pub width: usize,
    pub height: usize,
    /// Packed RGB8, row-major.
    pub rgb: Vec<u8>,
}

/// Library selection, display order and resident thumbnail textures.
pub struct LibraryState<T> {
    /// Folder of the last applied scan, used to reset the selection when
    /// the catalog switches folders.
    folder: Option<PathBuf>,
    pub selection: Option<usize>,
    /// Display order for the current sort, mapping grid positions to catalog
    /// entry indices.
    order: Vec<usize>,
    pub sort: LibrarySort,
    /// Textures keyed by source path with their last-use frame.
    textures: HashMap<PathBuf, (T, u64)>,
    /// Paths whose thumbnail bytes could not be decoded, so failures are not
    /// retried every frame.
    failures: HashSet<PathBuf>,
    /// Grid positions that intersected the viewport last frame.
    pub visible: (usize, usize),
    /// Column count of the laid-out grid, for keyboard navigation.
    pub columns: usize,
    frame: u64,
    decode_pending: bool,
    /// Scroll the selection into view once, after keyboard navigation.
    pub scroll_to_selection: bool,
}

impl<T> Default for LibraryState<T> {
    fn default() -> Self {
        Self {
            folder: None,
            selection: None,
            order: Vec::new(),
            sort: LibrarySort::default(),
            textures: HashMap::new(),
            failures: HashSet::new(),
            visible: (0, VISIBLE_MARGIN),
            columns: 1,
            frame: 0,
            decode_pending: false,
            scroll_to_selection: false,
        }
    }
}

impl<T> LibraryState<T> {
    /// Count a frame; texture recency is measured in frames.
    pub fn begin_frame(&mut self) {
        self.frame = self.frame.wrapping_add(1);
    }

    /// Follow the catalog: reset on a folder change, drop a selection past the
    /// end, and recompute the display order.
    pub fn sync(&mut self, catalog: &CatalogState) {
        if catalog.folder() != self.folder.as_deref() {
            self.folder = catalog.folder().map(Path::to_path_buf);
            self.selection = None;
            self.textures.clear();
            self.failures.clear();
        }
        let count = catalog.entry_count();
        self.selection = self.selection.filter(|index| *index < count);
        self.refresh_order(catalog);
    }

    /// Recompute the display order for the current sort.
    pub fn refresh_order(&mut self, catalog: &CatalogState) {
        self.order = sorted_indices(
            catalog.entry_count(),
            self.sort,
            |index| catalog.entry_name(index),
            |index| catalog.capture_date(index),
        );
    }

    /// Catalog entry indices in display order.
    pub fn order(&self) -> &[usize] {
        &self.order
    }

    /// The catalog entry shown at a grid position.
    pub fn catalog_index(&self, grid_index: usize) -> Option<usize> {
        self.order.get(grid_index).copied()
    }

    /// The grid position of a catalog entry.
    pub fn grid_index(&self, catalog_index: usize) -> Option<usize> {
        self.order.iter().position(|&index| index == catalog_index)
    }

    /// The grid position of the photo shown before or after the one at
    /// `active_catalog_index`, stepping in display order (which differs from
    /// catalog order when sorted by capture date). `None` at the ends or when
    /// nothing is active.
    pub fn adjacent_grid_index(
        &self,
        active_catalog_index: Option<usize>,
        forward: bool,
    ) -> Option<usize> {
        let active = self.grid_index(active_catalog_index?);
        adjacent_index(active, self.order.len(), forward)
    }

    /// Move the selection by a keyboard step and scroll it into view.
    pub fn navigate(&mut self, entry_count: usize, key: LibraryNavigation) {
        if entry_count == 0 {
            return;
        }
        self.selection = Some(moved_selection(
            self.selection,
            self.columns,
            entry_count,
            key,
        ));
        self.scroll_to_selection = true;
    }

    /// Whether more thumbnails are waiting for a later frame's decode budget.
    pub const fn decode_pending(&self) -> bool {
        self.decode_pending
    }

    pub fn texture(&self, path: &Path) -> Option<&T> {
        self.textures.get(path).map(|(texture, _)| texture)
    }

    /// Decode thumbnails for grid positions around `range`, at most
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
        for grid_index in start..end {
            let Some(catalog_index) = self.order.get(grid_index).copied() else {
                continue;
            };
            let (Some(path), Some(ThumbnailSlot::Ready(thumbnail))) = (
                catalog.entry_path(catalog_index),
                catalog.slot(catalog_index),
            ) else {
                continue;
            };
            if let Some(entry) = self.textures.get_mut(path) {
                entry.1 = self.frame;
                continue;
            }
            if budget == 0 {
                self.decode_pending = true;
                continue;
            }
            if self.failures.contains(path) {
                continue;
            }
            match decode_thumbnail(thumbnail.bytes()) {
                Ok(decoded) => {
                    let texture = upload(path, decoded);
                    self.textures
                        .insert(path.to_path_buf(), (texture, self.frame));
                    budget -= 1;
                }
                // A failed decode spends no budget: it is remembered, so each
                // corrupt thumbnail costs one attempt in total, not one per frame.
                Err(error) => {
                    warn!(
                        %error,
                        path = %path.display(),
                        "library grid texture decoding failed"
                    );
                    self.failures.insert(path.to_path_buf());
                }
            }
        }
        evict_least_recently_used(&mut self.textures, TEXTURE_CAP);
    }
}

/// Display order of catalog entries: scan order for filename sort, or
/// capture-date order with undated entries last.
pub fn sorted_indices<'a>(
    count: usize,
    sort: LibrarySort,
    name: impl Fn(usize) -> Option<&'a str>,
    capture_date: impl Fn(usize) -> Option<&'a str>,
) -> Vec<usize> {
    let mut order: Vec<usize> = (0..count).collect();
    if sort == LibrarySort::CaptureDate {
        order.sort_by(|&left, &right| {
            match (capture_date(left), capture_date(right)) {
                (Some(left_date), Some(right_date)) => left_date.cmp(right_date),
                (Some(_), None) => return Ordering::Less,
                (None, Some(_)) => return Ordering::Greater,
                (None, None) => Ordering::Equal,
            }
            .then_with(|| name(left).cmp(&name(right)))
        });
    }
    order
}

/// Move a library selection one grid step; `count` must be at least one entry.
/// Without a selection, any key selects the first entry.
pub fn moved_selection(
    current: Option<usize>,
    columns: usize,
    count: usize,
    key: LibraryNavigation,
) -> usize {
    let columns = columns.max(1);
    let last = count - 1;
    let Some(current) = current.map(|current| current.min(last)) else {
        return 0;
    };
    match key {
        LibraryNavigation::Left => current.saturating_sub(1),
        LibraryNavigation::Right => (current + 1).min(last),
        LibraryNavigation::Up => current.saturating_sub(columns),
        LibraryNavigation::Down => (current + columns).min(last),
        LibraryNavigation::Home => 0,
        LibraryNavigation::End => last,
    }
}

/// Step to the previous or next position in a list of `count`. Returns
/// `None` at the ends or when nothing is active.
pub fn adjacent_index(current: Option<usize>, count: usize, forward: bool) -> Option<usize> {
    let current = current?;
    if forward {
        (current + 1 < count).then_some(current + 1)
    } else {
        current.checked_sub(1)
    }
}

/// Decode encoded thumbnail bytes into a bounded grid texture image.
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
fn evict_least_recently_used<T>(textures: &mut HashMap<PathBuf, (T, u64)>, cap: usize) {
    if textures.len() <= cap {
        return;
    }
    let mut order: Vec<(u64, PathBuf)> = textures
        .iter()
        .map(|(path, (_, frame))| (*frame, path.clone()))
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

    use super::*;

    #[test]
    fn selection_moves_by_grid_steps_and_clamps() {
        use LibraryNavigation::{Down, End, Home, Left, Right, Up};

        assert_eq!(moved_selection(None, 3, 10, Right), 0);
        assert_eq!(moved_selection(None, 3, 10, Left), 0);
        assert_eq!(moved_selection(Some(4), 3, 10, Down), 7);
        assert_eq!(moved_selection(Some(4), 3, 10, Up), 1);
        assert_eq!(moved_selection(Some(4), 3, 10, Left), 3);
        assert_eq!(moved_selection(Some(9), 3, 10, Right), 9);
        assert_eq!(moved_selection(Some(9), 3, 10, Down), 9);
        assert_eq!(moved_selection(Some(5), 3, 10, Home), 0);
        assert_eq!(moved_selection(Some(5), 3, 10, End), 9);
        assert_eq!(moved_selection(Some(5), 1, 1, End), 0);
    }

    #[test]
    fn navigating_requests_a_scroll_and_ignores_an_empty_catalog() {
        let mut library = LibraryState::<()> {
            columns: 4,
            ..LibraryState::default()
        };
        library.navigate(0, LibraryNavigation::Right);
        assert_eq!(library.selection, None);
        assert!(!library.scroll_to_selection);

        library.navigate(10, LibraryNavigation::Right);
        assert_eq!(library.selection, Some(0));
        assert!(library.scroll_to_selection);
        library.navigate(10, LibraryNavigation::Down);
        assert_eq!(library.selection, Some(4));
    }

    #[test]
    fn filmstrip_navigation_steps_and_stops_at_the_ends() {
        assert_eq!(adjacent_index(None, 10, true), None);
        assert_eq!(adjacent_index(Some(0), 10, false), None);
        assert_eq!(adjacent_index(Some(0), 10, true), Some(1));
        assert_eq!(adjacent_index(Some(8), 10, true), Some(9));
        assert_eq!(adjacent_index(Some(9), 10, true), None);
        assert_eq!(adjacent_index(Some(9), 10, false), Some(8));
        assert_eq!(adjacent_index(Some(0), 0, true), None);
    }

    #[test]
    fn filmstrip_steps_follow_display_order_not_catalog_order() {
        let library = LibraryState::<()> {
            order: vec![2, 0, 3, 1],
            ..LibraryState::default()
        };
        // Entry 0 is shown second; the next photo shown is entry 3 (position 2).
        assert_eq!(library.adjacent_grid_index(Some(0), true), Some(2));
        assert_eq!(library.adjacent_grid_index(Some(0), false), Some(0));
        assert_eq!(library.adjacent_grid_index(Some(2), false), None);
        assert_eq!(library.adjacent_grid_index(Some(1), true), None);
        assert_eq!(library.adjacent_grid_index(None, true), None);
    }

    #[test]
    fn capture_date_order_puts_undated_entries_last_and_breaks_ties_by_name() {
        let names = ["a.arw", "b.arw", "c.arw", "d.arw", "e.arw"];
        let dates = [
            Some("2024:05:02"),
            None,
            Some("2024:05:01"),
            Some("2024:05:02"),
            None,
        ];
        let order = sorted_indices(
            names.len(),
            LibrarySort::CaptureDate,
            |index| names.get(index).copied(),
            |index| dates[index],
        );
        assert_eq!(order, [2, 0, 3, 1, 4]);

        let by_name = sorted_indices(
            names.len(),
            LibrarySort::Filename,
            |index| names.get(index).copied(),
            |index| dates[index],
        );
        assert_eq!(by_name, [0, 1, 2, 3, 4]);
    }

    #[test]
    fn thumbnail_decoding_reduces_long_edges() {
        let encode = |image: &RgbImage| {
            let mut bytes = Vec::new();
            image::codecs::jpeg::JpegEncoder::new_with_quality(&mut bytes, 85)
                .encode_image(image)
                .expect("encode grid texture source");
            bytes
        };
        let large = decode_thumbnail(&encode(&RgbImage::from_pixel(400, 300, Rgb([200, 10, 10]))))
            .expect("decode grid texture");
        assert_eq!((large.width, large.height), (256, 192));
        assert_eq!(large.rgb.len(), 256 * 192 * 3);

        let small = decode_thumbnail(&encode(&RgbImage::from_pixel(64, 64, Rgb([10, 200, 10]))))
            .expect("decode small grid texture");
        assert_eq!((small.width, small.height), (64, 64));
    }

    #[test]
    fn textures_evict_least_recently_used_beyond_the_cap() {
        let mut textures = HashMap::new();
        for (frame, path) in [(1_u64, "a"), (2, "b"), (3, "c")] {
            textures.insert(PathBuf::from(path), ((), frame));
        }

        evict_least_recently_used(&mut textures, 2);

        assert_eq!(textures.len(), 2);
        assert!(!textures.contains_key(&PathBuf::from("a")));
        assert!(textures.contains_key(&PathBuf::from("c")));
    }
}
