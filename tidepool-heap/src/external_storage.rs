//! Owner-authenticated external payload views shared by graph traversals.

/// A bounded span of managed slots, without a per-visit allocation. This is
/// not ownership: the machine's allocation ledger must keep the backing
/// allocation alive and unchanged throughout traversal and relocation.
///
/// wave5:external-view: migrate the machine's validated external payload view
/// to this span; preserve kind, length, capacity and provenance validation.
#[derive(Clone, Copy, Debug)]
pub struct ExternalPointerSlots {
    base: *mut *mut u8,
    count: usize,
}

impl ExternalPointerSlots {
    /// # Safety
    /// `base` names `count` initialized, aligned managed slots in one live
    /// allocation. For an empty span it may be dangling. The owning ledger
    /// must prevent deallocation or resizing while a consumer uses the span.
    pub unsafe fn from_validated(base: *mut *mut u8, count: usize) -> Self {
        Self { base, count }
    }

    pub fn len(&self) -> usize {
        self.count
    }

    pub fn is_empty(&self) -> bool {
        self.count == 0
    }
}

impl IntoIterator for ExternalPointerSlots {
    type Item = *mut *mut u8;
    type IntoIter = ExternalSlotIter;

    fn into_iter(self) -> Self::IntoIter {
        ExternalSlotIter { span: self, next: 0 }
    }
}

pub struct ExternalSlotIter {
    span: ExternalPointerSlots,
    next: usize,
}

impl Iterator for ExternalSlotIter {
    type Item = *mut *mut u8;

    fn next(&mut self) -> Option<Self::Item> {
        if self.next == self.span.count {
            return None;
        }
        // Pointer construction does not access the allocation; dereferencing
        // remains the checked collector/owner's unsafe operation.
        let slot = self.span.base.wrapping_add(self.next);
        self.next += 1;
        Some(slot)
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        let remaining = self.span.count - self.next;
        (remaining, Some(remaining))
    }
}

impl ExactSizeIterator for ExternalSlotIter {}
impl std::iter::FusedIterator for ExternalSlotIter {}
