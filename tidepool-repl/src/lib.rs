//! `tidepool-repl` — a GHCi-style stateful Haskell session as a SEPARATE MCP
//! server from the `tidepool` eval server (whose request path is untouched).
//!
//! ONE implicit session owns ONE live `JitEffectMachine` (the multi-agent story
//! is one repl server per agent). The primary tool `session_run` takes a LIST of
//! GHCi-capable items — top-level declarations (Lane A), bind statements
//! (`x <- e` / `let x = e`), bare expressions, and `:commands` — classified
//! automatically and run in sequence on the SAME machine, so the declaration
//! scope and value heap persist across turns. The session auto-opens on the
//! first `session_run`.
//!
//! An item's `ask` STOWS rather than blocks: the JIT continuation stays on the
//! machine as data and the whole `Session` — carrying the suspended item's tail
//! and the block loop's cursor — goes back into its manager slot until
//! `session_resume`. No OS thread is parked. This is the same threadless engine
//! `tidepool-harness` drives (`tidepool_runtime::session::PersistentSession`);
//! the repl is its single-node client.
//!
//! Tools: `session_run` · `session_resume` · `session_reset` (reset drops the
//! machine + any pending `ask`, then opens fresh). Resource:
//! `tidepool://session/bindings` — live session state as JSON.
//!
//! Module map:
//! - [`command`] — `SessionCommand` (`Block` of `BlockItem`s) + `TurnOutcome`.
//! - [`session`] — the resident `Session`: `run_block`'s re-enterable block
//!   cursor over the per-item `run_def`/`run_eval`/`run_meta`, and the stowed
//!   per-item tails a suspension re-enters through.
//! - [`manager`] — the single-session ownership slot (`Idle | Running |
//!   Suspended`) with atomic checkout/restore.
//! - [`server`] — the MCP `ServerHandler`, the three session tools, and the bindings resource.
//! - [`introspect`] — `:i` source-scan resolution for stdlib/preamble types.
//! - [`truncate`] — Rust-side result truncation + the `:stub <n>` fetch lane.

#![warn(clippy::unwrap_used, clippy::expect_used)]
pub mod command;
pub mod introspect;
pub mod kernel_adapter;
pub mod manager;
pub mod server;
pub mod session;
pub mod truncate;

pub use command::{DeclText, ExprText, MetaCommand, SessionCommand, TurnOutcome};
pub use manager::SessionManager;
pub use server::{ReplServerConfig, TidepoolReplServer};
pub use session::{BoxedStack, Session, SessionConfig, StackFactory, DEFAULT_NURSERY_SIZE};
