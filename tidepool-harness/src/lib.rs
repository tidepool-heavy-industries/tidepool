//! tidepool-harness — typed-yield session tree over the eval substrate: the
//! turn engine ([`engine`]), the node tree + forcing gates ([`forcing`]/
//! [`tree`]), the orchestrator ([`harness`]), the durable event log
//! ([`log`]) + replay ([`replay`]), calling-model providers ([`provider`]),
//! and the self-iterating harness's `render`/`loop` driver
//! ([`selfharness`]). See this crate's `CLAUDE.md` for the full module map
//! and the machine-lifecycle/replay-scope notes that don't fit here.

pub mod compile;
pub mod effect_trace;
pub mod engine;
pub mod forcing;
pub mod harness;
pub mod log;
pub mod provider;
pub mod registry;
pub mod replay;
pub mod selfharness;
pub mod synopsis;
pub mod timing;
pub mod tree;

pub use compile::{AsksSidecar, CompiledTurn};
pub use engine::{
    classify_hole, ClassifiedHole, EngineConfig, EngineError, HoleRouting, TurnOutcome,
};
pub use forcing::{derive_teaser, fan_badge, price_class, ForkShape, NodeTree, TreeError};
pub use harness::{Escalation, Harness, HarnessError, OperatorDecision};
pub use registry::{Checkout, CheckoutError, SessionRegistry};
pub use selfharness::{
    answerer_decls, load_harness_source, DriverError, Event, HarnessSource, HarnessSourceError,
    JsonlObserver, LogObserver, Observer, OperatorGate, PersistenceError, SelfHarnessDriver,
    SelfHarnessState, StdinGate,
};
pub use timing::{record_stage, ExtractTiming};
pub use tree::{FanBadge, HoleId, NodeId, NodeState, PriceClass, SiteId, Slot};
