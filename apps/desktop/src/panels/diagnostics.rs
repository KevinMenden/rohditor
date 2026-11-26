//! Left dock: the developer diagnostics, with a support-safe report to save. The engine gathers
//! them ([`rohditor_engine::diagnostics`]); this only lays them out.

use std::path::PathBuf;

use gpui::{Context, Entity, IntoElement, Render, Role, SharedString, Window, div, prelude::*};
use rohditor_engine::diagnostics::{Diagnostics, GpuDiagnostics};

use crate::library::Library;
use crate::session::Session;
use crate::theme::{SMALL_FONT_SIZE, theme};
use crate::ui::button::button;
use crate::ui::overlays::{Overlays, ToastKind};
use crate::ui::test_id::TestId as _;

pub struct DiagnosticsPanel {
    session: Entity<Session>,
    library: Entity<Library>,
    overlays: Entity<Overlays>,
}

impl DiagnosticsPanel {
    pub fn new(
        session: Entity<Session>,
        library: Entity<Library>,
        overlays: Entity<Overlays>,
        cx: &mut Context<Self>,
    ) -> Self {
        cx.observe(&session, |_, _, cx| cx.notify()).detach();
        cx.observe(&library, |_, _, cx| cx.notify()).detach();
        Self {
            session,
            library,
            overlays,
        }
    }

    fn diagnostics(&self, cx: &Context<Self>) -> Diagnostics {
        let catalog = self.library.read(cx).catalog();
        self.session.read(cx).diagnostics(catalog)
    }

    /// Ask where to, then save the diagnostics as they are now.
    fn save_report(&mut self, cx: &mut Context<Self>) {
        let diagnostics = self.diagnostics(cx);
        let directory = self
            .library
            .read(cx)
            .folder()
            .map(PathBuf::from)
            .unwrap_or_default();
        let chosen = cx.prompt_for_new_path(&directory, Some("rohditor-diagnostics.json"));
        let overlays = self.overlays.clone();
        cx.spawn(async move |_, cx| {
            let result = match chosen.await {
                Ok(Ok(Some(mut path))) => {
                    if path.extension().is_none() {
                        path.set_extension("json");
                    }
                    rohditor_engine::diagnostics::save_report(&diagnostics, &path)
                        .map(|()| format!("Saved the diagnostics report to {}.", path.display()))
                }
                Ok(Err(error)) => Err(format!("The file dialog failed: {error:#}")),
                Ok(Ok(None)) | Err(_) => return,
            };
            overlays.update(cx, |overlays, cx| match result {
                Ok(message) => overlays.toast(ToastKind::Success, message, cx),
                Err(error) => overlays.toast(ToastKind::Error, error, cx),
            });
        })
        .detach();
    }
}

impl Render for DiagnosticsPanel {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = theme();
        let diagnostics = self.diagnostics(cx);
        let note = |text: String| {
            div()
                .text_size(SMALL_FONT_SIZE)
                .text_color(theme.muted)
                .child(text)
        };
        let mut body = div()
            .flex()
            .flex_col()
            .gap_1()
            .child(div().child(format!("Processor: {}", diagnostics.processor)))
            .child(note(format!("{} UI", diagnostics.ui_renderer)))
            .children(diagnostics.gpu_device.clone().map(note))
            .children(diagnostics.messages.labelled().map(|(label, message)| {
                div()
                    .text_size(SMALL_FONT_SIZE)
                    .text_color(theme.warning)
                    .child(format!("{label}: {message}"))
            }))
            .child(section("Preview queue", diagnostics.queue_rows()))
            .children(
                diagnostics
                    .catalog
                    .map(|catalog| section("Catalog", catalog.rows())),
            );
        match &diagnostics.preview {
            None => body = body.child(note("No developed preview diagnostics yet.".to_owned())),
            Some(preview) => {
                body = body
                    .child(heading(format!("Last {} preview", preview.backend)))
                    .children(preview.summary().into_iter().map(note))
                    .child(rows(preview.cache.rows()))
                    .child(section("CPU stage wall times", preview.timings.rows()))
                    .children(preview.highlight.lines().into_iter().map(note))
                    .child(note(preview.memory()));
                if let Some(gpu) = preview.gpu {
                    body = body
                        .child(section("GPU preview", gpu.rows()))
                        .child(note(GpuDiagnostics::NOTE.to_owned()));
                }
            }
        }
        div()
            .id("diagnostics")
            .role(Role::Group)
            .aria_label("Diagnostics")
            .test_id("diagnostics-panel")
            .size_full()
            .flex()
            .flex_col()
            .overflow_y_scroll()
            .p_2()
            .gap_2()
            .child(div().flex().justify_end().child(button(
                "save-report",
                "Save Report…",
                cx.listener(|this, _, _, cx| this.save_report(cx)),
            )))
            .child(body)
    }
}

fn heading(title: impl Into<SharedString>) -> impl IntoElement {
    div()
        .pt_3()
        .text_size(SMALL_FONT_SIZE)
        .text_color(theme().text)
        .child(title.into())
}

fn section(title: &'static str, values: Vec<(&'static str, String)>) -> impl IntoElement {
    div().child(heading(title)).child(rows(values))
}

/// Labels on the left, their values on the right.
fn rows(values: Vec<(&'static str, String)>) -> impl IntoElement {
    let theme = theme();
    div()
        .flex()
        .flex_col()
        .text_size(SMALL_FONT_SIZE)
        .children(values.into_iter().map(move |(label, value)| {
            div()
                .flex()
                .justify_between()
                .gap_2()
                .child(div().text_color(theme.muted).child(label))
                .child(div().text_right().child(value))
        }))
}
