//! The `Console` suspension — decode-only.
//!
//! Only `Print` suspends through `classify_hole` (the authored outer loop's
//! `say`). Recognition only: `classify_hole` tags the constructor and moves
//! on — `Print`'s text is not read there, only at the servicing site
//! (`SelfHarnessDriver::service_outer_effect`'s Console arm, which additionally
//! posts it to the operator feed).
//!
//! Hand-carried Haskell decl: `tidepool-mcp/src/effect_defs.rs`'s `Print`
//! verb (`console_effect_def!`). NOT in [`crate::effects::all`] — see
//! [`crate::effects::suspension_roster`].

use crate::hs::HsType;
use crate::schema::{Arg, Effect, HandlingClass, OuterEffect, Polymorphism, RustBinding, Verb};

/// The `Console` suspension (`Print` only), decode-only.
#[must_use]
pub fn console() -> Effect {
    Effect {
        name: "Console",
        handler: "ConsoleDecodeHandler",
        handler_module: "console",
        req_enum: "ConsoleReq",
        decl_fn: "console_decl",
        description: &["Suspend to the driver's console/operator feed (decode-only schema)."],
        prompt_card: None,
        type_params: &[],
        default_row_args: &[],
        helpers_row_polymorphic: true,
        extra_imports: &[],
        type_defs: Vec::new(),
        foreign_types: &[],
        errors: None,
        verbs: vec![Verb {
            ctor: "Print",
            method: "print",
            args: vec![Arg {
                name: "msg",
                ty: HsType::Text,
                rust: RustBinding::Derived,
            }],
            ret: HsType::Unit,
            errors: None,
            handling: HandlingClass::OuterDispatch(OuterEffect::Console),
            extract: None,
        }],
        helpers: Vec::new(),
        polymorphism: Polymorphism::None,
        // Console has a REAL `tidepool-handlers::ConsoleHandler` — the hand
        // macro (`console_effect_def!`) still feeds BOTH the decl side
        // (`effect_decl_projection!`, here) and the handler side
        // (`effect_rust_projection!`, in `tidepool-handlers`), so this schema
        // entry cannot flip on its own without also touching
        // `tidepool-handlers` (out of scope for this migration — see
        // `suspension_roster`'s doc). `true` documents the real shape even
        // though this Effect stays out of `effects::all()` for now.
        dispatched: true,
    }
}
