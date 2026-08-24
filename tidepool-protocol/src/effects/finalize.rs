//! The `Finalize` suspension — decode-only (PRD 22 step 1).
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
use crate::schema::{Arg, Effect, HandlingClass, RustBinding, Verb};

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
        type_params: &[],
        default_row_args: &[],
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
            ret: HsType::Unit,
            errors: None,
            handling: HandlingClass::Finalize,
            extract: None,
        }],
        helpers: Vec::new(),
    }
}
