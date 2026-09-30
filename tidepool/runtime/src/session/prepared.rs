//! Runtime ownership for validated prepared-STG execution artifacts.
//!
//! Here `prepared` refers to the GHC prepared-STG handoff. It is distinct from
//! cell preparation in `workbench.rs` and `resident_workbench.rs`.
//!
//! Parsing, linking, compiled-owner construction, execution, cancellation,
//! disposition, and retained-program reuse cross this boundary in that order.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::Arc;

use tidepool_bridge::{BridgeError, HaskellValue, HaskellVisitor};
use tidepool_codegen::binding_table::{BindingEntry, BindingTable, BoundValue};
use tidepool_toolchain::certified_products::CertifiedTargetPackageInterfaces;

use super::binding_table::BindingIndex;
use tidepool_codegen::machine_state::MachineFailure;
use tidepool_codegen::prepared_program::{
    BatchImport, BatchLeaseRequest, BatchProgram, CompileError, CompiledProgram, DemandError,
    DemandedImage, ExecutionError, ImageRegistry, ImportBindings, InheritedSourceDemand,
    ManagedBuilder, ManagedField, ManagedNode, Parcel, ParkRequest, PreparedCallOptions,
    PreparedFrameEvidence, PreparedHandle, PreparedInput, PreparedMachine, PreparedMachineOptions,
    PreparedOuter as CodegenPreparedOuter, PreparedResult, PreparedResultBatch, ProgramId,
    RunOptions, SourceBinder, SourceInstanceLease, MAX_ANSWER_DEPTH,
};
// Re-exported: callers of this module's resource-scope cancellation API
// (`open_realm`/`cancel_handle`/`close_realm`) need both types without a
// separate `tidepool_codegen` dependency of their own.
pub use tidepool_codegen::machine::CancelHandle;
pub use tidepool_codegen::machine::MachineDisposition;
use tidepool_codegen::suspension::ContinuationId;
pub use tidepool_codegen::suspension::{RealmId, ValueHandle};
use tidepool_repr::execution_schema::{
    link_program, CachedHomeOwner, CtorRow, DefinitionsView, Group, HeapRhs, ImportOwner,
    ImportedValue, JsonLayout, LinkError, MachineImports, ParseError, PreparedProgram, RuntimeRep,
    Signature, SiteDelivery, SiteRow, SymbolIdentity, TypeNode, TypeNodeId, ValueId,
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
    #[error(transparent)]
    Demand(#[from] DemandError),
    #[error("no exact live owner for certified import {0:?}")]
    MissingCertifiedOwner(ImportOwner),
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
    /// A host-built answer with fields was offered to a frame parked for an
    /// open-reply request ([`UNSITED`]): there is no wire evidence to build
    /// it against, so the frame re-enters only by handle or with a
    /// field-less constructor. The frame stays parked.
    #[error("the parked request has an open reply type and accepts only handle or field-less constructor delivery")]
    UnsitedAnswer,
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
    #[error("typed site {site} does not admit constructor {host_id:?} in its answer")]
    AnswerConstructor { site: u64, host_id: DataConId },
    /// The answer's shape does not match the site's type evidence (a literal
    /// where a constructor is required, a field count or scalar width
    /// mismatch, a byte array, or excessive nesting). The frame stays parked.
    #[error("typed site {site} rejects the answer: {detail}")]
    AnswerShape { site: u64, detail: &'static str },
    /// The answer reaches a type the host cannot construct. The frame stays
    /// parked.
    #[error("typed site {site} has an unconstructible answer type: {reason}")]
    AnswerUnconstructible { site: u64, reason: String },
    /// Structural conversion failed at the dispatch/resume boundary. The
    /// frame stays parked and no answer root is published.
    #[error("typed site {site} rejects its structural answer: {source}")]
    AnswerRejected {
        site: u64,
        #[source]
        source: tidepool_bridge::BridgeError,
    },
    /// A resumed handle (bare or framed) is not live in this engine's
    /// ledger: unknown, released, or minted under a different engine. The
    /// frame stays parked.
    #[error("resume delivered a handle that is not live in this engine's ledger")]
    UnknownHandle,
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

impl PreparedRuntimeError {
    #[must_use]
    pub fn kind(&self) -> PreparedFailureKind {
        match self {
            Self::Parse(_)
            | Self::Link(_)
            | Self::Demand(_)
            | Self::MissingCertifiedOwner(_)
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
            | Self::UnknownSite { .. }
            | Self::UntypedRequest { .. }
            | Self::UnsitedAnswer
            | Self::UnhandledRequest
            | Self::DeferredRequiresAsyncHost
            | Self::NoResumeEntry { .. }
            | Self::AnswerDelivery { .. }
            | Self::AnswerConstructor { .. }
            | Self::AnswerShape { .. }
            | Self::AnswerUnconstructible { .. }
            | Self::AnswerRejected { .. }
            | Self::UnknownHandle
            | Self::HostMount { .. }
            | Self::NoHostingProgram
            | Self::NoApplyEntryEntry { .. }
            | Self::NoApplyValueEntry { .. }
            | Self::CrossRealmArgument { .. } => PreparedFailureKind::Rejected,
            Self::Cancelled => PreparedFailureKind::Cancelled,
            Self::Compile(_) => PreparedFailureKind::Rejected,
            // A handler fault is this turn's own failure, so the machine stays reusable.
            Self::Handler { .. } => PreparedFailureKind::Language,
            Self::Run(error) => match error {
                ExecutionError::MissingEntry(_)
                | ExecutionError::Unsupported(_)
                | ExecutionError::Arguments { .. }
                | ExecutionError::ArgumentRepresentation { .. }
                | ExecutionError::UnknownPreparedHandle
                | ExecutionError::ImportShape { .. }
                | ExecutionError::BatchSourceContract(_)
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
    tops: BTreeMap<ValueId, (SymbolIdentity, Option<Signature>)>,
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
    /// The typed sites this program declares and the type graph they point
    /// into, kept for site-evidence resolution and answer validation after
    /// the machine has taken the program's code.
    sites: Vec<SiteRow>,
    types: Vec<TypeNode>,
    /// The request constructors this program answers at a synthetic site,
    /// by bridge id, each with the index of its row in `sites`.
    verb_sites: Vec<(DataConId, usize)>,
    /// Constructor identities and bridge ids by this program's local
    /// `ConstructorId`, so two programs' type graphs compare by identity
    /// rather than local index, and a bridge `HaskellValue`'s constructor resolves
    /// to the row that admits it.
    constructors: Vec<(SymbolIdentity, DataConId, SymbolIdentity)>,
    /// Compiler-authenticated runtime IDs for the JSON constructors. This is
    /// the only JSON role inventory consumed by answer validation.
    json_layout: Option<JsonLayout<DataConId>>,
    /// `constructors`, indexed by qualified identity `(module, occurrence)`
    /// and built once in [`Self::of`], so a leaf lookup
    /// ([`Self::constructor_named`]) is one map lookup rather than a full
    /// scan repeated per leaf of an answer.
    by_identity: BTreeMap<(String, String), DataConId>,
}

/// What installing one program adds to the machine-owned evidence indexes.
struct EvidencePlan {
    sites: Vec<(u64, usize)>,
    verb_sites: Vec<(DataConId, usize)>,
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
    plan: EvidencePlan,
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

    pub(crate) fn compile_certified(
        prepared: PreparedProgram,
        registry: &ImageRegistry,
        package_interfaces: CertifiedTargetPackageInterfaces,
    ) -> Result<Self, CompileError> {
        let image = registry.get_or_compile_prepared(&prepared, || {
            CompiledProgram::compile_prepared_definitions(&prepared).map(Arc::new)
        })?;
        Ok(Self {
            prepared,
            image,
            package_interfaces,
        })
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
    facts: Vec<ProgramFacts>,
    plans: Vec<EvidencePlan>,
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
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SiteTypeEvidence {
    types: Vec<TypeNode>,
    constructors: Vec<SymbolIdentity>,
    input: TypeNodeId,
    answer: TypeNodeId,
}

trait TypeGraph {
    fn type_node(&self, id: TypeNodeId) -> Option<&TypeNode>;
    fn constructor_identity(
        &self,
        id: tidepool_repr::execution_schema::ConstructorId,
    ) -> Option<&SymbolIdentity>;
}

impl TypeGraph for SiteTypeEvidence {
    fn type_node(&self, id: TypeNodeId) -> Option<&TypeNode> {
        self.types.get(id.0 as usize)
    }

    fn constructor_identity(
        &self,
        id: tidepool_repr::execution_schema::ConstructorId,
    ) -> Option<&SymbolIdentity> {
        self.constructors.get(id.0 as usize)
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

    fn of(constructors: &[(SymbolIdentity, DataConId, SymbolIdentity)]) -> Option<Self> {
        Self::from_constructor_facts(constructors.iter())
            .ok()
            .flatten()
    }

    /// Recover the settled constructor pair from exact admitted constructor
    /// declarations. A reduced entry can import the two constructors from
    /// separate source owners, so requiring one `ProgramFacts` value to carry
    /// both declarations loses valid evidence. Conflicting declarations for
    /// either qualified identity remain a refusal.
    fn from_facts<'a>(
        facts: impl IntoIterator<Item = &'a ProgramFacts>,
    ) -> Result<Option<Self>, PreparedRuntimeError> {
        Self::from_constructor_facts(
            facts
                .into_iter()
                .flat_map(|facts| facts.constructors.iter()),
        )
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
        let tops: BTreeMap<ValueId, (SymbolIdentity, Option<Signature>)> = prepared
            .bindings()
            .iter()
            .flat_map(|group| match group {
                Group::NonRecursive(top) => std::slice::from_ref(top),
                Group::Recursive(tops) => tops.as_slice(),
            })
            .map(|top| {
                let export = match &top.binding.rhs {
                    HeapRhs::Function { signature, .. } | HeapRhs::Thunk { signature, .. } => {
                        prepared.signatures().get(signature.0 as usize).cloned()
                    }
                    HeapRhs::Constructor { .. } | HeapRhs::Bytes(_) => None,
                };
                (top.binding.id, (top.identity.clone(), export))
            })
            .collect();
        let entry_module = entry
            .as_ref()
            .and_then(|entry| tops.get(entry))
            .map(|(identity, _)| identity.module.clone());
        let resume = entry_module.clone().and_then(|module| {
            tops.iter().find_map(|(id, (identity, _))| {
                (identity.module == module && identity.occurrence == PREPARED_RESUME_TARGET)
                    .then_some(*id)
            })
        });
        let apply_entry = entry_module.clone().and_then(|module| {
            tops.iter().find_map(|(id, (identity, _))| {
                (identity.module == module && identity.occurrence == PREPARED_APPLY_ENTRY_TARGET)
                    .then_some(*id)
            })
        });
        let apply_value = entry_module.and_then(|module| {
            tops.iter().find_map(|(id, (identity, _))| {
                (identity.module == module && identity.occurrence == PREPARED_APPLY_VALUE_TARGET)
                    .then_some(*id)
            })
        });
        let constructors: Vec<(SymbolIdentity, DataConId, SymbolIdentity)> = prepared
            .constructors()
            .iter()
            .map(|declaration| {
                (
                    declaration.identity.clone(),
                    declaration.host_id,
                    declaration.family.clone(),
                )
            })
            .collect();
        let by_identity: BTreeMap<(String, String), DataConId> = constructors
            .iter()
            .map(|(identity, host_id, _)| {
                (
                    (identity.module.clone(), identity.occurrence.clone()),
                    *host_id,
                )
            })
            .collect();
        let json_layout = prepared
            .json_layout()
            .map(|layout| (*layout).map(|constructor| constructors[constructor.0 as usize].1));
        let sites = prepared.sites().to_vec();
        // Validation guarantees every entry names a declared constructor and
        // an admitted row.
        let verb_sites = prepared
            .verb_sites()
            .iter()
            .filter_map(|(constructor, site)| {
                let (_, host_id, _) = constructors.get(constructor.0 as usize)?;
                let row = sites.iter().position(|row| row.site == *site)?;
                Some((*host_id, row))
            })
            .collect();
        Self {
            entry,
            tops,
            settled: SettledIds::of(&constructors),
            resume,
            apply_entry,
            apply_value,
            sites,
            verb_sites,
            types: prepared.types().to_vec(),
            constructors,
            by_identity,
            json_layout,
        }
    }

    fn type_node(&self, id: TypeNodeId) -> Option<&TypeNode> {
        self.types.get(id.0 as usize)
    }

    fn constructor_identity(
        &self,
        id: tidepool_repr::execution_schema::ConstructorId,
    ) -> Option<&SymbolIdentity> {
        self.constructors
            .get(id.0 as usize)
            .map(|(identity, _, _)| identity)
    }

    fn json_layout(&self) -> Option<JsonLayout<DataConId>> {
        self.json_layout
    }

    fn is_json_value_node(&self, node: TypeNodeId) -> bool {
        let Some(layout) = self.json_layout() else {
            return false;
        };
        let Some(TypeNode::Data { rows, .. }) = self.type_node(node) else {
            return false;
        };
        rows.len() == 6
            && rows
                .iter()
                .any(|row| self.constructor_host_id(row.constructor) == Some(layout.object))
            && rows
                .iter()
                .any(|row| self.constructor_host_id(row.constructor) == Some(layout.array))
            && rows
                .iter()
                .any(|row| self.constructor_host_id(row.constructor) == Some(layout.string))
            && rows
                .iter()
                .any(|row| self.constructor_host_id(row.constructor) == Some(layout.number))
            && rows
                .iter()
                .any(|row| self.constructor_host_id(row.constructor) == Some(layout.bool_))
            && rows
                .iter()
                .any(|row| self.constructor_host_id(row.constructor) == Some(layout.null))
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

    /// The bridge id of a declared constructor, by qualified identity.
    fn constructor_named(&self, module: &str, occurrence: &str) -> Option<DataConId> {
        self.by_identity
            .get(&(module.to_string(), occurrence.to_string()))
            .copied()
    }

    /// The declared row for `host_id` among `node`'s rows, when `node` is a
    /// `Data` node. A framed-handle delivery validates its prefix fields
    /// against this row the same way an ordinary answer's constructor does.
    fn row_for(&self, node: TypeNodeId, host_id: DataConId) -> Option<&CtorRow> {
        match self.type_node(node)? {
            TypeNode::Data { rows, .. } => rows.iter().find(|row| {
                self.constructors
                    .get(row.constructor.0 as usize)
                    .is_some_and(|(_, declared, _)| *declared == host_id)
            }),
            _ => None,
        }
    }
}

impl TypeGraph for ProgramFacts {
    fn type_node(&self, id: TypeNodeId) -> Option<&TypeNode> {
        ProgramFacts::type_node(self, id)
    }

    fn constructor_identity(
        &self,
        id: tidepool_repr::execution_schema::ConstructorId,
    ) -> Option<&SymbolIdentity> {
        ProgramFacts::constructor_identity(self, id)
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

#[derive(Clone, Copy)]
enum StructuralExpected {
    Node(TypeNodeId),
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

struct StructuralAnswerVisitor<'facts, 'builder, 'machine, 'code> {
    site: u64,
    root: TypeNodeId,
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
                .copied()
                .ok_or_else(|| self.shape("the response emits too many constructor fields"))
        } else if self.result.is_none() {
            Ok(StructuralExpected::Node(self.root))
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
        let structural_node = node;
        let Some(node) = self.facts.type_node(structural_node) else {
            return Err(self.shape("the site's type evidence names an undeclared node"));
        };
        match node {
            TypeNode::Data { rows, .. } => {
                if self.facts.is_json_value_node(structural_node) {
                    let admitted = rows.iter().any(|row| {
                        self.facts
                            .constructors
                            .get(row.constructor.0 as usize)
                            .is_some_and(|(_, declared, _)| *declared == host_id)
                    });
                    if !admitted {
                        return Err(self.bridge_abort(PreparedRuntimeError::AnswerConstructor {
                            site: self.site,
                            host_id,
                        }));
                    }
                    return self.json_value_shape(host_id);
                }
                let row = rows
                    .iter()
                    .find(|row| {
                        self.facts
                            .constructors
                            .get(row.constructor.0 as usize)
                            .is_some_and(|(_, declared, _)| *declared == host_id)
                    })
                    .ok_or_else(|| {
                        self.bridge_abort(PreparedRuntimeError::AnswerConstructor {
                            site: self.site,
                            host_id,
                        })
                    })?;
                Ok(row
                    .fields
                    .iter()
                    .copied()
                    .map(StructuralExpected::Node)
                    .collect())
            }
            TypeNode::Text => {
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
            TypeNode::Integer => {
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
            TypeNode::Natural => {
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
            TypeNode::Scalar(_) => Err(self.shape("a scalar field requires a literal")),
            TypeNode::Unconstructible { reason, .. } => Err(self.bridge_abort(
                PreparedRuntimeError::AnswerUnconstructible {
                    site: self.site,
                    reason: reason.clone(),
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
            StructuralExpected::Node(node) => match self.facts.type_node(node) {
                Some(TypeNode::Scalar(rep)) => *rep,
                _ => return Err(self.shape("a literal was emitted for a constructor field")),
            },
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

/// The constructor `response` visits as exactly one field-less constructor,
/// or `None` for any other shape (fields, literals, byte arrays, nesting).
fn nullary_constructor_of(
    response: &dyn tidepool_bridge::ToHaskell,
    table: &DataConTable,
) -> Option<DataConId> {
    #[derive(Default)]
    struct Nullary {
        id: Option<DataConId>,
        open: bool,
    }
    impl Nullary {
        fn reject(got: &str) -> BridgeError {
            BridgeError::TypeMismatch {
                expected: "one field-less constructor".into(),
                got: got.into(),
            }
        }
    }
    impl HaskellVisitor for Nullary {
        fn begin_constructor(&mut self, id: DataConId, fields: usize) -> Result<(), BridgeError> {
            if fields != 0 || self.id.is_some() {
                return Err(Self::reject("a constructor with fields"));
            }
            self.id = Some(id);
            self.open = true;
            Ok(())
        }
        fn end_constructor(&mut self) -> Result<(), BridgeError> {
            if !self.open {
                return Err(Self::reject("an unmatched constructor end"));
            }
            self.open = false;
            Ok(())
        }
        fn literal(&mut self, _: Literal) -> Result<(), BridgeError> {
            Err(Self::reject("a literal"))
        }
        fn byte_array(&mut self, _: Vec<u8>) -> Result<(), BridgeError> {
            Err(Self::reject("a byte array"))
        }
    }
    let mut visitor = Nullary::default();
    response.visit(table, &mut visitor).ok()?;
    (!visitor.open).then_some(visitor.id).flatten()
}

fn build_structural_node(
    response: &dyn tidepool_bridge::ToHaskell,
    table: &DataConTable,
    site: u64,
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
    let mut visitor = StructuralAnswerVisitor {
        site,
        root,
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
    field_nodes: &[TypeNodeId],
    table: &DataConTable,
    site: u64,
    root: TypeNodeId,
    facts: &ProgramFacts,
    builder: &mut ManagedBuilder<'_, '_>,
) -> Result<ManagedNode, PreparedRuntimeError> {
    let response_table = table.with_json_layout(facts.json_layout());
    let mut visitor = StructuralAnswerVisitor {
        site,
        root,
        facts,
        builder,
        frames: vec![StructuralFrame {
            host_id: constructor,
            expected: field_nodes
                .iter()
                .copied()
                .map(StructuralExpected::Node)
                .collect(),
            fields: Vec::with_capacity(field_nodes.len()),
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

/// Whether two site rows from two programs carry the same evidence: the same
/// delivery mode and structurally equal wire and input type graphs, compared
/// by family and constructor identity with ordered arguments, never by local
/// node or constructor numbers. Cycles (recursive types) are compared
/// coinductively: a node pair already under comparison is taken as equal.
fn sites_equivalent(a: &ProgramFacts, a_row: &SiteRow, b: &ProgramFacts, b_row: &SiteRow) -> bool {
    if a_row.delivery != b_row.delivery || a_row.inputs.len() != b_row.inputs.len() {
        return false;
    }
    let mut visited = BTreeSet::new();
    type_nodes_equivalent(a, a_row.wire, b, b_row.wire, &mut visited)
        && a_row
            .inputs
            .iter()
            .zip(&b_row.inputs)
            .all(|(x, y)| type_nodes_equivalent(a, *x, b, *y, &mut visited))
}

fn type_nodes_equivalent<A: TypeGraph, B: TypeGraph>(
    a: &A,
    a_id: TypeNodeId,
    b: &B,
    b_id: TypeNodeId,
    visited: &mut BTreeSet<(u32, u32)>,
) -> bool {
    if !visited.insert((a_id.0, b_id.0)) {
        return true;
    }
    match (a.type_node(a_id), b.type_node(b_id)) {
        (
            Some(TypeNode::Data {
                family: a_family,
                arguments: a_arguments,
                rows: a_rows,
            }),
            Some(TypeNode::Data {
                family: b_family,
                arguments: b_arguments,
                rows: b_rows,
            }),
        ) => {
            a_family == b_family
                && a_arguments.len() == b_arguments.len()
                && a_rows.len() == b_rows.len()
                && a_arguments
                    .iter()
                    .zip(b_arguments)
                    .all(|(x, y)| type_nodes_equivalent(a, *x, b, *y, visited))
                && a_rows.iter().zip(b_rows).all(|(x, y)| {
                    a.constructor_identity(x.constructor) == b.constructor_identity(y.constructor)
                        && x.fields.len() == y.fields.len()
                        && x.fields
                            .iter()
                            .zip(&y.fields)
                            .all(|(f, g)| type_nodes_equivalent(a, *f, b, *g, visited))
                })
        }
        (Some(TypeNode::Text), Some(TypeNode::Text))
        | (Some(TypeNode::Integer), Some(TypeNode::Integer))
        | (Some(TypeNode::Natural), Some(TypeNode::Natural)) => true,
        (Some(TypeNode::Scalar(x)), Some(TypeNode::Scalar(y))) => x == y,
        (
            Some(TypeNode::Unconstructible {
                reason: a_reason,
                rendered: a_rendered,
            }),
            Some(TypeNode::Unconstructible {
                reason: b_reason,
                rendered: b_rendered,
            }),
        ) => a_reason == b_reason && a_rendered == b_rendered,
        _ => false,
    }
}

/// The site a frame parks under when its request has an open reply type.
/// Zero is never a declared site (`check_sites` refuses it).
const UNSITED: u64 = 0;

/// A boxed or unboxed non-negative `Int` field: the leading site argument of
/// an extractor-sited kernel request.
fn site_field(field: &HaskellValue, table: &DataConTable) -> Option<u64> {
    match field {
        HaskellValue::Lit(Literal::LitInt(n)) => u64::try_from(*n).ok(),
        HaskellValue::Con(id, inner) if table.name_of(*id) == Some("I#") => {
            match inner.as_slice() {
                [HaskellValue::Lit(Literal::LitInt(n))] => u64::try_from(*n).ok(),
                _ => None,
            }
        }
        _ => None,
    }
}

/// The typed site a suspended request names, read from the request's
/// rendered payload: the protocol's sited helpers place the site id under the
/// `typedSite` key of the request's JSON payload object
/// (`tidepool-protocol`'s `ObjectValue::Site`), the same field the harness
/// uses to classify a suspension. `None` for a request that carries no such
/// field: an ordinary handled effect.
fn typed_site_of(request: &HaskellValue, table: &DataConTable) -> Option<u64> {
    /// The `Text`/string-literal content of an aeson `Key`/`HaskellValue` leaf.
    fn value_text(value: &HaskellValue, table: &DataConTable) -> Option<String> {
        match value {
            HaskellValue::Lit(Literal::LitString(bytes)) => {
                std::str::from_utf8(bytes).ok().map(str::to_owned)
            }
            HaskellValue::Con(id, fields) if table.name_of(*id) == Some("Text") => {
                tidepool_bridge::shapes::text_bytes_clamped(fields, table)
                    .and_then(|bytes| String::from_utf8(bytes).ok())
            }
            _ => None,
        }
    }
    /// The numeric content of a `typedSite` leaf: an unboxed or boxed
    /// integral literal, or an aeson `Number` wrapping one. This is the one
    /// leaf this walk ever renders through the snapshot JSON renderer — never
    /// the whole request.
    fn value_u64(value: &HaskellValue, table: &DataConTable) -> Option<u64> {
        match value {
            HaskellValue::Lit(Literal::LitInt(n)) => u64::try_from(*n).ok(),
            HaskellValue::Lit(Literal::LitWord(n)) => Some(*n),
            HaskellValue::Con(_, _) => match crate::render::value_to_json(value, table, 0) {
                serde_json::Value::Number(n) => n.as_u64(),
                _ => None,
            },
            _ => None,
        }
    }
    /// Walk the request's `HaskellValue` tree directly (never its JSON rendering)
    /// looking for an aeson `Object` layer with a `typedSite` entry, the same
    /// depth bound (4) the JSON walk used.
    fn search(value: &HaskellValue, table: &DataConTable, depth: usize) -> Option<u64> {
        if depth > 4 {
            return None;
        }
        let HaskellValue::Con(id, fields) = value else {
            return None;
        };
        if table.name_of(*id) == Some("Object") {
            if let [map_val] = fields.as_slice() {
                let mut found = None;
                tidepool_bridge::shapes::walk_map_entries(
                    map_val,
                    table,
                    0,
                    MAX_ANSWER_DEPTH,
                    &mut |key, val, _| {
                        if found.is_none() && value_text(key, table).as_deref() == Some("typedSite")
                        {
                            found = value_u64(val, table);
                        }
                    },
                );
                if found.is_some() {
                    return found;
                }
            }
        }
        fields
            .iter()
            .find_map(|field| search(field, table, depth + 1))
    }
    search(request, table, 0)
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
    /// The machine-owned verb index: which installed program's synthetic
    /// site row answers an ordinary effect request, by the request's outer
    /// constructor. Extended in the same install transaction as `sites`,
    /// under the same structural-equivalence rule.
    verb_sites: BTreeMap<DataConId, SiteWitness>,
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
const HOME_UNIT: &str = "main";

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

impl PreparedEngine {
    /// Capture the request site's compiler-authenticated graph before its
    /// input travels to another actor's machine session.
    pub fn request_site_type_evidence(&self, site: u64) -> Option<SiteTypeEvidence> {
        let witness = self.sites.get(&site)?;
        let facts = self.programs.get(&witness.owner)?;
        let row = facts.sites.get(witness.row)?;
        Some(SiteTypeEvidence {
            types: facts.types.clone(),
            constructors: facts
                .constructors
                .iter()
                .map(|(identity, _, _)| identity.clone())
                .collect(),
            input: *row.inputs.first()?,
            answer: row.wire,
        })
    }

    /// Compare the original request graph with the access site installed in
    /// this machine. The third accessor input is its complete reply evidence.
    pub fn request_scope_types_match(&self, request: &SiteTypeEvidence, access_site: u64) -> bool {
        let Some(witness) = self.sites.get(&access_site) else {
            return false;
        };
        let Some(facts) = self.programs.get(&witness.owner) else {
            return false;
        };
        let Some(row) = facts.sites.get(witness.row) else {
            return false;
        };
        let (Some(input), Some(reply)) = (row.inputs.first(), row.inputs.get(2)) else {
            return false;
        };
        type_nodes_equivalent(request, request.input, facts, *input, &mut BTreeSet::new())
            && type_nodes_equivalent(request, request.answer, facts, *reply, &mut BTreeSet::new())
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

    /// Stream host UTF-8 text into the worker `Text` representation. A
    /// qualified constructor identity is required when it is present, so a
    /// same-named user constructor cannot silently become host text.
    pub fn build_host_text(
        &mut self,
        realm: RealmId,
        text: &str,
        table: &DataConTable,
    ) -> Result<PreparedHandle, PreparedRuntimeError> {
        let text_id = table
            .get_by_qualified_name("Data.Text.Internal.Text")
            .or_else(|| table.get_by_qualified_name("Data.Text.Text"))
            .filter(|id| table.get(*id).is_some_and(|con| con.rep_arity == 3))
            .ok_or_else(|| PreparedRuntimeError::HostMount {
                detail: "the compiler table does not declare Data.Text.Internal.Text".into(),
            })?;
        let length = i64::try_from(text.len()).map_err(|_| PreparedRuntimeError::HostMount {
            detail: "host Text exceeds the worker Int length range".into(),
        })?;
        let mut builder = self
            .machine
            .managed_builder()
            .map_err(PreparedRuntimeError::Run)?;
        let bytes = builder
            .bytes(text.as_bytes())
            .map_err(PreparedRuntimeError::Run)?;
        let mut zero = [0_u8; 16];
        zero[..8].copy_from_slice(&0_i64.to_ne_bytes());
        let mut len = [0_u8; 16];
        len[..8].copy_from_slice(&length.to_ne_bytes());
        let root = builder
            .constructor(
                text_id,
                &[
                    ManagedField::Consume(bytes),
                    ManagedField::Scalar(zero),
                    ManagedField::Scalar(len),
                ],
            )
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
    ) -> Result<ProgramId, PreparedRuntimeError> {
        let Some(registry) = self.registry.clone() else {
            let compiled = self
                .machine
                .compile_for_install(&linked)
                .map_err(PreparedRuntimeError::Compile)?;
            return self
                .machine
                .install_program(compiled, imports)
                .map_err(PreparedRuntimeError::Run);
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
        self.machine
            .install_shared(image, imports)
            .map_err(PreparedRuntimeError::Run)
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
        let facts = ProgramFacts::of(&prepared);
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
        let (machine, program) =
            PreparedMachine::new_shared(image, PreparedMachineOptions { nursery_bytes })
                .map_err(PreparedRuntimeError::Run)?;
        let mut engine = Self::from_machine(machine, registry);
        // The first program can conflict only with itself.
        let plan = engine.plan_evidence(&facts)?;
        engine.finish_program_install(program, facts, plan, exports)?;
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
            verb_sites: BTreeMap::new(),
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
            .map_err(PreparedRuntimeError::Run)?;
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
                let owner = self
                    .programs
                    .get(&witness.owner)
                    .ok_or(PreparedRuntimeError::Run(ExecutionError::UnknownProgram(
                        witness.owner,
                    )))?;
                (Some(witness.owner), owner, &owner.sites[witness.row])
            } else if let Some((_, earlier)) = planned.iter().find(|(id, _)| *id == site.site) {
                (None, facts, &facts.sites[*earlier])
            } else {
                planned.push((site.site, row));
                continue;
            };
            if !sites_equivalent(owner_facts, owner_row, facts, site) {
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

    /// The verb-index entries of `facts` that installing it would make
    /// canonical, under [`Self::plan_sites`]'s rule keyed by request
    /// constructor: a constructor already indexed (or earlier in `facts`)
    /// must be answered by a structurally equivalent row, and keeps its
    /// existing owner; otherwise the install is refused.
    fn plan_verb_sites(
        &self,
        facts: &ProgramFacts,
    ) -> Result<Vec<(DataConId, usize)>, PreparedRuntimeError> {
        let mut planned: Vec<(DataConId, usize)> = Vec::new();
        for &(host_id, row) in &facts.verb_sites {
            let site = &facts.sites[row];
            let (owner, owner_facts, owner_row) =
                if let Some(witness) = self.verb_sites.get(&host_id) {
                    let owner =
                        self.programs
                            .get(&witness.owner)
                            .ok_or(PreparedRuntimeError::Run(ExecutionError::UnknownProgram(
                                witness.owner,
                            )))?;
                    (Some(witness.owner), owner, &owner.sites[witness.row])
                } else if let Some((_, earlier)) = planned.iter().find(|(id, _)| *id == host_id) {
                    (None, facts, &facts.sites[*earlier])
                } else {
                    planned.push((host_id, row));
                    continue;
                };
            if !sites_equivalent(owner_facts, owner_row, facts, site) {
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

    /// Both indexes' plans for installing `facts`, checked before anything
    /// is compiled or published.
    fn plan_evidence(&self, facts: &ProgramFacts) -> Result<EvidencePlan, PreparedRuntimeError> {
        Ok(EvidencePlan {
            sites: self.plan_sites(facts)?,
            verb_sites: self.plan_verb_sites(facts)?,
        })
    }

    /// Check duplicate site and verb ownership across a batch before any
    /// machine mutation. Equal evidence keeps its first planned owner.
    fn plan_batch_evidence(
        &self,
        facts: &[ProgramFacts],
    ) -> Result<Vec<EvidencePlan>, PreparedRuntimeError> {
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
                    ) {
                        return Err(PreparedRuntimeError::DuplicateSite { site });
                    }
                } else {
                    sites.insert(site, (group, row));
                    accepted_sites.push((site, row));
                }
            }
            plan.sites = accepted_sites;
            let mut accepted_verbs = Vec::new();
            for (host_id, row) in plan.verb_sites.drain(..) {
                if let Some(&(prior_group, prior_row)) = verbs.get(&host_id) {
                    if !sites_equivalent(
                        &facts[prior_group],
                        &facts[prior_group].sites[prior_row],
                        group_facts,
                        &group_facts.sites[row],
                    ) {
                        return Err(PreparedRuntimeError::DuplicateSite {
                            site: group_facts.sites[row].site,
                        });
                    }
                } else {
                    verbs.insert(host_id, (group, row));
                    accepted_verbs.push((host_id, row));
                }
            }
            plan.verb_sites = accepted_verbs;
            plans.push(plan);
        }
        Ok(plans)
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
                    return Err(PreparedRuntimeError::Run(error));
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
        facts: ProgramFacts,
        plan: EvidencePlan,
        exports: Vec<(SymbolIdentity, ValueId, Option<Signature>)>,
    ) -> Result<(), PreparedRuntimeError> {
        self.machine
            .pin(program)
            .map_err(PreparedRuntimeError::Run)?;
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
        self.programs.insert(program, facts);
        self.publish_evidence(program, plan);
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
            }
        })
    }

    fn code_export_import(
        &self,
        owner: &ImportOwner,
        declaration: &tidepool_repr::execution_schema::GlobalDecl,
    ) -> Result<BatchImport, PreparedRuntimeError> {
        let ImportOwner::CodeExport {
            binder,
            generation,
            root_id,
        } = owner
        else {
            unreachable!("code export import has a code export owner")
        };
        let export = self
            .code_exports
            .get(binder)
            .filter(|export| {
                binder == &declaration.identity
                    && declaration.required_generation == Some(*generation)
                    && *generation == CODE_EXPORT_GENERATION
                    && export.handle.raw().0 == *root_id
                    && export.handle.rep() == declaration.rep
                    && self.machine.prepared_handle_of(export.handle.raw()) == Some(export.handle)
            })
            .ok_or_else(|| PreparedRuntimeError::MissingCertifiedOwner(owner.clone()))?;
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

    /// Make `program` the canonical owner of the planned rows and verb
    /// entries.
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
        self.verb_sites.extend(
            plan.verb_sites
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
        let plan = self.plan_evidence(&facts)?;
        let evidence_ms = lap();
        let linked = link_program(prepared, &values)?;
        let link_ms = lap();
        // `compile_and_install` consults `self.registry` (when this engine
        // has one) before compiling: a hit installs the already-compiled
        // image through `install_shared` and compiles nothing, so
        // `compile_ms` below also covers a registry lookup on the hit path.
        let program = self.compile_and_install(linked, imports)?;
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
        self.finish_program_install(program, facts, plan, exports)?;
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
                        .map_err(PreparedRuntimeError::Run)?;
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
                .map_err(PreparedRuntimeError::Run)?;
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
            .map(|selected| ProgramFacts::of_definitions(selected.group().definitions(), None))
            .collect();
        let plans = self.plan_batch_evidence(&facts)?;
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
                    ImportOwner::CodeExport { .. } => {
                        imports.push(self.code_export_import(owner, declaration)?);
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
            .map_err(PreparedRuntimeError::Run)?;
        for (binder, digest) in package_updates {
            self.code_exports
                .get_mut(&binder)
                .expect("preflighted package export remains installed")
                .interface_digest = Some(digest);
        }
        for ((id, facts), plan) in ids.iter().copied().zip(facts).zip(plans) {
            self.machine
                .pin(id)
                .expect("batch returned an installed program");
            self.programs.insert(id, facts);
            self.publish_evidence(id, plan);
        }
        self.installs_since_major += ids.len();
        Ok(ids)
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
        inherited: &BTreeMap<SourceBinder, SourceInstanceLease>,
        exact_external: &HashMap<ImportOwner, PreparedHandle>,
        bindings: &BindingTable,
    ) -> Result<CertifiedTurnInstall, PreparedRuntimeError> {
        let target_exports: BTreeMap<_, _> = exportable_code_tops(&target.prepared)
            .into_iter()
            .map(|(identity, value, _)| (identity, value))
            .collect();
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
            if self.code_exports.contains_key(binder) {
                continue;
            }
            let value = target_exports
                .get(binder)
                .copied()
                .filter(|_| {
                    binder.unit == *unit
                        && binder.module == *module
                        && matches_target
                        && target.package_interfaces.interface_digest(unit, module)
                            == Some(*interface_digest)
                })
                .ok_or_else(|| PreparedRuntimeError::MissingCertifiedOwner(owner.clone()))?;
            if target_packages
                .insert(binder.clone(), (value, *interface_digest))
                .is_some_and(|previous| previous != (value, *interface_digest))
            {
                return Err(PreparedRuntimeError::MissingCertifiedOwner(owner.clone()));
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
        inherited: &BTreeMap<SourceBinder, SourceInstanceLease>,
        exact_external: &HashMap<ImportOwner, PreparedHandle>,
        bindings: &BindingTable,
        target_packages: BTreeMap<SymbolIdentity, (ValueId, [u8; 32])>,
        certified_exports: BTreeMap<SymbolIdentity, [u8; 32]>,
    ) -> Result<CertifiedTurnInstall, PreparedRuntimeError> {
        if target.prepared.globals().len() != target_owners.len() {
            return Err(PreparedRuntimeError::CertifiedTargetOwners);
        }
        for (declaration, owner) in target.prepared.globals().iter().zip(target_owners) {
            let aligned = match owner {
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
            };
            if !aligned {
                return Err(PreparedRuntimeError::CertifiedTargetOwners);
            }
        }

        let mut source = BTreeMap::<SourceBinder, (usize, ValueId)>::new();
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
                let key = SourceBinder {
                    version: group.owner().module_version.clone(),
                    binder: binder.clone(),
                };
                if inherited.contains_key(&key) || source.insert(key.clone(), (index, id)).is_some()
                {
                    return Err(DemandError::DuplicateBinder(key).into());
                }
            }
        }

        let mut late_sources = BTreeMap::new();
        for requested in inherited_needed {
            let key = requested.binder().clone();
            if inherited.contains_key(&key)
                || source.contains_key(&key)
                || late_sources.insert(key.clone(), requested).is_some()
            {
                return Err(DemandError::DuplicateBinder(key).into());
            }
        }

        let mut pending = target_owners
            .iter()
            .filter_map(|owner| match owner {
                ImportOwner::Source { version, binder } => Some(SourceBinder {
                    version: version.clone(),
                    binder: binder.clone(),
                }),
                ImportOwner::Retained { .. }
                | ImportOwner::CodeExport { .. }
                | ImportOwner::Package { .. } => None,
            })
            .collect::<Vec<_>>();
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
                                ImportOwner::Source { version, binder } => Some(SourceBinder {
                                    version: version.clone(),
                                    binder: binder.clone(),
                                }),
                                ImportOwner::Retained { .. }
                                | ImportOwner::CodeExport { .. }
                                | ImportOwner::Package { .. } => None,
                            }),
                    );
                }
            } else if late_sources.contains_key(&binder) {
                needed_late.insert(binder);
            } else if !inherited.contains_key(&binder) {
                return Err(DemandError::MissingSource(binder).into());
            }
        }
        if reachable.len() != demanded.len() || needed_late.len() != late_sources.len() {
            return Err(PreparedRuntimeError::UnreachableCertifiedGroup);
        }

        for owner in target_owners.iter().chain(
            demanded
                .iter()
                .flat_map(|selected| selected.group().imports()),
        ) {
            let ImportOwner::Source { version, binder } = owner else {
                continue;
            };
            let key = SourceBinder {
                version: version.clone(),
                binder: binder.clone(),
            };
            let expected = source_evidence
                .get(&key)
                .ok_or_else(|| PreparedRuntimeError::InvalidCertifiedSourceOwner(key.clone()))?;
            let actual = if let Some(&(index, _)) = source.get(&key) {
                let group = demanded[index].group();
                (group.owner(), group.original_ordinal())
            } else if let Some(lease) = inherited.get(&key) {
                (lease.owner(), lease.original_ordinal())
            } else if let Some(requested) = late_sources.get(&key) {
                (requested.owner(), requested.original_ordinal())
            } else {
                return Err(PreparedRuntimeError::InvalidCertifiedSourceOwner(key));
            };
            if actual.0 != &expected.0 || actual.1 != expected.1 {
                return Err(PreparedRuntimeError::InvalidCertifiedSourceOwner(key));
            }
        }

        let mut late_leases = BTreeMap::new();
        for (key, requested) in late_sources {
            match self.machine.retain_certified_source_top(requested) {
                Ok(lease) => {
                    late_leases.insert(key, lease);
                }
                Err(error) => {
                    for lease in late_leases.into_values() {
                        assert!(
                            self.release(lease.handle()),
                            "staged sibling source root remains live"
                        );
                    }
                    return Err(PreparedRuntimeError::Run(error));
                }
            }
        }

        let result = (|| -> Result<CertifiedTurnInstall, PreparedRuntimeError> {
            let mut facts: Vec<_> = demanded
                .iter()
                .map(|selected| ProgramFacts::of_definitions(selected.group().definitions(), None))
                .collect();
            facts.push(ProgramFacts::of(&target.prepared));
            // A reduced target need not redeclare the Settled constructors:
            // it may reuse the exact original definitions already installed
            // by an earlier target or one of this batch's source groups.
            // ProgramFacts still carries the constructor identities used by
            // the decoder, and a conflicting host-id pair is never accepted.
            let settled = SettledIds::from_facts(self.programs.values().chain(facts.iter()))?;
            if let Some(target_facts) = facts.last_mut() {
                target_facts.settled = settled;
            }
            let plans = self.plan_batch_evidence(&facts)?;
            let exports = exportable_code_tops(&target.prepared);
            let mut programs = Vec::with_capacity(demanded.len() + 1);
            let mut package_updates = BTreeMap::<SymbolIdentity, [u8; 32]>::new();
            let mut source_needed = BTreeSet::<SourceBinder>::new();

            let mut append = |image: Arc<CompiledProgram>,
                              globals: &[tidepool_repr::execution_schema::GlobalDecl],
                              owners: &[ImportOwner]|
             -> Result<(), PreparedRuntimeError> {
                let mut imports = Vec::with_capacity(owners.len());
                for (declaration, owner) in globals.iter().zip(owners) {
                    let import = match owner {
                        ImportOwner::Source { version, binder } => {
                            let key = SourceBinder {
                                version: version.clone(),
                                binder: binder.clone(),
                            };
                            if let Some(&(group, binding)) = source.get(&key) {
                                source_needed.insert(key);
                                BatchImport::Source { group, binding }
                            } else {
                                let lease = inherited
                                    .get(&key)
                                    .or_else(|| late_leases.get(&key))
                                    .filter(|lease| {
                                        lease.binder() == &key
                                            && lease.owner().module_version == *version
                                            && lease.owner().unit == binder.unit
                                            && lease.owner().module == binder.module
                                    })
                                    .ok_or_else(|| DemandError::MissingSource(key.clone()))?;
                                self.machine
                                    .handle_is_evaluated(lease.handle())
                                    .map_err(PreparedRuntimeError::Run)?;
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
                        ImportOwner::CodeExport { .. } => {
                            self.code_export_import(owner, declaration)?
                        }
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
                                return Err(PreparedRuntimeError::MissingCertifiedOwner(
                                    owner.clone(),
                                ));
                            }
                            let import = if let Some(export) = self.code_exports.get(binder) {
                                if !matches_protected_package_interface(export, interface_digest) {
                                    return Err(PreparedRuntimeError::MissingCertifiedOwner(
                                        owner.clone(),
                                    ));
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
                                        PreparedRuntimeError::MissingCertifiedOwner(owner.clone())
                                    })?;
                                debug_assert_eq!(digest, *interface_digest);
                                BatchImport::Source {
                                    group: demanded.len(),
                                    binding,
                                }
                            };
                            if package_updates
                                .insert(binder.clone(), *interface_digest)
                                .is_some_and(|previous| previous != *interface_digest)
                            {
                                return Err(PreparedRuntimeError::MissingCertifiedOwner(
                                    owner.clone(),
                                ));
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
                )?;
            }
            append(
                Arc::clone(&target.image),
                target.prepared.globals(),
                target_owners,
            )?;
            drop(append);
            let requests = source_needed
                .iter()
                .map(|binder| {
                    let &(group, _) = source
                        .get(binder)
                        .expect("source edge selected an in-batch binder");
                    BatchLeaseRequest::for_demanded(group, &demanded[group], binder)
                        .map_err(PreparedRuntimeError::Run)
                })
                .collect::<Result<Vec<_>, _>>()?;
            let installed = self
                .machine
                .install_shared_batch_with_leases(programs, requests)
                .map_err(PreparedRuntimeError::Run)?;
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
                leases: installed.leases,
                facts,
                plans,
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
                staged.leases.extend(late_leases.into_values());
                Ok(staged)
            }
            Err(error) => {
                for lease in late_leases.into_values() {
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
        for ((id, facts), plan) in programs.zip(staged.facts).zip(staged.plans) {
            self.programs.insert(id, facts);
            self.publish_evidence(id, plan);
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
        let plan = self.plan_evidence(&facts)?;
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
            plan,
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
        snapshot: InstallSnapshot,
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
        let program = self
            .machine
            .install_shared(compiled, snapshot.imports)
            .map_err(PreparedRuntimeError::Run)?;
        tracing::info!(
            target: "tidepool_runtime::prepared_install",
            imports = import_count,
            compiled_off_checkout = true,
            "prepared install"
        );
        self.finish_program_install(program, snapshot.facts, snapshot.plan, snapshot.exports)?;
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
        if self.machine.realm_cancel_handle(realm).is_cancelled() {
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

    /// Apply a rooted `Int -> M a` closure `f` to `argument` through the
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
        if self.machine.realm_cancel_handle(realm).is_cancelled() {
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
        if self.machine.realm_cancel_handle(realm).is_cancelled() {
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

    /// Park a suspension `program`'s settled layer produced under `realm`:
    /// read the `Union` layer of `request`, observe its payload through the
    /// machine observe path (the request the host reports), read
    /// the typed site it names, resolve that site through the machine-owned
    /// index to its evidence owner, and park `continuation` with that
    /// evidence and `program`'s admitted resume entry. Every refusal releases
    /// both handles and parks nothing: a runner without a resume entry, a
    /// suspension under `HandleOrError` (nothing is handled on this route
    /// yet), a request without a typed site (an ordinary handled effect, not
    /// yet answered on this route), or a site no installed program declares.
    ///
    /// `pub`, not `pub(crate)`: this method is self-contained on `Self` —
    /// every input it reads (`self.sites`/`self.verb_sites`/`self.programs`,
    /// all populated by [`Self::bootstrap`]/[`Self::install`]) and mutates
    /// (the machine's own park registry) belongs to the engine itself, with
    /// no session, actor, or lexical-scope bookkeeping folded in. The rest of
    /// the parked-continuation cycle this feeds — [`Self::resume_with_structural_answer`],
    /// [`Self::parked_count`], [`Self::stowed_roots_count`],
    /// [`Self::parked_ids`], [`Self::parked_realm`], [`Self::close_realm`],
    /// [`Self::abort_parked`] — was already `pub`; this was the one private
    /// step in an otherwise-public cycle.
    ///
    /// Per `docs/continuation-parking-contract.md`'s "Consumer obligations":
    /// the returned [`PreparedParked::id`] is the caller's to retain — this
    /// engine enforces no capacity limit (e.g. "one outstanding turn") and no
    /// actor-local grant or principal check; those remain the caller's, same
    /// as on `PreparedEngine`. A parked frame is a registered GC root until
    /// resumed or its resource scope closes; an unresumed park that the caller drops
    /// on the floor leaks a root until [`Self::close_realm`].
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
        // A request carrying a dynamic site names it; an ordinary effect
        // request is classified by its outer constructor through the verb
        // index, which names the synthetic row answering it.
        let classified = match typed_site_of(&request, table) {
            Some(site) => self
                .sites
                .get(&site)
                .map(|witness| (site, witness.owner))
                .ok_or(PreparedRuntimeError::UnknownSite { site }),
            None => match &request {
                HaskellValue::Con(host_id, fields) => Ok(self
                    .verb_sites
                    .get(host_id)
                    .and_then(|witness| {
                        let row = self.programs.get(&witness.owner)?.sites.get(witness.row)?;
                        Some((row.site, *witness))
                    })
                    .or_else(|| {
                        // Kernel requests of an extractor-sited verb
                        // (`receiveSited`, `serveSited`) carry the site id as
                        // their first `Int` field; the reply index is open,
                        // so they have no synthetic row.
                        let site = fields.first().and_then(|field| site_field(field, table))?;
                        self.sites.get(&site).map(|witness| (site, *witness))
                    })
                    // An open-reply request (an actor `call`'s `result`)
                    // has no wire evidence to build an answer against; it
                    // parks unsited and re-enters only by handle.
                    .map_or((UNSITED, program), |(site, witness)| (site, witness.owner))),
                _ => Err(PreparedRuntimeError::UntypedRequest {
                    constructor: "a non-constructor value".to_owned(),
                }),
            },
        };
        let (site, owner) = match classified {
            Ok(classified) => classified,
            Err(error) => {
                self.machine.release(payload);
                self.machine.release(continuation);
                return Err(error);
            }
        };
        // A live-payload policy names one field of THIS request Con (the
        // convention's field 1) as the value crossing the runtime boundary
        // by reference. Classify first so a rejected request cannot strand a
        // newly tenured root without a frame to own it; then mirror the field
        // before releasing `payload`. `PreparedMachine::park` consumes the
        // root on both success and refusal.
        let live_payload_root =
            match self.tenure_live_payload(payload, realm, park.live_payload, &request) {
                Ok(root) => root,
                Err(error) => {
                    self.machine.release(payload);
                    self.machine.release(continuation);
                    return Err(PreparedRuntimeError::Run(error));
                }
            };
        self.machine.release(payload);
        let evidence = PreparedFrameEvidence {
            owner,
            site,
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
    /// field's handle is adopted into a bare root via
    /// [`PreparedMachine::take_handle_root`], and every other minted field
    /// handle is released immediately (`inspect_outer` is non-consuming and
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
    ) -> Result<Option<tidepool_codegen::old_space::RootSlot>, ExecutionError> {
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
                    root = self.machine.take_handle_root(handle)?;
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
        let taken = self.take_for_resume(id, answer);
        let (continuation, evidence, realm) = match taken {
            Ok(taken) => taken,
            Err(error) => {
                self.machine.release(answer);
                return Err(error);
            }
        };
        let batch = self.machine.run_entry_retained(
            evidence.runner,
            evidence.resume_entry,
            &[
                PreparedInput::Managed(continuation),
                PreparedInput::Managed(answer),
            ],
            SETTLE_CALL,
            realm,
        );
        self.machine.release(continuation);
        self.machine.release(answer);
        let batch = batch.map_err(PreparedRuntimeError::Run)?;
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
        if self.machine.realm_cancel_handle(realm).is_cancelled() {
            return Err(PreparedRuntimeError::Cancelled);
        }
        let (continuation, evidence) = self
            .machine
            .take_parked(id)
            .map_err(PreparedRuntimeError::Run)?;
        let batch = self.machine.run_entry_retained(
            evidence.runner,
            evidence.resume_entry,
            &[
                PreparedInput::Managed(continuation),
                PreparedInput::Managed(answer),
            ],
            SETTLE_CALL,
            realm,
        );
        self.machine.release(continuation);
        // `answer` stays live: the caller owns it before and after.
        let batch = batch.map_err(PreparedRuntimeError::Run)?;
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
    /// must be live in this engine's ledger; the only check possible on this
    /// route is its `RuntimeRep` (every handle this engine mints is
    /// `LiftedRef`), so no deeper type check is available on this path. The
    /// handle's root is a BORROW: this call does not release it.
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
        let site = evidence.site;
        if site == UNSITED {
            return Err(PreparedRuntimeError::UnsitedAnswer);
        }
        let (programs, machine) = (&self.programs, &mut self.machine);
        let owner = programs
            .get(&evidence.owner)
            .ok_or(PreparedRuntimeError::Run(ExecutionError::UnknownProgram(
                evidence.owner,
            )))?;
        let row = owner
            .sites
            .iter()
            .find(|row| row.site == site)
            .ok_or(PreparedRuntimeError::UnknownSite { site })?;
        if row.delivery != SiteDelivery::HostAnswer {
            return Err(PreparedRuntimeError::AnswerDelivery {
                site,
                delivery: row.delivery,
            });
        }
        let ctor_row = owner.row_for(row.wire, constructor).ok_or(
            PreparedRuntimeError::AnswerConstructor {
                site,
                host_id: constructor,
            },
        )?;
        if ctor_row.fields.len() != prefix.len() + 1 {
            return Err(PreparedRuntimeError::AnswerShape {
                site,
                detail: "the framed constructor's declared field count does not match the supplied prefix plus the borrowed handle field",
            });
        }
        if machine.realm_cancel_handle(realm).is_cancelled() {
            return Err(PreparedRuntimeError::Cancelled);
        }
        let mut builder = machine
            .managed_builder()
            .map_err(PreparedRuntimeError::Run)?;
        let root = build_framed_structural_node(
            prefix,
            handle,
            constructor,
            &ctor_row.fields,
            table,
            site,
            row.wire,
            owner,
            &mut builder,
        )?;
        let built = builder
            .finish(realm, root)
            .map_err(PreparedRuntimeError::Run)?;
        self.resume_parked(id, built)
    }

    /// The pre-take checks of a resume, then the take: the frame exists,
    /// `answer` is live under its resource scope, the scope is not cancelled.
    fn take_for_resume(
        &mut self,
        id: ContinuationId,
        answer: PreparedHandle,
    ) -> Result<(PreparedHandle, PreparedFrameEvidence, RealmId), PreparedRuntimeError> {
        let (realm, _) = self.machine.parked(id).ok_or(PreparedRuntimeError::Run(
            ExecutionError::UnknownContinuation(id),
        ))?;
        if self.machine.handle_realm(answer) != Some(realm) {
            return Err(PreparedRuntimeError::CrossRealmArgument { realm });
        }
        if self.machine.realm_cancel_handle(realm).is_cancelled() {
            return Err(PreparedRuntimeError::Cancelled);
        }
        let (continuation, evidence) = self
            .machine
            .take_parked(id)
            .map_err(PreparedRuntimeError::Run)?;
        Ok((continuation, evidence, realm))
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
        if evidence.site == UNSITED {
            return self.resume_unsited_with_nullary(id, realm, response, table);
        }
        if self.machine.realm_cancel_handle(realm).is_cancelled() {
            return Err(PreparedRuntimeError::Cancelled);
        }
        let site = evidence.site;
        let (programs, machine) = (&self.programs, &mut self.machine);
        let owner = programs
            .get(&evidence.owner)
            .ok_or(PreparedRuntimeError::Run(ExecutionError::UnknownProgram(
                evidence.owner,
            )))?;
        let row = owner
            .sites
            .iter()
            .find(|row| row.site == site)
            .ok_or(PreparedRuntimeError::UnknownSite { site })?;
        if row.delivery != SiteDelivery::HostAnswer {
            return Err(PreparedRuntimeError::AnswerDelivery {
                site,
                delivery: row.delivery,
            });
        }
        let mut builder = machine
            .managed_builder()
            .map_err(PreparedRuntimeError::Run)?;
        let root = build_structural_node(response, table, site, row.wire, owner, &mut builder)?;
        let answer = builder
            .finish(realm, root)
            .map_err(PreparedRuntimeError::Run)?;
        self.resume_parked(id, answer)
    }

    /// The one host-built answer an open-reply frame ([`UNSITED`]) accepts:
    /// a field-less constructor. The frame carries no wire evidence, so the
    /// constructor is built from the machine's authenticated descriptors
    /// alone; anything with a field is [`PreparedRuntimeError::UnsitedAnswer`]
    /// and the frame stays parked. `Tidepool.Actor.statefulLoop` parks its
    /// receive this way on purpose (its reply type `Maybe state` is open
    /// until the handler runs) and a drain resumes it with `Nothing`.
    fn resume_unsited_with_nullary(
        &mut self,
        id: ContinuationId,
        realm: RealmId,
        response: &dyn tidepool_bridge::ToHaskell,
        table: &DataConTable,
    ) -> Result<PreparedResumed, PreparedRuntimeError> {
        let host_id =
            nullary_constructor_of(response, table).ok_or(PreparedRuntimeError::UnsitedAnswer)?;
        if self.machine.realm_cancel_handle(realm).is_cancelled() {
            return Err(PreparedRuntimeError::Cancelled);
        }
        let mut builder = self
            .machine
            .managed_builder()
            .map_err(PreparedRuntimeError::Run)?;
        let root = builder
            .constructor(host_id, &[])
            .map_err(PreparedRuntimeError::Run)?;
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

    /// Hand a run result to the session binding store: the handle moves into
    /// the machine's ROOT scope (no resource-scope close releases it) and its
    /// persistent root slot is returned for the binding to load through.
    pub fn adopt(
        &mut self,
        handle: PreparedHandle,
    ) -> Option<tidepool_codegen::old_space::RootSlot> {
        self.machine
            .adopt_handle(handle)
            .then(|| self.machine.handle_root(handle))
            .flatten()
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
        self.machine
            .import_parcel(parcel, realm)
            .map(|(handle, imports)| (handle.raw(), imports))
            .map_err(PreparedRuntimeError::Run)
    }

    /// The persistent root slot behind a retained handle, by its bare
    /// cross-engine [`ValueHandle`] id -- `ResidentSession::run_rooted_entry`'s
    /// slot lookup, which only ever holds a `RootCustody`'s raw id (see
    /// [`Self::discard_handle`]'s doc for the same shape).
    #[must_use]
    pub fn handle_slot(
        &self,
        handle: ValueHandle,
    ) -> Option<tidepool_codegen::old_space::RootSlot> {
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
            if let Some(facts) = self.programs.remove(program) {
                self.retire_site_witnesses(*program, &facts);
            }
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

    /// Site and verb witnesses `retired` canonically owned: each moves to a
    /// still-installed program that declares a structurally equivalent row
    /// for the same site id (or request constructor) ([`sites_equivalent`]),
    /// or is dropped if none remains -- a later install can re-claim it
    /// fresh.
    fn retire_site_witnesses(&mut self, retired: ProgramId, facts: &ProgramFacts) {
        let owned: Vec<u64> = self
            .sites
            .iter()
            .filter(|(_, witness)| witness.owner == retired)
            .map(|(site, _)| *site)
            .collect();
        for site in owned {
            let Some(row) = facts.sites.iter().find(|row| row.site == site) else {
                self.sites.remove(&site);
                continue;
            };
            let successor = self
                .programs
                .iter()
                .find_map(|(candidate, candidate_facts)| {
                    candidate_facts
                        .sites
                        .iter()
                        .position(|candidate_row| {
                            candidate_row.site == site
                                && sites_equivalent(facts, row, candidate_facts, candidate_row)
                        })
                        .map(|row_index| (*candidate, row_index))
                });
            match successor {
                Some((owner, row)) => {
                    self.sites.insert(site, SiteWitness { owner, row });
                }
                None => {
                    self.sites.remove(&site);
                }
            }
        }
        let owned: Vec<(DataConId, usize)> = self
            .verb_sites
            .iter()
            .filter(|(_, witness)| witness.owner == retired)
            .map(|(host_id, witness)| (*host_id, witness.row))
            .collect();
        for (host_id, row) in owned {
            let Some(row) = facts.sites.get(row) else {
                self.verb_sites.remove(&host_id);
                continue;
            };
            let successor = self
                .programs
                .iter()
                .find_map(|(candidate, candidate_facts)| {
                    candidate_facts
                        .verb_sites
                        .iter()
                        .find(|(candidate_host, candidate_row)| {
                            *candidate_host == host_id
                                && sites_equivalent(
                                    facts,
                                    row,
                                    candidate_facts,
                                    &candidate_facts.sites[*candidate_row],
                                )
                        })
                        .map(|(_, candidate_row)| SiteWitness {
                            owner: *candidate,
                            row: *candidate_row,
                        })
                });
            match successor {
                Some(witness) => {
                    self.verb_sites.insert(host_id, witness);
                }
                None => {
                    self.verb_sites.remove(&host_id);
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
    use tidepool_repr::DataCon;
    use tidepool_repr::SessionModule;

    fn certified_source_group(
        name: &str,
        ordinal: u32,
        imported: &str,
    ) -> tidepool_repr::execution_schema::CertifiedGroup {
        use tidepool_repr::execution_schema::{
            CachedHomeOwner, CertifiedGroup, ImportOwner, ModuleVersion,
        };
        let mut wire = testing::wire_program();
        if let Group::NonRecursive(top) = &mut wire.bindings[0] {
            top.identity = testing::identity("Fixture", name);
        }
        let binder = testing::identity("Fixture", imported);
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
                module: "Fixture".into(),
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

    fn settled_test_facts(declarations: &[(&str, &str, DataConId)]) -> ProgramFacts {
        let constructors = declarations
            .iter()
            .map(|(unit, occurrence, host_id)| {
                let mut identity = testing::identity("Tidepool.Internal.Resume", occurrence);
                identity.unit = (*unit).into();
                identity.namespace = "constructor".into();
                let mut family = testing::identity("Tidepool.Internal.Resume", "Settled");
                family.unit = (*unit).into();
                family.namespace = "type".into();
                (identity, *host_id, family)
            })
            .collect::<Vec<_>>();
        let by_identity = constructors
            .iter()
            .map(|(identity, host_id, _)| {
                (
                    (identity.module.clone(), identity.occurrence.clone()),
                    *host_id,
                )
            })
            .collect();
        ProgramFacts {
            entry: None,
            tops: BTreeMap::new(),
            settled: SettledIds::of(&constructors),
            resume: None,
            apply_entry: None,
            apply_value: None,
            sites: Vec::new(),
            types: Vec::new(),
            verb_sites: Vec::new(),
            constructors,
            json_layout: None,
            by_identity,
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
        let inherited = BTreeMap::from([(root.clone(), selected.clone())]);
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
            engine.verb_sites.len(),
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
                engine.verb_sites.len(),
            ),
            before_abort,
        );
        assert!(engine.unpin(bootstrap));
    }

    fn install_source_publication_fixture(
        session: &mut super::super::PersistentSession,
        scope: tidepool_codegen::scope::ScopeId,
    ) -> (
        ProgramId,
        Vec<tidepool_codegen::binding_table::SourceLeaseKey>,
    ) {
        use tidepool_codegen::prepared_program::GroupInventory;
        use tidepool_repr::execution_schema::{ImportOwner, ModuleVersion};

        let groups = [
            certified_source_group("a", 2, "b"),
            certified_source_group("b", 7, "a"),
        ];
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
        let Group::NonRecursive(entry) = &mut wire.bindings[0] else {
            unreachable!("fixture has one entry")
        };
        entry.identity.unit = "main".into();
        wire.globals.push(GlobalDecl {
            identity: root.binder.clone(),
            rep: RuntimeRep::LiftedRef,
            entry_signature: None,
            required_evaluated: false,
            required_generation: None,
        });
        let target =
            CertifiedTargetImage::compile(testing::prepare(wire).unwrap(), &registry).unwrap();
        let owner = ImportOwner::Source {
            version: root.version.clone(),
            binder: root.binder.clone(),
        };
        let source_evidence = BTreeMap::from([
            (
                root,
                (groups[0].owner().clone(), groups[0].original_ordinal()),
            ),
            (
                SourceBinder {
                    version: ModuleVersion([1; 32]),
                    binder: testing::identity("Fixture", "b"),
                },
                (groups[1].owner().clone(), groups[1].original_ordinal()),
            ),
        ]);
        session
            .install_certified_turn_in(
                scope,
                target,
                &[owner],
                &source_evidence,
                demand.compile(&registry).unwrap(),
                &[],
            )
            .unwrap()
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
        lib.attach_recovery_graph_v2(&path).unwrap();
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
        let (target, mut keys) = install_source_publication_fixture(&mut session, private);
        keys.sort();
        assert_eq!(keys.len(), 2);
        assert!(session.prepared_mut().unwrap().unpin(target));
        let incarnation = session
            .public_visibility_snapshot_in(private)
            .unwrap()
            .machine_incarnation
            .unwrap();
        assert!(matches!(
            session.snapshot_publication(owner.clone(), public, foreign, vec![], keys.clone()),
            Err(SessionError::InvalidPublicBindingPromotion(
                BindingPromotionError::MissingOrForeignSourceInstance
            ))
        ));

        let cancel_stage = session
            .snapshot_publication(owner.clone(), public, private, vec![], keys.clone())
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
            .snapshot_publication(owner.clone(), public, private, vec![], keys.clone())
            .unwrap()
            .stage()
            .unwrap();
        let old_stage = session
            .snapshot_publication(owner.clone(), public, private, vec![], keys.clone())
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
        assert_eq!(published.source_instances, keys);
        let graph = super::super::recovery::read_v2(&path, root.path())
            .unwrap()
            .unwrap()
            .graph;
        let surface = graph
            .public_surfaces
            .iter()
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
            keys
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
            &existing,
            &HashMap::new(),
            &BindingTable::new(),
        );
        assert!(
            matches!(
                &bad_result,
                Err(PreparedRuntimeError::Run(
                    ExecutionError::BatchSourceContract(_)
                ))
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
                &existing,
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
                },
                target(binder.clone(), 1, false),
            ),
            (
                ImportOwner::CodeExport {
                    binder: wrong_identity.clone(),
                    generation: 0,
                    root_id,
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
            Err(PreparedRuntimeError::Run(
                ExecutionError::BatchSourceContract(_)
            ))
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
            PreparedRuntimeError::Run(ExecutionError::MissingEntry(ValueId(999)))
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
            CertifiedGroup::admit(
                CachedHomeOwner {
                    unit: "fixture".into(),
                    module: "Fixture".into(),
                    module_version: ModuleVersion([1; 32]),
                    skinny_iface_sha256: [2; 32],
                    product_sha256: [3; 32],
                },
                testing::projected_group(wire, 2).unwrap(),
                vec![package_owner(digest)],
            )
            .unwrap()
        };
        let registry = ImageRegistry::new();
        let target = || {
            let mut wire = testing::wire_program();
            let Group::NonRecursive(mut package_top) = wire.bindings[0].clone() else {
                unreachable!()
            };
            package_top.identity = package.clone();
            package_top.binding.id = ValueId(1);
            let HeapRhs::Function { body, .. } = &mut package_top.binding.rhs else {
                unreachable!()
            };
            *body = 1;
            wire.expressions
                .nodes
                .push(wire.expressions.nodes[0].clone());
            let mut optional_top = package_top.clone();
            optional_top.identity = optional.clone();
            optional_top.binding.id = ValueId(2);
            let HeapRhs::Function { body, .. } = &mut optional_top.binding.rhs else {
                unreachable!()
            };
            *body = 2;
            wire.expressions
                .nodes
                .push(wire.expressions.nodes[1].clone());
            wire.bindings.push(Group::NonRecursive(package_top));
            wire.bindings.push(Group::NonRecursive(optional_top));
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
            target(), std::slice::from_ref(&source_owner), &evidence, selected(&good), &[],
            &BTreeMap::new(), &HashMap::new(), &BindingTable::new(),
        ), Err(PreparedRuntimeError::MissingCertifiedOwner(owner)) if owner == package_owner([9; 32])));
        assert_eq!(engine.residency(), before);
        let admitted = || BTreeMap::from([(package.clone(), (ValueId(1), [9; 32]))]);
        let install = |engine: &mut PreparedEngine, group: &CertifiedGroup| {
            engine.install_certified_turn_admitted(
                target(),
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
            Err(PreparedRuntimeError::Run(
                ExecutionError::BatchSourceContract(_)
            ))
        ));
        assert_eq!(engine.residency(), before);
        assert!(engine.code_exports.is_empty());
        assert!(engine.programs.is_empty());
        let wrong_digest = group(false, [8; 32]);
        assert!(matches!(install(&mut engine, &wrong_digest),
            Err(PreparedRuntimeError::MissingCertifiedOwner(owner)) if owner == package_owner([8; 32])));
        assert_eq!(engine.residency(), before);
        let mut aborted = install(&mut engine, &good).unwrap();
        assert_eq!(aborted.exports.len(), 2);
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
            Err(PreparedRuntimeError::MissingCertifiedOwner(owner))
                if owner == package_owner([8; 32])
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

    pub(in crate::session) fn rooted_publication_fixture(
        state: &mut super::super::PersistentSession,
        name: &str,
        generation: u64,
    ) -> BindingEntry {
        let producer = producer_program();
        let top = producer.entry();
        let program = state
            .install_prepared(producer)
            .expect("install fixed producer");
        let engine = state.prepared_mut().expect("installed fixture machine");
        let handle = engine
            .machine
            .retain_top(program, top)
            .expect("retain fixture top");
        let root = engine.adopt(handle).expect("adopt real fixture root");
        BindingEntry {
            name: tidepool_repr::BindingName(name.into()),
            id: SessionVarId::from_extract(generation),
            module: SessionModule::val(tidepool_repr::Generation(generation)),
            value: BoundValue {
                root,
                handle,
                identity: producer_identity(),
            },
            type_display: None,
            defining_expr: None,
            scope: tidepool_codegen::scope::ScopeId::ROOT,
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
            .map_err(PreparedRuntimeError::Run)
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
        let root = engine.adopt(handle).expect("retained handle adopts a root");
        let entry = BindingEntry {
            name: tidepool_repr::BindingName("producer".into()),
            id: SessionVarId::from_extract(1),
            module: SessionModule::val(tidepool_repr::Generation(1)),
            value: BoundValue {
                root,
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
        let split_program = split
            .revalidate_and_install(snapshot, compiled, &bindings, &index)
            .expect("revalidation runs")
            .expect("nothing changed the import between snapshot and revalidation");
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

    fn mount_table_row(id: u64, name: &str, arity: u32, qualified_name: Option<&str>) -> DataCon {
        DataCon {
            id: DataConId(id),
            name: name.into(),
            tag: 1,
            rep_arity: arity,
            field_bangs: Vec::new(),
            qualified_name: qualified_name.map(str::to_owned),
            type_name: String::new(),
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
        wire.types = vec![
            TypeNode::Data {
                family: json_value_family,
                arguments: vec![],
                rows: vec![
                    CtorRow {
                        constructor: ConstructorId(1),
                        fields: vec![TypeNodeId(0)],
                    },
                    CtorRow {
                        constructor: ConstructorId(2),
                        fields: vec![TypeNodeId(0)],
                    },
                    CtorRow {
                        constructor: ConstructorId(3),
                        fields: vec![TypeNodeId(0)],
                    },
                    CtorRow {
                        constructor: ConstructorId(4),
                        fields: vec![TypeNodeId(0)],
                    },
                    CtorRow {
                        constructor: ConstructorId(5),
                        fields: vec![TypeNodeId(0)],
                    },
                    CtorRow {
                        constructor: ConstructorId(6),
                        fields: vec![],
                    },
                ],
            },
            TypeNode::Scalar(RuntimeRep::Int(64)),
            TypeNode::Data {
                family: {
                    let mut family = testing::identity("Fixture.Mount", "Framed");
                    family.namespace = "type".into();
                    family
                },
                arguments: vec![],
                rows: vec![CtorRow {
                    constructor: ConstructorId(22),
                    fields: vec![TypeNodeId(1), TypeNodeId(0)],
                }],
            },
        ];
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
        testing::prepare(wire).expect("JSON mount fixture")
    }

    fn json_mount_table() -> DataConTable {
        let mut table = DataConTable::new();
        for (id, name, arity, qualified_name) in [
            (100, "Object", 1, Some("Tidepool.Aeson.Value.Object")),
            (101, "Array", 1, Some("Tidepool.Aeson.Value.Array")),
            (102, "String", 1, Some("Tidepool.Aeson.Value.String")),
            (103, "Number", 1, Some("Tidepool.Aeson.Value.Number")),
            (104, "Bool", 1, Some("Tidepool.Aeson.Value.Bool")),
            (105, "Null", 0, Some("Tidepool.Aeson.Value.Null")),
            (
                110,
                "Scientific",
                2,
                Some("Tidepool.Aeson.Scientific.Scientific"),
            ),
            (120, "IS", 1, Some("GHC.Num.Integer.IS")),
            (121, "IP", 1, Some("GHC.Num.Integer.IP")),
            (122, "IN", 1, Some("GHC.Num.Integer.IN")),
            (130, "True", 0, Some("GHC.Types.True")),
            (131, "False", 0, Some("GHC.Types.False")),
            (140, "Bin", 5, Some("Data.Map.Internal.Bin")),
            (141, "Tip", 0, Some("Data.Map.Internal.Tip")),
            (150, "I#", 1, Some("GHC.Types.I#")),
            (160, "Text", 3, Some("Data.Text.Internal.Text")),
            (170, ":", 2, Some("GHC.Types.:")),
            (171, "[]", 0, Some("GHC.Types.[]")),
            (903, "Framed", 2, Some("Fixture.Mount.Framed")),
        ] {
            table
                .insert_checked(mount_table_row(id, name, arity, qualified_name))
                .unwrap();
        }
        table
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

        let mut wrong_text = DataConTable::new();
        wrong_text
            .insert_checked(mount_table_row(900, "Text", 3, None))
            .unwrap();
        let error = engine
            .build_host_text(RealmId::ROOT, "must not publish", &wrong_text)
            .expect_err("wrong Text descriptor is rejected after byte construction");
        assert!(matches!(error, PreparedRuntimeError::HostMount { .. }));
        assert_eq!(engine.handle_count(), initial_handles);
        assert_eq!(engine.persistent_roots_count(), initial_roots);

        let text = engine
            .build_host_text(RealmId::ROOT, "reusable after rejection", &table)
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
                        owner: program,
                        site: 7,
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
                        owner: program,
                        site: 8,
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

    /// A program that constructs nothing but declares an effect request
    /// constructor (bridge id 77) answered at synthetic site `site` whose
    /// reply evidence is `reply`.
    fn verb_program(site: u64, reply: TypeNode) -> PreparedProgram {
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
        wire.types = vec![reply];
        wire.sites = vec![SiteRow {
            site,
            origin: "Fixture.Effects.Print".into(),
            ordinal: 0,
            delivery: SiteDelivery::HostAnswer,
            wire: TypeNodeId(0),
            inputs: vec![],
        }];
        wire.verb_sites = vec![(ConstructorId(0), site)];
        testing::prepare(wire).expect("verb fixture validates")
    }

    fn typed_site_program(
        site: u64,
        types: Vec<TypeNode>,
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
    fn installed_sites_compare_request_input_and_reply_across_programs() {
        let mut response_family =
            testing::identity("Tidepool.Agent.Reply.Internal", "ResponseResult");
        response_family.namespace = "type".into();
        let (source, _) = PreparedEngine::bootstrap(typed_site_program(
            41,
            vec![
                TypeNode::Text,
                TypeNode::Integer,
                TypeNode::Data {
                    family: response_family.clone(),
                    arguments: vec![TypeNodeId(1)],
                    rows: vec![],
                },
            ],
            2,
            &[0],
        ))
        .expect("install request site");
        let evidence = source
            .request_site_type_evidence(41)
            .expect("capture source site before crossing sessions");
        let (recipient, _) = PreparedEngine::bootstrap(typed_site_program(
            42,
            vec![
                TypeNode::Integer,
                TypeNode::Natural,
                TypeNode::Text,
                TypeNode::Data {
                    family: response_family.clone(),
                    arguments: vec![TypeNodeId(0)],
                    rows: vec![],
                },
            ],
            1,
            &[2, 0, 3],
        ))
        .expect("install accessor in a different machine");

        assert!(recipient.request_scope_types_match(&evidence, 42));
        assert!(!recipient.request_scope_types_match(&evidence, 41));
        let (wrong_input, _) = PreparedEngine::bootstrap(typed_site_program(
            44,
            vec![
                TypeNode::Integer,
                TypeNode::Text,
                TypeNode::Data {
                    family: response_family,
                    arguments: vec![TypeNodeId(0)],
                    rows: vec![],
                },
            ],
            0,
            &[0, 0, 2],
        ))
        .expect("install wrong-input accessor");
        assert!(!wrong_input.request_scope_types_match(&evidence, 44));
        let (wrong, _) = PreparedEngine::bootstrap(typed_site_program(
            43,
            vec![TypeNode::Text, TypeNode::Integer],
            1,
            &[0, 0, 1],
        ))
        .expect("install wrong accessor");
        assert!(!wrong.request_scope_types_match(&evidence, 43));
    }

    #[test]
    fn programs_declaring_the_same_effect_constructor_share_one_verb_witness() {
        use tidepool_repr::execution_schema::SYNTHETIC_SITE_BIT;
        let site = SYNTHETIC_SITE_BIT | 5;
        let (mut engine, first) =
            PreparedEngine::bootstrap(verb_program(site, TypeNode::Text)).expect("bootstrap");
        let bindings = BindingTable::new();
        let index = BindingIndex::new();
        let second = engine
            .install(verb_program(site, TypeNode::Text), &bindings, &index)
            .expect("an equivalent duplicate is not a SiteConflict");
        assert_ne!(first, second);
        let witness = engine.verb_sites[&DataConId(77)];
        assert_eq!(witness.owner, first, "the existing owner stays canonical");
        assert_eq!(engine.sites[&site].owner, first);

        // The same constructor answered by different evidence under another
        // synthetic id is refused by the verb index, and installs nothing.
        let error = engine
            .install(
                verb_program(SYNTHETIC_SITE_BIT | 6, TypeNode::Integer),
                &bindings,
                &index,
            )
            .expect_err("a conflicting verb reply refuses the install");
        assert!(
            matches!(error, PreparedRuntimeError::SiteConflict { owner, .. } if owner == first),
            "expected SiteConflict, got {error:?}"
        );
        assert_eq!(engine.programs.len(), 2);
        assert!(!engine.sites.contains_key(&(SYNTHETIC_SITE_BIT | 6)));
    }

    #[test]
    fn alpha_stable_polymorphic_reply_evidence_installs_without_weakening_conflicts() {
        use tidepool_repr::execution_schema::SYNTHETIC_SITE_BIT;
        let site = SYNTHETIC_SITE_BIT | 7;
        let stable = TypeNode::Unconstructible {
            reason: "polymorphic".into(),
            rendered: "a".into(),
        };
        let (mut engine, first) =
            PreparedEngine::bootstrap(verb_program(site, stable.clone())).expect("bootstrap");
        let bindings = BindingTable::new();
        let index = BindingIndex::new();
        let second = engine
            .install(verb_program(site, stable), &bindings, &index)
            .expect("alpha-stable polymorphic evidence is equivalent");
        assert_ne!(first, second);

        let different = TypeNode::Unconstructible {
            reason: "polymorphic".into(),
            rendered: "a_unique".into(),
        };
        let error = engine
            .install(verb_program(site, different), &bindings, &index)
            .expect_err("different type evidence must remain a conflict");
        assert!(
            matches!(error, PreparedRuntimeError::SiteConflict { owner, .. } if owner == first),
            "expected SiteConflict, got {error:?}"
        );
        assert_eq!(engine.programs.len(), 2);
    }

    #[test]
    fn off_checkout_install_reuses_code_already_installed_on_the_machine() {
        use tidepool_repr::execution_schema::SYNTHETIC_SITE_BIT;
        let registry = Arc::new(ImageRegistry::new());
        let (mut engine, _) =
            PreparedEngine::bootstrap(verb_program(SYNTHETIC_SITE_BIT | 40, TypeNode::Text))
                .expect("bootstrap");
        engine.set_image_registry(Arc::clone(&registry));
        let bindings = BindingTable::new();
        let index = BindingIndex::new();
        let prepared = verb_program(SYNTHETIC_SITE_BIT | 41, TypeNode::Text);
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
        assert!(Arc::ptr_eq(
            &shared,
            &registry
                .lookup(&key)
                .expect("installed code remains shared")
        ));
    }

    #[test]
    fn two_engines_sharing_one_registry_the_second_install_is_a_registry_hit() {
        use tidepool_repr::execution_schema::SYNTHETIC_SITE_BIT;
        let registry = Arc::new(ImageRegistry::new());
        let bootstrap_site = SYNTHETIC_SITE_BIT | 30;
        let (mut engine_a, _) =
            PreparedEngine::bootstrap(verb_program(bootstrap_site, TypeNode::Text))
                .expect("engine a bootstraps");
        let (mut engine_b, _) =
            PreparedEngine::bootstrap(verb_program(bootstrap_site, TypeNode::Text))
                .expect("engine b bootstraps its own, independent machine");
        engine_a.set_image_registry(Arc::clone(&registry));
        engine_b.set_image_registry(Arc::clone(&registry));

        let bindings = BindingTable::new();
        let index = BindingIndex::new();
        let shared_site = SYNTHETIC_SITE_BIT | 31;

        engine_a
            .install(verb_program(shared_site, TypeNode::Text), &bindings, &index)
            .expect("engine a compiles and registers the image");
        assert_eq!(
            registry.misses(),
            1,
            "engine a's install is a registry miss"
        );
        assert_eq!(registry.hits(), 0);

        engine_b
            .install(verb_program(shared_site, TypeNode::Text), &bindings, &index)
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
        lib.attach_recovery_graph_v2(&manifest).unwrap();
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
            let root = engine.adopt(handle).unwrap();
            state
                .bind_in(
                    scope,
                    BindingEntry {
                        name: tidepool_repr::BindingName(name.into()),
                        id: SessionVarId::from_extract(id),
                        module: SessionModule::val(Generation(id)),
                        value: BoundValue {
                            root,
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
}
