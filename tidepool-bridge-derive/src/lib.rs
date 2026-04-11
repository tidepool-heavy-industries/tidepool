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
