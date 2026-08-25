//! The `ReadState` suspension.
//!
//! `getStateJson` (answerer row only): a nullary suspension serviced
//! IMMEDIATELY by the driver with the loop's current state JSON — no
//! operator, no model round. Nothing to decode beyond the constructor name.
//!
//! No real `tidepool-handlers` handler — harness-serviced only. Its one helper
//! (`getStateJson = send ReadStateWith`) is a thin nullary `send` wrapper, so
//! this effect's whole surface is representable by
//! [`crate::schema::HelperBody`]'s existing reviewed shapes and it is fully
//! migrated: in [`crate::effects::all`], its hand copy deleted from
//! `tidepool-mcp/src/effect_defs.rs`.

use crate::hs::HsType;
use crate::schema::{Effect, HandlingClass, Helper, HelperBody, Polymorphism, Verb};

/// The `ReadState` suspension.
#[must_use]
pub fn read_state() -> Effect {
    Effect {
        name: "ReadState",
        handler: "ReadStateHandler",
        handler_module: "read_state",
        req_enum: "ReadStateReq",
        // NOT snake_case("ReadState") ("read_state_decl") — the existing
        // public function name every caller already uses
        // (`tidepool_mcp::readstate_decl`); the flip must not move it.
        decl_fn: "readstate_decl",
        prompt_card: Some(&[
            "`getStateJson :: M Value` — the loop's durable state as JSON, ",
            "immediately (no operator, no model round), as of this loop iteration's ",
            "START (this iteration's answer and any operator message being ingested ",
            "are not in it yet). Query it with optics, e.g. ",
            "`v ^? key \"question\" . _String`; the shape is whatever the harness's ",
            "State type declares.",
        ]),
        description: &[
            "Read the loop's durable state — the same value your system instructions ",
            "render a SELECTION of — as JSON, immediately. `getStateJson :: M Value` ",
            "returns the state as of this loop iteration's start; the current ",
            "iteration's answer (and any operator message being ingested this ",
            "iteration) are not yet in it. Use optics for ad-hoc queries and compute ",
            "over it with ordinary Haskell.",
        ],
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
            ret: HsType::Value,
            errors: None,
            handling: HandlingClass::ReadState,
            extract: None,
        }],
        helpers: vec![Helper {
            name: "getStateJson",
            ctor: Some("ReadStateWith"),
            substrate: false,
            doc: &[],
            body: HelperBody::Nullary,
        }],
        polymorphism: Polymorphism::None,
        dispatched: false,
    }
}
