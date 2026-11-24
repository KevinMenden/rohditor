//! Keys shared by the widgets. What they mean is up to the view that handles the action.

use gpui::{App, KeyBinding, NoAction, actions};

use super::{menu, overlays, text_input};

actions!(
    ui,
    [
        Confirm,
        Cancel,
        SelectNext,
        SelectPrevious,
        FocusNext,
        FocusPrevious,
    ]
);

pub fn bind(cx: &mut App) {
    use text_input as input;

    cx.bind_keys([
        KeyBinding::new("tab", FocusNext, None),
        KeyBinding::new("shift-tab", FocusPrevious, None),
        KeyBinding::new("escape", Cancel, Some(overlays::CONTEXT)),
    ]);
    for context in [input::CONTEXT, menu::CONTEXT] {
        cx.bind_keys([
            KeyBinding::new("enter", Confirm, Some(context)),
            KeyBinding::new("escape", Cancel, Some(context)),
            KeyBinding::new("down", SelectNext, Some(context)),
            KeyBinding::new("up", SelectPrevious, Some(context)),
        ]);
    }
    let text = Some(input::CONTEXT);
    cx.bind_keys([
        KeyBinding::new("backspace", input::Backspace, text),
        KeyBinding::new("delete", input::Delete, text),
        KeyBinding::new("left", input::Left, text),
        KeyBinding::new("right", input::Right, text),
        KeyBinding::new("shift-left", input::SelectLeft, text),
        KeyBinding::new("shift-right", input::SelectRight, text),
        KeyBinding::new("home", input::Home, text),
        KeyBinding::new("end", input::End, text),
        KeyBinding::new("shift-home", input::SelectHome, text),
        KeyBinding::new("shift-end", input::SelectEnd, text),
        KeyBinding::new("ctrl-a", input::SelectAll, text),
        KeyBinding::new("ctrl-c", input::Copy, text),
        KeyBinding::new("ctrl-x", input::Cut, text),
        KeyBinding::new("ctrl-v", input::Paste, text),
        // Not the photo's undo while typing.
        KeyBinding::new("ctrl-z", NoAction {}, text),
        KeyBinding::new("ctrl-shift-z", NoAction {}, text),
    ]);
}
