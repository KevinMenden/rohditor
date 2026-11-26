//! What can be done with albums. The dialogs ask for what is missing.

use std::path::PathBuf;

use gpui::{App, Context, Entity, Window, prelude::*};

use crate::library::Library;
use crate::ui::confirm::Confirm;
use crate::ui::matching;
use crate::ui::name_form::NameForm;
use crate::ui::overlays::{Overlays, ToastKind};
use crate::ui::search_list::{SearchEntry, SearchList};

/// The albums and where to ask and tell about them.
#[derive(Clone)]
pub struct AlbumCommands {
    pub library: Entity<Library>,
    pub overlays: Entity<Overlays>,
}

impl AlbumCommands {
    /// Ask for a name and create an album of these photos.
    pub fn create(&self, photos: Vec<PathBuf>, window: &mut Window, cx: &mut App) {
        let library = self.library.clone();
        let create = move |name: &str, cx: &mut App| {
            let result = library.update(cx, |library, cx| library.create_album(name, &photos, cx));
            result.map_err(|error| error.to_string())
        };
        self.ask_name("New Album", "Create", String::new(), create, window, cx);
    }

    pub fn rename(&self, album: String, window: &mut Window, cx: &mut App) {
        let library = self.library.clone();
        let name = album.clone();
        let rename = move |new: &str, cx: &mut App| {
            let result = library.update(cx, |library, cx| library.rename_album(&album, new, cx));
            result.map_err(|error| error.to_string())
        };
        self.ask_name("Rename Album", "Rename", name, rename, window, cx);
    }

    fn ask_name(
        &self,
        title: &'static str,
        accept: &'static str,
        name: String,
        on_accept: impl Fn(&str, &mut App) -> Result<(), String> + 'static,
        window: &mut Window,
        cx: &mut App,
    ) {
        let form = cx.new(|cx| NameForm::new(title, accept, name, on_accept, cx));
        self.overlays.update(cx, |overlays, cx| {
            overlays.show_dialog(form, window, cx);
        });
    }

    pub fn delete(&self, album: String, window: &mut Window, cx: &mut App) {
        let this = self.clone();
        let question = cx.new(|cx| {
            Confirm::new(
                format!("Delete the album \"{album}\"?"),
                "Its photos and their edits stay.",
                "Delete",
                move |_, cx| {
                    let result = this
                        .library
                        .update(cx, |library, cx| library.delete_album(&album, cx));
                    if let Err(error) = result {
                        this.toast(ToastKind::Error, error.to_string(), cx);
                    }
                },
                cx,
            )
        });
        self.overlays.update(cx, |overlays, cx| {
            overlays.show_dialog(question, window, cx);
        });
    }

    /// Ask for the album to add the selected photos to.
    pub fn add_selected(&self, window: &mut Window, cx: &mut App) {
        let photos = self.library.read(cx).selected_photos();
        if photos.is_empty() {
            self.toast(ToastKind::Error, "No photos are selected.".to_owned(), cx);
            return;
        }
        let albums = self.library.read(cx).albums().iter().map(|album| {
            let (this, photos) = (self.clone(), photos.clone());
            let name = album.name().to_owned();
            let count = album.photos().len().to_string();
            SearchEntry::new(name.clone(), Some(count.into()), move |_, cx| {
                this.add(&name, &photos, cx);
            })
        });
        let mut entries: Vec<_> = albums.collect();
        let (this, new) = (self.clone(), photos);
        entries.push(SearchEntry::new("New Album…", None, move |window, cx| {
            this.create(new.clone(), window, cx);
        }));
        let list = |cx: &mut Context<SearchList>| {
            SearchList::new(
                "Add to album",
                "Search albums…",
                entries,
                matching::words,
                cx,
            )
        };
        let list = cx.new(list);
        self.overlays.update(cx, |overlays, cx| {
            overlays.show_dialog(list, window, cx);
        });
    }

    fn add(&self, album: &str, photos: &[PathBuf], cx: &mut App) {
        let result = self
            .library
            .update(cx, |library, cx| library.add_to_album(album, photos, cx));
        match result {
            Ok(0) => {
                let text = format!("Already in \"{album}\".");
                self.toast(ToastKind::Success, text, cx);
            }
            Ok(added) => {
                let noun = if added == 1 { "photo" } else { "photos" };
                let text = format!("Added {added} {noun} to \"{album}\".");
                self.toast(ToastKind::Success, text, cx);
            }
            Err(error) => self.toast(ToastKind::Error, error.to_string(), cx),
        }
    }

    /// Remove the selected photos from the album shown.
    pub fn remove_selected(&self, cx: &mut App) {
        let result = self.library.update(cx, Library::remove_from_album);
        if let Err(error) = result {
            self.toast(ToastKind::Error, error.to_string(), cx);
        }
    }

    fn toast(&self, kind: ToastKind, text: String, cx: &mut App) {
        self.overlays
            .update(cx, |overlays, cx| overlays.toast(kind, text, cx));
    }
}
