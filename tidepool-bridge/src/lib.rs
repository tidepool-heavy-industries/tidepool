//! Bidirectional conversion between Rust types and Tidepool Core values.
//!
//! Defines `FromCore` and `ToCore` traits with derive macros for automatic
//! marshalling across the Haskell-Rust boundary.

#![warn(clippy::unwrap_used, clippy::expect_used)]
pub mod error;
pub mod impls;
pub mod json;
pub mod record;
pub mod traits;

pub use error::*;
pub use impls::{field_decode_error, get_resilient, type_mismatch};
pub use record::*;
pub use traits::*;
