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
        verbs [
            $({ ctor $ctor:ident,
                method $method:ident,
                args { $($an:ident : $ah:literal as $ar:ty),* $(,)? },
                ret $ret:literal
                $(, errors $err:ty)?
                $(,)?
            }),* $(,)?
        ],
        helpers $hs:tt $(,)?
    ) => {
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
                    $( $req::$ctor($($an),*) => self.$method(cx $(, $an)*) ),*
                }
            }
        }
    };
}
pub(crate) use effect_rust_projection;
