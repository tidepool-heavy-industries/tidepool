//! # Tidepool
//!
//! Compile Haskell [`freer-simple`](https://hackage.haskell.org/package/freer-simple) effect
//! stacks into Cranelift-backed state machines drivable from Rust.
//!
//! This crate serves two roles:
//!
//! - **Binary** (`cargo install tidepool`): an MCP server.
//! - **Library**: re-exports the project's crates. Start with
//!   [`compile_haskell`] to load a prepared-STG program, then run it with
//!   [`compile_and_run`] and your effect handlers.
//!
//! See the repo-root `CLAUDE.md` for the crate map.

#![warn(clippy::unwrap_used, clippy::expect_used)]
pub mod actor_host;
pub mod compile_report;
pub mod haskell_sources;
mod host_dynamic_tools;
pub mod run_map;
pub mod shoal;
pub use tidepool_bridge as bridge;
pub use tidepool_bridge_derive as bridge_derive;
pub use tidepool_codegen as codegen;
pub use tidepool_effect as effect;
pub use tidepool_heap as heap;
pub use tidepool_mcp as mcp;
pub use tidepool_repr as repr;
pub use tidepool_runtime as runtime;

// Convenience re-exports
pub use tidepool_bridge::HaskellValue;
pub use tidepool_bridge::{FromHaskell, ToHaskell};
pub use tidepool_effect::dispatch::DispatchEffect;
pub use tidepool_repr::DataConTable;
pub use tidepool_runtime::{compile_and_run, compile_haskell, EvalResult, RuntimeError};

pub mod operator;

mod generated;
