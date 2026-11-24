//! Everything the user can do by key, button or command palette.

use gpui::{Action, App, KeyBinding, NoAction, SharedString, actions};
use rohditor_edit::RecipeSection;

use crate::panels::adjustments::SECTIONS;
use crate::{panels, ui};

actions!(
    rohditor,
    [
        OpenFile,
        OpenFolder,
        CloseFile,
        NextPhoto,
        PreviousPhoto,
        Save,
        Export,
        Undo,
        Redo,
        ResetAdjustments,
        AutoTone,
        ZoomToFit,
        ActualPixels,
        ToggleClipping,
        ToggleFiles,
        ToggleDevelop,
        FocusAdjustments,
        FocusPhoto,
        ToggleCommandPalette,
        Quit,
    ]
);

/// Restore the photo's defaults in one section.
#[derive(Debug, Clone, PartialEq, Eq, Action)]
#[action(namespace = rohditor, no_json)]
pub struct ResetSection(pub RecipeSection);

/// An entry of the command palette and, when it has a key, of the keymap.
pub struct Command {
    pub name: SharedString,
    pub key: Option<&'static str>,
    pub action: Box<dyn Action>,
}

fn command(
    name: impl Into<SharedString>,
    key: Option<&'static str>,
    action: impl Action,
) -> Command {
    Command {
        name: name.into(),
        key,
        action: Box::new(action),
    }
}

pub fn commands() -> Vec<Command> {
    let mut commands = vec![
        command("file: open", Some("ctrl-o"), OpenFile),
        command("file: open folder", Some("ctrl-k"), OpenFolder),
        command("file: close", Some("ctrl-w"), CloseFile),
        command("file: save edits", Some("ctrl-s"), Save),
        command("file: export JPEG", Some("ctrl-e"), Export),
        command("photo: next", Some("right"), NextPhoto),
        command("photo: previous", Some("left"), PreviousPhoto),
        command("edit: undo", Some("ctrl-z"), Undo),
        command("edit: redo", Some("ctrl-shift-z"), Redo),
        command("develop: auto tone", Some("ctrl-u"), AutoTone),
        command(
            "develop: reset adjustments",
            Some("ctrl-shift-r"),
            ResetAdjustments,
        ),
        command("view: zoom to fit", Some("ctrl-0"), ZoomToFit),
        command("view: actual pixels (1:1)", Some("ctrl-1"), ActualPixels),
        command("view: toggle clipping overlay", Some("j"), ToggleClipping),
        command("view: toggle files panel", Some("ctrl-b"), ToggleFiles),
        command("view: toggle develop panel", Some("ctrl-r"), ToggleDevelop),
        command("view: focus adjustments", Some("ctrl-d"), FocusAdjustments),
        command("view: focus photo", Some("escape"), FocusPhoto),
        command(
            "view: command palette",
            Some("ctrl-shift-p"),
            ToggleCommandPalette,
        ),
        command("rohditor: quit", Some("ctrl-q"), Quit),
    ];
    commands.extend(SECTIONS.iter().map(|section| {
        let section = section.section;
        let name = format!("develop: reset {}", section.label().to_lowercase());
        command(name, None, ResetSection(section))
    }));
    commands
}

impl Command {
    /// The entry that runs `action`, which must be in the list.
    pub fn of(action: &dyn Action) -> Self {
        commands()
            .into_iter()
            .find(|command| command.action.partial_eq(action))
            .expect("every action has a command")
    }

    /// `view: zoom to fit` as `Zoom to fit`.
    pub fn title(&self) -> String {
        let name = self
            .name
            .split_once(": ")
            .map_or(self.name.as_str(), |(_, name)| name);
        capitalized(name)
    }

    pub fn key_label(&self) -> Option<String> {
        self.key.map(key_label)
    }
}

fn capitalized(text: &str) -> String {
    let mut letters = text.chars();
    letters
        .next()
        .map(|first| first.to_uppercase().chain(letters).collect())
        .unwrap_or_default()
}

/// `ctrl-shift-p` as `Ctrl+Shift+P`.
pub fn key_label(key: &str) -> String {
    key.split('-')
        .map(capitalized)
        .collect::<Vec<_>>()
        .join("+")
}

pub fn bind_keys(cx: &mut App) {
    let bindings = commands().into_iter().filter_map(|command| {
        // The workspace context, so that menus and dialogs get the keys while they are open.
        // Quitting always works.
        let context = (!command.action.partial_eq(&Quit)).then(workspace_context);
        KeyBinding::load(
            command.key?,
            command.action,
            context,
            false,
            None,
            &gpui::DummyKeyboardMapper,
        )
        .ok()
    });
    let bindings: Vec<_> = bindings.collect();
    cx.bind_keys(bindings);
    cx.bind_keys(letters_to_type());
    ui::keys::bind(cx);
    panels::develop::bind_keys(cx);
}

/// Commands with a plain letter as their key leave it alone in text inputs.
fn letters_to_type() -> Vec<KeyBinding> {
    let letters = commands().into_iter().filter_map(|command| command.key);
    letters
        .filter(|key| key.len() == 1)
        .map(|key| KeyBinding::new(key, NoAction, Some(ui::text_input::CONTEXT)))
        .collect()
}

pub const WORKSPACE_CONTEXT: &str = "Workspace";

fn workspace_context() -> std::rc::Rc<gpui::KeyBindingContextPredicate> {
    std::rc::Rc::new(gpui::KeyBindingContextPredicate::Identifier(
        WORKSPACE_CONTEXT.into(),
    ))
}
