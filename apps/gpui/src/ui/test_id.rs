//! Names elements, so that UI tests find them by name instead of by position.
//!
//! Does nothing outside of test builds.

use gpui::{ParentElement, SharedString};

pub trait TestId: ParentElement + Sized {
    #[cfg(not(test))]
    fn test_id(self, _: impl Into<SharedString>) -> Self {
        self
    }

    /// Record the element's bounds under `name` whenever it is laid out.
    #[cfg(test)]
    fn test_id(self, name: impl Into<SharedString>) -> Self {
        use gpui::{Styled as _, canvas};

        let name = name.into();
        self.child(
            canvas(
                move |bounds, _, _| recorded::record(name, bounds),
                |_, _, _, _| {},
            )
            .absolute()
            .top_0()
            .left_0()
            .size_full(),
        )
    }
}

impl<E: ParentElement> TestId for E {}

/// What the last frames laid out. Each test runs its window on its own thread.
#[cfg(test)]
pub mod recorded {
    use std::cell::RefCell;
    use std::collections::HashMap;

    use gpui::{Bounds, Pixels, SharedString};

    thread_local! {
        static BOUNDS: RefCell<HashMap<SharedString, Bounds<Pixels>>> = RefCell::default();
    }

    pub(super) fn record(name: SharedString, bounds: Bounds<Pixels>) {
        BOUNDS.with_borrow_mut(|all| all.insert(name, bounds));
    }

    pub fn clear() {
        BOUNDS.with_borrow_mut(HashMap::clear);
    }

    pub fn get(name: &str) -> Option<Bounds<Pixels>> {
        BOUNDS.with_borrow(|all| all.get(name).copied())
    }
}
