//! Clicks of the pickers: sampled by the worker, applied when the sample returns.

use std::sync::Arc;

use rohditor_core::PreviewOptions;

use super::OpenDocument;
use crate::coordinator::RenderCoordinator;
use crate::document::PreviewTicket;
use crate::preview::PreviewSource;
use crate::tools::Picker;

impl OpenDocument {
    /// Sample at `position`, `0..1` over the image as shown. Ignored while another
    /// sample is on its way.
    pub fn pick(
        &mut self,
        coordinator: &RenderCoordinator,
        picker: Picker,
        position: (f64, f64),
        options: PreviewOptions,
    ) {
        if self.pending_pick.is_some() {
            return;
        }
        let developed = self.preview.source.is_some_and(PreviewSource::is_developed);
        let Some(frame) = self.frame.as_ref().filter(|_| developed).map(Arc::clone) else {
            self.error = Some("Wait for the developed preview before picking".to_owned());
            return;
        };
        let ticket = self.ticket();
        let recipe = self.edits.recipe().clone();
        let sent = match picker {
            Picker::WhiteBalance => {
                coordinator.sample_white_balance(ticket, frame, recipe, position, options)
            }
            Picker::ColorMixer => {
                coordinator.sample_color_mixer(ticket, frame, recipe, position, options)
            }
        };
        self.error = sent.err();
        if self.error.is_none() {
            self.pending_pick = Some(ticket);
        }
    }

    /// A white-balance sample arrived. Returns whether it changed the recipe, so
    /// the caller must queue a preview.
    pub fn white_balance_picked(
        &mut self,
        ticket: PreviewTicket,
        sample: Result<[f32; 3], String>,
    ) -> bool {
        match self.take_pick(ticket, sample) {
            Some(sample) => self.apply_white_balance_sample(sample),
            None => false,
        }
    }

    /// A colour mixer sample arrived. Returns the channel to select.
    pub fn color_mixer_picked(
        &mut self,
        ticket: PreviewTicket,
        channel: Result<usize, String>,
    ) -> Option<usize> {
        self.take_pick(ticket, channel)
    }

    /// Whether a sample is on its way.
    pub const fn is_picking(&self) -> bool {
        self.pending_pick.is_some()
    }

    /// Ignore the sample on its way.
    pub fn cancel_pick(&mut self) {
        self.pending_pick = None;
    }

    /// Settle the pick that `ticket` belongs to. Its result only counts if the
    /// recipe is still the one it was sampled with.
    fn take_pick<T>(&mut self, ticket: PreviewTicket, result: Result<T, String>) -> Option<T> {
        self.pending_pick.take_if(|pending| *pending == ticket)?;
        if self.edits.revision() != ticket.revision {
            return None;
        }
        match result {
            Ok(sample) => {
                self.error = None;
                Some(sample)
            }
            Err(message) => {
                self.error = Some(message);
                None
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use rohditor_core::default_recipe;

    use super::*;

    /// A document with a pick on its way.
    fn picking() -> (OpenDocument, PreviewTicket) {
        let path = PathBuf::from("photo.arw");
        let mut document = OpenDocument::opening(1, path, default_recipe(None), None);
        let ticket = document.ticket();
        document.pending_pick = Some(ticket);
        (document, ticket)
    }

    #[test]
    fn a_pick_is_settled_once() {
        let (mut document, ticket) = picking();
        assert_eq!(document.color_mixer_picked(ticket, Ok(3)), Some(3));
        assert_eq!(document.color_mixer_picked(ticket, Ok(3)), None);
    }

    #[test]
    fn the_sample_of_another_document_is_ignored() {
        let (mut document, ticket) = picking();
        let mut foreign = ticket;
        foreign.document_id += 1;
        assert_eq!(document.color_mixer_picked(foreign, Ok(3)), None);
        assert_eq!(document.color_mixer_picked(ticket, Ok(3)), Some(3));
    }

    #[test]
    fn the_sample_of_an_older_recipe_is_dropped() {
        let (mut document, ticket) = picking();
        let mut next = document.edits.recipe().clone();
        next.light.exposure_ev = 0.5;
        assert!(document.edits.set_discrete(next));
        assert_eq!(document.color_mixer_picked(ticket, Ok(3)), None);
        assert!(document.pending_pick.is_none(), "another pick can follow");
    }

    #[test]
    fn a_failed_pick_says_why() {
        let (mut document, ticket) = picking();
        assert!(!document.white_balance_picked(ticket, Err("too varied".to_owned())));
        assert_eq!(document.error.as_deref(), Some("too varied"));
    }
}
