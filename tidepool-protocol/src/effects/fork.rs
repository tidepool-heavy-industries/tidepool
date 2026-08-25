//! The `Fork` suspension — decode-only.
//!
//! `Tidepool.Fork`'s `fork @T brief` / `forkAll @T briefs`. Polymorphic
//! response type, bound at the invocation site — out of scope for the decl
//! side here (see the PRD); the wire shape decoded is monomorphic.
//!
//! Hand-carried Haskell decl: `tidepool-mcp/src/effect_defs.rs`'s `ForkWith`/
//! `ForkAllWith` verbs. NOT in [`crate::effects::all`] — see
//! [`crate::effects::suspension_roster`].

use crate::hs::HsType;
use crate::schema::{Arg, Effect, HandlingClass, Polymorphism, RustBinding, Verb};

fn site_arg() -> Arg {
    Arg {
        name: "site",
        ty: HsType::Int,
        rust: RustBinding::Derived,
    }
}

/// The `Fork` suspension, decode-only.
#[must_use]
pub fn fork() -> Effect {
    Effect {
        name: "Fork",
        handler: "ForkHandler",
        handler_module: "fork",
        req_enum: "ForkReq",
        decl_fn: "fork_decl",
        description: &["Suspend for a fanned-out typed answer (decode-only schema)."],
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
                ctor: "ForkWith",
                method: "fork_with",
                args: vec![
                    site_arg(),
                    Arg {
                        name: "brief",
                        ty: HsType::Text,
                        rust: RustBinding::Derived,
                    },
                ],
                ret: HsType::Value,
                errors: None,
                handling: HandlingClass::Fork,
                extract: None,
            },
            Verb {
                ctor: "ForkAllWith",
                method: "fork_all_with",
                args: vec![
                    site_arg(),
                    Arg {
                        name: "prompts",
                        ty: HsType::list(HsType::Text),
                        rust: RustBinding::Derived,
                    },
                ],
                ret: HsType::Value,
                errors: None,
                handling: HandlingClass::Fork,
                extract: None,
            },
        ],
        helpers: Vec::new(),
        // Both constructors return concrete `Value` — the `fork @T`/`forkAll
        // @T` polymorphism lives entirely in `forkSited`/`forkAllSited`'s
        // `unsafeCoerce`-marshaling signatures (`Tidepool.Effects`, not this
        // GADT), which are not yet representable by `HelperBody`'s reviewed
        // shapes (OPAQUE pragma + `Member`-polymorphic Sited delegation).
        // `None` here is correct, not a placeholder: nothing at the GADT/row
        // level binds at an invocation site.
        polymorphism: Polymorphism::None,
        // No real `tidepool-handlers` handler (see the module doc); deferred
        // for the reason above (helpers, not the GADT/verbs, are the gap).
        dispatched: false,
    }
}
