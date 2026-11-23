//! Icons compiled into the binary.

use std::borrow::Cow;

use anyhow::Result;
use gpui::{AssetSource, SharedString};

#[derive(Debug, Clone, Copy)]
pub enum Icon {
    Files,
    Sliders,
    Undo,
    Redo,
    Export,
    Close,
    Minimize,
    Maximize,
    Search,
    ChevronDown,
    ChevronRight,
    Image,
}

impl Icon {
    pub const fn path(self) -> &'static str {
        match self {
            Self::Files => "files.svg",
            Self::Sliders => "sliders.svg",
            Self::Undo => "undo.svg",
            Self::Redo => "redo.svg",
            Self::Export => "export.svg",
            Self::Close => "close.svg",
            Self::Minimize => "minimize.svg",
            Self::Maximize => "maximize.svg",
            Self::Search => "search.svg",
            Self::ChevronDown => "chevron-down.svg",
            Self::ChevronRight => "chevron-right.svg",
            Self::Image => "image.svg",
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
            "files.svg",
            "sliders.svg",
            "undo.svg",
            "redo.svg",
            "export.svg",
            "close.svg",
            "minimize.svg",
            "maximize.svg",
            "search.svg",
            "chevron-down.svg",
            "chevron-right.svg",
            "image.svg",
        ];
        Ok(bytes.map(Cow::Borrowed))
    }

    fn list(&self, _: &str) -> Result<Vec<SharedString>> {
        Ok(Vec::new())
    }
}
