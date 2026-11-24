//! Changes to the open photo's recipe.

use gpui::Context;
use rohditor_core::auto_tone;
use rohditor_edit::{
    EditRecipe, GradingTint, GradingTones, HighlightMethod, Parameter, RecipeSection,
    RenderingProfileSelection, WhiteBalancePreset,
};
use rohditor_engine::document::EditSession;
use rohditor_engine::open_document::OpenDocument;

use super::Session;

/// Whether an edit comes from a drag still in progress.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EditPhase {
    Dragging,
    Finished,
}

impl Session {
    pub fn begin_gesture(&mut self) {
        if let Some(document) = self.document.as_mut() {
            document.edits.begin_gesture();
        }
    }

    /// Change a parameter as part of a gesture, or as one step of its own. The white balance is
    /// drafted on the resident GPU frame while dragging and rendered exactly at the end.
    pub fn set_parameter(
        &mut self,
        parameter: Parameter,
        value: f32,
        phase: EditPhase,
        cx: &mut Context<Self>,
    ) {
        let Some(document) = self.document.as_mut() else {
            return;
        };
        let defaults = document.default_recipe();
        let next = parameter.with_value(document.edits.recipe(), &defaults, value);
        self.edit(next, phase, is_drafted(parameter), cx);
    }

    /// Move a colour grading wheel, as [`Self::set_parameter`].
    pub fn set_grading(
        &mut self,
        tones: GradingTones,
        tint: GradingTint,
        phase: EditPhase,
        cx: &mut Context<Self>,
    ) {
        if let Some(document) = self.document.as_ref() {
            let mut next = document.edits.recipe().clone();
            next.color.grading = tones.with(next.color.grading, tint);
            self.edit(next, phase, false, cx);
        }
    }

    fn edit(&mut self, next: EditRecipe, phase: EditPhase, draft: bool, cx: &mut Context<Self>) {
        let Some(document) = self.document.as_mut() else {
            return;
        };
        let changed = match phase {
            EditPhase::Dragging => document.edits.set_during_gesture(next),
            EditPhase::Finished => document.edits.set_discrete(next),
        };
        if !changed {
            return;
        }
        let drafted = draft
            && phase == EditPhase::Dragging
            && self.renderer.run(
                document,
                &mut self.display,
                |runtime, coordinator, preview, request| {
                    runtime.render_white_balance_draft(coordinator, preview, request)
                },
            );
        if !drafted {
            self.queue_preview();
        }
        self.tick(cx);
    }

    /// End the gesture that changed `parameter`.
    pub fn finish_gesture(&mut self, parameter: Parameter, cx: &mut Context<Self>) {
        if let Some(document) = self.document.as_mut() {
            document.edits.finish_gesture();
            if is_drafted(parameter) {
                self.queue_preview();
            }
        }
        self.tick(cx);
    }

    /// Restore the photo's defaults in one section.
    pub fn reset_section(&mut self, section: RecipeSection, cx: &mut Context<Self>) {
        if self
            .document
            .as_mut()
            .is_some_and(|document| document.reset_section(section))
        {
            self.queue_preview();
            self.tick(cx);
        }
    }

    pub fn set_white_balance(&mut self, preset: WhiteBalancePreset, cx: &mut Context<Self>) {
        self.change_recipe(|recipe, defaults| preset.apply(recipe, defaults), cx);
    }

    pub fn set_highlight_method(&mut self, method: HighlightMethod, cx: &mut Context<Self>) {
        self.change_recipe(
            |recipe, _| {
                let mut next = recipe.clone();
                next.raw.highlights.method = method;
                next
            },
            cx,
        );
    }

    pub fn set_rendering(&mut self, rendering: RenderingProfileSelection, cx: &mut Context<Self>) {
        self.change_recipe(
            |recipe, _| {
                let mut next = recipe.clone();
                next.rendering.profile = rendering;
                next
            },
            cx,
        );
    }

    /// One undo step, made from the recipe and its defaults.
    fn change_recipe(
        &mut self,
        change: impl FnOnce(&EditRecipe, &EditRecipe) -> EditRecipe,
        cx: &mut Context<Self>,
    ) {
        if let Some(document) = self.document.as_ref() {
            let next = change(document.edits.recipe(), &document.default_recipe());
            self.change_history(|edits| edits.set_discrete(next), cx);
        }
    }

    fn change_history(
        &mut self,
        change: impl FnOnce(&mut EditSession) -> bool,
        cx: &mut Context<Self>,
    ) {
        if self.document.as_mut().is_some_and(|d| change(&mut d.edits)) {
            self.queue_preview();
            self.tick(cx);
        }
    }

    pub fn undo(&mut self, cx: &mut Context<Self>) {
        self.change_history(EditSession::undo, cx);
    }

    pub fn redo(&mut self, cx: &mut Context<Self>) {
        self.change_history(EditSession::redo, cx);
    }

    pub fn reset(&mut self, cx: &mut Context<Self>) {
        let Some(defaults) = self.document.as_ref().map(OpenDocument::default_recipe) else {
            return;
        };
        self.change_history(|edits| edits.reset_to(defaults), cx);
    }

    /// Auto Tone needs the histogram of exactly the recipe it starts from.
    pub fn auto_tone(&mut self, cx: &mut Context<Self>) {
        let next = self.document.as_ref().and_then(|document| {
            let preview = &document.preview;
            if preview.histogram_revision != Some(document.edits.revision()) {
                return None;
            }
            auto_tone(document.edits.recipe(), preview.histogram.as_ref()?)
        });
        if let Some(next) = next {
            self.change_history(|edits| edits.set_discrete(next), cx);
        }
    }

    /// Auto noise reduction estimates the photo's noise first when needed.
    pub fn auto_noise_reduction(&mut self, cx: &mut Context<Self>) {
        let options = self.renderer.settings.render_options();
        let changed = self.document.as_mut().is_some_and(|document| {
            document.apply_noise_suggestion(&self.renderer.coordinator, options)
        });
        if changed {
            self.queue_preview();
        }
        self.tick(cx);
    }
}

/// Whether the parameter has a fast draft while it is dragged.
fn is_drafted(parameter: Parameter) -> bool {
    parameter.section() == RecipeSection::WhiteBalance
}
