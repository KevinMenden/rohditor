//! Icons compiled into the binary.

use std::borrow::Cow;

use anyhow::Result;
use gpui::{AssetSource, SharedString};

#[derive(Debug, Clone, Copy)]
pub enum Icon {
    Files,
    Album,
    Filmstrip,
    Sliders,
    Undo,
    Redo,
    Reset,
    Export,
    Close,
    Minimize,
    Maximize,
    Search,
    ChevronDown,
    ChevronRight,
    Image,
    Clipping,
    Pipette,
    Plus,
    Crop,
    Mask,
    Eye,
    EyeOff,
    Invert,
    Brush,
    Settings,
    Activity,
}

impl Icon {
    pub const fn path(self) -> &'static str {
        match self {
            Self::Album => "album.svg",
            Self::Files => "files.svg",
            Self::Filmstrip => "filmstrip.svg",
            Self::Sliders => "sliders.svg",
            Self::Undo => "undo.svg",
            Self::Redo => "redo.svg",
            Self::Reset => "reset.svg",
            Self::Export => "export.svg",
            Self::Close => "close.svg",
            Self::Minimize => "minimize.svg",
            Self::Maximize => "maximize.svg",
            Self::Search => "search.svg",
            Self::ChevronDown => "chevron-down.svg",
            Self::ChevronRight => "chevron-right.svg",
            Self::Image => "image.svg",
            Self::Clipping => "clipping.svg",
            Self::Pipette => "pipette.svg",
            Self::Plus => "plus.svg",
            Self::Crop => "crop.svg",
            Self::Mask => "mask.svg",
            Self::Eye => "eye.svg",
            Self::EyeOff => "eye-off.svg",
            Self::Invert => "invert.svg",
            Self::Brush => "brush.svg",
            Self::Settings => "settings.svg",
            Self::Activity => "activity.svg",
        }
    }
}

pub struct Assets;

impl AssetSource for Assets {
    fn load(&self, path: &str) -> Result<Option<Cow<'static, [u8]>>> {
        macro_rules! icons {
            ($($name:literal),* $(,)?) => {
                match path {
                    $($name => Some(include_bytes!(concat!("../assets/icons/", $name)).as_slice()),)*
                    _ => None,
                }
            };
        }
        let bytes = icons![
            "album.svg",
            "files.svg",
            "filmstrip.svg",
            "sliders.svg",
            "undo.svg",
            "redo.svg",
            "reset.svg",
            "export.svg",
            "close.svg",
            "minimize.svg",
            "maximize.svg",
            "search.svg",
            "chevron-down.svg",
            "chevron-right.svg",
            "image.svg",
            "clipping.svg",
            "pipette.svg",
            "plus.svg",
            "crop.svg",
            "mask.svg",
            "eye.svg",
            "eye-off.svg",
            "invert.svg",
            "brush.svg",
            "settings.svg",
            "activity.svg",
        ];
        Ok(bytes.map(Cow::Borrowed))
    }

    fn list(&self, _: &str) -> Result<Vec<SharedString>> {
        Ok(Vec::new())
    }
}
