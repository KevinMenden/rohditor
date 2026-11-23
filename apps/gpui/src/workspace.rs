//! The window: title bar, docks, tab, viewport and status bar.

mod chrome;

use gpui::{
    App, Context, DismissEvent, Entity, FocusHandle, Focusable, IntoElement, PathPromptOptions,
    Pixels, Render, StyleRefinement, Window, div, prelude::*, px,
};

use crate::actions::{
    ActualPixels, AutoTone, CloseFile, Export, NextPhoto, OpenFile, OpenFolder, PreviousPhoto,
    Quit, Redo, ResetAdjustments, Save, ToggleCommandPalette, ToggleDevelop, ToggleFiles, Undo,
    WORKSPACE_CONTEXT, ZoomToFit,
};
use crate::library::Library;
use crate::panels::command_palette::CommandPalette;
use crate::panels::develop::DevelopPanel;
use crate::panels::files::FilesPanel;
use crate::session::Session;
use crate::theme::{DEVELOP_WIDTH, FILES_WIDTH, UI_FONT_SIZE, theme};
use crate::viewport::Viewport;

pub struct Workspace {
    session: Entity<Session>,
    library: Entity<Library>,
    files: Entity<FilesPanel>,
    develop: Entity<DevelopPanel>,
    viewport: Entity<Viewport>,
    palette: Option<Entity<CommandPalette>>,
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
        cx.observe_in(&session, window, |_, session, window, cx| {
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
            files: cx.new(|cx| FilesPanel::new(session.clone(), library.clone(), cx)),
            develop: cx.new(|cx| DevelopPanel::new(session.clone(), cx)),
            viewport: cx.new(|cx| Viewport::new(session.clone(), cx)),
            session,
            library,
            palette: None,
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
            Ok(Err(error)) => log::error!("the file dialog failed: {error:#}"),
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
        if self.palette.is_some() {
            return self.close_palette(window, cx);
        }
        let palette = cx.new(CommandPalette::new);
        palette.focus_handle(cx).focus(window, cx);
        cx.subscribe_in(&palette, window, |this, _, _: &DismissEvent, window, cx| {
            this.close_palette(window, cx);
        })
        .detach();
        self.palette = Some(palette);
        cx.notify();
    }

    fn close_palette(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.palette.take().is_some() {
            self.focus.focus(window, cx);
            cx.notify();
        }
    }

    /// Attach the handler of every action the workspace performs.
    fn with_actions(&self, root: gpui::Div, cx: &mut Context<Self>) -> gpui::Div {
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
        );
        root.on_action(cx.listener(|this, _: &NextPhoto, _, cx| this.open_adjacent(true, cx)))
            .on_action(cx.listener(|this, _: &PreviousPhoto, _, cx| this.open_adjacent(false, cx)))
            .on_action(cx.listener(|this, _: &OpenFile, _, cx| this.prompt(false, cx)))
            .on_action(cx.listener(|this, _: &OpenFolder, _, cx| this.prompt(true, cx)))
            .on_action(cx.listener(|this, _: &ZoomToFit, window, cx| {
                this.viewport
                    .update(cx, |viewport, cx| viewport.zoom_to_fit(window, cx));
            }))
            .on_action(cx.listener(|this, _: &ActualPixels, window, cx| {
                this.viewport
                    .update(cx, |viewport, cx| viewport.actual_pixels(window, cx));
            }))
            .on_action(cx.listener(|this, _: &ToggleFiles, _, cx| {
                this.files_open = !this.files_open;
                cx.notify();
            }))
            .on_action(cx.listener(|this, _: &ToggleDevelop, _, cx| {
                this.develop_open = !this.develop_open;
                cx.notify();
            }))
            .on_action(cx.listener(Self::toggle_palette))
            .on_action(cx.listener(|_, _: &Quit, _, cx| cx.quit()))
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
            .key_context(WORKSPACE_CONTEXT)
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
            .children(self.palette.clone().map(|palette| {
                div()
                    .absolute()
                    .top_0()
                    .left_0()
                    .size_full()
                    .flex()
                    .justify_center()
                    .items_start()
                    .pt(px(90.0))
                    .child(palette)
            }))
            .children(chrome::resize_edges(window))
    }
}

#[cfg(test)]
impl Workspace {
    pub fn panel_renders(&self, cx: &App) -> (usize, usize) {
        (self.files.read(cx).renders, self.develop.read(cx).renders)
    }
}
