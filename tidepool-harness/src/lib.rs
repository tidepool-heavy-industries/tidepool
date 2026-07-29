//! tidepool-harness — typed-yield session tree over the eval substrate.
//!
//! R0 scaffold: this crate currently defines the CROSS-SEGMENT CONTRACTS
//! (plans/harness-r0/00-scaffold/contracts.md) that the segment leaves
//! compile against. Segment 20 adds the resident-session registry, 30 the
//! event log / replay / forcing / protocol types' behavior, 60 the provider
//! implementations. Types here are the vocabulary, not the machinery —
//! keep them dependency-light.

pub mod compile;
pub mod effect_trace;
pub mod engine;
pub mod forcing;
pub mod harness;
pub mod log;
pub mod provider;
pub mod registry;
pub mod replay;
pub mod tree;
pub mod ui;
pub mod uiof;

pub use compile::{AsksSidecar, CompiledTurn};
pub use engine::{
    classify_hole, ClassifiedHole, EngineConfig, EngineError, HoleRouting, TurnOutcome,
};
pub use forcing::{derive_teaser, fan_badge, price_class, ForkShape, NodeTree, TreeError};
pub use harness::{Escalation, Harness, HarnessError, HeapSummary, NodeSummary, OperatorDecision};
pub use registry::{Checkout, CheckoutError, SessionRegistry};
pub use tree::{FanBadge, HoleId, NodeId, NodeState, PriceClass, SiteId, Slot};
pub use ui::{BadgeKind, Ui};
pub use uiof::{defining_module, resume_expr_from_submission, ui_of};
