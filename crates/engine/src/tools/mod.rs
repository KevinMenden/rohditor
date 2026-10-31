//! Frontend-agnostic editing-tool state.
//!
//! One tool is active at a time; it decides what the tool panel shows and what
//! pointer gestures on the image mean. Everything here is pure state: the
//! caller owns undo (via `EditSession::set_discrete`) and preview scheduling.

mod crop;

pub use crop::{CropAspect, CropHandle, CropToolSession};

use rohditor_edit::EditRecipe;

/// The tools offered by the tool rail.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolKind {
    Edit,
    Crop,
}

/// Viewport sampling modes of the Edit tool. They are mutually exclusive, so
/// one click can never be interpreted by two samplers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Picker {
    WhiteBalance,
    ColorMixer,
}

/// The single active tool together with its transient (non-recipe) state.
#[derive(Debug, Clone)]
pub enum ActiveTool {
    Edit { picker: Option<Picker> },
    Crop(CropToolSession),
}

impl Default for ActiveTool {
    fn default() -> Self {
        Self::Edit { picker: None }
    }
}

impl ActiveTool {
    pub const fn kind(&self) -> ToolKind {
        match self {
            Self::Edit { .. } => ToolKind::Edit,
            Self::Crop(_) => ToolKind::Crop,
        }
    }

    pub const fn picker(&self) -> Option<Picker> {
        match self {
            Self::Edit { picker } => *picker,
            Self::Crop(_) => None,
        }
    }

    /// Sets or clears the Edit tool's picker. Pickers do not exist in other
    /// tools, so this is a no-op there.
    pub fn set_picker(&mut self, next: Option<Picker>) {
        if let Self::Edit { picker } = self {
            *picker = next;
        }
    }

    pub const fn crop(&self) -> Option<&CropToolSession> {
        match self {
            Self::Crop(session) => Some(session),
            Self::Edit { .. } => None,
        }
    }

    pub fn crop_mut(&mut self) -> Option<&mut CropToolSession> {
        match self {
            Self::Crop(session) => Some(session),
            Self::Edit { .. } => None,
        }
    }

    /// Switches to the Crop tool; an active picker is dropped.
    pub fn enter_crop(&mut self, session: CropToolSession) {
        *self = Self::Crop(session);
    }

    /// Rail click on `requested`: the active tool's own button returns to Edit.
    pub fn toggle_target(&self, requested: ToolKind) -> ToolKind {
        if self.kind() == requested {
            ToolKind::Edit
        } else {
            requested
        }
    }

    /// Leaves the current tool; returns the recipe to commit, if any.
    ///
    /// Pure over `EditRecipe`: the caller owns undo and revisions. A crop is
    /// only committed when the full frame was available to author against and
    /// the draft differs from the recipe's crop.
    #[must_use]
    pub fn leave(self, recipe: &EditRecipe, commit: bool) -> Option<EditRecipe> {
        match self {
            Self::Crop(session)
                if commit && session.full_frame_ready() && session.is_modified() =>
            {
                let mut next = recipe.clone();
                next.geometry.crop = session.committed_crop();
                Some(next)
            }
            Self::Crop(_) | Self::Edit { .. } => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn modified_session(ready: bool) -> CropToolSession {
        let mut session = CropToolSession::new(None, 6000, 4000);
        if ready {
            session.set_full_dimensions(6000, 4000);
        }
        session.set_aspect(CropAspect::Square);
        session
    }

    #[test]
    fn leaving_a_modified_crop_commits_only_when_asked() {
        let recipe = EditRecipe::default();
        let tool = ActiveTool::Crop(modified_session(true));
        let committed = tool.clone().leave(&recipe, true).expect("commit");
        assert!(committed.geometry.crop.is_some());
        assert_eq!(tool.leave(&recipe, false), None);
    }

    #[test]
    fn leaving_an_unmodified_or_unready_crop_commits_nothing() {
        let recipe = EditRecipe::default();
        let mut untouched = CropToolSession::new(None, 6000, 4000);
        untouched.set_full_dimensions(6000, 4000);
        assert_eq!(ActiveTool::Crop(untouched).leave(&recipe, true), None);
        assert_eq!(
            ActiveTool::Crop(modified_session(false)).leave(&recipe, true),
            None
        );
        assert_eq!(ActiveTool::default().leave(&recipe, true), None);
    }

    #[test]
    fn the_active_tools_button_returns_to_edit() {
        let edit = ActiveTool::default();
        assert_eq!(edit.toggle_target(ToolKind::Crop), ToolKind::Crop);
        assert_eq!(edit.toggle_target(ToolKind::Edit), ToolKind::Edit);
        let crop = ActiveTool::Crop(modified_session(true));
        assert_eq!(crop.toggle_target(ToolKind::Crop), ToolKind::Edit);
        assert_eq!(crop.toggle_target(ToolKind::Edit), ToolKind::Edit);
    }

    #[test]
    fn entering_crop_drops_an_active_picker() {
        let mut tool = ActiveTool::Edit {
            picker: Some(Picker::WhiteBalance),
        };
        tool.enter_crop(modified_session(true));
        assert_eq!(tool.kind(), ToolKind::Crop);
        assert_eq!(tool.picker(), None);
        tool.set_picker(Some(Picker::ColorMixer));
        assert_eq!(tool.picker(), None);
    }
}
