//! Public contract for starting, parking, and resuming JIT computations.

use std::num::NonZeroUsize;

use cranelift_module::FuncId;
use tidepool_effect::EffectBoundary;
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
}

/// Opaque identity of a rooted value in a machine's retained heap.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ValueHandle(pub u64);

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
        }
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
        /// Whether a closure-valued finalize payload is retained by reference
        /// on the parked frame.
        has_finalized_closure: bool,
    },
}

/// Capacity-one outcome used by the linear session façade.
pub enum Suspendable<T> {
    Completed(T),
    Suspended {
        request: Value,
        has_finalized_closure: bool,
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
