//! # Tidepool
//!
//! Compile Haskell [`freer-simple`](https://hackage.haskell.org/package/freer-simple) effect
//! stacks into Cranelift-backed state machines drivable from Rust.
//!
//! This crate serves two roles:
//!
//! - **Binary** (`cargo install tidepool`): an MCP server with built-in effect handlers
//!   for Console, KV, Fs, HTTP, Exec, Lsp, Llm, Git, Time, and (debug-only) Meta.
//! - **Library**: re-exports the main components of the Tidepool project. Start with
//!   [`compile_haskell`] to load a compiled Haskell module, then use
//!   [`tidepool_codegen::jit_machine::JitEffectMachine`] to JIT-compile and run it
//!   with your effect handlers.
//!
//! # Crate overview
//!
//! | Crate | Purpose |
//! |-------|---------|
//! | [`repr`] | Core IR types: [`CoreExpr`], [`DataConTable`], CBOR serialization |
//! | [`eval`] | Tree-walking interpreter with lazy evaluation |
//! | [`heap`] | Manual heap layout and copying GC for JIT runtime |
//! | [`optimize`] | Optimization passes: beta reduction, DCE, inlining |
//! | [`bridge`] | [`FromCore`] / [`ToCore`] traits for Rust <-> Core values |
//! | [`bridge_derive`] | `#[derive(FromCore)]` proc-macro |
//! | [`macro_impl`] | `haskell_inline!` proc-macro for build-time Haskell compilation |
//! | [`effect`] | [`DispatchEffect`] trait and HList-based handler dispatch |
//! | [`codegen`] | Cranelift JIT compiler producing effect machines |
//! | [`runtime`] | High-level API: [`compile_haskell`], [`compile_and_run`], caching |
//! | [`mcp`] | MCP server library, generic over effect handlers |

/// Traits for converting between Rust types and Core values.
pub use tidepool_bridge as bridge;
/// Derive macro for automatic `FromCore` implementations.
pub use tidepool_bridge_derive as bridge_derive;
/// Cranelift JIT compiler and effect machine.
pub use tidepool_codegen as codegen;
/// Effect system: handler trait, context, HList-based dispatch.
pub use tidepool_effect as effect;
/// Tree-walking interpreter with lazy thunks and environment-based evaluation.
pub use tidepool_eval as eval;
/// Manual heap layout, object headers, and copying garbage collector.
pub use tidepool_heap as heap;
/// `haskell_inline!` proc-macro for build-time Haskell compilation.
pub use tidepool_macro as macro_impl; // 'macro' is a keyword
/// MCP (Model Context Protocol) server library.
pub use tidepool_mcp as mcp;
/// Optimization passes operating on the Core IR.
pub use tidepool_optimize as optimize;
/// Core intermediate representation: expression trees, data constructor tables, CBOR serialization.
pub use tidepool_repr as repr;
/// High-level compilation and execution API with caching.
pub use tidepool_runtime as runtime;

// Convenience re-exports
pub use tidepool_bridge::{FromCore, ToCore};
pub use tidepool_effect::dispatch::DispatchEffect;
pub use tidepool_eval::Value;
pub use tidepool_repr::{CoreExpr, DataConTable};
pub use tidepool_runtime::{compile_and_run, compile_haskell, EvalResult, RuntimeError};
