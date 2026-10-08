//! Test observations of the actual candidate singleton decoder.

use std::cell::Cell;

thread_local! {
    static DECODES: Cell<Option<usize>> = const { Cell::new(None) };
}

pub(super) fn record() {
    DECODES.with(|count| {
        if let Some(value) = count.get() {
            count.set(Some(value + 1));
        }
    });
}

pub(super) struct DecodeCounter(Option<usize>);

impl DecodeCounter {
    pub(super) fn new() -> Self {
        Self(DECODES.with(|count| count.replace(Some(0))))
    }

    pub(super) fn count(&self) -> usize {
        DECODES.with(|count| count.get().expect("active decode observation"))
    }
}

impl Drop for DecodeCounter {
    fn drop(&mut self) {
        DECODES.with(|count| count.set(self.0));
    }
}
