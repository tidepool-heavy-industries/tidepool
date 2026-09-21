//! Derive macros for converting between Rust types and materialized Tidepool values.
//!
//! These derives bridge the Haskell–Rust boundary: a Haskell GADT describing
//! an effect becomes a Rust enum via `#[derive(FromHaskell)]`, and Rust values go
//! back via `#[derive(ToHaskell)]`.
//!
//! # Enum mapping
//!
//! Each Rust variant maps to a Haskell data constructor by name. Use
//! `#[haskell(name = "...")]` when the Rust and Haskell names differ:
//!
//! ```no_run
//! use tidepool_bridge_derive::FromHaskell;
//!
//! // Haskell:  data Console a where  Emit :: String -> Console ()
//! #[derive(FromHaskell)]
//! enum ConsoleReq {
//!     #[haskell(name = "Emit")]
//!     Emit(String),
//! }
//! ```
//!
//! Tuple and named variant fields are matched against the constructor's
//! arguments in declaration order. Field names are Rust-side structure; Haskell
//! constructors remain positional.
//!
//! # Struct mapping
//!
//! Single-constructor types can use a struct instead of an enum:
//!
//! ```no_run
//! use tidepool_bridge_derive::ToHaskell;
//!
//! #[derive(ToHaskell)]
//! #[haskell(name = "MyRecord")]
//! struct MyRecord { field1: String, field2: i64 }
//! ```

#![warn(clippy::unwrap_used, clippy::expect_used)]
extern crate proc_macro;

mod codegen;
mod parse;
mod record_codegen;

use parse::DataInfo;
use proc_macro::TokenStream;
use syn::{parse_macro_input, DeriveInput};

/// Derive `FromHaskell` to convert a Haskell value (from the JIT) into this Rust type.
///
/// The macro matches on the data constructor tag and extracts fields positionally.
/// Use `#[haskell(name = "HaskellCtorName")]` on variants when names differ.
#[proc_macro_derive(FromHaskell, attributes(haskell))]
pub fn derive_from_haskell(input: TokenStream) -> TokenStream {
    let input = parse_macro_input!(input as DeriveInput);
    match parse::parse_input(&input) {
        Ok(DataInfo::Enum(info)) => codegen::generate_from_haskell(&info).into(),
        Ok(DataInfo::Struct(info)) => codegen::generate_struct_from_haskell(&info).into(),
        Err(e) => e.to_compile_error().into(),
    }
}

/// Derive `ToHaskell` to convert this Rust type into a Haskell value for the JIT.
///
/// The macro builds a `Value::Con` with the appropriate constructor tag and fields.
/// Use `#[haskell(name = "HaskellCtorName")]` on variants when names differ.
#[proc_macro_derive(ToHaskell, attributes(haskell))]
pub fn derive_to_haskell(input: TokenStream) -> TokenStream {
    let input = parse_macro_input!(input as DeriveInput);
    match parse::parse_input(&input) {
        Ok(DataInfo::Enum(info)) => codegen::generate_to_haskell(&info).into(),
        Ok(DataInfo::Struct(info)) => codegen::generate_struct_to_haskell(&info).into(),
        Err(e) => e.to_compile_error().into(),
    }
}

/// Derive `HaskellRecord` to render the Haskell `data` declaration this Rust type
/// mirrors — the single source of truth for the record's Haskell shape.
///
/// The Rust struct/enum drives field order, names, and types; the generated
/// `haskell_decl()` is what the Haskell side must use. Per-field overrides:
/// `#[haskell(hs = "haskellName")]` renames a field, `#[haskell(hs_type = "T")]`
/// overrides its rendered Haskell type. The type also registers itself in an
/// `inventory` (`tidepool_bridge::all_record_decls`).
#[proc_macro_derive(HaskellRecord, attributes(haskell))]
pub fn derive_haskell_record(input: TokenStream) -> TokenStream {
    let input = parse_macro_input!(input as DeriveInput);
    match parse::parse_input(&input).and_then(|info| record_codegen::generate_haskell_record(&info))
    {
        Ok(ts) => ts.into(),
        Err(e) => e.to_compile_error().into(),
    }
}
