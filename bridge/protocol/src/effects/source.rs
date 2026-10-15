//! Inspect the executing request's immutable source generation.

use crate::hs::HsType;
use crate::schema::{
    Arg, Effect, ErrorAdt, ErrorField, ErrorVariant, HandlingClass, Helper, HelperBody,
    Polymorphism, RecordField, RustBinding, TypeDef, TypeShape, Verb, WireDerive, WireDerives,
};
use crate::types::JsonInstance;

const WIRE: WireDerives = WireDerives(&[
    WireDerive::ToHaskell,
    WireDerive::FromHaskell,
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
        description: &["Inspect source from the executing request's immutable generation. Source inspection does not publish edited files; synchronous Reload owns publication."],
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
                json: JsonInstance::Generic,
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
                json: JsonInstance::Generic,
                derives: WIRE,
                domain: None,
                doc: &[
                    "One snapshot of the workspace's source roots. `revisionIdentity` is",
                    "content — the same source always names the same revision, changed",
                    "source never does; it is not a time, a path or a commit, and an",
                    "experiment needs no clean tree. `revisionGeneration` is reserved",
                    "metadata and is 0 for content-issued source snapshots; it does not",
                    "indicate publication state or the actor's notebook generation.",
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
                json: JsonInstance::Generic,
                derives: WIRE,
                domain: None,
                doc: &[
                    "What this request was compiled against, and what the source roots say",
                    "right now. Equal identities mean the notebook is running the code on",
                    "disk; different ones mean an edit is waiting for a reload.",
                ],
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
