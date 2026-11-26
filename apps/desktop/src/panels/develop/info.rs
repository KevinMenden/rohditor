//! How the photo was taken and what its file holds.

use gpui::{Context, IntoElement, SharedString, div, prelude::*, px};
use rohditor_raw::RawFileInfo;

use super::DevelopPanel;
use super::control::Control;
use crate::panels::controls::{ControlPanel as _, Header, SectionHeaders as _};
use crate::theme::{SMALL_FONT_SIZE, theme};
use crate::ui::matching;
use crate::ui::test_id::TestId as _;
use crate::ui::tooltip::tooltip;

#[derive(PartialEq)]
pub(super) struct PhotoInfo {
    /// Labels and values, of those the file provides.
    rows: Vec<(&'static str, SharedString)>,
    exposure: Option<SharedString>,
}

impl PhotoInfo {
    pub fn of(info: &RawFileInfo) -> Self {
        let rows = info.capture_rows().into_iter().chain(info.file_rows());
        Self {
            rows: rows
                .filter_map(|row| Some((row.label, row.value?.into())))
                .collect(),
            exposure: info.capture.exposure_summary().map(Into::into),
        }
    }
}

impl DevelopPanel {
    pub(super) fn exposure_summary(&self) -> Option<impl IntoElement + use<>> {
        let exposure = self.model.info.as_ref()?.exposure.clone()?;
        Some(
            div()
                .test_id("exposure-summary")
                .relative()
                .flex()
                .justify_center()
                .pt_1()
                .text_size(SMALL_FONT_SIZE)
                .text_color(theme().muted)
                .child(exposure),
        )
    }

    pub(super) fn info(&self, cx: &mut Context<Self>) -> Option<impl IntoElement + use<>> {
        let theme = theme();
        let info = self.model.info.clone()?;
        let query = self.query(cx);
        let rows: Vec<_> = info
            .rows
            .iter()
            .filter(|(label, value)| matching::words(&format!("Info {label} {value}"), &query))
            .cloned()
            .collect();
        if self.edited_only || rows.is_empty() {
            return None;
        }
        let open = self.is_open(Control::Info, cx);
        let rows = rows.into_iter().filter(|_| open).map(|(label, value)| {
            div()
                .id(label)
                .test_id(format!("info:{label}"))
                .relative()
                .flex()
                .justify_between()
                .gap_3()
                .pl_2()
                .min_h(px(18.0))
                .text_size(SMALL_FONT_SIZE)
                .tooltip(tooltip(value.clone(), None))
                .child(div().flex_none().text_color(theme.faint).child(label))
                .child(div().min_w_0().truncate().child(value))
        });
        Some(
            div()
                .flex()
                .flex_col()
                .px_2()
                .pb_2()
                .child(
                    div()
                        .flex()
                        .child(self.title(Header::plain(Control::Info, "Info"), cx)),
                )
                .children(rows.collect::<Vec<_>>()),
        )
    }
}
