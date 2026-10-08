//! The `Ask` suspension — decode-only.
//!
//! `ask schema prompt` is the retained structured operator elicitation boundary.
//!
//! The raw `askRaw` wrapper is supplied by `ask_effect_def!` in
//! `bridge/mcp/src/effect_defs.rs`. This decode-only description is listed in
//! [`crate::effects::suspension_roster`], outside [`crate::effects::all`].
//!
//! `Tidepool.Form.Schema` owns `Schema`, its JSON conversion, and the composed
//! `ask` function. These authored Haskell definitions stay outside the generated
//! effect module, which cannot import authored library code.

use crate::hs::HsType;
use crate::schema::{Arg, Effect, HandlingClass, Polymorphism, RustBinding, Verb};

/// The `Ask` suspension, decode-only.
#[must_use]
pub fn ask() -> Effect {
    Effect {
        name: "Ask",
        authored_surface: crate::schema::AuthoredSurface::All,
        handler: "AskHandler",
        handler_module: "ask",
        req_enum: "AskReq",
        decl_fn: "ask_decl",
        description: &["Suspend for a raw structured operator elicitation (decode-only schema)."],
        prompt_card: None,
        type_params: &[],
        default_row_args: &[],
        extra_imports: &[],
        type_defs: Vec::new(),
        external_types: &[],
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
                    rust: RustBinding::HaskellValue,
                },
            ],
            ret: HsType::Unit,
            errors: None,
            handling: HandlingClass::Ask,
        }],
        helpers: Vec::new(),
        polymorphism: Polymorphism::None,
        // The harness services this suspension directly; there is no
        // tidepool-handlers implementation to generate.
        generated_handler: false,
        handler_execution: crate::schema::HandlerExecution::Immediate,
        caller_principal: false,
    }
}
