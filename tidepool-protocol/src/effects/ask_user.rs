//! The `AskUser` suspension — decode-only.
//!
//! Two constructors ride this one GADT: `AskUserWith spec` (a typed form,
//! routed to the human operator) and `NoteWith text` (a non-blocking display
//! line on the same GADT, sibling constructor). Both decode-only here: `spec`
//! is a `Value` whose JSON is deserialized into a `FormShape` by
//! `tidepool-harness::selfharness::operator` — untouched by this schema,
//! which only recognizes the constructor and hands back the raw payload.
//!
//! Hand-carried Haskell decl: `tidepool-mcp/src/effect_defs.rs`'s
//! `AskUserWith`/`NoteWith` verbs. NOT in [`crate::effects::all`] — see
//! [`crate::effects::suspension_roster`].

use crate::hs::HsType;
use crate::schema::{Arg, Effect, HandlingClass, RustBinding, Verb};

/// The `AskUser` suspension (both its constructors), decode-only.
#[must_use]
pub fn ask_user() -> Effect {
    Effect {
        name: "AskUser",
        handler: "AskUserHandler",
        handler_module: "ask_user",
        req_enum: "AskUserReq",
        decl_fn: "ask_user_decl",
        description: &["Suspend for operator input or a non-blocking note (decode-only schema)."],
        prompt_card: None,
        type_params: &[],
        default_row_args: &[],
        helpers_row_polymorphic: true,
        extra_imports: &[],
        type_defs: Vec::new(),
        foreign_types: &[],
        errors: None,
        verbs: vec![
            Verb {
                ctor: "AskUserWith",
                method: "ask_user_with",
                args: vec![Arg {
                    name: "spec",
                    ty: HsType::Value,
                    rust: RustBinding::CoreValue,
                }],
                ret: HsType::Unit,
                errors: None,
                handling: HandlingClass::AskUserForm,
                extract: None,
            },
            Verb {
                ctor: "NoteWith",
                method: "note_with",
                args: vec![Arg {
                    name: "text",
                    ty: HsType::Text,
                    rust: RustBinding::Derived,
                }],
                ret: HsType::Unit,
                errors: None,
                handling: HandlingClass::Note,
                extract: None,
            },
        ],
        helpers: Vec::new(),
    }
}
