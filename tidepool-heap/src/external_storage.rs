//! Owner-authenticated external payload views shared by graph traversals.

/// The two GC-external payload shapes owned by a machine.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExternalStorageKind {
    Bytes,
    BoxedArray,
}

/// The machine ledger authenticates external edges; descriptor metadata only
/// states which kind an object requires. Array handles are not provenance.
///
/// # Safety
/// Returned slots remain allocated, initialized and exclusively available to
/// the collector through the complete copy/fixup interval. No implementation
/// may collect, force a value, resize, revoke or sweep during this callback.
pub unsafe trait ExternalPayloadOwner {
    fn slots(
        &self,
        published: *mut u8,
        expected: ExternalStorageKind,
    ) -> Result<ExternalPointerSlots, ExternalStorageValidationError>;
}

/// Validation failures while authenticating an external payload view.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExternalStorageValidationError {
    Untracked(*mut u8),
    InvalidBase,
    LayoutAlignment {
        actual: usize,
    },
    PointerAlignment {
        kind: ExternalStorageKind,
    },
    KindMismatch {
        expected: ExternalStorageKind,
        actual: ExternalStorageKind,
    },
    PublishedPointerMismatch {
        kind: ExternalStorageKind,
    },
    SpanOverflow {
        kind: ExternalStorageKind,
        logical_len: usize,
    },
    SpanExceedsAllocation {
        kind: ExternalStorageKind,
        required: usize,
        allocated: usize,
    },
    CapacityPrefixMismatch {
        recorded: usize,
        stored: usize,
    },
    LogicalLengthMismatch {
        kind: ExternalStorageKind,
        recorded: usize,
        stored: usize,
    },
    LedgerChanged,
}

/// A bounded span of managed slots, without a per-visit allocation. This is
/// not ownership: the machine's allocation ledger must keep the backing
/// allocation alive and unchanged throughout traversal and relocation.
///
/// The machine validates kind, length, capacity, and provenance before it
/// constructs this span.
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
        ExternalSlotIter {
            span: self,
            next: 0,
        }
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

#[cfg(test)]
mod tests {
    use super::ExternalPointerSlots;

    #[test]
    fn span_iterator_is_exact_fused_and_in_address_order() {
        let mut slots: [*mut u8; 3] = [std::ptr::null_mut(); 3];
        // SAFETY: `slots` remains live and unchanged for the iterator's use.
        let span = unsafe { ExternalPointerSlots::from_validated(slots.as_mut_ptr(), slots.len()) };
        let first = slots.as_mut_ptr();
        let mut iter = span.into_iter();

        assert_eq!(iter.len(), 3);
        assert_eq!(iter.next(), Some(first));
        assert_eq!(iter.len(), 2);
        assert_eq!(iter.next(), Some(unsafe { first.add(1) }));
        assert_eq!(iter.next(), Some(unsafe { first.add(2) }));
        assert_eq!(iter.len(), 0);
        assert_eq!(iter.next(), None);
        assert_eq!(iter.next(), None);
    }

    #[test]
    fn empty_span_is_exact_and_fused() {
        // SAFETY: an empty span never dereferences its base.
        let span = unsafe {
            ExternalPointerSlots::from_validated(
                std::ptr::NonNull::<*mut u8>::dangling().as_ptr(),
                0,
            )
        };
        let mut iter = span.into_iter();
        assert_eq!(iter.len(), 0);
        assert_eq!(iter.next(), None);
        assert_eq!(iter.next(), None);
    }
}
