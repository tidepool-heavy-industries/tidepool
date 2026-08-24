//! # Tidepool
//!
//! Compile Haskell [`freer-simple`](https://hackage.haskell.org/package/freer-simple) effect
//! stacks into Cranelift-backed state machines drivable from Rust.
//!
//! This crate serves two roles:
//!
//! - **Binary** (`cargo install tidepool`): an MCP server.
//! - **Library**: re-exports the project's crates. Start with
//!   [`compile_haskell`] to load a compiled Haskell module, then use
//!   [`tidepool_codegen::jit_machine::JitEffectMachine`] to JIT-compile and run it
//!   with your effect handlers.
//!
//! See the repo-root `CLAUDE.md` for the crate map.

#![warn(clippy::unwrap_used, clippy::expect_used)]
pub mod compile_report;
pub use tidepool_bridge as bridge;
pub use tidepool_bridge_derive as bridge_derive;
pub use tidepool_codegen as codegen;
pub use tidepool_effect as effect;
pub use tidepool_eval as eval;
pub use tidepool_heap as heap;
pub use tidepool_macro as macro_impl; // 'macro' is a keyword
pub use tidepool_mcp as mcp;
pub use tidepool_repr as repr;
pub use tidepool_runtime as runtime;

// Convenience re-exports
pub use tidepool_bridge::{FromCore, ToCore};
pub use tidepool_effect::dispatch::DispatchEffect;
pub use tidepool_eval::Value;
pub use tidepool_repr::{CoreExpr, DataConTable};
pub use tidepool_runtime::{compile_and_run, compile_haskell, EvalResult, RuntimeError};
