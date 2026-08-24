//! The `Ask` suspension — decode-only (PRD 22 step 1).
//!
//! `ask schema prompt` (structured operator elicitation) — the fallback
//! [`crate::schema::HandlingClass::Ask`] routing, and also the shape a
//! malformed `AskUserWith` degrades to (handled at the harness plane, not
//! here — see `tidepool-harness::engine::classify_hole`'s doc).
//!
//! Hand-carried Haskell decl: `tidepool-mcp/src/effect_defs.rs`'s `AskWith`
//! verb. NOT in [`crate::effects::all`] — see
//! [`crate::effects::suspension_roster`].

use crate::hs::HsType;
use crate::schema::{Arg, Effect, HandlingClass, RustBinding, Verb};

/// The `Ask` suspension, decode-only.
#[must_use]
pub fn ask() -> Effect {
    Effect {
        name: "Ask",
        handler: "AskHandler",
        handler_module: "ask",
        req_enum: "AskReq",
        decl_fn: "ask_decl",
        description: &["Suspend for a raw structured operator elicitation (decode-only schema)."],
        prompt_card: None,
        type_params: &[],
        default_row_args: &[],
        helpers_row_polymorphic: true,
        extra_imports: &[],
        type_defs: Vec::new(),
        foreign_types: &[],
        errors: None,
        verbs: vec![Verb {
            ctor: "AskWith",
            method: "ask_with",
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
            handling: HandlingClass::Ask,
            extract: None,
        }],
        helpers: Vec::new(),
    }
}
