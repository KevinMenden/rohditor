//! The photos of the folder or album shown, by position in the order they are shown in.

use std::path::{Path, PathBuf};

use gpui::{Context, Modifiers};
use rohditor_engine::catalog::library::LibrarySort;
use rohditor_engine::catalog::{CatalogSource, ThumbnailSlot};

use super::Library;
use crate::atlas_image::AtlasImage;

/// One of the photos shown.
pub struct ShownPhoto<'a> {
    pub path: &'a Path,
    pub name: &'a str,
    /// `None` until it is ready, and for photos that have none.
    pub thumbnail: Option<&'a AtlasImage>,
    /// Whether the thumbnail is still to come.
    pub pending: bool,
    /// An album's photo that is not there any more.
    pub missing: bool,
    pub selected: bool,
}

impl Library {
    /// The folder or album shown.
    pub fn source(&self) -> Option<&CatalogSource> {
        self.catalog.source()
    }

    /// Whether the photos shown are still being looked for.
    pub fn scanning(&self) -> bool {
        self.catalog.scanning()
    }

    /// How many photos are shown.
    pub fn len(&self) -> usize {
        self.state.order().len()
    }

    pub fn photo(&self, position: usize) -> Option<ShownPhoto<'_>> {
        let index = self.state.catalog_index(position)?;
        let path = self.catalog.entry_path(index)?;
        let slot = self.catalog.slot(index)?;
        Some(ShownPhoto {
            path,
            name: self.catalog.entry_name(index)?,
            thumbnail: self.state.texture(path),
            pending: matches!(slot, ThumbnailSlot::Pending),
            missing: matches!(slot, ThumbnailSlot::Missing),
            selected: self.state.selection.contains(path),
        })
    }

    pub fn position(&self, photo: &Path) -> Option<usize> {
        let index = self.catalog.entry_index_for_path(photo)?;
        self.state.position(index)
    }

    /// The photo after or before `current`, or the first one if `current` is not shown.
    pub fn adjacent(&self, current: Option<&Path>, forward: bool) -> Option<PathBuf> {
        let path = self.state.adjacent_path(&self.catalog, current, forward);
        path.map(Path::to_path_buf)
    }

    pub fn sort(&self) -> LibrarySort {
        self.state.sort
    }

    pub fn set_sort(&mut self, sort: LibrarySort, cx: &mut Context<Self>) {
        self.state.sort = sort;
        self.tick(cx);
    }

    pub fn selected_count(&self) -> usize {
        self.state.selection.len()
    }

    /// The selected photos in the order shown, then those not shown.
    pub fn selected_photos(&self) -> Vec<PathBuf> {
        let shown = self.state.paths(&self.catalog);
        self.state.selection.in_order(shown)
    }

    /// A right click on a photo, whose menu is about the selection: it becomes the selection
    /// unless it is part of it.
    pub fn select_for_menu(&mut self, photo: &Path, cx: &mut Context<Self>) {
        if !self.state.selection.contains(photo) {
            self.state.selection.select_only(photo);
            cx.notify();
        }
    }

    /// Select every photo shown.
    pub fn select_all(&mut self, cx: &mut Context<Self>) {
        self.state.select_all(&self.catalog);
        cx.notify();
    }

    /// A click on one of the photos shown. Returns whether to open it.
    pub fn click_shown_photo(
        &mut self,
        photo: &Path,
        modifiers: Modifiers,
        cx: &mut Context<Self>,
    ) -> bool {
        cx.notify();
        if modifiers.shift {
            self.state.select_range(&self.catalog, photo);
            return false;
        }
        if modifiers.secondary() {
            self.state.selection.toggle(photo);
            return false;
        }
        self.state.selection.select_only(photo);
        let index = self.catalog.entry_index_for_path(photo);
        index.is_some_and(|index| !self.catalog.is_missing(index))
    }

    /// These positions are on screen now, or none; have their thumbnails ready.
    pub fn set_visible(&mut self, visible: Option<(usize, usize)>, cx: &mut Context<Self>) {
        if self.visible != visible {
            self.visible = visible;
            self.tick(cx);
        }
    }
}
