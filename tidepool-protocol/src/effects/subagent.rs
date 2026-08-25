//! The `Subagent` suspension — decode-only.
//!
//! Six verbs, one saga (`tidepool-handlers`'s `SubagentHandler`). Routed by
//! CONSTRUCTOR NAME only — `tidepool-harness::engine::classify_hole` never
//! decodes a Subagent verb's payload; the args here exist ONLY so this
//! effect's request enum matches the real wire ARITY (a `FromCore` decode
//! matches a `Con` by name+arity, so a wrong arity here would make a
//! legitimate `SubagentSpawn` request fail to classify). Every payload field
//! is bound as [`crate::schema::RustBinding::CoreValue`]/`Derived` rather than
//! the real bridged types (`AgSpawnSpec`, `AgAgentId`, `AgCycleId`, …):
//! classify_hole's job is recognition, not interpretation — the real decode,
//! against the real bridged types, happens once, at the servicing site
//! (`tidepool-handlers`'s own generated `SubagentReq`), which this schema
//! does not duplicate.
//!
//! Arities transcribed from `tidepool-mcp/src/effect_defs.rs`'s
//! `subagent_effect_def!` verbs list (`SubagentSpawn`/`SubagentBegin`/
//! `SubagentResume`/`SubagentSpawnAsync`/`SubagentAwait`/`SubagentCancel`).
//! NOT in [`crate::effects::all`] — see [`crate::effects::suspension_roster`].

use crate::hs::HsType;
use crate::schema::{Arg, Effect, HandlingClass, Polymorphism, RustBinding, Verb};

fn value_arg(name: &'static str) -> Arg {
    Arg {
        name,
        ty: HsType::Value,
        rust: RustBinding::CoreValue,
    }
}

/// The `Subagent` suspension (all six verbs), decode-only.
#[must_use]
pub fn subagent() -> Effect {
    Effect {
        name: "Subagent",
        handler: "SubagentDecodeHandler",
        handler_module: "subagent",
        req_enum: "SubagentReq",
        decl_fn: "subagent_decl",
        description: &["Suspend to the driver's subagent service (decode-only schema)."],
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
                ctor: "SubagentSpawn",
                method: "subagent_spawn",
                args: vec![value_arg("spec"), value_arg("schema")],
                ret: HsType::Unit,
                errors: None,
                handling: HandlingClass::Subagent,
                extract: None,
            },
            Verb {
                ctor: "SubagentBegin",
                method: "subagent_begin",
                args: vec![value_arg("spec"), value_arg("tools"), value_arg("schema")],
                ret: HsType::Unit,
                errors: None,
                handling: HandlingClass::Subagent,
                extract: None,
            },
            Verb {
                ctor: "SubagentResume",
                method: "subagent_resume",
                args: vec![
                    value_arg("agent"),
                    Arg {
                        name: "call",
                        ty: HsType::Text,
                        rust: RustBinding::Derived,
                    },
                    Arg {
                        name: "ok",
                        ty: HsType::Bool,
                        rust: RustBinding::Derived,
                    },
                    value_arg("body"),
                ],
                ret: HsType::Unit,
                errors: None,
                handling: HandlingClass::Subagent,
                extract: None,
            },
            Verb {
                ctor: "SubagentSpawnAsync",
                method: "subagent_spawn_async",
                args: vec![value_arg("spec"), value_arg("schema")],
                ret: HsType::Unit,
                errors: None,
                handling: HandlingClass::Subagent,
                extract: None,
            },
            Verb {
                ctor: "SubagentAwait",
                method: "subagent_await",
                args: vec![value_arg("cycle")],
                ret: HsType::Unit,
                errors: None,
                handling: HandlingClass::Subagent,
                extract: None,
            },
            Verb {
                ctor: "SubagentCancel",
                method: "subagent_cancel",
                args: vec![value_arg("cycle")],
                ret: HsType::Unit,
                errors: None,
                handling: HandlingClass::Subagent,
                extract: None,
            },
        ],
        helpers: Vec::new(),
        polymorphism: Polymorphism::None,
        // Same double-duty shape as Console (see that module's `dispatched`
        // doc): the hand macro also feeds `tidepool-handlers`'s real
        // `SubagentHandler` projection, out of this migration's scope.
        dispatched: true,
    }
}
