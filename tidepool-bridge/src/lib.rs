//! Bidirectional conversion between Rust types and Tidepool Core values.
//!
//! Defines `FromCore` and `ToCore` traits with derive macros for automatic
//! marshalling across the Haskell-Rust boundary.

#![warn(clippy::unwrap_used, clippy::expect_used)]
pub mod error;
pub mod impls;
pub mod json;
pub mod json_builder;
pub mod record;
pub mod shapes;
pub mod time;
pub mod traits;
pub mod value;

pub use error::*;
pub use impls::{field_decode_error, get_resilient, type_mismatch};
pub use record::*;
pub use traits::*;
pub use value::{SharedByteArray, Value};
