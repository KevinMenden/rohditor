//! Clicks of the pickers: sampled by the worker, applied when the sample returns.

use std::sync::Arc;

use rohditor_core::PreviewOptions;
use rohditor_edit::{ColorSample, EditRecipe};
use rohditor_raw::RawFrame;

use super::OpenDocument;
use crate::coordinator::RenderCoordinator;
use crate::document::PreviewTicket;
use crate::preview::PreviewSource;
use crate::tools::{ColorPick, MaskToolSession, Picker};

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
        self.send_pick(|ticket, frame, recipe| match picker {
            Picker::WhiteBalance => {
                coordinator.sample_white_balance(ticket, frame, recipe, position, options)
            }
            Picker::ColorMixer => {
                coordinator.sample_color_mixer(ticket, frame, recipe, position, options)
            }
        });
    }

    /// As [`Self::pick`], for the colour range of a mask.
    pub fn pick_color_range(
        &mut self,
        coordinator: &RenderCoordinator,
        pick: ColorPick,
        position: (f64, f64),
        options: PreviewOptions,
    ) {
        self.send_pick(|ticket, frame, recipe| {
            coordinator.sample_color_range(ticket, frame, recipe, position, pick, options)
        });
    }

    fn send_pick(
        &mut self,
        send: impl FnOnce(PreviewTicket, Arc<RawFrame>, EditRecipe) -> Result<(), String>,
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
        self.error = send(ticket, frame, recipe).err();
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

    /// A sample for a colour range arrived. It edits the range that `session` picks for. Returns
    /// whether the recipe changed, so the caller must queue a preview.
    pub fn color_range_picked(
        &mut self,
        session: Option<&mut MaskToolSession>,
        ticket: PreviewTicket,
        pick: ColorPick,
        sample: Result<ColorSample, String>,
    ) -> bool {
        // Settled in any case, so that another pick can follow.
        let sample = self.take_pick(ticket, sample);
        let (Some(sample), Some(session)) = (sample, session) else {
            return false;
        };
        session
            .pick_color(self.edits.recipe(), sample, pick)
            .is_some_and(|next| self.edits.set_discrete(next))
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
    fn a_picked_color_edits_the_range_of_the_masks_tool() {
        let sample = ColorSample {
            lightness: 0.5,
            a: 0.1,
            b: -0.1,
        };
        let (mut document, ticket) = picking();
        let mut session = MaskToolSession::default();
        session.start_color_pick();
        let pick = ColorPick::Replace;
        assert!(document.color_range_picked(Some(&mut session), ticket, pick, Ok(sample)));
        assert_eq!(document.edits.recipe().local.layers.len(), 1);
        let again = document.color_range_picked(Some(&mut session), ticket, pick, Ok(sample));
        assert!(!again, "settled once");

        // The tool was left meanwhile.
        let (mut document, ticket) = picking();
        assert!(!document.color_range_picked(None, ticket, pick, Ok(sample)));
        assert!(!document.is_picking(), "another pick can follow");
    }

    #[test]
    fn a_failed_pick_says_why() {
        let (mut document, ticket) = picking();
        assert!(!document.white_balance_picked(ticket, Err("too varied".to_owned())));
        assert_eq!(document.error.as_deref(), Some("too varied"));
    }
}
