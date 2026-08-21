//! tidepool-harness — typed-yield session tree over the eval substrate: the
//! turn engine ([`engine`]), the node tree + forcing gates ([`forcing`]/
//! [`tree`]), the orchestrator ([`harness`]), the durable event log
//! ([`log`]) + replay ([`replay`]), frozen context snapshots ([`snapshot`]),
//! calling-model providers ([`provider`]),
//! and the self-iterating harness's `render`/`loop` driver
//! ([`selfharness`]). See this crate's `CLAUDE.md` for the full module map
//! and the machine-lifecycle/replay-scope notes that don't fit here.

#![warn(clippy::unwrap_used, clippy::expect_used)]
pub mod effect_trace;
pub mod engine;
pub mod forcing;
pub mod harness;
pub mod log;
pub mod provider;
pub mod registry;
pub mod replay;
pub mod selfharness;
pub mod snapshot;
pub mod synopsis;
pub mod timing;
pub mod tree;

pub use engine::{
    classify_hole, compile_turn, compile_turns, extract_spawn_count, reset_extract_spawn_count,
    ClassifiedHole, ClassifyError, CompiledTurn, EngineConfig, EngineError, HoleRouting,
    TurnOutcome,
};
pub use forcing::{derive_teaser, NodeTree, TreeError};
pub use harness::{ContextRef, Escalation, Harness, HarnessError, OperatorDecision};
pub use registry::{Checkout, CheckoutError, SessionRegistry};
pub use selfharness::{
    acquire_lease, answerer_decls, answerer_decls_with_delegate, delegate_branches_decl,
    fold_run_journal, list_segments, load_harness_source, retire_lease, segment_path,
    AcquiredLease, ContinueSignal, DriverError, Event, HarnessSource, HarnessSourceError,
    JsonlObserver, LogObserver, Observer, OperatorGate, PersistenceError, ResumeFold,
    RunJournalError, RunLease, SelfHarnessDriver, SelfHarnessState, StdinGate,
};
pub use snapshot::{ContextSnapshot, SnapshotDigest};
pub use tidepool_runtime::AsksSidecar;
pub use timing::{record_stage, ExtractTiming};
pub use tree::{FanBadge, HoleId, NodeId, NodeState, PriceClass, SiteId, Slot};
