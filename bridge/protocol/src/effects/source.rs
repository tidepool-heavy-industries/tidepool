//! Reloading an actor's own Haskell source inside a live run.
//!
//! A Shoal run captures its declared source roots once and compiles every cell
//! against that capture. `Source` lets a program re-read the roots IT works in,
//! check the whole affected module graph, and publish it as the revision its
//! own later cells compile against — without restarting the session, and
//! without any cell losing the environment it was compiled in. Which roots
//! those are is fixed when the actor is built: the run's, for the actor that
//! owns the run; its own checkout's, for an actor working in one.
//!
//! The verbs are deliberately about *snapshots*, not files: a revision is
//! named by the content of the roots that produced it, so "which code is this"
//! is answerable as data rather than by reading a log line.

use crate::hs::HsType;
use crate::schema::{
    Arg, Effect, ErrorAdt, ErrorField, ErrorVariant, HandlingClass, Helper, HelperBody,
    OuterEffect, Polymorphism, RecordField, RustBinding, SumVariant, TypeDef, TypeShape, Verb,
    WireDerive, WireDerives,
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
            "Reload the Haskell source YOUR OWN cells compile against. Which source that ",
            "is was decided when you were created and cannot be chosen per call: if you ",
            "work in your own checkout, it is the `.shoal` package in that checkout, and ",
            "your reload is invisible to every other actor; if you own the run, it is the ",
            "run's own package, which every actor without a checkout of its own compiles ",
            "against. Write a module under a configured source root with ordinary file ",
            "tools, then `reloadSource []` re-reads every configured root, compiles the ",
            "whole affected module graph, and — only if all of it typechecks — makes that ",
            "snapshot the revision YOUR LATER cells compile against. It is a compilation ",
            "boundary, not dynamic scope: the cell that asked for the reload was already ",
            "compiled against the previous revision and keeps it for the rest of its own ",
            "computation, and a value bound before the reload keeps the code it was built ",
            "from. `ReloadRejected` is an ordinary result, not a catastrophe — the edited ",
            "files are still on disk exactly as written, the previously compiled graph is ",
            "still active, and the receipt names the snapshot that failed and carries its ",
            "diagnostics. `sourceStatus` answers what is active and what is on disk, for ",
            "your own source, as data — so a program can decide whether it is running the ",
            "current code rather than parsing a status page. An actor with no source of ",
            "its own can read that status and has nothing to reload.",
        ],
        prompt_card: None,
        type_params: &[],
        default_row_args: &[],
        helpers_row_polymorphic: true,
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
                    "What later cells are compiled against, and what the source roots say",
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
                                "The source roots still hold the active revision's content, so",
                                "nothing was rebuilt and nothing was republished.",
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
                    ],
                },
                json: JsonInstance::None,
                derives: WIRE,
                domain: None,
                doc: &["What one reload did. Every case is an ordinary value to match on."],
            },
        ],
        foreign_types: &[],
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
                ctor: "SourceReloadWith",
                method: "source_reload",
                args: vec![Arg {
                    name: "alsoCheck",
                    ty: HsType::list(HsType::Text),
                    rust: RustBinding::Derived,
                }],
                ret: HsType::Named("ReloadOutcome"),
                errors: Some("SourceError"),
                handling: HandlingClass::OuterDispatch(OuterEffect::Source),
                extract: None,
            },
            Verb {
                ctor: "SourceStatusWith",
                method: "source_status",
                args: Vec::new(),
                ret: HsType::Named("SourceStatus"),
                errors: Some("SourceError"),
                handling: HandlingClass::OuterDispatch(OuterEffect::Source),
                extract: None,
            },
        ],
        helpers: vec![
            Helper {
                name: "reloadSource",
                ctor: Some("SourceReloadWith"),
                substrate: false,
                doc: &[
                    "Re-read every configured source root of the package YOU work in, check",
                    "the affected module graph, and publish it as one transaction.",
                    "`reloadSource []` is that whole package; the list names ADDITIONAL",
                    "modules to pull into the checked",
                    "graph when a module you rely on is not reachable from the workspace's",
                    "configured module list. Natural spelling: `Right outcome <- reloadSource",
                    "[]`. The definitions become available to LATER cells — the cell that",
                    "called this keeps the environment it was compiled in, so do not expect",
                    "to call a newly loaded API from here.",
                ],
                body: HelperBody::Pointfree,
            },
            Helper {
                name: "sourceStatus",
                ctor: Some("SourceStatusWith"),
                substrate: false,
                doc: &[
                    "The revision YOUR later cells compile against, and the revision your",
                    "own source roots hold right now. Compare `revisionIdentity`s to tell whether an",
                    "edit is waiting, and look a module up in `revisionModules` to compare",
                    "one module across the two.",
                ],
                body: HelperBody::Nullary,
            },
        ],
        polymorphism: Polymorphism::None,
        dispatched: true,
        // A reload acts on the CALLER's own source layer: the root's for the
        // root, and an actor's own checkout layer for an actor that has one.
        // The handler needs the kernel-issued principal to reach it.
        caller_principal: true,
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
