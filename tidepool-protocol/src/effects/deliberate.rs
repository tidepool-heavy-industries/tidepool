//! The actor-local typed deliberation suspension.
//!
//! Authored Haskell uses `Tidepool.Deliberation.deliberate`. The request keeps
//! the authoritative input as its field-1 live payload and carries only prompt
//! and type descriptions on the bridged data plane. The actor executor mounts
//! that payload, obtains a GHC-checked result, and resumes the exact parked
//! continuation; no JSON answer or Rust-side effect-row ABI is involved.

use crate::hs::HsType;
use crate::schema::{Arg, Effect, HandlingClass, Polymorphism, RustBinding, Verb};

/// The `Deliberate` effect's generated GADT and actor-runtime decoder.
#[must_use]
pub fn deliberate() -> Effect {
    Effect {
        name: "Deliberate",
        authored_surface: crate::schema::AuthoredSurface::OPAQUE,
        handler: "DeliberateHandler",
        handler_module: "deliberate",
        req_enum: "DeliberateReq",
        decl_fn: "deliberate_decl",
        description: &[
            "Ask this actor's resident model to produce a statically typed Haskell value. ",
            "Use `Tidepool.Deliberation.deliberate`; the raw request constructor is runtime ",
            "substrate.",
        ],
        prompt_card: Some(&[
            "`deliberate goal input` — run the actor's persistent Haskell workbench and ",
            "resume with a GHC-checked result.",
        ]),
        type_params: &[],
        default_row_args: &[],
        helpers_row_polymorphic: true,
        extra_imports: &["import Tidepool.Deliberation"],
        type_defs: Vec::new(),
        foreign_types: &[],
        errors: None,
        verbs: vec![Verb {
            ctor: "DeliberateWith",
            method: "deliberate_with",
            args: vec![
                Arg {
                    name: "site",
                    ty: HsType::Int,
                    rust: RustBinding::Derived,
                },
                Arg {
                    name: "input",
                    ty: HsType::Var("input"),
                    rust: RustBinding::CoreValue,
                },
                Arg {
                    name: "task",
                    ty: HsType::Text,
                    rust: RustBinding::Derived,
                },
            ],
            ret: HsType::Var("output"),
            errors: None,
            handling: HandlingClass::Deliberate,
            extract: None,
        }],
        helpers: Vec::new(),
        // `output` is fixed by the caller's continuation. `input` is
        // existential and crosses only as the separately rooted live field.
        polymorphism: Polymorphism::ResultBound { tyvar: "output" },
        dispatched: false,
    }
}
