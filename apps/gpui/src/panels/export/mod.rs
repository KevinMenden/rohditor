//! The export dialog: format, size, sharpening and metadata, then where to save.

mod fields;

use gpui::{
    App, Context, DismissEvent, Div, Entity, EventEmitter, FocusHandle, Focusable, IntoElement,
    Render, Role, SharedString, Window, div, prelude::*, px,
};
use rohditor_core::{OutputMedium, OutputSharpening, OutputSharpeningStrength};
use rohditor_engine::export_options::{ExportKind, ExportOptions, PngDepth, SizeMode};

use crate::session::Session;
use crate::theme::{RADIUS, SMALL_FONT_SIZE, theme};
use crate::ui::button::button;
use crate::ui::choice::choice;
use crate::ui::keys::{Cancel, Confirm};
use crate::ui::test_id::TestId as _;
use crate::ui::text_input::TextInput;
use crate::ui::toggle::toggle;
use fields::Fields;

const KINDS: [(ExportKind, &str); 2] = [(ExportKind::Jpeg, "JPEG"), (ExportKind::Png, "PNG")];
const DEPTHS: [(PngDepth, &str); 2] = [(PngDepth::Eight, "8-bit"), (PngDepth::Sixteen, "16-bit")];
const SIZES: [(SizeMode, &str); 3] = [
    (SizeMode::Original, "Original"),
    (SizeMode::LongEdge, "Long edge"),
    (SizeMode::Fit, "Fit in box"),
];
const MEDIA: [(Option<OutputMedium>, &str); 4] = [
    (None, "Off"),
    (Some(OutputMedium::Screen), "Screen"),
    (Some(OutputMedium::Matte), "Matte"),
    (Some(OutputMedium::Glossy), "Glossy"),
];
const STRENGTHS: [(OutputSharpeningStrength, &str); 3] = [
    (OutputSharpeningStrength::Low, "Low"),
    (OutputSharpeningStrength::Standard, "Standard"),
    (OutputSharpeningStrength::High, "High"),
];

pub struct ExportDialog {
    session: Entity<Session>,
    focus: FocusHandle,
    options: ExportOptions,
    fields: Fields,
    error: Option<String>,
}

impl EventEmitter<DismissEvent> for ExportDialog {}

impl Focusable for ExportDialog {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl ExportDialog {
    /// Starts with the options of the last export.
    pub fn new(session: Entity<Session>, options: ExportOptions, cx: &mut Context<Self>) -> Self {
        let fields = Fields::new(
            &options,
            |this: &mut Self, cx| {
                this.error = None;
                cx.notify();
            },
            cx,
        );
        Self {
            session,
            focus: cx.focus_handle(),
            options,
            fields,
            error: None,
        }
    }

    /// Stays open if a number is wrong, so that it can be put right.
    fn accept(&mut self, cx: &mut Context<Self>) {
        let mut options = self.options.clone();
        if let Err(error) = self.fields.read_into(&mut options, cx) {
            self.error = Some(error);
            cx.notify();
            return;
        }
        cx.emit(DismissEvent);
        self.session
            .update(cx, |session, cx| session.export_with(options, cx));
    }

    /// What a choice does: change one option and redraw.
    fn set<T: Copy + 'static>(
        &self,
        cx: &mut Context<Self>,
        change: fn(&mut ExportOptions, T),
    ) -> impl Fn(T, &mut Window, &mut App) + 'static {
        let dialog = cx.entity();
        move |value, _, cx| {
            dialog.update(cx, |dialog, cx| {
                change(&mut dialog.options, value);
                dialog.error = None;
                cx.notify();
            });
        }
    }

    fn flag(
        &self,
        cx: &mut Context<Self>,
        id: &'static str,
        label: &'static str,
        on: bool,
        flip: fn(&mut ExportOptions),
    ) -> impl IntoElement + use<> {
        let dialog = cx.entity();
        toggle(id, label, on, move |_, cx| {
            dialog.update(cx, |dialog, cx| {
                flip(&mut dialog.options);
                cx.notify();
            });
        })
    }

    fn format(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let kind = self.options.kind;
        let format = choice(
            "format",
            "Format",
            &KINDS,
            kind,
            self.set(cx, |options, kind| options.kind = kind),
        );
        let detail = match kind {
            ExportKind::Jpeg => row("JPEG quality", number_field(&self.fields.quality)),
            ExportKind::Png => row(
                "Bit depth",
                choice(
                    "depth",
                    "Bit depth",
                    &DEPTHS,
                    self.options.png_depth,
                    self.set(cx, |options, depth| options.png_depth = depth),
                ),
            ),
        };
        div()
            .flex()
            .flex_col()
            .gap_2()
            .child(row("Format", format))
            .child(detail)
    }

    /// Output size: original, long edge, or bounding box. Never upscales.
    fn size(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let mode = self.options.size_mode;
        let modes = choice(
            "size",
            "Size",
            &SIZES,
            mode,
            self.set(cx, |options, mode| options.size_mode = mode),
        );
        let unit = || div().text_color(theme().muted).child("px");
        let limits = match mode {
            SizeMode::Original => None,
            SizeMode::LongEdge => Some(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(number_field(&self.fields.long_edge))
                    .child(unit()),
            ),
            SizeMode::Fit => Some(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(number_field(&self.fields.fit_width))
                    .child(div().text_color(theme().muted).child("×"))
                    .child(number_field(&self.fields.fit_height))
                    .child(unit()),
            ),
        };
        div()
            .flex()
            .flex_col()
            .gap_2()
            .child(row("Size", modes))
            .children(limits.map(|limits| row("", limits)))
    }

    /// Applied on export only, so the preview does not show it.
    fn sharpening(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let current = self.options.sharpening;
        let medium = choice(
            "sharpening",
            "Output sharpening",
            &MEDIA,
            current.map(|sharpening| sharpening.medium),
            self.set(cx, |options, medium: Option<OutputMedium>| {
                let strength = options
                    .sharpening
                    .map_or_else(Default::default, |s| s.strength);
                options.sharpening = medium.map(|medium| OutputSharpening { medium, strength });
            }),
        );
        let strength = current.map(|sharpening| {
            choice(
                "sharpening-strength",
                "Sharpening strength",
                &STRENGTHS,
                sharpening.strength,
                self.set(cx, |options, strength| {
                    if let Some(sharpening) = options.sharpening.as_mut() {
                        sharpening.strength = strength;
                    }
                }),
            )
        });
        let note = current.map(|_| {
            div()
                .text_size(SMALL_FONT_SIZE)
                .text_color(theme().faint)
                .child("Applied on export only; not shown in the preview.")
        });
        div()
            .flex()
            .flex_col()
            .gap_2()
            .child(row("Sharpening", medium))
            .children(strength.map(|strength| row("Strength", strength)))
            .children(note)
    }

    fn switches(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let options = &self.options;
        div()
            .flex()
            .flex_col()
            .child(self.flag(
                cx,
                "dither",
                "Ordered output dithering",
                options.dither,
                |options| options.dither ^= true,
            ))
            .child(self.flag(
                cx,
                "metadata",
                "Include safe EXIF metadata",
                options.safe_metadata,
                |options| options.safe_metadata ^= true,
            ))
            .child(self.flag(
                cx,
                "overwrite",
                "Allow replacing an existing file",
                options.overwrite,
                |options| options.overwrite ^= true,
            ))
    }
}

impl Render for ExportDialog {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = theme();
        div()
            .id("export-dialog")
            .role(Role::Dialog)
            .aria_label("Export")
            .test_id("export-dialog")
            .track_focus(&self.focus)
            .on_action(cx.listener(|this, _: &Confirm, _, cx| this.accept(cx)))
            .on_action(cx.listener(|_, _: &Cancel, _, cx| cx.emit(DismissEvent)))
            .w(px(440.0))
            .flex()
            .flex_col()
            .gap_3()
            .p_4()
            .rounded(px(8.0))
            .border_1()
            .border_color(theme.border)
            .bg(theme.elevated)
            .shadow_lg()
            .child("Export")
            .child(self.format(cx))
            .child(self.size(cx))
            .child(self.sharpening(cx))
            .child(self.switches(cx))
            .children(self.error.clone().map(|error| {
                div()
                    .test_id("export-error")
                    .relative()
                    .text_size(SMALL_FONT_SIZE)
                    .text_color(theme.error)
                    .child(error)
            }))
            .child(
                div()
                    .flex()
                    .justify_end()
                    .gap_2()
                    .child(button(
                        "cancel",
                        "Cancel",
                        cx.listener(|_, _, _, cx| cx.emit(DismissEvent)),
                    ))
                    .child(button(
                        "accept",
                        "Export…",
                        cx.listener(|this, _, _, cx| this.accept(cx)),
                    )),
            )
    }
}

/// A label with what it names.
fn row(label: impl Into<SharedString>, content: impl IntoElement) -> Div {
    div()
        .flex()
        .items_start()
        .gap_3()
        .child(
            div()
                .flex_none()
                .w(px(110.0))
                .py_0p5()
                .text_size(SMALL_FONT_SIZE)
                .text_color(theme().muted)
                .child(label.into()),
        )
        .child(div().flex_1().child(content))
}

fn number_field(input: &Entity<TextInput>) -> impl IntoElement + use<> {
    div()
        .flex()
        .items_center()
        .w(px(96.0))
        .h(px(26.0))
        .px_2()
        .rounded(RADIUS)
        .bg(theme().editor)
        .child(input.clone())
}
