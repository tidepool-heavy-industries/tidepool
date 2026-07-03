//! Derive macros for converting between Rust types and Tidepool Core values.
//!
//! These derives bridge the Haskell–Rust boundary: a Haskell GADT describing
//! an effect becomes a Rust enum via `#[derive(FromCore)]`, and Rust values go
//! back via `#[derive(ToCore)]`.
//!
//! # Enum mapping
//!
//! Each Rust variant maps to a Haskell data constructor by name. Use
//! `#[core(name = "...")]` when the Rust and Haskell names differ:
//!
//! ```no_run
//! use tidepool_bridge_derive::FromCore;
//!
//! // Haskell:  data Console a where  Emit :: String -> Console ()
//! #[derive(FromCore)]
//! enum ConsoleReq {
//!     #[core(name = "Emit")]
//!     Emit(String),
//! }
//! ```
//!
//! Variant fields are positionally matched against the constructor's arguments.
//!
//! # Struct mapping
//!
//! Single-constructor types can use a struct instead of an enum:
//!
//! ```no_run
//! use tidepool_bridge_derive::ToCore;
//!
//! #[derive(ToCore)]
//! #[core(name = "MyRecord")]
//! struct MyRecord { field1: String, field2: i64 }
//! ```

extern crate proc_macro;

mod codegen;
mod parse;
mod record_codegen;

use parse::DataInfo;
use proc_macro::TokenStream;
use syn::{parse_macro_input, DeriveInput};

/// Derive `FromCore` to convert a Core `Value` (from the JIT) into this Rust type.
///
/// The macro matches on the data constructor tag and extracts fields positionally.
/// Use `#[core(name = "HaskellCtorName")]` on variants when names differ.
#[proc_macro_derive(FromCore, attributes(core))]
pub fn derive_from_core(input: TokenStream) -> TokenStream {
    let input = parse_macro_input!(input as DeriveInput);
    match parse::parse_input(&input) {
        Ok(DataInfo::Enum(info)) => codegen::generate_from_core(&info).into(),
        Ok(DataInfo::Struct(info)) => codegen::generate_struct_from_core(&info).into(),
        Err(e) => e.to_compile_error().into(),
    }
}

/// Derive `ToCore` to convert this Rust type into a Core `Value` for the JIT.
///
/// The macro builds a `Value::Con` with the appropriate constructor tag and fields.
/// Use `#[core(name = "HaskellCtorName")]` on variants when names differ.
#[proc_macro_derive(ToCore, attributes(core))]
pub fn derive_to_core(input: TokenStream) -> TokenStream {
    let input = parse_macro_input!(input as DeriveInput);
    match parse::parse_input(&input) {
        Ok(DataInfo::Enum(info)) => codegen::generate_to_core(&info).into(),
        Ok(DataInfo::Struct(info)) => codegen::generate_struct_to_core(&info).into(),
        Err(e) => e.to_compile_error().into(),
    }
}

/// Derive `CoreRecord` to render the Haskell `data` declaration this Rust type
/// mirrors — the single source of truth for the record's Haskell shape.
///
/// The Rust struct/enum drives field order, names, and types; the generated
/// `haskell_decl()` is what the Haskell side must use. Per-field overrides:
/// `#[core(hs = "haskellName")]` renames a field, `#[core(hs_type = "T")]`
/// overrides its rendered Haskell type. The type also registers itself in an
/// `inventory` (`tidepool_bridge::all_record_decls`).
#[proc_macro_derive(CoreRecord, attributes(core))]
pub fn derive_core_record(input: TokenStream) -> TokenStream {
    let input = parse_macro_input!(input as DeriveInput);
    match parse::parse_input(&input).and_then(|info| record_codegen::generate_core_record(&info)) {
        Ok(ts) => ts.into(),
        Err(e) => e.to_compile_error().into(),
    }
}
