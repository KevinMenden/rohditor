//! The settings dialog, after Zed's: searchable rows of a title, a description and the control,
//! grouped in sections. A change is used and saved at once; there is nothing to apply.

mod entries;

use gpui::{
    App, Context, DismissEvent, Entity, EventEmitter, FocusHandle, Focusable, IntoElement, Render,
    Role, SharedString, Window, div, prelude::*, px,
};
use rohditor_core::OutputPolicy;
use rohditor_demosaic::DemosaicAlgorithm;
use rohditor_engine::settings::AppSettings;

use crate::assets::Icon;
use crate::session::{ModelInstall, Session};
use crate::theme::{RADIUS, SMALL_FONT_SIZE, theme};
use crate::ui::button::{button, icon, icon_button};
use crate::ui::choice::choice_except;
use crate::ui::keys::Cancel;
use crate::ui::matching;
use crate::ui::test_id::TestId as _;
use crate::ui::text_input::{Changed, TextInput};
use crate::ui::toggle::switch;
use entries::{SECTIONS, Setting};

const DEMOSAICS: [(DemosaicAlgorithm, &str); 4] = [
    (DemosaicAlgorithm::MalvarHeCutler, "Malvar-He-Cutler"),
    (DemosaicAlgorithm::Rcd, "RCD"),
    (DemosaicAlgorithm::Amaze, "AMaZE"),
    (DemosaicAlgorithm::Neural, "Neural"),
];
const GAMUT_POLICIES: [(OutputPolicy, &str); 2] = [
    (OutputPolicy::ClipToSrgb, "Clip"),
    (OutputPolicy::ChromaCompressToSrgb, "Chroma compress"),
];

pub struct SettingsDialog {
    session: Entity<Session>,
    search: Entity<TextInput>,
}

impl EventEmitter<DismissEvent> for SettingsDialog {}

impl Focusable for SettingsDialog {
    /// The search has the focus when the dialog opens.
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.search.focus_handle(cx)
    }
}

impl SettingsDialog {
    pub fn new(session: Entity<Session>, cx: &mut Context<Self>) -> Self {
        let search = cx.new(|cx| TextInput::new("Search settings", "Search…", String::new(), cx));
        cx.subscribe(&search, |_, _, _: &Changed, cx| cx.notify())
            .detach();
        // Settings change through the session; show them as they are.
        cx.observe(&session, |_, _, cx| cx.notify()).detach();
        Self { session, search }
    }

    /// Escape clears the search first, then closes.
    fn cancel(&mut self, cx: &mut Context<Self>) {
        if self.search.read(cx).text().is_empty() {
            cx.emit(DismissEvent);
        } else {
            self.search.update(cx, |search, cx| search.clear(cx));
        }
    }

    /// What a control does: change one setting.
    fn change<T: 'static>(
        &self,
        set: fn(&mut AppSettings, T),
    ) -> impl Fn(T, &mut Window, &mut App) + 'static {
        let session = self.session.clone();
        move |value, _, cx| {
            session.update(cx, |session, cx| {
                session.change_settings(|settings| set(settings, value), cx);
            });
        }
    }

    fn header(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let theme = theme();
        let search = div()
            .test_id("settings-search")
            .relative()
            .flex()
            .flex_1()
            .items_center()
            .gap_1()
            .px_1p5()
            .h(px(28.0))
            .rounded(RADIUS)
            .bg(theme.editor)
            .child(icon(Icon::Search, theme.faint))
            .child(div().flex_1().min_w_0().child(self.search.clone()));
        div()
            .flex()
            .flex_none()
            .items_center()
            .gap_3()
            .px_4()
            .py_3()
            .border_b_1()
            .border_color(theme.border)
            .child(div().flex_none().child("Settings"))
            .child(search)
            .child(icon_button(
                "close-settings",
                Icon::Close,
                "Close",
                true,
                cx.listener(|_, _, _, cx| cx.emit(DismissEvent)),
            ))
    }

    /// The sections with the settings the search finds.
    fn sections(&self, cx: &mut Context<Self>) -> Vec<gpui::AnyElement> {
        let query = self.search.read(cx).text().to_owned();
        let mut shown = Vec::new();
        for (section, settings) in SECTIONS {
            let found: Vec<_> = settings
                .iter()
                .filter(|setting| matching::words(&setting.search_text(section), &query))
                .collect();
            if found.is_empty() {
                continue;
            }
            shown.push(section_title(section).into_any_element());
            for &setting in found {
                shown.push(self.row(setting, cx).into_any_element());
            }
        }
        if shown.is_empty() {
            let nothing = div()
                .py_6()
                .text_center()
                .text_color(theme().muted)
                .child("No settings match the search.");
            shown.push(nothing.into_any_element());
        }
        shown
    }

    fn row(&self, setting: Setting, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let theme = theme();
        let session = self.session.read(cx);
        let note = match setting {
            Setting::Demosaic => Some(session.neural_unavailable().map_or_else(
                || {
                    SharedString::from(
                        "Neural demosaics and denoises in the background; the preview shows \
                         AMaZE until it is ready.",
                    )
                },
                |reason| {
                    if matches!(session.model_install(), Some(ModelInstall::Installed(_))) {
                        "Neural can be chosen after Rohditor restarts with the installed model."
                            .into()
                    } else {
                        format!("Neural is unavailable: {reason}. Install a model below.").into()
                    }
                },
            )),
            Setting::NeuralModel => Some(model_note(session)),
            _ => None,
        };
        let text = div()
            .flex()
            .flex_col()
            .flex_1()
            .min_w_0()
            .gap_0p5()
            .child(setting.title())
            .child(
                div()
                    .text_size(SMALL_FONT_SIZE)
                    .text_color(theme.muted)
                    .child(setting.description()),
            )
            .children(note.map(|note| {
                div()
                    .text_size(SMALL_FONT_SIZE)
                    .text_color(theme.faint)
                    .child(note)
            }));
        div()
            .test_id(format!("setting:{}", setting.title()))
            .relative()
            .flex()
            .items_start()
            .justify_between()
            .gap_6()
            .py_2()
            .child(text)
            .child(
                div()
                    .flex()
                    .flex_none()
                    .justify_end()
                    .max_w(px(340.0))
                    .child(self.control(setting, cx)),
            )
    }

    fn control(&self, setting: Setting, cx: &mut Context<Self>) -> gpui::AnyElement {
        let session = self.session.read(cx);
        let settings = session.settings();
        match setting {
            Setting::Demosaic => {
                let unavailable: &[DemosaicAlgorithm] = if session.neural_unavailable().is_some() {
                    &[DemosaicAlgorithm::Neural]
                } else {
                    &[]
                };
                choice_except(
                    "demosaic",
                    setting.title(),
                    &DEMOSAICS,
                    unavailable,
                    settings.demosaic(),
                    self.change(AppSettings::set_demosaic),
                )
                .justify_end()
                .into_any_element()
            }
            Setting::NeuralModel => self.model_control(session),
            Setting::OutputGamut => choice_except(
                "output-gamut",
                setting.title(),
                &GAMUT_POLICIES,
                &[],
                settings.output_policy(),
                self.change(AppSettings::set_output_policy),
            )
            .justify_end()
            .into_any_element(),
            Setting::AutoNoiseReduction => self.switch(
                "auto-noise-reduction",
                setting,
                settings.auto_noise_reduction(),
                AppSettings::set_auto_noise_reduction,
            ),
            Setting::CpuOnly => self.switch(
                "cpu-only",
                setting,
                settings.cpu_only(),
                AppSettings::set_cpu_only,
            ),
        }
    }

    /// Install a model, or replace the installed one; nothing while an install runs.
    fn model_control(&self, session: &Session) -> gpui::AnyElement {
        let installed = session.neural_model().is_some()
            || matches!(session.model_install(), Some(ModelInstall::Installed(_)));
        if session.model_install() == Some(ModelInstall::Installing) {
            return div().into_any_element();
        }
        let label = if installed {
            "Replace…"
        } else {
            "Install…"
        };
        let session = self.session.clone();
        button("install-neural-model", label, move |_, _, cx| {
            session.update(cx, |session, cx| session.prompt_neural_model(cx));
        })
        .into_any_element()
    }

    fn switch(
        &self,
        id: &'static str,
        setting: Setting,
        on: bool,
        set: fn(&mut AppSettings, bool),
    ) -> gpui::AnyElement {
        let change = self.change(set);
        switch(id, setting.title(), on, move |window, cx| {
            change(!on, window, cx);
        })
        .into_any_element()
    }
}

impl Render for SettingsDialog {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = theme();
        let warning = self.session.read(cx).settings_warning().map(|warning| {
            div()
                .test_id("settings-warning")
                .relative()
                .px_4()
                .py_2()
                .border_t_1()
                .border_color(theme.border)
                .text_size(SMALL_FONT_SIZE)
                .text_color(theme.warning)
                .child(warning.to_owned())
        });
        div()
            .id("settings-dialog")
            .role(Role::Dialog)
            .aria_label("Settings")
            .test_id("settings-dialog")
            .on_action(cx.listener(|this, _: &Cancel, _, cx| this.cancel(cx)))
            .w(px(640.0))
            .flex()
            .flex_col()
            .rounded(px(8.0))
            .border_1()
            .border_color(theme.border)
            .bg(theme.elevated)
            .shadow_lg()
            .child(self.header(cx))
            .child(
                div()
                    .id("settings-list")
                    .flex()
                    .flex_col()
                    .max_h(px(520.0))
                    .overflow_y_scroll()
                    .px_4()
                    .pb_3()
                    .children(self.sections(cx)),
            )
            .children(warning)
    }
}

/// Which model is in use, or is installed for the next start.
fn model_note(session: &Session) -> SharedString {
    match (session.model_install(), session.neural_model()) {
        (Some(ModelInstall::Installing), _) => "Installing…".into(),
        (Some(ModelInstall::Installed(id)), _) => format!(
            "Installed ({}). Restart Rohditor to use it.",
            short_id(&id.sha256_hex())
        )
        .into(),
        (None, Some(id)) => format!("In use: {}", short_id(&id.sha256_hex())).into(),
        (None, None) => session
            .neural_unavailable()
            .map_or_else(SharedString::default, |reason| {
                format!("Not in use: {reason}.").into()
            }),
    }
}

/// The start of a model's sha256, enough to tell models apart.
fn short_id(sha256: &str) -> String {
    format!("sha256 {}…", &sha256[..12])
}

fn section_title(title: &'static str) -> impl IntoElement {
    div()
        .pt_4()
        .pb_1()
        .border_b_1()
        .border_color(theme().border)
        .text_size(SMALL_FONT_SIZE)
        .text_color(theme().muted)
        .child(title)
}
