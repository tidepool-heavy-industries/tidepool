//! The `RunLLMTurn` suspension — decode-only (PRD 22 step 1).
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
//! Hand-carried Haskell decl: `tidepool-mcp/src/effect_defs.rs`'s
//! `RunLLMTurnWith` verb (`runllmturn_effect_def!`). NOT in
//! [`crate::effects::all`] — see [`crate::effects::suspension_roster`].

use crate::hs::HsType;
use crate::schema::{Arg, Effect, HandlingClass, RustBinding, Verb};

/// The `RunLLMTurn` suspension, decode-only.
#[must_use]
pub fn run_llm_turn() -> Effect {
    Effect {
        // "RunLlmTurn", not "RunLLMTurn": `snake_case` treats every uppercase
        // letter as a word boundary, and consecutive capitals (`LLM`) would
        // render as `run_l_l_m_turn` — matching `HandlingClass::RunLlmTurn`'s
        // casing here keeps the generated module name `run_llm_turn`. The
        // Haskell-facing spelling is untouched: it lives only in `ctor` below.
        name: "RunLlmTurn",
        handler: "RunLLMTurnHandler",
        handler_module: "run_llm_turn",
        req_enum: "RunLLMTurnReq",
        decl_fn: "run_llm_turn_decl",
        description: &["Suspend the answerer turn for a typed answer (decode-only schema)."],
        prompt_card: None,
        type_params: &[],
        default_row_args: &[],
        helpers_row_polymorphic: true,
        extra_imports: &[],
        type_defs: Vec::new(),
        foreign_types: &[],
        errors: None,
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
            ret: HsType::Unit,
            errors: None,
            handling: HandlingClass::RunLlmTurn,
            extract: None,
        }],
        helpers: Vec::new(),
    }
}
