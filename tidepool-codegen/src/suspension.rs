//! Public contract for starting, parking, and resuming JIT computations.

use std::num::NonZeroUsize;
use std::sync::atomic::{AtomicU64, Ordering};

use cranelift_module::FuncId;
use tidepool_effect::{EffectBoundary, LivePayloadPolicy};
use tidepool_eval::value::Value;
use tidepool_repr::DataConTable;

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

/// How a completed suspendable run materializes its result.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ParkKind {
    /// Bridge the completed result to an owned [`Value`].
    Plain,
    /// Retain one result as a session root, forcing it first when requested.
    Binding { forced: bool },
    /// Force and retain each field of a product result.
    Project { n_fields: NonZeroUsize },
    /// Retain field 0 and bridge field 1 for display.
    Render { field0_forced: bool },
}

/// Compiled function from which a suspendable run starts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SuspensionEntry {
    Main,
    Fragment(FuncId),
}

/// Complete configuration for one suspendable run.
pub struct SuspensionRun<'a> {
    pub entry: SuspensionEntry,
    pub table: &'a DataConTable,
    pub boundary: &'a EffectBoundary,
    pub realm: RealmId,
    pub completion: ParkKind,
    pub live_payload: LivePayloadPolicy,
}

impl<'a> SuspensionRun<'a> {
    #[must_use]
    pub fn main(table: &'a DataConTable, boundary: &'a EffectBoundary, realm: RealmId) -> Self {
        Self {
            entry: SuspensionEntry::Main,
            table,
            boundary,
            realm,
            completion: ParkKind::Plain,
            live_payload: LivePayloadPolicy::None,
        }
    }

    #[must_use]
    pub fn fragment(
        func_id: FuncId,
        table: &'a DataConTable,
        boundary: &'a EffectBoundary,
        realm: RealmId,
        completion: ParkKind,
    ) -> Self {
        Self {
            entry: SuspensionEntry::Fragment(func_id),
            table,
            boundary,
            realm,
            completion,
            live_payload: LivePayloadPolicy::None,
        }
    }

    /// Permit one request field to cross by reference when the data bridge
    /// reports a live closure there.
    #[must_use]
    pub fn with_live_payload(mut self, policy: LivePayloadPolicy) -> Self {
        self.live_payload = policy;
        self
    }
}

/// Outcome of a registry-backed start or resume operation.
#[derive(Debug)]
pub enum ParkedOutcome {
    CompletedValue(Value),
    CompletedBinding {
        value: Value,
        root: crate::old_space::RootSlot,
    },
    CompletedProject {
        roots: Vec<crate::old_space::RootSlot>,
    },
    CompletedRender {
        root: crate::old_space::RootSlot,
        rendered: Value,
    },
    Suspended {
        id: ContinuationId,
        request: Value,
        /// Whether the run's declared live payload is retained by reference on
        /// the parked frame.
        has_live_payload: bool,
    },
}

/// Capacity-one outcome used by the linear session façade.
pub enum Suspendable<T> {
    Completed(T),
    Suspended {
        request: Value,
        has_live_payload: bool,
    },
}

pub type SuspendableOutcome = Suspendable<Value>;

/// Input supplied when resuming a parked continuation.
pub enum ResumeInput {
    /// A validated value to materialize into the retained heap.
    Answer(Value),
    /// A value already rooted in this machine's retained heap.
    Handle(ValueHandle),
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
