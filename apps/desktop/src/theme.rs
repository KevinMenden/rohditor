//! Colours and metrics: a quiet, flat, editor-like dark theme. The viewport is a neutral grey so
//! the surround does not tint the photo.

use gpui::{Hsla, Pixels, px, rgb, rgba};

pub struct Theme {
    pub title_bar: Hsla,
    pub panel: Hsla,
    pub editor: Hsla,
    pub viewport: Hsla,
    pub elevated: Hsla,
    pub element: Hsla,
    pub hover: Hsla,
    pub selected: Hsla,
    pub border: Hsla,
    pub text: Hsla,
    pub muted: Hsla,
    pub faint: Hsla,
    pub accent: Hsla,
    /// Filled part of a slider.
    pub fill: Hsla,
    pub warning: Hsla,
    pub error: Hsla,
    pub success: Hsla,
}

pub fn theme() -> Theme {
    Theme {
        title_bar: rgb(0x2c2f36).into(),
        panel: rgb(0x23262c).into(),
        editor: rgb(0x1e2126).into(),
        viewport: rgb(0x191919).into(),
        elevated: rgb(0x292c33).into(),
        element: rgb(0x2b2f37).into(),
        hover: rgb(0x343943).into(),
        selected: rgb(0x3b424e).into(),
        border: rgb(0x373b44).into(),
        text: rgb(0xd5d9e0).into(),
        muted: rgb(0x98a0ad).into(),
        faint: rgb(0x656c79).into(),
        accent: rgb(0x6ea8e6).into(),
        fill: rgba(0x6ea8e633).into(),
        warning: rgb(0xd9b36c).into(),
        error: rgb(0xd9737a).into(),
        success: rgb(0x93bf7a).into(),
    }
}

pub const TITLE_BAR_HEIGHT: Pixels = px(36.0);
pub const TAB_HEIGHT: Pixels = px(34.0);
pub const STATUS_BAR_HEIGHT: Pixels = px(28.0);
pub const FILES_WIDTH: Pixels = px(248.0);
pub const FILMSTRIP_HEIGHT: Pixels = px(126.0);
pub const DEVELOP_WIDTH: Pixels = px(300.0);
pub const UI_FONT_SIZE: Pixels = px(13.0);
pub const SMALL_FONT_SIZE: Pixels = px(11.5);
pub const RADIUS: Pixels = px(4.0);
