//! `tidepool-repl` — a GHCi-style stateful Haskell session as a SEPARATE MCP
//! server from the `tidepool` eval server (whose request path is untouched).
//!
//! ONE implicit session pins ONE resident worker thread + live `JitEffectMachine`
//! (the multi-agent story is one repl server per agent). The primary tool
//! `session_run` takes a LIST of GHCi-capable items — top-level declarations
//! (Lane A), bind statements (`x <- e` / `let x = e`), bare expressions, and
//! `:commands` — classified automatically and run in sequence on the SAME
//! machine, so the declaration scope and value heap persist across turns. The
//! session auto-opens on the first `session_run`. An item's `ask` parks the
//! worker thread until `session_resume`.
//!
//! Tools: `session_run` · `session_resume` · `session_reset` (reset drops the
//! machine + any pending `ask`, then opens fresh). Resource:
//! `tidepool://session/bindings` — live session state as JSON.
//!
//! Module map:
//! - [`command`] — `SessionCommand` (`Block` of `BlockItem`s) + `TurnOutcome`.
//! - [`session`] — the resident `Session` + `SessionHandle<Open/Closed>` type-state;
//!   `run_block` drives a block by reusing the per-item `run_def`/`run_eval`/`run_meta`.
//! - [`worker`] — the resident worker thread + single-consumer channel + single-slot manager.
//! - [`server`] — the MCP `ServerHandler`, the three session tools, and the bindings resource.
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
