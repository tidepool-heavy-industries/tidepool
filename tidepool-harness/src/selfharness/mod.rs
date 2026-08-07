//! The self-iterating harness (`plans/self-iterating-harness/`): the outer
//! `render`/`loop` driver over the `Harness`/`Agent` monad split (01/02/03),
//! layered on top of this crate's existing turn engine + node tree rather
//! than replacing them — the nested Agent session that answers a
//! `runLLMTurn` hole IS a [`crate::harness::Harness`] node, driven by the
//! same `run_to_hole_or_done` turn loop the fork/return-control path already
//! uses.
//!
//! # Scaffold phase (07-impl-orchestration.md S1/S3/S4)
//!
//! Every seam here is a FROZEN CONTRACT, not a working implementation —
//! `unimplemented!()` bodies with coherent signatures for the fork wave
//! (WS-A/C/D/E/G) to build against:
//!
//! - [`driver`] (WS-A) — the outer lifecycle + the render/loop alternation,
//!   plus the `runLLMTurn`-hole-servicing seam.
//! - [`lifecycle`] (WS-A) — the driver's outer state machine, modeled on
//!   `tidepool_repl::state::SessionState`.
//! - [`state_cross`] (WS-C) — `State` JSON crossing at a loop boundary.
//! - [`harness_source`] (WS-D) — bootstrap-load of the authored
//!   `render`/`loop`/`State` source.
//! - [`observer`] (WS-H) — the pluggable event-observer extension point
//!   `driver` emits to; ships a `LogObserver`, stubs a future GUI subscriber
//!   + reactive hooks.
//! - [`persistence`] (W3) — local-file `State` json persist/restore +
//!   transcript-jsonl [`Observer`] impl (D5: "State-to-disk +
//!   restart-reload").
//!
//! `crate::engine::HoleRouting::Finalize` is the other half of the S3 freeze
//! (routing for the `finalize` effect WS-B adds); it lives in `engine.rs`
//! alongside the rest of `HoleRouting`, not here.

pub mod driver;
pub mod harness_source;
pub mod lifecycle;
pub mod observer;
pub mod persistence;
pub mod state_cross;

pub use driver::{answerer_decls, DriverError, SelfHarnessDriver};
pub use harness_source::{load_harness_source, HarnessSource, HarnessSourceError};
pub use lifecycle::SelfHarnessState;
pub use observer::{Event, GuiObserver, LogObserver, Observer, ReactiveHook};
pub use persistence::{JsonlObserver, PersistenceError};
pub use state_cross::{state_in, state_out};
