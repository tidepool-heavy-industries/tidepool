//! The `ReadState` suspension — decode-only (PRD 22 step 1).
//!
//! `getStateJson` (answerer row only): a nullary suspension serviced
//! IMMEDIATELY by the driver with the loop's current state JSON — no
//! operator, no model round. Nothing to decode beyond the constructor name.
//!
//! Hand-carried Haskell decl: `tidepool-mcp/src/effect_defs.rs`'s
//! `ReadStateWith` verb. NOT in [`crate::effects::all`] — see
//! [`crate::effects::suspension_roster`].

use crate::hs::HsType;
use crate::schema::{Effect, HandlingClass, Verb};

/// The `ReadState` suspension, decode-only.
#[must_use]
pub fn read_state() -> Effect {
    Effect {
        name: "ReadState",
        handler: "ReadStateHandler",
        handler_module: "read_state",
        req_enum: "ReadStateReq",
        decl_fn: "read_state_decl",
        description: &["Suspend to read the loop's current state (decode-only schema)."],
        prompt_card: None,
        type_params: &[],
        default_row_args: &[],
        helpers_row_polymorphic: true,
        extra_imports: &[],
        type_defs: Vec::new(),
        foreign_types: &[],
        errors: None,
        verbs: vec![Verb {
            ctor: "ReadStateWith",
            method: "read_state_with",
            args: vec![],
            ret: HsType::Unit,
            errors: None,
            handling: HandlingClass::ReadState,
            extract: None,
        }],
        helpers: Vec::new(),
    }
}
