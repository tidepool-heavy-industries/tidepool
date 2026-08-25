//! The `RunLLMTurn` suspension — decode-only.
//!
//! `runLLMTurn`/`runLLMTurnFork`/`runLLMTurnFanout` (`Tidepool.Agent`) all
//! suspend on the SAME single constructor, `RunLLMTurnWith prompt payload`,
//! where `payload` is a `Value` carrying a JSON OBJECT (`typedSite`/`fork`/
//! `fan`/`prompts`) rather than separate positional Core fields — that
//! nested-object shape has no schema vocabulary (and doesn't need one: it is
//! interpreted once, in `tidepool-harness::engine::classify_runllmturn_payload`,
//! which stays hand-written orchestration). This effect's job is only to get
//! the outer `Con`'s two positional fields — `prompt`, `payload` — decoded and
//! the constructor recognized; is-it-a-fork and fan/prompts decode stay in the
//! driver's own match arms, unchanged from before this migration.
//!
//! Polymorphic response type (`@T`), bound at the invocation site: out of
//! scope for this decl-side schema (see `tidepool-protocol/README.md` and the
//! PRD) — the wire shape decoded here is monomorphic (`prompt`, `payload`)
//! regardless of `T`.
//!
//! `runLLMTurn`/`runLLMTurnFork`/`runLLMTurnFanout` are OPAQUE call-forwarding
//! stubs the extractor head-swaps to their own `*Sited` sibling, substituting a
//! fresh per-call-site literal `Int` for the `0` placeholder in the forwarded
//! call (`Translate.hs`, matched by name). Each `*Sited` sibling embeds that
//! site id — plus, for the fork/fanout pair, classification flags — into the
//! `RunLLMTurnWith` payload's JSON object (this GADT has no dedicated site
//! field of its own, unlike `Fork`'s/`Finalize`'s), sends it, and
//! `unsafeCoerce`s the `Value` result back to the caller's answer type — safe
//! because the extractor has already checked the call site's answer type is
//! monomorphic (`checkRunLLMTurnType`): a pure function type is allowed (the
//! model may finalize a `State -> State`), a type mentioning the effect monad
//! is rejected (`typeMentionsEffectMonad`), so the harness always resumes with
//! a value the caller validated against that exact type — the coercion is a
//! same-representation relabeling, never a genuine type change.
//!
//! `InvocationExit` — why a forked child ended WITHOUT a typed answer — rides
//! this effect's `errors` block even though no verb is `errors`-tagged (no
//! verb of THIS GADT returns `Either InvocationExit _`; only the surface
//! `runLLMTurnFork`/`runLLMTurnFanout` HELPERS do): reusing [`ErrorAdt`] is
//! what keeps its `data`/`ToJSON` rendering single-sourced with every other
//! tagged-exit type in this schema rather than a second bespoke renderer for
//! the same tag+field-object shape. Declared divergence from the hand text:
//! the generated form emits `data`+`ToJSON` as ONE combined `type_defs` entry
//! (same convention `errors`-tagged effects already use) rather than two, and
//! without the hand text's own haddock comment / multi-line layout — same
//! Haskell semantics, `TypeDef`/[`ErrorAdt`]'s established single-line
//! rendering convention.
//!
//! No real `tidepool-handlers` handler (harness-serviced only, same
//! convention as `AskUser`/`ReadState`) — only `decl_rs`/`harness_req_rs`
//! consume this definition.

use crate::hs::HsType;
use crate::schema::{
    Arg, Effect, ErrorAdt, ErrorField, ErrorVariant, HandlingClass, Helper, HelperBody,
    ObjectValue, Polymorphism, RustBinding, SitedCtorArg, Verb,
};

/// The `RunLLMTurn` suspension, decode-only.
#[must_use]
pub fn run_llm_turn() -> Effect {
    Effect {
        // The REAL Haskell GADT type name (`data RunLLMTurn a where …`) —
        // `name` is not just a Rust-side label, it is literally what
        // `Effect::head()` renders into every constructor signature and
        // `Member` clause (`constructor_signatures()`, the OPAQUE+Sited
        // helpers' own signatures below). A prior draft spelled this
        // `"RunLlmTurn"` to keep `snake_case`'s derived module name pretty
        // (`run_llm_turn`, not `run_l_l_m_turn` — `snake_case` treats every
        // uppercase letter as a word boundary, and consecutive capitals
        // don't get special treatment) — harmless while this effect stayed
        // hand-carried and nothing rendered `head()` into real Haskell, but
        // wrong now that it does: it would declare a type literally named
        // `RunLlmTurn`, which nothing else in the codebase (the real
        // extractor, `Translate.hs`, any `Member RunLLMTurn` constraint)
        // spells that way. Correctness of the compiled Haskell wins over a
        // pretty derived module/file name — `run_l_l_m_turn` is accepted as
        // the generated module basename (see `tidepool-harness`'s
        // `generated/run_l_l_m_turn.rs`), same convention `handler_module`
        // already uses to carry an explicit override when a derived name
        // isn't the one wanted, just not plumbed as a THIRD field for a
        // single effect. `HandlingClass::RunLlmTurn` (below) is a Rust enum
        // variant identifier, unrelated to this string, and keeps its own
        // casing.
        name: "RunLLMTurn",
        handler: "RunLLMTurnHandler",
        handler_module: "run_llm_turn",
        req_enum: "RunLLMTurnReq",
        // NOT snake_case("RunLlmTurn") ("run_llm_turn_decl") — this is the
        // existing public function name every caller already uses
        // (`tidepool_mcp::runllmturn_decl`, e.g. `tidepool-harness`'s
        // `engine.rs`/`selfharness/driver/*`); the flip must not move it. See
        // `ask_user.rs`'s matching comment.
        decl_fn: "runllmturn_decl",
        description: &[
            "Suspend for a TYPED answer. `runLLMTurn \\@T prompt :: M T` — the same ",
            "calling model answers IN CONTEXT; its failure is this session's failure, ",
            "so the answer is bare. `runLLMTurnFork \\@T prompt :: M (Either ",
            "InvocationExit T)` — a forked sub-agent answers in its own child agent ",
            "session; `runLLMTurnFanout \\@T prompts :: M [Either InvocationExit T]` — ",
            "N forked sub-agents, one per prompt, one result per prompt IN DECLARED ",
            "ORDER. Each forked child's abnormal exit (round exhaustion, ",
            "non-finalization, cancellation, runtime failure) arrives as `Left exit` ",
            "instead of killing its siblings — natural spelling ",
            "`Right x <- runLLMTurnFork \\@T p`, or `renderInvocationExit e` to display ",
            "one. GHC validates each answer against `T` before it resumes the ",
            "continuation (an ill-typed answer never consumes it). ",
            "In every verb above, `T` may be any type in scope, including one you ",
            "declared yourself earlier this session.",
        ],
        prompt_card: None,
        type_params: &[],
        default_row_args: &[],
        helpers_row_polymorphic: true,
        extra_imports: &[],
        type_defs: Vec::new(),
        foreign_types: &[],
        errors: Some(ErrorAdt {
            name: "InvocationExit",
            variants: vec![
                ErrorVariant {
                    ctor: "ExitRoundsExhausted",
                    fields: vec![ErrorField {
                        name: "detail",
                        ty: HsType::Text,
                        rust: RustBinding::Derived,
                    }],
                    doc: "round exhaustion",
                },
                ErrorVariant {
                    ctor: "ExitNotFinalized",
                    fields: vec![ErrorField {
                        name: "detail",
                        ty: HsType::Text,
                        rust: RustBinding::Derived,
                    }],
                    doc: "non-finalization",
                },
                ErrorVariant {
                    ctor: "ExitCancelled",
                    fields: vec![ErrorField {
                        name: "detail",
                        ty: HsType::Text,
                        rust: RustBinding::Derived,
                    }],
                    doc: "cancelled",
                },
                ErrorVariant {
                    ctor: "ExitRuntimeFailure",
                    fields: vec![ErrorField {
                        name: "detail",
                        ty: HsType::Text,
                        rust: RustBinding::Derived,
                    }],
                    doc: "runtime failure",
                },
            ],
        }),
        verbs: vec![Verb {
            ctor: "RunLLMTurnWith",
            method: "run_llm_turn_with",
            args: vec![
                Arg {
                    name: "prompt",
                    ty: HsType::Text,
                    rust: RustBinding::Derived,
                },
                Arg {
                    name: "payload",
                    ty: HsType::Value,
                    rust: RustBinding::CoreValue,
                },
            ],
            ret: HsType::Value,
            errors: None,
            handling: HandlingClass::RunLlmTurn,
            extract: None,
        }],
        helpers: vec![
            Helper {
                name: "runLLMTurn",
                ctor: None,
                doc: &[],
                substrate: false,
                body: HelperBody::OpaqueForward {
                    tyvars: &["a"],
                    params: vec![Arg {
                        name: "prompt",
                        ty: HsType::Text,
                        rust: RustBinding::Derived,
                    }],
                    target: "runLLMTurnSited",
                    ret: HsType::Var("a"),
                },
            },
            Helper {
                name: "runLLMTurnFork",
                ctor: None,
                doc: &[],
                substrate: false,
                body: HelperBody::OpaqueForward {
                    tyvars: &["a"],
                    params: vec![Arg {
                        name: "prompt",
                        ty: HsType::Text,
                        rust: RustBinding::Derived,
                    }],
                    target: "runLLMTurnForkSited",
                    ret: HsType::either(HsType::Named("InvocationExit"), HsType::Var("a")),
                },
            },
            Helper {
                name: "runLLMTurnFanout",
                ctor: None,
                doc: &[],
                substrate: false,
                body: HelperBody::OpaqueForward {
                    tyvars: &["a"],
                    params: vec![Arg {
                        name: "prompts",
                        ty: HsType::list(HsType::Text),
                        rust: RustBinding::Derived,
                    }],
                    target: "runLLMTurnFanoutSited",
                    ret: HsType::list(HsType::either(
                        HsType::Named("InvocationExit"),
                        HsType::Var("a"),
                    )),
                },
            },
            Helper {
                name: "renderInvocationExit",
                ctor: None,
                doc: &[],
                substrate: false,
                body: HelperBody::VariantRender {
                    type_name: "InvocationExit",
                    binder: "d",
                    prefixes: vec![
                        "round exhaustion: ",
                        "non-finalization: ",
                        "cancelled: ",
                        "runtime failure: ",
                    ],
                },
            },
            Helper {
                name: "runLLMTurnSited",
                ctor: Some("RunLLMTurnWith"),
                doc: &[],
                substrate: true,
                body: HelperBody::OpaqueSited {
                    tyvars: &["a"],
                    site_param: "sid",
                    params: vec![Arg {
                        name: "p",
                        ty: HsType::Text,
                        rust: RustBinding::Derived,
                    }],
                    ctor_args: vec![
                        SitedCtorArg::Param("p"),
                        SitedCtorArg::Object(&[("typedSite", ObjectValue::Site)]),
                    ],
                    coerce: true,
                    ret: HsType::Var("a"),
                },
            },
            Helper {
                name: "runLLMTurnForkSited",
                ctor: Some("RunLLMTurnWith"),
                doc: &[],
                substrate: true,
                body: HelperBody::OpaqueSited {
                    tyvars: &["a"],
                    site_param: "sid",
                    params: vec![Arg {
                        name: "p",
                        ty: HsType::Text,
                        rust: RustBinding::Derived,
                    }],
                    ctor_args: vec![
                        SitedCtorArg::Param("p"),
                        SitedCtorArg::Object(&[
                            ("typedSite", ObjectValue::Site),
                            ("fork", ObjectValue::True),
                        ]),
                    ],
                    coerce: true,
                    ret: HsType::either(HsType::Named("InvocationExit"), HsType::Var("a")),
                },
            },
            Helper {
                name: "runLLMTurnFanoutSited",
                ctor: Some("RunLLMTurnWith"),
                doc: &[],
                substrate: true,
                body: HelperBody::OpaqueSited {
                    tyvars: &["a"],
                    site_param: "sid",
                    params: vec![Arg {
                        name: "prompts",
                        ty: HsType::list(HsType::Text),
                        rust: RustBinding::Derived,
                    }],
                    ctor_args: vec![
                        SitedCtorArg::IntercalateNewline("prompts"),
                        SitedCtorArg::Object(&[
                            ("typedSite", ObjectValue::Site),
                            ("fork", ObjectValue::True),
                            ("fan", ObjectValue::LengthOf("prompts")),
                            ("prompts", ObjectValue::Param("prompts")),
                        ]),
                    ],
                    coerce: true,
                    ret: HsType::list(HsType::either(
                        HsType::Named("InvocationExit"),
                        HsType::Var("a"),
                    )),
                },
            },
        ],
        // Same shape as Fork: the constructor returns concrete `Value`, and
        // the `runLLMTurn @T`/`runLLMTurnFork @T`/`runLLMTurnFanout @T`
        // polymorphism lives entirely in the `*Sited` helpers'
        // `unsafeCoerce`-marshaling signatures above, not this GADT — see
        // `fork.rs`'s matching comment.
        polymorphism: Polymorphism::None,
        // No real `tidepool-handlers` handler (see the module doc).
        dispatched: false,
    }
}
