//! Runtime failures and diagnostics for prepared execution.

use crate::gc::frame_walker::FrameWalkError;
use crate::machine_state::{current_machine, ExternalStorageKind, MachineDisposition};

/// Addresses below this are considered invalid (null page guard).
pub(crate) const MIN_VALID_ADDR: u64 = 0x1000;

/// Runtime errors raised by JIT code via host functions.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum RuntimeError {
    #[error("division by zero")]
    DivisionByZero,
    #[error("arithmetic overflow")]
    Overflow,
    #[error("arithmetic underflow")]
    Underflow,
    #[error("Haskell error called")]
    UserError,
    #[error("Haskell exception raised")]
    RaisedException,
    /// A raised exception whose message was recovered after the call.
    #[error("Haskell exception raised: {0}")]
    RaisedExceptionMessage(String),
    #[error("non-returning prepared entry returned successfully")]
    NoSuccessReturned,
    /// GHC's generated failure path for a refutable binding, incomplete case,
    /// or incomplete guarded equation.
    #[error("pattern match failure: {0}")]
    PatternMatchFailure(String),
    #[error("unsupported runtime capability: {0}")]
    UnsupportedCapability(String),
    #[error("GHC wired-in failure {kind:?}: {message}")]
    WiredInError {
        kind: tidepool_repr::execution_schema::WiredInErrorKind,
        message: String,
    },
    #[error("Haskell undefined forced")]
    Undefined,
    #[error("case trap: scrutinee constructor not among case alternatives (tag mismatch; diagnostics on server stderr)")]
    CaseTrap,
    /// An intact constructor reached a prepared `case` with no alternative
    /// and no default for it. Nothing was written, so only this call fails.
    #[error("case miss: constructor {constructor:?} reached a case in compiled value {owner} that has no alternative for it (a compiler defect; the session remains usable)")]
    CaseMiss {
        constructor: tidepool_repr::DataConId,
        owner: u64,
    },
    #[error("bad pointer in JIT runtime (diagnostics on server stderr)")]
    BadPointer,
    #[error("forced type metadata (should be dead code)")]
    TypeMetadata,
    #[error("application of null function pointer")]
    NullFunPtr,
    #[error("application of non-closure (tag={0})")]
    BadFunPtrTag(u8),
    #[error("heap overflow (nursery exhausted after GC)")]
    HeapOverflow,
    #[error("array index {index} out of bounds for length {len}")]
    ArrayIndexOutOfBounds { index: i64, len: usize },
    #[error("copyByteArray# source and destination name the same array")]
    AliasedByteCopy,
    #[error("data-to-tag expected an evaluated constructor")]
    ExpectedConstructor,
    #[error("external {kind:?} allocation failed ({bytes} bytes)")]
    ExternalAllocationFailed {
        kind: ExternalStorageKind,
        bytes: usize,
    },
    #[error("stack overflow — likely unbounded or very deep non-tail recursion (~20k live nested calls); a long list processed via a strict, TAIL-recursive fold is not bounded by this (only concurrently-live call nesting counts, not total calls made)")]
    StackOverflow,
    #[error("blackhole detected (infinite loop: thunk forced itself)")]
    BlackHole,
    #[error("thunk has invalid evaluation state: {0}")]
    BadThunkState(u8),
    #[error("Haskell error: {0}")]
    UserErrorMsg(String),
    /// External cancellation requested via a `CancelHandle`.
    /// Observed at the next GC safepoint (heap check).
    #[error("execution cancelled by external request")]
    Cancelled,
    #[error("incomplete GC root snapshot: {0}")]
    IncompleteRootSnapshot(FrameWalkError),
    /// Selective retention forwarded part of a prepared nursery but could not
    /// complete sibling fixup. Both semispaces and descriptor owners must be
    /// retained through unwind; the invocation is permanently unavailable.
    #[error("incomplete retention promotion: {0}")]
    IncompletePromotion(tidepool_heap::execution_descriptor::DescriptorTraceError),
    /// Application exhausted its full-signature and prefix probes for an
    /// otherwise-owned callable header. The failing application has not
    /// invoked a target and leaves the machine reusable.
    #[error(
        "unresolved cross-program callee (no installed target satisfies the demanded signature)"
    )]
    UnresolvedCallee,
}

impl RuntimeError {
    /// Classify whether this failure leaves the owning machine safe to reuse.
    /// Language failures, bounded resource failures, and cancellation unwind
    /// through normal run cleanup. Shape, pointer, and compiler-contract
    /// failures mean the live machine can no longer prove heap integrity.
    pub fn machine_disposition(&self) -> MachineDisposition {
        match self {
            Self::WiredInError { kind, .. } => {
                if kind.is_integrity_failure() {
                    MachineDisposition::Unavailable
                } else {
                    MachineDisposition::Reusable
                }
            }
            Self::CaseTrap
            | Self::ExpectedConstructor
            | Self::NoSuccessReturned
            | Self::BadPointer
            | Self::TypeMetadata
            | Self::NullFunPtr
            | Self::BadFunPtrTag(_)
            | Self::BadThunkState(_)
            | Self::IncompleteRootSnapshot(_)
            | Self::IncompletePromotion(_) => MachineDisposition::Unavailable,
            Self::DivisionByZero
            | Self::UnsupportedCapability(_)
            | Self::Overflow
            | Self::Underflow
            | Self::UserError
            | Self::RaisedException
            | Self::RaisedExceptionMessage(_)
            | Self::PatternMatchFailure(_)
            | Self::Undefined
            | Self::HeapOverflow
            | Self::ArrayIndexOutOfBounds { .. }
            | Self::AliasedByteCopy
            | Self::ExternalAllocationFailed { .. }
            | Self::StackOverflow
            | Self::BlackHole
            | Self::UserErrorMsg(_)
            | Self::UnresolvedCallee
            | Self::CaseMiss { .. }
            | Self::Cancelled => MachineDisposition::Reusable,
        }
    }
}

/// Push a diagnostic message to the current prepared machine.
pub fn push_diagnostic(message: String) {
    if let Some(machine) = unsafe { current_machine() } {
        machine.push_diagnostic(message);
    }
}

/// Drain diagnostics accumulated by the current prepared machine.
pub fn drain_diagnostics() -> Vec<String> {
    unsafe { current_machine() }
        .map(|machine| machine.drain_diagnostics())
        .unwrap_or_default()
}
