//! Announces the open photo's news as toasts.

use gpui::Context;

use super::Workspace;
use crate::ui::overlays::ToastKind;

/// What was last announced, so that every change is announced once.
#[derive(Default, PartialEq)]
pub(super) struct Announced {
    document: Option<u64>,
    notice: Option<String>,
    error: Option<String>,
}

impl Workspace {
    pub(super) fn announce(&mut self, cx: &mut Context<Self>) {
        let current = match self.session.read(cx).document() {
            Some(document) => Announced {
                document: Some(document.id),
                notice: document.notice.clone(),
                error: document.error.clone(),
            },
            None => Announced::default(),
        };
        if current == self.announced {
            return;
        }
        let same_photo = current.document == self.announced.document;
        let news = [
            (ToastKind::Success, &self.announced.notice, &current.notice),
            (ToastKind::Error, &self.announced.error, &current.error),
        ];
        for (kind, before, now) in news {
            let repeated = same_photo && before == now;
            if let Some(text) = now.clone().filter(|_| !repeated) {
                self.overlays
                    .update(cx, |overlays, cx| overlays.toast(kind, text, cx));
            }
        }
        self.announced = current;
    }
}
