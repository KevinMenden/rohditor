//! Writes edits to recipe sidecars, debounced while editing.

use std::time::{Duration, Instant};

use rohditor_engine::persistence::SaveJob;

use super::Session;

impl Session {
    /// Save when the debounce elapsed; returns when to ask again.
    pub(super) fn autosave(&mut self) -> Option<Duration> {
        let now = Instant::now();
        let document = self.document.as_mut()?;
        document.autosave.observe(
            document.edits.revision(),
            document.edits.gesture_active(),
            now,
        );
        if document.autosave.take_due(now) {
            self.save_now();
            return None;
        }
        document.autosave.delay(now)
    }

    /// Hand unsaved edits to the save worker.
    pub fn save_now(&mut self) {
        let Some(document) = self.document.as_mut() else {
            return;
        };
        // Undoing to the saved recipe while a save is in flight must still be written.
        let queued_differs = self
            .queued_saves
            .get(&document.path)
            .is_some_and(|(_, recipe)| recipe != document.edits.recipe());
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
        match self.saves.enqueue(job, true) {
            Ok(()) => {
                self.queued_saves.insert(document.path.clone(), queued);
            }
            Err(error) => document.error = Some(format!("Could not save edits: {error}")),
        }
    }

    pub(super) fn process_save_results(&mut self) {
        let results: Vec<_> = self.saves.try_results().collect();
        for result in results {
            let confirmed = self
                .queued_saves
                .get(&result.source)
                .is_some_and(|queued| *queued == (result.revision, result.recipe.clone()));
            if confirmed {
                self.queued_saves.remove(&result.source);
            }
            let Some(document) = self
                .document
                .as_mut()
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
}
