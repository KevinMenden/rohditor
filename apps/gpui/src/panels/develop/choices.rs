//! Choices from a list: rows with a dropdown.

use gpui::{Context, IntoElement, SharedString, div, prelude::*, px};
use rohditor_edit::{
    CameraProfileSelection, EditRecipe, RecipeSection, RenderingProfileSelection,
    WhiteBalancePreset,
};

use super::DevelopPanel;
use super::focus::Control;
use crate::actions::ImportCameraProfile;
use crate::panels::adjustments::{HIGHLIGHT_METHODS, RENDERINGS, method_label, rendering_label};
use crate::session::Session;
use crate::theme::theme;
use crate::ui::dropdown::dropdown;
use crate::ui::menu::MenuEntry;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(super) enum Choice {
    WhiteBalance,
    HighlightMethod,
    Rendering,
    CameraProfile,
}

impl Choice {
    pub const ALL: [Self; 4] = [
        Self::WhiteBalance,
        Self::HighlightMethod,
        Self::Rendering,
        Self::CameraProfile,
    ];

    pub const fn section(self) -> RecipeSection {
        match self {
            Self::WhiteBalance => RecipeSection::WhiteBalance,
            Self::HighlightMethod => RecipeSection::HighlightReconstruction,
            Self::Rendering | Self::CameraProfile => RecipeSection::ColorProfile,
        }
    }

    pub const fn label(self) -> &'static str {
        match self {
            Self::WhiteBalance => "Preset",
            Self::HighlightMethod => "Method",
            Self::Rendering => "Rendering",
            Self::CameraProfile => "Camera profile",
        }
    }

    const fn id(self) -> &'static str {
        match self {
            Self::WhiteBalance => "white-balance",
            Self::HighlightMethod => "highlight-method",
            Self::Rendering => "rendering",
            Self::CameraProfile => "camera-profile",
        }
    }

    pub fn is_changed(self, recipe: &EditRecipe, defaults: &EditRecipe) -> bool {
        match self {
            Self::WhiteBalance => self.section().is_changed(recipe, defaults),
            Self::HighlightMethod => recipe.raw.highlights.method != defaults.raw.highlights.method,
            Self::Rendering => recipe.rendering.profile != defaults.rendering.profile,
            Self::CameraProfile => recipe.color.camera_profile != defaults.color.camera_profile,
        }
    }

    fn value(self, recipe: &EditRecipe, defaults: &EditRecipe) -> SharedString {
        match self {
            Self::WhiteBalance => WhiteBalancePreset::of(recipe, defaults)
                .map_or("Custom", WhiteBalancePreset::label)
                .into(),
            Self::HighlightMethod => method_label(recipe.raw.highlights.method).into(),
            Self::Rendering => rendering_label(recipe.rendering.profile).into(),
            Self::CameraProfile => profile_label(&recipe.color.camera_profile),
        }
    }
}

fn profile_label(selection: &CameraProfileSelection) -> SharedString {
    match selection {
        CameraProfileSelection::Automatic => "Automatic".into(),
        CameraProfileSelection::Matrix(profile) => profile.name.clone().into(),
    }
}

/// What the rendering does to the photo.
const fn rendering_hint(rendering: RenderingProfileSelection) -> &'static str {
    match rendering {
        RenderingProfileSelection::RohditorMatched { .. } => "Fitted to this photo's camera JPEG",
        RenderingProfileSelection::RohditorStandard { .. } => "Balanced tone and highlight rolloff",
        RenderingProfileSelection::RohditorNeutral => "No base tone or color",
    }
}

impl DevelopPanel {
    /// Apply a choice made elsewhere, such as in the command palette.
    pub fn choose(
        &mut self,
        change: impl FnOnce(&mut Session, &mut Context<Session>),
        cx: &mut Context<Self>,
    ) {
        self.finish_nudge(cx);
        self.session.update(cx, change);
    }

    pub(super) fn choice(
        &self,
        choice: Choice,
        recipe: &EditRecipe,
        defaults: &EditRecipe,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        let theme = theme();
        let panel = cx.entity();
        let changed = choice.is_changed(recipe, defaults);
        let list = dropdown(
            choice.id(),
            choice.label(),
            choice.value(recipe, defaults),
            self.overlays.clone(),
            // Profiles are only looked up once the list opens.
            move |_, cx| panel.update(cx, |panel, cx| panel.entries(choice, cx)),
        );
        let control = Control::Choice(choice);
        let hint = (choice == Choice::Rendering).then(|| {
            div()
                .pl_2()
                .text_size(crate::theme::SMALL_FONT_SIZE)
                .text_color(theme.faint)
                .child(rendering_hint(recipe.rendering.profile))
        });
        div()
            .flex()
            .flex_col()
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap_2()
                    .h(px(24.0))
                    .pl_2()
                    .text_color(if changed { theme.text } else { theme.muted })
                    .child(div().flex_none().child(choice.label()))
                    .child(
                        list.min_w_0()
                            .track_focus(self.focus.control(control))
                            .child(self.revealer(control, cx)),
                    ),
            )
            .children(hint)
    }

    fn entries(&mut self, choice: Choice, cx: &mut Context<Self>) -> Vec<MenuEntry> {
        let Some((recipe, defaults)) = self.model.recipes.clone() else {
            return Vec::new();
        };
        match choice {
            Choice::WhiteBalance => {
                let current = WhiteBalancePreset::of(&recipe, &defaults);
                let presets = WhiteBalancePreset::ALL.into_iter().map(|preset| {
                    self.entry(preset.label(), current == Some(preset), cx, move |s, cx| {
                        s.set_white_balance(preset, cx);
                    })
                });
                presets.collect()
            }
            Choice::HighlightMethod => {
                let methods = HIGHLIGHT_METHODS.into_iter().map(|method| {
                    let current = recipe.raw.highlights.method == method;
                    self.entry(method_label(method), current, cx, move |s, cx| {
                        s.set_highlight_method(method, cx);
                    })
                });
                methods.collect()
            }
            Choice::Rendering => {
                let renderings = RENDERINGS.into_iter().map(|rendering| {
                    let current = recipe.rendering.profile == rendering;
                    self.entry(rendering_label(rendering), current, cx, move |s, cx| {
                        s.set_rendering(rendering, cx);
                    })
                });
                renderings.collect()
            }
            Choice::CameraProfile => {
                let selected = &recipe.color.camera_profile;
                let profiles = self.session.read(cx).camera_profiles();
                let profiles = profiles.into_iter().map(CameraProfileSelection::Matrix);
                let mut entries: Vec<_> = std::iter::once(CameraProfileSelection::Automatic)
                    .chain(profiles)
                    .map(|profile| {
                        let current = profile == *selected;
                        self.entry(profile_label(&profile), current, cx, move |s, cx| {
                            s.set_camera_profile(profile.clone(), cx);
                        })
                    })
                    .collect();
                entries.push(MenuEntry::new("Import DCP…", |window, cx| {
                    window.dispatch_action(Box::new(ImportCameraProfile), cx);
                }));
                entries
            }
        }
    }

    fn entry(
        &self,
        label: impl Into<SharedString>,
        current: bool,
        cx: &Context<Self>,
        change: impl Fn(&mut Session, &mut Context<Session>) + 'static,
    ) -> MenuEntry {
        let panel = cx.entity();
        MenuEntry::new(label, move |_, cx| {
            panel.update(cx, |panel, cx| panel.choose(&change, cx));
        })
        .checked(current)
    }
}
