//! Bidirectional conversion between Rust types and Tidepool Core values.
//!
//! Defines `FromCore` and `ToCore` traits with derive macros for automatic
//! marshalling across the Haskell-Rust boundary.

pub mod error;
pub mod impls;
pub mod json;
pub mod traits;

pub use error::*;
pub use impls::get_resilient;
pub use traits::*;
