extern crate proc_macro;
use proc_macro::TokenStream;

mod expand;

/// Embeds and evaluates a CBOR-serialized Haskell Core expression at runtime.
///
/// This macro embeds the contents of the file at the provided path (relative to the
/// calling file) using `include_bytes!`. At runtime, it deserializes the CBOR
/// data into a `CoreExpr` and evaluates it using the tree-walking interpreter.
///
/// The expanded expression evaluates to a `Result<core_eval::Value, core_eval::error::EvalError>`.
/// Evaluation errors are reported via this `Result`.
///
/// # Panics
///
/// The generated code will panic during CBOR deserialization if the embedded data
/// is malformed, incompatible with the expected format, or otherwise cannot be
/// decoded into a `CoreExpr`. Such failures typically indicate a build-system
/// synchronization issue (e.g. stale CBOR blobs).
///
/// # Dependencies
///
/// The expansion of this macro expects the following crates to be available in the
/// caller's scope:
///
/// - `core_repr`
/// - `core_eval`
///
/// # Returns
///
/// Returns a `Result<core_eval::Value, core_eval::error::EvalError>`.
///
/// # Example
///
/// ```ignore
/// let val = haskell_eval!("../../haskell/test/Identity_cbor/identity.cbor").unwrap();
/// ```
#[proc_macro]
pub fn haskell_eval(input: TokenStream) -> TokenStream {
    expand::expand(input.into()).into()
}