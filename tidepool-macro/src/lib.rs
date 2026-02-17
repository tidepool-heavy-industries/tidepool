extern crate proc_macro;
use proc_macro::TokenStream;

mod expand;

/// Embeds and evaluates a CBOR-serialized Haskell Core expression at runtime.
///
/// This macro embeds the contents of the file at the provided path (relative to the
/// calling file) using `include_bytes!`. At runtime, it deserializes the CBOR
/// data into a `CoreExpr` and evaluates it using the tree-walking interpreter.
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