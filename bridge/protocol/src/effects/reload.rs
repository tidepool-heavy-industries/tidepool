//! Synchronous source publication and observed retirement settlement.

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
 description: &["Publish edited source through an admitted synchronous endpoint. CompatibleOnly preserves the declared tool surface; AcceptCacheInvalidation permits supported surface changes and invalidates the provider tool cache. Force independently authorizes retirement of the captured dependent resources; by default blockers refuse without stops."],
 prompt_card: None, type_params: &[], default_row_args: &[], extra_imports: &[],
 type_defs: { let mut definitions = vec![TypeDef { name: "ReloadPolicy", wire_rust: Some("RlPolicy"), haskell_module: None, shape: TypeShape::Sum { variants: vec![SumVariant { ctor: "CompatibleOnly", fields: positional_fields![], doc: &[] },SumVariant { ctor: "AcceptCacheInvalidation", fields: positional_fields![], doc: &[] }] }, json: JsonInstance::Generic, derives: WIRE, domain: None, doc: &[] },TypeDef { name: "ReloadOptions", wire_rust: Some("RlOptions"), haskell_module: None, shape: TypeShape::Record { fields: vec![field("reloadAlsoCheck", "also_check", HsType::list(HsType::Text)),field("reloadPolicy", "policy", HsType::Named("ReloadPolicy")),field("reloadForce", "force", HsType::Bool)] }, json: JsonInstance::Generic, derives: WIRE, domain: None, doc: &[] },TypeDef { name: "ReloadSurfaceDiff", wire_rust: Some("RlSurfaceDiff"), haskell_module: None, shape: TypeShape::Record { fields: vec![field("surfaceAdded", "added", HsType::list(HsType::Text)),field("surfaceRemoved", "removed", HsType::list(HsType::Text)),field("surfaceChanged", "changed", HsType::list(HsType::Text))] }, json: JsonInstance::Generic, derives: WIRE, domain: None, doc: &[] },TypeDef { name: "ReloadRefusalCategory", wire_rust: Some("RlRefusalCategory"), haskell_module: None, shape: TypeShape::Sum { variants: vec![SumVariant { ctor: "PreparationFailed", fields: positional_fields![], doc: &[] },SumVariant { ctor: "SurfaceIncompatible", fields: positional_fields![], doc: &[] },SumVariant { ctor: "UnsupportedSurface", fields: positional_fields![], doc: &[] },SumVariant { ctor: "AuthorityDenied", fields: positional_fields![], doc: &[] }] }, json: JsonInstance::Generic, derives: WIRE, domain: None, doc: &[] },TypeDef { name: "ReloadRefusal", wire_rust: Some("RlRefusal"), haskell_module: None, shape: TypeShape::Record { fields: vec![field("refusalCategory", "category", HsType::Named("ReloadRefusalCategory")),field("refusalDetail", "detail", HsType::Text),field("refusalSurface", "surface", HsType::Named("ReloadSurfaceDiff"))] }, json: JsonInstance::Generic, derives: WIRE, domain: None, doc: &[] },            TypeDef {
                name: "ReloadOutcome",
                wire_rust: Some("SrReloadOutcome"),
                haskell_module: None,
                shape: TypeShape::Sum {
                    variants: vec![
                        SumVariant { ctor: "ReloadRetirement", fields: positional_fields![HsType::Named("ReloadRetirementReceipt")], doc: &["Observed retirement effects and publication settlement. A reopened generation may have confirmed partial stops; stopped resources are not restarted."] },
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
            },]; definitions.extend(retirement_types()); definitions },
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

// These receipts are observations issued by lifecycle owners, never stop authority.
// The additive ReloadRetirement constructor is the wire migration: old consumers
// refuse that unknown constructor rather than misclassifying partial effects.
// Every result after retirement capture must use ReloadRetirement, carrying the
// actual owner-issued outcomes. ReloadPublished, ReloadCancelled,
// ReloadPublishedWithWarnings and ReloadPublicationUnconfirmed remain temporarily
// for the host endpoint and its existing test callers. Delete those constructors
// when that endpoint/caller migration lands; DTO generation alone does not issue
// lifecycle evidence or qualify the endpoint's execution and recovery behavior.
fn retirement_types() -> Vec<TypeDef> {
    use HsType::{Int, Named, Text};
    let address = || HsType::Tuple(vec![Int, Int]);
    vec![
        record("ReloadOperation", "RlOperation", vec![
            field("reloadExecution", "execution", Text),
            field("reloadInputUnitIndex", "input_unit_index", Int),
            field("reloadEffectOrdinal", "effect_ordinal", Int),
        ], &["The existing hosted operation coordinate; absence means this caller has no issued coordinate."]),
        sum("ReloadReservation", "RlReservation", vec![
            variant("ReloadWorkbenchReservation", vec![Text, Text]),
            variant("ReloadRouteReservation", vec![Int]),
            variant("ReloadScopeReservation", vec![Int]),
        ], &["Existing request owner coordinates. Workbench execution and exact reservation attempt remain separate."]),
        sum("ReloadNativeIdentity", "RlNativeIdentity", vec![
            variant("ReloadNativeLocal", vec![Text]),
            variant("ReloadNativeRemote", vec![Text, Text]),
        ], &["Native actor identity preserves Local versus Remote and exact unsigned decimal node/pid coordinates without signed narrowing. These are observations, not authority."]),
        sum("ReloadResource", "RlResource", vec![
            variant("ReloadActorResource", vec![address()]),
            variant("ReloadNativeResource", vec![address(), Named("ReloadNativeIdentity")]),
            variant("ReloadPreparationResource", vec![Int]),
            variant("ReloadInvocationResource", vec![address(), Named("ReloadReservation")]),
            variant("ReloadCompilerResource", vec![address(), Named("ReloadReservation"), Int]),
            variant("ReloadFormResource", vec![address(), Named("ReloadReservation"), Text]),
            variant("ReloadDisplayResource", vec![Named("ReloadOperation")]),
            variant("ReloadWatchResource", vec![address(), Int]),
            variant("ReloadUnclassifiedPhysicalResource", vec![Named("ReloadNativeIdentity")]),
            variant("ReloadActorCompilerResource", vec![address(), Text]),
            variant("ReloadPreparationCompilerResource", vec![Text, Text]),
            variant("ReloadSourceBindingResource", vec![address(), Text]),
        ], &["Exact coordinates projected from existing resource owners, not names or new handles. Actor addresses contain id and incarnation. Native ids preserve the owning runtime's Local/Remote identity; form ids and reservation attempts retain their issuing UUIDs. Actor-owned and preparation-owned compiler resources are distinct from invocation-owned ReloadCompilerResource. Their registration ids and preparation ids preserve exact unsigned decimal coordinates as Text. Source binding resources name the actual actor's stable callback slot as unsigned decimal Text, not a watch id. These observations grant no authority."]),
        sum("ReloadBlockerReason", "RlBlockerReason", vec![
            variant("ReloadActiveDependentWork", vec![]),
            variant("ReloadUnclassifiedWork", vec![]),
            variant("ReloadStartingResource", vec![]),
            variant("ReloadRunningResource", vec![]),
            variant("ReloadTransferringResource", vec![]),
            variant("ReloadCleanupUnsupported", vec![]),
            variant("ReloadCleanupUnconfirmed", vec![Text]),
            variant("ReloadCompilerPending", vec![]),
            variant("ReloadCompilerUnconfirmed", vec![Text]),
            variant("ReloadPendingProducer", vec![]),
            variant("ReloadDisplayAcknowledgementPending", vec![]),
            variant("ReloadDisplayAcknowledgementUnconfirmed", vec![Text]),
        ], &[]),
        sum("ReloadLifetimeEdge", "RlLifetimeEdge", vec![
            variant("ReloadPhysicalSupervision", vec![]),
            variant("ReloadPendingSupervision", vec![]),
            variant("ReloadPendingResourcePreparation", vec![]),
            variant("ReloadResourceOwnership", vec![]),
            variant("ReloadOwnershipTransfer", vec![]),
            variant("ReloadOwnershipTransferUnconfirmed", vec![]),
        ], &[]),
        sum("ReloadBlocker", "RlBlocker", vec![
            variant("ReloadResourceBlocker", vec![Named("ReloadResource"), Named("ReloadBlockerReason")]),
            variant("ReloadCollateralBlocker", vec![Named("ReloadResource"), Named("ReloadResource"), Named("ReloadLifetimeEdge")]),
            variant("ReloadUnclassifiedSourceWork", vec![Int]),
        ], &["Observed work that prevents retirement. A collateral blocker names the proposed stop and the independently affected resource."]),
        sum("ReloadCauseKind", "RlCauseKind", vec![
            variant("ReloadRetirementBlocked", vec![]),
            variant("ReloadRetirementCancelled", vec![]),
            variant("ReloadCandidateSettlementFailed", vec![]),
            variant("ReloadOriginalReopenFailed", vec![]),
            variant("ReloadStopFailed", vec![]),
            variant("ReloadCleanupFailed", vec![]),
            variant("ReloadPublicationFailed", vec![]),
            variant("ReloadContinuationFailed", vec![]),
            variant("ReloadAcknowledgementUnconfirmed", vec![]),
        ], &[]),
        record("ReloadCause", "RlCause", vec![
            field("reloadCauseKind", "kind", Named("ReloadCauseKind")),
            field("reloadCauseDetail", "detail", Text),
        ], &["Typed cause with explanatory detail. Detail text never selects control flow."]),
        sum("ReloadStopOutcome", "RlStopOutcome", vec![
            variant("ReloadStopNotRequested", vec![]),
            variant("ReloadStopPending", vec![]),
            variant("ReloadStopConfirmed", vec![]),
            variant("ReloadStopRefused", vec![Named("ReloadCause")]),
            variant("ReloadStopUnconfirmed", vec![Named("ReloadCause")]),
        ], &[]),
        sum("ReloadCleanupOutcome", "RlCleanupOutcome", vec![
            variant("ReloadCleanupNotRequired", vec![]),
            variant("ReloadCleanupPending", vec![]),
            variant("ReloadCleanupConfirmed", vec![]),
            variant("ReloadCleanupRefused", vec![Named("ReloadCause")]),
            variant("ReloadCleanupUnknown", vec![Named("ReloadCause")]),
        ], &["Physical cleanup settlement is independent of terminal stop acknowledgement."]),
        record("ReloadResourceOutcome", "RlResourceOutcome", vec![
            field("reloadOutcomeResource", "resource", Named("ReloadResource")),
            field("reloadStopOutcome", "stop", Named("ReloadStopOutcome")),
            field("reloadCleanupOutcome", "cleanup", Named("ReloadCleanupOutcome")),
        ], &["Actual observed effects, never the planned population re-labelled as stopped."]),
        sum("ReloadRetirementStage", "RlRetirementStage", vec![
            variant("ReloadCandidateSettlement", vec![]),
            variant("ReloadOriginalReopen", vec![]),
            variant("ReloadResourceStop", vec![]),
            variant("ReloadResourceCleanup", vec![]),
            variant("ReloadSourcePublication", vec![]),
        ], &[]),
        sum("ReloadRetirementState", "RlRetirementState", vec![
            variant("ReloadAReopened", vec![]),
            variant("ReloadRetirementFenced", vec![Named("ReloadRetirementStage")]),
            variant("ReloadBPublished", vec![HsType::list(Text), HsType::list(Text)]),
        ], &["Source outcome only: AReopened proves A selected and B uncommitted; actual partial stops remain in the receipt. Fenced does not claim A or B selected. BPublished confirms B selected and records changed modules and projection warnings. Operational settlement is reported independently in the receipt."]),
        sum("ReloadObligationObservation", "RlObligationObservation", vec![
            variant("ReloadObligationPending", vec![]),
            variant("ReloadObligationUnconfirmed", vec![]),
            variant("ReloadObligationRefused", vec![]),
        ], &["Owner-observed status of an outstanding obligation. Refused means its owner refused the outstanding obligation; a terminal failed operation with no remaining obligation is operationally Settled and retains its typed cause separately."]),
        sum("ReloadRetirementObligation", "RlRetirementObligation", vec![
            variant("ReloadCandidateSettlementObligation", vec![Named("ReloadObligationObservation")]),
            variant("ReloadOriginalRetirementObligation", vec![Named("ReloadObligationObservation")]),
            variant("ReloadPhysicalCleanupObligation", vec![Named("ReloadObligationObservation")]),
            variant("ReloadPublicationNotificationObligation", vec![Named("ReloadObligationObservation")]),
            variant("ReloadSourceDeliveryObligation", vec![Named("ReloadResource"), Named("ReloadObligationObservation")]),
            variant("ReloadDiagnosticDeliveryObligation", vec![Named("ReloadObligationObservation")]),
        ], &["Exact outstanding work reported by its retained owner. Source delivery identifies the actual resource. These observations grant no stop, cleanup, publication or retry authority."]),
        sum("ReloadOperationalSettlement", "RlOperationalSettlement", vec![
            variant("ReloadOperationalSettled", vec![]),
            variant("ReloadOperationalUnsettled", vec![Named("ReloadRetirementObligation"), HsType::list(Named("ReloadRetirementObligation"))]),
        ], &["Operational settlement is independent of source outcome and typed causes. Unsettled has a mandatory first obligation and reports operational Unknown; Settled means no outstanding obligations, including a terminal failed operation whose cause remains separately recorded."]),
        record("ReloadRetirementReceipt", "RlRetirementReceipt", vec![
            field("reloadPrevious", "previous", Named("SourceRevision")),
            field("reloadCandidate", "candidate", Named("SourceRevision")),
            field("reloadOperation", "operation", HsType::maybe(Named("ReloadOperation"))),
            field("reloadRetirementState", "state", Named("ReloadRetirementState")),
            field("reloadBlockers", "blockers", HsType::list(Named("ReloadBlocker"))),
            field("reloadResourceOutcomes", "outcomes", HsType::list(Named("ReloadResourceOutcome"))),
            field("reloadRetirementSurface", "surface", Named("ReloadSurfaceDiff")),
            field("reloadPrimaryCause", "primary", HsType::maybe(Named("ReloadCause"))),
            field("reloadSecondaryCauses", "secondary", HsType::list(Named("ReloadCause"))),
            field("reloadOperationalSettlement", "settlement", Named("ReloadOperationalSettlement")),
        ], &["Retirement of exact frozen A toward candidate B. A later editable draft is not this candidate. Source outcome, operational settlement and primary/secondary causes are independent observations. Inspect the owning operation and resource records for detailed retained obligations."]),
    ]
}

fn record(
    name: &'static str,
    wire: &'static str,
    fields: Vec<RecordField>,
    doc: &'static [&'static str],
) -> TypeDef {
    TypeDef {
        name,
        wire_rust: Some(wire),
        haskell_module: None,
        shape: TypeShape::Record { fields },
        json: JsonInstance::Generic,
        derives: WIRE,
        domain: None,
        doc,
    }
}
fn sum(
    name: &'static str,
    wire: &'static str,
    variants: Vec<SumVariant>,
    doc: &'static [&'static str],
) -> TypeDef {
    TypeDef {
        name,
        wire_rust: Some(wire),
        haskell_module: None,
        shape: TypeShape::Sum { variants },
        json: JsonInstance::Generic,
        derives: WIRE,
        domain: None,
        doc,
    }
}
fn variant(ctor: &'static str, fields: Vec<HsType>) -> SumVariant {
    SumVariant {
        ctor,
        fields: crate::schema::VariantFields::Positional(fields),
        doc: &[],
    }
}
