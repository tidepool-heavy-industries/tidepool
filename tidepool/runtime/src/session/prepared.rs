//! Runtime ownership for validated prepared-STG execution artifacts.
//!
//! Here `prepared` refers to the GHC prepared-STG handoff. It is distinct from
//! cell preparation in `workbench.rs` and `resident_workbench.rs`.
//!
//! Parsing, linking, compiled-owner construction, execution, cancellation,
//! disposition, and retained-program reuse cross this boundary in that order.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::{atomic::AtomicBool, Arc};

use tidepool_bridge::{BridgeError, HaskellValue, HaskellVisitor};
use tidepool_codegen::binding_table::{BindingEntry, BindingTable, BoundValue};
use tidepool_toolchain::certified_products::CertifiedTargetPackageInterfaces;

use super::binding_table::BindingIndex;
use tidepool_codegen::machine_state::MachineFailure;
#[cfg(test)]
use tidepool_codegen::prepared_program::ScopedCertifiedGroup;
use tidepool_codegen::prepared_program::{
    BatchImport, BatchLeaseRequest, BatchProgram, CompileError, CompiledProgram, DefinitionFacts,
    DemandError, DemandedImage, ExecutionError, ImageRegistry, ImportBindings,
    InheritedSourceDemand, ManagedBuilder, ManagedField, ManagedNode, PackageLiteral, Parcel,
    ParkRequest, PreparedCallOptions, PreparedFrameEvidence, PreparedHandle, PreparedInput,
    PreparedMachine, PreparedMachineOptions, PreparedOuter as CodegenPreparedOuter,
    PreparedReplyEvidence, PreparedResult, PreparedResultBatch, ProgramId, RunOptions,
    ScopedDemandedImage, ScopedSourceBinder, SourceBinder, SourceInstanceAttachment,
    SourceInstanceDomain, SourceInstanceLease, SourceLiteral, MAX_ANSWER_DEPTH,
};
// Re-exported: callers of this module's resource-scope cancellation API
// (`open_realm`/`cancel_handle`/`close_realm`) need both types without a
// separate `tidepool_codegen` dependency of their own.
pub use tidepool_codegen::machine::CancelHandle;
pub use tidepool_codegen::machine::MachineDisposition;
use tidepool_codegen::suspension::ContinuationId;
pub use tidepool_codegen::suspension::{RealmId, ValueHandle};
use tidepool_repr::execution_schema::{
    link_program, CachedHomeOwner, CertifiedGroup, ConstructorReply, DefinitionsView, GlobalId,
    Group, HeapRhs, ImportOwner, ImportedValue, JsonLayout, LinkError, MachineImports, ParseError,
    PreparedProgram, RuntimeRep, Signature, SiteDelivery, SiteRow, SymbolIdentity, TypeNodeId,
    ValueId,
};
use tidepool_repr::type_graph::{
    DataView, GraphLimits, TypeCursor, TypeGraph, TypeGraphError, TypeView, TypeWorkBudget,
};
use tidepool_repr::{DataConId, DataConTable, Literal, PrincipalId, SessionVarId};

use tidepool_effect::{EffectRunPolicy, LivePayloadPolicy};

use super::turn::{
    PREPARED_APPLY_ENTRY_TARGET, PREPARED_APPLY_VALUE_TARGET, PREPARED_RESUME_TARGET,
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PreparedFailureKind {
    Rejected,
    Language,
    Cancelled,
    Integrity,
}

/// The owning prepared operation that failed, before or during execution.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PreparedFailureStage {
    Install,
    Run,
}

/// Bounded reply roots observed at a refused installation. These diagnostics
/// neither establish type equality nor authorize an alternate reply owner.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ConstructorReplyConflictEvidence {
    pub constructor: Option<SymbolIdentity>,
    pub existing: ConstructorReplyObservation,
    pub incoming: ConstructorReplyObservation,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ConstructorReplyObservation {
    AtSite,
    Static {
        node: TypeNodeId,
        shape: ReplyTypeObservation,
        input_site: Option<(u32, u32, Option<u32>)>,
    },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ReplyTypeObservation {
    Refused(TypeGraphError),
    Data {
        family: SymbolIdentity,
        argument_count: usize,
        constructor_count: usize,
    },
    Text,
    Integer,
    Natural,
    Scalar(RuntimeRep),
    Unconstructible {
        reason: String,
        rendered: String,
    },
}

/// The package evidence observed when a sealed package owner cannot be
/// resolved. These facts explain the refusal; they never authorize a fallback.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum CertifiedPackageOwnerEvidence {
    RetainedExport {
        interface_digest: Option<[u8; 32]>,
    },
    TargetDefinition {
        present: bool,
        interfaces_match: bool,
        interface_digest: Option<[u8; 32]>,
        diagnostic: Option<Box<CertifiedPackageOwnerDiagnostic>>,
    },
    DeclarationMismatch {
        declaration: SymbolIdentity,
    },
    ConflictingPackageProof {
        previous_interface_digest: [u8; 32],
    },
    ConflictingTargetDefinition {
        previous_binding: ValueId,
        requested_binding: ValueId,
        previous_interface_digest: [u8; 32],
        requested_interface_digest: [u8; 32],
    },
}

/// Bounded source facts captured when a package owner is not exportable from
/// either retained machine state or the sealed target. This is diagnostic
/// evidence only; none of these facts authorize an alternate owner.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CertifiedPackageOwnerDiagnostic {
    pub target_globals: Vec<PackageTargetGlobalFact>,
    pub target_globals_omitted: usize,
    pub target_global_count: usize,
    pub target_owner_count: usize,
    pub demanded_groups: Vec<PackageDemandedGroupFact>,
    pub demanded_groups_omitted: usize,
    pub demanded_group_count: usize,
    pub target_top: PackageTargetTopFact,
    pub retained_export: PackageRetainedExportFact,
    pub target_interfaces_match: bool,
    pub target_interface_digest: Option<[u8; 32]>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PackageTargetGlobalFact {
    pub identity: SymbolIdentity,
    pub required_generation: Option<u64>,
    pub owner: Option<ImportOwner>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PackageDemandedGroupFact {
    pub owner: CachedHomeOwner,
    pub original_ordinal: u32,
    pub imports: Vec<PackageDemandedImportFact>,
    pub imports_omitted: usize,
    pub import_count: usize,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PackageDemandedImportFact {
    pub position: usize,
    pub owner: ImportOwner,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PackageTargetTopKind {
    Absent,
    Bytes,
    Constructor,
    Function,
    Thunk,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PackageTargetTopExportability {
    Exportable,
    Absent,
    NotValueNamespace,
    HomeUnit,
    ByteLiteralNotAdmitted,
    ConstructorNotLifted,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PackageTargetTopFact {
    pub kind: PackageTargetTopKind,
    pub exportability: PackageTargetTopExportability,
    pub literal_admitted: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PackageRetainedExportFact {
    Missing,
    Present {
        interface_digest: Option<[u8; 32]>,
        protected_interface_matches: bool,
    },
}

const PACKAGE_DIAGNOSTIC_TARGET_GLOBAL_LIMIT: usize = 256;
const PACKAGE_DIAGNOSTIC_GROUP_LIMIT: usize = 128;
const PACKAGE_DIAGNOSTIC_IMPORT_LIMIT: usize = 2048;

#[derive(Clone, Copy)]
struct PackageOwnerDiagnosticLimits {
    target_globals: usize,
    groups: usize,
    imports: usize,
}

impl Default for PackageOwnerDiagnosticLimits {
    fn default() -> Self {
        Self {
            target_globals: PACKAGE_DIAGNOSTIC_TARGET_GLOBAL_LIMIT,
            groups: PACKAGE_DIAGNOSTIC_GROUP_LIMIT,
            imports: PACKAGE_DIAGNOSTIC_IMPORT_LIMIT,
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum PreparedRuntimeError {
    #[error(transparent)]
    Parse(#[from] ParseError),
    #[error(transparent)]
    Link(Box<LinkError>),
    #[error("prepared execution cancelled")]
    Cancelled,
    #[error("prepared compilation rejected: {0}")]
    Compile(CompileError),
    #[error("prepared installation failed: {0}")]
    Install(ExecutionError),
    #[error(transparent)]
    Demand(DemandError),
    #[error("Ready renderer lacks the exact prepared native image")]
    MissingPreparedNativeImage,
    #[error("prepared type evidence refused: {0}")]
    TypeEvidence(#[from] TypeGraphError),
    #[error("request access site {site} type evidence refused: {source}")]
    RequestScopeTypeEvidence {
        site: u64,
        #[source]
        source: TypeGraphError,
    },
    #[error("no exact live owner for certified import {0:?}")]
    MissingCertifiedOwner(ImportOwner),
    #[error("certified package owner {owner:?} has no exact source: {evidence:?}")]
    CertifiedPackageOwnerUnavailable {
        owner: ImportOwner,
        evidence: CertifiedPackageOwnerEvidence,
    },
    #[error(
        "no exact live owner for certified retained import {identity:?} at generation {generation}"
    )]
    MissingRetainedCertifiedOwner {
        identity: SymbolIdentity,
        generation: u64,
    },
    #[error("certified target import owners do not match its declared globals")]
    CertifiedTargetOwners,
    #[error("certified programs disagree on the resident Settled constructor identities")]
    ConflictingSettledConstructors,
    #[error("certified install includes a source group unreachable from its target")]
    UnreachableCertifiedGroup,
    #[error("certified source installation targeted a closed or conflicting lexical scope")]
    SourceScopeAdmission,
    #[error("lexical scope has multiple mutable instances for certified source {0:?}")]
    AmbiguousSourceInstance(SourceBinder),
    #[error("lexical scope has multiple mutable instances for certified group {owner:?} ordinal {ordinal}")]
    AmbiguousSourceGroup {
        owner: tidepool_repr::execution_schema::CachedHomeOwner,
        ordinal: u32,
    },
    #[error("certified source owner differs from the selected original group for {0:?}")]
    InvalidCertifiedSourceOwner(SourceBinder),
    #[error("prepared execution failed: {0}")]
    Run(ExecutionError),
    #[error("session binding {0:?} is not a live prepared binding")]
    UnknownBinding(SessionVarId),
    #[error(
        "a managed argument's handle is not live under realm {realm:?}: it was minted under a \
         different runtime resource scope (or already released)"
    )]
    CrossRealmArgument { realm: RealmId },
    /// A turn program whose entry is not the settled scaffold
    /// (`Tidepool.Internal.Resume.Settled`), or whose settled layer did not
    /// have the declared shape. Every turn template defines `__prepared`,
    /// so this is a stale or foreign artifact, never a user error.
    #[error("prepared program {program:?} has no settled entry layer: {detail}")]
    UnsettledEntry {
        program: ProgramId,
        detail: &'static str,
    },
    /// An operation requires the prepared machine before its first program
    /// has installed it.
    #[error("the prepared machine is not installed")]
    MachineNotInstalled,
    /// One artifact declares the same typed site twice with different
    /// evidence: a projection defect, refused before anything installs.
    #[error("the artifact declares typed site {site} twice with different evidence")]
    DuplicateSite { site: u64 },
    /// A pattern bind's settled value did not carry one managed field per
    /// GHC binder. The extractor projects the binders as one tuple, so this
    /// is a stale or foreign artifact, never a user error.
    #[error("pattern bind produced {fields} fields for {binders} GHC binders")]
    ProjectionShape { binders: usize, fields: usize },
    /// A program being installed declares a typed site another installed
    /// program already declares with different evidence (delivery, wire or
    /// input types). Installation is refused before anything is published;
    /// the existing owner stays canonical.
    #[error("typed site {site} is already installed by program {owner:?} with different evidence")]
    SiteConflict { site: u64, owner: ProgramId },
    /// A suspended request named a typed site no installed program declares.
    /// The request and continuation were released; nothing was parked.
    #[error("the suspended request names typed site {site}, which no installed program declares")]
    UnknownSite { site: u64 },
    /// A suspended request is not a constructor, so it names no site and no
    /// verb. The request and continuation were released; nothing was parked.
    #[error("the suspended request {constructor} names no typed site or verb")]
    UntypedRequest { constructor: String },
    #[error("request constructor {constructor:?} has no compiler-issued reply evidence")]
    MissingReplyEvidence { constructor: DataConId },
    #[error("request constructor {constructor:?} has a malformed first RequestSite carrier")]
    MalformedRequestSite { constructor: DataConId },
    #[error(
        "reply evidence for constructor {constructor:?} conflicts with installed program {owner:?}: {evidence:?}"
    )]
    ConstructorReplyConflict {
        constructor: DataConId,
        owner: ProgramId,
        evidence: Box<ConstructorReplyConflictEvidence>,
    },
    #[error("duplicate reply evidence for constructor {constructor:?}: {evidence:?}")]
    DuplicateConstructorReply {
        constructor: DataConId,
        evidence: Box<ConstructorReplyConflictEvidence>,
    },
    #[error("program {owner:?} has no reply site row {row}")]
    MissingReplySiteRow { owner: ProgramId, row: usize },
    /// The turn suspended under `HandleOrError`. The prepared route parks
    /// nothing under that policy: handled effects are answered from the
    /// parked frame ([`crate::session::ResidentSession`] offers every parked
    /// request to the session's handler stack), so a run that must not park
    /// cannot be handled either.
    #[error("the turn requested an effect under HandleOrError; the prepared route parks nothing under that policy")]
    UnhandledRequest,
    /// A one-shot synchronous driver reached async work and has no executor
    /// or retained completion channel to run it.
    #[error("deferred effect requires an async host")]
    DeferredRequiresAsyncHost,
    /// The session's effect handler stack claimed a parked request and then
    /// failed. The frame was aborted; nothing stays parked.
    #[error("effect handler for `{constructor}` failed: {detail}")]
    Handler { constructor: String, detail: String },
    /// The program that produced a suspension admits no resume entry, so its
    /// continuation could never be re-entered. Every turn template defines
    /// the entry; this is a stale or foreign artifact, never a user error.
    #[error("program {program:?} admits no `{entry}` entry, so its suspension cannot be parked")]
    NoResumeEntry {
        program: ProgramId,
        entry: &'static str,
    },
    /// A host-built answer was offered to a site whose delivery is not a host
    /// answer (a live re-entry, an exit-cell fill or a terminal capture).
    /// The frame stays parked.
    #[error("typed site {site} is delivered by {delivery:?}, not by a host-built answer")]
    AnswerDelivery { site: u64, delivery: SiteDelivery },
    /// The answer names a constructor outside the site's declared family
    /// closure (the wrong family, or a constructor the type's rows do not
    /// admit). The frame stays parked.
    #[error("reply {site} does not admit constructor {host_id:?} in its answer")]
    AnswerConstructor {
        site: ReplyTarget,
        host_id: DataConId,
    },
    /// The answer's shape does not match the site's type evidence (a literal
    /// where a constructor is required, a field count or scalar width
    /// mismatch, a byte array, or excessive nesting). The frame stays parked.
    #[error("reply {site} rejects the answer: {detail}")]
    AnswerShape {
        site: ReplyTarget,
        detail: &'static str,
    },
    /// The answer reaches a type the host cannot construct. The frame stays
    /// parked.
    #[error("reply {site} has an unconstructible answer type: {reason}")]
    AnswerUnconstructible { site: ReplyTarget, reason: String },
    #[error("reply {site} type evidence refused: {source}")]
    AnswerTypeEvidence {
        site: ReplyTarget,
        #[source]
        source: TypeGraphError,
    },
    /// Structural conversion failed at the dispatch/resume boundary. The
    /// frame stays parked and no answer root is published.
    #[error("reply {site} rejects its structural answer: {source}")]
    AnswerRejected {
        site: ReplyTarget,
        #[source]
        source: tidepool_bridge::BridgeError,
    },
    /// A resumed handle (bare or framed) is not live in this engine's
    /// ledger: unknown, released, or minted under a different engine. The
    /// frame stays parked.
    #[error("resume delivered a handle that is not live in this engine's ledger")]
    UnknownHandle,
    #[error("resume requires a lifted answer, received {actual:?}")]
    AnswerRepresentation { actual: RuntimeRep },
    /// A host value could not be streamed into the authenticated managed
    /// builder. No binding root is published on this path.
    #[error("host value mount rejected: {detail}")]
    HostMount { detail: String },
    /// [`PreparedEngine::run_rooted_entry`]/[`PreparedEngine::run_rooted_application`]
    /// found no installed program that both owns the rooted closure's object
    /// and admits the generic apply roots, and no OTHER installed program
    /// admits them either. Every executable template emits
    /// `__applyEntry`/`__applyValue` beside its settled scaffold, so this is
    /// only reachable before any program is installed.
    #[error(
        "no installed program admits `{}`/`{}`, so a rooted apply has nowhere to run",
        PREPARED_APPLY_ENTRY_TARGET,
        PREPARED_APPLY_VALUE_TARGET
    )]
    NoHostingProgram,
    /// The program hosting a rooted apply admits no `__applyEntry` entry.
    /// Every executable template defines it beside its settled scaffold;
    /// this is a stale or foreign artifact, never a user error.
    #[error("program {program:?} admits no `{entry}` entry, so a rooted apply cannot run")]
    NoApplyEntryEntry {
        program: ProgramId,
        entry: &'static str,
    },
    /// The program hosting a rooted apply admits no `__applyValue` entry. See
    /// [`Self::NoApplyEntryEntry`].
    #[error("program {program:?} admits no `{entry}` entry, so a rooted apply cannot run")]
    NoApplyValueEntry {
        program: ProgramId,
        entry: &'static str,
    },
}

impl From<DemandError> for PreparedRuntimeError {
    fn from(error: DemandError) -> Self {
        match error {
            DemandError::MissingPreparedNativeImage => Self::MissingPreparedNativeImage,
            error => Self::Demand(error),
        }
    }
}

impl PreparedRuntimeError {
    #[must_use]
    pub fn stage(&self) -> PreparedFailureStage {
        match self {
            Self::Parse(_)
            | Self::Link(_)
            | Self::Compile(_)
            | Self::Install(_)
            | Self::Demand(_)
            | Self::MissingPreparedNativeImage
            | Self::TypeEvidence(_)
            | Self::MissingCertifiedOwner(_)
            | Self::CertifiedPackageOwnerUnavailable { .. }
            | Self::MissingRetainedCertifiedOwner { .. }
            | Self::CertifiedTargetOwners
            | Self::ConflictingSettledConstructors
            | Self::UnreachableCertifiedGroup
            | Self::SourceScopeAdmission
            | Self::AmbiguousSourceInstance(_)
            | Self::AmbiguousSourceGroup { .. }
            | Self::InvalidCertifiedSourceOwner(_)
            | Self::DuplicateSite { .. }
            | Self::SiteConflict { .. }
            | Self::ConstructorReplyConflict { .. }
            | Self::DuplicateConstructorReply { .. } => PreparedFailureStage::Install,
            Self::Cancelled
            | Self::Run(_)
            | Self::UnknownBinding(_)
            | Self::CrossRealmArgument { .. }
            | Self::UnsettledEntry { .. }
            | Self::MachineNotInstalled
            | Self::ProjectionShape { .. }
            | Self::UnknownSite { .. }
            | Self::UntypedRequest { .. }
            | Self::MissingReplyEvidence { .. }
            | Self::MalformedRequestSite { .. }
            | Self::MissingReplySiteRow { .. }
            | Self::UnhandledRequest
            | Self::DeferredRequiresAsyncHost
            | Self::Handler { .. }
            | Self::NoResumeEntry { .. }
            | Self::AnswerDelivery { .. }
            | Self::AnswerConstructor { .. }
            | Self::AnswerShape { .. }
            | Self::AnswerUnconstructible { .. }
            | Self::AnswerTypeEvidence { .. }
            | Self::RequestScopeTypeEvidence { .. }
            | Self::AnswerRejected { .. }
            | Self::UnknownHandle
            | Self::AnswerRepresentation { .. }
            | Self::HostMount { .. }
            | Self::NoHostingProgram
            | Self::NoApplyEntryEntry { .. }
            | Self::NoApplyValueEntry { .. } => PreparedFailureStage::Run,
        }
    }

    #[must_use]
    pub fn kind(&self) -> PreparedFailureKind {
        match self {
            Self::Parse(_)
            | Self::Link(_)
            | Self::Demand(_)
            | Self::MissingCertifiedOwner(_)
            | Self::CertifiedPackageOwnerUnavailable { .. }
            | Self::MissingRetainedCertifiedOwner { .. }
            | Self::CertifiedTargetOwners
            | Self::ConflictingSettledConstructors
            | Self::UnreachableCertifiedGroup
            | Self::SourceScopeAdmission
            | Self::AmbiguousSourceInstance(_)
            | Self::AmbiguousSourceGroup { .. }
            | Self::InvalidCertifiedSourceOwner(_)
            | Self::UnknownBinding(_)
            | Self::UnsettledEntry { .. }
            | Self::MachineNotInstalled
            | Self::DuplicateSite { .. }
            | Self::ProjectionShape { .. }
            | Self::SiteConflict { .. }
            | Self::ConstructorReplyConflict { .. }
            | Self::DuplicateConstructorReply { .. }
            | Self::UnknownSite { .. }
            | Self::UntypedRequest { .. }
            | Self::MissingReplyEvidence { .. }
            | Self::MalformedRequestSite { .. }
            | Self::MissingReplySiteRow { .. }
            | Self::UnhandledRequest
            | Self::DeferredRequiresAsyncHost
            | Self::NoResumeEntry { .. }
            | Self::AnswerDelivery { .. }
            | Self::AnswerConstructor { .. }
            | Self::AnswerShape { .. }
            | Self::AnswerUnconstructible { .. }
            | Self::AnswerRejected { .. }
            | Self::UnknownHandle
            | Self::AnswerRepresentation { .. }
            | Self::HostMount { .. }
            | Self::NoHostingProgram
            | Self::NoApplyEntryEntry { .. }
            | Self::NoApplyValueEntry { .. }
            | Self::CrossRealmArgument { .. } => PreparedFailureKind::Rejected,
            Self::TypeEvidence(source)
            | Self::AnswerTypeEvidence { source, .. }
            | Self::RequestScopeTypeEvidence { source, .. } => match source {
                TypeGraphError::TraversalWork | TypeGraphError::Limit(_) => {
                    PreparedFailureKind::Rejected
                }
                _ => PreparedFailureKind::Integrity,
            },
            Self::MissingPreparedNativeImage => PreparedFailureKind::Integrity,
            Self::Cancelled => PreparedFailureKind::Cancelled,
            Self::Compile(_) => PreparedFailureKind::Rejected,
            // A handler fault is this turn's own failure, so the machine stays reusable.
            Self::Handler { .. } => PreparedFailureKind::Language,
            Self::Install(error) | Self::Run(error) => match error {
                ExecutionError::MissingEntry(_)
                | ExecutionError::Unsupported(_)
                | ExecutionError::Arguments { .. }
                | ExecutionError::ArgumentRepresentation { .. }
                | ExecutionError::UnknownPreparedHandle
                | ExecutionError::ImportShape { .. }
                | ExecutionError::BatchSourceContract(_)
                | ExecutionError::BatchImportContract(_)
                | ExecutionError::DescriptorShape { .. }
                | ExecutionError::HostIdConflict { .. }
                | ExecutionError::ForeignExternals
                | ExecutionError::BorrowedParcelCode
                // An evacuation refusal leaves both machines as they were.
                | ExecutionError::Evacuation(_)
                | ExecutionError::UnknownProgram(_)
                | ExecutionError::UnknownContinuation(_)
                | ExecutionError::Answer(_)
                | ExecutionError::Invariant(_)
                | ExecutionError::NotQuiescent => PreparedFailureKind::Rejected,
                ExecutionError::Runtime(failure) => {
                    if failure.disposition == MachineDisposition::Unavailable {
                        PreparedFailureKind::Integrity
                    } else if matches!(
                        failure.cause,
                        tidepool_codegen::host_fns::RuntimeError::Cancelled
                    ) {
                        PreparedFailureKind::Cancelled
                    } else {
                        PreparedFailureKind::Language
                    }
                }
                ExecutionError::Observation(_) | ExecutionError::Static(_) => {
                    PreparedFailureKind::Language
                }
            },
        }
    }
}

impl From<LinkError> for PreparedRuntimeError {
    fn from(error: LinkError) -> Self {
        Self::Link(Box::new(error))
    }
}

/// The facts about an installed program the session still needs after the
/// machine has taken its code: the declared entry and, per top-level binding,
/// the identity and entry signature an importer links against.
struct ProgramFacts {
    entry: Option<ValueId>,
    definitions: Arc<DefinitionFacts>,
    /// The `Tidepool.Internal.Resume.Settled` constructors this program
    /// declares, when its entry is a turn's settled scaffold.
    settled: Option<SettledIds>,
    /// The turn's admitted resume entry (`__resume q x = settle (resumeLifted
    /// q x)`, beside the entry in its module), when the artifact retained it.
    /// A suspension of a program without one is refused before parking.
    resume: Option<ValueId>,
    /// The turn's admitted generic apply entries (`__applyEntry f n = settle
    /// (f (I# n))`, `__applyValue f x = settle (f x)`, beside the entry in
    /// its module), when the artifact retained them. Looked up by
    /// [`PreparedEngine::run_rooted_entry`]/
    /// [`PreparedEngine::run_rooted_application`] to apply a rooted closure
    /// without compiling a fresh fragment for it.
    apply_entry: Option<ValueId>,
    apply_value: Option<ValueId>,
}

impl std::ops::Deref for ProgramFacts {
    type Target = DefinitionFacts;

    fn deref(&self) -> &Self::Target {
        &self.definitions
    }
}

/// What installing one program adds to the machine-owned evidence indexes.
struct EvidencePlan {
    sites: Vec<(u64, usize)>,
    constructor_replies: Vec<(DataConId, usize)>,
}

struct AdmittedProgramFacts {
    facts: ProgramFacts,
    plan: EvidencePlan,
}

/// Everything [`PreparedEngine::snapshot_install`] produces under a machine
/// checkout for an off-checkout compile: owned data with no reference to
/// the machine or its checkout. `values`/`imports` are kept alongside
/// `linked` (which consumes the resolved imports into linkage order) so
/// [`PreparedEngine::revalidate_and_install`] can compare a fresh resolve
/// against exactly what this snapshot resolved, by value.
pub(crate) struct InstallSnapshot {
    linked: tidepool_repr::execution_schema::LinkedProgram,
    values: MachineImports,
    imports: ImportBindings,
    facts: ProgramFacts,
    exports: Vec<(SymbolIdentity, ValueId, Option<Signature>)>,
    compile: tidepool_codegen::prepared_program::PreparedCompileSnapshot,
    /// This engine's registry, carried into the off-checkout step so a miss
    /// there can still be inserted for every other machine sharing it.
    /// `None` when the engine has none (today's behavior: always compile).
    registry: Option<Arc<ImageRegistry>>,
    /// Set when [`PreparedEngine::snapshot_install`] already found this
    /// content in the registry: [`PreparedEngine::compile_off_checkout`]
    /// then compiles nothing and just returns this `Arc`.
    precompiled: Option<Arc<CompiledProgram>>,
}

/// Exact target definitions and their shared native image, paired off
/// checkout. The target's declared entry is preserved when its source-group
/// closure installs in the same machine transaction.
pub(crate) struct CertifiedTargetImage {
    prepared: PreparedProgram,
    image: Arc<CompiledProgram>,
    package_interfaces: CertifiedTargetPackageInterfaces,
    package_literals: BTreeMap<SymbolIdentity, PackageLiteral>,
    source_plan: Option<super::persistent::ResolvedSourceDomainPlan>,
}

/// Strong custody of native code and its literal storage, with no installed state.
pub(crate) struct NativeImageBundle {
    registry: Arc<ImageRegistry>,
    images: BTreeMap<usize, Arc<CompiledProgram>>,
    target: usize,
}

impl std::fmt::Debug for NativeImageBundle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("NativeImageBundle")
            .field("images", &self.images.len())
            .field(
                "target",
                &self
                    .images
                    .get(&self.target)
                    .map(|image| image.image_instance_id()),
            )
            .finish()
    }
}

impl NativeImageBundle {
    fn holds(&self, image: &Arc<CompiledProgram>) -> bool {
        // The strong owner prevents address reuse while this index is live.
        // Registry equality remains the native selection authority.
        self.images
            .get(&(Arc::as_ptr(image) as usize))
            .is_some_and(|held| Arc::ptr_eq(held, image))
    }

    pub(crate) fn prepare_activation_renderer(
        compiled: &super::turn::CompiledTurn,
        registry: &Arc<ImageRegistry>,
    ) -> Result<Self, PreparedRuntimeError> {
        use tidepool_codegen::prepared_program::{PendingGroupInventory, SourceGroupOutline};
        use tidepool_repr::execution_schema::CertifiedGroupCode;
        use tidepool_toolchain::certified_products::PendingImportOwner;
        let certification = compiled
            .certification
            .as_ref()
            .ok_or(PreparedRuntimeError::CertifiedTargetOwners)?;
        let proof = certification
            .checked_activation_preview()
            .ok_or(PreparedRuntimeError::CertifiedTargetOwners)?;
        if !proof.matches_target(compiled.prepared()) {
            return Err(PreparedRuntimeError::CertifiedTargetOwners);
        }
        proof
            .validate_table(compiled.table())
            .map_err(|_| PreparedRuntimeError::CertifiedTargetOwners)?;
        proof
            .validate_yield_sites(&compiled.asks)
            .map_err(|_| PreparedRuntimeError::CertifiedTargetOwners)?;
        let target_literals =
            immutable_literal_owners(compiled.prepared().globals(), &certification.target_owners)?;
        let outlines = certification
            .groups
            .iter()
            .map(|pending| {
                immutable_literal_owners(pending.group().globals(), pending.imports())?;
                let imports = pending
                    .imports()
                    .iter()
                    .filter_map(|owner| match owner {
                        PendingImportOwner::Source { owner, binder, .. } => Some(SourceBinder {
                            version: owner.module_version.clone(),
                            binder: binder.clone(),
                        }),
                        _ => None,
                    })
                    .collect::<Vec<_>>();
                SourceGroupOutline::from_projected(
                    pending.owner().clone(),
                    pending.group(),
                    imports,
                )
                .map_err(Into::into)
            })
            .collect::<Result<Vec<_>, PreparedRuntimeError>>()?;
        let roots = certification
            .target_owners
            .iter()
            .filter_map(|owner| match owner {
                PendingImportOwner::Source { owner, binder, .. } => Some(SourceBinder {
                    version: owner.module_version.clone(),
                    binder: binder.clone(),
                }),
                _ => None,
            });
        // Close only source edges; historical retained contracts do not select a
        // group or require a live machine merely to compile its definitions.
        let selected = PendingGroupInventory::new(outlines)?.seal_with_inherited(
            roots,
            &BTreeMap::new(),
            &HashMap::new(),
        )?;
        let groups = selected
            .new_group_indices()
            .iter()
            .map(|index| {
                let pending = &certification.groups[*index];
                CertifiedGroupCode::admit(pending.owner().clone(), pending.group().clone())
                    .map(|code| (code, pending.imports()))
            })
            .collect::<Result<Vec<_>, _>>()?;
        // Native custody must correspond to the exact original owner/ordinal,
        // not just a binder with the same module-version spelling.
        for owner in certification
            .target_owners
            .iter()
            .chain(groups.iter().flat_map(|(_, imports)| imports.iter()))
        {
            if let PendingImportOwner::Source {
                owner,
                original_ordinal,
                binder,
            } = owner
            {
                if !groups.iter().any(|(code, _)| {
                    code.owner() == owner
                        && code.original_ordinal() == *original_ordinal
                        && code.binders().contains(binder)
                }) {
                    return Err(PreparedRuntimeError::InvalidCertifiedSourceOwner(
                        SourceBinder {
                            version: owner.module_version.clone(),
                            binder: binder.clone(),
                        },
                    ));
                }
            }
        }
        let mut images = Vec::with_capacity(groups.len() + 1);
        let mut literals = BTreeMap::new();
        let mut byte_images = HashMap::new();
        for (index, (group, imports)) in groups.iter().enumerate() {
            let definitions = group.definitions();
            if imports.is_empty()
                && definitions.globals().is_empty()
                && matches!(definitions.bindings(), [Group::NonRecursive(top)] if matches!(top.binding.rhs, HeapRhs::Bytes(_)))
            {
                let image = CompiledProgram::prepare_group_code(
                    group,
                    &[],
                    &BTreeMap::new(),
                    &BTreeMap::new(),
                    registry,
                )
                .map_err(PreparedRuntimeError::Compile)?;
                for (binder, literal) in image.source_literals() {
                    if literals
                        .insert(binder.clone(), literal.clone())
                        .is_some_and(|prior| prior != literal)
                    {
                        return Err(DemandError::DuplicateBinder(binder).into());
                    }
                }
                byte_images.insert(index, image);
            }
        }
        let target = CompiledProgram::prepare_target_code(
            compiled.prepared(),
            &target_literals,
            &literals,
            registry,
        )
        .map_err(PreparedRuntimeError::Compile)?;
        let packages = if certification
            .package_interfaces
            .matches_target(compiled.prepared())
        {
            target.package_literals(|unit, module| {
                certification
                    .package_interfaces
                    .interface_digest(unit, module)
            })
        } else {
            BTreeMap::new()
        };
        let target_key = Arc::as_ptr(&target) as usize;
        images.push(target);
        for (index, (group, imports)) in groups.iter().enumerate() {
            let image = match byte_images.remove(&index) {
                Some(image) => image,
                None => CompiledProgram::prepare_group_code(
                    group,
                    &immutable_literal_owners(group.definitions().globals(), imports)?,
                    &packages,
                    &literals,
                    registry,
                )
                .map_err(PreparedRuntimeError::Compile)?,
            };
            images.push(image);
        }
        tracing::info!(target: "tidepool_runtime::activation_renderer", image_instance = images[0].image_instance_id(), images = images.len(), outcome = "prepared", "activation renderer native custody");
        for (index, image) in images.iter().enumerate() {
            tracing::info!(target: "tidepool_runtime::activation_renderer", bundle_target = images[0].image_instance_id(), image_instance = image.image_instance_id(), role = if index == 0 { "target" } else { "source" }, literal_producer = !image.source_literals().is_empty(), outcome = "native_image", "activation renderer native custody");
        }
        Ok(Self {
            registry: registry.clone(),
            images: images
                .into_iter()
                .map(|image| (Arc::as_ptr(&image) as usize, image))
                .collect(),
            target: target_key,
        })
    }

    #[cfg(test)]
    pub(super) fn omitting_image(&self, key: usize) -> Self {
        Self {
            registry: self.registry.clone(),
            images: self
                .images
                .iter()
                .filter(|(address, _)| **address != key)
                .map(|(address, image)| (*address, image.clone()))
                .collect(),
            target: self.target,
        }
    }

    #[cfg(test)]
    pub(super) fn omitting_target_image(&self) -> Self {
        self.omitting_image(self.target)
    }

    #[cfg(test)]
    pub(super) fn image_owners(&self) -> impl ExactSizeIterator<Item = &Arc<CompiledProgram>> {
        self.images.values()
    }

    pub(super) fn image_instances(&self) -> impl Iterator<Item = u64> + '_ {
        self.images.values().map(|image| image.image_instance_id())
    }
}

fn immutable_literal_owners(
    globals: &[tidepool_repr::execution_schema::GlobalDecl],
    pending: &[tidepool_toolchain::certified_products::PendingImportOwner],
) -> Result<Vec<Option<tidepool_codegen::prepared_program::LiteralOwner>>, PreparedRuntimeError> {
    use tidepool_codegen::prepared_program::LiteralOwner;
    use tidepool_toolchain::certified_products::PendingImportOwner;
    if globals.len() != pending.len() {
        return Err(PreparedRuntimeError::CertifiedTargetOwners);
    }
    globals
        .iter()
        .zip(pending)
        .map(|(global, owner)| {
            let literal = match owner {
                PendingImportOwner::Source { owner, binder, .. }
                    if binder == &global.identity
                        && global.required_generation.is_none()
                        && binder.unit == owner.unit
                        && binder.module == owner.module =>
                {
                    Some(LiteralOwner::Source {
                        version: owner.module_version.clone(),
                        binder: binder.clone(),
                    })
                }
                PendingImportOwner::Package {
                    unit,
                    module,
                    binder,
                    interface_digest,
                } if binder == &global.identity
                    && &binder.unit == unit
                    && &binder.module == module
                    && global.required_generation.is_none() =>
                {
                    Some(LiteralOwner::Package {
                        unit: unit.clone(),
                        module: module.clone(),
                        binder: binder.clone(),
                        interface_digest: *interface_digest,
                    })
                }
                PendingImportOwner::Retained {
                    identity,
                    generation,
                } if identity == &global.identity
                    && global.required_generation == Some(*generation) =>
                {
                    None
                }
                PendingImportOwner::RetainedPackage {
                    unit,
                    module,
                    binder,
                    generation,
                    ..
                } if binder == &global.identity
                    && &binder.unit == unit
                    && &binder.module == module
                    && global.required_generation == Some(*generation) =>
                {
                    None
                }
                _ => return Err(PreparedRuntimeError::CertifiedTargetOwners),
            };
            Ok(literal)
        })
        .collect()
}

/// An immutable source-produced entry and the native images needed to install it.
/// This owns no heap, lexical scope, dispatcher, or actor resource grants.
pub struct PreparedSourceEntry {
    compiled: Arc<super::turn::CompiledTurn>,
    registry: Arc<ImageRegistry>,
    _images: Vec<Arc<CompiledProgram>>,
}

impl PreparedSourceEntry {
    /// Validate original compiler custody and prepare its entire executable closure.
    /// Each consumer subsequently installs these definitions into fresh actor state.
    pub fn prepare(
        compiled: Arc<super::turn::CompiledTurn>,
        registry: Arc<ImageRegistry>,
    ) -> Result<Self, super::resident::ResidentError> {
        let certification = compiled
            .certification
            .as_ref()
            .ok_or(super::resident::ResidentError::UnsealedStartupEntry)?;
        let proof = certification
            .original_compile_input
            .as_ref()
            .ok_or(super::resident::ResidentError::UnsealedStartupEntry)?;
        if !matches!(certification.purpose(), super::turn::TurnPurpose::Ordinary)
            || !proof.matches_bundle(
                &compiled.prepared(),
                &certification.groups,
                &certification.target_owners,
                &certification.package_interfaces,
                &compiled.table(),
                &compiled.asks,
            )
        {
            return Err(super::resident::ResidentError::UnsealedStartupEntry);
        }
        // Resolution uses the existing source-group owner with an empty lexical
        // environment. Any retained notebook dependency is therefore refused.
        let state = super::persistent::PersistentSession::new(None, 0);
        let resolved = state.resolve_certification_in(
            tidepool_codegen::scope::ScopeId::ROOT,
            &compiled.prepared(),
            certification,
        )?;
        let (target, demanded) = CertifiedTargetImage::compile_scoped(
            compiled.prepared().as_ref().clone(),
            &resolved,
            &registry,
        )?;
        let mut images = Vec::with_capacity(demanded.len() + 1);
        images.push(target.image);
        images.extend(demanded.iter().map(|image| Arc::clone(image.image())));
        Ok(Self {
            compiled,
            registry,
            _images: images,
        })
    }

    pub fn compiled(&self) -> &Arc<super::turn::CompiledTurn> {
        &self.compiled
    }

    pub fn image_registry(&self) -> &Arc<ImageRegistry> {
        &self.registry
    }
}

fn target_owners_match(prepared: &PreparedProgram, owners: &[ImportOwner]) -> bool {
    prepared.globals().len() == owners.len()
        && prepared
            .globals()
            .iter()
            .zip(owners)
            .all(|(declaration, owner)| match owner {
                ImportOwner::Source { binder, .. } => {
                    binder == &declaration.identity && declaration.required_generation.is_none()
                }
                ImportOwner::Retained { generation, .. } => {
                    declaration.required_generation == Some(*generation)
                }
                ImportOwner::CodeExport {
                    binder, generation, ..
                } => {
                    binder == &declaration.identity
                        && declaration.required_generation == Some(*generation)
                }
                ImportOwner::Package {
                    unit,
                    module,
                    binder,
                    ..
                } => {
                    binder == &declaration.identity
                        && &binder.unit == unit
                        && &binder.module == module
                        && declaration.required_generation.is_none()
                }
            })
}

/// Ready installation can only consume the bundle's prepared custody. Ordinary
/// compilation retains its existing producer route through the same selectors.
#[derive(Clone, Copy)]
enum NativeImageAcquisition<'a> {
    Compile(&'a ImageRegistry),
    Ready(&'a NativeImageBundle),
}

impl NativeImageAcquisition<'_> {
    fn group(
        self,
        group: CertifiedGroup,
        packages: &BTreeMap<SymbolIdentity, PackageLiteral>,
        sources: &BTreeMap<SourceBinder, SourceLiteral>,
    ) -> Result<DemandedImage, DemandError> {
        match self {
            Self::Compile(registry) => {
                DemandedImage::compile_with_literals(group, registry, packages, sources)
            }
            Self::Ready(bundle) => {
                let image = DemandedImage::lookup_with_literals(
                    group,
                    &bundle.registry,
                    packages,
                    sources,
                )?;
                if !bundle.holds(image.image()) {
                    return Err(DemandError::MissingPreparedNativeImage);
                }
                Ok(image)
            }
        }
    }

    fn target(
        self,
        prepared: &PreparedProgram,
        owners: &[ImportOwner],
        sources: &BTreeMap<SourceBinder, SourceLiteral>,
    ) -> Result<Arc<CompiledProgram>, PreparedRuntimeError> {
        match self {
            Self::Compile(registry) => CompiledProgram::compile_prepared_with_source_literals(
                prepared, owners, sources, registry,
            )
            .map_err(PreparedRuntimeError::Compile),
            Self::Ready(bundle) => {
                let image = CompiledProgram::lookup_prepared_with_source_literals(
                    prepared,
                    owners,
                    sources,
                    &bundle.registry,
                )
                .map_err(PreparedRuntimeError::Compile)?
                .filter(|image| bundle.holds(image))
                .ok_or(PreparedRuntimeError::MissingPreparedNativeImage)?;
                Ok(image)
            }
        }
    }
}

impl CertifiedTargetImage {
    #[cfg(test)]
    pub(crate) fn compile(
        prepared: PreparedProgram,
        registry: &ImageRegistry,
    ) -> Result<Self, CompileError> {
        Self::compile_certified(
            prepared,
            registry,
            CertifiedTargetPackageInterfaces::default(),
        )
    }

    #[cfg(test)]
    pub(crate) fn compile_certified(
        prepared: PreparedProgram,
        registry: &ImageRegistry,
        package_interfaces: CertifiedTargetPackageInterfaces,
    ) -> Result<Self, CompileError> {
        let image = registry.get_or_compile_prepared(&prepared, || {
            CompiledProgram::compile_prepared_definitions(&prepared).map(Arc::new)
        })?;
        let package_literals = if package_interfaces.matches_target(&prepared) {
            image.package_literals(|unit, module| package_interfaces.interface_digest(unit, module))
        } else {
            BTreeMap::new()
        };
        Ok(Self {
            prepared,
            image,
            package_interfaces,
            package_literals,
            source_plan: None,
        })
    }

    pub(crate) fn compile_scoped(
        prepared: PreparedProgram,
        resolved: &super::persistent::ResolvedCertifiedTurn,
        registry: &ImageRegistry,
    ) -> Result<(Self, Vec<DemandedImage>), PreparedRuntimeError> {
        Self::acquire_scoped(
            prepared,
            resolved,
            NativeImageAcquisition::Compile(registry),
        )
    }

    pub(crate) fn lookup_scoped(
        prepared: PreparedProgram,
        resolved: &super::persistent::ResolvedCertifiedTurn,
        bundle: &NativeImageBundle,
    ) -> Result<(Self, Vec<DemandedImage>), PreparedRuntimeError> {
        Self::acquire_scoped(prepared, resolved, NativeImageAcquisition::Ready(bundle))
    }

    fn acquire_scoped(
        prepared: PreparedProgram,
        resolved: &super::persistent::ResolvedCertifiedTurn,
        images: NativeImageAcquisition<'_>,
    ) -> Result<(Self, Vec<DemandedImage>), PreparedRuntimeError> {
        let groups = resolved
            .groups
            .iter()
            .map(|group| group.original().clone())
            .collect();
        let (target, compiled) = Self::acquire_originals(
            prepared,
            &resolved.target_owners,
            images,
            resolved.package_interfaces.clone(),
            groups,
        )?;
        let demanded = compiled
            .into_iter()
            .zip(resolved.groups.iter().cloned())
            .map(|(image, group)| {
                ScopedDemandedImage::admit(image, group).map(ScopedDemandedImage::into_image)
            })
            .collect::<Result<Vec<_>, _>>()?;
        Ok((
            target.with_source_plan(resolved.source_plan.clone()),
            demanded,
        ))
    }

    #[cfg(test)]
    fn compile_originals(
        prepared: PreparedProgram,
        owners: &[ImportOwner],
        registry: &ImageRegistry,
        package_interfaces: CertifiedTargetPackageInterfaces,
        groups: Vec<CertifiedGroup>,
    ) -> Result<(Self, Vec<DemandedImage>), PreparedRuntimeError> {
        Self::acquire_originals(
            prepared,
            owners,
            NativeImageAcquisition::Compile(registry),
            package_interfaces,
            groups,
        )
    }

    fn acquire_originals(
        prepared: PreparedProgram,
        owners: &[ImportOwner],
        images: NativeImageAcquisition<'_>,
        package_interfaces: CertifiedTargetPackageInterfaces,
        groups: Vec<CertifiedGroup>,
    ) -> Result<(Self, Vec<DemandedImage>), PreparedRuntimeError> {
        if !target_owners_match(&prepared, owners) {
            return Err(PreparedRuntimeError::CertifiedTargetOwners);
        }
        let (compiled, source_literals) = Self::compile_literal_producers(&groups, images)?;
        let image = images.target(&prepared, owners, &source_literals)?;
        let package_literals = if package_interfaces.matches_target(&prepared) {
            image.package_literals(|unit, module| package_interfaces.interface_digest(unit, module))
        } else {
            BTreeMap::new()
        };
        let target = Self {
            prepared,
            image,
            package_interfaces,
            package_literals,
            source_plan: None,
        };
        let demanded = target.finish_demanded(groups, compiled, &source_literals, images)?;
        Ok((target, demanded))
    }

    fn compile_literal_producers(
        groups: &[CertifiedGroup],
        images: NativeImageAcquisition<'_>,
    ) -> Result<
        (
            Vec<Option<DemandedImage>>,
            BTreeMap<SourceBinder, SourceLiteral>,
        ),
        DemandError,
    > {
        let mut compiled = (0..groups.len()).map(|_| None).collect::<Vec<_>>();
        let mut source_literals = BTreeMap::new();
        // Original string-literal groups have no imports. Compile those
        // producers first without changing the sealed group's batch position.
        for (index, group) in groups.iter().enumerate() {
            if !group.imports().is_empty() || !group.definitions().globals().is_empty() {
                continue;
            }
            let [Group::NonRecursive(top)] = group.definitions().bindings() else {
                continue;
            };
            if !matches!(top.binding.rhs, HeapRhs::Bytes(_)) {
                continue;
            }
            let image = images.group(group.clone(), &BTreeMap::new(), &BTreeMap::new())?;
            for (binder, literal) in image.source_literals() {
                if let Some(previous) = source_literals.get(&binder) {
                    if previous != &literal {
                        return Err(DemandError::DuplicateBinder(binder));
                    }
                } else {
                    source_literals.insert(binder, literal);
                }
            }
            compiled[index] = Some(image);
        }
        Ok((compiled, source_literals))
    }

    fn finish_demanded(
        &self,
        groups: Vec<CertifiedGroup>,
        compiled: Vec<Option<DemandedImage>>,
        source_literals: &BTreeMap<SourceBinder, SourceLiteral>,
        images: NativeImageAcquisition<'_>,
    ) -> Result<Vec<DemandedImage>, DemandError> {
        groups
            .into_iter()
            .zip(compiled)
            .map(|(group, compiled)| match compiled {
                Some(image) => Ok(image),
                None => images.group(group, &self.package_literals, source_literals),
            })
            .collect()
    }

    #[cfg(test)]
    pub(crate) fn compile_demanded(
        &self,
        groups: impl IntoIterator<Item = CertifiedGroup>,
        registry: &ImageRegistry,
    ) -> Result<Vec<DemandedImage>, DemandError> {
        let groups = groups.into_iter().collect::<Vec<_>>();
        tidepool_codegen::prepared_program::GroupInventory::new(&groups)?;
        let (compiled, literals) =
            Self::compile_literal_producers(&groups, NativeImageAcquisition::Compile(registry))?;
        self.finish_demanded(
            groups,
            compiled,
            &literals,
            NativeImageAcquisition::Compile(registry),
        )
    }

    pub(crate) fn with_source_plan(
        mut self,
        plan: super::persistent::ResolvedSourceDomainPlan,
    ) -> Self {
        self.source_plan = Some(plan);
        self
    }
    pub(crate) fn source_plan(&self) -> Option<&super::persistent::ResolvedSourceDomainPlan> {
        self.source_plan.as_ref()
    }
    fn qualified_source(&self, source: &SourceBinder) -> Result<ScopedSourceBinder, DemandError> {
        match &self.source_plan {
            Some(plan) => plan
                .target
                .get(source)
                .cloned()
                .ok_or_else(|| DemandError::MissingSource(source.clone())),
            None => Ok(ScopedSourceBinder {
                domain: SourceInstanceDomain::single(),
                source: source.clone(),
            }),
        }
    }
    #[cfg(test)]
    pub(crate) fn compile_scoped_demanded(
        &self,
        groups: impl IntoIterator<Item = ScopedCertifiedGroup>,
        registry: &ImageRegistry,
    ) -> Result<Vec<DemandedImage>, DemandError> {
        let groups: Vec<_> = groups.into_iter().collect();
        let originals = groups
            .iter()
            .map(|group| group.original().clone())
            .collect::<Vec<_>>();
        let (compiled, literals) =
            Self::compile_literal_producers(&originals, NativeImageAcquisition::Compile(registry))?;
        let compiled = self.finish_demanded(
            originals,
            compiled,
            &literals,
            NativeImageAcquisition::Compile(registry),
        )?;
        compiled
            .into_iter()
            .zip(groups)
            .map(|(image, group)| {
                ScopedDemandedImage::admit(image, group).map(ScopedDemandedImage::into_image)
            })
            .collect()
    }

    pub(crate) fn prepared(&self) -> &PreparedProgram {
        &self.prepared
    }

    pub(crate) fn globals(&self) -> &[tidepool_repr::execution_schema::GlobalDecl] {
        self.prepared.globals()
    }
}

pub(crate) struct CertifiedTurnInstall {
    pub target: ProgramId,
    pub groups: Vec<ProgramId>,
    pub leases: Vec<SourceInstanceLease>,
    pub domain_leases: Vec<SourceInstanceAttachment>,
    admitted: Vec<AdmittedProgramFacts>,
    package_updates: BTreeMap<SymbolIdentity, [u8; 32]>,
    exports: BTreeMap<SymbolIdentity, CodeExport>,
}

/// Which installed program's site table is authoritative for one site id.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct SiteWitness {
    owner: ProgramId,
    row: usize,
}

/// Canonical compiler type graph for one request's input and complete reply.
/// This travels with the activation because its originating site belongs to
/// the requesting machine session, not the recipient's machine session.
#[derive(Clone, Debug)]
pub struct SiteTypeEvidence {
    types: Arc<TypeGraph>,
    constructors: Arc<[(SymbolIdentity, DataConId, SymbolIdentity)]>,
    input: TypeNodeId,
    answer: TypeNodeId,
    request_context: Option<Arc<tidepool_toolchain::declaration_join::ExactCompileContext>>,
}

impl PartialEq for SiteTypeEvidence {
    fn eq(&self, other: &Self) -> bool {
        self.types.evidence_eq(&other.types)
            && self.input == other.input
            && self.answer == other.answer
            && self.request_context == other.request_context
            && self
                .constructors
                .iter()
                .map(|(identity, _, _)| identity)
                .eq(other.constructors.iter().map(|(identity, _, _)| identity))
    }
}

impl Eq for SiteTypeEvidence {}

/// Authenticated request types and the helper mode for one compiler admission.
/// Keeping these together preserves authored reply authority when an admission
/// reconstructs its compile view from a retained request snapshot.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RequestCompileAnnotations {
    evidence: Arc<SiteTypeEvidence>,
    helper_recipe: tidepool_toolchain::declaration_join::RequestHelperRecipe,
}

/// Exact compiler inputs retained by the runtime admission owner. A selected
/// projection is independent of request type signatures; neither substitutes
/// for the other.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RuntimeCompileInputs {
    annotations: Option<RequestCompileAnnotations>,
    projections: Vec<Arc<super::CertifiedDeclarationProjection>>,
}

impl RuntimeCompileInputs {
    pub fn new(
        annotations: Option<RequestCompileAnnotations>,
        mut projections: Vec<Arc<super::CertifiedDeclarationProjection>>,
    ) -> Result<Self, crate::CompileError> {
        projections.sort_by(|left, right| left.module_name().cmp(right.module_name()));
        for pair in projections.windows(2) {
            if pair[0].module_name() == pair[1].module_name()
                && pair[0].context().semantic_sha256() != pair[1].context().semantic_sha256()
            {
                return Err(crate::CompileError::ExtractFailed(
                    "conflicting certified declaration projections".into(),
                ));
            }
        }
        projections.dedup_by(|left, right| left.module_name() == right.module_name());
        Ok(Self {
            annotations,
            projections,
        })
    }

    pub fn annotations(&self) -> Option<&RequestCompileAnnotations> {
        self.annotations.as_ref()
    }
    pub fn projections(&self) -> &[Arc<super::CertifiedDeclarationProjection>] {
        &self.projections
    }

    pub(super) fn frame_authorization(&self, frame: &mut impl FnMut(&[u8])) {
        frame(b"TidepoolRuntimeCompileInputs1");
        match &self.annotations {
            Some(annotations) => {
                frame(b"request");
                annotations.frame_authorization(frame);
            }
            None => frame(b"no-request"),
        }
        frame(&(self.projections.len() as u64).to_le_bytes());
        for projection in &self.projections {
            frame(projection.module_name().as_bytes());
            frame(&projection.context().semantic_sha256());
        }
    }
}

impl From<RequestCompileAnnotations> for RuntimeCompileInputs {
    fn from(annotations: RequestCompileAnnotations) -> Self {
        Self {
            annotations: Some(annotations),
            projections: Vec::new(),
        }
    }
}

impl RequestCompileAnnotations {
    pub fn new(
        evidence: Arc<SiteTypeEvidence>,
        helper_recipe: tidepool_toolchain::declaration_join::RequestHelperRecipe,
    ) -> Result<Self, crate::CompileError> {
        if evidence.request_type_signatures().is_none() {
            return Err(crate::CompileError::ExtractFailed(
                "request annotations require original compiler authentication".into(),
            ));
        }
        Ok(Self {
            evidence,
            helper_recipe,
        })
    }

    pub fn evidence(&self) -> &Arc<SiteTypeEvidence> {
        &self.evidence
    }

    pub fn helper_recipe(&self) -> tidepool_toolchain::declaration_join::RequestHelperRecipe {
        self.helper_recipe
    }

    pub(super) fn frame_authorization(&self, frame: &mut impl FnMut(&[u8])) {
        frame(&self.evidence.commitment());
        frame(self.helper_recipe.as_str().as_bytes());
    }
}

impl SiteTypeEvidence {
    pub fn request_type_signatures(
        &self,
    ) -> Option<&Arc<tidepool_toolchain::checked_cell::RequestTypeSignatures>> {
        self.request_context.as_ref()?.request_types()
    }

    pub(crate) fn authenticate_request_types(
        mut self,
        signatures: tidepool_toolchain::checked_cell::RequestTypeSignatures,
        declarations: &Arc<tidepool_toolchain::declaration_join::ExactDeclarationContext>,
    ) -> Result<Self, crate::CompileError> {
        self.request_context = Some(Arc::new(
            tidepool_toolchain::declaration_join::ExactCompileContext::new(declarations.clone())
                .with_request_types(Arc::new(signatures)),
        ));
        Ok(self)
    }

    pub(crate) fn compile_context(
        &self,
        declarations: Option<&Arc<tidepool_toolchain::declaration_join::ExactDeclarationContext>>,
    ) -> Result<Arc<tidepool_toolchain::declaration_join::ExactCompileContext>, crate::CompileError>
    {
        let request = self.request_context.as_ref().ok_or_else(|| {
            crate::CompileError::ExtractFailed(
                "request type evidence lacks its original compiler authentication".into(),
            )
        })?;
        let base = match declarations {
            Some(declarations) => (**declarations).clone(),
            None => tidepool_toolchain::declaration_join::ExactDeclarationContext::new(
                &[],
                &[],
                Vec::new(),
            )?,
        };
        let declarations = Arc::new(base.extend_interface_context(request.declarations())?);
        Ok(Arc::new(
            tidepool_toolchain::declaration_join::ExactCompileContext::new(declarations)
                .with_request_types(
                    request
                        .request_types()
                        .expect("authenticated request signatures")
                        .clone(),
                ),
        ))
    }

    /// Commit to the complete canonical site type graph with explicit framing
    /// and a versioned domain. This is evidence identity, not a type renderer.
    pub(crate) fn commitment(&self) -> [u8; 32] {
        fn frame(hash: &mut blake3::Hasher, bytes: &[u8]) {
            hash.update(&(bytes.len() as u64).to_le_bytes());
            hash.update(bytes);
        }

        fn count(hash: &mut blake3::Hasher, value: usize) {
            frame(hash, &(value as u64).to_le_bytes());
        }

        fn type_node_id(hash: &mut blake3::Hasher, id: TypeNodeId) {
            frame(hash, &id.0.to_le_bytes());
        }

        fn identity(hash: &mut blake3::Hasher, identity: &SymbolIdentity) {
            frame(hash, identity.unit.as_bytes());
            frame(hash, identity.module.as_bytes());
            frame(hash, identity.namespace.as_bytes());
            frame(hash, identity.occurrence.as_bytes());
            match identity.record_parent.as_deref() {
                Some(parent) => {
                    frame(hash, &[1]);
                    frame(hash, parent.as_bytes());
                }
                None => frame(hash, &[0]),
            }
        }

        let mut hash = blake3::Hasher::new();
        frame(&mut hash, b"Tidepool.SiteTypeEvidence");
        frame(&mut hash, b"v3");
        self.types.write_evidence(|bytes| {
            hash.update(bytes);
        });
        count(&mut hash, self.constructors.len());
        for (constructor, _, _) in self.constructors.iter() {
            identity(&mut hash, constructor);
        }
        type_node_id(&mut hash, self.input);
        type_node_id(&mut hash, self.answer);
        match &self.request_context {
            Some(request) => {
                frame(&mut hash, &[1]);
                frame(
                    &mut hash,
                    &request
                        .request_types()
                        .expect("authenticated signatures")
                        .metadata_digest(),
                );
                frame(&mut hash, &request.declarations().semantic_sha256());
            }
            None => frame(&mut hash, &[0]),
        }
        *hash.finalize().as_bytes()
    }
}

/// The two constructors a turn's settled layer is read by (`host_id` is the
/// bridge `DataConId`). A reduced target may inherit these exact identities
/// from an already installed source program.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct SettledIds {
    done: tidepool_repr::DataConId,
    suspended: tidepool_repr::DataConId,
}

impl SettledIds {
    const MODULE: &'static str = "Tidepool.Internal.Resume";

    /// Recover the settled constructor pair from exact admitted constructor
    /// declarations. A reduced entry can import the two constructors from
    /// separate source owners, so requiring one `ProgramFacts` value to carry
    /// both declarations loses valid evidence. Conflicting declarations for
    /// either qualified identity remain a refusal.
    fn from_facts<'a>(
        facts: impl IntoIterator<Item = &'a ProgramFacts>,
    ) -> Result<Option<Self>, PreparedRuntimeError> {
        Self::from_constructor_facts(facts.into_iter().flat_map(|facts| {
            ["Done", "Suspended"]
                .into_iter()
                .flat_map(move |occurrence| {
                    facts
                        .by_identity
                        .get(Self::MODULE)
                        .and_then(|module| module.get(occurrence))
                        .into_iter()
                        .flatten()
                        .map(move |index| &facts.constructors[*index])
                })
        }))
    }

    fn from_constructor_facts<'a>(
        constructors: impl IntoIterator<Item = &'a (SymbolIdentity, DataConId, SymbolIdentity)>,
    ) -> Result<Option<Self>, PreparedRuntimeError> {
        let mut done = None;
        let mut suspended = None;
        for (identity, host_id, family) in constructors {
            if identity.module != Self::MODULE {
                continue;
            }
            let slot = match identity.occurrence.as_str() {
                "Done" => &mut done,
                "Suspended" => &mut suspended,
                _ => continue,
            };
            if identity.namespace != "constructor"
                || identity.record_parent.is_some()
                || family.module != Self::MODULE
                || family.namespace != "type"
                || family.occurrence != "Settled"
                || family.record_parent.is_some()
                || family.unit != identity.unit
            {
                return Err(PreparedRuntimeError::ConflictingSettledConstructors);
            }
            let current = (identity, *host_id, family);
            if slot.is_some_and(|known| known != current) {
                return Err(PreparedRuntimeError::ConflictingSettledConstructors);
            }
            *slot = Some(current);
        }
        match (done, suspended) {
            (
                Some((done_identity, done, done_family)),
                Some((suspended_identity, suspended, suspended_family)),
            ) if done != suspended
                && done_identity != suspended_identity
                && done_family == suspended_family =>
            {
                Ok(Some(Self { done, suspended }))
            }
            (Some(_), Some(_)) => Err(PreparedRuntimeError::ConflictingSettledConstructors),
            _ => Ok(None),
        }
    }
}

impl ProgramFacts {
    fn of(prepared: &PreparedProgram) -> Self {
        Self::of_definitions(prepared.definitions(), Some(prepared.entry()))
    }

    fn of_definitions(prepared: DefinitionsView<'_>, entry: Option<ValueId>) -> Self {
        Self::from_definitions(Arc::new(DefinitionFacts::new(prepared)), entry)
    }

    fn from_image(image: &CompiledProgram, entry: Option<ValueId>) -> Self {
        Self::from_definitions(Arc::clone(image.definition_facts()), entry)
    }

    fn from_definitions(definitions: Arc<DefinitionFacts>, entry: Option<ValueId>) -> Self {
        let entry_identity = entry
            .as_ref()
            .and_then(|entry| definitions.tops.get(entry))
            .map(|(identity, _)| identity);
        let helper = |occurrence: &str| {
            let mut expected = entry_identity?.clone();
            expected.occurrence = occurrence.into();
            definitions
                .tops
                .iter()
                .find_map(|(id, (identity, _))| (identity == &expected).then_some(*id))
        };
        let resume = helper(PREPARED_RESUME_TARGET);
        let apply_entry = helper(PREPARED_APPLY_ENTRY_TARGET);
        let apply_value = helper(PREPARED_APPLY_VALUE_TARGET);
        let mut facts = Self {
            entry,
            settled: None,
            definitions,
            resume,
            apply_entry,
            apply_value,
        };
        facts.settled = SettledIds::from_facts([&facts]).ok().flatten();
        facts
    }

    fn json_layout(&self) -> Option<JsonLayout<DataConId>> {
        self.json_layout
    }

    fn is_json_value(
        &self,
        data: &DataView,
        budget: &mut TypeWorkBudget,
    ) -> Result<bool, TypeGraphError> {
        let Some(layout) = self.json_layout() else {
            return Ok(false);
        };
        let expected = [
            layout.object,
            layout.array,
            layout.string,
            layout.number,
            layout.bool_,
            layout.null,
        ];
        let mut matched = [false; 6];
        let mut count = 0;
        for constructor in data.constructors() {
            budget.charge(1)?;
            let Some(index) = expected
                .iter()
                .position(|host| self.constructor_host_id(constructor) == Some(*host))
            else {
                return Ok(false);
            };
            if matched[index] {
                return Ok(false);
            }
            matched[index] = true;
            count += 1;
        }
        Ok(count == 6)
    }

    fn selected_constructor(
        &self,
        data: &DataView,
        host_id: DataConId,
        budget: &mut TypeWorkBudget,
    ) -> Result<Option<tidepool_repr::execution_schema::ConstructorId>, TypeGraphError> {
        for constructor in data.constructors() {
            budget.charge(1)?;
            if self.constructor_host_id(constructor) == Some(host_id) {
                return Ok(Some(constructor));
            }
        }
        Ok(None)
    }

    fn constructor_host_id(
        &self,
        id: tidepool_repr::execution_schema::ConstructorId,
    ) -> Option<DataConId> {
        self.constructors
            .get(id.0 as usize)
            .map(|(_, host_id, _)| *host_id)
    }

    fn is_json_list_constructor(&self, host_id: DataConId) -> bool {
        self.json_layout()
            .is_some_and(|layout| host_id == layout.cons || host_id == layout.nil)
    }

    /// A builtin leaf requires one unambiguous full constructor identity.
    /// The spelling index selects candidates; differing units or bridge ids
    /// cannot acquire authority by their order in the declaration table.
    fn constructor_named(&self, module: &str, occurrence: &str) -> Option<DataConId> {
        let indices = self.by_identity.get(module)?.get(occurrence)?;
        let selected = self.constructors.get(*indices.first()?)?;
        indices
            .iter()
            .all(|index| self.constructors.get(*index) == Some(selected))
            .then_some(selected.1)
    }

    fn selected_fields(
        &self,
        cursor: &TypeCursor,
        host_id: DataConId,
        budget: &mut TypeWorkBudget,
    ) -> Result<Option<Vec<TypeCursor>>, TypeGraphError> {
        let TypeView::Data(data) = cursor.view(budget)? else {
            return Ok(None);
        };
        let Some(constructor) = self.selected_constructor(&data, host_id, budget)? else {
            return Ok(None);
        };
        data.fields(constructor, budget)
    }
}

const TEXT_MODULE: &str = "Data.Text.Internal";
const INTEGER_MODULE: &str = "GHC.Num.Integer";
const NATURAL_MODULE: &str = "GHC.Num.Natural";

/// Target-encode `literal` for a field of representation `rep`: the value's
/// native bytes, of which the builder writes only the field's declared width.
/// `None` when the literal's kind does not match the representation.
fn scalar_bits(rep: RuntimeRep, literal: &Literal) -> Option<[u8; 16]> {
    let word: u128 = match (rep, literal) {
        (RuntimeRep::Int(bits), Literal::LitInt(value)) => {
            // Refuse a value the field cannot hold rather than truncating.
            if bits < 64 && (*value < -(1_i64 << (bits - 1)) || *value >= (1_i64 << (bits - 1))) {
                return None;
            }
            *value as u128
        }
        (RuntimeRep::Word(bits), Literal::LitWord(value)) => {
            if bits < 64 && *value >= (1_u64 << bits) {
                return None;
            }
            u128::from(*value)
        }
        (RuntimeRep::Word(bits), Literal::LitChar(value)) if bits >= 32 => {
            u128::from(*value as u32)
        }
        (RuntimeRep::Float(64), Literal::LitDouble(bits))
        | (RuntimeRep::Float(32), Literal::LitFloat(bits)) => u128::from(*bits),
        _ => return None,
    };
    Some(word.to_ne_bytes())
}

#[derive(Clone)]
enum StructuralExpected {
    Node(TypeCursor),
    Bytes,
    Scalar(RuntimeRep),
    JsonValue,
    JsonMap,
    JsonList,
    JsonText,
    JsonScientific,
    JsonInteger,
    JsonBool,
    JsonBoxedInt,
}

struct StructuralFrame {
    host_id: DataConId,
    expected: Vec<StructuralExpected>,
    fields: Vec<ManagedField>,
    counts_depth: bool,
}

/// Reply origin used by structural refusal diagnostics; static replies have no site ID.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReplyTarget {
    Static(DataConId),
    AtSite(u64),
}

impl std::fmt::Display for ReplyTarget {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Static(constructor) => write!(f, "constructor {constructor:?}"),
            Self::AtSite(site) => write!(f, "site {site}"),
        }
    }
}

struct StructuralAnswerVisitor<'facts, 'builder, 'machine, 'code> {
    site: ReplyTarget,
    root: TypeCursor,
    budget: TypeWorkBudget,
    facts: &'facts ProgramFacts,
    builder: &'builder mut ManagedBuilder<'machine, 'code>,
    frames: Vec<StructuralFrame>,
    result: Option<ManagedNode>,
    failure: Option<PreparedRuntimeError>,
    depth: usize,
}

impl StructuralAnswerVisitor<'_, '_, '_, '_> {
    fn json_value_shape(
        &mut self,
        host_id: DataConId,
    ) -> Result<Vec<StructuralExpected>, BridgeError> {
        let Some(layout) = self.facts.json_layout() else {
            return Err(self.shape("the program has no authenticated JSON layout"));
        };
        if host_id == layout.object {
            Ok(vec![StructuralExpected::JsonMap])
        } else if host_id == layout.array {
            Ok(vec![StructuralExpected::JsonList])
        } else if host_id == layout.string {
            Ok(vec![StructuralExpected::JsonText])
        } else if host_id == layout.number {
            Ok(vec![StructuralExpected::JsonScientific])
        } else if host_id == layout.bool_ {
            Ok(vec![StructuralExpected::JsonBool])
        } else if host_id == layout.null {
            Ok(Vec::new())
        } else {
            Err(self
                .shape("the JSON value constructor is absent from authenticated layout evidence"))
        }
    }

    fn bridge_abort(&mut self, error: PreparedRuntimeError) -> BridgeError {
        self.failure = Some(error);
        BridgeError::TypeMismatch {
            expected: "the parked site's answer type".into(),
            got: "structural response mismatch".into(),
        }
    }

    fn shape(&mut self, detail: &'static str) -> BridgeError {
        self.bridge_abort(PreparedRuntimeError::AnswerShape {
            site: self.site,
            detail,
        })
    }

    fn expected(&mut self) -> Result<StructuralExpected, BridgeError> {
        if let Some(frame) = self.frames.last() {
            frame
                .expected
                .get(frame.fields.len())
                .cloned()
                .ok_or_else(|| self.shape("the response emits too many constructor fields"))
        } else if self.result.is_none() {
            Ok(StructuralExpected::Node(self.root.clone()))
        } else {
            Err(self.shape("the response emits more than one root"))
        }
    }

    fn attach(&mut self, field: ManagedField) -> Result<(), BridgeError> {
        if let Some(frame) = self.frames.last_mut() {
            frame.fields.push(field);
            Ok(())
        } else if let ManagedField::Node(root) = field {
            self.result = Some(root);
            Ok(())
        } else {
            Err(self.shape("a host answer root must be a constructor"))
        }
    }

    fn constructor_shape(
        &mut self,
        expected: StructuralExpected,
        host_id: DataConId,
    ) -> Result<Vec<StructuralExpected>, BridgeError> {
        let StructuralExpected::Node(node) = expected else {
            return match expected {
                StructuralExpected::JsonMap => {
                    let Some(layout) = self.facts.json_layout() else {
                        return Err(self.shape("the program has no authenticated JSON layout"));
                    };
                    if host_id == layout.map_tip {
                        Ok(Vec::new())
                    } else if host_id == layout.map_bin {
                        let representation = self
                            .builder
                            .constructor_field_rep(host_id, 0)
                            .map_err(|error| self.bridge_abort(PreparedRuntimeError::Run(error)))?;
                        let size = match representation {
                            Some(RuntimeRep::Int(64)) => {
                                StructuralExpected::Scalar(RuntimeRep::Int(64))
                            }
                            Some(RuntimeRep::LiftedRef) => StructuralExpected::JsonBoxedInt,
                            _ => {
                                return Err(
                                    self.shape("a JSON map size has an invalid representation")
                                )
                            }
                        };
                        Ok(vec![
                            size,
                            StructuralExpected::JsonText,
                            StructuralExpected::JsonValue,
                            StructuralExpected::JsonMap,
                            StructuralExpected::JsonMap,
                        ])
                    } else {
                        Err(self.shape("a JSON object requires Data.Map Bin or Tip"))
                    }
                }
                StructuralExpected::JsonList => {
                    let Some(layout) = self.facts.json_layout() else {
                        return Err(self.shape("the program has no authenticated JSON layout"));
                    };
                    if host_id == layout.nil {
                        Ok(Vec::new())
                    } else if host_id == layout.cons {
                        Ok(vec![
                            StructuralExpected::JsonValue,
                            StructuralExpected::JsonList,
                        ])
                    } else {
                        Err(self.shape("a JSON array requires list constructors"))
                    }
                }
                StructuralExpected::JsonText => {
                    if self
                        .facts
                        .json_layout()
                        .is_some_and(|layout| host_id == layout.text)
                    {
                        Ok(vec![
                            StructuralExpected::Bytes,
                            StructuralExpected::Scalar(RuntimeRep::Int(64)),
                            StructuralExpected::Scalar(RuntimeRep::Int(64)),
                        ])
                    } else {
                        Err(self.shape("a JSON string requires the Text constructor"))
                    }
                }
                StructuralExpected::JsonScientific => {
                    if self
                        .facts
                        .json_layout()
                        .is_some_and(|layout| host_id == layout.scientific)
                    {
                        Ok(vec![
                            StructuralExpected::JsonInteger,
                            StructuralExpected::Scalar(RuntimeRep::Int(64)),
                        ])
                    } else {
                        Err(self.shape("a JSON number requires the Scientific constructor"))
                    }
                }
                StructuralExpected::JsonInteger => {
                    if self
                        .facts
                        .json_layout()
                        .is_some_and(|layout| host_id == layout.integer_small)
                    {
                        Ok(vec![StructuralExpected::Scalar(RuntimeRep::Int(64))])
                    } else if self.facts.json_layout().is_some_and(|layout| {
                        host_id == layout.integer_positive || host_id == layout.integer_negative
                    }) {
                        Ok(vec![StructuralExpected::Bytes])
                    } else {
                        Err(self.shape("a JSON coefficient requires IS, IP or IN"))
                    }
                }
                StructuralExpected::JsonBool => {
                    if self
                        .facts
                        .json_layout()
                        .is_some_and(|layout| host_id == layout.true_ || host_id == layout.false_)
                    {
                        Ok(Vec::new())
                    } else {
                        Err(self.shape("a JSON boolean requires True or False"))
                    }
                }
                StructuralExpected::JsonBoxedInt => {
                    if self
                        .facts
                        .json_layout()
                        .is_some_and(|layout| host_id == layout.int)
                    {
                        Ok(vec![StructuralExpected::Scalar(RuntimeRep::Int(64))])
                    } else {
                        Err(self.shape("a JSON map size requires I#"))
                    }
                }
                StructuralExpected::JsonValue => self.json_value_shape(host_id),
                StructuralExpected::Bytes | StructuralExpected::Scalar(_) => {
                    Err(self.shape("a constructor was emitted for a scalar or byte field"))
                }
                StructuralExpected::Node(_) => unreachable!(),
            };
        };
        let view = node.view(&mut self.budget).map_err(|source| {
            self.bridge_abort(PreparedRuntimeError::AnswerTypeEvidence {
                site: self.site,
                source,
            })
        })?;
        match view {
            TypeView::Data(data) => {
                let constructor = self
                    .facts
                    .selected_constructor(&data, host_id, &mut self.budget)
                    .map_err(|source| {
                        self.bridge_abort(PreparedRuntimeError::AnswerTypeEvidence {
                            site: self.site,
                            source,
                        })
                    })?
                    .ok_or_else(|| {
                        self.bridge_abort(PreparedRuntimeError::AnswerConstructor {
                            site: self.site,
                            host_id,
                        })
                    })?;
                if self
                    .facts
                    .is_json_value(&data, &mut self.budget)
                    .map_err(|source| {
                        self.bridge_abort(PreparedRuntimeError::AnswerTypeEvidence {
                            site: self.site,
                            source,
                        })
                    })?
                {
                    return self.json_value_shape(host_id);
                }
                let fields = data
                    .fields(constructor, &mut self.budget)
                    .map_err(|source| {
                        self.bridge_abort(PreparedRuntimeError::AnswerTypeEvidence {
                            site: self.site,
                            source,
                        })
                    })?
                    .ok_or_else(|| {
                        self.bridge_abort(PreparedRuntimeError::AnswerConstructor {
                            site: self.site,
                            host_id,
                        })
                    })?;
                Ok(fields.into_iter().map(StructuralExpected::Node).collect())
            }
            TypeView::Text => {
                let text = self.facts.constructor_named(TEXT_MODULE, "Text");
                if text != Some(host_id) {
                    return Err(self.shape("a Text answer requires the Text constructor"));
                }
                Ok(vec![
                    StructuralExpected::Bytes,
                    StructuralExpected::Scalar(RuntimeRep::Int(64)),
                    StructuralExpected::Scalar(RuntimeRep::Int(64)),
                ])
            }
            TypeView::Integer => {
                let is = self.facts.constructor_named(INTEGER_MODULE, "IS");
                let ip = self.facts.constructor_named(INTEGER_MODULE, "IP");
                let in_ = self.facts.constructor_named(INTEGER_MODULE, "IN");
                if is == Some(host_id) {
                    Ok(vec![StructuralExpected::Scalar(RuntimeRep::Int(64))])
                } else if ip == Some(host_id) || in_ == Some(host_id) {
                    Ok(vec![StructuralExpected::Bytes])
                } else {
                    Err(self.shape("an Integer answer requires IS, IP or IN"))
                }
            }
            TypeView::Natural => {
                let ns = self.facts.constructor_named(NATURAL_MODULE, "NS");
                let nb = self.facts.constructor_named(NATURAL_MODULE, "NB");
                if ns == Some(host_id) {
                    Ok(vec![StructuralExpected::Scalar(RuntimeRep::Word(64))])
                } else if nb == Some(host_id) {
                    Ok(vec![StructuralExpected::Bytes])
                } else {
                    Err(self.shape("a Natural answer requires NS or NB"))
                }
            }
            TypeView::Scalar(_) => Err(self.shape("a scalar field requires a literal")),
            TypeView::Unconstructible(reason) => Err(self.bridge_abort(
                PreparedRuntimeError::AnswerUnconstructible {
                    site: self.site,
                    reason: reason.to_string(),
                },
            )),
        }
    }
}

impl HaskellVisitor for StructuralAnswerVisitor<'_, '_, '_, '_> {
    fn expected_field_rep(&self) -> Option<RuntimeRep> {
        let frame = self.frames.last()?;
        self.builder
            .constructor_field_rep(frame.host_id, frame.fields.len())
            .ok()
            .flatten()
    }

    fn begin_constructor(&mut self, id: DataConId, fields: usize) -> Result<(), BridgeError> {
        let expected = self.expected()?;
        // Representation spines do not add semantic nesting: a flat 100k
        // element list or a large Map may have that many cons/tree nodes.
        // Their elements and JSON Value wrappers still pass through this
        // bound, as do ordinary nested constructors.
        let counts_depth = !matches!(
            expected,
            StructuralExpected::JsonList
                | StructuralExpected::JsonMap
                | StructuralExpected::JsonText
                | StructuralExpected::JsonScientific
                | StructuralExpected::JsonInteger
                | StructuralExpected::JsonBool
                | StructuralExpected::JsonBoxedInt
        ) && !self.facts.is_json_list_constructor(id);
        if counts_depth && self.depth >= MAX_ANSWER_DEPTH {
            return Err(self.shape("the response exceeds the maximum constructor nesting depth"));
        }
        let shape = self.constructor_shape(expected, id)?;
        if shape.len() != fields {
            return Err(self.shape("the constructor's field count does not match its declaration"));
        }
        self.frames.push(StructuralFrame {
            host_id: id,
            expected: shape,
            fields: Vec::with_capacity(fields),
            counts_depth,
        });
        self.depth += usize::from(counts_depth);
        Ok(())
    }

    fn end_constructor(&mut self) -> Result<(), BridgeError> {
        let frame = self
            .frames
            .pop()
            .ok_or_else(|| self.shape("a constructor ended without a matching begin"))?;
        self.depth -= usize::from(frame.counts_depth);
        if frame.fields.len() != frame.expected.len() {
            return Err(self.shape("a constructor ended before all fields were emitted"));
        }
        let mut fields = frame.fields;
        for field in &mut fields {
            *field = match *field {
                ManagedField::Node(node) => ManagedField::Consume(node),
                field => field,
            };
        }
        let node = self
            .builder
            .constructor(frame.host_id, &fields)
            .map_err(|error| self.bridge_abort(PreparedRuntimeError::Run(error)))?;
        self.attach(ManagedField::Node(node))
    }

    fn literal(&mut self, literal: Literal) -> Result<(), BridgeError> {
        let rep = match self.expected()? {
            StructuralExpected::Node(node) => {
                match node.view(&mut self.budget).map_err(|source| {
                    self.bridge_abort(PreparedRuntimeError::AnswerTypeEvidence {
                        site: self.site,
                        source,
                    })
                })? {
                    TypeView::Scalar(rep) => rep,
                    _ => return Err(self.shape("a literal was emitted for a constructor field")),
                }
            }
            StructuralExpected::Scalar(rep) => rep,
            StructuralExpected::Bytes => {
                return Err(self.shape("a literal was emitted for a byte-array field"))
            }
            StructuralExpected::JsonValue
            | StructuralExpected::JsonMap
            | StructuralExpected::JsonList
            | StructuralExpected::JsonText
            | StructuralExpected::JsonScientific
            | StructuralExpected::JsonInteger
            | StructuralExpected::JsonBool
            | StructuralExpected::JsonBoxedInt => {
                return Err(self.shape("a literal was emitted for a JSON constructor field"))
            }
        };
        let bits = scalar_bits(rep, &literal).ok_or_else(|| {
            self.shape("the literal does not fit the field's scalar representation")
        })?;
        self.attach(ManagedField::Scalar(bits))
    }

    fn byte_array(&mut self, bytes: Vec<u8>) -> Result<(), BridgeError> {
        if !matches!(self.expected()?, StructuralExpected::Bytes) {
            return Err(self.shape("a byte array was emitted for a non-byte field"));
        }
        let node = self
            .builder
            .bytes(&bytes)
            .map_err(|error| self.bridge_abort(PreparedRuntimeError::Run(error)))?;
        self.attach(ManagedField::Node(node))
    }
}

fn build_structural_node(
    response: &dyn tidepool_bridge::ToHaskell,
    table: &DataConTable,
    site: ReplyTarget,
    root: TypeNodeId,
    facts: &ProgramFacts,
    builder: &mut ManagedBuilder<'_, '_>,
) -> Result<ManagedNode, PreparedRuntimeError> {
    // A structural reply belongs to the parked site's owner, not to the
    // accumulated session table. Attach that owner's admitted JSON roles for
    // this visit only: `serde_json::Value` then cannot borrow another
    // program's layout, and ordinary JSON effect replies work even when the
    // session table was assembled before this program installed.
    let response_table = table.with_json_layout(facts.json_layout());
    let mut budget = TypeWorkBudget::new(GraphLimits::default().max_work);
    let root = facts
        .types
        .open_root(root, &mut budget)
        .map_err(|source| PreparedRuntimeError::AnswerTypeEvidence { site, source })?;
    let mut visitor = StructuralAnswerVisitor {
        site,
        root,
        budget,
        facts,
        builder,
        frames: Vec::new(),
        result: None,
        failure: None,
        depth: 0,
    };
    let visited = response.visit(&response_table, &mut visitor);
    if let Some(error) = visitor.failure.take() {
        return Err(error);
    }
    visited.map_err(|source| PreparedRuntimeError::AnswerRejected { site, source })?;
    if !visitor.frames.is_empty() {
        return Err(PreparedRuntimeError::AnswerShape {
            site,
            detail: "the response left a constructor unfinished",
        });
    }
    visitor.result.ok_or(PreparedRuntimeError::AnswerShape {
        site,
        detail: "the response emitted no managed answer root",
    })
}

#[allow(
    clippy::too_many_arguments,
    reason = "internal helper with one call site; the parameters are the disjoint framed-answer \
              build context (payload, target constructor, and lookup tables), not a natural struct"
)]
fn build_framed_structural_node(
    prefix: &[Box<dyn tidepool_bridge::ToHaskell + Send>],
    handle: PreparedHandle,
    constructor: DataConId,
    field_cursors: Vec<TypeCursor>,
    table: &DataConTable,
    site: ReplyTarget,
    root: TypeCursor,
    budget: TypeWorkBudget,
    facts: &ProgramFacts,
    builder: &mut ManagedBuilder<'_, '_>,
) -> Result<ManagedNode, PreparedRuntimeError> {
    let response_table = table.with_json_layout(facts.json_layout());
    let fields = Vec::with_capacity(field_cursors.len());
    let mut visitor = StructuralAnswerVisitor {
        site,
        root,
        budget,
        facts,
        builder,
        frames: vec![StructuralFrame {
            host_id: constructor,
            expected: field_cursors
                .into_iter()
                .map(StructuralExpected::Node)
                .collect(),
            fields,
            counts_depth: true,
        }],
        result: None,
        failure: None,
        depth: 1,
    };
    for field in prefix {
        let visited = field.visit(&response_table, &mut visitor);
        if let Some(error) = visitor.failure.take() {
            return Err(error);
        }
        visited.map_err(|source| PreparedRuntimeError::AnswerRejected { site, source })?;
    }
    if visitor.frames.len() != 1 {
        return Err(PreparedRuntimeError::AnswerShape {
            site,
            detail: "a framed prefix left a constructor unfinished",
        });
    }
    #[allow(clippy::expect_used, reason = "checked above: frames.len() == 1")]
    visitor
        .frames
        .last_mut()
        .expect("the outer framed constructor remains open")
        .fields
        .push(ManagedField::Handle(handle));
    let ended = visitor.end_constructor();
    if let Some(error) = visitor.failure.take() {
        return Err(error);
    }
    ended.map_err(|source| PreparedRuntimeError::AnswerRejected { site, source })?;
    visitor.result.ok_or(PreparedRuntimeError::AnswerShape {
        site,
        detail: "the framed response emitted no managed answer root",
    })
}

/// A deliberately small structural sink for values whose Haskell type is
/// fixed by a compiler-produced binding interface. It does not make a
/// `HaskellValue` tree: every completed child is immediately handed to the
/// machine's one managed builder. The builder authenticates constructor
/// descriptors and field representations before it allocates.
struct ManagedMountVisitor<'builder, 'machine, 'code> {
    builder: &'builder mut ManagedBuilder<'machine, 'code>,
    frames: Vec<MountFrame>,
    root: Option<ManagedNode>,
}

struct MountFrame {
    host_id: DataConId,
    expected: usize,
    fields: Vec<ManagedField>,
}

impl ManagedMountVisitor<'_, '_, '_> {
    fn rejected(expected: impl Into<String>, got: impl Into<String>) -> BridgeError {
        BridgeError::TypeMismatch {
            expected: expected.into(),
            got: got.into(),
        }
    }

    fn attach(&mut self, field: ManagedField) -> Result<(), BridgeError> {
        if let Some(frame) = self.frames.last_mut() {
            if frame.fields.len() == frame.expected {
                return Err(Self::rejected(
                    format!("constructor with {} fields", frame.expected),
                    "too many visitor fields",
                ));
            }
            frame.fields.push(field);
            return Ok(());
        }
        let ManagedField::Node(node) = field else {
            return Err(Self::rejected("one managed value root", "a scalar field"));
        };
        if self.root.replace(node).is_some() {
            return Err(Self::rejected("one visitor root", "multiple visitor roots"));
        }
        Ok(())
    }

    fn scalar(literal: Literal) -> Result<[u8; 16], BridgeError> {
        let word = match literal {
            Literal::LitInt(value) => value as u128,
            Literal::LitWord(value) => u128::from(value),
            Literal::LitChar(value) => u128::from(value as u32),
            Literal::LitFloat(bits) | Literal::LitDouble(bits) => u128::from(bits),
            literal => {
                return Err(Self::rejected(
                    "a scalar literal supported by managed construction",
                    format!("{literal:?}"),
                ))
            }
        };
        Ok(word.to_ne_bytes())
    }

    fn finish(self) -> Result<ManagedNode, BridgeError> {
        if !self.frames.is_empty() {
            return Err(Self::rejected(
                "closed visitor constructors",
                "unfinished constructor",
            ));
        }
        self.root
            .ok_or_else(|| Self::rejected("one visitor root", "no visitor root"))
    }
}

impl HaskellVisitor for ManagedMountVisitor<'_, '_, '_> {
    fn begin_constructor(&mut self, id: DataConId, fields: usize) -> Result<(), BridgeError> {
        self.frames.push(MountFrame {
            host_id: id,
            expected: fields,
            fields: Vec::with_capacity(fields),
        });
        Ok(())
    }

    fn end_constructor(&mut self) -> Result<(), BridgeError> {
        let frame = self.frames.pop().ok_or_else(|| {
            Self::rejected(
                "an open visitor constructor",
                "constructor end without begin",
            )
        })?;
        if frame.fields.len() != frame.expected {
            return Err(BridgeError::ArityMismatch {
                con: frame.host_id,
                expected: frame.expected,
                got: frame.fields.len(),
            });
        }
        let fields = frame
            .fields
            .into_iter()
            .map(|field| match field {
                ManagedField::Node(node) => ManagedField::Consume(node),
                field => field,
            })
            .collect::<Vec<_>>();
        let node = self
            .builder
            .constructor(frame.host_id, &fields)
            .map_err(|error| BridgeError::InternalError(error.to_string()))?;
        self.attach(ManagedField::Node(node))
    }

    fn literal(&mut self, literal: Literal) -> Result<(), BridgeError> {
        if let Literal::LitByteArray(bytes) = literal {
            let node = self
                .builder
                .bytes(&bytes)
                .map_err(|error| BridgeError::InternalError(error.to_string()))?;
            return self.attach(ManagedField::Node(node));
        }
        self.attach(ManagedField::Scalar(Self::scalar(literal)?))
    }

    fn byte_array(&mut self, bytes: Vec<u8>) -> Result<(), BridgeError> {
        let node = self
            .builder
            .bytes(&bytes)
            .map_err(|error| BridgeError::InternalError(error.to_string()))?;
        self.attach(ManagedField::Node(node))
    }

    fn expected_field_rep(&self) -> Option<RuntimeRep> {
        let frame = self.frames.last()?;
        self.builder
            .constructor_field_rep(frame.host_id, frame.fields.len())
            .ok()
            .flatten()
    }
}

fn constructor_replies_equivalent(
    a: &ProgramFacts,
    x: ConstructorReply,
    b: &ProgramFacts,
    y: ConstructorReply,
) -> Result<bool, TypeGraphError> {
    match (x, y) {
        (ConstructorReply::AtSite, ConstructorReply::AtSite) => Ok(true),
        (
            ConstructorReply::StaticWithSite {
                reply: x,
                field: xf,
                payload_field: xp,
                capture_input: xc,
            },
            ConstructorReply::StaticWithSite {
                reply: y,
                field: yf,
                payload_field: yp,
                capture_input: yc,
            },
        ) if (xf, xp, xc) == (yf, yp, yc) => {
            let mut budget = TypeWorkBudget::new(GraphLimits::default().max_work);
            a.types.rooted_compatible(x, &b.types, y, &mut budget)
        }
        (ConstructorReply::Static(x), ConstructorReply::Static(y)) => {
            let mut budget = TypeWorkBudget::new(GraphLimits::default().max_work);
            a.types.rooted_compatible(x, &b.types, y, &mut budget)
        }
        _ => Ok(false),
    }
}

fn reply_conflict_evidence(
    constructor: DataConId,
    existing: &ProgramFacts,
    existing_reply: ConstructorReply,
    incoming: &ProgramFacts,
    incoming_reply: ConstructorReply,
) -> Box<ConstructorReplyConflictEvidence> {
    fn text(value: &str) -> String {
        const MAX_CHARS: usize = 256;
        let mut chars = value.chars();
        let mut bounded: String = chars.by_ref().take(MAX_CHARS).collect();
        if chars.next().is_some() {
            bounded.push('…');
        }
        bounded
    }

    fn identity(value: &SymbolIdentity) -> SymbolIdentity {
        SymbolIdentity {
            unit: text(&value.unit),
            module: text(&value.module),
            namespace: text(&value.namespace),
            occurrence: text(&value.occurrence),
            record_parent: value.record_parent.as_deref().map(text),
        }
    }

    fn observe(facts: &ProgramFacts, reply: ConstructorReply) -> ConstructorReplyObservation {
        match reply {
            ConstructorReply::AtSite => ConstructorReplyObservation::AtSite,
            ConstructorReply::Static(node)
            | ConstructorReply::StaticWithSite { reply: node, .. } => {
                let input_site = match reply {
                    ConstructorReply::StaticWithSite {
                        field,
                        payload_field,
                        capture_input,
                        ..
                    } => Some((field, payload_field, capture_input)),
                    _ => None,
                };
                let mut budget = TypeWorkBudget::new(256);
                let shape = match facts.types.open_root(node, &mut budget) {
                    Err(source) => ReplyTypeObservation::Refused(source),
                    Ok(cursor) => match cursor.view(&mut budget) {
                        Err(source) => ReplyTypeObservation::Refused(source),
                        Ok(TypeView::Data(data)) => ReplyTypeObservation::Data {
                            family: identity(data.family()),
                            argument_count: data.argument_count(),
                            constructor_count: data.constructors().count(),
                        },
                        Ok(TypeView::Text) => ReplyTypeObservation::Text,
                        Ok(TypeView::Integer) => ReplyTypeObservation::Integer,
                        Ok(TypeView::Natural) => ReplyTypeObservation::Natural,
                        Ok(TypeView::Scalar(rep)) => ReplyTypeObservation::Scalar(rep),
                        Ok(TypeView::Unconstructible(reason)) => {
                            ReplyTypeObservation::Unconstructible {
                                reason: text(&reason.to_string()),
                                rendered: text(cursor.rendered()),
                            }
                        }
                    },
                };
                ConstructorReplyObservation::Static {
                    node,
                    shape,
                    input_site,
                }
            }
        }
    }

    Box::new(ConstructorReplyConflictEvidence {
        constructor: existing
            .constructors
            .iter()
            .find_map(|(symbol, host, _)| (*host == constructor).then(|| identity(symbol))),
        existing: observe(existing, existing_reply),
        incoming: observe(incoming, incoming_reply),
    })
}

fn sites_equivalent(
    a: &ProgramFacts,
    a_row: &SiteRow,
    b: &ProgramFacts,
    b_row: &SiteRow,
) -> Result<bool, TypeGraphError> {
    if a_row.delivery != b_row.delivery || a_row.inputs.len() != b_row.inputs.len() {
        return Ok(false);
    }
    let mut budget = TypeWorkBudget::new(GraphLimits::default().max_work);
    if !a
        .types
        .rooted_compatible(a_row.wire, &b.types, b_row.wire, &mut budget)?
    {
        return Ok(false);
    }
    for (x, y) in a_row.inputs.iter().zip(&b_row.inputs) {
        if !a.types.rooted_compatible(*x, &b.types, *y, &mut budget)? {
            return Ok(false);
        }
    }
    Ok(true)
}

/// A boxed or unboxed non-negative `Int` field: the leading site argument of
/// an extractor-sited kernel request.
fn site_field(field: &HaskellValue, table: &DataConTable) -> Option<u64> {
    match field {
        HaskellValue::Lit(Literal::LitInt(n)) => u64::try_from(*n).ok(),
        HaskellValue::Con(id, inner)
            if table.get_by_qualified_name("GHC.Types.I#") == Some(*id) =>
        {
            match inner.as_slice() {
                [HaskellValue::Lit(Literal::LitInt(n))] => u64::try_from(*n).ok(),
                _ => None,
            }
        }
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// PreparedEngine — the prepared half of a resident session's engine
// ---------------------------------------------------------------------------

/// The prepared-STG half of a resident session's engine: one machine shared
/// by every program the session's turns install, plus what the session keeps
/// about each program once the machine owns its code. Bindings, generations,
/// scopes and leases stay in `PersistentSession`; this owns only code and heap.
///
/// A session on the prepared route bootstraps its machine from its first
/// turn's program ([`Self::bootstrap`]) and installs every later one against
/// the session's live prepared bindings ([`Self::install`]). A turn runs as
/// its settled scaffold ([`Self::run_settled`]): the host reads completion or
/// suspension from one constructor layer and never walks freer data.
pub struct PreparedEngine {
    machine: PreparedMachine<'static>,
    programs: BTreeMap<ProgramId, ProgramFacts>,
    /// The machine-owned site index: which installed program's site table is
    /// authoritative for each typed site id. Extended in the install
    /// transaction before any code is compiled; a conflicting duplicate
    /// refuses the install (see [`Self::install`]).
    sites: BTreeMap<u64, SiteWitness>,
    /// Canonical evidence for exact request constructors. Each witness indexes
    /// the owner's immutable constructor reply table; static replies never
    /// manufacture a site row.
    constructor_replies: BTreeMap<DataConId, SiteWitness>,
    /// Prepared old-space bytes as of the last successful
    /// [`Self::quiesce_and_collect`] (the compacted figure
    /// `RetirementReceipt::old_bytes` reports); `0` before any collection
    /// has run.
    old_bytes: usize,
    /// Programs installed ([`Self::bootstrap`] counts as the first one)
    /// since the last successful major collection. Reset to `0` only when
    /// [`Self::quiesce_and_collect_now`] actually runs `collect_major`; a
    /// `NotQuiescent` refusal leaves it untouched so the next eligible turn
    /// retries with the same count. Read by [`Self::major_collection_due`].
    installs_since_major: usize,
    /// [`PreparedMachine::old_bytes_live`] as of the last successful major
    /// collection -- a live read of promoted bytes, not the install-count
    /// proxy `residency().block_words` (which only grows with installs and
    /// so cannot see promotion happening between them). `0` before any
    /// collection has run, which disables the growth trigger until there is
    /// a real baseline to grow from (see [`Self::major_collection_due`]).
    old_bytes_at_last_major: usize,
    /// Count of major collections [`Self::quiesce_and_collect_now`] has
    /// actually run (incremented only on success, never on a `NotQuiescent`
    /// refusal). The prepared route's analogue of
    /// `PreparedEngine::heap_stats`'s `gc_count` -- see [`Self::heap_stats`].
    major_collections: u64,
    /// Package-defined tops already installed on this machine, offered to
    /// every later turn as executable imports (see [`CodeExport`] and
    /// [`exportable_code_tops`]). Grows once, on the turns that first reach
    /// a package definition, and is never invalidated: a package's code
    /// cannot change under a live session.
    code_exports: BTreeMap<SymbolIdentity, CodeExport>,
    /// Shared with every other machine of this run when the composition
    /// root wires one up ([`Self::set_image_registry`]); `None` keeps
    /// today's behavior (every install compiles its own image). A registry
    /// hit installs the already-compiled `Arc<CompiledProgram>` through
    /// [`PreparedMachine::install_shared`] instead of compiling again.
    registry: Option<Arc<ImageRegistry>>,
}

/// One installed package top a later turn may import instead of projecting
/// its own copy of the body.
///
/// `handle` roots the top's value on this machine ([`PreparedMachine::retain_top`]),
/// which also keeps its defining program -- and therefore its code -- alive
/// for as long as anything can import it. `entry` is the PRODUCER's own
/// signature for the top, so [`link_program`] compares a later turn's
/// declared entry evidence against the real callable rather than against an
/// echo of itself.
#[derive(Clone)]
struct CodeExport {
    program: ProgramId,
    handle: PreparedHandle,
    entry: Option<Signature>,
    /// The exact GHC-loaded package interface selected for this exported
    /// binder. Ordinary legacy roots may have no digest, but protected package
    /// owners require a complete match before native resolution.
    interface_digest: Option<[u8; 32]>,
}

fn matches_protected_package_interface(export: &CodeExport, requested: &[u8; 32]) -> bool {
    requested != &[0; 32] && export.interface_digest.as_ref() == Some(requested)
}

/// The generation every code export is offered at. Package code is fixed for
/// the life of a process -- a different package set is a different extractor
/// deployment, which the toolchain fingerprint already refuses to mix -- so
/// there is nothing for a generation to distinguish, and every turn is told
/// the same number it will be handed back at install.
const CODE_EXPORT_GENERATION: u64 = 0;

/// GHC's unit id for everything compiled from source in this session: the
/// turn target, the session declaration environment and binding store, the workspace source layer,
/// and the Tidepool library modules on the include path. All of it can
/// differ from one turn to the next, so none of it is ever exported.
pub(super) const HOME_UNIT: &str = "main";

/// Which of `prepared`'s own tops later turns may import rather than project
/// a body for: the ones an INSTALLED PACKAGE defines.
///
/// A package's unit id (`ghc-internal`, `text-2.1.2-2594`, ...) names a
/// built, content-versioned package in the compiler's package database. Its
/// unfoldings cannot change while one session runs, so a top recovered from
/// one is the same code this turn, next turn, and after a source reload --
/// which is exactly what makes it safe to hand a later turn the already
/// compiled copy. Everything under [`HOME_UNIT`] is excluded for the
/// opposite reason.
///
/// Byte tops (GHC `StgTopStringLit`) are excluded: they have no managed
/// value to retain. A constructor top is excluded unless its result is a
/// lifted reference, because that is the only representation
/// [`PreparedMachine::retain_top`] mints a handle for and a representation
/// disagreement at link time would fail the turn rather than fall back.
fn exportable_code_tops(
    prepared: &PreparedProgram,
) -> Vec<(SymbolIdentity, ValueId, Option<Signature>)> {
    let signature = |id: tidepool_repr::execution_schema::SignatureId| {
        prepared.signatures().get(id.0 as usize).cloned()
    };
    let mut exports = Vec::new();
    for group in prepared.bindings() {
        let tops = match group {
            Group::NonRecursive(top) => std::slice::from_ref(top),
            Group::Recursive(tops) => tops.as_slice(),
        };
        for top in tops {
            if top.identity.unit == HOME_UNIT || top.identity.namespace != "value" {
                continue;
            }
            let entry = match &top.binding.rhs {
                HeapRhs::Bytes(_) => continue,
                HeapRhs::Constructor { constructor, .. } => {
                    let lifted = prepared
                        .constructors()
                        .get(constructor.0 as usize)
                        .is_some_and(|row| row.result_rep == RuntimeRep::LiftedRef);
                    if !lifted {
                        continue;
                    }
                    None
                }
                HeapRhs::Function { signature: id, .. } | HeapRhs::Thunk { signature: id, .. } => {
                    signature(*id)
                }
            };
            exports.push((top.identity.clone(), top.binding.id, entry));
        }
    }
    exports
}

#[derive(Clone, Copy)]
struct PackageOwnerRef<'a> {
    unit: &'a str,
    module: &'a str,
    binder: &'a SymbolIdentity,
    interface_digest: &'a [u8; 32],
}

impl PackageOwnerRef<'_> {
    fn matches(self, owner: &ImportOwner) -> bool {
        matches!(owner, ImportOwner::Package { unit, module, binder, interface_digest }
            if unit.as_str() == self.unit && module.as_str() == self.module && binder == self.binder
                && interface_digest == self.interface_digest)
    }
}

#[derive(Clone, Copy)]
struct CodeExportOwnerRef<'a> {
    binder: &'a SymbolIdentity,
    generation: u64,
    root_id: u64,
    interface_digest: Option<&'a [u8; 32]>,
}

impl CodeExportOwnerRef<'_> {
    fn to_owned(self) -> ImportOwner {
        ImportOwner::CodeExport {
            binder: self.binder.clone(),
            generation: self.generation,
            root_id: self.root_id,
            interface_digest: self.interface_digest.copied(),
        }
    }
}

fn certified_package_owner_diagnostic(
    target: &CertifiedTargetImage,
    target_owners: &[ImportOwner],
    demanded: &[DemandedImage],
    owner: PackageOwnerRef<'_>,
    target_exports: &BTreeMap<SymbolIdentity, ValueId>,
    code_exports: &BTreeMap<SymbolIdentity, CodeExport>,
    interfaces_match: bool,
    interface_digest: Option<[u8; 32]>,
    limits: PackageOwnerDiagnosticLimits,
) -> CertifiedPackageOwnerDiagnostic {
    let binder = owner.binder;

    let globals = target.prepared.globals();
    let target_globals: Vec<_> = globals
        .iter()
        .zip(
            target_owners
                .iter()
                .map(Some)
                .chain(std::iter::repeat(None)),
        )
        .take(limits.target_globals)
        .map(|(global, owner)| PackageTargetGlobalFact {
            identity: global.identity.clone(),
            required_generation: global.required_generation,
            owner: owner.cloned(),
        })
        .collect();

    let direct_groups: BTreeSet<_> = demanded
        .iter()
        .enumerate()
        .filter(|(_, image)| {
            image
                .group()
                .imports()
                .iter()
                .any(|import| owner.matches(import))
        })
        .map(|(index, _)| index)
        .collect();
    let mut group_order: Vec<_> = direct_groups.iter().copied().collect();
    group_order.extend(
        demanded
            .iter()
            .enumerate()
            .filter(|(index, _)| !direct_groups.contains(index))
            .map(|(index, _)| index),
    );
    let selected_group_count = group_order.len().min(limits.groups);
    group_order.truncate(selected_group_count);

    let mut demanded_groups: Vec<_> = group_order
        .iter()
        .map(|index| {
            let group = demanded[*index].group();
            PackageDemandedGroupFact {
                owner: group.owner().clone(),
                original_ordinal: group.original_ordinal(),
                imports: Vec::new(),
                imports_omitted: 0,
                import_count: group.imports().len(),
            }
        })
        .collect();
    let mut remaining_imports = limits.imports;
    // Preserve the exact failed import first, then source-owner edges because
    // they explain how a missing package definition entered the closure.
    for priority in 0..3 {
        for (fact_index, demanded_index) in group_order.iter().enumerate() {
            let imports = demanded[*demanded_index].group().imports();
            for (position, import) in imports.iter().enumerate() {
                let selected_priority = if owner.matches(import) {
                    0
                } else if matches!(import, ImportOwner::Source { .. }) {
                    1
                } else {
                    2
                };
                if selected_priority != priority {
                    continue;
                }
                if remaining_imports == 0 {
                    continue;
                }
                demanded_groups[fact_index]
                    .imports
                    .push(PackageDemandedImportFact {
                        position,
                        owner: import.clone(),
                    });
                remaining_imports -= 1;
            }
        }
    }
    for group in &mut demanded_groups {
        group.imports.sort_by_key(|import| import.position);
        group.imports_omitted = group.import_count - group.imports.len();
    }

    let target_top_binding = target
        .prepared
        .bindings()
        .iter()
        .flat_map(|group| match group {
            Group::NonRecursive(top) => std::slice::from_ref(top),
            Group::Recursive(tops) => tops.as_slice(),
        })
        .find(|top| top.identity == *binder);
    let target_top = target_top_binding.map_or(
        PackageTargetTopFact {
            kind: PackageTargetTopKind::Absent,
            exportability: PackageTargetTopExportability::Absent,
            literal_admitted: false,
        },
        |top| {
            let literal_admitted = target.package_literals.contains_key(binder);
            let kind = match &top.binding.rhs {
                HeapRhs::Bytes(_) => PackageTargetTopKind::Bytes,
                HeapRhs::Constructor { .. } => PackageTargetTopKind::Constructor,
                HeapRhs::Function { .. } => PackageTargetTopKind::Function,
                HeapRhs::Thunk { .. } => PackageTargetTopKind::Thunk,
            };
            let (kind, exportability) = if top.identity.unit == HOME_UNIT {
                (kind, PackageTargetTopExportability::HomeUnit)
            } else if top.identity.namespace != "value" {
                (kind, PackageTargetTopExportability::NotValueNamespace)
            } else {
                match &top.binding.rhs {
                    HeapRhs::Bytes(_) if literal_admitted => {
                        (kind, PackageTargetTopExportability::Exportable)
                    }
                    HeapRhs::Bytes(_) => {
                        (kind, PackageTargetTopExportability::ByteLiteralNotAdmitted)
                    }
                    HeapRhs::Constructor { constructor, .. }
                        if target
                            .prepared
                            .constructors()
                            .get(constructor.0 as usize)
                            .is_some_and(|row| row.result_rep == RuntimeRep::LiftedRef) =>
                    {
                        (kind, PackageTargetTopExportability::Exportable)
                    }
                    HeapRhs::Constructor { .. } => {
                        (kind, PackageTargetTopExportability::ConstructorNotLifted)
                    }
                    HeapRhs::Function { .. } => (kind, PackageTargetTopExportability::Exportable),
                    HeapRhs::Thunk { .. } => (kind, PackageTargetTopExportability::Exportable),
                }
            };
            PackageTargetTopFact {
                kind,
                exportability: if target_exports.contains_key(binder) {
                    PackageTargetTopExportability::Exportable
                } else {
                    exportability
                },
                literal_admitted,
            }
        },
    );
    let retained_export =
        code_exports
            .get(binder)
            .map_or(PackageRetainedExportFact::Missing, |export| {
                PackageRetainedExportFact::Present {
                    interface_digest: export.interface_digest,
                    protected_interface_matches: matches_protected_package_interface(
                        export,
                        owner.interface_digest,
                    ),
                }
            });

    CertifiedPackageOwnerDiagnostic {
        target_globals,
        target_globals_omitted: globals.len().saturating_sub(limits.target_globals),
        target_global_count: globals.len(),
        target_owner_count: target_owners.len(),
        demanded_groups,
        demanded_groups_omitted: demanded.len().saturating_sub(selected_group_count),
        demanded_group_count: demanded.len(),
        target_top,
        retained_export,
        target_interfaces_match: interfaces_match,
        target_interface_digest: interface_digest,
    }
}

/// How many programs may install between major collections before one runs
/// regardless of byte growth. Bounds the residency test's `programs` count:
/// `programs <= live_bindings + 1 + MAJOR_COLLECTION_INSTALL_INTERVAL`.
const MAJOR_COLLECTION_INSTALL_INTERVAL: usize = 4;

/// Live old-space growth since the last major collection, in bytes, that
/// forces one early even inside the install-count window -- whichever this
/// or the 50% relative threshold in [`PreparedEngine::major_collection_due`]
/// reaches first.
const MAJOR_COLLECTION_GROWTH_BYTES: usize = 1024 * 1024;

/// The run's parking policy, carried onto the eval thread beside the settle
/// plan: what a suspension is parked with. `pub` alongside
/// [`PreparedEngine::park_suspension`], which takes it.
#[derive(Clone, Copy)]
pub struct ParkPolicy {
    pub principal: PrincipalId,
    pub effect_policy: EffectRunPolicy,
    pub live_payload: LivePayloadPolicy,
}

/// How a settled entry (the scaffold or a resume) is called: nothing is
/// observed by the call itself, and no collection is forced before the
/// layer is read.
const SETTLE_CALL: PreparedCallOptions = PreparedCallOptions {
    observation_budget: 0,
    collect_before_observation: false,
};

/// One suspension parked by [`PreparedEngine::park_suspension`]: the frame's
/// id and the observed request.
pub struct PreparedParked {
    pub id: ContinuationId,
    pub request: HaskellValue,
}

/// A parked frame re-entered by [`PreparedEngine::resume_parked`]: the
/// settled layer the resume produced, under the frame's resource scope, by the runner
/// program whose entry re-entered it (the program a further suspension is
/// parked against).
pub struct PreparedResumed {
    pub settlement: PreparedSettlement,
    pub realm: RealmId,
    pub runner: ProgramId,
}

// SAFETY: the machine is the only non-auto-`Send` field, and `PersistentSession`
// moves the engine
// between exactly one owning thread at a time (stowed XOR running).
unsafe impl Send for PreparedEngine {}

static_assertions::assert_impl_all!(PreparedEngine: Send);

/// One turn's settled computation, read from the `Settled` layer its
/// `__prepared` scaffold produced. Every handle is retained under the run's
/// resource scope until the caller adopts or releases it.
#[derive(Debug)]
pub enum PreparedSettlement {
    /// The computation completed; `value` is in weak head normal form.
    Done { value: PreparedHandle },
    /// The computation requested an effect and retained its continuation.
    Suspended {
        request: PreparedHandle,
        continuation: PreparedHandle,
    },
}

/// The live binding an artifact's declared import resolves to by identity:
/// the newest prepared binding whose recorded identity is `identity`, or the
/// one at `generation` when the artifact pins one.
///
/// `index` answers the "which id" question in O(1) (amortized) instead of
/// scanning every live binding -- see [`super::binding_table`] -- and `get`
/// on the resolved id is an ordinary `BindingTable` id lookup.
fn resolve_prepared_import<'a>(
    bindings: &'a BindingTable,
    index: &BindingIndex,
    identity: &SymbolIdentity,
    generation: Option<u64>,
) -> Option<&'a BindingEntry> {
    let id = index.resolve_prepared(identity, generation)?;
    bindings.get(id)
}

fn build_host_text_node(
    builder: &mut ManagedBuilder<'_, '_>,
    text: &str,
    text_id: DataConId,
) -> Result<ManagedNode, PreparedRuntimeError> {
    let length = i64::try_from(text.len()).map_err(|_| PreparedRuntimeError::HostMount {
        detail: "host Text exceeds the worker Int length range".into(),
    })?;
    let bytes = builder
        .bytes(text.as_bytes())
        .map_err(PreparedRuntimeError::Run)?;
    let mut zero = [0_u8; 16];
    zero[..8].copy_from_slice(&0_i64.to_ne_bytes());
    let mut len = [0_u8; 16];
    len[..8].copy_from_slice(&length.to_ne_bytes());
    builder
        .constructor(
            text_id,
            &[
                ManagedField::Consume(bytes),
                ManagedField::Scalar(zero),
                ManagedField::Scalar(len),
            ],
        )
        .map_err(PreparedRuntimeError::Run)
}

impl PreparedEngine {
    /// Capture the request site's compiler-authenticated graph before its
    /// input travels to another actor's machine session.
    pub fn request_site_type_evidence(&self, site: u64) -> Option<SiteTypeEvidence> {
        let witness = self.sites.get(&site)?;
        let facts = self.programs.get(&witness.owner)?;
        let row = facts.sites.get(witness.row)?;
        Some(SiteTypeEvidence {
            types: Arc::clone(&facts.types),
            constructors: Arc::clone(&facts.constructors),
            input: *row.inputs.first()?,
            answer: row.wire,
            request_context: None,
        })
    }

    /// Compare the original request graph with the access site installed in
    /// this machine. The third accessor input is its complete reply evidence.
    pub fn request_scope_types_match(
        &self,
        request: &SiteTypeEvidence,
        access_site: u64,
    ) -> Result<bool, TypeGraphError> {
        let Some(witness) = self.sites.get(&access_site) else {
            return Ok(false);
        };
        let Some(facts) = self.programs.get(&witness.owner) else {
            return Ok(false);
        };
        let Some(row) = facts.sites.get(witness.row) else {
            return Ok(false);
        };
        let (Some(input), Some(reply)) = (row.inputs.first(), row.inputs.get(2)) else {
            return Ok(false);
        };
        let mut budget = TypeWorkBudget::new(GraphLimits::default().max_work);
        Ok(request
            .types
            .rooted_compatible(request.input, &facts.types, *input, &mut budget)?
            && request.types.rooted_compatible(
                request.answer,
                &facts.types,
                *reply,
                &mut budget,
            )?)
    }
    /// Stream a structurally encoded host value into the resident heap. The
    /// caller supplies the compiler-authenticated constructor table for the
    /// typed binding it will mount; no source-text representation is involved.
    pub fn build_host_value(
        &mut self,
        realm: RealmId,
        value: &dyn tidepool_bridge::ToHaskell,
        table: &DataConTable,
    ) -> Result<PreparedHandle, PreparedRuntimeError> {
        let mut builder = self
            .machine
            .managed_builder()
            .map_err(PreparedRuntimeError::Run)?;
        let root = {
            let mut visitor = ManagedMountVisitor {
                builder: &mut builder,
                frames: Vec::new(),
                root: None,
            };
            value
                .visit(table, &mut visitor)
                .map_err(|error| PreparedRuntimeError::HostMount {
                    detail: error.to_string(),
                })?;
            visitor
                .finish()
                .map_err(|error| PreparedRuntimeError::HostMount {
                    detail: error.to_string(),
                })?
        };
        builder
            .finish(realm, root)
            .map_err(PreparedRuntimeError::Run)
    }

    /// Stream a host JSON document into the resident heap. The caller supplies
    /// the compiler-authenticated table for the binding it will mount; this
    /// method deliberately has no source-text representation or parser path.
    pub fn build_host_json(
        &mut self,
        realm: RealmId,
        value: &serde_json::Value,
        layout: &JsonLayout<DataConId>,
    ) -> Result<PreparedHandle, PreparedRuntimeError> {
        let mut builder = self
            .machine
            .managed_builder()
            .map_err(PreparedRuntimeError::Run)?;
        let root = {
            let mut visitor = ManagedMountVisitor {
                builder: &mut builder,
                frames: Vec::new(),
                root: None,
            };
            tidepool_bridge::json_builder::visit_json(value, layout, &mut visitor).map_err(
                |error| PreparedRuntimeError::HostMount {
                    detail: error.to_string(),
                },
            )?;
            visitor
                .finish()
                .map_err(|error| PreparedRuntimeError::HostMount {
                    detail: error.to_string(),
                })?
        };
        builder
            .finish(realm, root)
            .map_err(PreparedRuntimeError::Run)
    }

    /// Build Text with the physical constructor already admitted by HostCarrier.
    pub(super) fn build_host_text_exact(
        &mut self,
        realm: RealmId,
        text: &str,
        text_id: DataConId,
    ) -> Result<PreparedHandle, PreparedRuntimeError> {
        let mut builder = self
            .machine
            .managed_builder()
            .map_err(PreparedRuntimeError::Run)?;
        let root = build_host_text_node(&mut builder, text, text_id)?;
        builder
            .finish(realm, root)
            .map_err(PreparedRuntimeError::Run)
    }

    pub(super) fn build_host_job_exact(
        &mut self,
        realm: RealmId,
        text: &str,
        job_id: DataConId,
        text_id: DataConId,
    ) -> Result<PreparedHandle, PreparedRuntimeError> {
        let mut builder = self
            .machine
            .managed_builder()
            .map_err(PreparedRuntimeError::Run)?;
        let text = build_host_text_node(&mut builder, text, text_id)?;
        let root = builder
            .constructor(job_id, &[ManagedField::Consume(text)])
            .map_err(PreparedRuntimeError::Run)?;
        builder
            .finish(realm, root)
            .map_err(PreparedRuntimeError::Run)
    }

    /// Share `registry` with this engine's machine: every later install
    /// consults it before compiling (see [`Self::install`] and the
    /// off-checkout split), and the compiling install's image is the one
    /// every other machine holding the same `Arc<ImageRegistry>` reuses.
    /// Set once by the composition root that owns a run's machines (e.g. a
    /// `ChildSessionFactory`); `None` (the default) keeps every install
    /// compiling its own image, exactly as before this existed.
    pub fn set_image_registry(&mut self, registry: Arc<ImageRegistry>) {
        self.registry = Some(registry);
    }

    /// The registry this engine shares its installs with, if any.
    #[must_use]
    pub fn image_registry(&self) -> Option<&Arc<ImageRegistry>> {
        self.registry.as_ref()
    }

    /// Compile `linked` (or reuse an already-compiled image) and install it,
    /// consulting [`Self::registry`] when this engine has one. Shared by
    /// [`Self::install`]'s single-checkout path.
    fn compile_and_install(
        &mut self,
        linked: tidepool_repr::execution_schema::LinkedProgram,
        imports: ImportBindings,
    ) -> Result<(ProgramId, Arc<DefinitionFacts>), PreparedRuntimeError> {
        let Some(registry) = self.registry.clone() else {
            let compiled = self
                .machine
                .compile_for_install(&linked)
                .map_err(PreparedRuntimeError::Compile)?;
            let definitions = Arc::clone(compiled.definition_facts());
            let program = self
                .machine
                .install_program(compiled, imports)
                .map_err(PreparedRuntimeError::Install)?;
            return Ok((program, definitions));
        };
        let image = match registry.lookup(&linked) {
            Some(image) => image,
            None => {
                let compiled = self
                    .machine
                    .compile_for_install(&linked)
                    .map_err(PreparedRuntimeError::Compile)?;
                registry.insert(linked, Arc::new(compiled))
            }
        };
        let definitions = Arc::clone(image.definition_facts());
        let program = self
            .machine
            .install_shared(image, imports)
            .map_err(PreparedRuntimeError::Install)?;
        Ok((program, definitions))
    }

    /// Create the session's machine from its first turn's program and
    /// install that program. The first turn can import nothing: no prepared
    /// binding exists before the machine does. Uses the default nursery
    /// size; see [`Self::bootstrap_with_nursery_bytes`] for a caller-chosen
    /// size (e.g. a session's configured `nursery_size`, or a one-shot
    /// caller that wants a small nursery to force collections).
    pub fn bootstrap(prepared: PreparedProgram) -> Result<(Self, ProgramId), PreparedRuntimeError> {
        Self::bootstrap_with_nursery_bytes(prepared, RunOptions::default().nursery_bytes)
    }

    /// As [`Self::bootstrap`], with an explicit nursery size instead of the
    /// default.
    pub fn bootstrap_with_nursery_bytes(
        prepared: PreparedProgram,
        nursery_bytes: usize,
    ) -> Result<(Self, ProgramId), PreparedRuntimeError> {
        Self::bootstrap_shared(prepared, nursery_bytes, None)
    }

    /// As [`Self::bootstrap_with_nursery_bytes`], installing the first
    /// program through `registry` when one is given: a machine bootstrapped
    /// from a program another machine already compiled installs that same
    /// image, so the two never hold two copies of one program's descriptors
    /// and statics. The engine keeps the registry for every later install.
    pub fn bootstrap_shared(
        prepared: PreparedProgram,
        nursery_bytes: usize,
        registry: Option<Arc<ImageRegistry>>,
    ) -> Result<(Self, ProgramId), PreparedRuntimeError> {
        let entry = prepared.entry();
        let exports = exportable_code_tops(&prepared);
        let linked = link_program(prepared, &MachineImports::default())?;
        let image = match &registry {
            Some(registry) => match registry.lookup(&linked) {
                Some(image) => image,
                None => {
                    let compiled =
                        CompiledProgram::compile(&linked).map_err(PreparedRuntimeError::Compile)?;
                    registry.insert(linked, Arc::new(compiled))
                }
            },
            None => {
                Arc::new(CompiledProgram::compile(&linked).map_err(PreparedRuntimeError::Compile)?)
            }
        };
        let facts = ProgramFacts::from_image(&image, Some(entry));
        let (machine, program) =
            PreparedMachine::new_shared(image, PreparedMachineOptions { nursery_bytes })
                .map_err(PreparedRuntimeError::Install)?;
        let mut engine = Self::from_machine(machine, registry);
        // The first program can conflict only with itself.
        let admitted = engine.admit_program_facts(facts)?;
        engine.finish_program_install(program, admitted, exports)?;
        Ok((engine, program))
    }

    fn from_machine(
        machine: PreparedMachine<'static>,
        registry: Option<Arc<ImageRegistry>>,
    ) -> Self {
        Self {
            machine,
            programs: BTreeMap::new(),
            sites: BTreeMap::new(),
            constructor_replies: BTreeMap::new(),
            old_bytes: 0,
            installs_since_major: 0,
            old_bytes_at_last_major: 0,
            major_collections: 0,
            code_exports: BTreeMap::new(),
            registry,
        }
    }

    /// Bootstrap an empty native machine for a certified target plus its
    /// demanded source groups. The batch installer publishes them together;
    /// no provisional legacy program or metadata becomes visible first.
    pub(crate) fn empty_certified(
        nursery_bytes: usize,
        registry: Option<Arc<ImageRegistry>>,
    ) -> Result<Self, PreparedRuntimeError> {
        let machine = PreparedMachine::empty(PreparedMachineOptions { nursery_bytes })
            .map_err(PreparedRuntimeError::Install)?;
        Ok(Self::from_machine(machine, registry))
    }

    /// The rows of `facts` that installing it would make canonical: every
    /// site no installed program declares yet. A site already installed (by
    /// an earlier program, or earlier in `facts` itself) is accepted only when
    /// the two rows carry the same evidence ([`sites_equivalent`]); the
    /// existing owner stays canonical and nothing is added for it. A
    /// conflicting duplicate is [`PreparedRuntimeError::SiteConflict`] and the
    /// caller installs nothing.
    fn plan_sites(&self, facts: &ProgramFacts) -> Result<Vec<(u64, usize)>, PreparedRuntimeError> {
        let mut planned: Vec<(u64, usize)> = Vec::new();
        for (row, site) in facts.sites.iter().enumerate() {
            let (owner, owner_facts, owner_row) = if let Some(witness) = self.sites.get(&site.site)
            {
                let owner =
                    self.programs
                        .get(&witness.owner)
                        .ok_or(PreparedRuntimeError::Install(
                            ExecutionError::UnknownProgram(witness.owner),
                        ))?;
                (Some(witness.owner), owner, &owner.sites[witness.row])
            } else if let Some((_, earlier)) = planned.iter().find(|(id, _)| *id == site.site) {
                (None, facts, &facts.sites[*earlier])
            } else {
                planned.push((site.site, row));
                continue;
            };
            if !sites_equivalent(owner_facts, owner_row, facts, site)? {
                return Err(match owner {
                    Some(owner) => PreparedRuntimeError::SiteConflict {
                        site: site.site,
                        owner,
                    },
                    None => PreparedRuntimeError::DuplicateSite { site: site.site },
                });
            }
        }
        Ok(planned)
    }

    fn plan_constructor_replies(
        &self,
        facts: &ProgramFacts,
    ) -> Result<Vec<(DataConId, usize)>, PreparedRuntimeError> {
        let mut planned: Vec<(DataConId, usize)> = Vec::new();
        for (row, &(host_id, reply)) in facts.constructor_replies.iter().enumerate() {
            let (owner, owner_facts, owner_reply) =
                if let Some(witness) = self.constructor_replies.get(&host_id) {
                    let owner =
                        self.programs
                            .get(&witness.owner)
                            .ok_or(PreparedRuntimeError::Install(
                                ExecutionError::UnknownProgram(witness.owner),
                            ))?;
                    (
                        Some(witness.owner),
                        owner,
                        owner.constructor_replies[witness.row].1,
                    )
                } else if let Some((_, earlier)) = planned.iter().find(|(id, _)| *id == host_id) {
                    (None, facts, facts.constructor_replies[*earlier].1)
                } else {
                    planned.push((host_id, row));
                    continue;
                };
            if !constructor_replies_equivalent(owner_facts, owner_reply, facts, reply)? {
                return Err(match owner {
                    Some(owner) => PreparedRuntimeError::ConstructorReplyConflict {
                        constructor: host_id,
                        owner,
                        evidence: reply_conflict_evidence(
                            host_id,
                            owner_facts,
                            owner_reply,
                            facts,
                            reply,
                        ),
                    },
                    None => PreparedRuntimeError::DuplicateConstructorReply {
                        constructor: host_id,
                        evidence: reply_conflict_evidence(
                            host_id,
                            owner_facts,
                            owner_reply,
                            facts,
                            reply,
                        ),
                    },
                });
            }
        }
        Ok(planned)
    }

    /// Both indexes' plans for installing `facts`, checked before anything
    /// is compiled or published.
    fn plan_evidence(&self, facts: &ProgramFacts) -> Result<EvidencePlan, PreparedRuntimeError> {
        Ok(EvidencePlan {
            sites: self.plan_sites(facts)?,
            constructor_replies: self.plan_constructor_replies(facts)?,
        })
    }

    fn admit_program_facts(
        &self,
        facts: ProgramFacts,
    ) -> Result<AdmittedProgramFacts, PreparedRuntimeError> {
        let plan = self.plan_evidence(&facts)?;
        Ok(AdmittedProgramFacts { facts, plan })
    }

    /// Check duplicate site and verb ownership across a batch before any
    /// machine mutation. Equal evidence keeps its first planned owner.
    fn plan_batch_evidence(
        &self,
        facts: Vec<ProgramFacts>,
    ) -> Result<Vec<AdmittedProgramFacts>, PreparedRuntimeError> {
        let mut sites = BTreeMap::<u64, (usize, usize)>::new();
        let mut verbs = BTreeMap::<DataConId, (usize, usize)>::new();
        let mut plans = Vec::with_capacity(facts.len());
        for (group, group_facts) in facts.iter().enumerate() {
            let mut plan = self.plan_evidence(group_facts)?;
            let mut accepted_sites = Vec::new();
            for (site, row) in plan.sites.drain(..) {
                if let Some(&(prior_group, prior_row)) = sites.get(&site) {
                    if !sites_equivalent(
                        &facts[prior_group],
                        &facts[prior_group].sites[prior_row],
                        group_facts,
                        &group_facts.sites[row],
                    )? {
                        return Err(PreparedRuntimeError::DuplicateSite { site });
                    }
                } else {
                    sites.insert(site, (group, row));
                    accepted_sites.push((site, row));
                }
            }
            plan.sites = accepted_sites;
            let mut accepted_verbs = Vec::new();
            for (host_id, row) in plan.constructor_replies.drain(..) {
                if let Some(&(prior_group, prior_row)) = verbs.get(&host_id) {
                    if !constructor_replies_equivalent(
                        &facts[prior_group],
                        facts[prior_group].constructor_replies[prior_row].1,
                        group_facts,
                        group_facts.constructor_replies[row].1,
                    )? {
                        return Err(PreparedRuntimeError::DuplicateConstructorReply {
                            constructor: host_id,
                            evidence: reply_conflict_evidence(
                                host_id,
                                &facts[prior_group],
                                facts[prior_group].constructor_replies[prior_row].1,
                                group_facts,
                                group_facts.constructor_replies[row].1,
                            ),
                        });
                    }
                } else {
                    verbs.insert(host_id, (group, row));
                    accepted_verbs.push((host_id, row));
                }
            }
            plan.constructor_replies = accepted_verbs;
            plans.push(plan);
        }
        Ok(facts
            .into_iter()
            .zip(plans)
            .map(|(facts, plan)| AdmittedProgramFacts { facts, plan })
            .collect())
    }

    /// Retain exports before any session metadata or source custody publishes.
    /// An existing export keeps its root. Optional absence is benign only on a
    /// reusable machine; retention failures release this stage's earlier roots
    /// and return the original error without publishing a partial export map.
    fn stage_code_exports(
        &mut self,
        program: ProgramId,
        exports: Vec<(SymbolIdentity, ValueId, Option<Signature>)>,
        required: &BTreeMap<SymbolIdentity, (ValueId, [u8; 32])>,
        certified_exports: &BTreeMap<SymbolIdentity, [u8; 32]>,
    ) -> Result<BTreeMap<SymbolIdentity, CodeExport>, PreparedRuntimeError> {
        let mut staged = BTreeMap::<SymbolIdentity, CodeExport>::new();
        for (identity, value, entry) in exports {
            if self.code_exports.contains_key(&identity) {
                continue;
            }
            let handle = match self.machine.retain_export_top(program, value) {
                Ok(handle) => handle,
                Err(ExecutionError::MissingEntry(_))
                    if !required.contains_key(&identity)
                        && self.machine.disposition() == MachineDisposition::Reusable =>
                {
                    continue;
                }
                Err(error) => {
                    for export in staged.into_values() {
                        assert!(
                            self.release(export.handle),
                            "failed export stage retains its earlier root"
                        );
                    }
                    return Err(PreparedRuntimeError::Install(error));
                }
            };
            // Recovered package definitions may have no incoming edge in this
            // target. Their export provenance comes from this same sealed
            // target's package closure, before a later target imports them.
            let interface_digest = certified_exports
                .get(&identity)
                .copied()
                .or_else(|| required.get(&identity).map(|(_, digest)| *digest));
            staged.insert(
                identity,
                CodeExport {
                    program,
                    handle,
                    entry,
                    interface_digest,
                },
            );
        }
        Ok(staged)
    }

    /// Complete an ordinary install at its fallible owner. Its pin protects
    /// the install-to-first-run gap; no retention runs after publication.
    fn finish_program_install(
        &mut self,
        program: ProgramId,
        admitted: AdmittedProgramFacts,
        exports: Vec<(SymbolIdentity, ValueId, Option<Signature>)>,
    ) -> Result<(), PreparedRuntimeError> {
        self.machine
            .pin(program)
            .map_err(PreparedRuntimeError::Install)?;
        let exports =
            match self.stage_code_exports(program, exports, &BTreeMap::new(), &BTreeMap::new()) {
                Ok(exports) => exports,
                Err(error) => {
                    assert!(self.unpin(program), "refused install retains its pin");
                    // Integrity failures keep native code/heap custody intact.
                    // Cleanup must neither collect an unavailable machine nor
                    // replace the first retention failure with a cleanup failure.
                    if self.machine.disposition() == MachineDisposition::Reusable {
                        if let Err(cleanup) = self.quiesce_and_collect_now() {
                            tracing::warn!(
                                ?cleanup,
                                "failed to collect refused export installation"
                            );
                        }
                    }
                    return Err(error);
                }
            };
        self.publish_admitted_program(program, admitted);
        self.code_exports.extend(exports);
        self.installs_since_major += 1;
        Ok(())
    }

    /// Every package top this machine already carries, as the extractor
    /// wants them: `(identity, generation)` pairs whose bodies the next
    /// turn's projection drops in favour of a declared global.
    pub(crate) fn code_export_retentions(
        &self,
    ) -> impl Iterator<Item = (SymbolIdentity, u64)> + '_ {
        self.code_exports
            .keys()
            .map(|identity| (identity.clone(), CODE_EXPORT_GENERATION))
    }

    /// Only live exports with an authenticated package interface may cause a
    /// checked compiler projection to omit their native definitions. Ordinary
    /// graph installs also retain package tops, but do not issue that proof.
    pub(crate) fn protected_code_export_retentions(
        &self,
    ) -> impl Iterator<Item = (SymbolIdentity, u64)> + '_ {
        self.code_exports.iter().filter_map(|(identity, export)| {
            (export
                .interface_digest
                .is_some_and(|digest| digest != [0; 32])
                && self.machine.prepared_handle_of(export.handle.raw()) == Some(export.handle))
            .then(|| (identity.clone(), CODE_EXPORT_GENERATION))
        })
    }

    /// Resolve an advertised immutable export through this engine's exact live
    /// ledger. Native `ValueHandle` identities are issued process-wide and
    /// never reused; retaining that identity fences a later install against a
    /// different machine or a replacement root, even at the same generation.
    pub(crate) fn retained_code_export_owner(
        &self,
        identity: &SymbolIdentity,
        generation: u64,
    ) -> Option<ImportOwner> {
        if generation != CODE_EXPORT_GENERATION {
            return None;
        }
        let export = self.code_exports.get(identity)?;
        (self.machine.prepared_handle_of(export.handle.raw()) == Some(export.handle)).then(|| {
            ImportOwner::CodeExport {
                binder: identity.clone(),
                generation,
                root_id: export.handle.raw().0,
                interface_digest: export.interface_digest,
            }
        })
    }

    pub(crate) fn retained_package_code_export_owner(
        &self,
        identity: &SymbolIdentity,
        generation: u64,
        interface_digest: &[u8; 32],
    ) -> Option<ImportOwner> {
        let export = self.code_exports.get(identity)?;
        if !matches_protected_package_interface(export, interface_digest) {
            return None;
        }
        self.retained_code_export_owner(identity, generation)
    }

    pub(crate) fn retained_code_export_owner_installed_by(
        &self,
        identity: &SymbolIdentity,
        program: ProgramId,
    ) -> Option<ImportOwner> {
        (self.code_exports.get(identity)?.program == program)
            .then(|| self.retained_code_export_owner(identity, CODE_EXPORT_GENERATION))
            .flatten()
    }

    fn code_export_import(
        &self,
        owner: CodeExportOwnerRef<'_>,
        declaration: &tidepool_repr::execution_schema::GlobalDecl,
    ) -> Result<BatchImport, PreparedRuntimeError> {
        let CodeExportOwnerRef {
            binder,
            generation,
            root_id,
            interface_digest,
        } = owner;
        let export = self
            .code_exports
            .get(binder)
            .filter(|export| {
                binder == &declaration.identity
                    && declaration.required_generation == Some(generation)
                    && generation == CODE_EXPORT_GENERATION
                    && export.handle.raw().0 == root_id
                    && export.handle.rep() == declaration.rep
                    && interface_digest
                        .is_none_or(|digest| matches_protected_package_interface(export, digest))
                    && self.machine.prepared_handle_of(export.handle.raw()) == Some(export.handle)
            })
            .ok_or_else(|| PreparedRuntimeError::MissingCertifiedOwner(owner.to_owned()))?;
        // The batch owner checks the declared callable signature and required
        // evaluatedness against this producer's signature and live native root.
        Ok(BatchImport::Existing {
            handle: export.handle,
            entry_signature: export.entry.clone(),
        })
    }

    /// How many package tops later turns can import instead of recompiling.
    #[must_use]
    pub fn code_export_count(&self) -> usize {
        self.code_exports.len()
    }

    fn publish_admitted_program(
        &mut self,
        program: ProgramId,
        admitted: AdmittedProgramFacts,
    ) -> bool {
        let added = match self.programs.entry(program) {
            std::collections::btree_map::Entry::Vacant(entry) => {
                entry.insert(admitted.facts);
                true
            }
            std::collections::btree_map::Entry::Occupied(_) => false,
        };
        self.publish_evidence(program, admitted.plan);
        added
    }

    /// Publish canonical site and constructor reply ownership together.
    fn publish_evidence(&mut self, program: ProgramId, plan: EvidencePlan) {
        let witness = |row| SiteWitness {
            owner: program,
            row,
        };
        self.sites.extend(
            plan.sites
                .into_iter()
                .map(|(site, row)| (site, witness(row))),
        );
        self.constructor_replies.extend(
            plan.constructor_replies
                .into_iter()
                .map(|(host_id, row)| (host_id, witness(row))),
        );
    }

    /// Install a later turn's program. Every global it declares is resolved
    /// to a live prepared binding in `bindings` by the identity recorded at
    /// bind time (at the declared `required_generation`, or the newest), and
    /// the artifact is linked against those bindings' live shape before
    /// anything is compiled, so a stale generation or an unresolvable
    /// identity is a typed link error with no machine side effect.
    pub(crate) fn install(
        &mut self,
        prepared: PreparedProgram,
        bindings: &BindingTable,
        index: &BindingIndex,
    ) -> Result<ProgramId, PreparedRuntimeError> {
        let mut clock = std::time::Instant::now();
        let mut lap = || {
            let now = std::time::Instant::now();
            let elapsed = now.duration_since(clock);
            clock = now;
            elapsed.as_millis() as u64
        };
        let (values, imports) = self.resolve_imports(&prepared, bindings, index)?;
        let resolve_imports_ms = lap();
        let import_count = imports.len();
        let exports = exportable_code_tops(&prepared);
        let facts = ProgramFacts::of(&prepared);
        // Site evidence is checked before anything is compiled or published:
        // a conflicting duplicate leaves the machine, its programs and the
        // site index exactly as they were.
        let mut admitted = self.admit_program_facts(facts)?;
        let evidence_ms = lap();
        let linked = link_program(prepared, &values)?;
        let link_ms = lap();
        // `compile_and_install` consults `self.registry` (when this engine
        // has one) before compiling: a hit installs the already-compiled
        // image through `install_shared` and compiles nothing, so
        // `compile_ms` below also covers a registry lookup on the hit path.
        let (program, definitions) = self.compile_and_install(linked, imports)?;
        admitted.facts.definitions = definitions;
        let compile_install_ms = lap();
        tracing::info!(
            target: "tidepool_runtime::prepared_install",
            resolve_imports_ms,
            evidence_ms,
            link_ms,
            compile_install_ms,
            imports = import_count,
            compiled_off_checkout = false,
            "prepared install"
        );
        self.finish_program_install(program, admitted, exports)?;
        Ok(program)
    }

    /// Resolve `prepared`'s declared globals against `bindings`/`index` (and
    /// this machine's package-top exports), one live-handle fact snapshot
    /// per import: `MachineImports` for `link_program`, `ImportBindings` for
    /// `install_program`. Shared by the single-checkout [`Self::install`]
    /// and both ends of the off-checkout split
    /// ([`Self::snapshot_install`]/[`Self::revalidate_and_install`]) so a
    /// snapshot and its revalidation resolve imports exactly the same way.
    fn resolve_imports(
        &self,
        prepared: &PreparedProgram,
        bindings: &BindingTable,
        index: &BindingIndex,
    ) -> Result<(MachineImports, ImportBindings), PreparedRuntimeError> {
        let mut values = MachineImports::default();
        let mut imports = ImportBindings::new();
        for declaration in prepared.globals() {
            let identity = &declaration.identity;
            let Some(entry) =
                resolve_prepared_import(bindings, index, identity, declaration.required_generation)
            else {
                // Not a session value binding: it may be a package top an
                // earlier turn already installed and this turn was told to
                // import (see `code_exports`). Absent from both: left out,
                // and `link_program` reports the typed `MissingImport`. This
                // is the legacy ordinary resolver; protected package imports
                // use exact `ImportOwner::Package` provenance below.
                if let Some(export) = self.code_exports.get(identity).cloned() {
                    let evaluated = self
                        .machine
                        .handle_is_evaluated(export.handle)
                        .map_err(PreparedRuntimeError::Install)?;
                    values.values.insert(
                        identity.clone(),
                        ImportedValue {
                            identity: identity.clone(),
                            rep: export.handle.rep(),
                            entry_signature: export.entry,
                            evaluated,
                            generation: CODE_EXPORT_GENERATION,
                        },
                    );
                    imports.insert(identity.clone(), export.handle);
                }
                continue;
            };
            let BoundValue { handle, .. } = &entry.value;
            let handle = *handle;
            let evaluated = self
                .machine
                .handle_is_evaluated(handle)
                .map_err(PreparedRuntimeError::Install)?;
            values.values.insert(
                identity.clone(),
                ImportedValue {
                    identity: identity.clone(),
                    rep: handle.rep(),
                    entry_signature: None,
                    evaluated,
                    generation: entry.module.gen().0,
                },
            );
            imports.insert(identity.clone(), handle);
        }
        Ok((values, imports))
    }

    /// Install an off-checkout compiled, worker-certified source closure.
    /// The caller supplies external handles under their exact retained or
    /// package owner, including the package interface digest; the legacy
    /// name-keyed package export table is not an owner authority here.
    /// Every returned program is pinned across the install-to-bind gap.
    pub fn install_certified_demand(
        &mut self,
        demanded: Vec<DemandedImage>,
        exact_external: &HashMap<ImportOwner, PreparedHandle>,
        bindings: &BindingTable,
    ) -> Result<Vec<ProgramId>, PreparedRuntimeError> {
        let mut source = BTreeMap::<SourceBinder, (usize, ValueId)>::new();
        for (index, selected) in demanded.iter().enumerate() {
            let group = selected.group();
            let definitions = group.definitions();
            for binder in group.binders() {
                let top = definitions.bindings().iter().find_map(|binding| {
                    let tops = match binding {
                        Group::NonRecursive(top) => std::slice::from_ref(top),
                        Group::Recursive(tops) => tops.as_slice(),
                    };
                    tops.iter().find(|top| &top.identity == binder)
                });
                let key = SourceBinder {
                    version: group.owner().module_version.clone(),
                    binder: binder.clone(),
                };
                let id = top
                    .ok_or_else(|| DemandError::MissingSource(key.clone()))?
                    .binding
                    .id;
                if source.insert(key.clone(), (index, id)).is_some() {
                    return Err(DemandError::DuplicateBinder(key).into());
                }
            }
        }
        let facts: Vec<_> = demanded
            .iter()
            .map(|selected| ProgramFacts::from_image(selected.image(), None))
            .collect();
        let admitted = self.plan_batch_evidence(facts)?;
        let mut programs = Vec::with_capacity(demanded.len());
        let mut package_updates = BTreeMap::new();
        for selected in &demanded {
            let group = selected.group();
            let mut imports = Vec::with_capacity(group.imports().len());
            for (declaration, owner) in group.definitions().globals().iter().zip(group.imports()) {
                match owner {
                    ImportOwner::Source { version, binder } => {
                        let key = SourceBinder {
                            version: version.clone(),
                            binder: binder.clone(),
                        };
                        let &(group, binding) = source
                            .get(&key)
                            .ok_or_else(|| DemandError::MissingSource(key.clone()))?;
                        imports.push(BatchImport::Source { group, binding });
                    }
                    ImportOwner::Retained { id, generation } => {
                        let entry = bindings.get(*id).ok_or_else(|| {
                            PreparedRuntimeError::MissingCertifiedOwner(owner.clone())
                        })?;
                        let handle = exact_external.get(owner).copied().ok_or_else(|| {
                            PreparedRuntimeError::MissingCertifiedOwner(owner.clone())
                        })?;
                        if entry.module.gen().0 != *generation
                            || entry.value.identity != declaration.identity
                            || entry.value.handle != handle
                        {
                            return Err(PreparedRuntimeError::MissingCertifiedOwner(owner.clone()));
                        }
                        imports.push(BatchImport::Existing {
                            handle,
                            entry_signature: None,
                        });
                    }
                    ImportOwner::CodeExport {
                        binder,
                        generation,
                        root_id,
                        interface_digest,
                    } => {
                        imports.push(self.code_export_import(
                            CodeExportOwnerRef {
                                binder,
                                generation: *generation,
                                root_id: *root_id,
                                interface_digest: interface_digest.as_ref(),
                            },
                            declaration,
                        )?);
                    }
                    ImportOwner::Package {
                        unit,
                        module,
                        binder,
                        interface_digest,
                    } => {
                        if *interface_digest == [0; 32] {
                            return Err(PreparedRuntimeError::MissingCertifiedOwner(owner.clone()));
                        }
                        let handle = exact_external.get(owner).copied().ok_or_else(|| {
                            PreparedRuntimeError::MissingCertifiedOwner(owner.clone())
                        })?;
                        let export = self
                            .code_exports
                            .get(binder)
                            .filter(|export| {
                                export.handle == handle
                                    && declaration.identity == *binder
                                    && binder.unit == *unit
                                    && binder.module == *module
                                    && matches_protected_package_interface(export, interface_digest)
                            })
                            .ok_or_else(|| {
                                PreparedRuntimeError::MissingCertifiedOwner(owner.clone())
                            })?;
                        if package_updates
                            .insert(binder.clone(), *interface_digest)
                            .is_some_and(|previous| previous != *interface_digest)
                        {
                            return Err(PreparedRuntimeError::MissingCertifiedOwner(owner.clone()));
                        }
                        imports.push(BatchImport::Existing {
                            handle,
                            entry_signature: export.entry.clone(),
                        });
                    }
                }
            }
            programs.push(BatchProgram {
                image: Arc::clone(selected.image()),
                imports,
            });
        }

        let ids = self
            .machine
            .install_shared_batch(programs)
            .map_err(PreparedRuntimeError::Install)?;
        for (binder, digest) in package_updates {
            self.code_exports
                .get_mut(&binder)
                .expect("preflighted package export remains installed")
                .interface_digest = Some(digest);
        }
        assert_eq!(
            ids.len(),
            admitted.len(),
            "installed batch matches admitted definitions"
        );
        for (id, admitted) in ids.iter().copied().zip(admitted) {
            self.machine
                .pin(id)
                .expect("batch returned an installed program");
            self.publish_admitted_program(id, admitted);
        }
        self.installs_since_major += ids.len();
        Ok(ids)
    }

    pub(crate) fn admit_source_instances(
        &self,
        bindings: &mut BindingTable,
        scopes: &tidepool_codegen::scope::ScopeTree,
        scope: tidepool_codegen::scope::ScopeId,
        attachments: Vec<SourceInstanceAttachment>,
    ) -> Result<tidepool_codegen::binding_table::SourceScopeAdmission, Vec<SourceInstanceAttachment>>
    {
        bindings.register_source_instances_in_domains(scopes, scope, attachments, &self.machine)
    }

    /// Install one executable target and only its newly demanded certified
    /// source groups in a single native transaction. Existing source imports
    /// are selected by exact lexical instance leases supplied by the caller;
    /// this method never searches a machine-global mutable-instance cache.
    /// The returned source tokens must be admitted to scoped custody before
    /// the pins on `groups` are released.
    pub(crate) fn install_certified_turn(
        &mut self,
        target: CertifiedTargetImage,
        target_owners: &[ImportOwner],
        source_evidence: &BTreeMap<SourceBinder, (CachedHomeOwner, u32)>,
        demanded: Vec<DemandedImage>,
        inherited_needed: &[InheritedSourceDemand],
        inherited: &BTreeMap<ScopedSourceBinder, SourceInstanceLease>,
        exact_external: &HashMap<ImportOwner, PreparedHandle>,
        bindings: &BindingTable,
    ) -> Result<CertifiedTurnInstall, PreparedRuntimeError> {
        let mut target_exports: BTreeMap<_, _> = exportable_code_tops(&target.prepared)
            .into_iter()
            .map(|(identity, value, _)| (identity, value))
            .collect();
        // Literal package imports are native image inputs, not managed exports.
        // Only the sealed target's admitted literal tokens select these rows.
        target_exports.extend(
            target
                .prepared
                .bindings()
                .iter()
                .flat_map(|group| match group {
                    Group::NonRecursive(top) => std::slice::from_ref(top),
                    Group::Recursive(tops) => tops.as_slice(),
                })
                .filter(|top| {
                    top.identity.unit != HOME_UNIT
                        && top.identity.namespace == "value"
                        && matches!(top.binding.rhs, HeapRhs::Bytes(_))
                        && target.package_literals.contains_key(&top.identity)
                })
                .map(|top| (top.identity.clone(), top.binding.id)),
        );
        let mut target_packages = BTreeMap::new();
        let matches_target = target.package_interfaces.matches_target(&target.prepared);
        let certified_exports = target_exports
            .keys()
            .filter_map(|binder| {
                matches_target
                    .then(|| {
                        target
                            .package_interfaces
                            .interface_digest(&binder.unit, &binder.module)
                    })
                    .flatten()
                    .filter(|digest| *digest != [0; 32])
                    .map(|digest| (binder.clone(), digest))
            })
            .collect();
        for owner in target_owners.iter().chain(
            demanded
                .iter()
                .flat_map(|selected| selected.group().imports()),
        ) {
            let ImportOwner::Package {
                unit,
                module,
                binder,
                interface_digest,
            } = owner
            else {
                continue;
            };
            if *interface_digest == [0; 32] {
                return Err(PreparedRuntimeError::MissingCertifiedOwner(owner.clone()));
            }
            if let Some(export) = self.code_exports.get(binder) {
                if !matches_protected_package_interface(export, interface_digest) {
                    return Err(PreparedRuntimeError::CertifiedPackageOwnerUnavailable {
                        owner: owner.clone(),
                        evidence: CertifiedPackageOwnerEvidence::RetainedExport {
                            interface_digest: export.interface_digest,
                        },
                    });
                }
                continue;
            }
            let target_definition = target_exports.get(binder).copied();
            let target_interface_digest = target.package_interfaces.interface_digest(unit, module);
            let value = target_definition
                .filter(|_| {
                    binder.unit == *unit
                        && binder.module == *module
                        && matches_target
                        && target_interface_digest == Some(*interface_digest)
                })
                .ok_or_else(|| PreparedRuntimeError::CertifiedPackageOwnerUnavailable {
                    owner: owner.clone(),
                    evidence: CertifiedPackageOwnerEvidence::TargetDefinition {
                        present: target_definition.is_some(),
                        interfaces_match: matches_target,
                        interface_digest: target_interface_digest,
                        diagnostic: Some(Box::new(certified_package_owner_diagnostic(
                            &target,
                            target_owners,
                            &demanded,
                            PackageOwnerRef {
                                unit,
                                module,
                                binder,
                                interface_digest,
                            },
                            &target_exports,
                            &self.code_exports,
                            matches_target,
                            target_interface_digest,
                            PackageOwnerDiagnosticLimits::default(),
                        ))),
                    },
                })?;
            if let Some(previous) =
                target_packages.insert(binder.clone(), (value, *interface_digest))
            {
                if previous != (value, *interface_digest) {
                    return Err(PreparedRuntimeError::CertifiedPackageOwnerUnavailable {
                        owner: owner.clone(),
                        evidence: CertifiedPackageOwnerEvidence::ConflictingTargetDefinition {
                            previous_binding: previous.0,
                            requested_binding: value,
                            previous_interface_digest: previous.1,
                            requested_interface_digest: *interface_digest,
                        },
                    });
                }
            }
        }
        self.install_certified_turn_admitted(
            target,
            target_owners,
            source_evidence,
            demanded,
            inherited_needed,
            inherited,
            exact_external,
            bindings,
            target_packages,
            certified_exports,
        )
    }

    // The only production caller supplies package tops admitted against the
    // protected target/interface certificate above. Native installation still
    // checks their full identity, representation and callable signature.
    fn install_certified_turn_admitted(
        &mut self,
        target: CertifiedTargetImage,
        target_owners: &[ImportOwner],
        source_evidence: &BTreeMap<SourceBinder, (CachedHomeOwner, u32)>,
        demanded: Vec<DemandedImage>,
        inherited_needed: &[InheritedSourceDemand],
        inherited: &BTreeMap<ScopedSourceBinder, SourceInstanceLease>,
        exact_external: &HashMap<ImportOwner, PreparedHandle>,
        bindings: &BindingTable,
        target_packages: BTreeMap<SymbolIdentity, (ValueId, [u8; 32])>,
        certified_exports: BTreeMap<SymbolIdentity, [u8; 32]>,
    ) -> Result<CertifiedTurnInstall, PreparedRuntimeError> {
        if !target_owners_match(&target.prepared, target_owners) {
            return Err(PreparedRuntimeError::CertifiedTargetOwners);
        }

        let mut source = BTreeMap::<ScopedSourceBinder, (usize, ValueId)>::new();
        for (index, selected) in demanded.iter().enumerate() {
            let group = selected.group();
            for binder in group.binders() {
                let id = group
                    .definitions()
                    .bindings()
                    .iter()
                    .flat_map(|binding| match binding {
                        Group::NonRecursive(top) => std::slice::from_ref(top),
                        Group::Recursive(tops) => tops.as_slice(),
                    })
                    .find(|top| &top.identity == binder)
                    .map(|top| top.binding.id)
                    .ok_or_else(|| {
                        DemandError::MissingSource(SourceBinder {
                            version: group.owner().module_version.clone(),
                            binder: binder.clone(),
                        })
                    })?;
                let key = ScopedSourceBinder {
                    domain: selected.domain(),
                    source: SourceBinder {
                        version: group.owner().module_version.clone(),
                        binder: binder.clone(),
                    },
                };
                if inherited.contains_key(&key) || source.insert(key.clone(), (index, id)).is_some()
                {
                    return Err(DemandError::DuplicateBinder(key.source).into());
                }
            }
        }

        let mut late_sources = BTreeMap::new();
        for requested in inherited_needed {
            let key = ScopedSourceBinder {
                domain: requested.domain(),
                source: requested.binder().clone(),
            };
            if inherited.contains_key(&key)
                || source.contains_key(&key)
                || late_sources.insert(key.clone(), requested).is_some()
            {
                return Err(DemandError::DuplicateBinder(key.source).into());
            }
        }

        let mut pending = target_owners
            .iter()
            .filter_map(|owner| match owner {
                ImportOwner::Source { version, binder } => {
                    Some(target.qualified_source(&SourceBinder {
                        version: version.clone(),
                        binder: binder.clone(),
                    }))
                }
                ImportOwner::Retained { .. }
                | ImportOwner::CodeExport { .. }
                | ImportOwner::Package { .. } => None,
            })
            .collect::<Result<Vec<_>, _>>()?;
        let mut reachable = BTreeSet::new();
        let mut needed_late = BTreeSet::new();
        while let Some(binder) = pending.pop() {
            if let Some(&(index, _)) = source.get(&binder) {
                if reachable.insert(index) {
                    pending.extend(
                        demanded[index]
                            .group()
                            .imports()
                            .iter()
                            .filter_map(|owner| match owner {
                                ImportOwner::Source { version, binder } => {
                                    Some(demanded[index].qualified_source(&SourceBinder {
                                        version: version.clone(),
                                        binder: binder.clone(),
                                    }))
                                }
                                ImportOwner::Retained { .. }
                                | ImportOwner::CodeExport { .. }
                                | ImportOwner::Package { .. } => None,
                            })
                            .collect::<Result<Vec<_>, _>>()?,
                    );
                }
            } else if late_sources.contains_key(&binder) {
                needed_late.insert(binder);
            } else if !inherited.contains_key(&binder) {
                return Err(DemandError::MissingSource(binder.source).into());
            }
        }
        if reachable.len() != demanded.len() || needed_late.len() != late_sources.len() {
            return Err(PreparedRuntimeError::UnreachableCertifiedGroup);
        }

        for key in target_owners
            .iter()
            .filter_map(|owner| match owner {
                ImportOwner::Source { version, binder } => {
                    Some(target.qualified_source(&SourceBinder {
                        version: version.clone(),
                        binder: binder.clone(),
                    }))
                }
                _ => None,
            })
            .collect::<Result<Vec<_>, _>>()?
            .into_iter()
            .chain(
                demanded
                    .iter()
                    .flat_map(|selected| {
                        selected
                            .group()
                            .imports()
                            .iter()
                            .filter_map(|owner| match owner {
                                ImportOwner::Source { version, binder } => {
                                    Some(selected.qualified_source(&SourceBinder {
                                        version: version.clone(),
                                        binder: binder.clone(),
                                    }))
                                }
                                _ => None,
                            })
                    })
                    .collect::<Result<Vec<_>, _>>()?,
            )
        {
            let expected = source_evidence.get(&key.source).ok_or_else(|| {
                PreparedRuntimeError::InvalidCertifiedSourceOwner(key.source.clone())
            })?;
            let actual = if let Some(&(index, _)) = source.get(&key) {
                let group = demanded[index].group();
                (group.owner(), group.original_ordinal())
            } else if let Some(lease) = inherited.get(&key) {
                (lease.owner(), lease.original_ordinal())
            } else if let Some(requested) = late_sources.get(&key) {
                (requested.owner(), requested.original_ordinal())
            } else {
                return Err(PreparedRuntimeError::InvalidCertifiedSourceOwner(
                    key.source,
                ));
            };
            if actual.0 != &expected.0 || actual.1 != expected.1 {
                return Err(PreparedRuntimeError::InvalidCertifiedSourceOwner(
                    key.source,
                ));
            }
        }

        let mut late_leases = BTreeMap::new();
        let mut late_attachments = Vec::<SourceInstanceAttachment>::new();
        let mut late_roots = BTreeMap::<
            (
                tidepool_codegen::prepared_program::GroupInstanceId,
                SourceBinder,
            ),
            usize,
        >::new();
        let mut new_late_roots = Vec::<SourceInstanceLease>::new();
        for (key, requested) in late_sources {
            let physical = (requested.anchor().instance(), requested.binder().clone());
            let existing = bindings.retained_source_sibling_attachment(requested);
            let new_root = existing.is_none() && !late_roots.contains_key(&physical);
            let issued = if let Some(existing) = existing {
                Ok(existing)
            } else if let Some(index) = late_roots.get(&physical) {
                self.machine
                    .share_certified_source_attachment(requested, &late_attachments[*index])
            } else {
                self.machine.retain_certified_source_attachment(requested)
            };
            let attachment = match issued {
                Ok(attachment) => attachment,
                Err(error) => {
                    for lease in new_late_roots {
                        assert!(
                            self.release(lease.handle()),
                            "staged sibling source root remains live"
                        );
                    }
                    return Err(PreparedRuntimeError::Install(error));
                }
            };
            let lease = attachment.lease().clone();
            if new_root {
                new_late_roots.push(lease.clone());
            }
            late_roots.entry(physical).or_insert(late_attachments.len());
            late_leases.insert(key, lease);
            late_attachments.push(attachment);
        }

        let result = (|| -> Result<CertifiedTurnInstall, PreparedRuntimeError> {
            let mut facts: Vec<_> = demanded
                .iter()
                .map(|selected| ProgramFacts::from_image(selected.image(), None))
                .collect();
            facts.push(ProgramFacts::from_image(
                &target.image,
                Some(target.prepared.entry()),
            ));
            // A reduced target need not redeclare the Settled constructors:
            // it may reuse the exact original definitions already installed
            // by an earlier target or one of this batch's source groups.
            // ProgramFacts still carries the constructor identities used by
            // the decoder, and a conflicting host-id pair is never accepted.
            let settled = SettledIds::from_facts(self.programs.values().chain(facts.iter()))?;
            if let Some(target_facts) = facts.last_mut() {
                target_facts.settled = settled;
            }
            let admitted = self.plan_batch_evidence(facts)?;
            // A target can contain incidental package tops absent from its
            // selected canonical interfaces. Keep those definitions local;
            // their presence cannot issue a retained package import later.
            let exports: Vec<_> = exportable_code_tops(&target.prepared)
                .into_iter()
                .filter(|(identity, _, _)| {
                    certified_exports.contains_key(identity)
                        || target_packages.contains_key(identity)
                })
                .collect();
            let mut programs = Vec::with_capacity(demanded.len() + 1);
            let mut package_updates = BTreeMap::<SymbolIdentity, [u8; 32]>::new();
            let mut source_needed = BTreeSet::<ScopedSourceBinder>::new();

            let mut append = |image: Arc<CompiledProgram>,
                              globals: &[tidepool_repr::execution_schema::GlobalDecl],
                              owners: &[ImportOwner],
                              source_plan: &dyn Fn(
                &SourceBinder,
            )
                -> Result<ScopedSourceBinder, DemandError>|
             -> Result<(), PreparedRuntimeError> {
                let mut imports = Vec::with_capacity(owners.len());
                for (index, (declaration, owner)) in globals.iter().zip(owners).enumerate() {
                    let import = match owner {
                        ImportOwner::Source { version, binder } => {
                            let binder_key = SourceBinder {
                                version: version.clone(),
                                binder: binder.clone(),
                            };
                            let key = source_plan(&binder_key)?;
                            if let Some(&(group, binding)) = source.get(&key) {
                                if image.authenticated_source_literal(GlobalId(index as u32))
                                    != Some(&key.source)
                                {
                                    source_needed.insert(key);
                                }
                                BatchImport::Source { group, binding }
                            } else {
                                let lease = inherited
                                    .get(&key)
                                    .or_else(|| late_leases.get(&key))
                                    .filter(|lease| {
                                        lease.binder() == &key.source
                                            && lease.owner().module_version == *version
                                            && lease.owner().unit == binder.unit
                                            && lease.owner().module == binder.module
                                    })
                                    .ok_or_else(|| {
                                        DemandError::MissingSource(key.source.clone())
                                    })?;
                                self.machine
                                    .handle_is_evaluated(lease.handle())
                                    .map_err(PreparedRuntimeError::Install)?;
                                BatchImport::Existing {
                                    handle: lease.handle(),
                                    entry_signature: lease.entry_signature().cloned(),
                                }
                            }
                        }
                        ImportOwner::Retained { id, generation } => {
                            let entry = bindings.get(*id).ok_or_else(|| {
                                PreparedRuntimeError::MissingCertifiedOwner(owner.clone())
                            })?;
                            let handle = exact_external.get(owner).copied().ok_or_else(|| {
                                PreparedRuntimeError::MissingCertifiedOwner(owner.clone())
                            })?;
                            if entry.module.gen().0 != *generation
                                || entry.value.identity != declaration.identity
                                || entry.value.handle != handle
                            {
                                return Err(PreparedRuntimeError::MissingCertifiedOwner(
                                    owner.clone(),
                                ));
                            }
                            BatchImport::Existing {
                                handle,
                                entry_signature: None,
                            }
                        }
                        ImportOwner::CodeExport {
                            binder,
                            generation,
                            root_id,
                            interface_digest,
                        } => self.code_export_import(
                            CodeExportOwnerRef {
                                binder,
                                generation: *generation,
                                root_id: *root_id,
                                interface_digest: interface_digest.as_ref(),
                            },
                            declaration,
                        )?,
                        ImportOwner::Package {
                            unit,
                            module,
                            binder,
                            interface_digest,
                        } => {
                            if *interface_digest == [0; 32]
                                || declaration.identity != *binder
                                || binder.unit != *unit
                                || binder.module != *module
                            {
                                return Err(
                                    PreparedRuntimeError::CertifiedPackageOwnerUnavailable {
                                        owner: owner.clone(),
                                        evidence:
                                            CertifiedPackageOwnerEvidence::DeclarationMismatch {
                                                declaration: declaration.identity.clone(),
                                            },
                                    },
                                );
                            }
                            let import = if let Some(export) = self.code_exports.get(binder) {
                                if !matches_protected_package_interface(export, interface_digest) {
                                    return Err(
                                        PreparedRuntimeError::CertifiedPackageOwnerUnavailable {
                                            owner: owner.clone(),
                                            evidence:
                                                CertifiedPackageOwnerEvidence::RetainedExport {
                                                    interface_digest: export.interface_digest,
                                                },
                                        },
                                    );
                                }
                                BatchImport::Existing {
                                    handle: export.handle,
                                    entry_signature: export.entry.clone(),
                                }
                            } else {
                                let &(binding, digest) = target_packages
                                    .get(binder)
                                    .filter(|(_, digest)| digest == interface_digest)
                                    .ok_or_else(|| {
                                        PreparedRuntimeError::CertifiedPackageOwnerUnavailable {
                                            owner: owner.clone(),
                                            evidence:
                                                CertifiedPackageOwnerEvidence::TargetDefinition {
                                                    present: exports
                                                        .iter()
                                                        .any(|(identity, _, _)| identity == binder),
                                                    interfaces_match: target
                                                        .package_interfaces
                                                        .matches_target(&target.prepared),
                                                    interface_digest: target
                                                        .package_interfaces
                                                        .interface_digest(unit, module),
                                                    diagnostic: None,
                                                },
                                        }
                                    })?;
                                debug_assert_eq!(digest, *interface_digest);
                                BatchImport::Source {
                                    group: demanded.len(),
                                    binding,
                                }
                            };
                            if !target.package_literals.contains_key(binder) {
                                if let Some(previous) =
                                    package_updates.insert(binder.clone(), *interface_digest)
                                {
                                    if previous != *interface_digest {
                                        return Err(PreparedRuntimeError::CertifiedPackageOwnerUnavailable {
                                            owner: owner.clone(),
                                            evidence: CertifiedPackageOwnerEvidence::ConflictingPackageProof {
                                                previous_interface_digest: previous,
                                            },
                                        });
                                    }
                                }
                            }
                            import
                        }
                    };
                    imports.push(import);
                }
                programs.push(BatchProgram { image, imports });
                Ok(())
            };
            for selected in &demanded {
                append(
                    Arc::clone(selected.image()),
                    selected.group().definitions().globals(),
                    selected.group().imports(),
                    &|source| selected.qualified_source(source),
                )?;
            }
            append(
                Arc::clone(&target.image),
                target.prepared.globals(),
                target_owners,
                &|source| target.qualified_source(source),
            )?;
            drop(append);
            let requests = source_needed
                .iter()
                .map(|binder| {
                    let &(group, _) = source
                        .get(binder)
                        .expect("source edge selected an in-batch binder");
                    BatchLeaseRequest::for_demanded(group, &demanded[group], &binder.source)
                        .map_err(PreparedRuntimeError::Install)
                })
                .collect::<Result<Vec<_>, _>>()?;
            let installed = self
                .machine
                .install_shared_batch_with_leases(programs, requests)
                .map_err(|mut error| {
                    if let ExecutionError::BatchImportContract(evidence) = &mut error {
                        let owners = if evidence.program == demanded.len() {
                            Some(target_owners)
                        } else {
                            demanded
                                .get(evidence.program)
                                .map(|selected| selected.group().imports())
                        };
                        evidence.owner = owners
                            .and_then(|owners| owners.get(evidence.import_position))
                            .cloned();
                    }
                    PreparedRuntimeError::Install(error)
                })?;
            let target_id = *installed
                .programs
                .last()
                .expect("target is always the final batch candidate");
            for id in installed.programs.iter().copied() {
                self.machine
                    .pin(id)
                    .expect("batch returned installed program");
            }
            let mut staged = CertifiedTurnInstall {
                target: target_id,
                groups: installed.programs[..installed.programs.len() - 1].to_vec(),
                domain_leases: installed.source_attachments,
                leases: installed.leases,
                admitted,
                package_updates,
                exports: BTreeMap::new(),
            };
            staged.exports = match self.stage_code_exports(
                target_id,
                exports,
                &target_packages,
                &certified_exports,
            ) {
                Ok(exports) => exports,
                Err(error) => {
                    let tokens = std::mem::take(&mut staged.leases);
                    if let Err(cleanup) = self.abort_certified_turn(staged, tokens) {
                        tracing::warn!(?cleanup, "failed to collect refused export batch");
                    }
                    return Err(error);
                }
            };
            Ok(staged)
        })();
        match result {
            Ok(mut staged) => {
                staged.domain_leases.extend(late_attachments);
                staged.leases.extend(new_late_roots);
                Ok(staged)
            }
            Err(error) => {
                for lease in new_late_roots {
                    assert!(
                        self.release(lease.handle()),
                        "failed install retains sibling source root"
                    );
                }
                Err(error)
            }
        }
    }

    /// Publish the staged native batch only after scope custody accepted all
    /// source lease tokens. Its machine programs are pinned but invisible to
    /// the session's facts, site and package-export ledgers until this call.
    pub(crate) fn commit_certified_turn(&mut self, staged: CertifiedTurnInstall) -> ProgramId {
        assert!(
            staged.leases.is_empty(),
            "source leases must enter scope custody"
        );
        self.code_exports.extend(staged.exports);
        for (binder, digest) in staged.package_updates {
            self.code_exports
                .get_mut(&binder)
                .expect("preflighted package export remains installed")
                .interface_digest = Some(digest);
        }
        let programs = staged
            .groups
            .iter()
            .copied()
            .chain(std::iter::once(staged.target));
        assert_eq!(
            staged.groups.len() + 1,
            staged.admitted.len(),
            "staged batch matches admitted definitions"
        );
        for (id, admitted) in programs.zip(staged.admitted) {
            self.publish_admitted_program(id, admitted);
        }
        self.installs_since_major += staged.groups.len() + 1;
        for group in staged.groups {
            assert!(self.unpin(group), "scoped source root replaces install pin");
        }
        staged.target
    }

    /// Discard an unpublished native batch after scope admission refuses its
    /// tokens. No session metadata or package roots were committed, so once
    /// these exact machine handles/pins release, collection retires the batch.
    pub(crate) fn abort_certified_turn(
        &mut self,
        staged: CertifiedTurnInstall,
        tokens: Vec<SourceInstanceLease>,
    ) -> Result<(), PreparedRuntimeError> {
        assert!(
            staged.leases.is_empty(),
            "returned tokens are passed separately"
        );
        for token in tokens {
            assert!(
                self.release(token.handle()),
                "rejected source token remains rooted"
            );
        }
        for export in staged.exports.into_values() {
            assert!(
                self.release(export.handle),
                "unpublished export remains rooted"
            );
        }
        for program in staged
            .groups
            .into_iter()
            .chain(std::iter::once(staged.target))
        {
            assert!(
                self.unpin(program),
                "unpublished program retains its install pin"
            );
        }
        self.quiesce_and_collect_now()
    }

    /// Step (a) of the off-checkout split install: resolve `prepared`'s
    /// imports, plan its evidence and take a compile snapshot -- everything
    /// [`Self::compile_off_checkout`] needs -- without compiling. Run this
    /// under the machine checkout; the returned [`InstallSnapshot`] carries
    /// no machine reference and may be compiled after the checkout is
    /// released.
    pub(crate) fn snapshot_install(
        &mut self,
        prepared: PreparedProgram,
        bindings: &BindingTable,
        index: &BindingIndex,
    ) -> Result<InstallSnapshot, PreparedRuntimeError> {
        let (values, imports) = self.resolve_imports(&prepared, bindings, index)?;
        let exports = exportable_code_tops(&prepared);
        let facts = ProgramFacts::of(&prepared);
        self.plan_evidence(&facts)?;
        let linked = link_program(prepared, &values)?;
        let precompiled = self
            .registry
            .as_ref()
            .and_then(|registry| registry.lookup(&linked));
        let registry = self.registry.clone();
        let compile = self.machine.compile_snapshot();
        Ok(InstallSnapshot {
            linked,
            values,
            imports,
            facts,
            exports,
            compile,
            registry,
            precompiled,
        })
    }

    /// Step (b): compile `snapshot`'s linked program off any checkout, or
    /// reuse an image the registry already had at snapshot time -- either
    /// way returning an `Arc` so [`Self::revalidate_and_install`] can
    /// install it with [`PreparedMachine::install_shared`]. A fresh compile
    /// with a registry attached is inserted here, off-checkout, so every
    /// other machine sharing that registry sees it as soon as this step
    /// finishes rather than waiting for `revalidate_and_install`. Pure with
    /// respect to the machine; safe to run on a blocking thread while other
    /// turns hold the checkout.
    pub(crate) fn compile_off_checkout(
        snapshot: &mut InstallSnapshot,
    ) -> Result<Arc<CompiledProgram>, CompileError> {
        if let Some(image) = &snapshot.precompiled {
            return Ok(Arc::clone(image));
        }
        let mut compile = || snapshot.compile.compile(&snapshot.linked).map(Arc::new);
        match &snapshot.registry {
            Some(registry) => registry.get_or_compile(&snapshot.linked, compile),
            None => compile(),
        }
    }

    /// Step (c): under the machine checkout again, re-resolve `snapshot`'s
    /// imports and compare them against the facts the off-checkout compile
    /// ran against. An import that became evaluated, changed representation
    /// or generation, or was retired invalidates the compile: this returns
    /// `Ok(None)` and the caller must recompile (or fall back to the
    /// single-checkout [`Self::install`]) rather than install a program
    /// linked against stale import facts. Otherwise installs, publishes and
    /// pins exactly as [`Self::install`] does.
    pub(crate) fn revalidate_and_install(
        &mut self,
        mut snapshot: InstallSnapshot,
        compiled: Arc<CompiledProgram>,
        bindings: &BindingTable,
        index: &BindingIndex,
    ) -> Result<Option<ProgramId>, PreparedRuntimeError> {
        let (fresh_values, fresh_imports) =
            self.resolve_imports(snapshot.linked.prepared(), bindings, index)?;
        // Reusing code never aliases installation state. Only changes to the
        // imported values can invalidate this compilation snapshot.
        if fresh_values != snapshot.values || fresh_imports != snapshot.imports {
            return Ok(None);
        }
        let import_count = snapshot.imports.len();
        snapshot.facts.definitions = Arc::clone(compiled.definition_facts());
        // Evidence ownership can change while compilation releases checkout.
        // Replan against current installations before any native mutation.
        let admitted = self.admit_program_facts(snapshot.facts)?;
        let program = self
            .machine
            .install_shared(compiled, snapshot.imports)
            .map_err(PreparedRuntimeError::Install)?;
        tracing::info!(
            target: "tidepool_runtime::prepared_install",
            imports = import_count,
            compiled_off_checkout = true,
            "prepared install"
        );
        self.finish_program_install(program, admitted, snapshot.exports)?;
        Ok(Some(program))
    }

    /// Run `program`'s settled scaffold under `realm` and read its one
    /// constructor layer. The scaffold value itself is released here; the
    /// layer's fields come back as retained handles.
    pub fn run_settled(
        &mut self,
        program: ProgramId,
        realm: RealmId,
    ) -> Result<PreparedSettlement, PreparedRuntimeError> {
        self.run_settled_with_inputs(program, realm, &[])
    }

    /// Invoke a compiled inspection entry against an already mounted value.
    pub(crate) fn run_settled_with_inputs(
        &mut self,
        program: ProgramId,
        realm: RealmId,
        inputs: &[PreparedInput],
    ) -> Result<PreparedSettlement, PreparedRuntimeError> {
        let facts = self
            .programs
            .get(&program)
            .ok_or(PreparedRuntimeError::Run(ExecutionError::UnknownProgram(
                program,
            )))?;
        let entry = facts.entry.ok_or(PreparedRuntimeError::UnsettledEntry {
            program,
            detail: "the installed definitions have no executable entry",
        })?;
        // The decoder needs the settled constructors; refuse before running
        // an entry whose layer could never be read.
        if facts.settled.is_none() {
            return Err(PreparedRuntimeError::UnsettledEntry {
                program,
                detail: "the program declares no Tidepool.Internal.Resume.Settled constructors",
            });
        }
        if self.cancellation_requested(realm) {
            return Err(PreparedRuntimeError::Cancelled);
        }
        let batch = self
            .machine
            .run_entry_retained(program, entry, inputs, SETTLE_CALL, realm)
            .map_err(PreparedRuntimeError::Run)?;
        self.settle_batch(program, realm, batch)
    }

    /// Read the settled layer an entry of `program` returned: the batch's one
    /// managed value is the `Settled` constructor (any other managed value is
    /// released), decoded through the shared decoder.
    fn settle_batch(
        &mut self,
        program: ProgramId,
        realm: RealmId,
        batch: PreparedResultBatch,
    ) -> Result<PreparedSettlement, PreparedRuntimeError> {
        let outer =
            self.take_first_managed(batch.values)
                .ok_or(PreparedRuntimeError::UnsettledEntry {
                    program,
                    detail: "the entry returned no managed settled value",
                })?;
        self.decode_settled(program, outer, realm)
    }

    /// The one settled-layer decoder: read `outer` (a `Settled` value some
    /// entry of `program` returned) as `Done`/`Suspended`, releasing `outer`
    /// and retaining its fields under the given resource scope. Initial runs
    /// ([`Self::run_settled`]) and resumed runs ([`Self::resume_parked`]) both
    /// pass through here.
    fn decode_settled(
        &mut self,
        program: ProgramId,
        outer: PreparedHandle,
        realm: RealmId,
    ) -> Result<PreparedSettlement, PreparedRuntimeError> {
        let settled = self
            .programs
            .get(&program)
            .ok_or(PreparedRuntimeError::Run(ExecutionError::UnknownProgram(
                program,
            )))?
            .settled
            .ok_or(PreparedRuntimeError::UnsettledEntry {
                program,
                detail: "the program declares no Tidepool.Internal.Resume.Settled constructors",
            })?;
        let layer = self.machine.inspect_outer(outer, realm);
        self.machine.release(outer);
        let CodegenPreparedOuter::Constructor { identity, fields } =
            layer.map_err(PreparedRuntimeError::Run)?;
        let managed: Vec<PreparedHandle> = fields
            .into_iter()
            .filter_map(|field| match field {
                PreparedResult::Managed(handle) => Some(handle),
                PreparedResult::Void | PreparedResult::Scalar(_) => None,
            })
            .collect();
        let mut shape = |detail| {
            self.release_all(managed.iter().copied());
            Err(PreparedRuntimeError::UnsettledEntry { program, detail })
        };
        if identity == settled.done {
            match managed.as_slice() {
                [value] => Ok(PreparedSettlement::Done { value: *value }),
                _ => shape("Done carried other than one managed field"),
            }
        } else if identity == settled.suspended {
            match managed.as_slice() {
                [request, continuation] => Ok(PreparedSettlement::Suspended {
                    request: *request,
                    continuation: *continuation,
                }),
                _ => shape("Suspended carried other than two managed fields"),
            }
        } else {
            shape("the settled layer is neither Done nor Suspended")
        }
    }

    /// The admitted resume entry of `program`, read without holding a borrow
    /// past this call so callers remain free to release handles afterward.
    fn resume_entry_of(&self, program: ProgramId) -> Result<ValueId, PreparedRuntimeError> {
        self.programs
            .get(&program)
            .ok_or(PreparedRuntimeError::Run(ExecutionError::UnknownProgram(
                program,
            )))?
            .resume
            .ok_or(PreparedRuntimeError::NoResumeEntry {
                program,
                entry: PREPARED_RESUME_TARGET,
            })
    }

    /// The admitted generic apply-entry entry (`__applyEntry`) of `program`.
    fn apply_entry_of(&self, program: ProgramId) -> Result<ValueId, PreparedRuntimeError> {
        self.programs
            .get(&program)
            .ok_or(PreparedRuntimeError::Run(ExecutionError::UnknownProgram(
                program,
            )))?
            .apply_entry
            .ok_or(PreparedRuntimeError::NoApplyEntryEntry {
                program,
                entry: PREPARED_APPLY_ENTRY_TARGET,
            })
    }

    /// The admitted generic apply-value entry (`__applyValue`) of `program`.
    fn apply_value_of(&self, program: ProgramId) -> Result<ValueId, PreparedRuntimeError> {
        self.programs
            .get(&program)
            .ok_or(PreparedRuntimeError::Run(ExecutionError::UnknownProgram(
                program,
            )))?
            .apply_value
            .ok_or(PreparedRuntimeError::NoApplyValueEntry {
                program,
                entry: PREPARED_APPLY_VALUE_TARGET,
            })
    }

    /// The program that should host a rooted apply of `handle`: the program
    /// whose descriptor owns the object `handle` refers to, when that
    /// program is still installed and admits both generic apply roots; else
    /// the most recently installed program that admits them. Every
    /// executable template emits `__applyEntry`/`__applyValue` beside its
    /// settled scaffold, so any installed program is normally a candidate —
    /// preferring the object's own owner keeps a rooted apply within the
    /// program whose code produced the closure whenever that is still live,
    /// without depending on install order to matter for correctness.
    pub(crate) fn hosting_program(&self, handle: PreparedHandle) -> Option<ProgramId> {
        fn admits_apply_roots(facts: &ProgramFacts) -> bool {
            facts.apply_entry.is_some() && facts.apply_value.is_some()
        }
        if let Some(owner) = self.machine.owner_of_handle(handle) {
            if self.programs.get(&owner).is_some_and(admits_apply_roots) {
                return Some(owner);
            }
        }
        self.programs
            .iter()
            .rev()
            .find(|&(_, facts)| admits_apply_roots(facts))
            .map(|(id, _)| *id)
    }

    /// Apply a rooted `Int -> Eff effects a` closure `f` to `argument` through the
    /// hosting program's `__applyEntry` root and finish the settled layer as
    /// far as reading its `Done`/`Suspended` shape — the caller
    /// ([`crate::session::resident::ResidentSession::run_rooted_entry_borrowed`])
    /// finishes a `Done` value or parks a `Suspended` one exactly as an
    /// ordinary prepared turn does. `f` is BORROWED: never released here,
    /// whatever the outcome. `argument` crosses as a bare unboxed scalar,
    /// matching `__applyEntry`'s `Int#` parameter.
    pub(crate) fn run_rooted_entry(
        &mut self,
        f: PreparedHandle,
        argument: i64,
        realm: RealmId,
    ) -> Result<(ProgramId, PreparedSettlement), PreparedRuntimeError> {
        let program = self
            .hosting_program(f)
            .ok_or(PreparedRuntimeError::NoHostingProgram)?;
        if self.cancellation_requested(realm) {
            return Err(PreparedRuntimeError::Cancelled);
        }
        let entry = self.apply_entry_of(program)?;
        let batch = self
            .machine
            .run_entry_retained(
                program,
                entry,
                &[
                    PreparedInput::Managed(f),
                    PreparedInput::Scalar(argument as u64),
                ],
                SETTLE_CALL,
                realm,
            )
            .map_err(PreparedRuntimeError::Run)?;
        let settlement = self.settle_batch(program, realm, batch)?;
        Ok((program, settlement))
    }

    /// Apply one rooted Haskell function `f` to one rooted Haskell value `x`
    /// through the hosting program's `__applyValue` root. Both handles are
    /// BORROWED: neither is released here, whatever the outcome. See
    /// [`Self::run_rooted_entry`] for the shared hosting-program and
    /// finishing contract
    /// ([`crate::session::resident::ResidentSession::run_rooted_application`]
    /// is the caller).
    pub(crate) fn run_rooted_application(
        &mut self,
        f: PreparedHandle,
        x: PreparedHandle,
        realm: RealmId,
    ) -> Result<(ProgramId, PreparedSettlement), PreparedRuntimeError> {
        let program = self
            .hosting_program(f)
            .ok_or(PreparedRuntimeError::NoHostingProgram)?;
        if self.cancellation_requested(realm) {
            return Err(PreparedRuntimeError::Cancelled);
        }
        let entry = self.apply_value_of(program)?;
        let batch = self
            .machine
            .run_entry_retained(
                program,
                entry,
                &[PreparedInput::Managed(f), PreparedInput::Managed(x)],
                SETTLE_CALL,
                realm,
            )
            .map_err(PreparedRuntimeError::Run)?;
        let settlement = self.settle_batch(program, realm, batch)?;
        Ok((program, settlement))
    }

    fn classify_reply(
        &self,
        request: &HaskellValue,
        table: &DataConTable,
    ) -> Result<PreparedReplyEvidence, PreparedRuntimeError> {
        let HaskellValue::Con(constructor, fields) = request else {
            return Err(PreparedRuntimeError::UntypedRequest {
                constructor: "a non-constructor value".into(),
            });
        };
        let witness = self.constructor_replies.get(constructor).ok_or(
            PreparedRuntimeError::MissingReplyEvidence {
                constructor: *constructor,
            },
        )?;
        let facts = self
            .programs
            .get(&witness.owner)
            .ok_or(PreparedRuntimeError::Run(ExecutionError::UnknownProgram(
                witness.owner,
            )))?;
        match facts.constructor_replies[witness.row].1 {
            ConstructorReply::Static(node) => Ok(PreparedReplyEvidence::Static {
                owner: witness.owner,
                constructor: *constructor,
                node,
            }),
            ConstructorReply::StaticWithSite {
                reply: node,
                field,
                payload_field,
                capture_input,
            } => {
                let site = fields
                    .get(field as usize)
                    .and_then(|field| site_field(field, table))
                    .ok_or(PreparedRuntimeError::MalformedRequestSite {
                        constructor: *constructor,
                    })?;
                let selected = self
                    .sites
                    .get(&site)
                    .ok_or(PreparedRuntimeError::UnknownSite { site })?;
                let row = &self.programs[&selected.owner].sites[selected.row];
                if capture_input.is_some_and(|input| row.inputs.len() != input as usize + 1) {
                    return Err(PreparedRuntimeError::MalformedRequestSite {
                        constructor: *constructor,
                    });
                }
                Ok(PreparedReplyEvidence::StaticWithSite {
                    owner: witness.owner,
                    constructor: *constructor,
                    node,
                    site_owner: selected.owner,
                    site_row: selected.row,
                    payload_field,
                    capture_input,
                })
            }
            ConstructorReply::AtSite => {
                let site = fields
                    .first()
                    .and_then(|field| site_field(field, table))
                    .ok_or(PreparedRuntimeError::MalformedRequestSite {
                        constructor: *constructor,
                    })?;
                let witness = self
                    .sites
                    .get(&site)
                    .ok_or(PreparedRuntimeError::UnknownSite { site })?;
                Ok(PreparedReplyEvidence::AtSite {
                    owner: witness.owner,
                    row: witness.row,
                })
            }
        }
    }

    fn structural_reply(
        &self,
        reply: PreparedReplyEvidence,
    ) -> Result<(ProgramId, TypeNodeId, ReplyTarget), PreparedRuntimeError> {
        let owner = reply.owner();
        let facts = self.programs.get(&owner).ok_or(PreparedRuntimeError::Run(
            ExecutionError::UnknownProgram(owner),
        ))?;
        match reply {
            PreparedReplyEvidence::Static {
                constructor, node, ..
            }
            | PreparedReplyEvidence::StaticWithSite {
                constructor, node, ..
            } => Ok((owner, node, ReplyTarget::Static(constructor))),
            PreparedReplyEvidence::AtSite { row, .. } => {
                let row = facts
                    .sites
                    .get(row)
                    .ok_or(PreparedRuntimeError::MissingReplySiteRow { owner, row })?;
                if row.delivery != SiteDelivery::HostAnswer {
                    return Err(PreparedRuntimeError::AnswerDelivery {
                        site: row.site,
                        delivery: row.delivery,
                    });
                }
                Ok((owner, row.wire, ReplyTarget::AtSite(row.site)))
            }
        }
    }

    /// Dynamic request site, if this frame carries compiler-attested AtSite evidence.
    pub fn parked_site(&self, id: ContinuationId) -> Option<u64> {
        let (_, evidence) = self.machine.parked(id)?;
        let (owner, row) = match evidence.reply {
            PreparedReplyEvidence::AtSite { owner, row } => (owner, row),
            PreparedReplyEvidence::StaticWithSite {
                site_owner,
                site_row,
                ..
            } => (site_owner, site_row),
            PreparedReplyEvidence::Static { .. } => return None,
        };
        Some(self.programs.get(&owner)?.sites.get(row)?.site)
    }

    /// Result capture requires the actual parked constructor's compiler proof
    /// that its retained payload has the final input's type.
    pub fn parked_capture_site(&self, id: ContinuationId) -> Option<(u64, usize)> {
        let (_, evidence) = self.machine.parked(id)?;
        let PreparedReplyEvidence::StaticWithSite {
            site_owner,
            site_row,
            capture_input: Some(input),
            ..
        } = evidence.reply
        else {
            return None;
        };
        let row = self.programs.get(&site_owner)?.sites.get(site_row)?;
        (row.inputs.len() == input as usize + 1).then_some((row.site, input as usize))
    }

    /// Observe and classify a suspension by its exact request constructor.
    /// Only AtSite evidence admits the first erased RequestSite field. Every
    /// refusal releases both handles before parking; a successful frame roots
    /// the reply evidence owner independently of the runner and continuation.
    pub fn park_suspension(
        &mut self,
        program: ProgramId,
        realm: RealmId,
        park: ParkPolicy,
        request: PreparedHandle,
        continuation: PreparedHandle,
        table: &DataConTable,
    ) -> Result<PreparedParked, PreparedRuntimeError> {
        let resume_entry = match self.resume_entry_of(program) {
            Ok(resume_entry) => resume_entry,
            Err(error) => {
                self.release_all([request, continuation]);
                return Err(error);
            }
        };
        if park.effect_policy == EffectRunPolicy::HandleOrError {
            self.release_all([request, continuation]);
            return Err(PreparedRuntimeError::UnhandledRequest);
        }
        // The `Union` layer: an unpacked tag word and the lazy payload.
        let outer = self.machine.inspect_outer(request, realm);
        self.machine.release(request);
        let CodegenPreparedOuter::Constructor { fields, .. } = match outer {
            Ok(outer) => outer,
            Err(error) => {
                self.machine.release(continuation);
                return Err(PreparedRuntimeError::Run(error));
            }
        };
        let payload = match self.take_first_managed(fields) {
            Some(payload) => payload,
            None => {
                self.machine.release(continuation);
                return Err(PreparedRuntimeError::UnsettledEntry {
                    program,
                    detail: "the suspended Union carried no managed payload",
                });
            }
        };
        // The request is observed (forced) through the existing observe
        // path, producing the value reported for a suspension. This runs
        // AFTER any prior effect in the same turn already committed (the
        // continuation being parked here is its own committed answer's
        // continuation), so an exhausted budget here must not fail the turn
        // outright the way it would for a fresh, uncommitted observation --
        // that would report a hard failure for effects that already ran.
        //
        // Unlike model-facing display, this observation feeds a TYPED
        // decode: `handlers.dispatch` below reconstructs a concrete Rust
        // request type (e.g. `JevAskWith`) from the value, field by field.
        // `BudgetPolicy::Bounded` is the wrong tool here -- a cut can land on
        // ANY field, swapping in `OVERSIZE_SENTINEL` in place of whatever
        // that field's own type was (an `Int`, a `Map` entry, not only a
        // `Text`), which decode then rejects with a confusing type mismatch
        // instead of a clean, budget-attributed failure. `100_000` is a
        // DISPLAY-sized ceiling; the request already exists in full on the
        // heap, so the real constraint on rematerializing it here is memory,
        // not that ceiling. Retry under `Complete` with no such ceiling
        // instead of degrading the shape.
        let observed = match self.machine.observe_handle(
            program,
            payload,
            RunOptions::default().observation_budget,
        ) {
            Ok(request) => Ok(request),
            Err(error) if error.is_observation_budget_exhausted() => {
                self.machine.observe_handle(program, payload, usize::MAX)
            }
            Err(error) => Err(error),
        };
        let request = match observed {
            Ok(request) => request,
            Err(error) => {
                self.machine.release(payload);
                self.machine.release(continuation);
                return Err(PreparedRuntimeError::Run(error));
            }
        };
        let classified = self.classify_reply(&request, table);
        let reply = match classified {
            Ok(reply) => reply,
            Err(error) => {
                self.machine.release(payload);
                self.machine.release(continuation);
                return Err(error);
            }
        };
        // A live-payload policy names one field of THIS request Con (the
        // convention's field 1) as the value crossing the runtime boundary
        // by reference. Classify first so a rejected request cannot strand a
        // newly tenured handle without a frame to own it; then mirror the field
        // before releasing `payload`. `PreparedMachine::park` consumes the
        // handle on both success and refusal.
        let live_payload = match (park.live_payload, reply) {
            (
                LivePayloadPolicy::ValueField(_),
                PreparedReplyEvidence::StaticWithSite { payload_field, .. },
            ) => LivePayloadPolicy::ValueField(payload_field as usize),
            (
                LivePayloadPolicy::ClosureField(_),
                PreparedReplyEvidence::StaticWithSite { payload_field, .. },
            ) => LivePayloadPolicy::ClosureField(payload_field as usize),
            (policy, _) => policy,
        };
        let live_payload_root =
            match self.tenure_live_payload(payload, realm, live_payload, &request) {
                Ok(root) => root,
                Err(error) => {
                    self.machine.release(payload);
                    self.machine.release(continuation);
                    return Err(PreparedRuntimeError::Run(error));
                }
            };
        self.machine.release(payload);
        let reply = match reply {
            PreparedReplyEvidence::StaticWithSite {
                owner,
                constructor,
                node,
                site_owner,
                site_row,
                payload_field,
                capture_input: _,
            } if live_payload != LivePayloadPolicy::ValueField(payload_field as usize) => {
                PreparedReplyEvidence::StaticWithSite {
                    owner,
                    constructor,
                    node,
                    site_owner,
                    site_row,
                    payload_field,
                    capture_input: None,
                }
            }
            reply => reply,
        };
        let evidence = PreparedFrameEvidence {
            reply,
            runner: program,
            resume_entry,
            continuation_rep: continuation.rep(),
        };
        let id = self
            .machine
            .park(
                continuation,
                realm,
                live_payload_root,
                ParkRequest {
                    principal: park.principal,
                    effect_policy: park.effect_policy,
                    live_payload: park.live_payload,
                    evidence,
                },
            )
            .map_err(PreparedRuntimeError::Run)?;
        Ok(PreparedParked { id, request })
    }

    /// Retain one field of `payload` (the request Con `park_suspension` just
    /// observed) as a persistent root, per `policy` -- the prepared route's
    /// analogue of `PreparedEngine`'s `tenure_live_payload`, built from the
    /// primitives this layer actually has above the JIT boundary:
    /// `payload`'s OWN fields are read through `PreparedMachine::inspect_outer`
    /// (which mints a fresh handle per managed field), the policy's chosen
    /// field's handle stays ledger-owned until atomic parking, and every other
    /// minted field handle is released immediately (`inspect_outer` is non-consuming and
    /// re-mints on every call, so nothing here is `payload`'s own retained
    /// registration).
    ///
    /// `LivePayloadPolicy::ValueField(field)` retains `field` whenever the
    /// bridged `request` has that many fields, whatever its shape;
    /// `ClosureField(field)` retains it only when the bridge found the
    /// closure sentinel there -- both read `request`, exactly as
    /// `PreparedEngine`'s own `request_has_field`/
    /// `request_field_carries_closure_sentinel` do. `None` when the
    /// policy names no field, the constructor doesn't have it, or (rare: a
    /// nullary/scalar-only request) the field is not itself managed.
    fn tenure_live_payload(
        &mut self,
        payload: PreparedHandle,
        realm: RealmId,
        policy: LivePayloadPolicy,
        request: &HaskellValue,
    ) -> Result<Option<PreparedHandle>, ExecutionError> {
        let field = match policy {
            LivePayloadPolicy::None => None,
            LivePayloadPolicy::ClosureField(field) => {
                matches!(request, HaskellValue::Con(_, fields)
                    if fields.get(field).is_some_and(tidepool_codegen::observation::contains_closure_sentinel))
                .then_some(field)
            }
            LivePayloadPolicy::ValueField(field) => {
                matches!(request, HaskellValue::Con(_, fields) if field < fields.len()).then_some(field)
            }
        };
        let Some(field) = field else {
            return Ok(None);
        };
        let CodegenPreparedOuter::Constructor { fields, .. } =
            self.machine.inspect_outer(payload, realm)?;
        let mut root = None;
        for (index, value) in fields.into_iter().enumerate() {
            match value {
                PreparedResult::Managed(handle) if index == field => {
                    root = Some(handle);
                }
                PreparedResult::Managed(handle) => {
                    self.machine.release(handle);
                }
                PreparedResult::Void | PreparedResult::Scalar(_) => {}
            }
        }
        Ok(root)
    }

    /// Peek at a parked frame's evidence and resource-scope id without consuming it.
    #[must_use]
    pub fn parked(&self, id: ContinuationId) -> Option<(RealmId, PreparedFrameEvidence)> {
        self.machine
            .parked(id)
            .map(|(realm, evidence)| (realm, *evidence))
    }

    /// The runtime resource scope owning the frame parked under `id`.
    #[must_use]
    pub fn parked_realm(&self, id: ContinuationId) -> Option<RealmId> {
        self.machine.parked_realm(id)
    }

    /// The ids currently parked on this engine's machine, ascending.
    #[must_use]
    pub fn parked_ids(&self) -> Vec<ContinuationId> {
        self.machine.parked_ids()
    }

    /// Mint a [`ValueHandle`] over the declared live payload of the frame
    /// parked under `id` (`PreparedMachine::take_live_payload_handle`). The
    /// frame stays parked; `None` when `id` is not parked or its frame holds
    /// no untaken live payload.
    pub fn live_payload_handle(
        &mut self,
        id: ContinuationId,
    ) -> Result<Option<ValueHandle>, PreparedRuntimeError> {
        self.machine
            .take_live_payload_handle(id)
            .map_err(PreparedRuntimeError::Run)
    }

    /// [`Self::live_payload_handle`] with the handle owned by the given resource scope rather
    /// than the frame's own resource scope (see `PreparedMachine::take_live_payload_handle_owned_by`).
    pub fn live_payload_handle_owned_by(
        &mut self,
        id: ContinuationId,
        realm: RealmId,
    ) -> Result<Option<ValueHandle>, PreparedRuntimeError> {
        self.machine
            .take_live_payload_handle_owned_by(id, Some(realm))
            .map_err(PreparedRuntimeError::Run)
    }

    /// Re-enter the frame parked under `id` with `answer`, a handle the
    /// caller has already validated against the frame's site evidence and
    /// retained under the frame's resource scope: take the frame, enter the runner's
    /// resume entry with the continuation and the answer, and read the
    /// settled layer through the shared settlement reader. `answer` is consumed on
    /// every path. Every failure before the take (unknown id, an answer from
    /// another resource scope, cancellation) leaves the frame parked and rooted; a
    /// failure after the take is a run failure.
    pub fn resume_parked(
        &mut self,
        id: ContinuationId,
        answer: PreparedHandle,
    ) -> Result<PreparedResumed, PreparedRuntimeError> {
        if let Err(error) = self.validate_owned_resume(id, answer) {
            self.machine.release(answer);
            return Err(error);
        }
        let resumed = self
            .machine
            .run_parked_entry_retained(id, answer, SETTLE_CALL);
        self.machine.release(answer);
        let (realm, evidence, batch) = resumed.map_err(PreparedRuntimeError::Run)?;
        let settlement = self.settle_batch(evidence.runner, realm, batch)?;
        Ok(PreparedResumed {
            settlement,
            realm,
            runner: evidence.runner,
        })
    }

    /// [`Self::resume_parked`], but `answer` is BORROWED rather than
    /// consumed: it is delivered to the resume entry and left exactly as
    /// live afterward, ownership unchanged — borrowed-handle delivery
    /// (`docs/continuation-parking-contract.md`), which reads a handle's
    /// current heap pointer without releasing its root. No resource-scope check: a
    /// handle is meant to move between parked continuations across resource
    /// scopes (see `ValueHandle`'s own doc), unlike a freshly built answer,
    /// which is always scoped to the frame's resource scope.
    fn resume_parked_borrowed(
        &mut self,
        id: ContinuationId,
        answer: PreparedHandle,
    ) -> Result<PreparedResumed, PreparedRuntimeError> {
        let (realm, _) = self.machine.parked(id).ok_or(PreparedRuntimeError::Run(
            ExecutionError::UnknownContinuation(id),
        ))?;
        if answer.rep() != RuntimeRep::LiftedRef {
            return Err(PreparedRuntimeError::AnswerRepresentation {
                actual: answer.rep(),
            });
        }
        if self.cancellation_requested(realm) {
            return Err(PreparedRuntimeError::Cancelled);
        }
        let (realm, evidence, batch) = self
            .machine
            .run_parked_entry_retained(id, answer, SETTLE_CALL)
            .map_err(PreparedRuntimeError::Run)?;
        let settlement = self.settle_batch(evidence.runner, realm, batch)?;
        Ok(PreparedResumed {
            settlement,
            realm,
            runner: evidence.runner,
        })
    }

    /// Re-enter the frame parked under `id` by delivering an
    /// already-retained value verbatim — no materialization, closures
    /// included, the same shape borrowed-handle delivery accepts. `raw`
    /// must be live in this engine's ledger and carry a lifted representation.
    /// This trusted delivery operation does not prove Haskell type equality.
    /// The handle's root is borrowed and stays retained after this call.
    pub fn resume_with_handle(
        &mut self,
        id: ContinuationId,
        raw: ValueHandle,
    ) -> Result<PreparedResumed, PreparedRuntimeError> {
        let handle = self
            .machine
            .prepared_handle_of(raw)
            .ok_or(PreparedRuntimeError::UnknownHandle)?;
        self.resume_parked_borrowed(id, handle)
    }

    /// Re-enter the frame parked under `id` with a constructor whose final
    /// field borrows `raw` verbatim. Prefix fields stream through the same
    /// typed structural visitor as ordinary host answers; the borrowed field
    /// is spliced in unvalidated beyond its `RuntimeRep`, under the framed-delivery contract
    /// (`docs/continuation-parking-contract.md`). The built constructor is
    /// released as usual once the resume entry has read it; `raw`'s root is
    /// untouched throughout.
    pub fn resume_with_framed_handle(
        &mut self,
        id: ContinuationId,
        raw: ValueHandle,
        constructor: DataConId,
        prefix: Vec<HaskellValue>,
        table: &DataConTable,
    ) -> Result<PreparedResumed, PreparedRuntimeError> {
        let prefix = prefix
            .into_iter()
            .map(|field| Box::new(field) as Box<dyn tidepool_bridge::ToHaskell + Send>)
            .collect::<Vec<_>>();
        self.resume_with_framed_handle_sources(id, raw, constructor, &prefix, table)
    }

    /// [`Self::resume_with_framed_handle`] with each prefix field supplied as
    /// an owned structural source. The borrowed final field remains outside
    /// normal structural construction, under the framed-delivery contract.
    pub fn resume_with_framed_handle_sources(
        &mut self,
        id: ContinuationId,
        raw: ValueHandle,
        constructor: DataConId,
        prefix: &[Box<dyn tidepool_bridge::ToHaskell + Send>],
        table: &DataConTable,
    ) -> Result<PreparedResumed, PreparedRuntimeError> {
        let handle = self
            .machine
            .prepared_handle_of(raw)
            .ok_or(PreparedRuntimeError::UnknownHandle)?;
        let (realm, evidence) = self.machine.parked(id).ok_or(PreparedRuntimeError::Run(
            ExecutionError::UnknownContinuation(id),
        ))?;
        let evidence = *evidence;
        let (owner_id, wire, site) = self.structural_reply(evidence.reply)?;
        let (programs, machine) = (&self.programs, &mut self.machine);
        let owner = programs.get(&owner_id).ok_or(PreparedRuntimeError::Run(
            ExecutionError::UnknownProgram(owner_id),
        ))?;
        let mut budget = TypeWorkBudget::new(GraphLimits::default().max_work);
        let root = owner
            .types
            .open_root(wire, &mut budget)
            .map_err(|source| PreparedRuntimeError::AnswerTypeEvidence { site, source })?;
        let fields = owner
            .selected_fields(&root, constructor, &mut budget)
            .map_err(|source| PreparedRuntimeError::AnswerTypeEvidence { site, source })?
            .ok_or(PreparedRuntimeError::AnswerConstructor {
                site,
                host_id: constructor,
            })?;
        if fields.len() != prefix.len() + 1 {
            return Err(PreparedRuntimeError::AnswerShape {
                site,
                detail: "the framed constructor's declared field count does not match the supplied prefix plus the borrowed handle field",
            });
        }
        if machine.cancellation_requested(realm) {
            return Err(PreparedRuntimeError::Cancelled);
        }
        let mut builder = machine
            .managed_builder()
            .map_err(PreparedRuntimeError::Run)?;
        let root = build_framed_structural_node(
            prefix,
            handle,
            constructor,
            fields,
            table,
            site,
            root,
            budget,
            owner,
            &mut builder,
        )?;
        let built = builder
            .finish(realm, root)
            .map_err(PreparedRuntimeError::Run)?;
        self.resume_parked(id, built)
    }

    /// Borrow a live result under compiler-checked single-field wrappers.
    /// Construction is transactional; a refused wrapper keeps both the root
    /// and parked continuation live.
    pub fn resume_with_nested_handle(
        &mut self,
        id: ContinuationId,
        raw: ValueHandle,
        constructors: &[DataConId],
    ) -> Result<PreparedResumed, PreparedRuntimeError> {
        let handle = self
            .machine
            .prepared_handle_of(raw)
            .ok_or(PreparedRuntimeError::UnknownHandle)?;
        let (realm, evidence) = self.machine.parked(id).ok_or(PreparedRuntimeError::Run(
            ExecutionError::UnknownContinuation(id),
        ))?;
        let (owner_id, wire, site) = self.structural_reply(evidence.reply)?;
        if constructors.is_empty() || constructors.len() > MAX_ANSWER_DEPTH {
            return Err(PreparedRuntimeError::AnswerShape {
                site,
                detail: "nested borrowed framing requires a bounded nonempty constructor path",
            });
        }
        let owner = &self.programs[&owner_id];
        let mut budget = TypeWorkBudget::new(GraphLimits::default().max_work);
        let mut cursor = owner
            .types
            .open_root(wire, &mut budget)
            .map_err(|source| PreparedRuntimeError::AnswerTypeEvidence { site, source })?;
        for &constructor in constructors.iter().rev() {
            let fields = owner
                .selected_fields(&cursor, constructor, &mut budget)
                .map_err(|source| PreparedRuntimeError::AnswerTypeEvidence { site, source })?
                .ok_or(PreparedRuntimeError::AnswerConstructor {
                    site,
                    host_id: constructor,
                })?;
            let [field] = fields.as_slice() else {
                return Err(PreparedRuntimeError::AnswerShape {
                    site,
                    detail: "nested borrowed framing requires one field per constructor",
                });
            };
            cursor = field.clone();
        }
        if self.machine.cancellation_requested(realm) {
            return Err(PreparedRuntimeError::Cancelled);
        }
        let mut builder = self
            .machine
            .managed_builder()
            .map_err(PreparedRuntimeError::Run)?;
        let mut field = ManagedField::Handle(handle);
        let mut root = None;
        for &constructor in constructors {
            let node = builder
                .constructor(constructor, &[field])
                .map_err(PreparedRuntimeError::Run)?;
            root = Some(node);
            field = ManagedField::Consume(node);
        }
        let built = builder
            .finish(
                realm,
                root.ok_or(PreparedRuntimeError::AnswerShape {
                    site,
                    detail: "the nested constructor path produced no root",
                })?,
            )
            .map_err(PreparedRuntimeError::Run)?;
        self.resume_parked(id, built)
    }

    /// Validate the owned answer before current-thread admission and frame transfer.
    /// The frame exists,
    /// `answer` is live under its resource scope, the scope is not cancelled.
    fn validate_owned_resume(
        &mut self,
        id: ContinuationId,
        answer: PreparedHandle,
    ) -> Result<(), PreparedRuntimeError> {
        let (realm, _) = self.machine.parked(id).ok_or(PreparedRuntimeError::Run(
            ExecutionError::UnknownContinuation(id),
        ))?;
        if answer.rep() != RuntimeRep::LiftedRef {
            return Err(PreparedRuntimeError::AnswerRepresentation {
                actual: answer.rep(),
            });
        }
        if self.machine.handle_realm(answer) != Some(realm) {
            return Err(PreparedRuntimeError::CrossRealmArgument { realm });
        }
        if self.cancellation_requested(realm) {
            return Err(PreparedRuntimeError::Cancelled);
        }
        Ok(())
    }

    /// Validate and construct an owned response directly from structural
    /// visitor events. Completed children enter the shared incremental
    /// builder immediately, so no intermediate `HaskellValue` tree exists.
    pub fn resume_with_structural_answer(
        &mut self,
        id: ContinuationId,
        response: &dyn tidepool_bridge::ToHaskell,
        table: &DataConTable,
    ) -> Result<PreparedResumed, PreparedRuntimeError> {
        let (realm, evidence) = self.machine.parked(id).ok_or(PreparedRuntimeError::Run(
            ExecutionError::UnknownContinuation(id),
        ))?;
        let evidence = *evidence;
        if self.cancellation_requested(realm) {
            return Err(PreparedRuntimeError::Cancelled);
        }
        let (owner_id, wire, site) = self.structural_reply(evidence.reply)?;
        let (programs, machine) = (&self.programs, &mut self.machine);
        let owner = programs.get(&owner_id).ok_or(PreparedRuntimeError::Run(
            ExecutionError::UnknownProgram(owner_id),
        ))?;
        let mut builder = machine
            .managed_builder()
            .map_err(PreparedRuntimeError::Run)?;
        let root = build_structural_node(response, table, site, wire, owner, &mut builder)?;
        let answer = builder
            .finish(realm, root)
            .map_err(PreparedRuntimeError::Run)?;
        self.resume_parked(id, answer)
    }

    /// Consume the frame parked under `id` without entering it: the
    /// continuation is released and nothing runs. An unknown id is a typed
    /// error with nothing changed.
    pub fn abort_parked(&mut self, id: ContinuationId) -> Result<(), PreparedRuntimeError> {
        let (continuation, _) = self
            .machine
            .take_parked(id)
            .map_err(PreparedRuntimeError::Run)?;
        self.machine.release(continuation);
        Ok(())
    }

    /// Materialize a retained value as a bridge `HaskellValue`, forcing its lazy
    /// fields through `program`'s force adapter. The handle stays retained.
    pub fn observe(
        &mut self,
        program: ProgramId,
        handle: PreparedHandle,
    ) -> Result<HaskellValue, PreparedRuntimeError> {
        self.machine
            .observe_handle(program, handle, RunOptions::default().observation_budget)
            .map_err(PreparedRuntimeError::Run)
    }

    /// [`Self::observe`] under the same budget, but a budget that runs out
    /// CUTS the walk instead of failing it: the result is a bounded SELECTION
    /// carrying `tidepool_codegen::observation::OVERSIZE_SENTINEL` wherever a
    /// subtree was left unread. Use it where a display-sized limit must not
    /// discard work that already ran; the handle stays retained, so the part
    /// the cut omitted is still reachable through the binding.
    pub fn observe_bounded(
        &mut self,
        program: ProgramId,
        handle: PreparedHandle,
    ) -> Result<HaskellValue, PreparedRuntimeError> {
        self.machine
            .observe_handle_bounded(program, handle, RunOptions::default().observation_budget)
            .map_err(PreparedRuntimeError::Run)
    }

    /// The managed fields of one constructor layer of a retained value,
    /// each retained as its own handle under the given resource scope, without forcing. The
    /// pattern-bind lane reads a settled tuple this way: the extractor
    /// projects `(x, y) <- ...` as one tuple entry, so the tuple's fields ARE
    /// the binders, in order. `handle` itself stays retained; the caller
    /// releases it.
    pub fn fields(
        &mut self,
        handle: PreparedHandle,
        realm: RealmId,
        binders: usize,
    ) -> Result<Vec<PreparedHandle>, PreparedRuntimeError> {
        let CodegenPreparedOuter::Constructor { fields, .. } = self
            .machine
            .inspect_outer(handle, realm)
            .map_err(PreparedRuntimeError::Run)?;
        let produced = fields.len();
        let managed: Vec<PreparedHandle> = fields
            .into_iter()
            .filter_map(|field| match field {
                PreparedResult::Managed(handle) => Some(handle),
                PreparedResult::Void | PreparedResult::Scalar(_) => None,
            })
            .collect();
        if managed.len() != binders || produced != binders {
            self.release_all(managed);
            return Err(PreparedRuntimeError::ProjectionShape {
                binders,
                fields: produced,
            });
        }
        Ok(managed)
    }

    /// Move a live handle into ROOT so resource-scope closure cannot release
    /// a value published in the session binding store.
    pub fn adopt(&mut self, handle: PreparedHandle) -> bool {
        self.machine.adopt_handle(handle)
    }

    /// Duplicate a live root into independent custody of the same heap object.
    pub fn retain_handle_value(
        &mut self,
        handle: PreparedHandle,
        owner: RealmId,
    ) -> Result<PreparedHandle, PreparedRuntimeError> {
        self.machine
            .retain_handle_value(handle, owner)
            .map_err(PreparedRuntimeError::Run)
    }

    /// Release one retained handle and deregister its root.
    pub fn release(&mut self, handle: PreparedHandle) -> bool {
        self.machine.release(handle)
    }

    /// [`tidepool_codegen::prepared_program::PreparedMachine::inspect_retained`]:
    /// a non-forcing structural read of one constructor layer of an
    /// arbitrary retained root by its bare cross-engine [`ValueHandle`] id
    /// (a `RootCustody`'s, typically) -- no Haskell compiles, no thunk
    /// forces. Managed fields come back as fresh handles owned by the
    /// handle's own resource scope; the caller releases every one it does
    /// not keep.
    pub fn inspect_retained(
        &mut self,
        handle: ValueHandle,
    ) -> Result<CodegenPreparedOuter, PreparedRuntimeError> {
        self.machine
            .inspect_retained(handle)
            .map_err(PreparedRuntimeError::Run)
    }

    /// [`Self::release`] by the bare cross-engine [`ValueHandle`] id --
    /// `ResidentSession::settle_dropped_custody`'s deferred-cleanup path,
    /// which only ever recovers a dropped [`crate::session::RootCustody`]'s
    /// raw id (see `PreparedMachine::discard_handle`'s doc).
    pub fn discard_handle(&mut self, handle: ValueHandle) -> bool {
        self.machine.discard_handle(handle)
    }

    /// Move a live handle to another runtime resource scope
    /// (`PreparedMachine::rehome_handle`).
    pub fn rehome_handle(&mut self, handle: ValueHandle, owner: RealmId) -> bool {
        self.machine.rehome_handle(handle, owner)
    }

    /// Export the graph the bare cross-engine [`ValueHandle`] roots into a
    /// detached [`Parcel`] (`PreparedMachine::export_parcel`). Like
    /// [`Self::inspect_retained`], this is a non-consuming read: the handle
    /// stays exactly as live afterward, so the caller (`ResidentSession::export_custody`)
    /// releases it itself once the parcel is safely out.
    pub fn export_parcel(&mut self, handle: ValueHandle) -> Result<Parcel, PreparedRuntimeError> {
        let prepared = self
            .machine
            .prepared_handle_of(handle)
            .ok_or(PreparedRuntimeError::Run(
                ExecutionError::UnknownPreparedHandle,
            ))?;
        self.machine
            .export_parcel(prepared)
            .map_err(PreparedRuntimeError::Run)
    }

    /// Import `parcel` under `realm`, rooting its value as a new old-space
    /// arena on this engine's machine, and mint a bare cross-engine
    /// [`ValueHandle`] over it (`PreparedMachine::import_parcel`), mirroring
    /// [`Self::live_payload_handle_owned_by`]'s handle minting for a value
    /// that did not come from a parked frame. The second element pairs each
    /// distinct import identity the parcel's newly installed images name
    /// with the (still-tagged) [`PreparedHandle`] rooting its copied value
    /// — kept as a `PreparedHandle`, not a bare `ValueHandle`, because the
    /// session layer (`ResidentSession::import_parcel`) roots it as a
    /// [`tidepool_codegen::binding_table::BoundValue`], which carries a
    /// `PreparedHandle` the same way any other persistent binding does.
    pub fn import_parcel(
        &mut self,
        parcel: Parcel,
        realm: RealmId,
    ) -> Result<(ValueHandle, Vec<(SymbolIdentity, PreparedHandle)>), PreparedRuntimeError> {
        // Native code and its typed delivery evidence cross as one owner.
        // All site conflicts are refused before the machine imports any roots.
        let facts = parcel
            .images()
            .iter()
            .map(|image| ProgramFacts::from_image(&image.image, None))
            .collect::<Vec<_>>();
        let admitted = self.plan_batch_evidence(facts)?;
        let imported = self
            .machine
            .import_parcel(parcel, realm)
            .map_err(PreparedRuntimeError::Run)?;
        assert_eq!(
            imported.programs.len(),
            admitted.len(),
            "exact parcel image report"
        );
        for ((program, image), admitted) in imported.programs.into_iter().zip(admitted) {
            assert!(
                Arc::ptr_eq(image.definition_facts(), &admitted.facts.definitions),
                "parcel metadata retains the same native image"
            );
            if self.publish_admitted_program(program, admitted) {
                self.installs_since_major += 1;
            }
        }
        Ok((imported.value.raw(), imported.imports))
    }

    pub(crate) fn pending_parcel_import_identities(&self, parcel: &Parcel) -> Vec<SymbolIdentity> {
        self.machine.pending_parcel_import_identities(parcel)
    }

    /// The persistent root slot behind a retained handle, by its bare
    /// cross-engine [`ValueHandle`] id -- `ResidentSession::run_rooted_entry`'s
    /// slot lookup, which only ever holds a `RootCustody`'s raw id (see
    /// [`Self::discard_handle`]'s doc for the same shape).
    #[must_use]
    pub fn handle_slot(
        &self,
        handle: ValueHandle,
    ) -> Option<tidepool_codegen::old_space::RootRef<'_>> {
        self.machine.handle_slot(handle)
    }

    /// Look up a bare cross-engine [`ValueHandle`] (a [`crate::session::RootCustody`]'s
    /// raw id) as this engine's own [`PreparedHandle`], when it is live in
    /// this machine's ledger -- `settle_rooted_entry`/`settle_rooted_application`'s
    /// resolution of a rooted apply's borrowed argument handles.
    #[must_use]
    pub fn prepared_handle_of(&self, handle: ValueHandle) -> Option<PreparedHandle> {
        self.machine.prepared_handle_of(handle)
    }

    /// Release every handle in `handles`.
    pub fn release_all(&mut self, handles: impl IntoIterator<Item = PreparedHandle>) {
        for handle in handles {
            self.machine.release(handle);
        }
    }

    /// Take the first managed value out of a batch of call results, releasing
    /// every other managed value the batch carried. Non-managed results
    /// (`Void`, `Scalar`) are ignored.
    fn take_first_managed(
        &mut self,
        values: impl IntoIterator<Item = PreparedResult>,
    ) -> Option<PreparedHandle> {
        let mut first = None;
        for value in values {
            match (value, &first) {
                (PreparedResult::Managed(handle), None) => first = Some(handle),
                (PreparedResult::Managed(handle), Some(_)) => {
                    self.machine.release(handle);
                }
                (PreparedResult::Void | PreparedResult::Scalar(_), _) => {}
            }
        }
        first
    }

    /// Close a runtime resource scope: `(frames, handles_released)`.
    pub fn close_realm(&mut self, realm: RealmId) -> (usize, usize) {
        self.machine.close_realm(realm)
    }

    pub(crate) fn cancellation_requested(&mut self, realm: RealmId) -> bool {
        self.machine.cancellation_requested(realm)
    }

    pub(crate) fn set_invocation_cancel(&mut self, cancel: Option<Arc<AtomicBool>>) {
        self.machine.set_invocation_cancel(cancel);
    }

    pub fn cancel_handle(&mut self, realm: RealmId) -> CancelHandle {
        self.machine.realm_cancel_handle(realm)
    }

    #[must_use]
    pub fn disposition(&self) -> MachineDisposition {
        self.machine.disposition()
    }

    #[must_use]
    pub fn failure(&self) -> Option<MachineFailure> {
        self.machine.failure()
    }

    #[must_use]
    pub fn handle_count(&self) -> usize {
        self.machine.handle_count()
    }

    #[must_use]
    pub fn persistent_roots_count(&self) -> usize {
        self.machine.total_persistent_roots()
    }

    /// The stowed roots of parked frames (accounting class 1, root half).
    #[must_use]
    pub fn stowed_roots_count(&self) -> usize {
        self.machine.stowed_roots_count()
    }

    /// The parked continuations (accounting class 1, frame half).
    #[must_use]
    pub fn parked_count(&self) -> usize {
        self.machine.parked_count()
    }

    /// The machine's residency counters at this point (only meaningful
    /// right after [`Self::quiesce_and_collect`] has run; otherwise an
    /// ordinary live snapshot).
    #[must_use]
    pub fn residency(&self) -> tidepool_codegen::prepared_program::ResidencyCounts {
        self.machine.residency()
    }

    /// Lifetime Cranelift work this session's installs have paid for:
    /// `(functions, code_bytes)`. Diff it across one turn to see how much
    /// code generation that turn caused.
    #[must_use]
    pub fn codegen_totals(&self) -> (u64, u64) {
        (
            self.machine.compiled_functions(),
            self.machine.compiled_code_bytes(),
        )
    }

    /// Release a pin taken at install time ([`Self::install`],
    /// [`Self::bootstrap`]). `false` if it was not held (already unpinned,
    /// or an unknown program).
    pub fn unpin(&mut self, program: ProgramId) -> bool {
        self.machine.unpin(program)
    }

    /// Whether [`Self::quiesce_and_collect`] should actually run a major
    /// collection at this between-turn point, rather than return without
    /// touching the machine: either the install-count window has closed
    /// (`installs_since_major >= MAJOR_COLLECTION_INSTALL_INTERVAL`), or live
    /// old-space bytes have grown by at least [`MAJOR_COLLECTION_GROWTH_BYTES`]
    /// or 50% since the baseline recorded at the last major collection
    /// ([`Self::old_bytes_at_last_major`]). The growth check is skipped while
    /// there is no baseline yet (`0`, before any collection has run) -- the
    /// install count alone gates the first collection, matching the
    /// residency test's bound.
    ///
    /// `PreparedMachine::old_bytes_live` is read fresh here: unlike
    /// `residency().block_words` (which only grows with installs and is
    /// therefore redundant with the install-count trigger above), it reads
    /// the old space's own live byte count, so a turn that promotes a large
    /// structure without installing another program still trips the growth
    /// trigger early.
    #[must_use]
    fn major_collection_due(&self) -> bool {
        if self.installs_since_major >= MAJOR_COLLECTION_INSTALL_INTERVAL {
            return true;
        }
        if self.old_bytes_at_last_major == 0 {
            return false;
        }
        let baseline_bytes = self.old_bytes_at_last_major;
        let current_bytes = self.machine.old_bytes_live();
        let grown = current_bytes.saturating_sub(baseline_bytes);
        grown >= MAJOR_COLLECTION_GROWTH_BYTES
            || current_bytes.saturating_mul(2) >= baseline_bytes.saturating_mul(3)
    }

    /// Programs installed since the last successful major collection --
    /// [`Self::bootstrap`] and every accepted [`Self::install`] count.
    /// Exposed for tests and diagnostics; production code drives this only
    /// through [`Self::quiesce_and_collect`].
    #[must_use]
    pub fn installs_since_major(&self) -> usize {
        self.installs_since_major
    }

    /// The between-turn quiescent point, gated by [`Self::major_collection_due`]:
    /// when the policy is not due, this returns `Ok(())` without proving
    /// quiescence or touching the machine at all -- most turns pay nothing
    /// here. When it is due, this proves the machine is quiescent and, if
    /// so, runs a major collection exactly as [`Self::quiesce_and_collect_now`]
    /// does. A `quiesce` refusal ([`ExecutionError::NotQuiescent`]: the
    /// machine is mid-call, holds temporary roots, or an observation borrows
    /// old space) is not reported: the caller is simply not at a quiescent
    /// point yet, the policy's counters are left exactly as they are, and
    /// the next eligible turn retries.
    pub fn quiesce_and_collect(&mut self) -> Result<(), PreparedRuntimeError> {
        if !self.major_collection_due() {
            return Ok(());
        }
        self.quiesce_and_collect_now()
    }

    /// Force the between-turn quiescent point regardless of
    /// [`Self::major_collection_due`]: prove the machine is quiescent
    /// (`PreparedMachine::quiesce`) and, if so, run a major collection and
    /// drain its retirement receipt -- removing each retired program's
    /// [`ProgramFacts`] and re-homing or dropping the site witnesses it
    /// canonically owned ([`Self::retire_site_witnesses`]). A `quiesce`
    /// refusal ([`ExecutionError::NotQuiescent`]) is not reported and resets
    /// nothing (see [`Self::quiesce_and_collect`]'s doc); every other
    /// failure of the gate or the collection is returned. On a successful
    /// collection, [`Self::installs_since_major`] resets to `0` and
    /// [`Self::old_bytes_at_last_major`] is rebaselined from the
    /// post-collection live old-space bytes. The prepared route currently
    /// leases nothing per program, so there are no leases to release here
    /// (see the S4/G2 test module doc below). Used directly by tests and by
    /// any explicit session-level "collect now" entry point.
    pub fn quiesce_and_collect_now(&mut self) -> Result<(), PreparedRuntimeError> {
        let token = match self.machine.quiesce() {
            Ok(token) => token,
            Err(ExecutionError::NotQuiescent) => return Ok(()),
            Err(error) => return Err(PreparedRuntimeError::Run(error)),
        };
        let receipt = match self.machine.collect_major(token) {
            Ok(receipt) => receipt,
            Err(ExecutionError::NotQuiescent) => return Ok(()),
            Err(error) => return Err(PreparedRuntimeError::Run(error)),
        };
        self.old_bytes = receipt.old_bytes;
        for program in &receipt.programs {
            self.programs.remove(program);
        }
        for program in &receipt.programs {
            self.retire_site_witnesses(*program);
        }
        self.installs_since_major = 0;
        self.old_bytes_at_last_major = self.machine.old_bytes_live();
        self.major_collections += 1;
        Ok(())
    }

    /// Read-only heap/GC snapshot. Field mapping onto the prepared machine's
    /// own accounting:
    ///
    /// - `fragments` ↔ installed programs ([`Self::residency`]'s
    ///   `programs`) -- bounded by [`Self::quiesce_and_collect_now`]'s
    ///   retirement, so this count can fall as programs retire;
    /// - `live_bytes` ↔ prepared old-space bytes as of the last successful
    ///   major collection ([`Self::old_bytes`]);
    /// - `gc_count` ↔ major collections actually run
    ///   ([`Self::major_collections`]) -- there is no nursery-collection
    ///   counter exposed at this boundary;
    /// - `nursery_bytes` ↔ `0`: `PreparedMachine` does not expose its
    ///   nursery capacity today, and no consumer of this snapshot reads it.
    #[must_use]
    pub fn heap_stats(&self) -> tidepool_codegen::machine::HeapStats {
        tidepool_codegen::machine::HeapStats {
            nursery_bytes: 0,
            live_bytes: self.old_bytes,
            gc_count: self.major_collections,
            fragments: self.residency().programs as u64,
        }
    }

    /// Prepared old-space bytes as of the last successful
    /// [`Self::quiesce_and_collect`] -- the compacted figure
    /// `RetirementReceipt::old_bytes` reported, not a live recount.
    #[must_use]
    pub fn old_bytes(&self) -> usize {
        self.old_bytes
    }

    /// Transfer immutable admitted evidence to a surviving member of its
    /// compatibility class. Installation checked every duplicate before
    /// publication; retirement cannot introduce another declaration.
    fn retire_site_witnesses(&mut self, retired: ProgramId) {
        let owned: Vec<u64> = self
            .sites
            .iter()
            .filter(|(_, witness)| witness.owner == retired)
            .map(|(site, _)| *site)
            .collect();
        for site in owned {
            let successor = self.programs.iter().find_map(|(owner, facts)| {
                facts
                    .sites
                    .iter()
                    .position(|row| row.site == site)
                    .map(|row| SiteWitness { owner: *owner, row })
            });
            match successor {
                Some(witness) => {
                    self.sites.insert(site, witness);
                }
                None => {
                    self.sites.remove(&site);
                }
            }
        }
        let owned: Vec<DataConId> = self
            .constructor_replies
            .iter()
            .filter(|(_, witness)| witness.owner == retired)
            .map(|(host, _)| *host)
            .collect();
        for host in owned {
            let successor = self.programs.iter().find_map(|(owner, facts)| {
                facts
                    .constructor_replies
                    .iter()
                    .position(|(candidate, _)| *candidate == host)
                    .map(|row| SiteWitness { owner: *owner, row })
            });
            match successor {
                Some(witness) => {
                    self.constructor_replies.insert(host, witness);
                }
                None => {
                    self.constructor_replies.remove(&host);
                }
            }
        }
    }

    /// The unit every home module of `program` was compiled in -- what a
    /// later program's import identity for one of this turn's session
    /// binders names.
    #[must_use]
    pub fn entry_unit(&self, program: ProgramId) -> Option<String> {
        let facts = self.programs.get(&program)?;
        facts
            .tops
            .get(&facts.entry?)
            .map(|(identity, _)| identity.unit.clone())
    }
}

#[cfg(test)]
pub(super) mod tests {
    use super::*;
    use tidepool_bridge::ToHaskell;
    use tidepool_codegen::host_fns::RuntimeError;
    use tidepool_codegen::machine_state::MachineFailure;
    use tidepool_repr::execution_schema::{
        testing, Atom, CheckedLayout, ConstructorDecl, ConstructorId, ExprFrame, FieldLayout,
        GlobalDecl, GlobalId, HeapBinding, ResultContract, ScalarLiteral, Signature, SignatureId,
        StorageLayout, TopBinding, UpdatePolicy, ValueRef,
    };
    use tidepool_repr::type_graph::{
        DeclarationForm, ForAllFlag, NominalHeadKind, ParameterFlag, RootDomain, SourceBinderFlag,
        SyntaxRestriction, TypeEdge, TypeLiteral, TypeNode,
    };
    use tidepool_repr::DataCon;
    use tidepool_repr::SessionModule;
    use tidepool_test_data::prepared as prepared_data;

    #[test]
    fn scientific_plain_and_quasiquoted_programs_share_one_representation() {
        use tidepool_repr::freer_names::find_declared;
        use tidepool_toolchain::artifacts::{compile_invocation, CompileInvocation};

        fn observed_int(value: &HaskellValue) -> i64 {
            match value {
                HaskellValue::Lit(Literal::LitInt(value)) => *value,
                HaskellValue::Con(_, fields) if fields.len() == 1 => observed_int(&fields[0]),
                other => panic!("expected an observed integer: {other:?}"),
            }
        }

        tidepool_testing::eval_harness::require_extract();
        let include = [tidepool_testing::eval_harness::prelude_path()];
        // Keep every compiler-issued group and import owner: at O0 the target
        // still calls Scientific helpers from its certified source closure.
        let artifacts = [
            include_str!("../../tests/fixtures/ScientificPlain.hs"),
            include_str!("../../tests/fixtures/ScientificQuoted.hs"),
        ]
        .map(|source| {
            tidepool_testing::with_settlement(|settlement| {
                compile_invocation(
                    &CompileInvocation {
                        source,
                        targets: &["result"],
                        include: &include,
                        fallback_module_name: "Input",
                    },
                    |_, _, _| {},
                    settlement,
                )
            })
            .expect("compile Scientific fixture with its certified closure")
        });
        let programs = artifacts
            .each_ref()
            .map(|artifact| artifact.targets["result"].prepared.prepared());
        let declarations = programs.map(|program| {
            find_declared(
                program.constructors(),
                "Tidepool.Aeson.Scientific",
                "Scientific",
            )
            .expect("Scientific declaration before execution")
        });
        assert_eq!(declarations[0], declarations[1]);
        assert_eq!(
            declarations[0].field_reps,
            [RuntimeRep::LiftedRef, RuntimeRep::Int(64)]
        );
        let scientific_id = declarations[0].host_id;
        for order in [[0, 1], [1, 0]] {
            let mut state = super::super::PersistentSession::new(None, 4096);
            let first = crate::install_compiled_target(&mut state, &artifacts[order[0]], "result")
                .expect("install first Scientific certified closure");
            let second = crate::install_compiled_target(&mut state, &artifacts[order[1]], "result")
                .expect("second certified closure shares constructor interning");
            let engine = state.require_prepared().expect("shared prepared machine");
            for (index, program) in [(order[0], first), (order[1], second), (order[0], first)] {
                let result = engine
                    .machine
                    .run_entry_with_raw_cancel(
                        program,
                        programs[index].entry(),
                        &[],
                        PreparedCallOptions {
                            collect_before_observation: true,
                            observation_budget: RunOptions::default().observation_budget,
                        },
                        Arc::new(AtomicBool::new(false)),
                    )
                    .expect("Scientific program executes after shared installation");
                let value = &result.values[0];
                let scientific = if index == 1 {
                    let HaskellValue::Con(_, fields) = value else {
                        panic!("expected Number: {value:?}")
                    };
                    assert_eq!(fields.len(), 1);
                    &fields[0]
                } else {
                    value
                };
                let HaskellValue::Con(id, fields) = scientific else {
                    panic!("expected Scientific: {scientific:?}")
                };
                assert_eq!(*id, scientific_id);
                assert_eq!(fields.len(), 2);
                assert_eq!(observed_int(&fields[0]), 42);
                assert_eq!(observed_int(&fields[1]), 0);
            }
            assert!(engine.unpin(first));
            assert!(engine.unpin(second));
        }
    }

    proptest::proptest! {
        #![proptest_config(proptest::test_runner::Config::with_cases(16))]
        #[test]
        fn native_code_custody_is_independent_of_retained_binding_ids(ids in proptest::collection::vec(1u64..10000, 1..8), generation in 1u64..128) {
            use tidepool_repr::execution_schema::{testing, CertifiedGroupCode, GlobalDecl, ModuleVersion};
            let mut wire = testing::wire_program();
            wire.globals.push(GlobalDecl { identity: testing::identity("Fixture", "retained"), rep: RuntimeRep::LiftedRef, entry_signature: None, required_evaluated: false, required_generation: Some(generation) });
            let owner = CachedHomeOwner { unit: "fixture".into(), module: "Fixture".into(), module_version: ModuleVersion([1; 32]), skinny_iface_sha256: [2; 32], product_sha256: [3; 32] };
            let group = testing::projected_group(wire, 9).unwrap();
            let code = CertifiedGroupCode::admit(owner.clone(), group.clone()).unwrap();
            let registry = ImageRegistry::new();
            let image = CompiledProgram::prepare_group_code(&code, &[None], &BTreeMap::new(), &BTreeMap::new(), &registry).unwrap();
            let weak = Arc::downgrade(&image);
            for id in ids {
                let scoped = CertifiedGroup::admit(owner.clone(), group.clone(), vec![ImportOwner::Retained { id: SessionVarId::from_extract(id), generation }]).unwrap();
                let selected = DemandedImage::lookup_with_literals(scoped, &registry, &BTreeMap::new(), &BTreeMap::new()).unwrap();
                proptest::prop_assert!(Arc::ptr_eq(&image, selected.image()));
            }
            drop(image);
            proptest::prop_assert!(weak.upgrade().is_none(), "registry alone retains no executable or literal owner");
        }
    }

    fn certified_source_group(
        name: &str,
        ordinal: u32,
        imported: &str,
    ) -> tidepool_repr::execution_schema::CertifiedGroup {
        certified_source_group_modules("Fixture", name, ordinal, "Fixture", imported)
    }

    pub(in crate::session) fn certified_source_group_modules(
        module: &str,
        name: &str,
        ordinal: u32,
        imported_module: &str,
        imported: &str,
    ) -> CertifiedGroup {
        use tidepool_repr::execution_schema::{
            CachedHomeOwner, CertifiedGroup, ImportOwner, ModuleVersion,
        };
        let mut wire = testing::wire_program();
        if let Group::NonRecursive(top) = &mut wire.bindings[0] {
            top.identity = testing::identity(module, name);
        }
        let binder = testing::identity(imported_module, imported);
        wire.globals.push(GlobalDecl {
            identity: binder.clone(),
            rep: RuntimeRep::LiftedRef,
            entry_signature: None,
            required_evaluated: false,
            required_generation: None,
        });
        CertifiedGroup::admit(
            CachedHomeOwner {
                unit: "fixture".into(),
                module: module.into(),
                module_version: ModuleVersion([1; 32]),
                skinny_iface_sha256: [2; 32],
                product_sha256: [3; 32],
            },
            testing::projected_group(wire, ordinal).unwrap(),
            vec![ImportOwner::Source {
                version: ModuleVersion([1; 32]),
                binder,
            }],
        )
        .unwrap()
    }

    fn certified_source_evidence(
        groups: &[tidepool_repr::execution_schema::CertifiedGroup],
    ) -> BTreeMap<SourceBinder, (CachedHomeOwner, u32)> {
        groups
            .iter()
            .flat_map(|group| {
                group.binders().iter().map(move |binder| {
                    (
                        SourceBinder {
                            version: group.owner().module_version.clone(),
                            binder: binder.clone(),
                        },
                        (group.owner().clone(), group.original_ordinal()),
                    )
                })
            })
            .collect()
    }

    fn certified_source_literal_groups() -> [CertifiedGroup; 2] {
        let owner = CachedHomeOwner {
            unit: "fixture".into(),
            module: "Fixture".into(),
            module_version: tidepool_repr::execution_schema::ModuleVersion([1; 32]),
            skinny_iface_sha256: [2; 32],
            product_sha256: [3; 32],
        };
        let literal = testing::identity("Fixture", "literal");
        let mut producer = testing::wire_program();
        producer.expressions.nodes.clear();
        producer.bindings[0] = Group::NonRecursive(TopBinding {
            identity: literal.clone(),
            binding: HeapBinding {
                id: ValueId(0),
                rhs: HeapRhs::Bytes(b"error\0tail".to_vec()),
            },
        });
        let producer = CertifiedGroup::admit(
            owner.clone(),
            testing::projected_group(producer, 1).unwrap(),
            vec![],
        )
        .unwrap();
        let mut reader = testing::wire_program();
        reader.signatures[0].results = ResultContract::Returns(vec![RuntimeRep::Address]);
        reader.expressions.nodes[0] =
            ExprFrame::Return(vec![Atom::Ref(ValueRef::Global(GlobalId(0)))]);
        if let Group::NonRecursive(top) = &mut reader.bindings[0] {
            top.identity = testing::identity("Fixture", "reader");
            if let HeapRhs::Function { captures, .. } = &mut top.binding.rhs {
                captures.push(ValueRef::Global(GlobalId(0)));
            }
        }
        reader.globals.push(GlobalDecl {
            identity: literal.clone(),
            rep: RuntimeRep::Address,
            entry_signature: None,
            required_evaluated: true,
            required_generation: None,
        });
        let reader = CertifiedGroup::admit(
            owner.clone(),
            testing::projected_group(reader, 38).unwrap(),
            vec![ImportOwner::Source {
                version: owner.module_version,
                binder: literal,
            }],
        )
        .unwrap();
        // Sealed group order need not put literal producers before importers.
        [reader, producer]
    }

    #[test]
    fn ready_source_and_target_lookup_refuses_incomplete_custody_without_compilation() {
        let groups = certified_source_literal_groups();
        let reader = SourceBinder {
            version: groups[0].owner().module_version.clone(),
            binder: testing::identity("Fixture", "reader"),
        };
        let owners = vec![ImportOwner::Source {
            version: reader.version.clone(),
            binder: reader.binder.clone(),
        }];
        let mut wire = testing::wire_program();
        wire.globals.push(GlobalDecl {
            identity: reader.binder.clone(),
            rep: RuntimeRep::LiftedRef,
            entry_signature: None,
            required_evaluated: false,
            required_generation: None,
        });
        let prepared = testing::prepare(wire).unwrap();
        let registry = Arc::new(ImageRegistry::new());
        let mut scopes = tidepool_codegen::scope::ScopeTree::new();
        let scope = scopes.mint_isolated();
        let bindings = BindingTable::new();
        let selection = bindings.source_domain_selection_in(&scopes, scope).unwrap();
        let snapshot = bindings.scope_snapshot(&scopes, scope).unwrap();
        let (selected, inherited, roots) =
            tidepool_codegen::prepared_program::GroupInventory::new(&groups)
                .unwrap()
                .seal_in_domains([reader], &selection)
                .unwrap();
        assert!(inherited.is_empty());
        let resolved = super::super::persistent::ResolvedCertifiedTurn {
            groups: selected,
            target_owners: owners,
            package_interfaces: CertifiedTargetPackageInterfaces::default(),
            source_evidence: certified_source_evidence(&groups),
            inherited_needed: vec![],
            source_plan: super::super::persistent::ResolvedSourceDomainPlan::fixture(
                roots, selection, snapshot,
            ),
        };
        let absent = NativeImageBundle {
            registry: registry.clone(),
            images: BTreeMap::new(),
            target: 0,
        };
        let before = CompiledProgram::successful_image_compilations();
        assert!(matches!(
            CertifiedTargetImage::lookup_scoped(prepared.clone(), &resolved, &absent),
            Err(PreparedRuntimeError::MissingPreparedNativeImage)
        ));
        assert_eq!(CompiledProgram::successful_image_compilations(), before);
        let (target, demanded) =
            CertifiedTargetImage::compile_scoped(prepared.clone(), &resolved, &registry).unwrap();
        let complete = NativeImageBundle {
            registry: registry.clone(),
            images: std::iter::once(target.image.clone())
                .chain(demanded.iter().map(|image| image.image().clone()))
                .map(|image| (Arc::as_ptr(&image) as usize, image))
                .collect(),
            target: Arc::as_ptr(&target.image) as usize,
        };
        let before = CompiledProgram::successful_image_compilations();
        for key in complete.images.keys() {
            let incomplete = complete.omitting_image(*key);
            assert!(matches!(
                CertifiedTargetImage::lookup_scoped(prepared.clone(), &resolved, &incomplete),
                Err(PreparedRuntimeError::MissingPreparedNativeImage)
            ));
            assert_eq!(CompiledProgram::successful_image_compilations(), before);
        }
        let (looked_up, groups) =
            CertifiedTargetImage::lookup_scoped(prepared.clone(), &resolved, &complete).unwrap();
        assert!(Arc::ptr_eq(&looked_up.image, &target.image));
        assert!(groups
            .iter()
            .zip(&demanded)
            .all(|(a, b)| Arc::ptr_eq(a.image(), b.image())));
        assert_eq!(CompiledProgram::successful_image_compilations(), before);
        drop((target, demanded, looked_up, groups, complete));
        assert!(
            matches!(
                CertifiedTargetImage::lookup_scoped(prepared, &resolved, &absent),
                Err(PreparedRuntimeError::MissingPreparedNativeImage)
            ),
            "expired keys cannot create a new image"
        );
        assert_eq!(CompiledProgram::successful_image_compilations(), before);
    }

    #[test]
    fn certified_source_literals_require_selected_original_producers() {
        let registry = ImageRegistry::new();
        let target = CertifiedTargetImage::compile(
            testing::prepare(testing::wire_program()).unwrap(),
            &registry,
        )
        .unwrap();
        let groups = certified_source_literal_groups();
        assert!(target
            .compile_demanded([groups[0].clone()], &registry)
            .is_err());
        let demanded = target.compile_demanded(groups.clone(), &registry).unwrap();
        assert_eq!(demanded[0].group(), &groups[0]);
        assert_eq!(demanded[1].group(), &groups[1]);
        assert_eq!(
            demanded[0]
                .image()
                .authenticated_source_literal(GlobalId(0)),
            Some(&SourceBinder {
                version: groups[1].owner().module_version.clone(),
                binder: testing::identity("Fixture", "literal"),
            }),
        );
        assert!(matches!(
            target.compile_demanded([groups[1].clone(), groups[1].clone()], &registry),
            Err(DemandError::DuplicateGroup { unit, module, ordinal })
                if unit == groups[1].owner().unit
                    && module == groups[1].owner().module
                    && ordinal == groups[1].original_ordinal()
        ));
    }

    #[test]
    fn certified_source_literals_validate_provenance_without_managed_byte_leases() {
        let groups = certified_source_literal_groups();
        let evidence = certified_source_evidence(&groups);
        let reader = testing::identity("Fixture", "reader");
        let owner = ImportOwner::Source {
            version: groups[0].owner().module_version.clone(),
            binder: reader.clone(),
        };
        let mut wire = testing::wire_program();
        wire.signatures[0].results = ResultContract::Returns(vec![RuntimeRep::Address]);
        wire.expressions.nodes[0] = ExprFrame::Call {
            callee: Atom::Ref(ValueRef::Global(GlobalId(0))),
            signature: SignatureId(0),
            arguments: vec![],
        };
        wire.globals.push(GlobalDecl {
            identity: reader,
            rep: RuntimeRep::LiftedRef,
            entry_signature: Some(SignatureId(0)),
            required_evaluated: true,
            required_generation: None,
        });
        let prepared = testing::prepare(wire).unwrap();
        let registry = ImageRegistry::new();
        let target = || CertifiedTargetImage::compile(prepared.clone(), &registry).unwrap();
        let demanded = || {
            target()
                .compile_demanded(groups.clone(), &registry)
                .unwrap()
        };
        let (mut engine, _) =
            PreparedEngine::bootstrap(testing::prepare(testing::wire_program()).unwrap()).unwrap();
        let before = engine.residency();
        let literal = SourceBinder {
            version: groups[1].owner().module_version.clone(),
            binder: testing::identity("Fixture", "literal"),
        };
        for mutation in 0..6 {
            let mut wrong = evidence.clone();
            match mutation {
                0 => {
                    wrong.remove(&literal);
                }
                1 => wrong.get_mut(&literal).unwrap().0.product_sha256 = [9; 32],
                2 => wrong.get_mut(&literal).unwrap().0.skinny_iface_sha256 = [9; 32],
                3 => {
                    wrong.get_mut(&literal).unwrap().0.module_version =
                        tidepool_repr::execution_schema::ModuleVersion([9; 32])
                }
                4 => wrong.get_mut(&literal).unwrap().1 += 1,
                5 => wrong.get_mut(&literal).unwrap().0.module = "Other".into(),
                _ => unreachable!(),
            }
            assert!(matches!(
                engine.install_certified_turn(
                    target(),
                    &[owner.clone()],
                    &wrong,
                    demanded(),
                    &[],
                    &BTreeMap::new(),
                    &HashMap::new(),
                    &BindingTable::new(),
                ),
                Err(PreparedRuntimeError::InvalidCertifiedSourceOwner(_))
            ));
            assert_eq!(engine.residency(), before);
        }
        let mut installed = engine
            .install_certified_turn(
                target(),
                &[owner],
                &evidence,
                demanded(),
                &[],
                &BTreeMap::new(),
                &HashMap::new(),
                &BindingTable::new(),
            )
            .unwrap();
        assert_eq!(installed.groups.len(), 2);
        assert_eq!(installed.leases.len(), 1);
        assert_eq!(installed.leases[0].binder().binder.occurrence, "reader");
        let leases = std::mem::take(&mut installed.leases);
        let literal_program = installed.groups[1];
        let id = engine.commit_certified_turn(installed);
        let result = engine
            .machine
            .run_entry_retained(
                id,
                ValueId(0),
                &[],
                PreparedCallOptions {
                    observation_budget: 0,
                    collect_before_observation: true,
                },
                RealmId::ROOT,
            )
            .unwrap();
        let [PreparedResult::Scalar(address)] = result.values.as_slice() else {
            panic!("literal reader returns one address");
        };
        engine.quiesce_and_collect_now().unwrap();
        assert!(!engine.programs.contains_key(&literal_program));
        // SAFETY: the pinned importer image owns the complete literal allocation.
        let bytes = unsafe { std::slice::from_raw_parts(*address as *const u8, 11) };
        assert_eq!(bytes, b"error\0tail\0");
        for lease in leases {
            assert!(engine.release(lease.handle()));
        }
        assert!(engine.unpin(id));
        engine.quiesce_and_collect_now().unwrap();
        assert_eq!(engine.residency(), before);
    }

    #[test]
    fn certified_target_only_literal_preserves_custody_and_collection_without_leases() {
        let [_, producer] = certified_source_literal_groups();
        let groups = vec![producer];
        let evidence = certified_source_evidence(&groups);
        let literal = SourceBinder {
            version: groups[0].owner().module_version.clone(),
            binder: testing::identity("Fixture", "literal"),
        };
        let owners = vec![ImportOwner::Source {
            version: literal.version.clone(),
            binder: literal.binder.clone(),
        }];
        let mut wire = testing::wire_program();
        wire.signatures[0].results = ResultContract::Returns(vec![RuntimeRep::Address]);
        wire.expressions.nodes[0] =
            ExprFrame::Return(vec![Atom::Ref(ValueRef::Global(GlobalId(0)))]);
        wire.globals.push(GlobalDecl {
            identity: literal.binder.clone(),
            rep: RuntimeRep::Address,
            entry_signature: None,
            required_evaluated: true,
            required_generation: None,
        });
        let prepared = testing::prepare(wire).unwrap();
        let registry = ImageRegistry::new();
        let mut scopes = tidepool_codegen::scope::ScopeTree::new();
        let scope = scopes.mint_isolated();
        let bindings = BindingTable::new();
        let selection = bindings.source_domain_selection_in(&scopes, scope).unwrap();
        let snapshot = bindings.scope_snapshot(&scopes, scope).unwrap();
        let (selected, inherited, roots) =
            tidepool_codegen::prepared_program::GroupInventory::new(&groups)
                .unwrap()
                .seal_in_domains([literal.clone()], &selection)
                .unwrap();
        assert!(inherited.is_empty());
        let resolved = super::super::persistent::ResolvedCertifiedTurn {
            groups: selected,
            target_owners: owners.clone(),
            package_interfaces: CertifiedTargetPackageInterfaces::default(),
            source_evidence: evidence.clone(),
            inherited_needed: vec![],
            source_plan: super::super::persistent::ResolvedSourceDomainPlan::fixture(
                roots, selection, snapshot,
            ),
        };
        let compile = || {
            CertifiedTargetImage::compile_scoped(prepared.clone(), &resolved, &registry).unwrap()
        };
        let (target, demanded) = compile();
        assert_eq!(
            target.image.authenticated_source_literal(GlobalId(0)),
            Some(&literal)
        );
        assert_eq!(demanded.len(), 1);
        assert_eq!(demanded[0].group(), &groups[0]);
        let (mut engine, _) =
            PreparedEngine::bootstrap(testing::prepare(testing::wire_program()).unwrap()).unwrap();
        let before = engine.residency();
        for mutation in 0..6 {
            let mut wrong = evidence.clone();
            match mutation {
                0 => {
                    wrong.remove(&literal);
                }
                1 => wrong.get_mut(&literal).unwrap().0.product_sha256 = [9; 32],
                2 => wrong.get_mut(&literal).unwrap().0.skinny_iface_sha256 = [9; 32],
                3 => {
                    wrong.get_mut(&literal).unwrap().0.module_version =
                        tidepool_repr::execution_schema::ModuleVersion([9; 32])
                }
                4 => wrong.get_mut(&literal).unwrap().1 += 1,
                5 => wrong.get_mut(&literal).unwrap().0.module = "Other".into(),
                _ => unreachable!(),
            }
            let (target, demanded) = compile();
            assert!(matches!(
                engine.install_certified_turn(
                    target,
                    &owners,
                    &wrong,
                    demanded,
                    &[],
                    &BTreeMap::new(),
                    &HashMap::new(),
                    &bindings,
                ),
                Err(PreparedRuntimeError::InvalidCertifiedSourceOwner(_))
            ));
            assert_eq!(engine.residency(), before);
        }
        let installed = engine
            .install_certified_turn(
                target,
                &owners,
                &evidence,
                demanded,
                &[],
                &BTreeMap::new(),
                &HashMap::new(),
                &bindings,
            )
            .unwrap();
        assert!(installed.leases.is_empty());
        let producer = installed.groups[0];
        let id = engine.commit_certified_turn(installed);
        let result = engine
            .machine
            .run_entry_retained(
                id,
                ValueId(0),
                &[],
                PreparedCallOptions {
                    observation_budget: 0,
                    collect_before_observation: true,
                },
                RealmId::ROOT,
            )
            .unwrap();
        let [PreparedResult::Scalar(address)] = result.values.as_slice() else {
            panic!("target returns its authenticated source address");
        };
        engine.quiesce_and_collect_now().unwrap();
        assert!(!engine.programs.contains_key(&producer));
        // SAFETY: the pinned target image owns this authenticated literal allocation.
        let bytes = unsafe { std::slice::from_raw_parts(*address as *const u8, 11) };
        assert_eq!(bytes, b"error\0tail\0");
        assert!(engine.unpin(id));
        engine.quiesce_and_collect_now().unwrap();
        assert_eq!(engine.residency(), before);

        let counts = (registry.hits(), registry.misses());
        for requested in [vec![], vec![owners[0].clone(), owners[0].clone()]] {
            assert!(matches!(
                CertifiedTargetImage::compile_originals(
                    prepared.clone(),
                    &requested,
                    &registry,
                    CertifiedTargetPackageInterfaces::default(),
                    groups.clone(),
                ),
                Err(PreparedRuntimeError::CertifiedTargetOwners)
            ));
        }
        let wrong = [ImportOwner::Source {
            version: literal.version.clone(),
            binder: testing::identity("Other", "literal"),
        }];
        assert!(matches!(
            CertifiedTargetImage::compile_originals(
                prepared.clone(),
                &wrong,
                &registry,
                CertifiedTargetPackageInterfaces::default(),
                groups.clone(),
            ),
            Err(PreparedRuntimeError::CertifiedTargetOwners)
        ));
        assert_eq!((registry.hits(), registry.misses()), counts);
        assert!(matches!(
            CertifiedTargetImage::compile_originals(
                prepared,
                &owners,
                &registry,
                CertifiedTargetPackageInterfaces::default(),
                vec![],
            ),
            Err(PreparedRuntimeError::Compile(CompileError::Unsupported(_)))
        ));
    }

    #[test]
    fn uncertified_target_literals_cannot_specialize_package_addresses() {
        let registry = ImageRegistry::new();
        let mut literal = testing::identity("Fixture.Package", "literal");
        literal.unit = "fixture-package".into();
        let mut target_wire = testing::wire_program();
        target_wire.bindings.push(Group::NonRecursive(TopBinding {
            identity: literal.clone(),
            binding: HeapBinding {
                id: ValueId(1),
                rhs: HeapRhs::Bytes(b"literal".to_vec()),
            },
        }));
        let target =
            CertifiedTargetImage::compile(testing::prepare(target_wire).unwrap(), &registry)
                .unwrap();
        assert_eq!(target.image.package_literals(|_, _| Some([9; 32])).len(), 1);
        assert!(target.package_literals.is_empty());
        let mut group_wire = testing::wire_program();
        group_wire.globals.push(GlobalDecl {
            identity: literal.clone(),
            rep: RuntimeRep::Address,
            entry_signature: None,
            required_evaluated: true,
            required_generation: None,
        });
        let group = CertifiedGroup::admit(
            CachedHomeOwner {
                unit: "fixture".into(),
                module: "Fixture".into(),
                module_version: tidepool_repr::execution_schema::ModuleVersion([1; 32]),
                skinny_iface_sha256: [2; 32],
                product_sha256: [3; 32],
            },
            testing::projected_group(group_wire, 7).unwrap(),
            vec![ImportOwner::Package {
                unit: literal.unit.clone(),
                module: literal.module.clone(),
                binder: literal,
                interface_digest: [9; 32],
            }],
        )
        .unwrap();
        assert!(target.compile_demanded([group], &registry).is_err());
        assert!(exportable_code_tops(target.prepared())
            .iter()
            .all(|(identity, _, _)| identity.unit != "fixture-package"));
    }

    #[test]
    fn prepared_helpers_require_the_entry_symbol_owner() {
        for include_owned in [false, true] {
            for owned_first in [false, true] {
                let mut wire = testing::wire_program();
                let template = wire.bindings[0].clone();
                let mut expected = Vec::new();
                for occurrence in [
                    PREPARED_RESUME_TARGET,
                    PREPARED_APPLY_ENTRY_TARGET,
                    PREPARED_APPLY_VALUE_TARGET,
                ] {
                    let mut owned_id = None;
                    for owned in [owned_first, !owned_first] {
                        if owned && !include_owned {
                            continue;
                        }
                        let Group::NonRecursive(mut top) = template.clone() else {
                            unreachable!()
                        };
                        top.identity.occurrence = occurrence.into();
                        if !owned {
                            top.identity.unit = "foreign-unit".into();
                        }
                        top.binding.id = ValueId(wire.bindings.len() as u32);
                        let HeapRhs::Function { body, .. } = &mut top.binding.rhs else {
                            unreachable!()
                        };
                        *body = wire.expressions.nodes.len();
                        wire.expressions
                            .nodes
                            .push(wire.expressions.nodes[0].clone());
                        if owned {
                            owned_id = Some(top.binding.id);
                        }
                        wire.bindings.push(Group::NonRecursive(top));
                    }
                    expected.push(owned_id);
                }
                let prepared = testing::prepare(wire).unwrap();
                let facts = ProgramFacts::of(&prepared);
                assert_eq!(
                    [facts.resume, facts.apply_entry, facts.apply_value],
                    expected.as_slice(),
                );
            }
        }
    }

    fn settled_test_facts(declarations: &[(&str, &str, DataConId)]) -> ProgramFacts {
        constructor_test_facts("Tidepool.Internal.Resume", "Settled", declarations)
    }

    fn constructor_test_facts(
        module: &str,
        family: &str,
        declarations: &[(&str, &str, DataConId)],
    ) -> ProgramFacts {
        let constructors = declarations
            .iter()
            .map(|(unit, occurrence, host_id)| {
                let mut identity = testing::identity(module, occurrence);
                identity.unit = (*unit).into();
                identity.namespace = "constructor".into();
                let mut family = testing::identity(module, family);
                family.unit = (*unit).into();
                family.namespace = "type".into();
                (identity, *host_id, family)
            })
            .collect::<Vec<_>>();
        let mut by_identity = BTreeMap::<_, BTreeMap<_, Vec<usize>>>::new();
        for (index, (identity, _, _)) in constructors.iter().enumerate() {
            by_identity
                .entry(identity.module.clone())
                .or_default()
                .entry(identity.occurrence.clone())
                .or_default()
                .push(index);
        }
        ProgramFacts::from_definitions(
            Arc::new(DefinitionFacts {
                tops: BTreeMap::new(),
                sites: Vec::new(),
                types: Arc::default(),
                constructor_replies: Vec::new(),
                constructors: constructors.into(),
                json_layout: None,
                by_identity,
            }),
            None,
        )
    }

    #[test]
    fn builtin_constructor_lookup_rejects_same_spelling_from_different_units() {
        for (module, family, occurrence) in [
            (TEXT_MODULE, "Text", "Text"),
            (INTEGER_MODULE, "Integer", "IS"),
            (NATURAL_MODULE, "Natural", "NS"),
        ] {
            let canonical = ("selected-package", occurrence, DataConId(1));
            let original = constructor_test_facts(module, family, &[canonical]);
            assert_eq!(
                original.constructor_named(module, occurrence),
                Some(DataConId(1))
            );
            for shadow in [
                ("foreign-unit", occurrence, DataConId(2)),
                ("foreign-unit", occurrence, DataConId(1)),
                ("selected-package", occurrence, DataConId(2)),
            ] {
                for declarations in [[canonical, shadow], [shadow, canonical]] {
                    let mixed = constructor_test_facts(module, family, &declarations);
                    assert_eq!(mixed.constructor_named(module, occurrence), None);
                }
            }
        }
    }

    #[test]
    fn certified_settled_pair_joins_exact_constructors_across_live_owners() {
        let done = settled_test_facts(&[("fixture", "Done", DataConId(1))]);
        let suspended = settled_test_facts(&[("fixture", "Suspended", DataConId(2))]);
        let target = settled_test_facts(&[]);

        assert_eq!(done.settled, None);
        assert_eq!(suspended.settled, None);
        assert_eq!(
            SettledIds::from_facts([&done, &suspended, &target]).unwrap(),
            Some(SettledIds {
                done: DataConId(1),
                suspended: DataConId(2),
            })
        );

        let conflicting_done = settled_test_facts(&[("other-unit", "Done", DataConId(1))]);
        assert!(matches!(
            SettledIds::from_facts([&done, &suspended, &conflicting_done]),
            Err(PreparedRuntimeError::ConflictingSettledConstructors)
        ));

        let duplicate_done = settled_test_facts(&[
            ("fixture", "Done", DataConId(1)),
            ("fixture", "Done", DataConId(3)),
        ]);
        assert!(matches!(
            SettledIds::from_facts([&duplicate_done, &suspended]),
            Err(PreparedRuntimeError::ConflictingSettledConstructors)
        ));

        let split_units = settled_test_facts(&[("other-unit", "Suspended", DataConId(2))]);
        assert!(matches!(
            SettledIds::from_facts([&done, &split_units]),
            Err(PreparedRuntimeError::ConflictingSettledConstructors)
        ));
    }

    #[test]
    fn certified_demand_installs_cyclic_groups_with_distinct_instances() {
        use tidepool_codegen::prepared_program::GroupInventory;
        let groups = [
            certified_source_group("a", 2, "b"),
            certified_source_group("b", 7, "a"),
        ];
        let inventory = GroupInventory::new(&groups).unwrap();
        let demand = inventory
            .seal([SourceBinder {
                version: tidepool_repr::execution_schema::ModuleVersion([1; 32]),
                binder: testing::identity("Fixture", "a"),
            }])
            .unwrap();
        let registry = ImageRegistry::new();
        let (mut engine, base) =
            PreparedEngine::bootstrap(testing::prepare(testing::wire_program()).unwrap()).unwrap();
        let bindings = BindingTable::new();
        let first = engine
            .install_certified_demand(
                demand.compile(&registry).unwrap(),
                &HashMap::new(),
                &bindings,
            )
            .unwrap();
        let second = engine
            .install_certified_demand(
                demand.compile(&registry).unwrap(),
                &HashMap::new(),
                &bindings,
            )
            .unwrap();
        assert_eq!(first.len(), 2);
        assert_eq!(second.len(), 2);
        assert_ne!(first, second);
        for (first, second) in first.iter().zip(&second) {
            assert!(Arc::ptr_eq(
                &engine.programs[first].definitions,
                &engine.programs[second].definitions,
            ));
        }
        assert_eq!(registry.misses(), 2);
        assert_eq!(registry.hits(), 2);
        assert_eq!(engine.residency().programs, 5);
        for id in first.into_iter().chain(second) {
            assert!(engine.unpin(id));
        }
        assert!(engine.unpin(base));
        engine.quiesce_and_collect_now().unwrap();
        // Bootstrap's package code export retains its original program.
        assert_eq!(engine.residency().programs, 1);
        assert_eq!(engine.programs.len(), 1);
        assert!(engine.programs.contains_key(&base));
    }

    #[test]
    fn certified_target_and_reachable_groups_install_with_source_leases() {
        use tidepool_codegen::prepared_program::GroupInventory;
        use tidepool_repr::execution_schema::{ImportOwner, ModuleVersion};
        let groups = [
            certified_source_group("a", 2, "b"),
            certified_source_group("b", 7, "a"),
            certified_source_group("unused", 12, "unused"),
        ];
        let source_evidence = certified_source_evidence(&groups);
        let root = SourceBinder {
            version: ModuleVersion([1; 32]),
            binder: testing::identity("Fixture", "a"),
        };
        let registry = ImageRegistry::new();
        let demand = GroupInventory::new(&groups)
            .unwrap()
            .seal([root.clone()])
            .unwrap();
        let mut wire = testing::wire_program();
        wire.signatures[0].results = ResultContract::Returns(vec![RuntimeRep::LiftedRef]);
        wire.expressions.nodes[0] =
            ExprFrame::Return(vec![Atom::Ref(ValueRef::Global(GlobalId(0)))]);
        wire.globals.push(GlobalDecl {
            identity: root.binder.clone(),
            rep: RuntimeRep::LiftedRef,
            entry_signature: None,
            required_evaluated: false,
            required_generation: None,
        });
        let target =
            CertifiedTargetImage::compile(testing::prepare(wire).unwrap(), &registry).unwrap();
        let target_prepared = target.prepared.clone();
        let owner = ImportOwner::Source {
            version: root.version.clone(),
            binder: root.binder.clone(),
        };
        let (mut engine, bootstrap) =
            PreparedEngine::bootstrap(testing::prepare(testing::wire_program()).unwrap()).unwrap();
        let before = engine.residency();
        let mut wrong_source_evidence = source_evidence.clone();
        wrong_source_evidence
            .get_mut(&root)
            .unwrap()
            .0
            .product_sha256 = [99; 32];
        assert!(matches!(
            engine.install_certified_turn(
                CertifiedTargetImage::compile(target.prepared.clone(), &registry).unwrap(),
                &[owner.clone()],
                &wrong_source_evidence,
                demand.compile(&registry).unwrap(),
                &[],
                &BTreeMap::new(),
                &HashMap::new(),
                &BindingTable::new(),
            ),
            Err(PreparedRuntimeError::InvalidCertifiedSourceOwner(_))
        ));
        assert_eq!(engine.residency(), before);
        assert!(matches!(
            engine.install_certified_turn(
                CertifiedTargetImage::compile(target.prepared.clone(), &registry).unwrap(),
                &[],
                &source_evidence,
                demand.compile(&registry).unwrap(),
                &[],
                &BTreeMap::new(),
                &HashMap::new(),
                &BindingTable::new(),
            ),
            Err(PreparedRuntimeError::CertifiedTargetOwners)
        ));
        assert_eq!(engine.residency(), before);
        let with_unused = GroupInventory::new(&groups)
            .unwrap()
            .seal([
                root.clone(),
                SourceBinder {
                    version: ModuleVersion([1; 32]),
                    binder: testing::identity("Fixture", "unused"),
                },
            ])
            .unwrap();
        assert!(matches!(
            engine.install_certified_turn(
                CertifiedTargetImage::compile(target.prepared.clone(), &registry).unwrap(),
                &[owner.clone()],
                &source_evidence,
                with_unused.compile(&registry).unwrap(),
                &[],
                &BTreeMap::new(),
                &HashMap::new(),
                &BindingTable::new(),
            ),
            Err(PreparedRuntimeError::UnreachableCertifiedGroup)
        ));
        assert_eq!(engine.residency(), before);
        let mut installed = engine
            .install_certified_turn(
                target,
                &[owner],
                &source_evidence,
                demand.compile(&registry).unwrap(),
                &[],
                &BTreeMap::new(),
                &HashMap::new(),
                &BindingTable::new(),
            )
            .unwrap();
        assert_eq!(installed.groups.len(), 2);
        assert_eq!(installed.leases.len(), 2);
        assert!(!engine.programs.contains_key(&installed.target));
        let installed_target = installed.target;
        let installed_groups = installed.groups.clone();
        let installed_leases = std::mem::take(&mut installed.leases);
        assert_eq!(engine.commit_certified_turn(installed), installed_target);
        assert_eq!(engine.programs[&installed_target].entry, Some(ValueId(0)));
        assert!(installed_groups
            .iter()
            .all(|id| engine.programs[id].entry.is_none()));
        let selected = installed_leases
            .iter()
            .find(|lease| lease.binder() == &root)
            .unwrap();
        let result = engine
            .machine
            .run_entry_retained(
                installed_target,
                ValueId(0),
                &[],
                PreparedCallOptions {
                    observation_budget: 0,
                    collect_before_observation: false,
                },
                tidepool_codegen::suspension::RealmId::ROOT,
            )
            .unwrap();
        let [PreparedResult::Managed(returned)] = result.values.as_slice() else {
            panic!("target must return a certified source closure");
        };
        assert_eq!(returned.rep(), selected.handle().rep());
        assert!(engine
            .machine
            .handle_is_evaluated(selected.handle())
            .is_ok());
        assert!(engine.machine.release(*returned));
        let inherited = BTreeMap::from([(
            ScopedSourceBinder {
                domain: SourceInstanceDomain::single(),
                source: root.clone(),
            },
            selected.clone(),
        )]);
        let reused = engine
            .install_certified_turn(
                CertifiedTargetImage::compile(target_prepared.clone(), &registry).unwrap(),
                &[ImportOwner::Source {
                    version: root.version.clone(),
                    binder: root.binder.clone(),
                }],
                &source_evidence,
                vec![],
                &[],
                &inherited,
                &HashMap::new(),
                &BindingTable::new(),
            )
            .unwrap();
        assert!(reused.groups.is_empty());
        assert!(reused.leases.is_empty());
        assert_ne!(reused.target, installed_target);
        let reused_target = reused.target;
        assert_eq!(engine.commit_certified_turn(reused), reused_target);
        assert!(Arc::ptr_eq(
            &engine.programs[&installed_target].definitions,
            &engine.programs[&reused_target].definitions,
        ));
        assert!(engine.unpin(reused_target));
        let mut scopes = tidepool_codegen::scope::ScopeTree::new();
        let source_scope = scopes.mint_isolated();
        let capture_scope = scopes.mint_isolated();
        let mut bindings = BindingTable::new();
        let keys = bindings
            .register_source_instances_in(&scopes, source_scope, installed_leases)
            .unwrap();
        assert_eq!(keys.len(), 2);
        assert_eq!(bindings.source_instances_in(&scopes, source_scope).len(), 2);
        bindings.seed_detached_scope(&scopes, source_scope, capture_scope);
        assert_eq!(
            bindings.source_instances_in(&scopes, capture_scope).len(),
            2
        );
        assert!(engine.unpin(installed_target));
        assert_eq!(scopes.retire(source_scope), vec![source_scope]);
        assert!(bindings
            .drain_scope_with_sources(source_scope)
            .source_instances
            .is_empty());
        engine.quiesce_and_collect_now().unwrap();
        assert_eq!(engine.residency().programs, 3);
        assert_eq!(
            bindings.source_instances_in(&scopes, capture_scope).len(),
            2
        );
        assert_eq!(scopes.retire(capture_scope), vec![capture_scope]);
        let retired = bindings
            .drain_scope_with_sources(capture_scope)
            .source_instances;
        assert_eq!(retired.len(), 2);
        for lease in retired {
            assert!(engine.release(lease.handle()));
        }
        engine.quiesce_and_collect_now().unwrap();
        assert_eq!(engine.residency().programs, 1);
        let before_abort = (
            engine.residency(),
            engine.code_export_count(),
            engine.programs.len(),
            engine.sites.len(),
            engine.constructor_replies.len(),
        );
        let mut aborted = engine
            .install_certified_turn(
                CertifiedTargetImage::compile(target_prepared, &registry).unwrap(),
                &[ImportOwner::Source {
                    version: root.version.clone(),
                    binder: root.binder.clone(),
                }],
                &source_evidence,
                demand.compile(&registry).unwrap(),
                &[],
                &BTreeMap::new(),
                &HashMap::new(),
                &bindings,
            )
            .unwrap();
        let dead_scope = scopes.mint_isolated();
        assert_eq!(scopes.retire(dead_scope), vec![dead_scope]);
        let rejected_tokens = std::mem::take(&mut aborted.leases);
        let returned = bindings
            .register_source_instances_in(&scopes, dead_scope, rejected_tokens)
            .unwrap_err();
        assert_eq!(returned.len(), 2);
        assert!(bindings
            .source_instances_in(&scopes, source_scope)
            .is_empty());
        assert!(bindings
            .source_instances_in(&scopes, capture_scope)
            .is_empty());
        assert!(!engine.programs.contains_key(&aborted.target));
        assert_eq!(engine.code_export_count(), before_abort.1);
        engine.abort_certified_turn(aborted, returned).unwrap();
        assert_eq!(
            (
                engine.residency(),
                engine.code_export_count(),
                engine.programs.len(),
                engine.sites.len(),
                engine.constructor_replies.len(),
            ),
            before_abort,
        );
        assert!(engine.unpin(bootstrap));
    }

    pub(in crate::session) fn install_source_publication_fixture(
        session: &mut super::super::PersistentSession,
        scope: tidepool_codegen::scope::ScopeId,
    ) -> (
        ProgramId,
        tidepool_codegen::binding_table::SourceScopeAdmission,
    ) {
        install_selected_source_fixture(session, scope, "a")
    }

    pub(in crate::session) fn install_selected_source_fixture(
        session: &mut super::super::PersistentSession,
        scope: tidepool_codegen::scope::ScopeId,
        root_name: &str,
    ) -> (
        ProgramId,
        tidepool_codegen::binding_table::SourceScopeAdmission,
    ) {
        use tidepool_repr::execution_schema::ModuleVersion;

        let groups = [
            certified_source_group("a", 2, "b"),
            certified_source_group("b", 7, "a"),
            certified_source_group("late", 12, "a"),
        ];
        let root = SourceBinder {
            version: ModuleVersion([1; 32]),
            binder: testing::identity("Fixture", root_name),
        };
        install_selected_groups_fixture(session, scope, &groups, root)
    }

    pub(in crate::session) fn install_selected_groups_fixture(
        session: &mut super::super::PersistentSession,
        scope: tidepool_codegen::scope::ScopeId,
        groups: &[CertifiedGroup],
        root: SourceBinder,
    ) -> (
        ProgramId,
        tidepool_codegen::binding_table::SourceScopeAdmission,
    ) {
        let (target, owners, evidence, demanded, inherited) =
            prepare_selected_groups_fixture(session, scope, groups, &[root]);
        session
            .install_certified_turn_in(scope, target, &owners, &evidence, demanded, &inherited)
            .unwrap()
    }

    pub(in crate::session) fn prepare_selected_groups_fixture(
        session: &super::super::PersistentSession,
        scope: tidepool_codegen::scope::ScopeId,
        groups: &[CertifiedGroup],
        roots: &[SourceBinder],
    ) -> (
        CertifiedTargetImage,
        Vec<ImportOwner>,
        BTreeMap<SourceBinder, (CachedHomeOwner, u32)>,
        Vec<DemandedImage>,
        Vec<InheritedSourceDemand>,
    ) {
        use tidepool_codegen::prepared_program::GroupInventory;
        let selection = session
            .bindings()
            .source_domain_selection_in(session.scope_tree(), scope)
            .unwrap();
        let snapshot = session
            .bindings()
            .scope_snapshot(session.scope_tree(), scope)
            .unwrap();
        let registry = ImageRegistry::new();
        let (selected, inherited, target_sources) = GroupInventory::new(groups)
            .unwrap()
            .seal_in_domains(roots.iter().cloned(), &selection)
            .unwrap();
        let inherited = inherited
            .into_iter()
            .map(|scoped| scoped.into_demand())
            .collect();
        let mut wire = testing::wire_program();
        let Group::NonRecursive(entry) = &mut wire.bindings[0] else {
            unreachable!()
        };
        entry.identity.unit = "main".into();
        for root in roots {
            wire.globals.push(GlobalDecl {
                identity: root.binder.clone(),
                rep: RuntimeRep::LiftedRef,
                entry_signature: None,
                required_evaluated: false,
                required_generation: None,
            });
        }
        let target = CertifiedTargetImage::compile(testing::prepare(wire).unwrap(), &registry)
            .unwrap()
            .with_source_plan(super::super::persistent::ResolvedSourceDomainPlan::fixture(
                target_sources,
                selection,
                snapshot,
            ));
        let demanded = target.compile_scoped_demanded(selected, &registry).unwrap();
        let owners = roots
            .iter()
            .map(|root| ImportOwner::Source {
                version: root.version.clone(),
                binder: root.binder.clone(),
            })
            .collect();
        (
            target,
            owners,
            certified_source_evidence(groups),
            demanded,
            inherited,
        )
    }

    #[test]
    fn certified_target_registration_uses_exact_persistent_scope_custody() {
        let registry = ImageRegistry::new();
        let mut session = super::super::persistent::PersistentSession::new(None, 1024 * 1024);
        let scope = session.mint_isolated_scope();
        let (installed, source_keys) = install_source_publication_fixture(&mut session, scope);
        assert_eq!(
            session
                .bindings()
                .source_instances_in(session.scope_tree(), scope)
                .len(),
            2
        );
        assert_eq!(session.residency().unwrap().programs, 3);
        assert_eq!(session.prepared_mut().unwrap().code_export_count(), 0);
        assert!(matches!(
            session.install_certified_turn_in(
                tidepool_codegen::scope::ScopeId(u64::MAX),
                CertifiedTargetImage::compile(
                    testing::prepare(testing::wire_program()).unwrap(),
                    &registry,
                )
                .unwrap(),
                &[],
                &BTreeMap::new(),
                Vec::new(),
                &[],
            ),
            Err(PreparedRuntimeError::SourceScopeAdmission)
        ));
        assert_eq!(session.residency().unwrap().programs, 3);
        let newly_rooted = session
            .bindings()
            .source_instances_in(session.scope_tree(), scope);
        assert!(session.retire_failed_turn_source_instances(scope, &source_keys));
        assert!(session
            .bindings()
            .source_instances_in(session.scope_tree(), scope)
            .is_empty());
        assert!(session.prepared_mut().unwrap().unpin(installed));
        session
            .prepared_mut()
            .unwrap()
            .quiesce_and_collect_now()
            .unwrap();
        assert_eq!(session.residency().unwrap().programs, 0);
        for lease in newly_rooted {
            assert!(!session.prepared_mut().unwrap().release(lease.handle()));
        }
    }

    #[test]
    fn source_publication_preserves_original_instances_after_uncertain_commit() {
        use super::super::{
            ModuleEnv, PersistentSession, PublicManifestCommit, PublicationDecision,
            PublicationPhase, RecoveryPublicOwner, SessionError, SessionLib,
        };
        use tidepool_codegen::binding_table::BindingPromotionError;
        use tidepool_codegen::scope::ScopeId;

        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("declarations.json");
        let mut lib = SessionLib::open(
            tidepool_repr::SessionId(773),
            root.path().join("include"),
            ModuleEnv::standalone_default(),
        )
        .unwrap();
        tidepool_testing::with_settlement(|settlement| {
            lib.attach_recovery_graph_v2(&path, settlement)
        })
        .unwrap();
        let mut session = PersistentSession::new(Some(lib), 1024 * 1024);
        let public = session.mint_scope(ScopeId::ROOT).unwrap();
        let private = session.mint_detached_scope(public).unwrap();
        let foreign = session.mint_detached_scope(public).unwrap();
        let owner =
            RecoveryPublicOwner::new(&tidepool_repr::ActorPath::parse("root/source").unwrap(), 1)
                .unwrap();
        session
            .bind_durable_public_scope(owner.clone(), public)
            .unwrap();
        let (target, admission) = install_source_publication_fixture(&mut session, private);
        let mut keys = admission.to_vec();
        keys.sort();
        assert_eq!(keys.len(), 2);
        assert!(session.prepared_mut().unwrap().unpin(target));
        let incarnation = session
            .public_visibility_snapshot_in(private)
            .unwrap()
            .machine_incarnation
            .unwrap();
        assert!(matches!(
            session.snapshot_publication(owner.clone(), public, foreign, vec![], keys.to_vec()),
            Err(SessionError::InvalidPublicBindingPromotion(
                BindingPromotionError::MissingOrForeignSourceInstance
            ))
        ));

        let cancel_stage = session
            .snapshot_publication(owner.clone(), public, private, vec![], keys.to_vec())
            .unwrap()
            .stage()
            .unwrap();
        let cancelled = PublicationDecision::new();
        cancelled.request_cancellation();
        assert_eq!(
            session
                .publish_staged_public_manifest(cancel_stage, &cancelled)
                .unwrap(),
            PublicManifestCommit::Cancelled
        );
        assert!(session
            .public_visibility_snapshot_in(public)
            .unwrap()
            .source_instances
            .is_empty());
        assert!(!path.exists());

        let stage = session
            .snapshot_publication(owner.clone(), public, private, vec![], keys.to_vec())
            .unwrap()
            .stage()
            .unwrap();
        let old_stage = session
            .snapshot_publication(owner.clone(), public, private, vec![], keys.to_vec())
            .unwrap()
            .stage()
            .unwrap();
        session.lib_mut().fail_recovery_durability_once = true;
        let decision = PublicationDecision::new();
        assert!(matches!(
            session
                .publish_staged_public_manifest(stage, &decision)
                .unwrap(),
            PublicManifestCommit::PublishedDurabilityUnconfirmed { .. }
        ));
        assert_eq!(decision.phase(), PublicationPhase::Published);
        let published = session.public_visibility_snapshot_in(public).unwrap();
        assert_eq!(published.epoch, 1);
        assert_eq!(published.machine_incarnation, Some(incarnation));
        assert_eq!(published.source_instances, keys.to_vec());
        let graph = super::super::recovery::read_v2(&path, root.path())
            .unwrap()
            .unwrap()
            .graph;
        let surface = graph
            .public_surfaces()
            .find(|surface| surface.owner == owner)
            .unwrap();
        assert_eq!(surface.source_instances.len(), keys.len());
        for key in &keys {
            let source = surface
                .source_instances
                .iter()
                .find(|source| source.instance == key.instance.raw())
                .unwrap();
            assert_eq!(source.machine_incarnation, incarnation.0);
            assert_eq!(source.module_version, key.binder.version.0);
            assert_eq!(source.binder.unit, key.binder.binder.unit);
            assert_eq!(source.binder.module, key.binder.binder.module);
            assert_eq!(source.binder.namespace, key.binder.binder.namespace);
            assert_eq!(source.binder.occurrence, key.binder.binder.occurrence);
            assert_eq!(source.binder.record_parent, key.binder.binder.record_parent);
        }
        let stale = PublicationDecision::new();
        assert_eq!(
            session
                .publish_staged_public_manifest(old_stage, &stale)
                .unwrap(),
            PublicManifestCommit::Stale
        );
        assert_eq!(stale.phase(), PublicationPhase::Running);
        let bytes = std::fs::read(&path).unwrap();
        session.lib_mut().confirm_recovery_durability().unwrap();
        session.lib_mut().confirm_recovery_durability().unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), bytes);
        assert_eq!(
            session.public_visibility_snapshot_in(public).unwrap(),
            published
        );

        assert_eq!(session.retire_scope(private).roots_released, 0);
        assert_eq!(
            session
                .public_visibility_snapshot_in(public)
                .unwrap()
                .source_instances,
            keys.to_vec()
        );
        assert_eq!(session.retire_scope(public).roots_released, keys.len());
        // Retiring the leases releases their handles. Quiescence then retires
        // the unreachable native groups and their internal import roots.
        session
            .prepared_mut()
            .unwrap()
            .quiesce_and_collect_now()
            .unwrap();
        assert_eq!(session.residency().unwrap().programs, 0);
        assert_eq!(session.persistent_roots_count(), 0);
    }

    pub(in crate::session) fn certified_sibling_fixture() -> CertifiedGroup {
        use tidepool_repr::execution_schema::ModuleVersion;
        let mut wire = testing::wire_program();
        let Group::NonRecursive(mut first) = wire.bindings[0].clone() else {
            unreachable!()
        };
        first.identity = testing::identity("Fixture", "a");
        let mut sibling = first.clone();
        sibling.identity = testing::identity("Fixture", "b");
        sibling.binding.id = ValueId(1);
        wire.expressions
            .nodes
            .push(wire.expressions.nodes[0].clone());
        if let HeapRhs::Function { body, .. } = &mut sibling.binding.rhs {
            *body = 1;
        }
        wire.bindings = vec![Group::Recursive(vec![first, sibling])];
        CertifiedGroup::admit(
            CachedHomeOwner {
                unit: "fixture".into(),
                module: "Fixture".into(),
                module_version: ModuleVersion([1; 32]),
                skinny_iface_sha256: [2; 32],
                product_sha256: [3; 32],
            },
            testing::projected_group(wire, 11).unwrap(),
            vec![],
        )
        .unwrap()
    }

    #[test]
    fn issued_source_attachments_refuse_released_handles_before_consumption() {
        use tidepool_codegen::{prepared_program::GroupInventory, scope::ScopeTree};
        use tidepool_repr::execution_schema::ModuleVersion;
        let mut scopes = ScopeTree::new();
        let a = scopes.mint_isolated();
        let c = scopes.mint_isolated();
        let mut bindings = BindingTable::new();
        let registry = ImageRegistry::new();
        let group = certified_sibling_fixture();
        let groups = [group];
        let binder = |name| SourceBinder {
            version: ModuleVersion([1; 32]),
            binder: testing::identity("Fixture", name),
        };
        let prepare = |bindings: &BindingTable, scope, root: SourceBinder| {
            let selected = bindings.source_domain_selection_in(&scopes, scope).unwrap();
            let snapshot = bindings.scope_snapshot(&scopes, scope).unwrap();
            let (groups, inherited, roots) = GroupInventory::new(&groups)
                .unwrap()
                .seal_in_domains([root.clone()], &selected)
                .unwrap();
            let inherited: Vec<_> = inherited
                .into_iter()
                .map(|request| request.into_demand())
                .collect();
            let mut wire = testing::wire_program();
            let Group::NonRecursive(entry) = &mut wire.bindings[0] else {
                unreachable!()
            };
            entry.identity.unit = HOME_UNIT.into();
            wire.globals.push(GlobalDecl {
                identity: root.binder.clone(),
                rep: RuntimeRep::LiftedRef,
                entry_signature: None,
                required_evaluated: false,
                required_generation: None,
            });
            let target = CertifiedTargetImage::compile(testing::prepare(wire).unwrap(), &registry)
                .unwrap()
                .with_source_plan(super::super::persistent::ResolvedSourceDomainPlan::fixture(
                    roots,
                    selected.clone(),
                    snapshot,
                ));
            let demanded = target.compile_scoped_demanded(groups, &registry).unwrap();
            (
                target,
                vec![ImportOwner::Source {
                    version: root.version,
                    binder: root.binder,
                }],
                demanded,
                inherited,
                selected,
            )
        };
        let evidence = certified_source_evidence(&groups);
        let mut engine = PreparedEngine::empty_certified(64 * 1024, None).unwrap();
        let (target, owners, demanded, inherited, selected) = prepare(&bindings, a, binder("a"));
        let mut refused = engine
            .install_certified_turn(
                target,
                &owners,
                &evidence,
                demanded,
                &inherited,
                selected.inherited(),
                &HashMap::new(),
                &bindings,
            )
            .unwrap();
        let dead = refused.leases[0].handle();
        assert!(engine.release(dead));
        let revision = bindings.mutation_revision();
        let held = std::mem::take(&mut refused.domain_leases);
        assert!(engine
            .admit_source_instances(&mut bindings, &scopes, a, held)
            .is_err());
        assert_eq!(bindings.mutation_revision(), revision);
        assert!(bindings.source_instances_in(&scopes, a).is_empty());
        refused.leases.clear();
        engine.abort_certified_turn(refused, vec![]).unwrap();
        let install = |engine: &mut PreparedEngine, bindings: &mut BindingTable, scope, root| {
            let (target, owners, demanded, inherited, selected) = prepare(bindings, scope, root);
            let mut staged = engine
                .install_certified_turn(
                    target,
                    &owners,
                    &evidence,
                    demanded,
                    &inherited,
                    selected.inherited(),
                    &HashMap::new(),
                    bindings,
                )
                .unwrap();
            let admissions = std::mem::take(&mut staged.domain_leases);
            let delta = engine
                .admit_source_instances(bindings, &scopes, scope, admissions)
                .ok()
                .unwrap();
            staged.leases.clear();
            (engine.commit_certified_turn(staged), delta)
        };
        let (ta, _) = install(&mut engine, &mut bindings, a, binder("a"));
        bindings.seed_detached_scope(&scopes, a, c);
        let (tb, delta_b) = install(&mut engine, &mut bindings, a, binder("b"));
        let (_, _, _, late, _) = prepare(&bindings, c, binder("b"));
        let held = bindings
            .retained_source_sibling_attachment(&late[0])
            .unwrap();
        let released = bindings.rollback_source_admission(a, &delta_b).unwrap();
        for lease in released {
            assert!(engine.release(lease.handle()));
        }
        let revision = bindings.mutation_revision();
        assert!(engine
            .admit_source_instances(&mut bindings, &scopes, c, vec![held])
            .is_err());
        assert_eq!(bindings.mutation_revision(), revision);
        assert_eq!(bindings.selected_source_instances_in(&scopes, c).len(), 1);
        for target in [ta, tb] {
            assert!(engine.unpin(target));
        }
        for scope in [a, c] {
            for lease in bindings.drain_scope_with_sources(scope).source_instances {
                assert!(engine.release(lease.handle()));
            }
        }
        engine.quiesce_and_collect_now().unwrap();
        assert_eq!(engine.residency().programs, 0);
    }

    #[test]
    fn later_sibling_binder_reuses_exact_scoped_group_instance() {
        use tidepool_codegen::prepared_program::{
            GroupInventory, PendingGroupInventory, SourceGroupOutline,
        };
        use tidepool_repr::execution_schema::{
            CachedHomeOwner, CertifiedGroup, ImportOwner, ModuleVersion,
        };

        let mut group_wire = testing::wire_program();
        let Group::NonRecursive(mut first) = group_wire.bindings[0].clone() else {
            unreachable!()
        };
        first.identity = testing::identity("Fixture", "a");
        let mut sibling = first.clone();
        sibling.identity = testing::identity("Fixture", "b");
        sibling.binding.id = ValueId(1);
        group_wire
            .expressions
            .nodes
            .push(group_wire.expressions.nodes[0].clone());
        if let tidepool_repr::execution_schema::HeapRhs::Function { body, .. } =
            &mut sibling.binding.rhs
        {
            *body = 1;
        }
        group_wire.bindings = vec![Group::Recursive(vec![first, sibling])];
        let projected = testing::projected_group(group_wire, 11).unwrap();
        let group = CertifiedGroup::admit(
            CachedHomeOwner {
                unit: "fixture".into(),
                module: "Fixture".into(),
                module_version: ModuleVersion([1; 32]),
                skinny_iface_sha256: [2; 32],
                product_sha256: [3; 32],
            },
            projected.clone(),
            vec![],
        )
        .unwrap();
        let pending_outline =
            SourceGroupOutline::from_projected(group.owner().clone(), &projected, vec![]).unwrap();
        let groups = [group];
        let source_evidence = certified_source_evidence(&groups);
        let inventory = GroupInventory::new(&groups).unwrap();
        let binder = |name| SourceBinder {
            version: ModuleVersion([1; 32]),
            binder: testing::identity("Fixture", name),
        };
        let target = |name: &str, registry: &ImageRegistry| {
            let mut wire = testing::wire_program();
            wire.signatures[0].results = ResultContract::Returns(vec![RuntimeRep::LiftedRef]);
            wire.expressions.nodes[0] =
                ExprFrame::Return(vec![Atom::Ref(ValueRef::Global(GlobalId(0)))]);
            wire.globals.push(GlobalDecl {
                identity: testing::identity("Fixture", name),
                rep: RuntimeRep::LiftedRef,
                entry_signature: None,
                required_evaluated: false,
                required_generation: None,
            });
            CertifiedTargetImage::compile(testing::prepare(wire).unwrap(), registry).unwrap()
        };
        let registry = ImageRegistry::new();
        let (mut engine, bootstrap) =
            PreparedEngine::bootstrap(testing::prepare(testing::wire_program()).unwrap()).unwrap();
        let first_demand = inventory.seal([binder("a")]).unwrap();
        let mut first = engine
            .install_certified_turn(
                target("a", &registry),
                &[ImportOwner::Source {
                    version: ModuleVersion([1; 32]),
                    binder: binder("a").binder,
                }],
                &source_evidence,
                first_demand.compile(&registry).unwrap(),
                &[],
                &BTreeMap::new(),
                &HashMap::new(),
                &BindingTable::new(),
            )
            .unwrap();
        assert_eq!(first.groups.len(), 1);
        let first_target = first.target;
        let mut first_leases = std::mem::take(&mut first.leases);
        assert_eq!(first_leases.len(), 1);
        assert_eq!(engine.commit_certified_turn(first), first_target);
        assert!(engine.unpin(first_target));
        let anchor = first_leases.pop().unwrap();
        let existing = BTreeMap::from([(binder("a"), anchor.clone())]);
        let anchors = HashMap::from([((groups[0].owner().clone(), 11), anchor.clone())]);
        let mut wrong_owner = groups[0].owner().clone();
        wrong_owner.product_sha256 = [9; 32];
        let wrong_groups = [CertifiedGroup::admit(wrong_owner, projected, vec![]).unwrap()];
        assert!(matches!(
            GroupInventory::new(&wrong_groups)
                .unwrap()
                .seal_with_inherited([binder("a")], &existing, &anchors),
            Err(DemandError::InvalidInheritedInstance(_))
        ));
        let already_rooted = inventory
            .seal_with_inherited([binder("a")], &existing, &anchors)
            .unwrap();
        assert_eq!(already_rooted.groups().len(), 0);
        assert!(already_rooted.inherited_demands().is_empty());
        let sibling_demand = inventory
            .seal_with_inherited([binder("b")], &existing, &anchors)
            .unwrap();
        assert_eq!(sibling_demand.groups().len(), 0);
        assert_eq!(sibling_demand.inherited_demands().len(), 1);
        let group_misses = registry.misses();
        assert!(sibling_demand.compile(&registry).unwrap().is_empty());
        assert_eq!(registry.misses(), group_misses);
        let pending_sibling = PendingGroupInventory::new(vec![pending_outline])
            .unwrap()
            .seal_with_inherited([binder("b")], &existing, &anchors)
            .unwrap();
        assert!(pending_sibling.new_group_indices().is_empty());
        assert_eq!(pending_sibling.inherited_demands().len(), 1);
        let mut bad_wire = testing::wire_program();
        bad_wire.signatures.push(Signature {
            arguments: vec![],
            results: ResultContract::Returns(vec![RuntimeRep::LiftedRef]),
        });
        bad_wire.globals.push(GlobalDecl {
            identity: binder("b").binder,
            rep: RuntimeRep::LiftedRef,
            entry_signature: Some(SignatureId(1)),
            required_evaluated: false,
            required_generation: None,
        });
        let before_bad = engine.residency();
        let bad_result = engine.install_certified_turn(
            CertifiedTargetImage::compile(testing::prepare(bad_wire).unwrap(), &registry).unwrap(),
            &[ImportOwner::Source {
                version: ModuleVersion([1; 32]),
                binder: binder("b").binder,
            }],
            &source_evidence,
            vec![],
            pending_sibling.inherited_demands(),
            &existing
                .iter()
                .map(|(source, lease)| {
                    (
                        ScopedSourceBinder {
                            domain: SourceInstanceDomain::single(),
                            source: source.clone(),
                        },
                        lease.clone(),
                    )
                })
                .collect(),
            &HashMap::new(),
            &BindingTable::new(),
        );
        assert!(
            matches!(
                &bad_result,
                Err(PreparedRuntimeError::Install(
                    ExecutionError::BatchImportContract(evidence)
                )) if evidence.owner == Some(ImportOwner::Source {
                    version: ModuleVersion([1; 32]),
                    binder: binder("b").binder,
                })
            ),
            "unexpected result: {:?}",
            bad_result.err().map(|error| error.to_string())
        );
        assert_eq!(engine.residency(), before_bad);
        let mut second = engine
            .install_certified_turn(
                target("b", &registry),
                &[ImportOwner::Source {
                    version: ModuleVersion([1; 32]),
                    binder: binder("b").binder,
                }],
                &source_evidence,
                vec![],
                pending_sibling.inherited_demands(),
                &existing
                    .iter()
                    .map(|(source, lease)| {
                        (
                            ScopedSourceBinder {
                                domain: SourceInstanceDomain::single(),
                                source: source.clone(),
                            },
                            lease.clone(),
                        )
                    })
                    .collect(),
                &HashMap::new(),
                &BindingTable::new(),
            )
            .unwrap();
        assert!(second.groups.is_empty());
        let second_target = second.target;
        let mut second_leases = std::mem::take(&mut second.leases);
        assert_eq!(second_leases.len(), 1);
        let sibling = second_leases.pop().unwrap();
        assert_eq!(sibling.instance(), anchor.instance());
        assert_ne!(sibling.handle(), anchor.handle());
        assert_eq!(engine.commit_certified_turn(second), second_target);
        assert!(engine.unpin(second_target));
        assert!(engine.release(anchor.handle()));
        assert!(engine.release(sibling.handle()));
        engine.quiesce_and_collect_now().unwrap();
        assert_eq!(engine.residency().programs, 1);
        assert!(engine.unpin(bootstrap));
    }

    #[test]
    fn retained_code_export_owners_fence_identity_generation_and_native_root() {
        let prepared = testing::prepare(testing::wire_program()).unwrap();
        let binder = testing::identity("Fixture", "entry");
        let (mut engine, bootstrap) = PreparedEngine::bootstrap(prepared.clone()).unwrap();
        let (foreign, _) = PreparedEngine::bootstrap(prepared).unwrap();
        let owner = engine.retained_code_export_owner(&binder, 0).unwrap();
        assert_eq!(
            engine.retained_code_export_owner_installed_by(&binder, bootstrap),
            Some(owner.clone())
        );
        let foreign_owner = foreign.retained_code_export_owner(&binder, 0).unwrap();
        assert_ne!(owner, foreign_owner);
        assert!(engine.retained_code_export_owner(&binder, 1).is_none());
        let mut wrong_identity = binder.clone();
        wrong_identity.unit = "another-package".into();
        assert!(engine
            .retained_code_export_owner(&wrong_identity, 0)
            .is_none());
        let ImportOwner::CodeExport { root_id, .. } = owner.clone() else {
            panic!("native export must have a distinct owner");
        };
        let registry = ImageRegistry::new();
        let target = |identity: SymbolIdentity, generation, bad_signature| {
            let mut wire = testing::wire_program();
            let Group::NonRecursive(top) = &mut wire.bindings[0] else {
                unreachable!()
            };
            top.identity.unit = HOME_UNIT.into();
            if bad_signature {
                wire.signatures[0].results = ResultContract::Returns(vec![RuntimeRep::LiftedRef]);
            }
            wire.expressions.nodes[0] = ExprFrame::Call {
                callee: Atom::Ref(ValueRef::Global(GlobalId(0))),
                signature: SignatureId(0),
                arguments: vec![],
            };
            wire.globals.push(GlobalDecl {
                identity,
                rep: RuntimeRep::LiftedRef,
                entry_signature: Some(SignatureId(0)),
                required_evaluated: true,
                required_generation: Some(generation),
            });
            CertifiedTargetImage::compile(testing::prepare(wire).unwrap(), &registry).unwrap()
        };
        let install = |engine: &mut PreparedEngine, owner: ImportOwner, target| {
            engine.install_certified_turn(
                target,
                &[owner],
                &BTreeMap::new(),
                vec![],
                &[],
                &BTreeMap::new(),
                &HashMap::new(),
                &BindingTable::new(),
            )
        };
        let before = engine.residency();
        for (refused, image) in [
            (foreign_owner, target(binder.clone(), 0, false)),
            (
                ImportOwner::CodeExport {
                    binder: binder.clone(),
                    generation: 1,
                    root_id,
                    interface_digest: None,
                },
                target(binder.clone(), 1, false),
            ),
            (
                ImportOwner::CodeExport {
                    binder: wrong_identity.clone(),
                    generation: 0,
                    root_id,
                    interface_digest: None,
                },
                target(wrong_identity, 0, false),
            ),
        ] {
            assert!(matches!(install(&mut engine, refused.clone(), image),
                Err(PreparedRuntimeError::MissingCertifiedOwner(actual)) if actual == refused));
            assert_eq!(engine.residency(), before);
        }
        assert!(matches!(
            install(&mut engine, owner.clone(), target(binder.clone(), 0, true)),
            Err(PreparedRuntimeError::Install(
                ExecutionError::BatchImportContract(evidence)
            )) if evidence.owner == Some(owner.clone())
        ));
        assert_eq!(engine.residency(), before);
        let staged = install(&mut engine, owner, target(binder.clone(), 0, false)).unwrap();
        assert!(staged.leases.is_empty());
        let program = engine.commit_certified_turn(staged);
        let result = engine
            .machine
            .run_entry_retained(
                program,
                ValueId(0),
                &[],
                PreparedCallOptions {
                    observation_budget: 0,
                    collect_before_observation: true,
                },
                RealmId::ROOT,
            )
            .unwrap();
        assert_eq!(result.values, vec![PreparedResult::Scalar(42)]);
        assert!(engine.unpin(program));
        assert!(engine.unpin(bootstrap));
        engine.quiesce_and_collect_now().unwrap();
        let export = engine.code_exports.remove(&binder).unwrap();
        assert!(engine.release(export.handle));
        assert!(engine.retained_code_export_owner(&binder, 0).is_none());
        engine.quiesce_and_collect_now().unwrap();
        assert_eq!(engine.residency().programs, 0);
    }

    #[test]
    fn retained_package_exports_require_original_provenance_and_live_root() {
        let binder = testing::identity("Fixture", "entry");
        let (mut engine, _) = certified_package_export_fixture([9; 32]);
        let (foreign, _) = certified_package_export_fixture([9; 32]);
        let owner = engine
            .retained_package_code_export_owner(&binder, 0, &[9; 32])
            .unwrap();
        let ImportOwner::CodeExport {
            binder: owner_binder,
            generation,
            root_id,
            interface_digest,
        } = &owner
        else {
            panic!("retained package fixture must supply a code export owner")
        };
        let owner_ref = CodeExportOwnerRef {
            binder: owner_binder,
            generation: *generation,
            root_id: *root_id,
            interface_digest: interface_digest.as_ref(),
        };
        let declaration = GlobalDecl {
            identity: binder.clone(),
            rep: RuntimeRep::LiftedRef,
            entry_signature: Some(SignatureId(0)),
            required_evaluated: true,
            required_generation: Some(0),
        };
        assert!(engine.code_export_import(owner_ref, &declaration).is_ok());
        for (identity, generation, digest) in [
            (binder.clone(), 0, [0; 32]),
            (binder.clone(), 0, [8; 32]),
            (binder.clone(), 1, [9; 32]),
            (testing::identity("Absent", "entry"), 0, [9; 32]),
        ] {
            assert!(engine
                .retained_package_code_export_owner(&identity, generation, &digest)
                .is_none());
        }
        let (legacy, _) =
            PreparedEngine::bootstrap(testing::prepare(testing::wire_program()).unwrap()).unwrap();
        assert!(legacy
            .retained_package_code_export_owner(&binder, 0, &[9; 32])
            .is_none());
        assert_eq!(legacy.code_exports[&binder].interface_digest, None);
        assert!(legacy
            .code_export_retentions()
            .any(|row| row == (binder.clone(), 0)));
        assert!(legacy.protected_code_export_retentions().next().is_none());
        assert_eq!(
            engine
                .protected_code_export_retentions()
                .collect::<Vec<_>>(),
            vec![(binder.clone(), 0)]
        );
        let foreign_owner = foreign
            .retained_package_code_export_owner(&binder, 0, &[9; 32])
            .unwrap();
        let ImportOwner::CodeExport {
            binder: foreign_binder,
            generation,
            root_id,
            interface_digest,
        } = &foreign_owner
        else {
            panic!("foreign package fixture must supply a code export owner")
        };
        let foreign_owner_ref = CodeExportOwnerRef {
            binder: foreign_binder,
            generation: *generation,
            root_id: *root_id,
            interface_digest: interface_digest.as_ref(),
        };
        assert!(
            matches!(engine.code_export_import(foreign_owner_ref, &declaration),
            Err(PreparedRuntimeError::MissingCertifiedOwner(actual)) if actual == foreign_owner)
        );
        for changed in [None, Some([8; 32])] {
            engine
                .code_exports
                .get_mut(&binder)
                .unwrap()
                .interface_digest = changed;
            assert!(matches!(engine.code_export_import(owner_ref, &declaration),
                Err(PreparedRuntimeError::MissingCertifiedOwner(actual)) if actual == owner));
        }
        engine
            .code_exports
            .get_mut(&binder)
            .unwrap()
            .interface_digest = Some([9; 32]);
        let mut wrong_rep = declaration.clone();
        wrong_rep.rep = RuntimeRep::Address;
        assert!(matches!(engine.code_export_import(owner_ref, &wrong_rep),
            Err(PreparedRuntimeError::MissingCertifiedOwner(actual)) if actual == owner));
        let export = engine.code_exports.remove(&binder).unwrap();
        assert!(engine
            .retained_package_code_export_owner(&binder, 0, &[9; 32])
            .is_none());
        assert!(matches!(engine.code_export_import(owner_ref, &declaration),
            Err(PreparedRuntimeError::MissingCertifiedOwner(actual)) if actual == owner));
        assert!(engine.release(export.handle));
    }

    proptest::proptest! {
        #![proptest_config({
            let mut config = proptest::test_runner::Config::default();
            if let Some(path) = option_env!("TIDEPOOL_PROPTEST_REGRESSIONS") {
                config.failure_persistence = Some(Box::new(proptest::test_runner::FileFailurePersistence::Direct(path)));
            }
            config
        })]

        #[test]
        fn protected_export_advertisement_tracks_provenance_and_root_lifetime(
            history in proptest::collection::vec(0_u8..5, 0..32)
        ) {
            let binder = testing::identity("Fixture", "entry");
            let (mut engine, _) = certified_package_export_fixture([9; 32]);
            let handle = engine.code_exports[&binder].handle;
            let mut alive = true;
            // Always cover missing, zero, restored and released evidence;
            // generated prefixes exercise repeated transitions and stale roots.
            for operation in history.into_iter().chain([0, 1, 2, 4, 2]) {
                let digest = match operation {
                    0 => None,
                    1 => Some([0; 32]),
                    2 => Some([9; 32]),
                    3 => Some([8; 32]),
                    _ => {
                        if alive {
                            proptest::prop_assert!(engine.release(handle));
                            alive = false;
                        }
                        engine.code_exports[&binder].interface_digest
                    }
                };
                engine.code_exports.get_mut(&binder).unwrap().interface_digest = digest;
                let expected = if alive && digest.is_some_and(|value| value != [0; 32]) {
                    vec![(binder.clone(), 0)]
                } else {
                    vec![]
                };
                proptest::prop_assert_eq!(engine.protected_code_export_retentions().collect::<Vec<_>>(), expected);
                for requested in [[0; 32], [8; 32], [9; 32]] {
                    proptest::prop_assert_eq!(
                        engine.retained_package_code_export_owner(&binder, 0, &requested).is_some(),
                        alive && requested != [0; 32] && digest == Some(requested)
                    );
                }
            }
        }
    }

    #[test]
    fn recovered_independent_group_runs_without_unused_retained_package_lease() {
        use tidepool_codegen::prepared_program::GroupInventory;
        use tidepool_repr::execution_schema::{CertifiedGroup, ModuleVersion};
        let package = testing::identity("Fixture", "entry");
        let (historical, _) = certified_package_export_fixture([9; 32]);
        let former_owner = historical
            .retained_package_code_export_owner(&package, 0, &[9; 32])
            .unwrap();
        let owner = CachedHomeOwner {
            unit: HOME_UNIT.into(),
            module: "Original".into(),
            module_version: ModuleVersion([1; 32]),
            skinny_iface_sha256: [2; 32],
            product_sha256: [3; 32],
        };
        let symbol = |name: &str| SymbolIdentity {
            unit: owner.unit.clone(),
            module: owner.module.clone(),
            ..testing::identity("Original", name)
        };
        let independent = symbol("independent");
        let dependent = symbol("dependent");
        let group = |identity: SymbolIdentity, ordinal, requires_package: bool| {
            let mut wire = testing::wire_program();
            let Group::NonRecursive(top) = &mut wire.bindings[0] else {
                unreachable!()
            };
            top.identity = identity;
            let imports = if requires_package {
                wire.expressions.nodes[0] = ExprFrame::Call {
                    callee: Atom::Ref(ValueRef::Global(GlobalId(0))),
                    signature: SignatureId(0),
                    arguments: vec![],
                };
                wire.globals.push(GlobalDecl {
                    identity: package.clone(),
                    rep: RuntimeRep::LiftedRef,
                    entry_signature: Some(SignatureId(0)),
                    required_evaluated: true,
                    required_generation: Some(0),
                });
                vec![former_owner.clone()]
            } else {
                vec![]
            };
            CertifiedGroup::admit(
                owner.clone(),
                testing::projected_group(wire, ordinal).unwrap(),
                imports,
            )
            .unwrap()
        };
        // Recovered originals retain both native groups and the dependent
        // group's package obligation, independently of selected execution.
        let groups = [
            group(independent.clone(), 0, false),
            group(dependent.clone(), 1, true),
        ];
        drop(historical);
        let registry = ImageRegistry::new();
        let evidence = certified_source_evidence(&groups);
        let mut engine = PreparedEngine::empty_certified(64 * 1024, None).unwrap();
        let stage = |engine: &mut PreparedEngine, binder: &SymbolIdentity| {
            let mut wire = testing::wire_program();
            let Group::NonRecursive(top) = &mut wire.bindings[0] else {
                unreachable!()
            };
            top.identity.unit = HOME_UNIT.into();
            top.identity.module = "Consumer".into();
            wire.expressions.nodes[0] = ExprFrame::Call {
                callee: Atom::Ref(ValueRef::Global(GlobalId(0))),
                signature: SignatureId(0),
                arguments: vec![],
            };
            wire.globals.push(GlobalDecl {
                identity: binder.clone(),
                rep: RuntimeRep::LiftedRef,
                entry_signature: Some(SignatureId(0)),
                required_evaluated: true,
                required_generation: None,
            });
            let demand = GroupInventory::new(&groups)
                .unwrap()
                .seal([SourceBinder {
                    version: owner.module_version.clone(),
                    binder: binder.clone(),
                }])
                .unwrap()
                .compile(&registry)
                .unwrap();
            engine.install_certified_turn(
                CertifiedTargetImage::compile(testing::prepare(wire).unwrap(), &registry).unwrap(),
                &[ImportOwner::Source {
                    version: owner.module_version.clone(),
                    binder: binder.clone(),
                }],
                &evidence,
                demand,
                &[],
                &BTreeMap::new(),
                &HashMap::new(),
                &BindingTable::new(),
            )
        };
        let mut installed = stage(&mut engine, &independent).unwrap();
        let leases = std::mem::take(&mut installed.leases);
        let program = engine.commit_certified_turn(installed);
        let result = engine
            .machine
            .run_entry_retained(
                program,
                ValueId(0),
                &[],
                PreparedCallOptions {
                    observation_budget: 0,
                    collect_before_observation: true,
                },
                RealmId::ROOT,
            )
            .unwrap();
        assert_eq!(result.values, vec![PreparedResult::Scalar(42)]);
        let before = engine.residency();
        assert!(matches!(stage(&mut engine, &dependent),
            Err(PreparedRuntimeError::MissingCertifiedOwner(actual)) if actual == former_owner));
        assert_eq!(engine.residency(), before);
        for lease in leases {
            assert!(engine.release(lease.handle()));
        }
        assert!(engine.unpin(program));
    }

    #[test]
    fn export_staging_rolls_back_prior_roots_on_required_absence() {
        let prepared = testing::prepare(testing::wire_program()).unwrap();
        let image = Arc::new(CompiledProgram::compile_prepared_definitions(&prepared).unwrap());
        let mut engine = PreparedEngine::empty_certified(64 * 1024, None).unwrap();
        let program = engine
            .machine
            .install_shared_batch(vec![BatchProgram {
                image,
                imports: vec![],
            }])
            .unwrap()[0];
        engine.machine.pin(program).unwrap();
        let before = engine.residency();
        let optional = testing::identity("Fixture", "entry");
        let required = testing::identity("Fixture", "missing");
        let error = engine
            .stage_code_exports(
                program,
                vec![
                    (optional, ValueId(0), Some(prepared.signatures()[0].clone())),
                    (required.clone(), ValueId(999), None),
                ],
                &BTreeMap::from([(required.clone(), (ValueId(999), [9; 32]))]),
                &BTreeMap::new(),
            )
            .err()
            .expect("required missing export must refuse the stage");
        assert!(matches!(
            error,
            PreparedRuntimeError::Install(ExecutionError::MissingEntry(ValueId(999)))
        ));
        assert_eq!(engine.residency(), before);
        assert!(engine.programs.is_empty());
        assert!(engine.code_exports.is_empty());
        assert_eq!(engine.disposition(), MachineDisposition::Reusable);
        let absent = engine
            .stage_code_exports(
                program,
                vec![(required, ValueId(999), None)],
                &BTreeMap::new(),
                &BTreeMap::new(),
            )
            .unwrap();
        assert!(absent.is_empty());
        assert_eq!(engine.residency(), before);
        assert!(engine.unpin(program));
        engine.quiesce_and_collect_now().unwrap();
        assert_eq!(engine.residency().programs, 0);
    }

    #[test]
    fn cold_certified_package_forward_edge_executes_and_rolls_back() {
        use tidepool_codegen::prepared_program::GroupInventory;
        use tidepool_repr::execution_schema::{CertifiedGroup, ModuleVersion};

        let package = SymbolIdentity {
            unit: "fixture-package".into(),
            module: "FixturePackage".into(),
            ..testing::identity("FixturePackage", "packageValue")
        };
        let optional = SymbolIdentity {
            unit: package.unit.clone(),
            ..testing::identity("FixturePackage", "optionalValue")
        };
        let incidental = SymbolIdentity {
            unit: "unselected-package".into(),
            ..testing::identity("UnselectedPackage", "incidentalValue")
        };
        let source = testing::identity("Fixture", "cached");
        let package_owner = |digest| ImportOwner::Package {
            unit: package.unit.clone(),
            module: package.module.clone(),
            binder: package.clone(),
            interface_digest: digest,
        };
        let source_owner = ImportOwner::Source {
            version: ModuleVersion([1; 32]),
            binder: source.clone(),
        };
        let group = |bad_signature: bool, digest| {
            let mut wire = testing::wire_program();
            let Group::NonRecursive(top) = &mut wire.bindings[0] else {
                unreachable!()
            };
            top.identity = source.clone();
            if bad_signature {
                wire.signatures.push(Signature {
                    arguments: vec![],
                    results: ResultContract::Returns(vec![RuntimeRep::LiftedRef]),
                });
            } else {
                wire.expressions.nodes[0] = ExprFrame::Call {
                    callee: Atom::Ref(ValueRef::Global(GlobalId(0))),
                    signature: SignatureId(0),
                    arguments: vec![],
                };
            }
            wire.globals.push(GlobalDecl {
                identity: package.clone(),
                rep: RuntimeRep::LiftedRef,
                entry_signature: Some(SignatureId(u32::from(bad_signature))),
                required_evaluated: false,
                required_generation: None,
            });
            wire.globals.push(GlobalDecl {
                identity: source.clone(),
                rep: RuntimeRep::LiftedRef,
                entry_signature: None,
                required_evaluated: false,
                required_generation: None,
            });
            CertifiedGroup::admit(
                CachedHomeOwner {
                    unit: "fixture".into(),
                    module: "Fixture".into(),
                    module_version: ModuleVersion([1; 32]),
                    skinny_iface_sha256: [2; 32],
                    product_sha256: [3; 32],
                },
                testing::projected_group(wire, 2).unwrap(),
                vec![package_owner(digest), source_owner.clone()],
            )
            .unwrap()
        };
        let registry = ImageRegistry::new();
        let target = |include_package: bool| {
            let mut wire = testing::wire_program();
            let Group::NonRecursive(mut package_top) = wire.bindings[0].clone() else {
                unreachable!()
            };
            package_top.identity = package.clone();
            package_top.binding.id = ValueId(1);
            let mut optional_top = package_top.clone();
            optional_top.identity = optional.clone();
            optional_top.binding.id = ValueId(u32::from(include_package) + 1);
            let (
                HeapRhs::Function {
                    body: package_body, ..
                },
                HeapRhs::Function {
                    body: optional_body,
                    ..
                },
            ) = (&mut package_top.binding.rhs, &mut optional_top.binding.rhs)
            else {
                unreachable!()
            };
            if include_package {
                *package_body = 1;
                *optional_body = 2;
                wire.expressions
                    .nodes
                    .push(wire.expressions.nodes[0].clone());
                wire.expressions
                    .nodes
                    .push(wire.expressions.nodes[1].clone());
                wire.bindings.push(Group::NonRecursive(package_top));
            } else {
                *optional_body = 1;
                wire.expressions
                    .nodes
                    .push(wire.expressions.nodes[0].clone());
            }
            let mut incidental_top = optional_top.clone();
            incidental_top.identity = incidental.clone();
            incidental_top.binding.id = ValueId(u32::from(include_package) + 2);
            wire.bindings.push(Group::NonRecursive(optional_top));
            wire.bindings.push(Group::NonRecursive(incidental_top));
            let Group::NonRecursive(top) = &mut wire.bindings[0] else {
                unreachable!()
            };
            top.identity.unit = HOME_UNIT.into();
            wire.expressions.nodes[0] = ExprFrame::Call {
                callee: Atom::Ref(ValueRef::Global(GlobalId(0))),
                signature: SignatureId(0),
                arguments: vec![],
            };
            wire.globals.push(GlobalDecl {
                identity: source.clone(),
                rep: RuntimeRep::LiftedRef,
                entry_signature: Some(SignatureId(0)),
                required_evaluated: false,
                required_generation: None,
            });
            CertifiedTargetImage::compile(testing::prepare(wire).unwrap(), &registry).unwrap()
        };
        let selected = |group: &CertifiedGroup| {
            GroupInventory::new(std::slice::from_ref(group))
                .unwrap()
                .seal([SourceBinder {
                    version: ModuleVersion([1; 32]),
                    binder: source.clone(),
                }])
                .unwrap()
                .compile(&registry)
                .unwrap()
        };
        let mut engine = PreparedEngine::empty_certified(64 * 1024, None).unwrap();
        let before = engine.residency();
        let good = group(false, [9; 32]);
        let evidence = certified_source_evidence(std::slice::from_ref(&good));
        // An unsigned target cannot authorize a package body merely because
        // it has the same identity. Producer-sealed admission is tested by the
        // real compiler lane; these fixtures exercise its native transaction.
        assert!(matches!(engine.install_certified_turn(
            target(true), std::slice::from_ref(&source_owner), &evidence, selected(&good), &[],
            &BTreeMap::new(), &HashMap::new(), &BindingTable::new(),
        ), Err(PreparedRuntimeError::CertifiedPackageOwnerUnavailable {
            owner,
            evidence: CertifiedPackageOwnerEvidence::TargetDefinition {
                present: true,
                interfaces_match: false,
                interface_digest: None,
                diagnostic: Some(diagnostic),
            },
        }) if owner == package_owner([9; 32])
            && diagnostic.target_global_count == 1
            && diagnostic.target_globals[0].identity == source
            && diagnostic.target_globals[0].owner == Some(source_owner.clone())
            && diagnostic.target_top.kind == PackageTargetTopKind::Function
            && diagnostic.target_top.exportability == PackageTargetTopExportability::Exportable
            && diagnostic.demanded_group_count == 1
            && diagnostic.demanded_groups[0].owner.module == "Fixture"
            && diagnostic.demanded_groups[0].original_ordinal == 2
            && diagnostic.demanded_groups[0].imports.len() == 2
            && diagnostic.demanded_groups[0].imports[0].position == 0
            && diagnostic.demanded_groups[0].imports[0].owner == package_owner([9; 32])
            && diagnostic.demanded_groups[0].imports[1].position == 1
            && diagnostic.demanded_groups[0].imports[1].owner == source_owner));
        assert_eq!(engine.residency(), before);
        assert!(matches!(engine.install_certified_turn(
            target(false), std::slice::from_ref(&source_owner), &evidence, selected(&good), &[],
            &BTreeMap::new(), &HashMap::new(), &BindingTable::new(),
        ), Err(PreparedRuntimeError::CertifiedPackageOwnerUnavailable {
            owner,
            evidence: CertifiedPackageOwnerEvidence::TargetDefinition {
                present: false,
                interfaces_match: false,
                interface_digest: None,
                diagnostic: Some(diagnostic),
            },
        }) if owner == package_owner([9; 32])
            && diagnostic.target_top.kind == PackageTargetTopKind::Absent
            && diagnostic.target_top.exportability == PackageTargetTopExportability::Absent
            && diagnostic.retained_export == PackageRetainedExportFact::Missing));
        assert_eq!(engine.residency(), before);
        let capped_target = target(false);
        let capped_target_exports = exportable_code_tops(&capped_target.prepared)
            .into_iter()
            .map(|(identity, value, _)| (identity, value))
            .collect();
        let capped_demanded = selected(&good);
        let capped_owner = package_owner([9; 32]);
        let ImportOwner::Package {
            unit,
            module,
            binder,
            interface_digest,
        } = &capped_owner
        else {
            panic!("package diagnostic fixture must supply a package owner")
        };
        let capped_diagnostic = certified_package_owner_diagnostic(
            &capped_target,
            std::slice::from_ref(&source_owner),
            &capped_demanded,
            PackageOwnerRef {
                unit,
                module,
                binder,
                interface_digest,
            },
            &capped_target_exports,
            &engine.code_exports,
            false,
            None,
            PackageOwnerDiagnosticLimits {
                target_globals: 0,
                groups: 1,
                imports: 1,
            },
        );
        assert!(capped_diagnostic.target_globals.is_empty());
        assert_eq!(capped_diagnostic.target_globals_omitted, 1);
        assert_eq!(capped_diagnostic.demanded_groups.len(), 1);
        assert_eq!(capped_diagnostic.demanded_groups_omitted, 0);
        assert_eq!(capped_diagnostic.demanded_groups[0].import_count, 2);
        assert_eq!(capped_diagnostic.demanded_groups[0].imports_omitted, 1);
        assert_eq!(capped_diagnostic.demanded_groups[0].imports.len(), 1);
        assert_eq!(
            capped_diagnostic.demanded_groups[0].imports[0].owner,
            package_owner([9; 32]),
            "the exact failed import must survive the cap before source-edge context"
        );
        let admitted = || BTreeMap::from([(package.clone(), (ValueId(1), [9; 32]))]);
        let install = |engine: &mut PreparedEngine, group: &CertifiedGroup| {
            engine.install_certified_turn_admitted(
                target(true),
                std::slice::from_ref(&source_owner),
                &evidence,
                selected(group),
                &[],
                &BTreeMap::new(),
                &HashMap::new(),
                &BindingTable::new(),
                admitted(),
                BTreeMap::from([(optional.clone(), [9; 32])]),
            )
        };
        let bad = group(true, [9; 32]);
        assert!(matches!(
            install(&mut engine, &bad),
            Err(PreparedRuntimeError::Install(
                ExecutionError::BatchImportContract(evidence)
            )) if evidence.owner == Some(package_owner([9; 32]))
        ));
        assert_eq!(engine.residency(), before);
        assert!(engine.code_exports.is_empty());
        assert!(engine.programs.is_empty());
        let wrong_digest = group(false, [8; 32]);
        assert!(matches!(install(&mut engine, &wrong_digest),
            Err(PreparedRuntimeError::CertifiedPackageOwnerUnavailable { owner, .. })
                if owner == package_owner([8; 32])));
        assert_eq!(engine.residency(), before);
        let mut aborted = install(&mut engine, &good).unwrap();
        assert_eq!(aborted.exports.len(), 2);
        assert!(!aborted.exports.contains_key(&incidental));
        assert_eq!(aborted.exports[&optional].interface_digest, Some([9; 32]));
        assert_eq!(aborted.exports[&package].interface_digest, Some([9; 32]));
        assert_eq!(engine.residency().code_exports, before.code_exports + 2);
        assert!(engine.code_exports.is_empty());
        let tokens = std::mem::take(&mut aborted.leases);
        engine.abort_certified_turn(aborted, tokens).unwrap();
        // First native installation interns the boxed-array, mutable-variable
        // and byte-array descriptors for the machine's lifetime. Program
        // retirement releases callable descriptors, not these shared layouts.
        let initialized = tidepool_codegen::prepared_program::ResidencyCounts {
            descriptor_rows: 3,
            ..before
        };
        assert_eq!(engine.residency(), initialized);
        assert!(engine.code_exports.is_empty());
        assert!(engine.programs.is_empty());
        let mut repeated_abort = install(&mut engine, &good).unwrap();
        let tokens = std::mem::take(&mut repeated_abort.leases);
        engine.abort_certified_turn(repeated_abort, tokens).unwrap();
        assert_eq!(engine.residency(), initialized);
        let mut staged = install(&mut engine, &good).unwrap();
        let staged_roots = engine.persistent_roots_count();
        let staged_residency = engine.residency();
        let tokens = std::mem::take(&mut staged.leases);
        let program = engine.commit_certified_turn(staged);
        assert!(!engine.code_exports.contains_key(&incidental));
        assert!(!engine
            .protected_code_export_retentions()
            .any(|(identity, _)| identity == incidental));
        assert_eq!(
            engine.residency().code_exports,
            staged_residency.code_exports
        );
        assert_eq!(engine.persistent_roots_count(), staged_roots);
        assert_eq!(
            engine.code_exports[&optional].interface_digest,
            Some([9; 32])
        );
        assert_eq!(
            engine.code_exports[&package].interface_digest,
            Some([9; 32])
        );
        let result = engine
            .machine
            .run_entry_retained(
                program,
                ValueId(0),
                &[],
                PreparedCallOptions {
                    observation_budget: 0,
                    collect_before_observation: false,
                },
                RealmId::ROOT,
            )
            .unwrap();
        assert!(matches!(
            result.values.as_slice(),
            [PreparedResult::Scalar(42)]
        ));
        let stale = group(false, [8; 32]);
        assert!(matches!(
            install(&mut engine, &stale),
            Err(PreparedRuntimeError::CertifiedPackageOwnerUnavailable {
                owner,
                evidence: CertifiedPackageOwnerEvidence::RetainedExport {
                    interface_digest,
                },
            })
                if owner == package_owner([8; 32]) && interface_digest == Some([9; 32])
        ));
        assert_eq!(
            engine.code_exports[&package].interface_digest,
            Some([9; 32])
        );
        for token in tokens {
            assert!(engine.release(token.handle()));
        }
        assert!(engine.unpin(program));
        engine.quiesce_and_collect_now().unwrap();
        assert_eq!(engine.code_export_count(), 2);
    }

    #[test]
    fn certified_package_import_requires_exact_interface_owner() {
        use tidepool_codegen::prepared_program::GroupInventory;
        use tidepool_repr::execution_schema::{
            CachedHomeOwner, CertifiedGroup, ImportOwner, ModuleVersion,
        };
        let mut wire = testing::wire_program();
        let package = testing::identity("Package", "value");
        wire.globals.push(GlobalDecl {
            identity: package.clone(),
            rep: RuntimeRep::LiftedRef,
            entry_signature: None,
            required_evaluated: false,
            required_generation: None,
        });
        let owner = ImportOwner::Package {
            unit: package.unit.clone(),
            module: package.module.clone(),
            binder: package,
            interface_digest: [9; 32],
        };
        let group = CertifiedGroup::admit(
            CachedHomeOwner {
                unit: "fixture".into(),
                module: "Fixture".into(),
                module_version: ModuleVersion([1; 32]),
                skinny_iface_sha256: [2; 32],
                product_sha256: [3; 32],
            },
            testing::projected_group(wire, 2).unwrap(),
            vec![owner.clone()],
        )
        .unwrap();
        let groups = [group];
        let demand = GroupInventory::new(&groups)
            .unwrap()
            .seal([SourceBinder {
                version: ModuleVersion([1; 32]),
                binder: testing::identity("Fixture", "entry"),
            }])
            .unwrap();
        let registry = ImageRegistry::new();
        let (mut engine, _) =
            PreparedEngine::bootstrap(testing::prepare(testing::wire_program()).unwrap()).unwrap();
        let before = engine.residency();
        assert!(matches!(
            engine.install_certified_demand(demand.compile(&registry).unwrap(), &HashMap::new(), &BindingTable::new()),
            Err(PreparedRuntimeError::MissingCertifiedOwner(missing)) if missing == owner
        ));
        assert_eq!(engine.residency(), before);
    }

    #[test]
    fn protected_package_import_rejects_unproven_legacy_export() {
        use tidepool_codegen::prepared_program::GroupInventory;
        use tidepool_repr::execution_schema::{
            CachedHomeOwner, CertifiedGroup, ImportOwner, ModuleVersion,
        };
        let package = testing::identity("Fixture", "entry");
        let owner = ImportOwner::Package {
            unit: package.unit.clone(),
            module: package.module.clone(),
            binder: package.clone(),
            interface_digest: [9; 32],
        };
        let group = |mismatched_signature: bool, digest| {
            let mut wire = testing::wire_program();
            if let Group::NonRecursive(top) = &mut wire.bindings[0] {
                top.identity = testing::identity("Fixture", "cached");
            }
            if mismatched_signature {
                wire.signatures.push(Signature {
                    arguments: vec![],
                    results: ResultContract::Returns(vec![RuntimeRep::LiftedRef]),
                });
            }
            wire.globals.push(GlobalDecl {
                identity: package.clone(),
                rep: RuntimeRep::LiftedRef,
                entry_signature: mismatched_signature.then_some(SignatureId(1)),
                required_evaluated: false,
                required_generation: None,
            });
            CertifiedGroup::admit(
                CachedHomeOwner {
                    unit: "fixture".into(),
                    module: "Fixture".into(),
                    module_version: ModuleVersion([1; 32]),
                    skinny_iface_sha256: [2; 32],
                    product_sha256: [3; 32],
                },
                testing::projected_group(wire, 2).unwrap(),
                vec![ImportOwner::Package {
                    unit: package.unit.clone(),
                    module: package.module.clone(),
                    binder: package.clone(),
                    interface_digest: digest,
                }],
            )
            .unwrap()
        };
        let (mut engine, _) =
            PreparedEngine::bootstrap(testing::prepare(testing::wire_program()).unwrap()).unwrap();
        let handle = engine.code_exports[&package].handle;
        let exact = HashMap::from([(owner.clone(), handle)]);
        fn selected(groups: &[CertifiedGroup]) -> Vec<DemandedImage> {
            GroupInventory::new(groups)
                .unwrap()
                .seal([SourceBinder {
                    version: ModuleVersion([1; 32]),
                    binder: testing::identity("Fixture", "cached"),
                }])
                .unwrap()
                .compile(&ImageRegistry::new())
                .unwrap()
        }
        let bad = [group(true, [9; 32])];
        assert!(engine
            .install_certified_demand(selected(&bad), &exact, &BindingTable::new())
            .is_err_and(|error| matches!(
                error,
                PreparedRuntimeError::MissingCertifiedOwner(missing) if missing == owner
            )));
        assert_eq!(engine.code_exports[&package].interface_digest, None);
        let good = [group(false, [9; 32])];
        let error = engine
            .install_certified_demand(selected(&good), &exact, &BindingTable::new())
            .unwrap_err();
        assert!(matches!(
            error,
            PreparedRuntimeError::MissingCertifiedOwner(missing) if missing == owner
        ));
        assert_eq!(engine.code_exports[&package].interface_digest, None);
        let zero_owner = ImportOwner::Package {
            unit: package.unit.clone(),
            module: package.module.clone(),
            binder: package.clone(),
            interface_digest: [0; 32],
        };
        let zero = [group(false, [0; 32])];
        assert!(matches!(
            engine.install_certified_demand(
                selected(&zero),
                &HashMap::from([(zero_owner.clone(), handle)]),
                &BindingTable::new(),
            ),
            Err(PreparedRuntimeError::MissingCertifiedOwner(missing)) if missing == zero_owner
        ));
        assert_eq!(engine.code_exports[&package].interface_digest, None);
        let wrong = ImportOwner::Package {
            unit: package.unit.clone(),
            module: package.module.clone(),
            binder: package.clone(),
            interface_digest: [8; 32],
        };
        let wrong_exact = HashMap::from([(wrong.clone(), handle)]);
        // The native image is identical; only the newly claimed package owner
        // differs. The already attached provenance must refuse it.
        let mut wire = testing::wire_program();
        if let Group::NonRecursive(top) = &mut wire.bindings[0] {
            top.identity = testing::identity("Fixture", "cached");
        }
        wire.globals.push(GlobalDecl {
            identity: package.clone(),
            rep: RuntimeRep::LiftedRef,
            entry_signature: None,
            required_evaluated: false,
            required_generation: None,
        });
        let mismatched = CertifiedGroup::admit(
            good[0].owner().clone(),
            testing::projected_group(wire, 2).unwrap(),
            vec![wrong.clone()],
        )
        .unwrap();
        let wrong_groups = [mismatched];
        assert!(matches!(
            engine.install_certified_demand(selected(&wrong_groups), &wrong_exact, &BindingTable::new()),
            Err(PreparedRuntimeError::MissingCertifiedOwner(rejected)) if rejected == wrong
        ));
        assert_eq!(engine.code_exports[&package].interface_digest, None);
    }

    #[test]
    fn integrity_failure_is_typed_independently_from_its_cause() {
        let failure = MachineFailure {
            cause: RuntimeError::Cancelled,
            disposition: MachineDisposition::Unavailable,
        };
        let error = PreparedRuntimeError::Run(ExecutionError::Runtime(failure.clone()));
        assert_eq!(error.kind(), PreparedFailureKind::Integrity);
        assert!(matches!(
            error,
            PreparedRuntimeError::Run(ExecutionError::Runtime(retained)) if retained == failure
        ));
    }

    #[test]
    fn compiled_language_and_cancellation_are_not_integrity_failures() {
        for (cause, expected) in [
            (RuntimeError::Cancelled, PreparedFailureKind::Cancelled),
            (RuntimeError::HeapOverflow, PreparedFailureKind::Language),
            (RuntimeError::DivisionByZero, PreparedFailureKind::Language),
        ] {
            let error = PreparedRuntimeError::Run(ExecutionError::Runtime(MachineFailure {
                cause,
                disposition: MachineDisposition::Reusable,
            }));
            assert_eq!(error.kind(), expected);
        }
    }

    // ---- S4/G2: `link_program`'s identity/generation-linked import
    // contract and `PreparedMachine::install_program` verification.
    //
    // `PreparedEngine` only ever runs a turn's settled
    // scaffold (`run_settled`); it exposes no general-purpose entry call, so
    // it cannot force an arbitrary CAF the way these tests need to flip a
    // binding from unevaluated to evaluated. What is under test here is
    // `link_program`'s contract check itself (stale generation, missing
    // import, `required_evaluated` against live handle state) plus
    // `PreparedMachine::install_program`'s import verification --
    // exactly the mechanism `PreparedEngine::install` owns one layer up (and
    // which `tidepool/runtime/tests/prepared_turn.rs`'s `notebook_turns`
    // exercises end to end on the happy path). Driving `PreparedMachine`
    // directly here isolates the contract from turn orchestration.
    //
    fn producer_identity() -> SymbolIdentity {
        testing::identity("S4Session", "producer")
    }

    /// A memoized CAF returning `Field(99)`: unevaluated until first run,
    /// then an updated indirection to an evaluated constructor.
    fn producer_program() -> PreparedProgram {
        let mut wire = testing::wire_program();
        wire.signatures[0].results = ResultContract::Returns(vec![RuntimeRep::LiftedRef]);
        wire.constructors.push(ConstructorDecl {
            identity: testing::identity("S4Session", "Field"),
            family: testing::identity("S4Session", "Field"),
            host_id: tidepool_repr::DataConId(980),
            result_rep: RuntimeRep::LiftedRef,
            tag: 1,
            family_size: 1,
            field_reps: vec![RuntimeRep::Int(64)],
            strict_fields: vec![true],
            layout: CheckedLayout {
                fields: vec![FieldLayout {
                    rep: RuntimeRep::Int(64),
                    offset: 0,
                }],
                alignment: 8,
                payload_size: 8,
                root_mask: vec![false],
            },
        });
        wire.expressions.nodes[0] = ExprFrame::Construct {
            constructor: ConstructorId(0),
            fields: vec![Atom::Scalar(ScalarLiteral::Int {
                bits: 64,
                bytes: 99_i64.to_be_bytes().to_vec(),
            })],
        };
        let Group::NonRecursive(top) = &mut wire.bindings[0] else {
            unreachable!()
        };
        top.identity = producer_identity();
        top.binding.rhs = HeapRhs::Thunk {
            signature: SignatureId(0),
            update: UpdatePolicy::Memoize,
            captures: vec![],
            body: 0,
        };
        testing::prepare(wire).expect("producer fixture")
    }

    pub(in crate::session) fn certified_package_export_fixture(
        digest: [u8; 32],
    ) -> (PreparedEngine, ProgramId) {
        let registry = ImageRegistry::new();
        let prepared = testing::prepare(testing::wire_program()).unwrap();
        let binder = testing::identity("Fixture", "entry");
        let target = CertifiedTargetImage::compile(prepared, &registry).unwrap();
        let mut engine = PreparedEngine::empty_certified(64 * 1024, None).unwrap();
        // Native transaction fixture: the real producer lane supplies this
        // same private sealed package map, never caller-requested provenance.
        let staged = engine
            .install_certified_turn_admitted(
                target,
                &[],
                &BTreeMap::new(),
                vec![],
                &[],
                &BTreeMap::new(),
                &HashMap::new(),
                &BindingTable::new(),
                BTreeMap::new(),
                BTreeMap::from([(binder, digest)]),
            )
            .unwrap();
        let program = engine.commit_certified_turn(staged);
        (engine, program)
    }

    pub(in crate::session) fn rooted_publication_fixture(
        state: &mut super::super::PersistentSession,
        name: &str,
        generation: u64,
    ) -> BindingEntry {
        rooted_program_fixture(state, name, generation, producer_program())
    }

    pub(in crate::session) fn evaluated_publication_fixture(
        state: &mut super::super::PersistentSession,
        name: &str,
        generation: u64,
    ) -> BindingEntry {
        let mut binding = rooted_publication_fixture(state, name, generation);
        let engine = state.prepared_mut().unwrap();
        let program = engine
            .machine
            .owner_of_handle(binding.value.handle)
            .unwrap();
        let result = engine
            .machine
            .run_entry_retained(program, ValueId(0), &[], SETTLE_CALL, RealmId::ROOT)
            .unwrap();
        let [PreparedResult::Managed(handle)] = result.values.as_slice() else {
            panic!("computed managed result")
        };
        let handle = *handle;
        assert!(engine.release(binding.value.handle));
        assert!(engine.adopt(handle));
        binding.value = BoundValue {
            handle,
            identity: binding.value.identity,
        };
        binding
    }

    pub(in crate::session) fn rooted_program_fixture(
        state: &mut super::super::PersistentSession,
        name: &str,
        generation: u64,
        producer: PreparedProgram,
    ) -> BindingEntry {
        let top = producer.entry();
        let program = state
            .install_prepared(producer)
            .expect("install fixed producer");
        let engine = state.prepared_mut().expect("installed fixture machine");
        let handle = engine
            .machine
            .retain_top(program, top)
            .expect("retain fixture top");
        assert!(engine.adopt(handle));
        BindingEntry {
            name: tidepool_repr::BindingName(name.into()),
            id: SessionVarId::from_extract(generation),
            module: SessionModule::val(tidepool_repr::Generation(generation)),
            value: BoundValue {
                handle,
                identity: producer_identity(),
            },
            type_display: None,
            defining_expr: None,
            scope: tidepool_codegen::scope::ScopeId::ROOT,
        }
    }

    #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
    #[test]
    fn small_stack_resume_preserves_frame_and_answer_custody_for_normal_thread_retry() {
        for borrowed in [false, true] {
            let (mut engine, program, parked) = park_json_fixture_request(
                Some(ConstructorReply::Static(TypeNodeId(0))),
                8,
                serde_json::Value::Null,
            );
            let id = parked.unwrap().id;
            let answer = engine
                .build_host_value(
                    RealmId::ROOT,
                    &HaskellValue::Con(DataConId(105), vec![]),
                    &json_mount_table(),
                )
                .unwrap();
            let before_roots = engine.persistent_roots_count();
            engine.machine.quiesce().unwrap();
            let mut engine = std::thread::Builder::new()
                .stack_size(64 * 1024)
                .spawn(move || {
                    let refused = if borrowed {
                        engine.resume_with_handle(id, answer.raw())
                    } else {
                        engine.resume_parked(id, answer)
                    };
                    assert!(matches!(
                        refused,
                        Err(PreparedRuntimeError::Run(ExecutionError::Runtime(
                            tidepool_codegen::machine_state::MachineFailure {
                                cause: tidepool_codegen::host_fns::RuntimeError::StackOverflow,
                                disposition:
                                    tidepool_codegen::machine_state::MachineDisposition::Reusable,
                            }
                        )))
                    ));
                    assert_eq!(engine.parked_count(), 1);
                    assert_eq!(engine.prepared_handle_of(answer.raw()).is_some(), borrowed);
                    assert_eq!(
                        engine.persistent_roots_count(),
                        before_roots - usize::from(!borrowed)
                    );
                    engine.machine.quiesce().unwrap();
                    engine
                })
                .unwrap()
                .join()
                .unwrap();
            let retry = if borrowed {
                answer
            } else {
                engine
                    .build_host_value(
                        RealmId::ROOT,
                        &HaskellValue::Con(DataConId(105), vec![]),
                        &json_mount_table(),
                    )
                    .unwrap()
            };
            let resumed = if borrowed {
                engine.resume_with_handle(id, retry.raw())
            } else {
                engine.resume_parked(id, retry)
            }
            .unwrap();
            let PreparedSettlement::Done { value } = resumed.settlement else {
                panic!("resume returns Done answer")
            };
            let HaskellValue::Con(identity, ref fields) =
                engine.machine.observe_handle(program, value, 100).unwrap()
            else {
                panic!("resumed answer is a Null constructor")
            };
            assert_eq!(identity, DataConId(105));
            assert!(fields.is_empty());
            assert_eq!(engine.parked_count(), 0);
            if borrowed {
                assert!(engine.release(answer));
            }
            assert!(engine.release(value));
            assert_eq!(engine.handle_count(), 0);
            assert_eq!(engine.stowed_roots_count(), 0);
            assert_eq!(engine.code_export_count(), 2);
            assert!(engine
                .code_exports
                .contains_key(&testing::identity("Fixture", "entry")));
            assert!(engine
                .code_exports
                .contains_key(&testing::identity("Fixture", PREPARED_RESUME_TARGET)));
            assert_eq!(engine.residency().root_cells, 2);
            // Bootstrap's exports retain their own roots after result custody ends.
            for export in std::mem::take(&mut engine.code_exports).into_values() {
                assert_eq!(export.program, program);
                assert!(engine.release(export.handle));
            }
            assert!(engine.unpin(program));
            engine.quiesce_and_collect_now().unwrap();
            assert_eq!(engine.residency().root_cells, 0);
            assert_eq!(engine.residency().programs, 0);
        }
    }

    #[test]
    fn root_views_are_reborrowed_after_quiescent_engine_thread_transfer() {
        let prepared = producer_program();
        let top = prepared.entry();
        let (mut engine, program) = PreparedEngine::bootstrap(prepared).unwrap();
        let handle = engine.machine.retain_top(program, top).unwrap();
        let address = engine.handle_slot(handle.raw()).unwrap().addr() as usize;
        engine.machine.quiesce().unwrap();
        // PreparedEngine owns the production quiescent transfer boundary;
        // no readable RootRef or separate MachineState owner crosses it.
        let mut engine = std::thread::spawn(move || {
            assert_eq!(
                engine.handle_slot(handle.raw()).unwrap().addr() as usize,
                address
            );
            engine.quiesce_and_collect_now().unwrap();
            assert_eq!(
                engine.handle_slot(handle.raw()).unwrap().addr() as usize,
                address
            );
            let result = engine
                .machine
                .run_entry_retained(
                    program,
                    top,
                    &[],
                    PreparedCallOptions {
                        observation_budget: 100,
                        collect_before_observation: true,
                    },
                    RealmId::ROOT,
                )
                .unwrap();
            let [PreparedResult::Managed(returned)] = result.values.as_slice() else {
                panic!("native producer returns one managed Field value")
            };
            assert_eq!(
                engine.handle_slot(handle.raw()).unwrap().addr() as usize,
                address
            );
            let CodegenPreparedOuter::Constructor { identity, fields } = engine
                .machine
                .inspect_outer(*returned, RealmId::ROOT)
                .unwrap();
            assert_eq!(identity, tidepool_repr::DataConId(980));
            assert!(matches!(fields.as_slice(), [PreparedResult::Scalar(99)]));
            assert!(engine.release(*returned));
            assert!(engine.release(handle));
            assert!(engine.handle_slot(handle.raw()).is_none());
            engine.machine.quiesce().unwrap();
            engine
        })
        .join()
        .unwrap();
        assert_eq!(engine.handle_count(), 0);
        assert_eq!(engine.parked_count(), 0);
        assert_eq!(engine.stowed_roots_count(), 0);
        assert_eq!(engine.code_export_count(), 1);
        assert!(engine.code_exports.contains_key(&producer_identity()));
        assert_eq!(engine.residency().root_cells, 1);
        assert!(engine.handle_slot(handle.raw()).is_none());
        assert!(!engine.release(handle));
        // The native export is separate from the explicit transferred handles.
        for export in std::mem::take(&mut engine.code_exports).into_values() {
            assert_eq!(export.program, program);
            assert!(engine.release(export.handle));
        }
        assert!(engine.unpin(program));
        engine.quiesce_and_collect_now().unwrap();
        assert_eq!(engine.residency().root_cells, 0);
        assert_eq!(engine.residency().programs, 0);
    }

    #[test]
    fn exact_handle_alias_custody_survives_capture_and_distinct_handle_retirement() {
        use tidepool_repr::{BindingName, Generation, SessionModule};

        for capture_first in [false, true] {
            let mut state = super::super::PersistentSession::new(None, 64 * 1024);
            let original_scope = state.mint_isolated_scope();
            let alias_scope = state.mint_isolated_scope();
            let distinct_scope = state.mint_isolated_scope();
            let original = evaluated_publication_fixture(&mut state, "original", 901);
            let original_id = original.id;
            let handle = original.value.handle;
            let shared_value = original.value.clone();
            let distinct = state
                .prepared_mut()
                .unwrap()
                .retain_handle_value(handle, RealmId::ROOT)
                .unwrap();
            assert_ne!(handle.raw(), distinct.raw());
            {
                let engine = state.prepared_mut().unwrap();
                // Independent handles keep independent cells for one object.
                let first = engine.handle_slot(handle.raw()).unwrap();
                let second = engine.handle_slot(distinct.raw()).unwrap();
                assert_ne!(first.addr(), second.addr());
                assert_eq!(first.current(), second.current());
            }
            state.bind_in(original_scope, original).unwrap();
            state
                .bind_in(
                    distinct_scope,
                    BindingEntry {
                        name: BindingName("distinct".into()),
                        id: SessionVarId::from_extract(902),
                        module: SessionModule::val(Generation(902)),
                        value: BoundValue {
                            handle: distinct,
                            identity: shared_value.identity.clone(),
                        },
                        type_display: None,
                        defining_expr: None,
                        scope: distinct_scope,
                    },
                )
                .unwrap();
            let mut alias_value = shared_value;
            alias_value.identity.occurrence = "alias".into();
            tidepool_testing::with_settlement(|settlement| {
                state.publish_alias_in(
                    original_scope,
                    BindingEntry {
                        name: BindingName("alias".into()),
                        id: SessionVarId::from_extract(903),
                        module: SessionModule::val(Generation(903)),
                        value: alias_value,
                        type_display: None,
                        defining_expr: None,
                        scope: original_scope,
                    },
                    original_id,
                    settlement,
                )
            })
            .unwrap();
            let sibling_value = state
                .resolve_in(original_scope, "alias")
                .unwrap()
                .value
                .clone();
            state
                .bind_in(
                    alias_scope,
                    BindingEntry {
                        name: BindingName("sibling".into()),
                        id: SessionVarId::from_extract(905),
                        module: SessionModule::val(Generation(905)),
                        value: sibling_value,
                        type_display: None,
                        defining_expr: None,
                        scope: alias_scope,
                    },
                )
                .unwrap();
            let capture = state.mint_detached_scope(original_scope).unwrap();
            let roots = state.persistent_roots_count();
            assert_eq!(state.value_handle_count(), 2);
            assert_eq!(state.retire_scope(original_scope).roots_released, 0);
            assert_eq!(
                state.resolve_in(capture, "original").unwrap().id,
                original_id
            );
            assert_eq!(state.retire_scope(distinct_scope).roots_released, 1);
            assert!(state
                .prepared()
                .unwrap()
                .prepared_handle_of(distinct.raw())
                .is_none());
            assert_eq!(state.persistent_roots_count(), roots - 1);
            let order = if capture_first {
                [capture, alias_scope]
            } else {
                [alias_scope, capture]
            };
            assert_eq!(state.retire_scope(order[0]).roots_released, 0);
            {
                let engine = state.prepared_mut().unwrap();
                engine.quiesce_and_collect_now().unwrap();
                let CodegenPreparedOuter::Constructor { identity, fields } = engine
                    .machine
                    .inspect_outer(handle, RealmId::ROOT)
                    .expect("surviving alias or frozen capture retains the original value");
                assert_eq!(identity, tidepool_repr::DataConId(980));
                assert!(matches!(fields.as_slice(), [PreparedResult::Scalar(99)]));
            }
            assert_eq!(state.value_handle_count(), 1);
            // Major collection may independently retire the producer's root
            // block once only its shared constructor remains reachable.
            let roots_after_collection = state.persistent_roots_count();
            assert_eq!(state.retire_scope(order[1]).roots_released, 1);
            assert_eq!(state.value_handle_count(), 0);
            assert_eq!(state.persistent_roots_count(), roots_after_collection - 1);
            let engine = state.prepared_mut().unwrap();
            assert!(engine.prepared_handle_of(handle.raw()).is_none());
            assert!(matches!(
                engine.retain_handle_value(handle, RealmId::ROOT),
                Err(PreparedRuntimeError::Run(
                    ExecutionError::UnknownPreparedHandle
                ))
            ));
            // A later admission cannot revive the stale handle identity.
            let fresh = evaluated_publication_fixture(&mut state, "fresh", 904);
            let fresh_handle = fresh.value.handle;
            assert_ne!(fresh_handle.raw(), handle.raw());
            let fresh_scope = state.mint_isolated_scope();
            state.bind_in(fresh_scope, fresh).unwrap();
            assert!(state
                .prepared()
                .unwrap()
                .prepared_handle_of(handle.raw())
                .is_none());
            assert_eq!(state.retire_scope(fresh_scope).roots_released, 1);
            assert_eq!(state.value_handle_count(), 0);
        }
    }

    /// A program whose only entry returns its one imported global. The
    /// declaration carries the producer top's own `[] -> LiftedRef` entry
    /// signature, so linking also exercises the exported-signature check.
    fn consumer_program(
        required_evaluated: bool,
        required_generation: Option<u64>,
    ) -> PreparedProgram {
        let mut wire = testing::wire_program();
        wire.signatures[0] = Signature {
            arguments: vec![],
            results: ResultContract::Returns(vec![RuntimeRep::LiftedRef]),
        };
        wire.globals = vec![GlobalDecl {
            identity: producer_identity(),
            rep: RuntimeRep::LiftedRef,
            entry_signature: Some(SignatureId(0)),
            required_evaluated,
            required_generation,
        }];
        wire.expressions.nodes[0] =
            ExprFrame::Return(vec![Atom::Ref(ValueRef::Global(GlobalId(0)))]);
        let Group::NonRecursive(top) = &mut wire.bindings[0] else {
            unreachable!()
        };
        top.binding.rhs = HeapRhs::Function {
            signature: SignatureId(0),
            parameters: vec![],
            captures: vec![],
            body: 0,
        };
        testing::prepare(wire).expect("consumer fixture")
    }

    /// Bootstrap a machine from `producer_program()`, returning it alongside
    /// the producer's own program id and its one top's [`ValueId`] -- the
    /// same shape [`PreparedEngine::bootstrap`] returns, minus the
    /// [`ProgramFacts`] bookkeeping this test module drives by hand.
    fn producer_machine() -> (PreparedMachine<'static>, ProgramId, ValueId) {
        let prepared = producer_program();
        let top = prepared.entry();
        let linked = link_program(prepared, &MachineImports::default()).expect("producer links");
        let compiled = CompiledProgram::compile(&linked).expect("producer compiles");
        let (machine, program) = PreparedMachine::new(
            compiled,
            PreparedMachineOptions {
                nursery_bytes: RunOptions::default().nursery_bytes,
            },
        )
        .expect("producer installs");
        (machine, program, top)
    }

    /// Link and install `prepared` against exactly the imports named,
    /// exactly [`PreparedEngine::install`]'s own link-then-compile-then-
    /// install sequence, minus the identity-resolution and [`ProgramFacts`]
    /// bookkeeping this test module either drives by hand or does not need.
    fn install_importing(
        machine: &mut PreparedMachine<'static>,
        prepared: PreparedProgram,
        imports: &[(SymbolIdentity, PreparedHandle, ImportedValue)],
    ) -> Result<ProgramId, PreparedRuntimeError> {
        let mut values = MachineImports::default();
        let mut bindings = ImportBindings::new();
        for (identity, handle, imported) in imports {
            values.values.insert(identity.clone(), imported.clone());
            bindings.insert(identity.clone(), *handle);
        }
        let linked = link_program(prepared, &values)?;
        let compiled = machine
            .compile_for_install(&linked)
            .map_err(PreparedRuntimeError::Compile)?;
        machine
            .install_program(compiled, bindings)
            .map_err(PreparedRuntimeError::Install)
    }

    /// The [`ImportedValue`] `link_program` checks a declared import
    /// against, read from `handle`'s live state under `generation` -- the
    /// same facts [`PreparedEngine::install`] assembles per import before
    /// linking.
    fn imported_value_of(
        machine: &PreparedMachine<'static>,
        identity: &SymbolIdentity,
        handle: PreparedHandle,
        generation: u64,
    ) -> ImportedValue {
        ImportedValue {
            identity: identity.clone(),
            rep: handle.rep(),
            entry_signature: Some(Signature {
                arguments: vec![],
                results: ResultContract::Returns(vec![RuntimeRep::LiftedRef]),
            }),
            evaluated: machine.handle_is_evaluated(handle).expect("handle is live"),
            generation,
        }
    }

    /// [`consumer_program`] declares its import with `entry_signature:
    /// Some(_)`, matching how [`install_importing`]'s hand-built
    /// [`ImportedValue`]s model a `code_exports` import. A real session
    /// value resolved through [`resolve_prepared_import`] never carries an
    /// entry signature (see [`PreparedEngine::resolve_imports`]), so a
    /// split-install test exercising that real path needs its own
    /// plain-import consumer, otherwise every install (single-checkout
    /// included) is refused by `link_program`'s `wrong_entry` check.
    fn plain_import_consumer_program(
        required_evaluated: bool,
        required_generation: Option<u64>,
    ) -> PreparedProgram {
        let mut wire = testing::wire_program();
        wire.signatures[0] = Signature {
            arguments: vec![],
            results: ResultContract::Returns(vec![RuntimeRep::LiftedRef]),
        };
        wire.globals = vec![GlobalDecl {
            identity: producer_identity(),
            rep: RuntimeRep::LiftedRef,
            entry_signature: None,
            required_evaluated,
            required_generation,
        }];
        wire.expressions.nodes[0] =
            ExprFrame::Return(vec![Atom::Ref(ValueRef::Global(GlobalId(0)))]);
        let Group::NonRecursive(top) = &mut wire.bindings[0] else {
            unreachable!()
        };
        top.binding.rhs = HeapRhs::Function {
            signature: SignatureId(0),
            parameters: vec![],
            captures: vec![],
            body: 0,
        };
        testing::prepare(wire).expect("plain-import consumer fixture")
    }

    /// A bootstrapped [`PreparedEngine`] whose producer top is bound into a
    /// real `BindingTable`/`BindingIndex` pair under `producer_identity()`
    /// at generation 1 -- the same shape [`PreparedEngine::install`]'s
    /// `resolve_imports` reads through [`resolve_prepared_import`], built
    /// the way `ResidentSession::bind_prepared` builds one (`adopt` then a
    /// `BindingEntry`), so a split install against it exercises the same
    /// import-resolution path a live turn does, not the hand-assembled
    /// `MachineImports` `install_importing` uses.
    fn engine_with_bound_producer(
        force: bool,
    ) -> (
        PreparedEngine,
        ProgramId,
        ValueId,
        BindingTable,
        BindingIndex,
    ) {
        let prepared = producer_program();
        let top = prepared.entry();
        let (mut engine, first) = PreparedEngine::bootstrap(prepared).expect("producer bootstraps");
        if force {
            engine
                .machine
                .run_entry(
                    first,
                    top,
                    &[],
                    PreparedCallOptions {
                        observation_budget: RunOptions::default().observation_budget,
                        collect_before_observation: true,
                    },
                    RealmId::ROOT,
                )
                .expect("producer entry runs");
        }
        let handle = engine
            .machine
            .retain_top(first, top)
            .expect("producer top binds");
        assert!(engine.adopt(handle));
        let entry = BindingEntry {
            name: tidepool_repr::BindingName("producer".into()),
            id: SessionVarId::from_extract(1),
            module: SessionModule::val(tidepool_repr::Generation(1)),
            value: BoundValue {
                handle,
                identity: producer_identity(),
            },
            type_display: None,
            defining_expr: None,
            scope: tidepool_codegen::scope::ScopeId::ROOT,
        };
        let mut index = BindingIndex::new();
        index.on_bind(&entry);
        let mut bindings = BindingTable::new();
        bindings.bind(entry).unwrap();
        (engine, first, top, bindings, index)
    }

    #[test]
    fn native_install_refusal_retains_stage_on_shared_and_owned_images() {
        for shared in [false, true] {
            let (mut engine, first, _, bindings, index) = engine_with_bound_producer(false);
            if shared {
                engine.set_image_registry(Arc::new(ImageRegistry::new()));
            }
            let (mut foreign, foreign_program, foreign_top) = producer_machine();
            let foreign_handle = foreign.retain_top(foreign_program, foreign_top).unwrap();
            let prepared = plain_import_consumer_program(false, Some(1));
            let (values, mut imports) = engine
                .resolve_imports(&prepared, &bindings, &index)
                .unwrap();
            imports.insert(producer_identity(), foreign_handle);
            let linked = link_program(prepared, &values).unwrap();
            let before = engine.residency();
            let error = engine.compile_and_install(linked, imports).err().unwrap();
            assert!(matches!(
                error,
                PreparedRuntimeError::Install(ExecutionError::UnknownPreparedHandle)
            ));
            assert_eq!(error.stage(), PreparedFailureStage::Install);
            assert_eq!(error.kind(), PreparedFailureKind::Rejected);
            assert_eq!(engine.residency(), before);

            let run_error = engine
                .run_settled(first, RealmId::ROOT)
                .err()
                .expect("ordinary producer has no settled scaffold");
            assert!(matches!(
                run_error,
                PreparedRuntimeError::UnsettledEntry { program, .. } if program == first
            ));
            assert_eq!(run_error.stage(), PreparedFailureStage::Run);
            assert_eq!(engine.residency(), before);
            engine
                .install(
                    plain_import_consumer_program(false, Some(1)),
                    &bindings,
                    &index,
                )
                .unwrap();
            assert!(foreign.release(foreign_handle));
        }
    }

    #[test]
    fn split_install_matches_single_checkout_install_with_imports() {
        let (mut single, _, _, bindings, index) = engine_with_bound_producer(true);
        let single_program = single
            .install(
                plain_import_consumer_program(true, Some(1)),
                &bindings,
                &index,
            )
            .expect("single-checkout install links against the bound producer");
        let single_read = read_consumer_import(&mut single, single_program);

        let (mut split, _, _, bindings, index) = engine_with_bound_producer(true);
        let snapshot = split
            .snapshot_install(
                plain_import_consumer_program(true, Some(1)),
                &bindings,
                &index,
            )
            .expect("snapshot step resolves the same imports off no checkout yet");
        let mut snapshot = snapshot;
        let compiled = PreparedEngine::compile_off_checkout(&mut snapshot)
            .expect("off-checkout compile of the linked program succeeds");
        let definitions = Arc::clone(compiled.definition_facts());
        let split_program = split
            .revalidate_and_install(snapshot, compiled, &bindings, &index)
            .expect("revalidation runs")
            .expect("nothing changed the import between snapshot and revalidation");
        assert!(Arc::ptr_eq(
            &split.programs[&split_program].definitions,
            &definitions,
        ));
        let split_read = read_consumer_import(&mut split, split_program);

        // Both engines were bootstrapped and bound identically, so the split
        // install must be observably the same program as the single-checkout
        // one: same value read back through the same import.
        assert_eq!(single_read, split_read);
    }

    /// Run `program`'s one entry (a [`plain_import_consumer_program`],
    /// which just returns its imported global) and decode the constructor
    /// it reads back, releasing the observed value afterward.
    fn read_consumer_import(
        engine: &mut PreparedEngine,
        program: ProgramId,
    ) -> (tidepool_repr::DataConId, Vec<u64>) {
        let read = engine
            .machine
            .run_entry_retained(
                program,
                ValueId(0),
                &[],
                PreparedCallOptions {
                    observation_budget: RunOptions::default().observation_budget,
                    collect_before_observation: true,
                },
                RealmId::ROOT,
            )
            .expect("consumer entry runs and reads its import through the slot");
        let mut values = read.values;
        let PreparedResult::Managed(value) = values.remove(0) else {
            panic!("consumer must return the imported managed value");
        };
        let CodegenPreparedOuter::Constructor { identity, fields } = engine
            .machine
            .inspect_outer(value, RealmId::ROOT)
            .expect("imported value inspects through the shared machine");
        let scalars = fields
            .iter()
            .map(|field| match field {
                PreparedResult::Scalar(n) => *n,
                other => panic!("expected a scalar field, got {other:?}"),
            })
            .collect();
        assert!(engine.machine.release(value));
        (identity, scalars)
    }

    #[test]
    fn stale_import_between_snapshot_and_revalidation_falls_back() {
        // Snapshot against the UNFORCED producer top (required_evaluated:
        // false, so linking the snapshot itself does not reject it), then
        // force the CAF -- simulating another actor's turn mutating the
        // same shared import while this compile ran off-checkout -- before
        // revalidating.
        let (mut engine, first, top, bindings, index) = engine_with_bound_producer(false);
        let snapshot = engine
            .snapshot_install(
                plain_import_consumer_program(false, Some(1)),
                &bindings,
                &index,
            )
            .expect("snapshot resolves the still-unforced import");
        let mut snapshot = snapshot;
        let compiled = PreparedEngine::compile_off_checkout(&mut snapshot)
            .expect("off-checkout compile succeeds against the unforced snapshot");

        engine
            .machine
            .run_entry(
                first,
                top,
                &[],
                PreparedCallOptions {
                    observation_budget: RunOptions::default().observation_budget,
                    collect_before_observation: true,
                },
                RealmId::ROOT,
            )
            .expect("forcing the CAF between snapshot and revalidation");

        let outcome = engine
            .revalidate_and_install(snapshot, compiled, &bindings, &index)
            .expect("revalidation itself does not error");
        assert!(
            outcome.is_none(),
            "an import that became evaluated between snapshot and revalidation \
             must invalidate the split compile rather than install it"
        );

        // The caller's documented recovery -- recompile fresh, or fall back
        // to the single-checkout path -- still succeeds against the now-
        // forced import.
        engine
            .install(
                plain_import_consumer_program(false, Some(1)),
                &bindings,
                &index,
            )
            .expect("the single-checkout fallback installs against the current import");
    }

    #[test]
    fn bind_install_run_reads_the_bound_top_by_generation() {
        let (mut machine, first, top) = producer_machine();
        // Force the CAF once so it is an evaluated (updated) constructor.
        machine
            .run_entry(
                first,
                top,
                &[],
                PreparedCallOptions {
                    observation_budget: RunOptions::default().observation_budget,
                    collect_before_observation: true,
                },
                RealmId::ROOT,
            )
            .expect("producer entry runs");
        let handle = machine.retain_top(first, top).expect("producer top binds");
        let identity = producer_identity();
        let imported = imported_value_of(&machine, &identity, handle, 1);
        let consumer = install_importing(
            &mut machine,
            consumer_program(true, Some(1)),
            &[(identity, handle, imported)],
        )
        .expect("consumer links against generation 1 and installs");
        assert_ne!(consumer, first);

        let read = machine
            .run_entry_retained(
                consumer,
                ValueId(0),
                &[],
                PreparedCallOptions {
                    observation_budget: RunOptions::default().observation_budget,
                    collect_before_observation: true,
                },
                RealmId::ROOT,
            )
            .expect("consumer reads its import through the slot");
        let mut values = read.values;
        let PreparedResult::Managed(value) = values.remove(0) else {
            panic!("consumer must return the imported managed value");
        };
        let CodegenPreparedOuter::Constructor { identity, fields } = machine
            .inspect_outer(value, RealmId::ROOT)
            .expect("imported value inspects through the shared machine");
        assert_eq!(identity, tidepool_repr::DataConId(980));
        assert!(matches!(fields.as_slice(), [PreparedResult::Scalar(99)]));
        assert!(machine.release(value));
    }

    #[test]
    fn stale_generation_is_refused_by_link_before_any_install_side_effect() {
        let (mut machine, first, top) = producer_machine();
        let handle = machine
            .retain_top(first, top)
            .expect("producer top binds unevaluated");
        let identity = producer_identity();
        let imported = imported_value_of(&machine, &identity, handle, 1);
        let handles_before = machine.handle_count();
        let error = install_importing(
            &mut machine,
            consumer_program(false, Some(7)),
            &[(identity.clone(), handle, imported.clone())],
        )
        .expect_err("a consumer linked against generation 7 must not install");
        assert!(
            matches!(&error, PreparedRuntimeError::Link(link) if matches!(**link, LinkError::ImportContract(_))),
            "expected ImportContract, got {error:?}"
        );
        assert_eq!(error.kind(), PreparedFailureKind::Rejected);
        assert_eq!(machine.handle_count(), handles_before);
        // The same producer binding still installs a correctly-linked
        // consumer -- the refused link left the machine installable.
        let mut imported_at_1 = imported;
        imported_at_1.generation = 1;
        install_importing(
            &mut machine,
            consumer_program(false, Some(1)),
            &[(identity, handle, imported_at_1)],
        )
        .expect("the refused link left the machine installable");
    }

    #[test]
    fn missing_import_is_refused_by_link() {
        let (mut machine, _first, _top) = producer_machine();
        let error = install_importing(&mut machine, consumer_program(false, None), &[])
            .expect_err("a declared global with no binding must not install");
        assert!(
            matches!(&error, PreparedRuntimeError::Link(link) if matches!(**link, LinkError::MissingImport(_))),
            "expected MissingImport, got {error:?}"
        );
    }

    #[test]
    fn required_evaluated_is_checked_against_the_live_value() {
        let (mut machine, first, top) = producer_machine();
        let handle = machine
            .retain_top(first, top)
            .expect("the unforced CAF binds");
        let identity = producer_identity();
        let unforced = imported_value_of(&machine, &identity, handle, 1);
        assert!(!unforced.evaluated, "the CAF has not been forced yet");
        let error = install_importing(
            &mut machine,
            consumer_program(true, Some(1)),
            &[(identity.clone(), handle, unforced)],
        )
        .expect_err("an unforced thunk does not satisfy required_evaluated");
        assert!(
            matches!(&error, PreparedRuntimeError::Link(link) if matches!(**link, LinkError::ImportContract(_))),
            "expected ImportContract, got {error:?}"
        );
        machine
            .run_entry(
                first,
                top,
                &[],
                PreparedCallOptions {
                    observation_budget: RunOptions::default().observation_budget,
                    collect_before_observation: true,
                },
                RealmId::ROOT,
            )
            .expect("forcing the CAF updates the bound top in place");
        let forced = imported_value_of(&machine, &identity, handle, 1);
        assert!(forced.evaluated, "the CAF is now forced");
        install_importing(
            &mut machine,
            consumer_program(true, Some(1)),
            &[(identity, handle, forced)],
        )
        .expect("the same binding now satisfies required_evaluated");
    }

    #[allow(
        clippy::too_many_arguments,
        reason = "test fixture builder; each argument is an independent field of the constructor \
                  under test, not a natural grouping"
    )]
    fn mount_constructor(
        module: &str,
        occurrence: &str,
        family_module: &str,
        family_occurrence: &str,
        id: u64,
        tag: u32,
        family_size: u32,
        fields: Vec<RuntimeRep>,
    ) -> ConstructorDecl {
        let layout = StorageLayout::for_reps(&testing::target(), &fields)
            .expect("mount fixture field layout");
        let mut identity = testing::identity(module, occurrence);
        identity.namespace = "constructor".into();
        let mut family = testing::identity(family_module, family_occurrence);
        family.namespace = "type".into();
        ConstructorDecl {
            identity,
            family,
            host_id: DataConId(id),
            result_rep: RuntimeRep::LiftedRef,
            tag,
            family_size,
            strict_fields: vec![true; fields.len()],
            field_reps: fields,
            layout: CheckedLayout {
                fields: layout
                    .fields()
                    .iter()
                    .map(|field| FieldLayout {
                        rep: field.rep(),
                        offset: field.offset(),
                    })
                    .collect(),
                alignment: layout.alignment(),
                payload_size: layout.payload_size(),
                root_mask: layout
                    .fields()
                    .iter()
                    .map(|field| {
                        matches!(field.rep(), RuntimeRep::LiftedRef | RuntimeRep::UnliftedRef)
                    })
                    .collect(),
            },
        }
    }

    fn mount_table_row(constructor: &ConstructorDecl) -> DataCon {
        DataCon {
            identity: constructor.identity.clone(),
            id: constructor.host_id,
            name: constructor.identity.occurrence.clone(),
            tag: constructor.tag,
            rep_arity: constructor.field_reps.len() as u32,
            field_bangs: Vec::new(),
            qualified_name: Some(format!(
                "{}.{}",
                constructor.identity.module, constructor.identity.occurrence
            )),
            type_name: constructor.family.occurrence.clone(),
        }
    }

    fn canonical_mount_value(value: &HaskellValue, output: &mut String) {
        match value {
            HaskellValue::Lit(Literal::LitByteArray(bytes)) => {
                output.push_str(&format!("B{bytes:?};"));
            }
            HaskellValue::Lit(literal) => output.push_str(&format!("L{literal:?};")),
            HaskellValue::ByteArray(bytes) => {
                output.push_str(&format!("B{:?};", *bytes.lock().expect("byte array lock")));
            }
            HaskellValue::Con(id, fields) => {
                output.push_str(&format!("C{}(", id.0));
                for field in fields {
                    canonical_mount_value(field, output);
                }
                output.push_str(");");
            }
        }
    }

    /// The whole family the JSON bridge emits, plus `BadText`: it deliberately
    /// has three scalar fields so its table row passes name/arity lookup but
    /// descriptor validation rejects it after the byte array was built.
    fn json_mount_program() -> PreparedProgram {
        json_mount_program_with_verb(false)
    }

    fn json_mount_program_with_verb(include_verb: bool) -> PreparedProgram {
        json_mount_program_with_replies(if include_verb {
            vec![(ConstructorId(0), ConstructorReply::Static(TypeNodeId(0)))]
        } else {
            vec![]
        })
    }

    fn json_mount_program_with_replies(
        replies: Vec<(ConstructorId, ConstructorReply)>,
    ) -> PreparedProgram {
        let mut wire = testing::wire_program();
        wire.signatures[0].results = ResultContract::Returns(vec![RuntimeRep::LiftedRef]);
        wire.constructors = vec![
            mount_constructor(
                "Fixture.Mount",
                "Unit",
                "Fixture.Mount",
                "Unit",
                1,
                1,
                1,
                vec![],
            ),
            mount_constructor(
                "Tidepool.Aeson.Value",
                "Object",
                "Tidepool.Aeson.Value",
                "Value",
                100,
                1,
                6,
                vec![RuntimeRep::LiftedRef],
            ),
            mount_constructor(
                "Tidepool.Aeson.Value",
                "Array",
                "Tidepool.Aeson.Value",
                "Value",
                101,
                2,
                6,
                vec![RuntimeRep::LiftedRef],
            ),
            mount_constructor(
                "Tidepool.Aeson.Value",
                "String",
                "Tidepool.Aeson.Value",
                "Value",
                102,
                3,
                6,
                vec![RuntimeRep::LiftedRef],
            ),
            mount_constructor(
                "Tidepool.Aeson.Value",
                "Number",
                "Tidepool.Aeson.Value",
                "Value",
                103,
                4,
                6,
                vec![RuntimeRep::LiftedRef],
            ),
            mount_constructor(
                "Tidepool.Aeson.Value",
                "Bool",
                "Tidepool.Aeson.Value",
                "Value",
                104,
                5,
                6,
                vec![RuntimeRep::LiftedRef],
            ),
            mount_constructor(
                "Tidepool.Aeson.Value",
                "Null",
                "Tidepool.Aeson.Value",
                "Value",
                105,
                6,
                6,
                vec![],
            ),
            mount_constructor(
                "Tidepool.Aeson.Scientific",
                "Scientific",
                "Tidepool.Aeson.Scientific",
                "Scientific",
                110,
                1,
                1,
                vec![RuntimeRep::LiftedRef, RuntimeRep::Int(64)],
            ),
            mount_constructor(
                "GHC.Num.Integer",
                "IS",
                "GHC.Num.Integer",
                "Integer",
                120,
                1,
                3,
                vec![RuntimeRep::Int(64)],
            ),
            mount_constructor(
                "GHC.Num.Integer",
                "IP",
                "GHC.Num.Integer",
                "Integer",
                121,
                2,
                3,
                vec![RuntimeRep::UnliftedRef],
            ),
            mount_constructor(
                "GHC.Num.Integer",
                "IN",
                "GHC.Num.Integer",
                "Integer",
                122,
                3,
                3,
                vec![RuntimeRep::UnliftedRef],
            ),
            mount_constructor("GHC.Types", "True", "GHC.Types", "Bool", 130, 2, 2, vec![]),
            mount_constructor("GHC.Types", "False", "GHC.Types", "Bool", 131, 1, 2, vec![]),
            mount_constructor(
                "Data.Map.Internal",
                "Bin",
                "Data.Map.Internal",
                "Map",
                140,
                1,
                2,
                vec![RuntimeRep::LiftedRef; 5],
            ),
            mount_constructor(
                "Data.Map.Internal",
                "Tip",
                "Data.Map.Internal",
                "Map",
                141,
                2,
                2,
                vec![],
            ),
            mount_constructor(
                "GHC.Types",
                "I#",
                "GHC.Types",
                "Int",
                150,
                1,
                1,
                vec![RuntimeRep::Int(64)],
            ),
            mount_constructor(
                "Data.Text.Internal",
                "Text",
                "Data.Text.Internal",
                "Text",
                160,
                1,
                1,
                vec![
                    RuntimeRep::UnliftedRef,
                    RuntimeRep::Int(64),
                    RuntimeRep::Int(64),
                ],
            ),
            mount_constructor(
                "GHC.Types",
                ":",
                "GHC.Types",
                "List",
                170,
                2,
                2,
                vec![RuntimeRep::LiftedRef; 2],
            ),
            mount_constructor("GHC.Types", "[]", "GHC.Types", "List", 171, 1, 2, vec![]),
            mount_constructor(
                "Fixture.Mount",
                "BadText",
                "Fixture.Mount",
                "BadText",
                900,
                1,
                1,
                vec![RuntimeRep::Int(64); 3],
            ),
            mount_constructor(
                "Tidepool.Internal.Resume",
                "Done",
                "Tidepool.Internal.Resume",
                "Settled",
                901,
                1,
                2,
                vec![RuntimeRep::LiftedRef],
            ),
            mount_constructor(
                "Tidepool.Internal.Resume",
                "Suspended",
                "Tidepool.Internal.Resume",
                "Settled",
                902,
                2,
                2,
                vec![RuntimeRep::LiftedRef; 2],
            ),
            mount_constructor(
                "Fixture.Mount",
                "Framed",
                "Fixture.Mount",
                "Framed",
                903,
                1,
                1,
                vec![RuntimeRep::Int(64), RuntimeRep::LiftedRef],
            ),
        ];
        wire.expressions.nodes[0] = ExprFrame::Construct {
            constructor: ConstructorId(0),
            fields: vec![],
        };
        let Group::NonRecursive(top) = &mut wire.bindings[0] else {
            unreachable!()
        };
        top.binding.rhs = HeapRhs::Thunk {
            signature: SignatureId(0),
            update: UpdatePolicy::Memoize,
            captures: vec![],
            body: 0,
        };
        wire.signatures.push(Signature {
            arguments: vec![RuntimeRep::LiftedRef; 2],
            results: ResultContract::Returns(vec![RuntimeRep::LiftedRef]),
        });
        wire.expressions.nodes.push(ExprFrame::Construct {
            constructor: ConstructorId(20),
            fields: vec![Atom::Ref(ValueRef::Local(ValueId(3)))],
        });
        wire.bindings.push(Group::NonRecursive(TopBinding {
            identity: testing::identity("Fixture", PREPARED_RESUME_TARGET),
            binding: HeapBinding {
                id: ValueId(1),
                rhs: HeapRhs::Function {
                    signature: SignatureId(1),
                    parameters: vec![ValueId(2), ValueId(3)],
                    captures: vec![],
                    body: 1,
                },
            },
        }));
        let mut json_value_family = testing::identity("Tidepool.Aeson.Value", "Value");
        json_value_family.namespace = "type".into();
        let mut scalar_family = testing::identity("Fixture.Mount", "Int64");
        scalar_family.namespace = "type".into();
        let mut framed_family = testing::identity("Fixture.Mount", "Framed");
        framed_family.namespace = "type".into();
        let declaration = |identity, form| TypeNode::Declaration {
            identity,
            parameters: vec![],
            form,
            restriction: SyntaxRestriction::None,
        };
        let mut nodes = vec![
            TypeNode::Root {
                domain: RootDomain::Closed,
                binders: vec![],
                rendered: "Value".into(),
            },
            TypeNode::Root {
                domain: RootDomain::Closed,
                binders: vec![],
                rendered: "Int#".into(),
            },
            TypeNode::Root {
                domain: RootDomain::Closed,
                binders: vec![],
                rendered: "Framed".into(),
            },
            declaration(json_value_family, DeclarationForm::Data),
            declaration(scalar_family, DeclarationForm::Scalar(RuntimeRep::Int(64))),
            declaration(framed_family, DeclarationForm::Data),
            TypeNode::NominalApplication,
            TypeNode::NominalApplication,
            TypeNode::NominalApplication,
        ];
        let mut edges = vec![
            (0, 6, TypeEdge::Body),
            (1, 7, TypeEdge::Body),
            (2, 8, TypeEdge::Body),
            (6, 3, TypeEdge::Head),
            (7, 4, TypeEdge::Head),
            (8, 5, TypeEdge::Head),
        ];
        for constructor in 1..=6 {
            let template = nodes.len() as u32;
            nodes.push(TypeNode::ConstructorTemplate {
                constructor: ConstructorId(constructor),
                identity: wire.constructors[constructor as usize].identity.clone(),
            });
            edges.push((
                3,
                template,
                TypeEdge::Constructor(wire.constructors[constructor as usize].tag),
            ));
            if constructor != 6 {
                edges.push((
                    template,
                    6,
                    TypeEdge::Field {
                        ordinal: 0,
                        source_rep: RuntimeRep::LiftedRef,
                    },
                ));
            }
        }
        let framed = nodes.len() as u32;
        nodes.push(TypeNode::ConstructorTemplate {
            constructor: ConstructorId(22),
            identity: wire.constructors[22].identity.clone(),
        });
        edges.extend([
            (5, framed, TypeEdge::Constructor(wire.constructors[22].tag)),
            (
                framed,
                7,
                TypeEdge::Field {
                    ordinal: 0,
                    source_rep: RuntimeRep::Int(64),
                },
            ),
            (
                framed,
                6,
                TypeEdge::Field {
                    ordinal: 1,
                    source_rep: RuntimeRep::LiftedRef,
                },
            ),
        ]);
        wire.types = prepared_data::type_graph(nodes, &edges, &wire.constructors)
            .expect("JSON and framed templates");
        wire.sites = vec![
            SiteRow {
                site: 7,
                origin: "Fixture.Mount.json_reply".into(),
                ordinal: 0,
                delivery: SiteDelivery::HostAnswer,
                wire: TypeNodeId(0),
                inputs: vec![],
            },
            SiteRow {
                site: 8,
                origin: "Fixture.Mount.framed_reply".into(),
                ordinal: 1,
                delivery: SiteDelivery::HostAnswer,
                wire: TypeNodeId(2),
                inputs: vec![],
            },
        ];
        wire.json_layout = Some(JsonLayout {
            object: ConstructorId(1),
            array: ConstructorId(2),
            string: ConstructorId(3),
            number: ConstructorId(4),
            bool_: ConstructorId(5),
            null: ConstructorId(6),
            map_bin: ConstructorId(13),
            map_tip: ConstructorId(14),
            true_: ConstructorId(11),
            false_: ConstructorId(12),
            cons: ConstructorId(17),
            nil: ConstructorId(18),
            scientific: ConstructorId(7),
            integer_small: ConstructorId(8),
            integer_positive: ConstructorId(9),
            integer_negative: ConstructorId(10),
            text: ConstructorId(16),
            int: ConstructorId(15),
        });
        wire.constructor_replies = replies;
        testing::prepare(wire).expect("JSON mount fixture")
    }

    fn json_mount_table() -> DataConTable {
        let mut table = DataConTable::new();
        table
            .extend_checked(
                json_mount_program()
                    .constructors()
                    .iter()
                    .map(mount_table_row),
            )
            .unwrap();
        table
    }

    fn park_json_fixture_request(
        reply: Option<ConstructorReply>,
        first_field: i64,
        payload: serde_json::Value,
    ) -> (
        PreparedEngine,
        ProgramId,
        Result<PreparedParked, PreparedRuntimeError>,
    ) {
        let prepared = json_mount_program_with_replies(
            reply
                .map(|reply| (ConstructorId(22), reply))
                .into_iter()
                .collect(),
        );
        let layout = prepared
            .json_layout()
            .unwrap()
            .try_map(|constructor| {
                prepared
                    .constructors()
                    .get(constructor.0 as usize)
                    .map(|declaration| declaration.host_id)
                    .ok_or(())
            })
            .unwrap();
        let table = json_mount_table().with_json_layout(Some(layout));
        let (mut engine, program) = PreparedEngine::bootstrap(prepared).unwrap();
        let payload = payload.to_value(&table).unwrap();
        let request = HaskellValue::Con(
            DataConId(901),
            vec![HaskellValue::Con(
                DataConId(903),
                vec![HaskellValue::Lit(Literal::LitInt(first_field)), payload],
            )],
        );
        let request = engine
            .build_host_value(RealmId::ROOT, &request, &table)
            .unwrap();
        let continuation = engine
            .build_host_value(
                RealmId::ROOT,
                &HaskellValue::Con(DataConId(105), vec![]),
                &table,
            )
            .unwrap();
        let parked = engine.park_suspension(
            program,
            RealmId::ROOT,
            ParkPolicy {
                principal: PrincipalId::SYSTEM,
                effect_policy: EffectRunPolicy::SuspendAll,
                live_payload: LivePayloadPolicy::None,
            },
            request,
            continuation,
            &table,
        );
        (engine, program, parked)
    }

    #[test]
    fn nested_user_typed_site_cannot_override_static_reply_evidence() {
        let (mut engine, owner, parked) = park_json_fixture_request(
            Some(ConstructorReply::Static(TypeNodeId(0))),
            8,
            serde_json::json!({"user": {"typedSite": 8}}),
        );
        let parked = parked.unwrap();
        let (realm, evidence) = engine.parked(parked.id).unwrap();
        assert_eq!(realm, RealmId::ROOT);
        assert_eq!(
            evidence.reply,
            PreparedReplyEvidence::Static {
                owner,
                constructor: DataConId(903),
                node: TypeNodeId(0)
            }
        );
        assert_eq!(engine.parked_site(parked.id), None);
        engine.abort_parked(parked.id).unwrap();
        assert_eq!(engine.parked_count(), 0);
        assert_eq!(engine.handle_count(), 0);
    }

    #[test]
    fn unattested_request_refuses_unrelated_int_without_parking_or_leaking_roots() {
        let (engine, _, parked) = park_json_fixture_request(None, 8, serde_json::Value::Null);
        assert!(matches!(
            parked,
            Err(PreparedRuntimeError::MissingReplyEvidence {
                constructor: DataConId(903)
            })
        ));
        assert_eq!(engine.parked_count(), 0);
        assert_eq!(engine.handle_count(), 0);
        assert_eq!(engine.stowed_roots_count(), 0);
    }

    #[test]
    fn host_json_and_text_mount_stream_through_tiny_nursery_and_recover_after_rejection() {
        let prepared = json_mount_program();
        let layout = prepared
            .json_layout()
            .expect("JSON mount program carries layout")
            .try_map(|constructor| {
                prepared
                    .constructors()
                    .get(constructor.0 as usize)
                    .map(|declaration| declaration.host_id)
                    .ok_or(())
            })
            .expect("JSON layout IDs are declared");
        let (mut engine, program) = PreparedEngine::bootstrap_with_nursery_bytes(prepared, 64)
            .expect("bootstrap JSON mount fixture");
        let table = json_mount_table();
        let table = table.with_json_layout(Some(layout));
        let payload = serde_json::json!({
            "nested": [[{"key": "value", "n": serde_json::Value::Number("1000000000000000000000000000001".parse().expect("large JSON number"))}], [true, null]],
            "large": (0..128).map(|index| serde_json::json!({"index": index, "text": "x".repeat(32)})).collect::<Vec<_>>(),
        });

        let initial_handles = engine.handle_count();
        let initial_roots = engine.persistent_roots_count();
        let json = engine
            .build_host_json(RealmId::ROOT, &payload, &layout)
            .expect("large nested JSON mounts under a tiny nursery");
        let observed = engine.observe(program, json).expect("observe mounted JSON");
        let expected = payload.to_value(&table).expect("reference JSON value");
        let mut observed_shape = String::new();
        canonical_mount_value(&observed, &mut observed_shape);
        let mut expected_shape = String::new();
        canonical_mount_value(&expected, &mut expected_shape);
        assert_eq!(observed_shape, expected_shape);
        assert!(engine.release(json));

        struct FailsAfterBytes;
        impl tidepool_bridge::sealed::ToHaskellSealed for FailsAfterBytes {}
        impl tidepool_bridge::ToHaskell for FailsAfterBytes {
            fn visit(
                &self,
                _table: &DataConTable,
                visitor: &mut dyn tidepool_bridge::HaskellVisitor,
            ) -> Result<(), tidepool_bridge::BridgeError> {
                visitor.literal(Literal::LitByteArray(vec![1; 256]))?;
                Err(tidepool_bridge::BridgeError::UnknownDataConName(
                    "unresolved".into(),
                ))
            }
        }
        assert!(matches!(
            engine.build_host_value(RealmId::ROOT, &FailsAfterBytes, &table),
            Err(PreparedRuntimeError::HostMount { .. })
        ));
        assert_eq!(engine.handle_count(), initial_handles);
        assert_eq!(engine.persistent_roots_count(), initial_roots);

        let text = engine
            .build_host_text_exact(RealmId::ROOT, "reusable after rejection", DataConId(160))
            .expect("a later Text mount succeeds");
        assert!(matches!(
            engine.observe(program, text).expect("observe mounted Text"),
            HaskellValue::Con(id, ref fields)
                if id == DataConId(160)
                    && matches!(fields.as_slice(), [HaskellValue::Lit(Literal::LitByteArray(bytes)), HaskellValue::Lit(Literal::LitInt(0)), HaskellValue::Lit(Literal::LitInt(24))] if bytes == b"reusable after rejection")
        ));
        assert!(engine.release(text));
        assert_eq!(engine.handle_count(), initial_handles);
        assert_eq!(engine.persistent_roots_count(), initial_roots);
    }

    #[test]
    fn cancelled_invocation_keeps_parked_resume_and_same_realm_sibling_usable() {
        let (mut engine, program) = PreparedEngine::bootstrap(json_mount_program()).unwrap();
        let realm = RealmId::fresh();
        let mut parked = Vec::new();
        for _ in 0..2 {
            let mut builder = engine.machine.managed_builder().unwrap();
            let root = builder.constructor(DataConId(105), &[]).unwrap();
            let continuation = builder.finish(realm, root).unwrap();
            parked.push(
                engine
                    .machine
                    .park(
                        continuation,
                        realm,
                        None,
                        ParkRequest {
                            principal: PrincipalId::SYSTEM,
                            effect_policy: EffectRunPolicy::SuspendAll,
                            live_payload: LivePayloadPolicy::None,
                            evidence: PreparedFrameEvidence {
                                reply: PreparedReplyEvidence::AtSite {
                                    owner: program,
                                    row: 0,
                                },
                                runner: program,
                                resume_entry: ValueId(1),
                                continuation_rep: RuntimeRep::LiftedRef,
                            },
                        },
                    )
                    .unwrap(),
            );
        }
        let table = json_mount_table();
        let response = serde_json::json!({"answer": 42});
        let handles = engine.handle_count();
        let roots = engine.persistent_roots_count();
        engine.set_invocation_cancel(Some(Arc::new(AtomicBool::new(true))));
        assert!(matches!(
            engine.resume_with_structural_answer(parked[0], &response, &table),
            Err(PreparedRuntimeError::Cancelled)
        ));
        assert_eq!(engine.parked_count(), 2);
        assert_eq!(engine.handle_count(), handles);
        assert_eq!(engine.persistent_roots_count(), roots);
        assert!(!engine.cancel_handle(realm).is_cancelled());

        engine.set_invocation_cancel(Some(Arc::new(AtomicBool::new(false))));
        let resumed = engine
            .resume_with_structural_answer(parked[1], &response, &table)
            .unwrap();
        let PreparedSettlement::Done { value } = resumed.settlement else {
            panic!("fixture resume returns Done");
        };
        engine.release(value);
        assert_eq!(engine.parked_count(), 1);
        engine.cancel_handle(realm).cancel();
        assert!(matches!(
            engine.resume_with_structural_answer(parked[0], &response, &table),
            Err(PreparedRuntimeError::Cancelled)
        ));
        assert_eq!(engine.parked_count(), 1);
        assert_eq!(engine.close_realm(realm), (1, 0));
        assert_eq!(engine.parked_count(), 0);
    }

    #[test]
    fn structural_json_effect_reply_uses_parked_owner_layout() {
        let (mut engine, program) =
            PreparedEngine::bootstrap(json_mount_program()).expect("bootstrap JSON reply fixture");
        let table = json_mount_table();
        assert!(
            table.json_layout().is_none(),
            "the accumulated session table deliberately has no JSON authority"
        );

        let continuation = {
            let mut builder = engine
                .machine
                .managed_builder()
                .expect("JSON reply fixture opens a managed builder");
            let root = builder
                .constructor(DataConId(105), &[])
                .expect("fixture Null constructor is declared");
            builder
                .finish(RealmId::ROOT, root)
                .expect("fixture continuation is retained")
        };
        let id = engine
            .machine
            .park(
                continuation,
                RealmId::ROOT,
                None,
                ParkRequest {
                    principal: PrincipalId::SYSTEM,
                    effect_policy: EffectRunPolicy::SuspendAll,
                    live_payload: LivePayloadPolicy::None,
                    evidence: PreparedFrameEvidence {
                        reply: PreparedReplyEvidence::AtSite {
                            owner: program,
                            row: 0,
                        },
                        runner: program,
                        resume_entry: ValueId(1),
                        continuation_rep: RuntimeRep::LiftedRef,
                    },
                },
            )
            .expect("fixture continuation parks");

        let response = serde_json::json!({"worked": [true, 3]});
        let resumed = engine
            .resume_with_structural_answer(id, &response, &table)
            .expect("JSON response derives its layout from the parked site owner");
        let PreparedSettlement::Done { value } = resumed.settlement else {
            panic!("resume fixture returns a settled Done value")
        };
        assert!(matches!(
            engine
                .observe(program, value)
                .expect("observe resumed JSON"),
            HaskellValue::Con(DataConId(100), _)
        ));
        assert!(engine.release(value));
        assert_eq!(engine.parked_count(), 0);
    }

    #[test]
    fn parcel_import_retains_typed_replies_across_instance_retirement() {
        fn park_json_reply(
            engine: &mut PreparedEngine,
            owner: ProgramId,
            site: u64,
        ) -> ContinuationId {
            let continuation = {
                let mut builder = engine.machine.managed_builder().expect("reply builder");
                let root = builder.constructor(DataConId(105), &[]).expect("Null");
                builder
                    .finish(RealmId::ROOT, root)
                    .expect("rooted continuation")
            };
            engine
                .machine
                .park(
                    continuation,
                    RealmId::ROOT,
                    None,
                    ParkRequest {
                        principal: PrincipalId::SYSTEM,
                        effect_policy: EffectRunPolicy::SuspendAll,
                        live_payload: LivePayloadPolicy::None,
                        evidence: PreparedFrameEvidence {
                            reply: PreparedReplyEvidence::AtSite {
                                owner,
                                row: engine.programs[&owner]
                                    .sites
                                    .iter()
                                    .position(|row| row.site == site)
                                    .expect("declared reply site"),
                            },
                            runner: owner,
                            resume_entry: ValueId(1),
                            continuation_rep: RuntimeRep::LiftedRef,
                        },
                    },
                )
                .expect("imported continuation parks")
        }
        fn answer_json(engine: &mut PreparedEngine, owner: ProgramId) {
            let id = park_json_reply(engine, owner, 7);
            let resumed = engine
                .resume_with_structural_answer(
                    id,
                    &serde_json::json!({"worked": [true, 3]}),
                    &json_mount_table(),
                )
                .expect("imported site's field-bearing answer remains typed");
            let PreparedSettlement::Done { value } = resumed.settlement else {
                panic!("imported resume returns Done")
            };
            assert!(matches!(
                engine.observe(owner, value).expect("typed JSON"),
                HaskellValue::Con(DataConId(100), _)
            ));
            assert!(engine.release(value));
        }

        let registry = Arc::new(ImageRegistry::new());
        let (mut first, first_program) = PreparedEngine::bootstrap_shared(
            json_mount_program_with_verb(true),
            RunOptions::default().nursery_bytes,
            Some(Arc::clone(&registry)),
        )
        .expect("first source instance");
        let (mut second, second_program) = PreparedEngine::bootstrap_shared(
            json_mount_program_with_verb(true),
            RunOptions::default().nursery_bytes,
            Some(registry),
        )
        .expect("second mutable instance of the same compiled image");
        assert!(Arc::ptr_eq(
            &first.programs[&first_program].definitions,
            &second.programs[&second_program].definitions
        ));
        let first_function = first
            .machine
            .retain_top(first_program, ValueId(1))
            .expect("first function");
        let second_function = second
            .machine
            .retain_top(second_program, ValueId(1))
            .expect("second function");
        let (mut receiver, _) = PreparedEngine::bootstrap(
            testing::prepare(testing::wire_program()).expect("receiver wire"),
        )
        .expect("unrelated receiver");
        let before = receiver.residency().programs;
        let before_installs = receiver.installs_since_major();
        let (first_root, imports) = receiver
            .import_parcel(
                first
                    .export_parcel(first_function.raw())
                    .expect("first parcel"),
                RealmId::ROOT,
            )
            .expect("first typed import");
        assert!(imports.is_empty());
        let first_owner = receiver
            .machine
            .owner_of_handle(
                receiver
                    .prepared_handle_of(first_root)
                    .expect("first handle"),
            )
            .expect("first exact installed instance");
        let (repeated_root, _) = receiver
            .import_parcel(
                first
                    .export_parcel(first_function.raw())
                    .expect("repeat parcel"),
                RealmId::ROOT,
            )
            .expect("repeated instance import");
        assert_eq!(receiver.residency().programs, before + 1);
        assert_eq!(
            receiver.installs_since_major(),
            before_installs + 1,
            "repeated exact instances do not count as installations"
        );
        let (second_root, _) = receiver
            .import_parcel(
                second
                    .export_parcel(second_function.raw())
                    .expect("second parcel"),
                RealmId::ROOT,
            )
            .expect("distinct mutable instance import");
        let second_owner = receiver
            .machine
            .owner_of_handle(
                receiver
                    .prepared_handle_of(second_root)
                    .expect("second handle"),
            )
            .expect("second exact installed instance");
        assert_ne!(first_owner, second_owner);
        assert_eq!(receiver.residency().programs, before + 2);
        assert_eq!(receiver.sites[&7].owner, first_owner);
        assert_eq!(
            receiver.constructor_replies[&DataConId(1)].owner,
            first_owner
        );
        assert_eq!(receiver.installs_since_major(), before_installs + 2);
        answer_json(&mut receiver, first_owner);
        assert!(receiver.discard_handle(first_root));
        assert!(receiver.discard_handle(repeated_root));
        receiver
            .quiesce_and_collect_now()
            .expect("retire only the first imported instance");
        assert!(!receiver.programs.contains_key(&first_owner));
        assert!(receiver.programs.contains_key(&second_owner));
        assert_eq!(receiver.sites[&7].owner, second_owner);
        assert_eq!(
            receiver.constructor_replies[&DataConId(1)].owner,
            second_owner
        );
        answer_json(&mut receiver, second_owner);

        assert!(receiver.discard_handle(second_root));

        let entry = first.programs[&first_program].entry;
        let self_parcel = first
            .export_parcel(first_function.raw())
            .expect("already installed parcel");
        let (self_root, _) = first
            .import_parcel(self_parcel, RealmId::ROOT)
            .expect("existing instance report");
        assert_eq!(
            first.programs[&first_program].entry, entry,
            "repeated import preserves the owner's admitted entry metadata"
        );
        assert!(first.discard_handle(self_root));

        let (mut conflicting, _) = PreparedEngine::bootstrap(typed_site_program(
            7,
            closed_reply_type(DeclarationForm::Integer),
            0,
            &[],
        ))
        .expect("different site owner");
        let before = conflicting.residency();
        assert!(matches!(
            conflicting.import_parcel(
                first
                    .export_parcel(first_function.raw())
                    .expect("conflicting parcel"),
                RealmId::ROOT,
            ),
            Err(PreparedRuntimeError::SiteConflict { site: 7, .. })
        ));
        assert_eq!(
            conflicting.residency(),
            before,
            "typed preflight precedes native mutation"
        );
    }

    #[test]
    fn framed_structural_prefix_rejects_without_consuming_then_retries() {
        struct RawInt(i64);
        impl tidepool_bridge::sealed::ToHaskellSealed for RawInt {}
        impl ToHaskell for RawInt {
            fn visit(
                &self,
                _table: &DataConTable,
                visitor: &mut dyn tidepool_bridge::HaskellVisitor,
            ) -> Result<(), BridgeError> {
                visitor.literal(Literal::LitInt(self.0))
            }
        }

        let (mut engine, program) =
            PreparedEngine::bootstrap(json_mount_program()).expect("bootstrap framed fixture");
        let table = json_mount_table();
        let make_null = |engine: &mut PreparedEngine| {
            let mut builder = engine
                .machine
                .managed_builder()
                .expect("framed fixture opens a managed builder");
            let root = builder
                .constructor(DataConId(105), &[])
                .expect("fixture Null constructor is declared");
            builder
                .finish(RealmId::ROOT, root)
                .expect("fixture value is retained")
        };
        let continuation = make_null(&mut engine);
        let held = make_null(&mut engine);
        let id = engine
            .machine
            .park(
                continuation,
                RealmId::ROOT,
                None,
                ParkRequest {
                    principal: PrincipalId::SYSTEM,
                    effect_policy: EffectRunPolicy::SuspendAll,
                    live_payload: LivePayloadPolicy::None,
                    evidence: PreparedFrameEvidence {
                        reply: PreparedReplyEvidence::AtSite {
                            owner: program,
                            row: 1,
                        },
                        runner: program,
                        resume_entry: ValueId(1),
                        continuation_rep: RuntimeRep::LiftedRef,
                    },
                },
            )
            .expect("fixture continuation parks");
        let roots_before = engine.persistent_roots_count();
        let handles_before = engine.handle_count();

        let error = match engine.resume_with_framed_handle_sources(
            id,
            held.raw(),
            DataConId(903),
            &[],
            &table,
        ) {
            Err(error) => error,
            Ok(_) => panic!("missing prefix must be rejected"),
        };
        assert!(matches!(error, PreparedRuntimeError::AnswerShape { .. }));
        assert_eq!(engine.parked_count(), 1);
        assert_eq!(engine.persistent_roots_count(), roots_before);
        assert_eq!(engine.handle_count(), handles_before);

        let wrong: Vec<Box<dyn ToHaskell + Send>> = vec![Box::new("not an Int".to_owned())];
        let error = match engine.resume_with_framed_handle_sources(
            id,
            held.raw(),
            DataConId(903),
            &wrong,
            &table,
        ) {
            Err(error) => error,
            Ok(_) => panic!("wrong prefix shape must be rejected"),
        };
        assert!(matches!(error, PreparedRuntimeError::AnswerRejected { .. }));
        assert_eq!(engine.parked_count(), 1);
        assert_eq!(engine.persistent_roots_count(), roots_before);
        assert_eq!(engine.handle_count(), handles_before);

        let prefix: Vec<Box<dyn ToHaskell + Send>> = vec![Box::new(RawInt(37))];
        let resumed = engine
            .resume_with_framed_handle_sources(id, held.raw(), DataConId(903), &prefix, &table)
            .expect("valid prefix retries the exact parked frame");
        let PreparedSettlement::Done { value } = resumed.settlement else {
            panic!("framed fixture returns a settled Done value")
        };
        assert!(matches!(
            engine.observe(program, value).expect("observe framed reply"),
            HaskellValue::Con(DataConId(903), ref fields)
                if matches!(fields.as_slice(), [HaskellValue::Lit(Literal::LitInt(37)), HaskellValue::Con(DataConId(105), nested)] if nested.is_empty())
        ));
        assert!(engine.release(value));
        assert!(engine.release(held));
        assert_eq!(engine.parked_count(), 0);
    }

    fn closed_reply_type(form: DeclarationForm) -> Arc<TypeGraph> {
        prepared_data::closed_type_graph(testing::identity("Fixture.Types", "Reply"), form)
    }

    fn polymorphic_reply_type(rendered: &str, body_is_bound: bool) -> Arc<TypeGraph> {
        let mut kind = testing::identity("Fixture.Types", "Type");
        kind.namespace = "type".into();
        let mut integer = testing::identity(INTEGER_MODULE, "Integer");
        integer.namespace = "type".into();
        let mut nodes = vec![
            TypeNode::Root {
                domain: RootDomain::Closed,
                binders: vec![],
                rendered: rendered.into(),
            },
            TypeNode::ForAll(ForAllFlag::Specified),
            TypeNode::Declaration {
                identity: kind,
                parameters: vec![],
                form: DeclarationForm::Opaque {
                    head_kind: NominalHeadKind::Constructor,
                    reason: "kind".into(),
                },
                restriction: SyntaxRestriction::None,
            },
            TypeNode::NominalApplication,
            TypeNode::Bound(0),
        ];
        let mut edges = vec![
            (0, 1, TypeEdge::Body),
            (1, 3, TypeEdge::Kind),
            (1, 4, TypeEdge::Body),
            (3, 2, TypeEdge::Head),
        ];
        if !body_is_bound {
            nodes[4] = TypeNode::NominalApplication;
            nodes.push(TypeNode::Declaration {
                identity: integer,
                parameters: vec![],
                form: DeclarationForm::Integer,
                restriction: SyntaxRestriction::None,
            });
            edges.push((4, 5, TypeEdge::Head));
        }
        prepared_data::type_graph(nodes, &edges, &[]).expect("finite polymorphic reply type")
    }

    fn response_result_types(
        forms: &[DeclarationForm],
        argument: usize,
        family: &SymbolIdentity,
    ) -> Arc<TypeGraph> {
        let mut nodes = (0..=forms.len())
            .map(|_| TypeNode::Root {
                domain: RootDomain::Closed,
                binders: vec![],
                rendered: "request fixture".into(),
            })
            .collect::<Vec<_>>();
        let mut edges = Vec::new();
        let mut expressions = Vec::new();
        for (root, form) in forms.iter().enumerate() {
            let name = match form {
                DeclarationForm::Text => "Text",
                DeclarationForm::Integer => "Integer",
                DeclarationForm::Natural => "Natural",
                _ => panic!("request fixture leaf"),
            };
            let mut identity = testing::identity("Fixture.Types", name);
            identity.namespace = "type".into();
            let declaration = nodes.len() as u32;
            nodes.push(TypeNode::Declaration {
                identity,
                parameters: vec![],
                form: form.clone(),
                restriction: SyntaxRestriction::None,
            });
            let expression = nodes.len() as u32;
            nodes.push(TypeNode::NominalApplication);
            expressions.push(expression);
            edges.extend([
                (root as u32, expression, TypeEdge::Body),
                (expression, declaration, TypeEdge::Head),
            ]);
        }
        let declaration = nodes.len() as u32;
        nodes.push(TypeNode::Declaration {
            identity: family.clone(),
            parameters: vec![ParameterFlag::AnonymousVisible],
            form: DeclarationForm::Opaque {
                head_kind: NominalHeadKind::Constructor,
                reason: "abstract request result".into(),
            },
            restriction: SyntaxRestriction::None,
        });
        let kind = nodes.len() as u32;
        nodes.push(TypeNode::Literal(TypeLiteral::Symbol("kind".into())));
        let expression = nodes.len() as u32;
        nodes.push(TypeNode::NominalApplication);
        edges.extend([
            (declaration, kind, TypeEdge::BinderKind(0)),
            (forms.len() as u32, expression, TypeEdge::Body),
            (expression, declaration, TypeEdge::Head),
            (expression, expressions[argument], TypeEdge::Argument(0)),
        ]);
        prepared_data::type_graph(nodes, &edges, &[]).expect("finite request input and reply types")
    }

    /// An effect constructor with immutable static reply evidence. `revision`
    /// distinguishes native fixture images without manufacturing reply sites.
    fn verb_program(revision: u64, reply: Arc<TypeGraph>) -> PreparedProgram {
        let mut wire = testing::wire_program();
        wire.constructors = vec![ConstructorDecl {
            identity: testing::identity("Fixture.Effects", "Print"),
            family: testing::identity("Fixture.Effects", "Console"),
            result_rep: RuntimeRep::LiftedRef,
            field_reps: vec![],
            strict_fields: vec![],
            layout: CheckedLayout {
                fields: vec![],
                alignment: 1,
                payload_size: 0,
                root_mask: vec![],
            },
            tag: 1,
            family_size: 1,
            host_id: DataConId(77),
        }];
        wire.types = reply;
        if let Group::NonRecursive(top) = &mut wire.bindings[0] {
            top.identity.occurrence = format!("entry_{revision}");
        }
        wire.constructor_replies =
            vec![(ConstructorId(0), ConstructorReply::Static(TypeNodeId(0)))];
        testing::prepare(wire).expect("verb fixture validates")
    }

    fn typed_site_program(
        site: u64,
        types: Arc<TypeGraph>,
        wire: u32,
        inputs: &[u32],
    ) -> PreparedProgram {
        let mut program = testing::wire_program();
        program.types = types;
        program.sites = vec![SiteRow {
            site,
            origin: "Fixture.Request".into(),
            ordinal: 0,
            delivery: SiteDelivery::HostAnswer,
            wire: TypeNodeId(wire),
            inputs: inputs.iter().copied().map(TypeNodeId).collect(),
        }];
        testing::prepare(program).expect("typed site fixture validates")
    }

    #[test]
    fn site_type_evidence_commitment_covers_graph_and_endpoints() {
        let constructor = mount_constructor(
            "Fixture.Types",
            "ReplyValue",
            "Fixture.Types",
            "Reply",
            170,
            1,
            1,
            vec![RuntimeRep::LiftedRef],
        );
        let mut text = testing::identity("Fixture.Types", "Text");
        text.namespace = "type".into();
        let types = prepared_data::type_graph(
            vec![
                TypeNode::Root {
                    domain: RootDomain::Closed,
                    binders: vec![],
                    rendered: "Text".into(),
                },
                TypeNode::Root {
                    domain: RootDomain::Closed,
                    binders: vec![],
                    rendered: "Reply Text".into(),
                },
                TypeNode::Declaration {
                    identity: text,
                    parameters: vec![],
                    form: DeclarationForm::Text,
                    restriction: SyntaxRestriction::None,
                },
                TypeNode::Declaration {
                    identity: constructor.family.clone(),
                    parameters: vec![ParameterFlag::AnonymousVisible],
                    form: DeclarationForm::Data,
                    restriction: SyntaxRestriction::None,
                },
                TypeNode::NominalApplication,
                TypeNode::NominalApplication,
                TypeNode::ConstructorTemplate {
                    constructor: ConstructorId(0),
                    identity: constructor.identity.clone(),
                },
                TypeNode::Bound(0),
                TypeNode::Literal(TypeLiteral::Symbol("kind".into())),
            ],
            &[
                (3, 8, TypeEdge::BinderKind(0)),
                (0, 4, TypeEdge::Body),
                (1, 5, TypeEdge::Body),
                (4, 2, TypeEdge::Head),
                (5, 3, TypeEdge::Head),
                (5, 4, TypeEdge::Argument(0)),
                (3, 6, TypeEdge::Constructor(constructor.tag)),
                (
                    6,
                    7,
                    TypeEdge::Field {
                        ordinal: 0,
                        source_rep: RuntimeRep::LiftedRef,
                    },
                ),
            ],
            std::slice::from_ref(&constructor),
        )
        .expect("canonical site graph fixture");
        let evidence = SiteTypeEvidence {
            types,
            constructors: vec![(
                constructor.identity.clone(),
                constructor.host_id,
                constructor.family.clone(),
            )]
            .into(),
            input: TypeNodeId(0),
            answer: TypeNodeId(1),
            request_context: None,
        };
        let commitment = evidence.commitment();
        assert_eq!(commitment, evidence.clone().commitment());
        assert!(evidence.request_type_signatures().is_none());
        for helper in [
            tidepool_toolchain::declaration_join::RequestHelperRecipe::None,
            tidepool_toolchain::declaration_join::RequestHelperRecipe::ActorReply,
        ] {
            assert!(RequestCompileAnnotations::new(Arc::new(evidence.clone()), helper).is_err());
        }
        assert!(
            evidence.compile_context(None).is_err(),
            "public type graphs cannot authenticate native request signatures"
        );

        let mut renamed = evidence.clone();
        let mut storage = renamed.types.graph().clone();
        let TypeNode::Root { rendered, .. } =
            &mut storage[tidepool_repr::type_graph::TypeNodeId::new(1)]
        else {
            panic!("fixture reply root");
        };
        *rendered = "diagnostic name changed".into();
        renamed.types = Arc::new(
            TypeGraph::validate(
                storage,
                std::slice::from_ref(&constructor),
                GraphLimits::default(),
            )
            .expect("renamed graph preserves structural evidence"),
        );
        assert_eq!(evidence, renamed);
        assert_ne!(
            evidence.types, renamed.types,
            "artifact content retains diagnostic bytes"
        );
        assert_eq!(commitment, renamed.commitment());
        let mut relocated = evidence.clone();
        Arc::make_mut(&mut relocated.constructors)[0].1 = DataConId(999);
        assert_eq!(evidence, relocated);
        assert_eq!(commitment, relocated.commitment());

        let mut changed_template = evidence.clone();
        let mut storage = changed_template.types.graph().clone();
        let field = tidepool_repr::type_graph::TypeNodeId::new(7);
        storage[field] = TypeNode::NominalApplication;
        let mut integer = testing::identity(INTEGER_MODULE, "Integer");
        integer.namespace = "type".into();
        let declaration = storage.add_node(TypeNode::Declaration {
            identity: integer,
            parameters: vec![],
            form: DeclarationForm::Integer,
            restriction: SyntaxRestriction::None,
        });
        storage.add_edge(field, declaration, TypeEdge::Head);
        changed_template.types = Arc::new(
            TypeGraph::validate(
                storage,
                std::slice::from_ref(&constructor),
                GraphLimits::default(),
            )
            .expect("changed finite field template"),
        );
        assert_ne!(evidence, changed_template);
        assert_ne!(commitment, changed_template.commitment());
        let mut changed_family = evidence.clone();
        let mut storage = changed_family.types.graph().clone();
        let mut renamed_constructor = constructor.clone();
        renamed_constructor.family.occurrence = "OtherReply".into();
        let TypeNode::Declaration { identity, .. } =
            &mut storage[tidepool_repr::type_graph::TypeNodeId::new(3)]
        else {
            panic!("fixture nominal declaration");
        };
        *identity = renamed_constructor.family.clone();
        changed_family.types = Arc::new(
            TypeGraph::validate(
                storage,
                std::slice::from_ref(&renamed_constructor),
                GraphLimits::default(),
            )
            .expect("changed nominal family"),
        );
        Arc::make_mut(&mut changed_family.constructors)[0].2 = renamed_constructor.family;
        assert_ne!(evidence, changed_family);
        assert_ne!(commitment, changed_family.commitment());

        let mut changed_endpoint = evidence.clone();
        changed_endpoint.answer = TypeNodeId(0);
        assert_ne!(commitment, changed_endpoint.commitment());

        let mut changed_constructor = evidence;
        Arc::make_mut(&mut changed_constructor.constructors)[0]
            .0
            .occurrence = "OtherReplyValue".into();
        assert_ne!(commitment, changed_constructor.commitment());
    }

    #[test]
    fn installed_sites_compare_request_input_and_reply_across_programs() {
        let mut response_family =
            testing::identity("Tidepool.Agent.Reply.Internal", "ResponseResult");
        response_family.namespace = "type".into();
        let (source, _) = PreparedEngine::bootstrap(typed_site_program(
            41,
            response_result_types(
                &[DeclarationForm::Text, DeclarationForm::Integer],
                1,
                &response_family,
            ),
            2,
            &[0],
        ))
        .expect("install request site");
        let evidence = source
            .request_site_type_evidence(41)
            .expect("capture source site before crossing sessions");
        let source_facts = &source.programs[&source.sites[&41].owner];
        assert!(Arc::ptr_eq(&evidence.types, &source_facts.types));
        assert!(Arc::ptr_eq(
            &evidence.constructors,
            &source_facts.constructors
        ));
        let (recipient, _) = PreparedEngine::bootstrap(typed_site_program(
            42,
            response_result_types(
                &[
                    DeclarationForm::Integer,
                    DeclarationForm::Natural,
                    DeclarationForm::Text,
                ],
                0,
                &response_family,
            ),
            1,
            &[2, 0, 3],
        ))
        .expect("install accessor in a different machine");

        assert!(recipient
            .request_scope_types_match(&evidence, 42)
            .expect("bounded request compatibility"));
        assert!(!recipient
            .request_scope_types_match(&evidence, 41)
            .expect("bounded request compatibility"));
        let (wrong_input, _) = PreparedEngine::bootstrap(typed_site_program(
            44,
            response_result_types(
                &[DeclarationForm::Integer, DeclarationForm::Text],
                0,
                &response_family,
            ),
            0,
            &[0, 0, 2],
        ))
        .expect("install wrong-input accessor");
        assert!(!wrong_input
            .request_scope_types_match(&evidence, 44)
            .expect("bounded request compatibility"));
        let (wrong, _) = PreparedEngine::bootstrap(typed_site_program(
            43,
            prepared_data::closed_type_roots(&[
                (
                    testing::identity("Fixture.Types", "Text"),
                    DeclarationForm::Text,
                ),
                (
                    testing::identity("Fixture.Types", "Integer"),
                    DeclarationForm::Integer,
                ),
            ]),
            1,
            &[0, 0, 1],
        ))
        .expect("install wrong accessor");
        assert!(!wrong
            .request_scope_types_match(&evidence, 43)
            .expect("bounded request compatibility"));
    }

    fn attested_request_program(reply: ConstructorReply) -> PreparedProgram {
        let mut wire = testing::wire_program();
        wire.constructors = vec![mount_constructor(
            "Fixture",
            "Request",
            "Fixture",
            "Effect",
            77,
            1,
            1,
            vec![RuntimeRep::Int(64)],
        )];
        wire.types = polymorphic_reply_type("forall a. a", true);
        wire.constructor_replies = vec![(ConstructorId(0), reply)];
        wire.sites = vec![SiteRow {
            site: 41,
            origin: "Fixture.site".into(),
            ordinal: 0,
            delivery: SiteDelivery::LiveReentry,
            wire: TypeNodeId(0),
            inputs: vec![],
        }];
        testing::prepare(wire).unwrap()
    }

    fn park_attested_fixture(
        engine: &mut PreparedEngine,
        program: ProgramId,
        reply: PreparedReplyEvidence,
    ) -> ContinuationId {
        let continuation = engine.machine.retain_top(program, ValueId(0)).unwrap();
        engine
            .machine
            .park(
                continuation,
                RealmId::ROOT,
                None,
                ParkRequest {
                    principal: PrincipalId::SYSTEM,
                    effect_policy: EffectRunPolicy::SuspendAll,
                    live_payload: LivePayloadPolicy::None,
                    evidence: PreparedFrameEvidence {
                        reply,
                        runner: program,
                        resume_entry: ValueId(0),
                        continuation_rep: RuntimeRep::LiftedRef,
                    },
                },
            )
            .unwrap()
    }

    #[test]
    fn static_reply_ignores_leading_int_and_nested_typed_site_payload() {
        for (leading, payload) in [
            (8, serde_json::Value::Null),
            (0, serde_json::json!({"payload": {"typedSite": 8}})),
        ] {
            let (mut engine, owner, parked) = park_json_fixture_request(
                Some(ConstructorReply::Static(TypeNodeId(0))),
                leading,
                payload,
            );
            let parked = parked.unwrap();
            assert_eq!(
                engine.parked(parked.id).unwrap().1.reply,
                PreparedReplyEvidence::Static {
                    owner,
                    constructor: DataConId(903),
                    node: TypeNodeId(0),
                }
            );
            assert_eq!(engine.parked_site(parked.id), None);
            assert!(matches!(
                engine.classify_reply(
                    &HaskellValue::Con(DataConId(999), vec![HaskellValue::Lit(Literal::LitInt(8))]),
                    &json_mount_table(),
                ),
                Err(PreparedRuntimeError::MissingReplyEvidence {
                    constructor: DataConId(999)
                })
            ));
            engine.abort_parked(parked.id).unwrap();
            assert_eq!(engine.parked_count(), 0);
            assert_eq!(engine.handle_count(), 0);
        }
    }

    fn nonleading_site_program(capture_input: Option<u32>) -> PreparedProgram {
        let prepared = attested_request_program(ConstructorReply::Static(TypeNodeId(0)));
        let mut wire = prepared_data::wire_from_prepared(&prepared);
        wire.constructors[0] = mount_constructor(
            "Fixture",
            "Request",
            "Fixture",
            "Effect",
            77,
            1,
            1,
            vec![
                RuntimeRep::Int(64),
                RuntimeRep::Int(64),
                RuntimeRep::LiftedRef,
            ],
        );
        wire.constructor_replies[0].1 = ConstructorReply::StaticWithSite {
            reply: TypeNodeId(0),
            field: 1,
            payload_field: 2,
            capture_input,
        };
        wire.sites[0].inputs = vec![TypeNodeId(0)];
        testing::prepare(wire).unwrap()
    }

    #[test]
    fn nonleading_site_preserves_closed_reply_and_requires_exact_capture_vector() {
        let (mut engine, owner) =
            PreparedEngine::bootstrap(nonleading_site_program(Some(0))).unwrap();
        let table = json_mount_table();
        let request = |site| {
            HaskellValue::Con(
                DataConId(77),
                vec![
                    HaskellValue::Lit(Literal::LitInt(999)),
                    site,
                    HaskellValue::Con(DataConId(105), vec![]),
                ],
            )
        };
        for malformed in [
            HaskellValue::Lit(Literal::LitInt(-1)),
            HaskellValue::Lit(Literal::LitWord(41)),
        ] {
            assert!(matches!(
                engine.classify_reply(&request(malformed), &table),
                Err(PreparedRuntimeError::MalformedRequestSite { .. })
            ));
        }
        assert!(matches!(
            engine.classify_reply(&request(HaskellValue::Lit(Literal::LitInt(42))), &table),
            Err(PreparedRuntimeError::UnknownSite { site: 42 })
        ));
        let valid = request(HaskellValue::Lit(Literal::LitInt(41)));
        let reply = engine.classify_reply(&valid, &table).unwrap();
        let (reply_owner, node, target) = engine.structural_reply(reply).unwrap();
        assert_eq!(reply_owner, owner);
        assert_eq!(node, TypeNodeId(0));
        assert!(matches!(target, ReplyTarget::Static(DataConId(77))));
        let id = park_attested_fixture(&mut engine, owner, reply);
        assert_eq!(engine.parked_site(id), Some(41));
        assert_eq!(engine.parked_capture_site(id), Some((41, 0)));
        engine.abort_parked(id).unwrap();
        let mut missing = prepared_data::wire_from_prepared(&nonleading_site_program(Some(0)));
        missing.sites[0].inputs.clear();
        let (engine, _) = PreparedEngine::bootstrap(testing::prepare(missing).unwrap()).unwrap();
        assert!(matches!(
            engine.classify_reply(&valid, &table),
            Err(PreparedRuntimeError::MalformedRequestSite { .. })
        ));
    }

    #[test]
    fn nonleading_original_site_without_typed_payload_never_issues_capture() {
        let (mut engine, owner) = PreparedEngine::bootstrap(nonleading_site_program(None)).unwrap();
        let request = HaskellValue::Con(
            DataConId(77),
            vec![
                HaskellValue::Lit(Literal::LitInt(999)),
                HaskellValue::Lit(Literal::LitInt(41)),
                HaskellValue::Con(DataConId(105), vec![]),
            ],
        );
        let reply = engine
            .classify_reply(&request, &json_mount_table())
            .unwrap();
        let id = park_attested_fixture(&mut engine, owner, reply);
        assert_eq!(engine.parked_site(id), Some(41));
        assert_eq!(engine.parked_capture_site(id), None);
        engine.abort_parked(id).unwrap();
    }

    #[test]
    fn at_site_requires_exact_first_carrier_and_checks_delivery() {
        let (mut engine, owner) =
            PreparedEngine::bootstrap(attested_request_program(ConstructorReply::AtSite)).unwrap();
        let json_fixture = json_mount_program();
        let json_layout = json_fixture
            .json_layout()
            .unwrap()
            .try_map(|constructor| {
                json_fixture
                    .constructors()
                    .get(constructor.0 as usize)
                    .map(|declaration| declaration.host_id)
                    .ok_or(())
            })
            .unwrap();
        let table = json_mount_table().with_json_layout(Some(json_layout));
        for fields in [
            vec![],
            vec![HaskellValue::Lit(Literal::LitInt(-1))],
            vec![HaskellValue::Lit(Literal::LitWord(41))],
            vec![serde_json::json!({"typedSite": 41})
                .to_value(&table)
                .unwrap()],
        ] {
            assert!(matches!(
                engine.classify_reply(&HaskellValue::Con(DataConId(77), fields), &table),
                Err(PreparedRuntimeError::MalformedRequestSite { .. })
            ));
        }
        assert!(matches!(
            engine.classify_reply(
                &HaskellValue::Con(DataConId(77), vec![HaskellValue::Lit(Literal::LitInt(42))]),
                &table
            ),
            Err(PreparedRuntimeError::UnknownSite { site: 42 })
        ));
        let reply = engine
            .classify_reply(
                &HaskellValue::Con(DataConId(77), vec![HaskellValue::Lit(Literal::LitInt(41))]),
                &table,
            )
            .unwrap();
        let id = park_attested_fixture(&mut engine, owner, reply);
        assert_eq!(engine.parked_site(id), Some(41));
        assert!(matches!(
            engine.resume_with_structural_answer(
                id,
                &HaskellValue::Con(DataConId(105), vec![]),
                &table
            ),
            Err(PreparedRuntimeError::AnswerDelivery {
                site: 41,
                delivery: SiteDelivery::LiveReentry
            })
        ));
        assert_eq!(engine.parked_count(), 1);
        engine.abort_parked(id).unwrap();
    }

    #[test]
    fn retained_handle_resume_rejects_unlifted_representation_before_consuming_frame() {
        let (mut engine, owner) = PreparedEngine::bootstrap(attested_request_program(
            ConstructorReply::Static(TypeNodeId(0)),
        ))
        .unwrap();
        let reply = PreparedReplyEvidence::Static {
            owner,
            constructor: DataConId(77),
            node: TypeNodeId(0),
        };
        let id = park_attested_fixture(&mut engine, owner, reply);
        let mut wire = testing::wire_program();
        let mut constructor = mount_constructor(
            "Fixture",
            "Unlifted",
            "Fixture",
            "Unlifted",
            80,
            1,
            1,
            vec![],
        );
        constructor.result_rep = RuntimeRep::UnliftedRef;
        wire.constructors = vec![constructor];
        wire.signatures[0].results = ResultContract::Returns(vec![RuntimeRep::UnliftedRef]);
        wire.expressions.nodes[0] = ExprFrame::Construct {
            constructor: ConstructorId(0),
            fields: vec![],
        };
        let producer = engine
            .install(
                testing::prepare(wire).unwrap(),
                &BindingTable::new(),
                &BindingIndex::new(),
            )
            .unwrap();
        let batch = engine
            .machine
            .run_entry_retained(producer, ValueId(0), &[], SETTLE_CALL, RealmId::ROOT)
            .unwrap();
        let [PreparedResult::Managed(answer)] = batch.values.as_slice() else {
            panic!("unlifted constructor returns one managed value")
        };
        let answer = *answer;
        assert_eq!(answer.rep(), RuntimeRep::UnliftedRef);
        assert!(matches!(
            engine.resume_with_handle(id, answer.raw()),
            Err(PreparedRuntimeError::AnswerRepresentation {
                actual: RuntimeRep::UnliftedRef
            })
        ));
        assert_eq!(engine.parked_count(), 1);
        assert!(
            engine.prepared_handle_of(answer.raw()).is_some(),
            "borrowed refusal keeps the caller's root"
        );
        assert!(matches!(
            engine.resume_parked(id, answer),
            Err(PreparedRuntimeError::AnswerRepresentation {
                actual: RuntimeRep::UnliftedRef
            })
        ));
        assert_eq!(engine.parked_count(), 1);
        assert!(
            engine.prepared_handle_of(answer.raw()).is_none(),
            "owned refusal releases the supplied answer"
        );
        engine.abort_parked(id).unwrap();
    }

    #[test]
    fn unconstructible_static_reply_rejects_nullary_and_structural_values_without_consuming_frame()
    {
        let (mut engine, owner) = PreparedEngine::bootstrap(attested_request_program(
            ConstructorReply::Static(TypeNodeId(0)),
        ))
        .unwrap();
        let reply = PreparedReplyEvidence::Static {
            owner,
            constructor: DataConId(77),
            node: TypeNodeId(0),
        };
        let id = park_attested_fixture(&mut engine, owner, reply);
        let roots = engine.persistent_roots_count();
        let table = json_mount_table();
        for value in [
            HaskellValue::Con(DataConId(105), vec![]),
            HaskellValue::Con(
                DataConId(103),
                vec![HaskellValue::Con(DataConId(130), vec![])],
            ),
            HaskellValue::Lit(Literal::LitInt(41)),
        ] {
            assert!(engine
                .resume_with_structural_answer(id, &value, &table)
                .is_err());
            assert_eq!(engine.parked_count(), 1);
            assert_eq!(engine.persistent_roots_count(), roots);
        }
        engine.abort_parked(id).unwrap();
    }

    #[test]
    fn polymorphic_static_maybe_graph_accepts_nothing_only() {
        let mut wire = testing::wire_program();
        wire.constructors = vec![
            mount_constructor("Fixture", "Request", "Fixture", "Effect", 77, 1, 1, vec![]),
            mount_constructor(
                "GHC.Maybe",
                "Nothing",
                "GHC.Maybe",
                "Maybe",
                78,
                1,
                2,
                vec![],
            ),
            mount_constructor(
                "GHC.Maybe",
                "Just",
                "GHC.Maybe",
                "Maybe",
                79,
                2,
                2,
                vec![RuntimeRep::LiftedRef],
            ),
        ];
        let mut kind = testing::identity("Fixture.Types", "Type");
        kind.namespace = "type".into();
        wire.types = prepared_data::type_graph(
            vec![
                TypeNode::Root {
                    domain: RootDomain::ConstructorScheme,
                    binders: vec![SourceBinderFlag::Specified],
                    rendered: "Maybe state".into(),
                },
                TypeNode::Declaration {
                    identity: wire.constructors[1].family.clone(),
                    parameters: vec![ParameterFlag::AnonymousVisible],
                    form: DeclarationForm::Data,
                    restriction: SyntaxRestriction::None,
                },
                TypeNode::Declaration {
                    identity: kind,
                    parameters: vec![],
                    form: DeclarationForm::Opaque {
                        head_kind: NominalHeadKind::Constructor,
                        reason: "kind".into(),
                    },
                    restriction: SyntaxRestriction::None,
                },
                TypeNode::NominalApplication,
                TypeNode::Bound(0),
                TypeNode::NominalApplication,
                TypeNode::ConstructorTemplate {
                    constructor: ConstructorId(1),
                    identity: wire.constructors[1].identity.clone(),
                },
                TypeNode::ConstructorTemplate {
                    constructor: ConstructorId(2),
                    identity: wire.constructors[2].identity.clone(),
                },
            ],
            &[
                (0, 3, TypeEdge::Body),
                (0, 5, TypeEdge::BinderKind(0)),
                (1, 5, TypeEdge::BinderKind(0)),
                (3, 1, TypeEdge::Head),
                (3, 4, TypeEdge::Argument(0)),
                (5, 2, TypeEdge::Head),
                (1, 6, TypeEdge::Constructor(wire.constructors[1].tag)),
                (1, 7, TypeEdge::Constructor(wire.constructors[2].tag)),
                (
                    7,
                    4,
                    TypeEdge::Field {
                        ordinal: 0,
                        source_rep: RuntimeRep::LiftedRef,
                    },
                ),
            ],
            &wire.constructors,
        )
        .expect("polymorphic Maybe constructor templates");
        wire.constructor_replies =
            vec![(ConstructorId(0), ConstructorReply::Static(TypeNodeId(0)))];
        let mut table = DataConTable::new();
        table
            .extend_checked(wire.constructors[1..].iter().map(mount_table_row))
            .unwrap();
        let (mut engine, owner) =
            PreparedEngine::bootstrap(testing::prepare(wire).unwrap()).unwrap();
        let reply = PreparedReplyEvidence::Static {
            owner,
            constructor: DataConId(77),
            node: TypeNodeId(0),
        };
        let id = park_attested_fixture(&mut engine, owner, reply);
        let roots = engine.persistent_roots_count();
        let failure = match engine.resume_with_structural_answer(
            id,
            &HaskellValue::Con(
                DataConId(79),
                vec![HaskellValue::Con(DataConId(78), vec![])],
            ),
            &table,
        ) {
            Err(failure) => failure,
            Ok(_) => panic!("Just demands a value for its universally quantified field"),
        };
        assert!(
            matches!(
                &failure,
                PreparedRuntimeError::AnswerUnconstructible {
                    site: ReplyTarget::Static(DataConId(77)),
                    reason,
                } if reason == &tidepool_repr::type_graph::ConstructionRefusal::Polymorphic.to_string()
            ),
            "{failure:?}"
        );
        assert_eq!(engine.parked_count(), 1);
        assert_eq!(engine.persistent_roots_count(), roots);
        let (programs, machine) = (&engine.programs, &mut engine.machine);
        let mut builder = machine.managed_builder().unwrap();
        let node = build_structural_node(
            &HaskellValue::Con(DataConId(78), vec![]),
            &table,
            ReplyTarget::Static(DataConId(77)),
            TypeNodeId(0),
            &programs[&owner],
            &mut builder,
        )
        .unwrap();
        let value = builder.finish(RealmId::ROOT, node).unwrap();
        assert!(engine.release(value));
        engine.abort_parked(id).unwrap();
    }

    #[test]
    fn programs_declaring_the_same_effect_constructor_share_one_constructor_reply_witness() {
        let site = 5;
        let (mut engine, first) =
            PreparedEngine::bootstrap(verb_program(site, closed_reply_type(DeclarationForm::Text)))
                .expect("bootstrap");
        let bindings = BindingTable::new();
        let index = BindingIndex::new();
        let second = engine
            .install(
                verb_program(site, closed_reply_type(DeclarationForm::Text)),
                &bindings,
                &index,
            )
            .expect("an equivalent duplicate is not a SiteConflict");
        assert_ne!(first, second);
        let witness = engine.constructor_replies[&DataConId(77)];
        assert_eq!(witness.owner, first, "the existing owner stays canonical");

        // Different static type evidence refuses before any native mutation.
        let error = engine
            .install(
                verb_program(6, closed_reply_type(DeclarationForm::Integer)),
                &bindings,
                &index,
            )
            .expect_err("a conflicting verb reply refuses the install");
        match error {
            PreparedRuntimeError::ConstructorReplyConflict {
                owner, evidence, ..
            } => {
                assert_eq!(owner, first);
                assert!(matches!(
                    evidence.existing,
                    ConstructorReplyObservation::Static {
                        shape: ReplyTypeObservation::Text,
                        ..
                    }
                ));
                assert!(matches!(
                    evidence.incoming,
                    ConstructorReplyObservation::Static {
                        shape: ReplyTypeObservation::Integer,
                        ..
                    }
                ));
            }
            other => panic!("expected ConstructorReplyConflict, got {other:?}"),
        }
        assert_eq!(engine.programs.len(), 2);
    }

    #[test]
    fn admitted_site_and_verb_survive_multiple_and_final_retirement() {
        let routing = |revision| {
            let prepared = verb_program(revision, closed_reply_type(DeclarationForm::Text));
            let mut wire = prepared_data::wire_from_prepared(&prepared);
            wire.sites = vec![SiteRow {
                site: 7,
                origin: "Fixture.Routing".into(),
                ordinal: 0,
                delivery: SiteDelivery::HostAnswer,
                wire: TypeNodeId(0),
                inputs: vec![],
            }];
            testing::prepare(wire).expect("admitted routing fixture")
        };
        let (mut engine, first) = PreparedEngine::bootstrap(routing(101)).unwrap();
        let bindings = BindingTable::new();
        let index = BindingIndex::new();
        let second = engine.install(routing(102), &bindings, &index).unwrap();
        let third = engine.install(routing(103), &bindings, &index).unwrap();
        assert_eq!(engine.sites[&7].owner, first);
        assert_eq!(engine.constructor_replies[&DataConId(77)].owner, first);
        for export in std::mem::take(&mut engine.code_exports).into_values() {
            assert!(engine.release(export.handle));
        }
        assert!(engine.unpin(first));
        assert!(engine.unpin(second));
        engine.quiesce_and_collect_now().unwrap();
        assert!(!engine.programs.contains_key(&first));
        assert!(!engine.programs.contains_key(&second));
        assert_eq!(engine.sites[&7].owner, third);
        assert_eq!(engine.constructor_replies[&DataConId(77)].owner, third);
        assert!(engine.unpin(third));
        engine.quiesce_and_collect_now().unwrap();
        assert!(!engine.programs.contains_key(&third));
        assert!(!engine.sites.contains_key(&7));
        assert!(!engine.constructor_replies.contains_key(&DataConId(77)));
    }

    #[test]
    fn structural_graph_work_refusal_preserves_parked_frame_and_roots() {
        let (mut engine, owner, parked) = park_json_fixture_request(
            Some(ConstructorReply::Static(TypeNodeId(0))),
            0,
            serde_json::Value::Null,
        );
        let parked = parked.expect("fixture parks its authenticated request");
        let roots = engine.persistent_roots_count();
        let (programs, machine) = (&engine.programs, &mut engine.machine);
        let facts = &programs[&owner];
        let root = facts
            .types
            .open_root(
                TypeNodeId(0),
                &mut TypeWorkBudget::new(GraphLimits::default().max_work),
            )
            .unwrap();
        let mut builder = machine.managed_builder().unwrap();
        let mut visitor = StructuralAnswerVisitor {
            site: ReplyTarget::Static(DataConId(903)),
            root,
            budget: TypeWorkBudget::new(0),
            facts,
            builder: &mut builder,
            frames: vec![],
            result: None,
            failure: None,
            depth: 0,
        };
        assert!(visitor.begin_constructor(DataConId(105), 0).is_err());
        assert!(matches!(
            visitor.failure,
            Some(PreparedRuntimeError::AnswerTypeEvidence {
                source: TypeGraphError::TraversalWork,
                ..
            })
        ));
        assert!(visitor.result.is_none());
        let failure = visitor.failure.as_ref().unwrap();
        assert_eq!(failure.kind(), PreparedFailureKind::Rejected);
        assert_eq!(failure.stage(), PreparedFailureStage::Run);
        drop(visitor);
        drop(builder);
        assert_eq!(engine.persistent_roots_count(), roots);
        assert_eq!(engine.parked_count(), 1);
        assert_eq!(
            engine.parked(parked.id).unwrap().1.reply,
            PreparedReplyEvidence::Static {
                owner,
                constructor: DataConId(903),
                node: TypeNodeId(0)
            }
        );
        engine.abort_parked(parked.id).unwrap();
    }

    #[test]
    fn retained_callable_and_parked_frame_preserve_static_evidence_across_collection() {
        let (mut engine, first) =
            PreparedEngine::bootstrap(verb_program(5, closed_reply_type(DeclarationForm::Text)))
                .unwrap();
        let function = engine.machine.retain_top(first, ValueId(0)).unwrap();
        let second = engine
            .install(
                verb_program(6, closed_reply_type(DeclarationForm::Text)),
                &BindingTable::new(),
                &BindingIndex::new(),
            )
            .unwrap();
        assert!(engine.unpin(first));
        assert!(engine.unpin(second));
        for export in std::mem::take(&mut engine.code_exports).into_values() {
            assert!(engine.release(export.handle));
        }
        engine.quiesce_and_collect_now().unwrap();
        assert!(
            engine.programs.contains_key(&first),
            "the retained callable keeps its immutable facts alive"
        );
        assert!(!engine.programs.contains_key(&second));
        let reply = engine
            .classify_reply(
                &HaskellValue::Con(DataConId(77), vec![]),
                &DataConTable::new(),
            )
            .unwrap();
        assert_eq!(
            reply,
            PreparedReplyEvidence::Static {
                owner: first,
                constructor: DataConId(77),
                node: TypeNodeId(0)
            }
        );
        let realm = RealmId::fresh();
        let continuation = engine.machine.retain_handle_value(function, realm).unwrap();
        let id = engine
            .machine
            .park(
                continuation,
                realm,
                None,
                ParkRequest {
                    principal: PrincipalId::SYSTEM,
                    effect_policy: EffectRunPolicy::SuspendAll,
                    live_payload: LivePayloadPolicy::None,
                    evidence: PreparedFrameEvidence {
                        reply,
                        runner: first,
                        resume_entry: ValueId(0),
                        continuation_rep: continuation.rep(),
                    },
                },
            )
            .unwrap();
        assert!(engine.release(function));
        engine.quiesce_and_collect_now().unwrap();
        assert!(
            engine.programs.contains_key(&first),
            "the parked frame retains its evidence owner separately"
        );
        let (frame_realm, evidence) = engine.parked(id).unwrap();
        assert_eq!(frame_realm, realm);
        assert_eq!(evidence.reply, reply);
        assert_eq!(engine.close_realm(realm), (1, 0));
        assert_eq!(engine.parked_count(), 0);
        assert_eq!(engine.stowed_roots_count(), 0);
        engine.quiesce_and_collect_now().unwrap();
        assert!(!engine.programs.contains_key(&first));
        assert!(!engine.constructor_replies.contains_key(&DataConId(77)));
    }

    #[test]
    fn alpha_stable_polymorphic_reply_evidence_installs_without_weakening_conflicts() {
        let site = 7;
        let stable = polymorphic_reply_type("forall a. a", true);
        let (mut engine, first) =
            PreparedEngine::bootstrap(verb_program(site, stable.clone())).expect("bootstrap");
        let bindings = BindingTable::new();
        let index = BindingIndex::new();
        let second = engine
            .install(
                verb_program(
                    site,
                    polymorphic_reply_type("forall renamed. renamed", true),
                ),
                &bindings,
                &index,
            )
            .expect("alpha-stable polymorphic evidence is equivalent");
        assert_ne!(first, second);

        let different = polymorphic_reply_type("forall a. a", false);
        let error = engine
            .install(verb_program(site, different), &bindings, &index)
            .expect_err("different type evidence must remain a conflict");
        assert!(
            matches!(error, PreparedRuntimeError::ConstructorReplyConflict { owner, .. } if owner == first),
            "expected SiteConflict, got {error:?}"
        );
        assert_eq!(engine.programs.len(), 2);
    }

    #[test]
    fn off_checkout_install_reuses_code_already_installed_on_the_machine() {
        let registry = Arc::new(ImageRegistry::new());
        let (mut engine, _) =
            PreparedEngine::bootstrap(verb_program(40, closed_reply_type(DeclarationForm::Text)))
                .expect("bootstrap");
        engine.set_image_registry(Arc::clone(&registry));
        let bindings = BindingTable::new();
        let index = BindingIndex::new();
        let prepared = verb_program(41, closed_reply_type(DeclarationForm::Text));
        let mut snapshot = engine
            .snapshot_install(prepared.clone(), &bindings, &index)
            .expect("capture installation");
        let key = snapshot.linked.clone();
        let compiled =
            PreparedEngine::compile_off_checkout(&mut snapshot).expect("compile code off checkout");
        let shared = Arc::clone(&compiled);
        let first = engine
            .install(prepared, &bindings, &index)
            .expect("another actor installs shared code first");
        let second = engine
            .revalidate_and_install(snapshot, compiled, &bindings, &index)
            .expect("imports remain current")
            .expect("same code does not stale an installation");
        assert_ne!(first, second, "each installation owns a separate instance");
        for program in [first, second] {
            assert!(Arc::ptr_eq(
                &engine.programs[&program].definitions,
                shared.definition_facts(),
            ));
        }
        assert!(Arc::ptr_eq(
            &shared,
            &registry
                .lookup(&key)
                .expect("installed code remains shared")
        ));
    }

    #[test]
    fn off_checkout_install_rejects_intervening_evidence_conflict() {
        let mut engine = PreparedEngine::empty_certified(64 * 1024, None).unwrap();
        let bindings = BindingTable::new();
        let index = BindingIndex::new();
        let site = 42;
        let mut snapshot = engine
            .snapshot_install(
                verb_program(site, closed_reply_type(DeclarationForm::Text)),
                &bindings,
                &index,
            )
            .unwrap();
        let compiled = PreparedEngine::compile_off_checkout(&mut snapshot).unwrap();
        let owner = engine
            .install(
                verb_program(site, closed_reply_type(DeclarationForm::Integer)),
                &bindings,
                &index,
            )
            .unwrap();
        let before = engine.residency();
        assert!(matches!(
            engine.revalidate_and_install(snapshot, compiled, &bindings, &index),
            Err(PreparedRuntimeError::ConstructorReplyConflict { constructor: rejected, owner: actual, .. })
                if rejected == DataConId(77) && actual == owner
        ));
        assert_eq!(engine.residency(), before);
        assert_eq!(engine.programs.len(), 1);
        assert_eq!(engine.constructor_replies[&DataConId(77)].owner, owner);
    }

    #[test]
    fn off_checkout_install_restores_retired_evidence_ownership() {
        let site = 43;
        let prepared = verb_program(site, closed_reply_type(DeclarationForm::Text));
        let (mut engine, owner) = PreparedEngine::bootstrap(prepared.clone()).unwrap();
        let bindings = BindingTable::new();
        let index = BindingIndex::new();
        let mut snapshot = engine
            .snapshot_install(prepared, &bindings, &index)
            .unwrap();
        let compiled = PreparedEngine::compile_off_checkout(&mut snapshot).unwrap();
        assert!(engine.unpin(owner));
        for export in std::mem::take(&mut engine.code_exports).into_values() {
            assert!(engine.release(export.handle));
        }
        engine.quiesce_and_collect_now().unwrap();
        assert!(!engine.programs.contains_key(&owner));
        assert!(!engine.constructor_replies.contains_key(&DataConId(77)));
        let installed = engine
            .revalidate_and_install(snapshot, compiled, &bindings, &index)
            .unwrap()
            .unwrap();
        assert_eq!(engine.constructor_replies[&DataConId(77)].owner, installed);
    }

    #[test]
    fn two_engines_sharing_one_registry_the_second_install_is_a_registry_hit() {
        let registry = Arc::new(ImageRegistry::new());
        let bootstrap_site = 30;
        let (mut engine_a, _) = PreparedEngine::bootstrap(verb_program(
            bootstrap_site,
            closed_reply_type(DeclarationForm::Text),
        ))
        .expect("engine a bootstraps");
        let (mut engine_b, _) = PreparedEngine::bootstrap(verb_program(
            bootstrap_site,
            closed_reply_type(DeclarationForm::Text),
        ))
        .expect("engine b bootstraps its own, independent machine");
        engine_a.set_image_registry(Arc::clone(&registry));
        engine_b.set_image_registry(Arc::clone(&registry));

        let bindings = BindingTable::new();
        let index = BindingIndex::new();
        let shared_site = 31;

        engine_a
            .install(
                verb_program(shared_site, closed_reply_type(DeclarationForm::Text)),
                &bindings,
                &index,
            )
            .expect("engine a compiles and registers the image");
        assert_eq!(
            registry.misses(),
            1,
            "engine a's install is a registry miss"
        );
        assert_eq!(registry.hits(), 0);

        engine_b
            .install(
                verb_program(shared_site, closed_reply_type(DeclarationForm::Text)),
                &bindings,
                &index,
            )
            .expect("engine b installs the same content against its own machine");
        assert_eq!(
            registry.hits(),
            1,
            "engine b's install of the same linked program content is a registry hit"
        );
        assert_eq!(registry.misses(), 1, "no second compile happened");
    }
    #[test]
    fn staged_publications_preserve_real_binding_winners_after_private_retirement() {
        use tidepool_codegen::scope::ScopeId;
        use tidepool_repr::{Generation, SessionModule};

        let dir = tempfile::tempdir().unwrap();
        let manifest = dir.path().join("declarations.json");
        let mut lib = super::super::SessionLib::open(
            tidepool_repr::SessionId(81),
            dir.path().join("session"),
            super::super::ModuleEnv::standalone_default(),
        )
        .unwrap();
        tidepool_testing::with_settlement(|settlement| {
            lib.attach_recovery_graph_v2(&manifest, settlement)
        })
        .unwrap();
        let mut state = super::super::PersistentSession::new(Some(lib), 1024);
        let producer = producer_program();
        let top = producer.entry();
        let program = state.install_prepared(producer).unwrap();
        state
            .prepared_mut()
            .unwrap()
            .machine
            .run_entry(
                program,
                top,
                &[],
                PreparedCallOptions {
                    observation_budget: RunOptions::default().observation_budget,
                    collect_before_observation: true,
                },
                RealmId::ROOT,
            )
            .unwrap();
        let public_a = state.mint_scope(ScopeId::ROOT).unwrap();
        let public_b = state.mint_scope(ScopeId::ROOT).unwrap();
        let private_a = state.mint_detached_scope(public_a).unwrap();
        let private_b = state.mint_detached_scope(public_b).unwrap();
        let actor_a = super::super::RecoveryPublicOwner::new(
            &tidepool_repr::ActorPath::parse("root/a").unwrap(),
            1,
        )
        .unwrap();
        let actor_b = super::super::RecoveryPublicOwner::new(
            &tidepool_repr::ActorPath::parse("root/b").unwrap(),
            1,
        )
        .unwrap();
        state
            .bind_durable_public_scope(actor_a.clone(), public_a)
            .unwrap();
        state
            .bind_durable_public_scope(actor_b.clone(), public_b)
            .unwrap();

        for (name, id, scope) in [("a", 11, private_a), ("b", 12, private_b)] {
            let engine = state.prepared_mut().unwrap();
            let handle = engine.machine.retain_top(program, top).unwrap();
            assert!(engine.adopt(handle));
            state
                .bind_in(
                    scope,
                    BindingEntry {
                        name: tidepool_repr::BindingName(name.into()),
                        id: SessionVarId::from_extract(id),
                        module: SessionModule::val(Generation(id)),
                        value: BoundValue {
                            handle,
                            identity: producer_identity(),
                        },
                        type_display: None,
                        defining_expr: None,
                        scope,
                    },
                )
                .unwrap();
        }

        let staged_a = state
            .snapshot_binding_publication(
                actor_a.clone(),
                public_a,
                private_a,
                vec![SessionVarId::from_extract(11)],
            )
            .unwrap()
            .stage()
            .unwrap();
        let staged_b = state
            .snapshot_binding_publication(
                actor_b.clone(),
                public_b,
                private_b,
                vec![SessionVarId::from_extract(12)],
            )
            .unwrap()
            .stage()
            .unwrap();
        assert_eq!(state.publish_staged_public_manifest(
            staged_b, &super::super::PublicationDecision::new(),
        ).unwrap(), super::super::PublicManifestCommit::Durable);
        let decision_a = super::super::PublicationDecision::new();
        assert_eq!(
            state
                .publish_staged_public_manifest(staged_a, &decision_a)
                .unwrap(),
            super::super::PublicManifestCommit::Stale
        );
        let restaged_a = state
            .snapshot_binding_publication(
                actor_a.clone(),
                public_a,
                private_a,
                vec![SessionVarId::from_extract(11)],
            )
            .unwrap()
            .stage()
            .unwrap();
        assert_eq!(
            state
                .publish_staged_public_manifest(restaged_a, &decision_a)
                .unwrap(),
            super::super::PublicManifestCommit::Durable
        );
        state.retire_scope(private_a);
        state.retire_scope(private_b);
        assert_eq!(state.resolve_in(public_a, "a").unwrap().id.raw(), 11);
        assert_eq!(state.resolve_in(public_b, "b").unwrap().id.raw(), 12);
        for (scope, name) in [(public_a, "a"), (public_b, "b")] {
            let handle = state.resolve_in(scope, name).unwrap().value.handle;
            let CodegenPreparedOuter::Constructor { identity, fields } = state
                .prepared_mut()
                .unwrap()
                .machine
                .inspect_outer(handle, RealmId::ROOT)
                .expect("published binding remains a live constructor after private retirement");
            assert_eq!(identity, tidepool_repr::DataConId(980));
            assert!(matches!(fields.as_slice(), [PreparedResult::Scalar(99)]));
        }

        let graph = super::super::recovery::read_v2(&manifest, dir.path())
            .unwrap()
            .unwrap()
            .graph;
        let a = graph.public_binding_tombstones(&actor_a).unwrap();
        let b = graph.public_binding_tombstones(&actor_b).unwrap();
        assert_eq!(a.len(), 1);
        assert_eq!(b.len(), 1);
        assert_eq!(a[0].name, "a");
        assert_eq!(a[0].winner.variable, 11);
        assert_eq!(b[0].name, "b");
        assert_eq!(b[0].winner.variable, 12);
    }
    include!("prepared/package_original_caf_tests.rs");
}
