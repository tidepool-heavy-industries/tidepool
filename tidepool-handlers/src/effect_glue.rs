//! The tidepool-handlers projection of a single-source effect definition
//! (`tidepool_mcp::<effect>_effect_def!` — see `tidepool-mcp/src/effect_defs.rs`
//! for the grammar and the design rationale).
//!
//! Expanding a definition through [`effect_rust_projection!`] generates the
//! whole mechanical Rust half of the effect contract:
//!
//! - `#[derive(FromCore)] pub enum <Eff>Req` — one variant per GADT
//!   constructor, named EXACTLY as the Haskell constructor (no
//!   `#[core(name)]` rename layer), fields from the definition's Rust arg
//!   types;
//! - `impl DescribeEffect for <Handler>` — wired to the generated
//!   `tidepool_mcp::<decl_fn>()`;
//! - `impl EffectHandler<CapturedOutput> for <Handler>` — the dispatch match,
//!   each arm calling the hand-written inherent method named in the
//!   definition's `method` slot.
//!
//! What stays hand-written in the effect's module: the handler struct itself
//! (its fields/constructor are configuration, not contract) and one inherent
//! method per verb — `fn <method>(&mut self, cx: &EffectContext<'_,
//! CapturedOutput>, <args>) -> Result<Response, EffectError>` — the handler
//! BODY, free to use any `cx.respond*` variant its result shape needs.
macro_rules! effect_rust_projection {
    (
        effect $eff:ident,
        handler $handler:ident,
        req $req:ident,
        decl_fn $decl_fn:ident,
        description $desc:tt,
        type_defs $tds:tt,
        $(errors $errname:ident [
            $($evariant:tt),* $(,)?
        ],)?
        verbs [
            $({ ctor $ctor:ident,
                method $method:ident,
                args { $($an:ident : $ah:literal as $ar:ty),* $(,)? },
                ret $ret:literal
                $(, errors $everr:ident)?
                $(,)?
            }),* $(,)?
        ],
        helpers $hs:tt $(,)?
    ) => {
        // Generated failure ADT (#335). Present only when the definition has an
        // `errors` block; its variants live in the generated `Tidepool.Effects`
        // module, so ToCore resolves them by qualified name.
        $( crate::effect_glue::error_enum!($errname, $($evariant),*); )?

        #[derive(tidepool_bridge_derive::FromCore)]
        pub enum $req {
            $( $ctor($($ar),*) ),*
        }

        impl tidepool_mcp::DescribeEffect for $handler {
            fn effect_decl() -> tidepool_mcp::EffectDecl {
                tidepool_mcp::$decl_fn()
            }
        }

        impl tidepool_effect::dispatch::EffectHandler<tidepool_mcp::CapturedOutput> for $handler {
            type Request = $req;

            fn handle(
                &mut self,
                req: $req,
                cx: &tidepool_effect::dispatch::EffectContext<'_, tidepool_mcp::CapturedOutput>,
            ) -> Result<tidepool_effect::Response, tidepool_effect::error::EffectError> {
                match req {
                    $(
                        $req::$ctor($($an),*) => crate::effect_glue::dispatch_body!(
                            self, cx, $method, [ $($an),* ] $(, errors $everr)?
                        ),
                    )*
                }
            }
        }
    };
}
pub(crate) use effect_rust_projection;

/// Emit the whole `errors` ADT as one item (a macro can't sit in enum-variant
/// position, so the variants are built inline here from the re-matched block).
/// `Debug` lets `Display`/`fs_err_to_effect` render it when an untagged method
/// forwards a shared-helper failure. ToCore/FromCore use plain name+arity lookup
/// (the variant names are unique), matching the bridged records — no module
/// qualifier (the generated `data` decl's constructors are not registered under
/// a `Module.Ctor` qualified name).
macro_rules! error_enum {
    ( $errname:ident, $({ ctor $c:ident,
                          fields { $($efn:ident : $efh:literal as $efr:ty),* $(,)? },
                          doc $d:literal $(,)? }),* $(,)? ) => {
        // FromCore is for test-side decoding of a `Left err`; the error is only
        // ever SENT (ToCore) in production. Debug backs the `Display` path;
        // PartialEq/Eq let handler tests assert on decoded `Left` payloads.
        #[derive(
            tidepool_bridge_derive::ToCore,
            tidepool_bridge_derive::FromCore,
            Debug,
            PartialEq,
            Eq
        )]
        pub enum $errname {
            $( $c( $($efr),* ) ),*
        }
    };
}
pub(crate) use error_enum;

/// The BODY of one dispatch match arm (the arm's pattern is written inline in
/// the projection — a macro can't expand to a whole `pat => body` arm). An
/// `errors`-tagged verb's method returns typed `Result<T, ErrEnum>` and takes
/// no `cx`; the body wraps it via `cx.respond` (Ok→Right, Err→Left), so the
/// handler is total by construction. A plain verb's method takes `cx` and
/// returns `Result<Response, EffectError>`; the body forwards it directly.
///
/// The receiver is threaded in as `$s:expr` (`self` from the projection site):
/// `self` written literally here would resolve to the module, not the method
/// receiver (macro hygiene), so the projection passes its own `self` token.
macro_rules! dispatch_body {
    ( $s:expr, $cx:ident, $method:ident, [ $($an:ident),* $(,)? ], errors $everr:ident ) => {
        $cx.respond($s.$method($($an),*))
    };
    ( $s:expr, $cx:ident, $method:ident, [ $($an:ident),* $(,)? ] ) => {
        $s.$method($cx $(, $an)*)
    };
}
pub(crate) use dispatch_body;
