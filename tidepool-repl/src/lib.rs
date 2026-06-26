//! `tidepool-repl` — a GHCi-style stateful Haskell session as a SEPARATE MCP
//! server from the `tidepool` eval server (whose request path is untouched).
//!
//! One resident worker thread pins ONE live `JitEffectMachine` and drives it
//! command-by-command: `session_def` accumulates declarations (Lane A) and
//! `session_eval` runs an `M a` expression on the SAME machine so the value
//! heap persists across turns (the mechanism Wave 3b's value binding builds on).
//!
//! Module map:
//! - [`command`] — the `SessionCommand` sum + `TurnOutcome` (the tool surface).
//! - [`session`] — the resident `Session` + `SessionHandle<Open/Closed>` type-state.
//! - [`worker`] — the resident worker thread + single-consumer channel + manager.
//! - [`server`] — the MCP `ServerHandler` and the seven session tools.
//! - [`ask`] — the parked-thread suspend/resume mechanism for an in-turn `ask`.

pub mod ask;
pub mod command;
pub mod handlers;
pub mod server;
pub mod session;
pub mod worker;

pub use command::{DeclText, ExprText, MetaCommand, SessionCommand, TurnOutcome};
pub use handlers::{ConsoleHandler, default_decls};
pub use server::{ReplServerConfig, TidepoolReplServer};
pub use session::{Closed, Open, Session, SessionConfig, SessionHandle, DEFAULT_NURSERY_SIZE};
pub use worker::{spawn_worker, SessionManager, WorkerHandle, WorkerJob};
