//! The `Ask` suspension — decode-only.
//!
//! `ask schema prompt` (structured operator elicitation) — the fallback
//! [`crate::schema::HandlingClass::Ask`] routing, and also the shape a
//! malformed `AskUserWith` degrades to (handled at the harness plane, not
//! here — see `tidepool-harness::engine::classify_hole`'s doc).
//!
//! Hand-carried Haskell decl: `tidepool-mcp/src/effect_defs.rs`'s `AskWith`
//! verb. NOT in [`crate::effects::all`] — see
//! [`crate::effects::suspension_roster`].
//!
//! **Why this effect stays hand-carried while Fork/Finalize/RunLLMTurn/Green
//! (the rest of #20's deferred five) all flipped.** Those four shared one
//! representable shape — OPAQUE call-forwarding to a `*Sited` sibling,
//! `unsafeCoerce`-marshaling a `send` result — which
//! [`crate::schema::HelperBody::OpaqueForward`]/`OpaqueSited` now express as
//! reviewed, bounded data. `Ask`'s own helpers are a DIFFERENT kind of thing
//! entirely: `ask` builds its payload by calling `schemaToValue` (not a bare
//! `send (Ctor …)`), and `isOpt`/`innerSchema`/`schemaToValue` are ordinary
//! multi-equation pure Haskell functions recursing over the `Schema` sum —
//! no verb, no `send`, no site id, no OPAQUE pragma. They are exactly the
//! case `crate::schema::Helper`'s own doc names: "a helper that is neither
//! [a verb-wrapper] [nor a projection] is not representable, and stays
//! hand-written OUTSIDE the contract until its lane makes it a deliberate
//! schema feature." Modeling arbitrary pattern-matching function bodies as
//! schema data would mean a general-purpose "Haskell expression as data"
//! mechanism — not a bounded extension sized to a handful of real uses, and
//! exactly the raw-hatch-by-another-name this schema's no-raw-hatch rule
//! exists to refuse.
//!
//! **This is the concrete motivating case for the parked #24 one-home rule**
//! (stdlib-vs-generator ownership, `plans/README.md`'s carried-forward
//! list): `isOpt`/`innerSchema`/`schemaToValue` are STDLIB-shaped code (pure
//! functions over a schema type), not DECL-shaped code (a thin verb
//! surface) — the likely #24 resolution is that helpers like these migrate
//! to `haskell/lib` (imported via the preamble, the same relocation
//! Worktree's/RepoEvent's own non-representable helpers already took — see
//! `worktree.rs`/`event.rs`'s module docs) and this effect's decl block
//! shrinks to `ask`'s own thin verb wrapper, rather than the generator ever
//! learning to express arbitrary function bodies. Not decided or started
//! here — #24 is still parked, this module is just its waiting example.

use crate::hs::HsType;
use crate::schema::{Arg, Effect, HandlingClass, Polymorphism, RustBinding, Verb};

/// The `Ask` suspension, decode-only.
#[must_use]
pub fn ask() -> Effect {
    Effect {
        name: "Ask",
        handler: "AskHandler",
        handler_module: "ask",
        req_enum: "AskReq",
        decl_fn: "ask_decl",
        description: &["Suspend for a raw structured operator elicitation (decode-only schema)."],
        prompt_card: None,
        type_params: &[],
        default_row_args: &[],
        helpers_row_polymorphic: true,
        extra_imports: &[],
        type_defs: Vec::new(),
        foreign_types: &[],
        errors: None,
        verbs: vec![Verb {
            ctor: "AskWith",
            method: "ask_with",
            args: vec![
                Arg {
                    name: "prompt",
                    ty: HsType::Text,
                    rust: RustBinding::Derived,
                },
                Arg {
                    name: "payload",
                    ty: HsType::Value,
                    rust: RustBinding::CoreValue,
                },
            ],
            ret: HsType::Unit,
            errors: None,
            handling: HandlingClass::Ask,
            extract: None,
        }],
        helpers: Vec::new(),
        polymorphism: Polymorphism::None,
        // No real `tidepool-handlers` handler (harness/server machinery
        // services it directly — see the module doc), but its real helper
        // surface (`ask`/`isOpt`/`innerSchema`/`schemaToValue`, real Haskell
        // logic, not thin `send` wrappers) is not yet representable by
        // `HelperBody`'s reviewed shapes; deferred alongside Fork/RunLlmTurn/
        // Finalize/Green rather than flipped with a wrong or raw-hatch
        // rendering.
        dispatched: false,
    }
}
