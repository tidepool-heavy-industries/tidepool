//! Console output and actor-owned structured display suspensions — decode-only.
//!
//! Console suspends to the actor driver. `Print` carries narrative text;
//! structured display retains an actor-owned continuation and asks its host
//! for an allowance before demanding a page. Expansion input is available only
//! while the host is driving an authorized retained callback.
//!
//! The matched Haskell declaration is generated through
//! `bridge/mcp/src/effect_defs.rs`'s `console_effect_def!` macro. This decode-only
//! schema belongs to [`crate::effects::suspension_roster`].

use crate::hs::HsType;
use crate::schema::{Arg, Effect, HandlingClass, OuterEffect, Polymorphism, RustBinding, Verb};

/// Narrative output and structured display suspensions, decode-only.
#[must_use]
pub fn console() -> Effect {
    Effect {
        name: "Console",
        authored_surface: crate::schema::AuthoredSurface::All,
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
        verbs: vec![
            Verb {
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
            },
            Verb {
                ctor: "DisplayWith",
                method: "display_with",
                args: vec![
                    Arg {
                        name: "view",
                        ty: HsType::Tuple(vec![
                            display_id(),
                            HsType::Text,
                            expansion_keys(),
                            HsType::Bool,
                        ]),
                        rust: RustBinding::Path(
                            "((i64, i64, i64), String, Vec<(i64, String)>, bool)",
                        ),
                    },
                    Arg {
                        name: "continuation",
                        ty: HsType::Var("payload"),
                        rust: RustBinding::HaskellValue,
                    },
                ],
                ret: display_id(),
                errors: None,
                handling: HandlingClass::OuterDispatch(OuterEffect::Console),
            },
            Verb {
                ctor: "DisplayExpandWith",
                method: "display_expand_with",
                args: vec![Arg {
                    name: "selection",
                    ty: HsType::Tuple(vec![display_id(), HsType::Int]),
                    rust: RustBinding::Path("((i64, i64, i64), i64)"),
                }],
                ret: expansion_keys(),
                errors: None,
                handling: HandlingClass::OuterDispatch(OuterEffect::Console),
            },
            Verb {
                ctor: "DisplayAllowanceWith",
                method: "display_allowance_with",
                args: vec![],
                ret: HsType::Int,
                errors: None,
                handling: HandlingClass::OuterDispatch(OuterEffect::Console),
            },
            Verb {
                ctor: "DisplayExpansionInputWith",
                method: "display_expansion_input_with",
                args: vec![],
                ret: HsType::Tuple(vec![display_id(), HsType::Int, HsType::Int]),
                errors: None,
                handling: HandlingClass::OuterDispatch(OuterEffect::Console),
            },
        ],
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
        generated_handler: true,
        handler_execution: crate::schema::HandlerExecution::Immediate,
        caller_principal: false,
    }
}

fn display_id() -> HsType {
    HsType::Tuple(vec![HsType::Int, HsType::Int, HsType::Int])
}

fn expansion_keys() -> HsType {
    HsType::list(HsType::Tuple(vec![HsType::Int, HsType::Text]))
}
