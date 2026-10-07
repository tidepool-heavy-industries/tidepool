//! Console output, rich views and actor-owned bounded inspection.
//!
//! Console suspends to the actor driver. `Print` carries narrative text;
//! structured display retains an actor-owned continuation and asks its host
//! for an allowance before demanding a page. Expansion input is available only
//! while the host is driving an authorized retained callback.
//!
//! All Haskell declarations, handler codecs and actor decoders use this owner.

use crate::hs::HsType;
use crate::schema::{
    Arg, Effect, HandlingClass, Helper, HelperBody, OuterEffect, Polymorphism, RustBinding, Verb,
};

/// Narrative output and structured display suspensions, decode-only.
#[must_use]
pub fn console() -> Effect {
    Effect {
        name: "Console",
        authored_surface: crate::schema::AuthoredSurface::All,
        handler: "ConsoleHandler",
        handler_module: "console",
        req_enum: "ConsoleReq",
        decl_fn: "console_decl",
        description: &["Suspend to the driver's console/operator feed (decode-only schema)."],
        prompt_card: None,
        type_params: &[],
        default_row_args: &[],
        extra_imports: &[],
        type_defs: Vec::new(),
        external_types: &[],
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
                ctor: "DisplayViewWith",
                method: "display_view_with",
                args: vec![Arg {
                    name: "view",
                    ty: HsType::Value,
                    rust: RustBinding::JsonValue,
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
        helpers: vec![
            Helper {
                name: "say",
                ctor: Some("Print"),
                substrate: false,
                doc: &["Emit a line of console output."],
                body: HelperBody::Pointfree,
            },
            Helper {
                name: "displayViewRaw",
                ctor: Some("DisplayViewWith"),
                substrate: true,
                doc: &[],
                body: HelperBody::Applied(&["view"]),
            },
        ],
        polymorphism: Polymorphism::None,
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
