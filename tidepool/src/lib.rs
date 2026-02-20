//! Tidepool: Haskell Core to Rust compiler and runtime.
//!
//! This is the facade crate that re-exports the main components of the Tidepool project.

pub use tidepool_repr as repr;
pub use tidepool_eval as eval;
pub use tidepool_heap as heap;
pub use tidepool_optimize as optimize;
pub use tidepool_bridge as bridge;
pub use tidepool_bridge_derive as bridge_derive;
pub use tidepool_effect as effect;
pub use tidepool_codegen as codegen;
pub use tidepool_macro as macro_impl; // 'macro' is a keyword
pub use tidepool_runtime as runtime;
pub use tidepool_mcp as mcp;

// Convenience re-exports
pub use tidepool_repr::{CoreExpr, DataConTable};
pub use tidepool_eval::Value;
pub use tidepool_bridge::{FromCore, ToCore};
pub use tidepool_effect::dispatch::DispatchEffect;
pub use tidepool_runtime::{compile_and_run, compile_haskell, RuntimeError};
