//! The `Finalize` suspension — decode-only.
//!
//! `finalize @T x` (`Tidepool.Agent`) hands a typed value up to the parent
//! `runLLMTurn` hole and terminates the answerer's own turn loop. The `value`
//! field crosses IN-HEAP and may carry a non-serializable payload (a
//! closure) — it is never JSON-decoded, here or anywhere in
//! `tidepool-harness`; only the leading `Int` site id is read. `value`'s
//! Rust binding is [`crate::schema::RustBinding::CoreValue`] for exactly that
//! reason: identity capture, no interpretation.
//!
//! Hand-carried Haskell decl: `tidepool-mcp/src/effect_defs.rs`'s
//! `FinalizeWith` verb. NOT in [`crate::effects::all`] — see
//! [`crate::effects::suspension_roster`].

use crate::hs::HsType;
use crate::schema::{Arg, Effect, HandlingClass, Polymorphism, RustBinding, Verb};

/// The `Finalize` suspension, decode-only.
#[must_use]
pub fn finalize() -> Effect {
    Effect {
        name: "Finalize",
        handler: "FinalizeHandler",
        handler_module: "finalize",
        req_enum: "FinalizeReq",
        decl_fn: "finalize_decl",
        description: &["Terminate the answerer turn with a typed value (decode-only schema)."],
        prompt_card: None,
        // `v` is a real, applied GADT parameter (`data Finalize v a where`) —
        // the ROW ENTRY (`Finalize <T>`) is what `Member (Finalize v) effs`
        // pins per-compile, exactly like `State s`. `Void` (canonical
        // `Data.Void`, imported into the generated Core module — not a
        // bespoke `data` decl here) is the default for a turn not answering a
        // typed hole: uninhabited, so such a turn simply cannot finalize.
        type_params: &["v"],
        default_row_args: &["Void"],
        helpers_row_polymorphic: true,
        extra_imports: &[],
        type_defs: Vec::new(),
        foreign_types: &[],
        errors: None,
        verbs: vec![Verb {
            ctor: "FinalizeWith",
            method: "finalize_with",
            args: vec![
                Arg {
                    name: "site",
                    ty: HsType::Int,
                    rust: RustBinding::Derived,
                },
                Arg {
                    name: "value",
                    ty: HsType::Var("v"),
                    rust: RustBinding::CoreValue,
                },
            ],
            // `a` is finalize's own "return type" — genuinely free, never
            // actually returned (the send diverges via suspension), left
            // INDEPENDENT of `v` on purpose (see `tidepool-harness/CLAUDE.md`'s
            // "The answer contract" section). Matches the hand-written
            // `FinalizeWith`'s `ret "a"` exactly.
            ret: HsType::Var("a"),
            errors: None,
            handling: HandlingClass::Finalize,
            extract: None,
        }],
        helpers: Vec::new(),
        // The one effect in this migration whose row itself is the
        // invocation-site binding: `finalize @T x` type-checks because the
        // ROW was built with `Finalize T`, not because of anything
        // verb-local. See `Polymorphism::ArgBound`'s own doc.
        polymorphism: Polymorphism::ArgBound { tyvar: "v" },
        // No real `tidepool-handlers` handler (see the module doc); deferred
        // — `finalize`/`finalizeSited`'s OPAQUE two-tyvar helper text is not
        // yet representable by `HelperBody`'s reviewed shapes.
        dispatched: false,
    }
}
