//! Bidirectional conversion between Rust types and materialized Tidepool values.
//!
//! Defines `FromHaskell` and `ToHaskell` traits with derive macros for automatic
//! marshalling across the Haskell-Rust boundary.

pub mod decimal;
pub mod error;
pub mod impls;
pub mod json;
pub mod json_builder;
pub mod record;
pub mod shapes;
pub mod traits;
pub mod value;

pub use error::*;
pub use impls::{field_decode_error, get_qualified, type_mismatch};
pub use record::*;
pub use traits::*;
pub use value::{HaskellValue, HaskellValueFrame, SharedByteArray};
