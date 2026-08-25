//! The Journal effect — the first effect migrated in this schema's second
//! phase.
//!
//! Chosen second because it is the smallest live effect that is NOT Exec: one
//! verb, no error ADT, opt-in row placement (not in `build_base_stack`), and
//! freshly documented semantics (`haskell/lib/Tidepool/Journal.hs`'s haddock).
//!
//! It is also the first migrated effect to carry a `Value` argument bound as
//! `RustBinding::JsonValue` (`payload`), and the first with no error ADT at
//! all — both slots the schema already declared for this purpose
//! (`RustBinding::JsonValue`, `Effect::errors: Option<ErrorAdt>`), so nothing
//! new was needed to describe it.

use crate::hs::HsType;
use crate::schema::{
    Arg, Effect, HandlingClass, Helper, HelperBody, OuterEffect, Polymorphism, RustBinding, Verb,
};

/// The Journal effect, completely.
#[must_use]
pub fn journal() -> Effect {
    Effect {
        name: "Journal",
        handler: "JournalHandler",
        handler_module: "journal",
        req_enum: "JournalReq",
        decl_fn: "journal_decl",
        description: &["Durable append-only run journal: a resident harness records completed \
            steps as it happens, mid-loop, so progress survives a crash and resume \
            can fold the journal instead of redoing finished work. `record kind key \
            payload` appends ONE entry — `kind` and `key` are caller-chosen labels \
            (e.g. a step kind and the branch or task it concerns), `payload` is an \
            opaque JSON value. Every append is flushed immediately; the journal is \
            append-only forever — there is no rewrite or compaction verb. \
            `trace stage key payload` appends ONE observability entry to the run's \
            sibling TRACE stream instead: never folded into resume, timestamped at \
            the handler, for decision narration and telemetry a reader merges into \
            one timeline by ts."],
        prompt_card: None,
        type_params: &[],
        default_row_args: &[],
        helpers_row_polymorphic: true,
        // The READ half of the run journal. `record` stays write-only —
        // `Tidepool.Resume` reads nothing; it is the type of the
        // already-folded value the driver injects at boot.
        extra_imports: &["import qualified Tidepool.Resume as Resume"],
        type_defs: Vec::new(),
        foreign_types: &[],
        errors: None,
        verbs: vec![Verb {
            ctor: "RecordStep",
            method: "record_step",
            args: vec![
                Arg {
                    name: "kind",
                    ty: HsType::Text,
                    rust: RustBinding::Derived,
                },
                Arg {
                    name: "key",
                    ty: HsType::Text,
                    rust: RustBinding::Derived,
                },
                Arg {
                    name: "payload",
                    ty: HsType::Value,
                    rust: RustBinding::JsonValue,
                },
            ],
            ret: HsType::Unit,
            errors: None,
            handling: HandlingClass::OuterDispatch(OuterEffect::Journal),
            extract: None,
        },
        Verb {
            ctor: "TraceStep",
            method: "trace_step",
            args: vec![
                Arg {
                    name: "stage",
                    ty: HsType::Text,
                    rust: RustBinding::Derived,
                },
                Arg {
                    name: "key",
                    ty: HsType::Text,
                    rust: RustBinding::Derived,
                },
                Arg {
                    name: "payload",
                    ty: HsType::Value,
                    rust: RustBinding::JsonValue,
                },
            ],
            ret: HsType::Unit,
            errors: None,
            handling: HandlingClass::OuterDispatch(OuterEffect::Journal),
            extract: None,
        }],
        helpers: vec![Helper {
            name: "record",
            ctor: Some("RecordStep"),
            substrate: false,
            doc: &[
                "Append one durable journal entry. `kind` and `key` are",
                "caller-chosen labels; `payload` is an opaque JSON value. Flushed",
                "immediately; append-only — never rewritten or compacted.",
            ],
            body: HelperBody::Applied(&["kind", "key", "payload"]),
        },
        Helper {
            name: "trace",
            ctor: Some("TraceStep"),
            substrate: false,
            doc: &["Append one observability entry to the run's sibling TRACE stream: \
                decision narration and telemetry, never folded into resume. `stage` \
                names what kind of moment this is (e.g. \"resume-verdict\", \"park\"); \
                `key` is the branch or unit it concerns; `payload` is an opaque JSON \
                value whose shape may evolve freely. The handler stamps a timestamp \
                on every line, so trace and journal merge into one timeline."],
            body: HelperBody::Applied(&["stage", "key", "payload"]),
        }],
        polymorphism: Polymorphism::None,
        dispatched: true,
    }
}
