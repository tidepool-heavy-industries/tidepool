//! Inspect the executing request's immutable source generation.

use crate::hs::HsType;
use crate::schema::{
    Arg, Effect, ErrorAdt, ErrorField, ErrorVariant, HandlingClass, Helper, HelperBody,
    Polymorphism, RecordField, RustBinding, SumVariant, TypeDef, TypeShape, Verb, WireDerive,
    WireDerives,
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

/// Synchronous, invocation-authorized publication of edited source.
#[must_use]
pub fn reload() -> Effect {
    Effect { name: "Reload", authored_surface: crate::schema::AuthoredSurface::All,
 handler: "ReloadHandler", handler_module: "reload", req_enum: "ReloadReq", decl_fn: "reload_decl",
 description: &["Publish edited source through an admitted synchronous endpoint. CompatibleOnly preserves the declared tool surface; AcceptCacheInvalidation permits supported surface changes and invalidates the provider tool cache."],
 prompt_card: None, type_params: &[], default_row_args: &[], extra_imports: &[],
 type_defs: vec![TypeDef { name: "ReloadPolicy", wire_rust: Some("RlPolicy"), haskell_module: None, shape: TypeShape::Sum { variants: vec![SumVariant { ctor: "CompatibleOnly", fields: positional_fields![], doc: &[] },SumVariant { ctor: "AcceptCacheInvalidation", fields: positional_fields![], doc: &[] }] }, json: JsonInstance::Generic, derives: WIRE, domain: None, doc: &[] },TypeDef { name: "ReloadOptions", wire_rust: Some("RlOptions"), haskell_module: None, shape: TypeShape::Record { fields: vec![field("reloadAlsoCheck", "also_check", HsType::list(HsType::Text)),field("reloadPolicy", "policy", HsType::Named("ReloadPolicy"))] }, json: JsonInstance::Generic, derives: WIRE, domain: None, doc: &[] },TypeDef { name: "ReloadSurfaceDiff", wire_rust: Some("RlSurfaceDiff"), haskell_module: None, shape: TypeShape::Record { fields: vec![field("surfaceAdded", "added", HsType::list(HsType::Text)),field("surfaceRemoved", "removed", HsType::list(HsType::Text)),field("surfaceChanged", "changed", HsType::list(HsType::Text))] }, json: JsonInstance::Generic, derives: WIRE, domain: None, doc: &[] },TypeDef { name: "ReloadRefusalCategory", wire_rust: Some("RlRefusalCategory"), haskell_module: None, shape: TypeShape::Sum { variants: vec![SumVariant { ctor: "PreparationFailed", fields: positional_fields![], doc: &[] },SumVariant { ctor: "SurfaceIncompatible", fields: positional_fields![], doc: &[] },SumVariant { ctor: "UnsupportedSurface", fields: positional_fields![], doc: &[] },SumVariant { ctor: "AuthorityDenied", fields: positional_fields![], doc: &[] }] }, json: JsonInstance::Generic, derives: WIRE, domain: None, doc: &[] },TypeDef { name: "ReloadRefusal", wire_rust: Some("RlRefusal"), haskell_module: None, shape: TypeShape::Record { fields: vec![field("refusalCategory", "category", HsType::Named("ReloadRefusalCategory")),field("refusalDetail", "detail", HsType::Text),field("refusalSurface", "surface", HsType::Named("ReloadSurfaceDiff"))] }, json: JsonInstance::Generic, derives: WIRE, domain: None, doc: &[] },            TypeDef {
                name: "ReloadOutcome",
                wire_rust: Some("SrReloadOutcome"),
                haskell_module: None,
                shape: TypeShape::Sum {
                    variants: vec![
                        SumVariant { ctor: "ReloadRefused", fields: positional_fields![HsType::Named("SourceRevision"), HsType::Named("SourceRevision"), HsType::Named("ReloadRefusal")], doc: &["The candidate was refused without publication; the receipt gives the typed category and surface difference."] },
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
                                HsType::Named("ReloadSurfaceDiff"),
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
                                HsType::Named("ReloadSurfaceDiff"),
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
                json: JsonInstance::Generic,
                derives: WIRE,
                domain: None,
                doc: &["What one reload did. Every case is an ordinary value to match on."],
            },],
 external_types: &[crate::schema::ExternalType { haskell_name: "SourceRevision", rust_wire: "SrRevision", core_module: None }],
 errors: Some(ErrorAdt { name: "ReloadError", variants: vec![
 ErrorVariant { ctor: "ReloadUnavailable", fields: vec![ErrorField { name: "detail", ty: HsType::Text, rust: RustBinding::Derived }], doc: "this actor does not own editable source publication" },
 ErrorVariant { ctor: "ReloadUnreadable", fields: vec![ErrorField { name: "detail", ty: HsType::Text, rust: RustBinding::Derived }], doc: "the declared source roots could not be read or captured" },
 ] }),
 verbs: vec![Verb { ctor: "ReloadSourceWith", method: "reload_source", args: vec![Arg { name: "options", ty: HsType::Named("ReloadOptions"), rust: RustBinding::Path("tidepool_bridge_effects::RlOptions") }], ret: HsType::Named("ReloadOutcome"), errors: Some("ReloadError"), handling: HandlingClass::Actor }],
 helpers: vec![Helper { name: "reloadSourceWith", ctor: Some("ReloadSourceWith"), substrate: false, doc: &["Capture, validate and atomically publish edited source under the explicit surface policy."], body: HelperBody::Pointfree }],
 polymorphism: Polymorphism::None, generated_handler: false, handler_execution: crate::schema::HandlerExecution::Immediate, caller_principal: false,
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
