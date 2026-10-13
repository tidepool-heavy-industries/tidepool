//! Source inspection and coherent configuration reload belong to the actor owner.
//!
//! A request reads its issued immutable generation. Reload prepares edited source,
//! compatible handlers and workspace prompts together, then publishes a fresh
//! notebook generation. Existing requests retain their original environment.

use crate::hs::HsType;
use crate::schema::{
    Arg, Effect, ErrorAdt, ErrorField, ErrorVariant, HandlingClass, Helper, HelperBody,
    Polymorphism, RecordField, RustBinding, SumVariant, TypeDef, TypeShape, Verb, WireDerive,
    WireDerives,
};
use crate::types::JsonInstance;

const WIRE: WireDerives = WireDerives(&[
    WireDerive::ToHaskell,
    WireDerive::Clone,
    WireDerive::Debug,
    WireDerive::PartialEq,
    WireDerive::Eq,
]);

/// The Source effect, completely.
#[must_use]
pub fn source() -> Effect {
    Effect {
        name: "Source",
        authored_surface: crate::schema::AuthoredSurface::All,
        handler: "SourceHandler",
        handler_module: "source",
        req_enum: "SourceReq",
        decl_fn: "source_decl",
        description: &[
            "Inspect source from the executing request's immutable generation. Edit ordinary Haskell and prompt files, then reloadSource [] to activate compatible source, tool implementations and workspace prompts together. ",
            "Changed configuration starts a fresh notebook; unchanged configuration preserves bindings. Put reusable definitions in .hs files. Already-issued work and children keep their original generation. ",
            "Reload preserves conversation, resources and published memory context; it does not rerun initialization, refresh memory or commit files to Git. Failed preparation leaves the active generation unchanged. ",
            "Source roots and grants remain launch inputs. Captured-only actors can inspect their source but cannot publish editable configuration.",
        ],
        prompt_card: None,
        type_params: &[],
        default_row_args: &[],
        extra_imports: &[],
        type_defs: vec![
            TypeDef {
                name: "SourceModule",
                wire_rust: Some("SrModule"),
                haskell_module: None,
                shape: TypeShape::Record {
                    fields: vec![
                        field("sourceModuleName", "name", HsType::Text),
                        field("sourceModuleDigest", "digest", HsType::Text),
                    ],
                },
                json: JsonInstance::None,
                derives: WIRE,
                domain: None,
                doc: &[
                    "One module a revision provides, and the digest of its source. Two",
                    "revisions agree about a module exactly when these digests match.",
                ],
            },
            TypeDef {
                name: "SourceRevision",
                wire_rust: Some("SrRevision"),
                haskell_module: None,
                shape: TypeShape::Record {
                    fields: vec![
                        field("revisionIdentity", "identity", HsType::Text),
                        field("revisionGeneration", "generation", HsType::Int),
                        field(
                            "revisionModules",
                            "modules",
                            HsType::list(HsType::Named("SourceModule")),
                        ),
                    ],
                },
                json: JsonInstance::None,
                derives: WIRE,
                domain: None,
                doc: &[
                    "One snapshot of the workspace's source roots. `revisionIdentity` is",
                    "content — the same source always names the same revision, changed",
                    "source never does; it is not a time, a path or a commit, and an",
                    "experiment needs no clean tree. `revisionGeneration` is a 1-based",
                    "publication counter for display only, and is 0 for a snapshot that has",
                    "never been published.",
                ],
            },
            TypeDef {
                name: "SourceStatus",
                wire_rust: Some("SrStatus"),
                haskell_module: None,
                shape: TypeShape::Record {
                    fields: vec![
                        field("statusActive", "active", HsType::Named("SourceRevision")),
                        field("statusDisk", "disk", HsType::Named("SourceRevision")),
                    ],
                },
                json: JsonInstance::None,
                derives: WIRE,
                domain: None,
                doc: &[
                    "What this request was compiled against, and what the source roots say",
                    "right now. Equal identities mean the notebook is running the code on",
                    "disk; different ones mean an edit is waiting for a reload.",
                ],
            },
            TypeDef {
                name: "ReloadOutcome",
                wire_rust: Some("SrReloadOutcome"),
                haskell_module: None,
                shape: TypeShape::Sum {
                    variants: vec![
                        SumVariant {
                            ctor: "ReloadUnchanged",
                            fields: positional_fields![HsType::Named("SourceRevision")],
                            doc: &[
                                "The complete configuration is unchanged; notebook bindings are preserved.",
                            ],
                        },
                        SumVariant {
                            ctor: "ReloadPublished",
                            fields: positional_fields![
                                HsType::Named("SourceRevision"),
                                HsType::Named("SourceRevision"),
                                HsType::list(HsType::Text),
                            ],
                            doc: &[
                                "The revision that was active, the revision now active, and the",
                                "modules whose source differs between them. Later cells compile",
                                "against the new one.",
                            ],
                        },
                        SumVariant {
                            ctor: "ReloadRejected",
                            fields: positional_fields![
                                HsType::Named("SourceRevision"),
                                HsType::Named("SourceRevision"),
                                HsType::Text,
                            ],
                            doc: &[
                                "The affected module graph did not typecheck. The first revision",
                                "is the one still active — unchanged — the second names the",
                                "snapshot that failed, and the text carries its diagnostics. The",
                                "edited files are untouched on disk.",
                            ],
                        },
                        SumVariant {
                            ctor: "ReloadCancelled",
                            fields: positional_fields![HsType::Named("SourceRevision")],
                            doc: &["Cancellation won before publication; the issued revision remains active."],
                        },
                        SumVariant {
                            ctor: "ReloadPublishedWithWarnings",
                            fields: positional_fields![
                                HsType::Named("SourceRevision"),
                                HsType::Named("SourceRevision"),
                                HsType::list(HsType::Text),
                                HsType::list(HsType::Text),
                            ],
                            doc: &["Publication committed; the receipt lists derived projections still needing repair. Do not replay the reload."],
                        },
                        SumVariant {
                            ctor: "ReloadPublicationUnconfirmed",
                            fields: positional_fields![HsType::Named("SourceRevision"), HsType::Text],
                            doc: &["Publication acknowledgement is uncertain. New admission is blocked until reconciliation; do not replay."],
                        },
                    ],
                },
                json: JsonInstance::None,
                derives: WIRE,
                domain: None,
                doc: &["What one reload did. Every case is an ordinary value to match on."],
            },
        ],
        external_types: &[],
        errors: Some(ErrorAdt {
            name: "SourceError",
            variants: vec![
                ErrorVariant {
                    ctor: "SourceUnavailable",
                    fields: vec![ErrorField {
                        name: "detail",
                        ty: HsType::Text,
                        rust: RustBinding::Derived,
                    }],
                    doc: "this run has no workspace source layer to reload",
                },
                ErrorVariant {
                    ctor: "SourceUnreadable",
                    fields: vec![ErrorField {
                        name: "detail",
                        ty: HsType::Text,
                        rust: RustBinding::Derived,
                    }],
                    doc: "the declared source roots could not be re-read or captured",
                },
            ],
        }),
        verbs: vec![
            Verb {
                ctor: "SourceReadModule", method: "source_read_module",
                args: vec![Arg { name: "name", ty: HsType::Text, rust: RustBinding::Derived }],
                ret: HsType::Tuple(vec![HsType::Text, HsType::Text]),
                errors: Some("SourceError"), handling: HandlingClass::Actor,
            },
            Verb {
                ctor: "SourceReloadWith",
                method: "source_reload",
                args: vec![
                    Arg {
                        name: "alsoCheck",
                        ty: HsType::list(HsType::Text),
                        rust: RustBinding::Derived,
                    },
                ],
                ret: HsType::Named("ReloadOutcome"),
                errors: Some("SourceError"),
                handling: HandlingClass::Actor,
            },
            Verb {
                ctor: "SourceStatusWith",
                method: "source_status",
                args: Vec::new(),
                ret: HsType::Named("SourceStatus"),
                errors: Some("SourceError"),
                handling: HandlingClass::Actor,
            },
        ],
        helpers: vec![
            Helper { name: "readSourceModule", ctor: Some("SourceReadModule"), substrate: false,
                doc: &["Read a named module from this request's captured source generation. Returns (revision identity, source text), never pending workspace bytes."], body: HelperBody::Pointfree },
            Helper {
                name: "reloadSource",
                ctor: Some("SourceReloadWith"),
                substrate: false,
                doc: &[
                    "Capture and validate edited source and prompts, then publish one coherent generation.",
                    "The list selects additional modules to check. Changed configuration starts a fresh",
                    "notebook; unchanged configuration preserves it. This computation retains its issued",
                    "generation for the rest of its execution. Memory refresh and Git commits are separate.",
                ],
                body: HelperBody::Applied(&["alsoCheck"]),
            },
            Helper {
                name: "sourceStatus",
                ctor: Some("SourceStatusWith"),
                substrate: false,
                doc: &[
                    "The revision this request captured, and the revision your",
                    "own source roots hold right now. Compare `revisionIdentity`s to tell whether an",
                    "edit is waiting, and look a module up in `revisionModules` to compare",
                    "one module across the two.",
                ],
                body: HelperBody::Nullary,
            },
        ],
        polymorphism: Polymorphism::None,
        generated_handler: false,
        handler_execution: crate::schema::HandlerExecution::Immediate,
        // The actor owner supplies the issued generation and publication authority.
        caller_principal: false,
    }
}

fn field(hs_name: &'static str, rust_name: &'static str, ty: HsType) -> RecordField {
    RecordField {
        hs_name,
        rust_name,
        ty,
        doc: &[],
    }
}
