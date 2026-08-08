//! The self-iterating harness (`plans/self-iterating-harness/`): the outer
//! `render`/`loop` driver over the `Harness`/`Agent` monad split (01/02/03),
//! layered on top of this crate's existing turn engine + node tree rather
//! than replacing them — the nested Agent session that answers a
//! `runLLMTurn` hole IS a [`crate::harness::Harness`] node, driven by the
//! same `run_to_hole_or_done` turn loop the fork/return-control path already
//! uses.
//!
//! # Modules
//!
//! - [`driver`] (WS-A) — the outer lifecycle + the render/loop alternation,
//!   the `runLLMTurn`-hole servicer, and the `askUser` operator-form servicer
//!   (self-iterating-harness Wave 2, both the answerer and the outer-loop
//!   form paths).
//! - [`lifecycle`] (WS-A) — the driver's outer state machine, modeled on
//!   `tidepool_repl::state::SessionState`.
//! - [`state_cross`] (WS-C) — `State` JSON crossing at a loop boundary.
//! - [`harness_source`] (WS-D) — bootstrap-load of the authored
//!   `render`/`loop`/`State` source.
//! - [`observer`] (WS-H) — the pluggable event-observer extension point
//!   `driver` emits to; ships a `LogObserver`, stubs a future GUI subscriber
//!   + reactive hooks.
//! - [`operator`] (Wave 2) — the frozen [`operator::OperatorGate`] seam the
//!   driver blocks on for operator input + the `FormSpec`/`Submission` wire
//!   types; ships a headless [`operator::StdinGate`].
//! - [`persistence`] (W3) — local-file `State` json persist/restore +
//!   transcript-jsonl [`Observer`] impl (D5: "State-to-disk +
//!   restart-reload") + the durable-log path helpers (WS4).
//!
//! `crate::engine::HoleRouting::Finalize` is the other half of the S3 freeze
//! (routing for the `finalize` effect WS-B adds); it lives in `engine.rs`
//! alongside the rest of `HoleRouting`, not here.

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
pub use operator::{EnumOption, Field, FieldKind, FormSpec, OperatorGate, StdinGate, Submission};
pub use persistence::{JsonlObserver, PersistenceError};
pub use state_cross::{state_in, state_out};
