extern crate proc_macro;
use proc_macro::TokenStream;

mod expand;

/// Embeds and evaluates a Haskell Core expression at runtime.
///
/// Accepts either a `.cbor` path (pre-compiled CBOR) or a `.hs` path (Haskell
/// source compiled on-demand via `nix run .#tidepool-extract`).
///
/// For `.cbor` paths, the file is embedded directly via `include_bytes!`. For
/// `.hs` paths, the macro invokes GHC through nix at compile time, producing
/// CBOR in `target/tidepool-cbor/`, then embeds the result. The `.hs` source
/// file is tracked by cargo for automatic recompilation.
///
/// # Haskell Source Support
///
/// When given a `.hs` path, the macro compiles it via `nix run .#tidepool-extract`.
/// This requires `nix` to be available on `PATH`.
///
/// **Path resolution:** `.hs` paths resolve relative to `CARGO_MANIFEST_DIR`
/// (the crate root). `.cbor` paths resolve relative to the calling file (standard
/// `include_bytes!` behavior).
///
/// For modules with multiple top-level bindings, specify which binding to
/// evaluate using the `::binding` syntax. If only one binding exists (excluding
/// metadata), it is selected automatically.
///
/// # Panics
///
/// The generated code will panic during CBOR deserialization if the embedded data
/// is malformed or incompatible with the expected format.
///
/// # Dependencies
///
/// The expansion of this macro expects the following crates to be available in the
/// caller's scope:
///
/// - `tidepool_repr`
/// - `tidepool_eval`
///
/// # Returns
///
/// Returns a `Result<tidepool_eval::Value, tidepool_eval::error::EvalError>`.
///
/// # Examples
///
/// ```ignore
/// // Pre-compiled CBOR
/// let val = haskell_eval!("../../haskell/test/Identity_cbor/identity.cbor").unwrap();
///
/// // Haskell source (single binding)
/// let val = haskell_eval!("../../haskell/test/SingleBinding.hs").unwrap();
///
/// // Haskell source with binding selector
/// let val = haskell_eval!("../../haskell/test/Identity.hs::identity").unwrap();
/// ```
#[proc_macro]
pub fn haskell_eval(input: TokenStream) -> TokenStream {
    expand::expand(input.into()).into()
}

/// Embeds a Haskell Core expression and its DataConTable without evaluating.
///
/// Unlike `haskell_eval!`, this macro does NOT evaluate the expression. It
/// returns `(CoreExpr, DataConTable)` — suitable for effect-driven execution
/// via `EffectMachine` where the caller controls evaluation.
///
/// Accepts the same path formats as `haskell_eval!`:
/// - `.cbor` path (pre-compiled CBOR, requires a sibling `meta.cbor`)
/// - `.hs` path (compiled on-demand via `nix run .#tidepool-extract`)
/// - `.hs::binding` syntax for multi-binding modules
///
/// # Returns
///
/// Returns `(tidepool_repr::CoreExpr, tidepool_repr::DataConTable)`.
///
/// # Examples
///
/// ```ignore
/// let (expr, table) = haskell_expr!("../Guess.hs::game");
/// let mut heap = tidepool_eval::heap::VecHeap::new();
/// let mut machine = tidepool_effect::EffectMachine::new(&table, &mut heap).unwrap();
/// ```
#[proc_macro]
pub fn haskell_expr(input: TokenStream) -> TokenStream {
    expand::expand_expr(input.into()).into()
}

/// Embeds inline Haskell source as a Core expression with its DataConTable.
///
/// Writes the Haskell source to a temporary file, compiles it via
/// `nix run .#tidepool-extract`, and embeds the resulting CBOR.
///
/// Supports `include` paths for importing local Haskell modules.
///
/// # Returns
///
/// Returns `(tidepool_repr::CoreExpr, tidepool_repr::DataConTable)`.
///
/// # Examples
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