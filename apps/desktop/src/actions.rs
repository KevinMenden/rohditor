//! Everything the user can do by key, button or command palette.

use gpui::{Action, App, KeyBinding, NoAction, SharedString, actions};
use rohditor_edit::{
    HighlightMethod, LensProfileSelection, MaskMode, RecipeSection, RenderingProfileSelection,
    WhiteBalancePreset,
};
use rohditor_engine::catalog::library::LibrarySort;
use rohditor_engine::tools::CropAspect;

use crate::panels::adjustments::{
    HIGHLIGHT_METHODS, RENDERINGS, method_label, rendering_label, sections,
};
use crate::panels::masks::Shape;
use crate::{panels, ui};

actions!(
    rohditor,
    [
        OpenFile,
        OpenFolder,
        RefreshFolders,
        CloseFile,
        NextPhoto,
        PreviousPhoto,
        SelectAllPhotos,
        NewAlbum,
        AddToAlbum,
        RemoveFromAlbum,
        Save,
        Export,
        Undo,
        Redo,
        ResetAdjustments,
        AutoTone,
        AutoNoiseReduction,
        ImportCameraProfile,
        ChooseLens,
        ApplyPreset,
        SavePreset,
        PickWhiteBalance,
        PickMixerColor,
        EditTool,
        CropTool,
        LockCrop,
        FlipCrop,
        ResetCrop,
        MasksTool,
        DeleteMask,
        RenameMask,
        ToggleMask,
        MoveMaskUp,
        MoveMaskDown,
        ToggleMaskOverlay,
        PaintMask,
        PickMaskColors,
        ToggleErase,
        GrowBrush,
        ShrinkBrush,
        SoftenBrush,
        HardenBrush,
        ApplyTool,
        CancelTool,
        ZoomToFit,
        ActualPixels,
        ToggleClipping,
        ToggleFiles,
        ShowFiles,
        ShowAlbums,
        ToggleFilmstrip,
        ToggleDevelop,
        FocusAdjustments,
        SearchAdjustments,
        FocusPhoto,
        ToggleCommandPalette,
        OpenSettings,
        ToggleDiagnostics,
        Quit,
    ]
);

/// Restore the photo's defaults in one section.
#[derive(Debug, Clone, PartialEq, Eq, Action)]
#[action(namespace = rohditor, no_json)]
pub struct ResetSection(pub RecipeSection);

/// Choose how clipped highlights are treated.
#[derive(Debug, Clone, PartialEq, Eq, Action)]
#[action(namespace = rohditor, no_json)]
pub struct SetHighlightMethod(pub HighlightMethod);

#[derive(Debug, Clone, PartialEq, Eq, Action)]
#[action(namespace = rohditor, no_json)]
pub struct SetWhiteBalance(pub WhiteBalancePreset);

#[derive(Debug, Clone, PartialEq, Eq, Action)]
#[action(namespace = rohditor, no_json)]
pub struct SetRendering(pub RenderingProfileSelection);

#[derive(Debug, Clone, PartialEq, Eq, Action)]
#[action(namespace = rohditor, no_json)]
pub struct SetLensProfile(pub LensProfileSelection);

#[derive(Debug, Clone, PartialEq, Eq, Action)]
#[action(namespace = rohditor, no_json)]
pub struct SetCropAspect(pub CropAspect);

/// Choose the order of the photos shown.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Action)]
#[action(namespace = rohditor, no_json)]
pub struct SortPhotos(pub LibrarySort);

/// Make a new mask of a shape.
#[derive(Debug, Clone, PartialEq, Eq, Action)]
#[action(namespace = rohditor, no_json)]
pub struct NewMask(pub Shape);

/// Add a shape to the selected mask.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Action)]
#[action(namespace = rohditor, no_json)]
pub struct AddToMask {
    pub shape: Shape,
    pub mode: MaskMode,
}

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
        command("file: refresh folders", Some("f5"), RefreshFolders),
        command("file: close", Some("ctrl-w"), CloseFile),
        command("file: save edits", Some("ctrl-s"), Save),
        command("file: export", Some("ctrl-e"), Export),
        command("photo: next", Some("right"), NextPhoto),
        command("photo: previous", Some("left"), PreviousPhoto),
        command("photo: select all", Some("ctrl-a"), SelectAllPhotos),
        command("album: new album", None, NewAlbum),
        command("album: add selected photos to album", None, AddToAlbum),
        command(
            "album: remove selected photos from album",
            None,
            RemoveFromAlbum,
        ),
        command("edit: undo", Some("ctrl-z"), Undo),
        command("edit: redo", Some("ctrl-shift-z"), Redo),
        command("develop: auto tone", Some("ctrl-u"), AutoTone),
        command("develop: auto noise reduction", None, AutoNoiseReduction),
        command(
            "develop: reset adjustments",
            Some("ctrl-shift-r"),
            ResetAdjustments,
        ),
        command(
            "develop: import camera profile (DCP)",
            None,
            ImportCameraProfile,
        ),
        command("develop: apply preset", Some("p"), ApplyPreset),
        command("develop: save as preset", None, SavePreset),
        command("develop: choose lens profile", None, ChooseLens),
        command(
            "develop: lens profile automatic",
            None,
            SetLensProfile(LensProfileSelection::Automatic),
        ),
        command(
            "develop: lens profile off",
            None,
            SetLensProfile(LensProfileSelection::Off),
        ),
        command("develop: pick white balance", Some("w"), PickWhiteBalance),
        command("develop: pick mixer color", None, PickMixerColor),
        command("tool: edit", Some("e"), EditTool),
        command("tool: crop", Some("c"), CropTool),
        command("crop: lock ratio", None, LockCrop),
        command("crop: flip ratio", Some("x"), FlipCrop),
        command("crop: reset", None, ResetCrop),
        command("tool: masks", Some("m"), MasksTool),
        command("masks: delete selected shape", Some("delete"), DeleteMask),
        command("masks: rename mask", Some("f2"), RenameMask),
        command("masks: enable or disable mask", None, ToggleMask),
        command("masks: move mask up", Some("alt-up"), MoveMaskUp),
        command("masks: move mask down", Some("alt-down"), MoveMaskDown),
        command("masks: show or hide the mask", Some("o"), ToggleMaskOverlay),
        command("masks: paint selected brush", Some("b"), PaintMask),
        command("masks: pick colors", None, PickMaskColors),
        // After the tool with the same key, so that it comes first while painting.
        command("masks: erase or paint", Some("e"), ToggleErase),
        command("masks: larger brush", Some("]"), GrowBrush),
        command("masks: smaller brush", Some("["), ShrinkBrush),
        command("masks: softer brush", Some("}"), SoftenBrush),
        command("masks: harder brush", Some("{"), HardenBrush),
        command("tool: apply", Some("enter"), ApplyTool),
        command("view: zoom to fit", Some("ctrl-0"), ZoomToFit),
        command("view: actual pixels (1:1)", Some("ctrl-1"), ActualPixels),
        command("view: toggle clipping overlay", Some("j"), ToggleClipping),
        command("view: toggle files panel", Some("ctrl-b"), ToggleFiles),
        command("view: files", None, ShowFiles),
        command("view: albums", None, ShowAlbums),
        command("view: toggle filmstrip", Some("ctrl-j"), ToggleFilmstrip),
        command("view: toggle develop panel", Some("ctrl-r"), ToggleDevelop),
        command("view: toggle diagnostics", None, ToggleDiagnostics),
        command("view: focus adjustments", Some("ctrl-d"), FocusAdjustments),
        command(
            "view: search adjustments",
            Some("ctrl-f"),
            SearchAdjustments,
        ),
        command("view: focus photo", Some("escape"), FocusPhoto),
        // After what else the key does, so that it comes first.
        command("tool: cancel", Some("escape"), CancelTool),
        command(
            "view: command palette",
            Some("ctrl-shift-p"),
            ToggleCommandPalette,
        ),
        command("rohditor: settings", Some("ctrl-,"), OpenSettings),
        command("rohditor: quit", Some("ctrl-q"), Quit),
    ];
    commands.extend(LibrarySort::ALL.into_iter().map(|sort| {
        let name = format!("photo: sort by {}", sort.label().to_lowercase());
        command(name, None, SortPhotos(sort))
    }));
    commands.extend(sections().map(|section| {
        let section = section.section;
        let name = format!("develop: reset {}", section.label().to_lowercase());
        command(name, None, ResetSection(section))
    }));
    commands.extend(HIGHLIGHT_METHODS.into_iter().map(|method| {
        let name = format!(
            "develop: highlight reconstruction {}",
            method_label(method).to_lowercase()
        );
        command(name, None, SetHighlightMethod(method))
    }));
    commands.extend(WhiteBalancePreset::ALL.into_iter().map(|preset| {
        let name = format!("develop: white balance {}", preset.label().to_lowercase());
        command(name, None, SetWhiteBalance(preset))
    }));
    commands.extend(CropAspect::ALL.into_iter().map(|aspect| {
        let name = format!("crop: aspect {}", aspect.label().to_lowercase());
        command(name, None, SetCropAspect(aspect))
    }));
    for shape in Shape::ALL {
        let name = shape.name();
        commands.push(command(format!("masks: new {name}"), None, NewMask(shape)));
        commands.extend(shape.modes().iter().map(|&mode| {
            let name = format!("masks: {} {name}", mode.label().to_lowercase());
            command(name, None, AddToMask { shape, mode })
        }));
    }
    commands.extend(RENDERINGS.into_iter().map(|rendering| {
        let name = format!(
            "develop: rendering {}",
            rendering_label(rendering).to_lowercase()
        );
        command(name, None, SetRendering(rendering))
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
        let context = if command.action.partial_eq(&Quit) {
            None
        } else if command.action.partial_eq(&ApplyTool) {
            // Enter also presses the focused control.
            Some(context(PHOTO_CONTEXT))
        } else if is_for_painting(command.action.as_ref()) {
            Some(context(PAINTING_CONTEXT))
        } else {
            Some(context(WORKSPACE_CONTEXT))
        };
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
    let painting = Some(PAINTING_CONTEXT);
    let [larger, smaller] = BRUSH_SIZE_KEYS;
    cx.bind_keys([
        KeyBinding::new(larger, GrowBrush, painting),
        KeyBinding::new(smaller, ShrinkBrush, painting),
    ]);
    cx.bind_keys(letters_to_type());
    ui::keys::bind(cx);
    panels::controls::bind_keys(cx);
    panels::develop::bind_keys(cx);
    panels::file_tree::bind_keys(cx);
    panels::albums::bind_keys(cx);
}

/// Commands with a plain letter as their key leave it alone in text inputs.
fn letters_to_type() -> Vec<KeyBinding> {
    let letters = commands().into_iter().filter_map(|command| command.key);
    letters
        .filter(|key| key.len() == 1)
        .chain(BRUSH_SIZE_KEYS)
        .map(|key| KeyBinding::new(key, NoAction, Some(ui::text_input::CONTEXT)))
        .collect()
}

/// Larger and smaller, besides the brackets, which many keyboard layouts only have with AltGr.
const BRUSH_SIZE_KEYS: [&str; 2] = ["+", "-"];

/// Whether the action's key means it only while painting a mask.
fn is_for_painting(action: &dyn Action) -> bool {
    let brush: [&dyn Action; 5] = [
        &ToggleErase,
        &GrowBrush,
        &ShrinkBrush,
        &SoftenBrush,
        &HardenBrush,
    ];
    brush.iter().any(|one| one.partial_eq(action))
}

pub const WORKSPACE_CONTEXT: &str = "Workspace";
/// Added while the photo has the focus.
pub const PHOTO_CONTEXT: &str = "Photo";
/// Added while painting a mask.
pub const PAINTING_CONTEXT: &str = "Painting";

fn context(name: &'static str) -> std::rc::Rc<gpui::KeyBindingContextPredicate> {
    std::rc::Rc::new(gpui::KeyBindingContextPredicate::Identifier(name.into()))
}
