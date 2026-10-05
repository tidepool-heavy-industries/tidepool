//! The tidepool-handlers projection of a single-source effect definition
//! (`tidepool_mcp::<effect>_effect_def!` — see `bridge/mcp/src/effect_defs.rs`
//! for the grammar and the design rationale).
//!
//! Expanding a definition through [`effect_rust_projection!`] generates the
//! whole mechanical Rust half of the effect contract:
//!
//! - `#[derive(FromHaskell)] pub enum <Eff>Req` — one variant per GADT
//!   constructor, named EXACTLY as the Haskell constructor (no
//!   `#[haskell(name)]` rename layer), fields from the definition's Rust arg
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
        // Accepted and ignored here: the Haskell decl projection may route
        // this errors ADT's `data`/`ToJSON` text to a stable committed
        // module instead of inline `Tidepool.Effects` (bridge/mcp/src/
        // effect_defs.rs's `stable_errors true` arm) — the Rust enum is
        // generated from the SAME `errors` block above either way, so this
        // side is unaffected by the flag.
        $(stable_errors $se:tt,)?
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
        // module, so ToHaskell resolves them by qualified name.
        $( crate::effect_glue::error_enum!($errname, $($evariant),*); )?

        #[derive(tidepool_bridge_derive::FromHaskell)]
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

            crate::effect_glue::effect_prepare_method!($eff, $req);
        }
    };
}
pub(crate) use effect_rust_projection;

/// Filesystem reads can block on the host filesystem. Keep the generated
/// request match as the only behavior owner and move that same `handle` call
/// to a blocking worker after capturing its typed request and exact context.
macro_rules! effect_prepare_method {
    (FsRead, $req:ident) => {
        crate::effect_glue::blocking_prepare_method!($req);
    };
    (Git, $req:ident) => {
        crate::effect_glue::blocking_prepare_method!($req);
    };
    (Exec, $req:ident) => {
        crate::effect_glue::blocking_prepare_method!($req);
    };
    (Http, $req:ident) => {
        crate::effect_glue::blocking_prepare_method!($req);
    };
    (KV, $req:ident) => {
        crate::effect_glue::blocking_prepare_method!($req);
    };
    (Llm, $req:ident) => {
        fn prepare(
            &mut self,
            req: $req,
            cx: &tidepool_effect::dispatch::EffectContext<'_, tidepool_mcp::CapturedOutput>,
        ) -> Result<tidepool_effect::dispatch::EffectDispatch, tidepool_effect::error::EffectError>
        {
            let mut handler = self.clone();
            handler.call_count = std::sync::Arc::clone(&self.call_count);
            let table = cx.table().clone();
            let principal = cx.principal();
            let output = cx.user().clone();
            Ok(tidepool_effect::dispatch::EffectDispatch::Deferred(
                tidepool_effect::dispatch::DeferredEffect::blocking(move || {
                    let owned = tidepool_effect::dispatch::EffectContext::with_principal(
                        &table, principal, &output,
                    );
                    tidepool_effect::dispatch::EffectHandler::handle(&mut handler, req, &owned)
                }),
            ))
        }
    };
    ($other:ident, $req:ident) => {};
}
pub(crate) use effect_prepare_method;

macro_rules! blocking_prepare_method {
    ($req:ident) => {
        fn prepare(
            &mut self,
            req: $req,
            cx: &tidepool_effect::dispatch::EffectContext<'_, tidepool_mcp::CapturedOutput>,
        ) -> Result<tidepool_effect::dispatch::EffectDispatch, tidepool_effect::error::EffectError>
        {
            let mut handler = self.clone();
            let table = cx.table().clone();
            let principal = cx.principal();
            let output = cx.user().clone();
            Ok(tidepool_effect::dispatch::EffectDispatch::Deferred(
                tidepool_effect::dispatch::DeferredEffect::blocking(move || {
                    let owned = tidepool_effect::dispatch::EffectContext::with_principal(
                        &table, principal, &output,
                    );
                    tidepool_effect::dispatch::EffectHandler::handle(&mut handler, req, &owned)
                }),
            ))
        }
    };
}
pub(crate) use blocking_prepare_method;

/// Emit the whole `errors` ADT as one item (a macro can't sit in enum-variant
/// position, so the variants are built inline here from the re-matched block).
/// `Debug` lets `Display`/`fs_err_to_effect` render it when an untagged method
/// forwards a shared-helper failure. ToHaskell/FromHaskell use plain name+arity lookup
/// (the variant names are unique), matching the bridged records — no module
/// qualifier (the generated `data` decl's constructors are not registered under
/// a `Module.Ctor` qualified name).
macro_rules! error_enum {
    ( $errname:ident, $({ ctor $c:ident,
                          fields { $($efn:ident : $efh:literal as $efr:ty),* $(,)? },
                          doc $d:literal $(,)? }),* $(,)? ) => {
        // FromHaskell is for test-side decoding of a `Left err`; the error is only
        // ever SENT (ToHaskell) in production. Debug backs the `Display` path;
        // PartialEq/Eq let handler tests assert on decoded `Left` payloads.
        #[derive(
            tidepool_bridge_derive::ToHaskell,
            tidepool_bridge_derive::FromHaskell,
            Debug,
            PartialEq,
            Eq
        )]
        pub enum $errname {
            $( $c( $($efr),* ) ),*
        }
        #[cfg(test)]
        impl $errname {
            pub(crate) const TEST_CONSTRUCTORS: &'static [(&'static str, u32)] = &[
                $( (stringify!($c), (&[$(stringify!($efn)),*] as &[&str]).len() as u32) ),*
            ];
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

/// An incoming aeson-`HaskellValue` GADT argument (e.g. `HttpPost`'s body,
/// `LlmStructured`'s schema), pre-converted to `serde_json::Value`.
///
/// An `errors`-tagged verb's method receives no `cx` (see [`dispatch_body!`]),
/// so it has no `DataConTable` to interpret a materialized Haskell `HaskellValue` — the table
/// lookup has to happen at Req-decode time instead, while `cx` (and so the
/// table) is still in scope. `FromHaskell` for a LOCAL wrapper type is exactly
/// that decode-time hook: `tidepool_bridge_derive`'s enum derive calls
/// `<$ar as FromHaskell>::from_value(&fields[i], table)` for every GADT arg
/// (`$ar` here is `JsonArg`), so the conversion rides the SAME table the
/// dispatch already has, before the tagged method ever runs.
pub struct JsonArg(pub serde_json::Value);

impl tidepool_bridge::sealed::FromHaskellSealed for JsonArg {}

impl tidepool_bridge::FromHaskell for JsonArg {
    fn from_value(
        value: &tidepool_bridge::HaskellValue,
        table: &tidepool_repr::DataConTable,
    ) -> Result<Self, tidepool_bridge::BridgeError> {
        Ok(JsonArg(tidepool_runtime::value_to_json(value, table, 0)))
    }
}
