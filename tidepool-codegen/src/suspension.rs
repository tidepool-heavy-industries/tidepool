//! Public contract for starting, parking, and resuming JIT computations.

use std::sync::atomic::{AtomicU64, Ordering};

use tidepool_bridge::HaskellValue;

/// Identity of a continuation parked in one machine. Ids are never reused.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ContinuationId(pub u64);

/// Ownership scope for parked continuations and rooted value handles.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct RealmId(pub u64);

impl RealmId {
    /// The machine's root resource scope.
    pub const ROOT: Self = Self(0);

    /// Mint a process-unique runtime resource scope.
    ///
    /// All production scope allocation goes through this issuer. Keeping one
    /// sequence avoids caller-owned numeric partitions and makes scopes safe
    /// to compose on a shared machine.
    #[must_use]
    pub fn fresh() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(1);
        let id = NEXT
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |next| {
                next.checked_add(1)
            })
            .unwrap_or_else(|_| panic!("runtime resource-scope ids exhausted"));
        Self(id)
    }
}

/// Process-unique identity of a rooted value in a machine's retained heap.
///
/// Production handles are minted by the owning machine. Their process-wide
/// uniqueness makes accidental delivery to another machine fail as unknown
/// instead of aliasing an unrelated value there.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ValueHandle(pub u64);

impl ValueHandle {
    /// Mint a process-unique handle identity for a rooted machine value.
    pub(crate) fn fresh() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(1);
        let id = NEXT
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |next| {
                next.checked_add(1)
            })
            .unwrap_or_else(|_| panic!("rooted-value handle ids exhausted"));
        Self(id)
    }
}

/// Input supplied when resuming a parked continuation.
pub enum ResumeInput {
    /// A validated value to materialize into the retained heap.
    Answer(HaskellValue),
    /// A value already rooted in this machine's retained heap.
    Handle(ValueHandle),
    /// A constructor whose final field borrows an existing live value.
    FramedHandle {
        handle: ValueHandle,
        constructor: tidepool_repr::DataConId,
        prefix: Vec<HaskellValue>,
    },
    /// Consume the continuation without running it.
    Abort(String),
}

#[cfg(test)]
mod tests {
    use super::{RealmId, ValueHandle};

    #[test]
    fn fresh_realms_are_distinct_and_never_root() {
        let first = RealmId::fresh();
        let second = RealmId::fresh();
        assert_ne!(first, RealmId::ROOT);
        assert_ne!(second, RealmId::ROOT);
        assert_ne!(first, second);
    }

    #[test]
    fn fresh_value_handles_are_distinct_and_reserve_zero_for_invalid_tests() {
        let first = ValueHandle::fresh();
        let second = ValueHandle::fresh();
        assert_ne!(first, ValueHandle(0));
        assert_ne!(second, ValueHandle(0));
        assert_ne!(first, second);
    }
}
