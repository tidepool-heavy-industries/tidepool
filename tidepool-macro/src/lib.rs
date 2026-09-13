#![warn(clippy::unwrap_used, clippy::expect_used)]
extern crate proc_macro;
use proc_macro::TokenStream;

mod expand;

/// Embeds inline Haskell source as a Core expression with its DataConTable.
///
/// Writes the Haskell source to a temporary file, compiles it via
/// `tidepool-extract` (see [`haskell_eval`] for the resolution order), and
/// embeds the resulting CBOR.
///
/// Supports `include` paths for importing local Haskell modules.
///
/// # Returns
///
/// Returns `(tidepool_repr::CoreExpr, tidepool_repr::DataConTable)`.
///
/// # Examples
///
/// This example is intentionally `ignore`'d: the macro runs `nix run
/// .#tidepool-extract` at proc-macro expansion time, which cannot execute in a
/// doctest sandbox. See `examples/guess` for a worked use.
///
/// ```ignore
/// let (expr, table) = haskell_inline! {
///     target = "game",
///     include = "haskell",
///     r#"
///         import Effects
///
///         game :: Eff '[Console, Rng] ()
///         game = do
///           target <- randInt 1 100
///           emit "I'm thinking of a number between 1 and 100."
///           guessLoop target
///     "#
/// };
/// ```
#[proc_macro]
pub fn haskell_inline(input: TokenStream) -> TokenStream {
    expand::expand_inline(input.into()).into()
}
