//! `tidepool-repl` — a GHCi-style stateful Haskell session as a SEPARATE MCP
//! server from the `tidepool` eval server (whose request path is untouched).
//!
//! Each named session pins ONE resident worker thread + live `JitEffectMachine`.
//! The single tool `session_run` takes a LIST of GHCi-capable items — top-level
//! declarations (Lane A), bind statements (`x <- e` / `let x = e`), bare
//! expressions, and `:commands` — classified automatically and run in sequence
//! on the SAME machine, so the declaration scope and value heap persist across
//! turns. An item's `ask` parks the worker thread until `session_resume`.
//!
//! Tools: `session_open` · `session_run` · `session_close` · `session_resume` ·
//! `session_abort`.
//!
//! Module map:
//! - [`command`] — `SessionCommand` (`Block` of `BlockItem`s) + `TurnOutcome`.
//! - [`session`] — the resident `Session` + `SessionHandle<Open/Closed>` type-state;
//!   `run_block` drives a block by reusing the per-item `run_def`/`run_eval`/`run_meta`.
//! - [`worker`] — the resident worker thread + single-consumer channel + manager.
//! - [`server`] — the MCP `ServerHandler` and the five session tools.
//! - [`ask`] — the parked-thread suspend/resume mechanism for an in-turn `ask`.
//! - [`introspect`] — `:i` source-scan resolution for stdlib/preamble types.
//! - [`truncate`] — Rust-side result truncation + the `:stub <n>` fetch lane.

pub mod ask;
pub mod command;
pub mod introspect;
pub mod server;
pub mod session;
pub mod state;
pub mod truncate;
pub mod worker;

pub use command::{DeclText, ExprText, MetaCommand, SessionCommand, TurnOutcome};
pub use server::{ReplServerConfig, TidepoolReplServer};
pub use session::{Closed, Open, Session, SessionConfig, SessionHandle, DEFAULT_NURSERY_SIZE};
pub use worker::{spawn_worker, SessionManager, WorkerHandle, WorkerJob};
