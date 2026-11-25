//! The albums, and showing one of them.

use std::path::{Path, PathBuf};

use gpui::Context;
use rohditor_engine::albums::{Album, AlbumError};
use rohditor_engine::library_file::Shown;

use super::Library;

impl Library {
    pub fn albums(&self) -> &[Album] {
        self.albums.albums()
    }

    /// Why album files could not be loaded.
    pub fn album_warnings(&self) -> &[String] {
        self.albums.warnings()
    }

    /// The album shown.
    pub fn shown_album(&self) -> Option<&str> {
        match &self.file.shown {
            Some(Shown::Album(name)) => Some(name),
            _ => None,
        }
    }

    /// Does nothing if there is no such album.
    pub fn show_album(&mut self, name: &str, cx: &mut Context<Self>) {
        if let Some(album) = self.albums.get(name) {
            let shown = Shown::Album(album.name().to_owned());
            self.show(album.source(), shown, cx);
        }
    }

    pub fn create_album(
        &mut self,
        name: &str,
        photos: &[PathBuf],
        cx: &mut Context<Self>,
    ) -> Result<(), AlbumError> {
        self.albums.save(Album::new(name, photos)?)?;
        cx.notify();
        Ok(())
    }

    pub fn rename_album(
        &mut self,
        name: &str,
        new_name: &str,
        cx: &mut Context<Self>,
    ) -> Result<(), AlbumError> {
        self.albums.rename(name, new_name)?;
        if self.shown_album() == Some(name) {
            self.show_album(new_name, cx);
        }
        cx.notify();
        Ok(())
    }

    /// The photos stay. If it was shown, the open photo's folder is instead.
    pub fn delete_album(&mut self, name: &str, cx: &mut Context<Self>) -> Result<(), AlbumError> {
        self.albums.delete(name)?;
        if self.shown_album() == Some(name) {
            let open = self.state.selection.open();
            match open.and_then(Path::parent).map(Path::to_path_buf) {
                Some(folder) => self.show_folder(folder, cx),
                None => {
                    self.catalog.clear();
                    self.file.shown = None;
                    self.save();
                    self.tick(cx);
                }
            }
        }
        cx.notify();
        Ok(())
    }

    /// Returns how many photos were not in the album yet.
    pub fn add_to_album(
        &mut self,
        name: &str,
        photos: &[PathBuf],
        cx: &mut Context<Self>,
    ) -> Result<usize, AlbumError> {
        let added = self.albums.add_photos(name, photos)?;
        self.album_changed(name, cx);
        Ok(added)
    }

    /// Take the selected photos out of the album shown.
    pub fn remove_from_album(&mut self, cx: &mut Context<Self>) -> Result<(), AlbumError> {
        let Some(name) = self.shown_album().map(str::to_owned) else {
            return Ok(());
        };
        let photos = self.selected_photos();
        self.albums.remove_photos(&name, &photos)?;
        self.album_changed(&name, cx);
        Ok(())
    }

    fn album_changed(&mut self, name: &str, cx: &mut Context<Self>) {
        if self.shown_album() == Some(name) {
            self.show_album(name, cx);
        }
        cx.notify();
    }
}
