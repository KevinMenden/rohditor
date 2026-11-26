//! Writes edits to recipe sidecars, debounced while editing.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use anyhow::Result;
use rohditor_edit::EditRecipe;
use rohditor_engine::open_document::OpenDocument;
use rohditor_engine::persistence::{SaveJob, SaveWorker};

pub struct Saves {
    worker: SaveWorker,
    /// Recipes handed to the worker and not yet confirmed, by source file.
    queued: HashMap<PathBuf, (u64, EditRecipe)>,
}

impl Saves {
    pub fn start() -> Result<Self> {
        Ok(Self {
            worker: SaveWorker::new()?,
            queued: HashMap::new(),
        })
    }

    /// Edits of `path` still on their way to disk.
    pub fn queued_recipe(&self, path: &Path) -> Option<&EditRecipe> {
        self.queued.get(path).map(|(_, recipe)| recipe)
    }

    pub fn pending(&self) -> bool {
        !self.queued.is_empty()
    }

    /// Save when the debounce elapsed; returns when to ask again.
    pub fn autosave(&mut self, document: &mut OpenDocument) -> Option<Duration> {
        let now = Instant::now();
        document.autosave.observe(
            document.edits.revision(),
            document.edits.gesture_active(),
            now,
        );
        if document.autosave.take_due(now) {
            self.save(document);
            return None;
        }
        document.autosave.delay(now)
    }

    /// Hand unsaved edits to the worker.
    pub fn save(&mut self, document: &mut OpenDocument) {
        // Undoing to the saved recipe while a save is in flight must still be written.
        let queued_differs = self
            .queued_recipe(&document.path)
            .is_some_and(|recipe| recipe != document.edits.recipe());
        if !document.is_dirty() && !queued_differs {
            return;
        }
        let job = SaveJob {
            document_id: document.id,
            revision: document.edits.revision(),
            source: document.path.clone(),
            recipe: document.edits.recipe().clone(),
        };
        let queued = (job.revision, job.recipe.clone());
        match self.worker.enqueue(job, true) {
            Ok(()) => {
                self.queued.insert(document.path.clone(), queued);
            }
            Err(error) => document.error = Some(format!("Could not save edits: {error}")),
        }
    }

    /// `document` is the open photo, which may not be the one that was saved.
    pub fn process_results(&mut self, mut document: Option<&mut OpenDocument>) {
        for result in self.worker.try_results() {
            let confirmed = self
                .queued
                .get(&result.source)
                .is_some_and(|queued| *queued == (result.revision, result.recipe.clone()));
            if confirmed {
                self.queued.remove(&result.source);
            }
            let Some(document) = document
                .as_deref_mut()
                .filter(|open| open.id == result.document_id)
            else {
                continue;
            };
            match result.result {
                Ok(_) => {
                    document
                        .edits
                        .mark_saved_if_current(result.revision, &result.recipe);
                }
                Err(error) => document.error = Some(format!("Could not save edits: {error}")),
            }
        }
    }

    /// Finish the queued saves.
    pub fn shut_down(&mut self) {
        self.worker.shutdown();
    }
}
