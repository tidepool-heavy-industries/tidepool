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
//! `finalize @T x = finalizeSited 0 x` is an OPAQUE call-forwarding stub the
//! extractor head-swaps to `finalizeSited`, substituting a fresh
//! per-call-site literal `Int` for the `0` placeholder (`Translate.hs`,
//! matched by name — same mechanism as `RunLLMTurn`'s family). Unlike every
//! other OPAQUE+Sited pair, `finalizeSited` does NOT `unsafeCoerce`: `value`
//! crosses at its own native representation (`v` itself, never `Value`) the
//! whole way, so there is nothing to relabel.
//!
//! No real `tidepool-handlers` handler (harness-serviced only, same
//! convention as `AskUser`/`ReadState`) — only `decl_rs`/`harness_req_rs`
//! consume this definition.

use crate::hs::HsType;
use crate::schema::{
    Arg, Effect, HandlingClass, Helper, HelperBody, Polymorphism, RustBinding, SitedCtorArg, Verb,
};

/// The `Finalize` suspension, decode-only.
#[must_use]
pub fn finalize() -> Effect {
    Effect {
        name: "Finalize",
        handler: "FinalizeHandler",
        handler_module: "finalize",
        req_enum: "FinalizeReq",
        decl_fn: "finalize_decl",
        description: &[
            "Terminate the current Agent turn loop and hand a typed value UP to ",
            "the parent `runLLMTurn` hole, in-heap (no JSON round-trip — the value ",
            "may be a closure or other non-serializable value). `finalize x` never ",
            "resumes; the harness driver reads the value directly and resolves the ",
            "parent hole via `run_child`.",
        ],
        prompt_card: Some(&[
            "`finalize @T value` — commit the typed answer; this ends your whole ",
            "agent session and delivers `value` to whoever asked (the parent's ",
            "`fork`/`runLLMTurn` call site), in-heap.",
        ]),
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
        helpers: vec![
            Helper {
                name: "finalize",
                ctor: None,
                doc: &[],
                substrate: false,
                body: HelperBody::OpaqueForward {
                    tyvars: &["v", "a"],
                    params: vec![Arg {
                        name: "v",
                        ty: HsType::Var("v"),
                        rust: RustBinding::CoreValue,
                    }],
                    target: "finalizeSited",
                    ret: HsType::Var("a"),
                },
            },
            Helper {
                name: "finalizeSited",
                ctor: Some("FinalizeWith"),
                doc: &[],
                substrate: false,
                body: HelperBody::OpaqueSited {
                    tyvars: &["v", "a"],
                    site_param: "sid",
                    params: vec![Arg {
                        name: "v",
                        ty: HsType::Var("v"),
                        rust: RustBinding::CoreValue,
                    }],
                    ctor_args: vec![SitedCtorArg::Site, SitedCtorArg::Param("v")],
                    coerce: false,
                    ret: HsType::Var("a"),
                },
            },
        ],
        // The one effect in this migration whose row itself is the
        // invocation-site binding: `finalize @T x` type-checks because the
        // ROW was built with `Finalize T`, not because of anything
        // verb-local. See `Polymorphism::ArgBound`'s own doc.
        polymorphism: Polymorphism::ArgBound { tyvar: "v" },
        // No real `tidepool-handlers` handler (see the module doc).
        dispatched: false,
    }
}
