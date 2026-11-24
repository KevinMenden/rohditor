//! The window: title bar, docks, tab, viewport and status bar.

mod chrome;
mod notifications;

use gpui::{
    App, Context, Div, Entity, FocusHandle, Focusable, IntoElement, PathPromptOptions, Pixels,
    Render, Role, Stateful, StyleRefinement, Window, div, prelude::*,
};

use rohditor_engine::tools::Picker;

use crate::actions::{
    ActualPixels, ApplyPreset, AutoNoiseReduction, AutoTone, ChooseLens, CloseFile, Export,
    FocusAdjustments, FocusPhoto, ImportCameraProfile, NextPhoto, OpenFile, OpenFolder,
    PickMixerColor, PickWhiteBalance, PreviousPhoto, Quit, Redo, ResetAdjustments, ResetSection,
    Save, SavePreset, SearchAdjustments, SetHighlightMethod, SetLensProfile, SetRendering,
    SetWhiteBalance, ToggleClipping, ToggleCommandPalette, ToggleDevelop, ToggleFiles, Undo,
    WORKSPACE_CONTEXT, ZoomToFit,
};
use crate::library::Library;
use crate::panels::command_palette::command_palette;
use crate::panels::develop::DevelopPanel;
use crate::panels::files::FilesPanel;
use crate::session::Session;
use crate::theme::{DEVELOP_WIDTH, FILES_WIDTH, UI_FONT_SIZE, theme};
use crate::ui::confirm::Confirm;
use crate::ui::keys::{FocusNext, FocusPrevious};
use crate::ui::overlays::{Overlays, ToastKind};
use crate::ui::search_list::SearchList;
use crate::viewport::Viewport;
use notifications::Announced;

pub struct Workspace {
    session: Entity<Session>,
    library: Entity<Library>,
    files: Entity<FilesPanel>,
    develop: Entity<DevelopPanel>,
    viewport: Entity<Viewport>,
    overlays: Entity<Overlays>,
    announced: Announced,
    files_open: bool,
    develop_open: bool,
    focus: FocusHandle,
}

impl Focusable for Workspace {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Workspace {
    pub fn new(
        session: Entity<Session>,
        library: Entity<Library>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let focus = cx.focus_handle();
        focus.focus(window, cx);
        // The focused control went away.
        cx.on_focus_lost(window, |this, window, cx| this.focus.focus(window, cx))
            .detach();
        let this = cx.entity();
        window.on_window_should_close(cx, move |window, cx| {
            this.update(cx, |this, cx| this.may_quit(window, cx))
        });
        let overlays = cx.new(|_| Overlays::default());
        // An open menu or dialog changes which keys work here.
        cx.observe(&overlays, |_, _, cx| cx.notify()).detach();
        cx.observe_in(&session, window, |this, session, window, cx| {
            this.announce(cx);
            let title = session.read(cx).document().map_or_else(
                || "Rohditor".to_owned(),
                |d| format!("{} — Rohditor", d.file_name()),
            );
            window.set_window_title(&title);
            cx.notify();
        })
        .detach();
        cx.observe(&library, |_, _, cx| cx.notify()).detach();
        Self {
            files: cx
                .new(|cx| FilesPanel::new(session.clone(), library.clone(), overlays.clone(), cx)),
            develop: cx.new(|cx| DevelopPanel::new(session.clone(), overlays.clone(), window, cx)),
            viewport: cx.new(|cx| Viewport::new(session.clone(), cx)),
            session,
            library,
            overlays,
            announced: Announced::default(),
            files_open: true,
            develop_open: true,
            focus,
        }
    }

    fn prompt(&mut self, folder: bool, cx: &mut Context<Self>) {
        let chosen = cx.prompt_for_paths(PathPromptOptions {
            files: !folder,
            directories: folder,
            multiple: false,
            prompt: None,
        });
        let (session, library) = (self.session.clone(), self.library.clone());
        let overlays = self.overlays.clone();
        cx.spawn(async move |_, cx| match chosen.await {
            Ok(Ok(Some(paths))) => {
                if let Some(path) = paths.into_iter().next() {
                    if folder {
                        library.update(cx, |library, cx| library.open_folder(path, cx));
                    } else {
                        session.update(cx, |session, cx| session.open_file(path, cx));
                    }
                }
            }
            Ok(Err(error)) => overlays.update(cx, |overlays, cx| {
                let text = format!("The file dialog failed: {error:#}");
                overlays.toast(ToastKind::Error, text, cx);
            }),
            Ok(Ok(None)) | Err(_) => {}
        })
        .detach();
    }

    /// Open the photo after or before the current one in the folder.
    fn open_adjacent(&mut self, forward: bool, cx: &mut Context<Self>) {
        let current = self.session.read(cx).document().map(|d| d.path.as_path());
        if let Some(path) = self.library.read(cx).adjacent(current, forward) {
            self.session
                .update(cx, |session, cx| session.open_file(path, cx));
        }
    }

    fn toggle_palette(
        &mut self,
        _: &ToggleCommandPalette,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.overlays.update(cx, |overlays, cx| {
            if overlays.dialog::<SearchList>().is_some() {
                overlays.close(window, cx);
            } else {
                let palette = cx.new(command_palette);
                overlays.show_dialog(palette, window, cx);
            }
        });
    }

    /// Whether nothing would be lost by quitting now. Asks the user otherwise.
    fn may_quit(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let exporting = self
            .session
            .read(cx)
            .document()
            .is_some_and(|document| document.export_status.is_some());
        if exporting {
            self.overlays.update(cx, |overlays, cx| {
                let question = cx.new(|cx| {
                    Confirm::new(
                        "Quit while exporting?",
                        "The export is not finished and would be lost.",
                        "Quit Anyway",
                        |_, cx| cx.quit(),
                        cx,
                    )
                });
                overlays.show_dialog(question, window, cx);
            });
        }
        !exporting
    }

    /// Attach the handler of every action the workspace performs.
    fn with_actions(&self, root: Stateful<Div>, cx: &mut Context<Self>) -> Stateful<Div> {
        macro_rules! on_session {
            ($root:expr, $($action:ident => $call:expr),* $(,)?) => {
                $root$(.on_action(cx.listener(|this, _: &$action, _, cx| {
                    let call: fn(&mut Session, &mut Context<Session>) = $call;
                    this.session.update(cx, call);
                })))*
            };
        }
        let root = on_session!(root,
            CloseFile => |session, cx| session.close_document(cx),
            Save => |session, cx| { session.save_now(); session.tick(cx); },
            Export => |session, cx| session.prompt_export(cx),
            Undo => |session, cx| session.undo(cx),
            Redo => |session, cx| session.redo(cx),
            ResetAdjustments => |session, cx| session.reset(cx),
            AutoTone => |session, cx| session.auto_tone(cx),
            PickWhiteBalance => |session, cx| session.toggle_picker(Picker::WhiteBalance, cx),
            PickMixerColor => |session, cx| session.toggle_picker(Picker::ColorMixer, cx),
            ImportCameraProfile => |session, cx| session.prompt_camera_profile(cx),
            ToggleClipping => |session, cx| session.toggle_clipping(cx),
        );
        root.on_action(cx.listener(|this, _: &NextPhoto, _, cx| this.open_adjacent(true, cx)))
            .on_action(cx.listener(|this, _: &PreviousPhoto, _, cx| this.open_adjacent(false, cx)))
            .on_action(cx.listener(|this, _: &OpenFile, _, cx| this.prompt(false, cx)))
            .on_action(cx.listener(|this, _: &OpenFolder, _, cx| this.prompt(true, cx)))
            .on_action(cx.listener(|this, _: &ZoomToFit, _, cx| {
                this.viewport.update(cx, Viewport::zoom_to_fit);
            }))
            .on_action(cx.listener(|this, _: &ActualPixels, _, cx| {
                this.viewport.update(cx, Viewport::actual_pixels);
            }))
            .on_action(cx.listener(|this, _: &ToggleFiles, _, cx| {
                this.files_open = !this.files_open;
                cx.notify();
            }))
            .on_action(cx.listener(|this, _: &ToggleDevelop, _, cx| {
                this.develop_open = !this.develop_open;
                cx.notify();
            }))
            .on_action(cx.listener(|this, action: &ResetSection, window, cx| {
                this.develop
                    .update(cx, |panel, cx| panel.reset_section(action.0, window, cx));
            }))
            .on_action(cx.listener(|this, _: &AutoNoiseReduction, _, cx| {
                this.develop.update(cx, |panel, cx| {
                    panel.choose(Session::auto_noise_reduction, cx);
                });
            }))
            .on_action(cx.listener(|this, action: &SetHighlightMethod, _, cx| {
                this.develop.update(cx, |panel, cx| {
                    panel.choose(|session, cx| session.set_highlight_method(action.0, cx), cx);
                });
            }))
            .on_action(cx.listener(|this, action: &SetWhiteBalance, _, cx| {
                this.develop.update(cx, |panel, cx| {
                    panel.choose(|session, cx| session.set_white_balance(action.0, cx), cx);
                });
            }))
            .on_action(cx.listener(|this, action: &SetLensProfile, _, cx| {
                this.develop.update(cx, |panel, cx| {
                    panel.choose(
                        |session, cx| session.set_lens_profile(action.0.clone(), cx),
                        cx,
                    );
                });
            }))
            .on_action(cx.listener(|this, _: &ApplyPreset, window, cx| {
                this.develop
                    .update(cx, |panel, cx| panel.choose_preset(window, cx));
            }))
            .on_action(cx.listener(|this, _: &SavePreset, window, cx| {
                this.develop
                    .update(cx, |panel, cx| panel.save_preset(window, cx));
            }))
            .on_action(cx.listener(|this, _: &ChooseLens, window, cx| {
                this.develop
                    .update(cx, |panel, cx| panel.choose_lens(window, cx));
            }))
            .on_action(cx.listener(|this, action: &SetRendering, _, cx| {
                this.develop.update(cx, |panel, cx| {
                    panel.choose(|session, cx| session.set_rendering(action.0, cx), cx);
                });
            }))
            .on_action(cx.listener(|this, _: &SearchAdjustments, window, cx| {
                this.develop_open = true;
                cx.notify();
                this.develop
                    .update(cx, |panel, cx| panel.focus_search(window, cx));
            }))
            .on_action(cx.listener(Self::toggle_palette))
            .on_action(cx.listener(|this, _: &FocusAdjustments, window, cx| {
                this.develop_open = true;
                cx.notify();
                this.develop
                    .update(cx, |panel, cx| panel.focus_adjustments(window, cx));
            }))
            .on_action(cx.listener(|this, _: &FocusPhoto, window, cx| {
                this.session.update(cx, Session::stop_picking);
                this.focus.focus(window, cx);
            }))
            .on_action(cx.listener(|this, _: &Quit, window, cx| {
                if this.may_quit(window, cx) {
                    cx.quit();
                }
            }))
            .on_action(cx.listener(|_, _: &FocusNext, window, cx| window.focus_next(cx)))
            .on_action(cx.listener(|_, _: &FocusPrevious, window, cx| window.focus_prev(cx)))
    }
}

/// A side panel, redrawn only when it notifies.
fn dock<P: Render>(panel: &Entity<P>, width: Pixels) -> impl IntoElement + use<P> {
    panel
        .clone()
        .cached(StyleRefinement::default().flex_none().w(width).h_full())
}

impl Render for Workspace {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = theme();
        let root = div()
            .id("workspace")
            .role(Role::Application)
            .aria_label("Rohditor")
            .when(!self.overlays.read(cx).is_open(), |root| {
                root.key_context(WORKSPACE_CONTEXT)
            })
            .track_focus(&self.focus)
            .relative()
            .size_full()
            .flex()
            .flex_col()
            .bg(theme.editor)
            .text_size(UI_FONT_SIZE)
            .text_color(theme.text);
        self.with_actions(root, cx)
            .child(self.title_bar(window, cx))
            .child(
                div()
                    .flex()
                    .flex_1()
                    .min_h_0()
                    .when(self.files_open, |body| {
                        body.child(dock(&self.files, FILES_WIDTH))
                    })
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .flex_1()
                            .min_w_0()
                            .children(self.tab(cx))
                            .child(self.viewport.clone()),
                    )
                    .when(self.develop_open, |body| {
                        body.child(dock(&self.develop, DEVELOP_WIDTH))
                    }),
            )
            .child(self.status_bar(window, cx))
            .child(self.overlays.clone())
            .children(chrome::resize_edges(window))
    }
}

#[cfg(test)]
impl Workspace {
    pub fn viewport(&self) -> &Entity<Viewport> {
        &self.viewport
    }

    pub fn panel_renders(&self, cx: &App) -> (usize, usize) {
        (self.files.read(cx).renders, self.develop.read(cx).renders)
    }
}
