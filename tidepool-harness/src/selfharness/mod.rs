//! The self-iterating harness (`plans/self-iterating-harness/`): the outer
//! `render`/`loop` driver over the `Harness`/`Agent` monad split. The Agent
//! node that answers a `runLLMTurn` hole IS a [`crate::harness::Harness`]
//! node — attached to the shared outer session as a per-loop realm rather
//! than owning a session of its own (the one-session collapse; see
//! `driver`'s module doc).
//!
//! # Modules
//!
//! - [`driver`] — the outer lifecycle + the render/loop alternation,
//!   the `runLLMTurn`-hole servicer, and the `askUser` operator-form servicer
//!   (both the answerer and the outer-loop form paths).
//! - [`lifecycle`] — the driver's outer state machine, modeled on
//!   `tidepool_repl::state::SessionState`.
//! - [`state_cross`] — `State` JSON crossing at a loop boundary.
//! - [`harness_source`] — bootstrap-load of the authored
//!   `render`/`loop`/`State` source.
//! - [`observer`] — the pluggable event-observer extension point
//!   `driver` emits to; ships a `LogObserver`, stubs a future GUI subscriber
//!   + reactive hooks.
//! - [`operator`] — the frozen [`operator::OperatorGate`] seam the driver
//!   blocks on for operator input + the [`operator::FormShape`] wire
//!   [`operator::OperatorGate::present_form`] carries; ships a headless
//!   [`operator::StdinGate`].
//! - [`persistence`] — local-file checkpoint (state + compaction + harness-
//!   source fingerprint, one generation-tagged record) persist/restore +
//!   transcript-jsonl [`Observer`] impl + the durable-log path helpers.
//!
//! `crate::engine::HoleRouting::Finalize` is the other half of the
//! synchronous hole-routing contract (routing for the `finalize` effect);
//! it lives in `engine.rs` alongside the rest of `HoleRouting`, not here.

pub mod driver;
pub mod harness_source;
pub mod lifecycle;
pub mod observer;
pub mod operator;
pub mod persistence;
pub mod state_cross;

pub use driver::{answerer_decls, DriverError, SelfHarnessDriver};
pub use harness_source::{load_harness_source, HarnessSource, HarnessSourceError};
pub use lifecycle::SelfHarnessState;
pub use observer::{Event, LogObserver, Observer};
pub use operator::{ContinueSignal, OperatorGate, StdinGate};
pub use persistence::{Checkpoint, JsonlObserver, PersistenceError};
pub use state_cross::{state_in, state_out};
