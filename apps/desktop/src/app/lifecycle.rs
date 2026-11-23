//! Document open/save/close transitions and background recipe persistence.

use std::path::PathBuf;

use eframe::egui;
use rohditor_core::default_recipe;

use super::{Document, RohditorApp};
use rohditor_engine::document::EditSession;
use rohditor_engine::persistence::{self, SaveJob, SaveResult};
use rohditor_engine::tools::ActiveTool;

/// A gesture is orphaned when it is open but the toolkit reports no pressed
/// pointer: the release happened while the dragged widget was unmounted (the
/// panel swapped out from under the drag), so no `drag_stopped` will ever
/// arrive for it. `pointer_down` is egui's persistent held-button state, not
/// a per-frame event, so a long legitimate hold keeps this false.
fn is_gesture_orphaned(gesture_active: bool, pointer_down: bool) -> bool {
    gesture_active && !pointer_down
}

/// Commits an orphaned gesture with end-of-drag semantics: exactly what the
/// `drag_stopped` the release frame lost would have done — one undo step,
/// `dragging` false again so the autosave debounce schedules on this beat.
fn reap_orphaned_gesture(edits: &mut EditSession, pointer_down: bool) {
    if is_gesture_orphaned(edits.gesture_active(), pointer_down) {
        edits.finish_gesture();
    }
}

impl RohditorApp {
    pub(super) fn update_autosave(&mut self, context: &egui::Context) {
        let now = std::time::Instant::now();
        // Run after the widget pass, before `observe`: if the release frame
        // lost the dragged widget, commit the drag now so `observe` below sees
        // the gesture closed and schedules the normal one-second debounce on
        // this same beat.
        let pointer_down = context.input(|input| input.pointer.any_down());
        if let Some(document) = self.document.as_mut() {
            reap_orphaned_gesture(&mut document.core.edits, pointer_down);
        }
        let (revision_changed, due, delay) = {
            let Some(document) = self.document.as_mut() else {
                return;
            };
            let revision_changed = document.core.autosave.observe(
                document.core.edits.revision(),
                document.core.edits.gesture_active(),
                now,
            );
            let due = document.core.autosave.take_due(now);
            let delay = document.core.autosave.delay(now);
            (revision_changed, due, delay)
        };
        if revision_changed
            && self
                .recipe_save_status
                .as_ref()
                .is_some_and(|status| status.notice_deadline().is_some())
        {
            // A previous success message must not describe the newly edited
            // recipe while its replacement is still in the debounce window.
            self.recipe_save_status = None;
        }
        if let Some(delay) = delay {
            context.request_repaint_after(delay);
        }
        if due {
            let _ = self.schedule_recipe_save(true);
        }
    }

    pub(super) fn open_path(&mut self, context: &egui::Context, path: PathBuf) {
        let _ = self.schedule_recipe_save(true);
        self.open_path_now(context, path);
    }

    fn open_path_now(&mut self, context: &egui::Context, path: PathBuf) {
        self.close_document_now(context);
        let (recipe, warning, initialize_white_balance_from_metadata) = match self
            .queued_recipe_snapshots
            .get(&path)
            .map(|(_, recipe)| recipe.clone())
            .map(|recipe| Ok(Some(recipe)))
            .unwrap_or_else(|| persistence::load_recipe(&path))
        {
            Ok(Some(recipe)) => (recipe, None, false),
            Ok(None) => (default_recipe(None), None, true),
            Err(error) => (
                default_recipe(None),
                Some(format!(
                    "Could not load saved edits for {}: {error}; using the default recipe.",
                    path.display()
                )),
                true,
            ),
        };
        let document_id = self.next_document_id;
        self.next_document_id = self.next_document_id.saturating_add(1);
        let mut document = Document::opening(document_id, path.clone(), recipe, warning);
        // Only a photo without saved edits starts from the automatic default.
        if initialize_white_balance_from_metadata {
            document
                .core
                .mark_new_photo(self.settings.auto_noise_reduction());
        }
        self.document = Some(document);
        if self.gpu_required_but_unavailable() {
            if let Some(document) = self.document.as_mut() {
                document.core.open_status = None;
                document.core.error = self.startup_error.clone();
            }
            self.update_window_title(context);
            return;
        }
        if let Err(error) = self.coordinator.open(document_id, path)
            && let Some(document) = self.document.as_mut()
        {
            document.core.open_status = None;
            document.core.error = Some(error);
        }
        self.update_window_title(context);
    }

    pub(super) fn request_close_document(&mut self, context: &egui::Context) {
        let _ = self.schedule_recipe_save(true);
        self.close_document_now(context);
    }

    fn close_document_now(&mut self, context: &egui::Context) {
        self.color_mixer_channel = 0;
        self.pending_white_balance_pick = None;
        self.pending_color_pick = None;
        self.white_balance_memory = rohditor_core::WhiteBalanceModeMemory::default();
        self.tool = ActiveTool::default();
        if let Some(mut document) = self.document.take() {
            self.release_gpu_preview(&mut document);
            self.coordinator.abandon(document.core.id);
        }
        self.update_window_title(context);
    }

    /// Request an immediate background write for the current recipe.
    pub(super) fn save_document(&mut self) -> Result<(), String> {
        self.schedule_recipe_save(true)
    }

    /// Called after the UI debounce, or immediately for navigation and Save.
    pub(super) fn schedule_recipe_save(&mut self, immediate: bool) -> Result<(), String> {
        let Some(document) = self.document.as_ref().filter(|document| {
            document.core.is_dirty()
                || self
                    .queued_recipe_snapshots
                    .get(&document.core.path)
                    .is_some_and(|(_, recipe)| recipe != document.core.edits.recipe())
        }) else {
            return Ok(());
        };
        let document_id = document.core.id;
        let revision = document.core.edits.revision();
        let source = document.core.path.clone();
        let recipe = document.core.edits.recipe().clone();
        let job = SaveJob {
            document_id,
            revision,
            source: source.clone(),
            recipe: recipe.clone(),
        };
        self.recipe_save_status = Some(if immediate {
            super::RecipeSaveStatus::Pending("Saving edits…".to_owned())
        } else {
            super::RecipeSaveStatus::Pending("Edits pending save…".to_owned())
        });
        self.recipe_save_error = None;
        if let Err(error) = self.recipe_saves.enqueue(job, immediate) {
            self.recipe_save_error = Some(error.clone());
            if let Some(document) = self.document.as_mut() {
                document.core.error = Some(format!("Could not queue edits for saving: {error}"));
            }
            return Err(error);
        }
        self.queued_recipe_snapshots
            .insert(source, (revision, recipe));
        Ok(())
    }

    pub(super) fn process_recipe_save_events(&mut self) {
        let results = self.recipe_saves.try_results().collect::<Vec<_>>();
        for result in results {
            self.process_recipe_save_result(result);
        }
    }

    fn process_recipe_save_result(&mut self, result: SaveResult) {
        let SaveResult {
            document_id,
            revision,
            source,
            recipe,
            result,
        } = result;
        match result {
            Ok(path) => {
                if self.queued_recipe_snapshots.get(&source).is_some_and(
                    |(queued_revision, queued_recipe)| {
                        *queued_revision == revision && *queued_recipe == recipe
                    },
                ) {
                    self.queued_recipe_snapshots.remove(&source);
                }
                let newer_pending = self.queued_recipe_snapshots.contains_key(&source);
                let same_document = self
                    .document
                    .as_ref()
                    .is_some_and(|document| document.core.id == document_id);
                let current = self
                    .document
                    .as_mut()
                    .filter(|document| document.core.id == document_id)
                    .is_some_and(|document| {
                        document.core.edits.mark_saved_if_current(revision, &recipe)
                    });
                self.recipe_save_status = Some(if !newer_pending && (!same_document || current) {
                    super::RecipeSaveStatus::Notice {
                        message: format!("Saved edits to {}.", path.display()),
                        until: std::time::Instant::now() + super::RECIPE_SAVE_NOTICE_DURATION,
                    }
                } else {
                    super::RecipeSaveStatus::Pending("Edits pending save…".to_owned())
                });
                if current
                    && let Some(document) = self
                        .document
                        .as_mut()
                        .filter(|document| document.core.id == document_id)
                {
                    document.core.error = None;
                }
                self.recipe_save_error = None;
            }
            Err(error) => {
                let newer_pending = self.queued_recipe_snapshots.contains_key(&source);
                self.recipe_save_status = Some(if newer_pending {
                    super::RecipeSaveStatus::Pending("Edits pending save…".to_owned())
                } else {
                    super::RecipeSaveStatus::Error("Could not save edits".to_owned())
                });
                self.recipe_save_error = Some(error.clone());
                if let Some(document) = self
                    .document
                    .as_mut()
                    .filter(|document| document.core.id == document_id)
                {
                    document.core.error = Some(format!("Could not save edits: {error}"));
                }
            }
        }
    }

    pub(super) fn update_window_title(&self, context: &egui::Context) {
        let title = self.document.as_ref().map_or_else(
            || "Rohditor".to_owned(),
            |document| {
                let marker = if document.core.is_dirty() { "* " } else { "" };
                format!("{marker}{} — Rohditor", document.core.file_name())
            },
        );
        context.send_viewport_cmd(egui::ViewportCommand::Title(title));
    }
}

impl Drop for RohditorApp {
    fn drop(&mut self) {
        // The worker drains all queued snapshots before joining, so closing
        // the native window cannot discard the current recipe.
        let _ = self.schedule_recipe_save(true);
        self.recipe_saves.shutdown();
    }
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use eframe::egui;
    use rohditor_edit::EXPOSURE_EV_RANGE;
    use rohditor_engine::document::EditSession;

    use super::{is_gesture_orphaned, reap_orphaned_gesture};
    use crate::app::apply_adjustment_interaction;
    use crate::ui::adjustment_panel::{AdjustmentInteraction, AdjustmentTarget};
    use rohditor_engine::autosave::Autosave;

    const FRAME: f64 = 1.0 / 60.0;
    /// A point on the mounted slider's track, and a modest drag along it. The
    /// delta stays small so the value never clamps (`Response::changed()`
    /// would drop on a clamped slider).
    const PRESS: egui::Pos2 = egui::pos2(60.0, 10.0);
    const DRAG_TO: egui::Pos2 = egui::pos2(78.0, 10.0);

    fn button_event(pressed: bool, pos: egui::Pos2) -> egui::Event {
        egui::Event::PointerButton {
            pos,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: egui::Modifiers::NONE,
        }
    }

    /// A key press with the auto-repeat flag set, as a held key delivers.
    fn key_event(key: egui::Key) -> egui::Event {
        egui::Event::Key {
            key,
            physical_key: None,
            pressed: true,
            repeat: true,
            modifiers: egui::Modifiers::NONE,
        }
    }

    /// Hover, then press the mounted slider: the two frames that open a
    /// pointer drag. The clock stands at the end of the press frame.
    fn hover_and_press(
        context: &egui::Context,
        edits: &mut EditSession,
        value: &mut f32,
        saw_drag_stopped: &mut bool,
    ) {
        slider_frame(
            context,
            FRAME,
            vec![egui::Event::PointerMoved(PRESS)],
            true,
            edits,
            value,
            saw_drag_stopped,
        );
        slider_frame(
            context,
            2.0 * FRAME,
            vec![button_event(true, PRESS)],
            true,
            edits,
            value,
            saw_drag_stopped,
        );
    }

    /// Runs one egui frame over a bare exposure slider, mounted or not, and
    /// wires the response into the real `apply_adjustment_interaction` exactly
    /// as `record_slider` does in the adjustment panel.
    fn slider_frame(
        context: &egui::Context,
        time: f64,
        events: Vec<egui::Event>,
        mounted: bool,
        edits: &mut EditSession,
        value: &mut f32,
        saw_drag_stopped: &mut bool,
    ) {
        let _ = context.run_ui(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(300.0, 400.0),
                )),
                time: Some(time),
                events,
                ..Default::default()
            },
            |root| {
                egui::CentralPanel::default()
                    .frame(egui::Frame::NONE)
                    .show(root, |ui| {
                        if !mounted {
                            return;
                        }
                        let response = ui.add(egui::Slider::new(value, -1.0_f32..=1.0));
                        if response.changed() || response.drag_started() || response.drag_stopped()
                        {
                            *saw_drag_stopped |= response.drag_stopped();
                            let _ = apply_adjustment_interaction(
                                edits,
                                AdjustmentInteraction {
                                    target: AdjustmentTarget::Exposure,
                                    value: *value,
                                    changed: response.changed(),
                                    drag_started: response.drag_started(),
                                    dragged: response.dragged(),
                                    drag_stopped: response.drag_stopped(),
                                    reset: false,
                                },
                                None,
                            );
                        }
                    });
            },
        );
    }

    /// The `update_autosave` sequence for one beat: reap, then observe, then
    /// take, through the same `reap_orphaned_gesture` the app calls. Returns
    /// whether the beat schedules a save.
    fn reap_and_observe(
        edits: &mut EditSession,
        save: &mut Autosave,
        pointer_down: bool,
        now: Instant,
    ) -> bool {
        reap_orphaned_gesture(edits, pointer_down);
        save.observe(edits.revision(), edits.gesture_active(), now);
        save.take_due(now)
    }

    #[test]
    fn only_an_open_gesture_without_a_held_pointer_is_orphaned() {
        assert!(!is_gesture_orphaned(false, false));
        assert!(!is_gesture_orphaned(false, true));
        assert!(is_gesture_orphaned(true, false));
        // A legitimate multi-frame hold: never orphaned while down.
        for _ in 0..600 {
            assert!(!is_gesture_orphaned(true, true));
        }
    }

    #[test]
    fn releasing_while_the_slider_is_unmounted_stalls_autosave_and_the_reap_resumes_it() {
        let context = egui::Context::default();
        let mut edits = EditSession::default();
        let mut save = Autosave::default();
        let mut value = 0.0_f32;
        let mut saw_drag_stopped = false;
        let base = Instant::now();
        let revision_before_drag = edits.revision();

        // Frames 1 and 2: egui latches the drag on the press frame, so the
        // interaction must be applied there too, not only on moves.
        hover_and_press(&context, &mut edits, &mut value, &mut saw_drag_stopped);
        assert!(edits.gesture_active());
        assert_eq!(edits.recipe().light.exposure_ev, value);

        // Frame 3: a modest drag moves the live value into the open gesture.
        slider_frame(
            &context,
            3.0 * FRAME,
            vec![egui::Event::PointerMoved(DRAG_TO)],
            true,
            &mut edits,
            &mut value,
            &mut saw_drag_stopped,
        );
        assert!(edits.revision() > revision_before_drag);
        assert_eq!(edits.recipe().light.exposure_ev, value);

        // Frames 4 and 5: the panel swaps out (the slider unmounts) while the
        // button is still down, and the release arrives to nothing.
        slider_frame(
            &context,
            4.0 * FRAME,
            vec![],
            false,
            &mut edits,
            &mut value,
            &mut saw_drag_stopped,
        );
        slider_frame(
            &context,
            5.0 * FRAME,
            vec![button_event(false, DRAG_TO)],
            false,
            &mut edits,
            &mut value,
            &mut saw_drag_stopped,
        );

        // The defect (#14): no `drag_stopped` ever reached the session, so the
        // gesture stays open and autosave stalls forever.
        assert!(!saw_drag_stopped);
        assert!(edits.gesture_active());
        let stalled_at = base + Duration::from_secs(10);
        save.observe(edits.revision(), edits.gesture_active(), stalled_at);
        assert!(!save.take_due(stalled_at));

        // Frame 6, the real `update_autosave` path: the button is up, so the
        // reap commits the orphaned drag with end-of-drag semantics.
        let pointer_down = context.input(|input| input.pointer.any_down());
        assert!(!pointer_down);
        let reaped_at = base + Duration::from_secs(10);
        reap_and_observe(&mut edits, &mut save, pointer_down, reaped_at);
        assert!(!edits.gesture_active());

        // Exactly one undo step for the whole drag.
        assert!(edits.can_undo());
        assert!(edits.undo());
        assert_eq!(edits.recipe().light.exposure_ev, EXPOSURE_EV_RANGE.neutral);
        assert!(!edits.undo());

        // Autosave resumes on the normal released-plus-one-second debounce.
        assert!(!save.take_due(reaped_at));
        assert!(save.take_due(reaped_at + Duration::from_secs(1)));
    }

    #[test]
    fn a_mounted_release_finishes_itself_and_the_watchdog_stays_idle() {
        let context = egui::Context::default();
        let mut edits = EditSession::default();
        let mut save = Autosave::default();
        let mut value = 0.0_f32;
        let mut saw_drag_stopped = false;
        let base = Instant::now();

        hover_and_press(&context, &mut edits, &mut value, &mut saw_drag_stopped);
        slider_frame(
            &context,
            3.0 * FRAME,
            vec![egui::Event::PointerMoved(DRAG_TO)],
            true,
            &mut edits,
            &mut value,
            &mut saw_drag_stopped,
        );
        // The release frame renders the slider: the widget itself finishes.
        slider_frame(
            &context,
            4.0 * FRAME,
            vec![button_event(false, DRAG_TO)],
            true,
            &mut edits,
            &mut value,
            &mut saw_drag_stopped,
        );

        assert!(saw_drag_stopped);
        assert!(!edits.gesture_active());
        let pointer_down = context.input(|input| input.pointer.any_down());
        assert!(!is_gesture_orphaned(edits.gesture_active(), pointer_down));

        // The watchdog beat finds nothing to reap; the drag is one undo step.
        let committed = edits.recipe().light.exposure_ev;
        assert!(!reap_and_observe(&mut edits, &mut save, pointer_down, base));
        assert!(edits.undo());
        assert_eq!(edits.recipe().light.exposure_ev, EXPOSURE_EV_RANGE.neutral);
        assert!(!edits.undo());
        assert!(edits.redo());
        assert_eq!(edits.recipe().light.exposure_ev, committed);
        assert!(!save.take_due(base));
        assert!(save.take_due(base + Duration::from_secs(1)));
    }

    #[test]
    fn a_six_hundred_frame_hold_is_never_reaped() {
        let context = egui::Context::default();
        let mut edits = EditSession::default();
        let mut save = Autosave::default();
        let mut value = 0.0_f32;
        let mut saw_drag_stopped = false;
        let base = Instant::now();

        hover_and_press(&context, &mut edits, &mut value, &mut saw_drag_stopped);

        // 600 eventless frames with the button held: `any_down` is persistent
        // state, so the watchdog sees a real hold on every beat.
        for frame in 3..=602 {
            slider_frame(
                &context,
                frame as f64 * FRAME,
                vec![],
                true,
                &mut edits,
                &mut value,
                &mut saw_drag_stopped,
            );
            let pointer_down = context.input(|input| input.pointer.any_down());
            assert!(pointer_down);
            reap_and_observe(
                &mut edits,
                &mut save,
                pointer_down,
                base + Duration::from_secs_f64(frame as f64 * FRAME),
            );
            assert!(edits.gesture_active());
        }
        // Still open: a long legitimate hold must never be reaped, and nothing
        // was committed to the undo stack mid-hold.
        assert!(edits.gesture_active());
        assert!(!saw_drag_stopped);
    }

    /// A keyboard step on a focused slider reports only `changed` — egui sets
    /// no drag flag without a pointer — so `apply_adjustment_interaction`
    /// takes its `set_discrete` branch and never opens a gesture. The watchdog
    /// therefore has nothing to commit: a held arrow key keeps producing one
    /// discrete undo step per repeat, exactly as before the watchdog existed.
    #[test]
    fn held_arrow_keys_keep_discrete_undo_steps_under_the_watchdog() {
        let context = egui::Context::default();
        let mut edits = EditSession::default();
        let mut save = Autosave::default();
        let mut value = 0.0_f32;
        let mut saw_drag_stopped = false;
        let base = Instant::now();

        // Tab focuses the slider; one settling frame installs its arrow-key
        // focus filter. No pointer is ever pressed.
        slider_frame(
            &context,
            FRAME,
            vec![key_event(egui::Key::Tab)],
            true,
            &mut edits,
            &mut value,
            &mut saw_drag_stopped,
        );
        slider_frame(
            &context,
            2.0 * FRAME,
            vec![],
            true,
            &mut edits,
            &mut value,
            &mut saw_drag_stopped,
        );

        // Two held-arrow repeats on consecutive frames, each followed by the
        // watchdog beat exactly as `update` runs it: widgets first, then reap.
        for frame in 3..=4 {
            slider_frame(
                &context,
                frame as f64 * FRAME,
                vec![key_event(egui::Key::ArrowRight)],
                true,
                &mut edits,
                &mut value,
                &mut saw_drag_stopped,
            );
            let pointer_down = context.input(|input| input.pointer.any_down());
            assert!(!pointer_down);
            assert!(
                !edits.gesture_active(),
                "a keyboard step must not open a gesture the reap could commit"
            );
            reap_and_observe(
                &mut edits,
                &mut save,
                pointer_down,
                base + Duration::from_secs_f64(frame as f64 * FRAME),
            );
        }
        assert!(value > 0.0, "the arrow keys moved the focused slider");
        assert_eq!(edits.recipe().light.exposure_ev, value);
        assert!(!saw_drag_stopped);

        // One undo step per key press — the upstream granularity, unchanged.
        assert!(edits.undo());
        assert!(edits.recipe().light.exposure_ev > EXPOSURE_EV_RANGE.neutral);
        assert!(edits.undo());
        assert_eq!(edits.recipe().light.exposure_ev, EXPOSURE_EV_RANGE.neutral);
        assert!(!edits.undo());
    }

    /// The wheel half of the same concern: egui's sliders do not listen for
    /// wheel events at all (0.33), and the only wheel consumer in the app is
    /// viewport zoom, which edits no recipe. The wheel can thus open no
    /// gesture and leave nothing for the reap to commit.
    #[test]
    fn wheel_events_over_a_slider_edit_nothing_the_watchdog_could_commit() {
        let context = egui::Context::default();
        let mut edits = EditSession::default();
        let mut save = Autosave::default();
        let mut value = 0.0_f32;
        let mut saw_drag_stopped = false;
        let base = Instant::now();

        // Hover only: the wheel acts on the hovered slider, button up.
        slider_frame(
            &context,
            FRAME,
            vec![egui::Event::PointerMoved(PRESS)],
            true,
            &mut edits,
            &mut value,
            &mut saw_drag_stopped,
        );
        for frame in 2..=3 {
            slider_frame(
                &context,
                frame as f64 * FRAME,
                vec![egui::Event::MouseWheel {
                    unit: egui::MouseWheelUnit::Point,
                    phase: egui::TouchPhase::Move,
                    delta: egui::vec2(0.0, 24.0),
                    modifiers: egui::Modifiers::NONE,
                }],
                true,
                &mut edits,
                &mut value,
                &mut saw_drag_stopped,
            );
            let pointer_down = context.input(|input| input.pointer.any_down());
            assert!(!pointer_down);
            reap_and_observe(
                &mut edits,
                &mut save,
                pointer_down,
                base + Duration::from_secs_f64(frame as f64 * FRAME),
            );
            assert!(!edits.gesture_active());
        }
        assert_eq!(value, 0.0, "the wheel must not move a slider value");
        assert!(!edits.can_undo());
        assert!(!saw_drag_stopped);
    }
}
