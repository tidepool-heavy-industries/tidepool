//! GHC-authoritative inspection of the exact source environment used by a
//! resident workbench turn.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use ciborium::value::Value as CborValue;
use tempfile::TempDir;
use tidepool_extract_cmd::{ExtractCmd, SpawnError};

use crate::{timing, CompileError};

use super::assemble_inspection_module;

/// Descriptive request identity observed by a query; never request authority.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct InspectionRequestIdentity(u64);

impl InspectionRequestIdentity {
    pub const fn new(identity: u64) -> Self {
        Self(identity)
    }
}

/// Small actor facts captured by its source owner alongside the runtime view.
/// This description grants no runtime or compiler admission.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InspectionActorObservation {
    principal: tidepool_repr::PrincipalId,
    effects: super::HaskellTypeSource,
    request: Option<InspectionRequestIdentity>,
    model_type: Option<super::HaskellTypeSource>,
}

impl InspectionActorObservation {
    pub fn new(
        principal: tidepool_repr::PrincipalId,
        effects: super::HaskellTypeSource,
        request: Option<InspectionRequestIdentity>,
        model_type: Option<super::HaskellTypeSource>,
    ) -> Self {
        Self {
            principal,
            effects,
            request,
            model_type,
        }
    }

    /// Compare actor/incarnation, effect source/imports and model TYPE only.
    /// This is not request-identity proof: the actual Workbench-issued pair
    /// separately binds the original request and its exact TYPE evidence.
    pub fn same_actor_context(&self, other: &Self) -> bool {
        self.principal == other.principal
            && self.effects == other.effects
            && self.model_type == other.model_type
    }

    fn frame(&self, frame: &mut impl FnMut(&[u8])) {
        fn ty(source: &super::HaskellTypeSource, frame: &mut impl FnMut(&[u8])) {
            frame(source.expression().as_bytes());
            frame(&(source.required_imports().specs().len() as u64).to_le_bytes());
            for import in source.required_imports().specs() {
                frame(import.as_bytes());
            }
        }
        frame(&self.principal.identity.to_le_bytes());
        frame(&self.principal.incarnation.to_le_bytes());
        ty(&self.effects, frame);
        match self.request {
            Some(request) => {
                frame(b"request");
                frame(&request.0.to_le_bytes());
            }
            None => frame(b"no-request"),
        }
        match &self.model_type {
            Some(source) => {
                frame(b"model-type");
                ty(source, frame);
            }
            None => frame(b"no-model-type"),
        }
    }
}

/// An immutable observational snapshot of one runtime base and the actor's
/// exact import/projection membrane. It carries no admission authority. Its
/// existing cached base digest is resolved only when an answer needs the pin.
/// The base includes owner state hidden from authored imports, so observations
/// may conservatively differ even when visible name lists are unchanged.
pub struct InspectionScopeSnapshot {
    pub(super) base: std::sync::Arc<super::persistent::CachedCompileView>,
    pub(super) generation: u64,
    pub(super) runtime_scope: tidepool_codegen::scope::ScopeId,
    pub(super) source_revision: std::sync::Arc<[u8]>,
    pub(super) selected_imports: super::SourceImports,
    pub(super) inputs: super::RuntimeCompileInputs,
    pub(super) actor: Option<InspectionActorObservation>,
    pub(super) scope: NameScope,
}

impl InspectionScopeSnapshot {
    pub fn provenance(&self) -> ScopeProvenance {
        inspection_provenance(
            self.base.digest(),
            self.generation,
            &self.source_revision,
            &self.selected_imports,
            &self.inputs,
            self.actor.as_ref(),
            self.scope.clone(),
        )
    }
}

/// Correlate observations without granting admission authority. The base is
/// the runtime owner's existing cached semantic commitment; the actor inputs
/// describe the deterministic import/projection membrane applied to that base.
pub(super) fn inspection_provenance(
    base: [u8; 32],
    generation: u64,
    source_revision: &[u8],
    selected_imports: &super::SourceImports,
    inputs: &super::RuntimeCompileInputs,
    actor: Option<&InspectionActorObservation>,
    scope: NameScope,
) -> ScopeProvenance {
    let mut fingerprint = blake3::Hasher::new();
    let mut frame = |bytes: &[u8]| {
        fingerprint.update(&(bytes.len() as u64).to_le_bytes());
        fingerprint.update(bytes);
    };
    frame(b"runtime-inspection-view-v2");
    frame(&base);
    frame(source_revision);
    frame(&serde_json::to_vec(selected_imports.specs()).expect("source imports serialize"));
    inputs.frame_authorization(&mut frame);
    match actor {
        Some(actor) => {
            frame(b"actor");
            actor.frame(&mut frame);
        }
        None => frame(b"runtime-only"),
    }
    ScopeProvenance {
        scope,
        generation,
        fingerprint: fingerprint.finalize().to_hex().to_string(),
    }
}

#[derive(Clone, Debug)]
pub enum InspectionQuery {
    TypeOf(String),
    /// No production query plan constructs this; the resident workbench uses
    /// `StructuredInfo`. It stays because `inspection`'s execution test still
    /// drives the shared-environment assembly and the worker's `InspectNameInfo`
    /// arm, which remains wire-reachable even when the runtime does not plan it.
    Info(String),
    TypeSearch(String),
    /// Type search whose encoded match list stops at `budget`. The search
    /// itself still examines every candidate so an answer stays complete.
    TypeSearchBounded {
        pattern: String,
        budget: usize,
    },
    ScopeBrowse,
    Browse {
        module: String,
        expanded: bool,
    },
    StructuredInfo {
        query: NameQuery,
        provenance: ScopeProvenance,
    },
    StructuredType {
        query: NameQuery,
        provenance: ScopeProvenance,
    },
}

/// Query equality keeps full structural comparison except for the compile
/// view a structured query was planned against: two plans over the same view
/// identity are the same work even when their scope labels differ.
impl PartialEq for InspectionQuery {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::TypeOf(left), Self::TypeOf(right)) => left == right,
            (Self::Info(left), Self::Info(right)) => left == right,
            (Self::TypeSearch(left), Self::TypeSearch(right)) => left == right,
            (
                Self::TypeSearchBounded {
                    pattern: left,
                    budget: left_budget,
                },
                Self::TypeSearchBounded {
                    pattern: right,
                    budget: right_budget,
                },
            ) => left == right && left_budget == right_budget,
            (Self::ScopeBrowse, Self::ScopeBrowse) => true,
            (
                Self::Browse {
                    module: left,
                    expanded: left_expanded,
                },
                Self::Browse {
                    module: right,
                    expanded: right_expanded,
                },
            ) => left == right && left_expanded == right_expanded,
            (
                Self::StructuredInfo {
                    query: left,
                    provenance: left_provenance,
                },
                Self::StructuredInfo {
                    query: right,
                    provenance: right_provenance,
                },
            ) => left == right && left_provenance.same_view(right_provenance),
            (
                Self::StructuredType {
                    query: left,
                    provenance: left_provenance,
                },
                Self::StructuredType {
                    query: right,
                    provenance: right_provenance,
                },
            ) => left == right && left_provenance.same_view(right_provenance),
            _ => false,
        }
    }
}

impl Eq for InspectionQuery {}

use tidepool_toolchain::inspection::{
    decode_identifier_ref, decode_scope, decode_structured_result, decode_type_match,
    StructuredReply,
};
pub use tidepool_toolchain::inspection::{
    ClassMethodInfo, ConstructorInfo, DeclarationInfo, FieldInfo, IdentifierInfo,
    IdentifierNamespace, IdentifierRef, InspectionAvailability, NameNamespace, NameQuery,
    NameScope, QueryError, ScopeProvenance, TypeExpression, TypeInfo, TypeMatch, TypeMatchQuality,
};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InfoEntry {
    pub name: String,
    pub module: Option<String>,
    pub kind: String,
    pub display: String,
    pub availability: InspectionAvailability,
    pub references: Vec<IdentifierRef>,
    /// The entry's canonical resolved identity. `references` are the
    /// identifiers the declaration mentions or resolves; this is the entry
    /// itself, so a local and a global sharing `(name, module, kind)` remain
    /// distinguishable.
    pub identity: IdentifierRef,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum InspectionResult {
    Type {
        expression: String,
        display: String,
        availability: InspectionAvailability,
    },
    Info {
        query: String,
        entries: Vec<InfoEntry>,
    },
    Ambiguous {
        query: String,
        entries: Vec<InfoEntry>,
    },
    NotFound {
        query: String,
    },
    ModuleNotFound {
        module: String,
        scopes: Vec<NameScope>,
    },
    Rejected {
        diagnostic: String,
    },
    Browse {
        module: String,
        expanded: bool,
        entries: Vec<InfoEntry>,
    },
    TypeMatches {
        query: String,
        matches: Vec<TypeMatch>,
    },
    StructuredInfo(Result<Box<IdentifierInfo>, QueryError>),
    StructuredType(Result<TypeInfo, QueryError>),
}

impl InspectionResult {
    #[must_use]
    pub fn render(&self) -> String {
        match self {
            Self::Type {
                expression,
                display,
                ..
            } => format!("{expression} :: {display}"),
            Self::Info { entries, .. } => entries
                .iter()
                .map(|entry| entry.display.trim())
                .collect::<Vec<_>>()
                .join("\n\n"),
            Self::Ambiguous { query, entries } => {
                let candidates = entries
                    .iter()
                    .map(|entry| match &entry.module {
                        Some(module) => format!("{module}.{}", entry.name),
                        None => entry.name.clone(),
                    })
                    .collect::<Vec<_>>()
                    .join(", ");
                format!("ambiguous name `{query}`; candidates: {candidates}")
            }
            Self::NotFound { query } => format!("unknown name `{query}`"),
            Self::ModuleNotFound { module, .. } => format!("unknown module `{module}`"),
            Self::Rejected { diagnostic } => diagnostic.clone(),
            Self::Browse {
                module, entries, ..
            } => {
                let declarations = entries
                    .iter()
                    .map(|entry| entry.display.trim())
                    .collect::<Vec<_>>()
                    .join("\n");
                if declarations.is_empty() {
                    format!("-- {module}")
                } else {
                    format!("-- {module}\n{declarations}")
                }
            }
            Self::StructuredInfo(Ok(info)) => format!(
                "{} -- {}.{}",
                render_declaration(&info.declaration),
                info.identifier.module,
                info.identifier.name
            ),
            Self::StructuredType(Ok(info)) => format!(
                "{}.{} :: {}",
                info.identifier.module, info.identifier.name, info.expression.canonical
            ),
            Self::StructuredInfo(Err(error)) | Self::StructuredType(Err(error)) => {
                render_query_error(error)
            }
            Self::TypeMatches { matches, .. } => matches
                .iter()
                .map(|entry| format!("{} :: {}", entry.name, entry.signature))
                .collect::<Vec<_>>()
                .join("\n"),
        }
    }
}

fn render_declaration(declaration: &DeclarationInfo) -> String {
    match declaration {
        DeclarationInfo::Value(ty) => ty.canonical.clone(),
        DeclarationInfo::Data { constructors, .. } => {
            format!("data ({} constructors)", constructors.len())
        }
        DeclarationInfo::Newtype { constructor, .. } => {
            format!("newtype ({})", constructor.identifier.name)
        }
        DeclarationInfo::TypeSynonym { body, .. } => format!("type = {}", body.canonical),
        DeclarationInfo::Class { methods, .. } => format!("class ({} methods)", methods.len()),
        DeclarationInfo::Constructor { constructor, .. } => constructor.ty.canonical.clone(),
        DeclarationInfo::RecordSelector { ty, .. } => ty.canonical.clone(),
    }
}

fn render_query_error(error: &QueryError) -> String {
    match error {
        QueryError::Unknown(query) => format!("unknown name `{}`", query.name),
        QueryError::Ambiguous(query, candidates) => format!(
            "ambiguous name `{}`; candidates: {}",
            query.name,
            candidates
                .iter()
                .map(|candidate| format!("{}.{}", candidate.module, candidate.name))
                .collect::<Vec<_>>()
                .join(", ")
        ),
        QueryError::UnknownModule(module) => format!("unknown module `{module}`"),
        QueryError::Unsupported(detail) => detail.clone(),
    }
}

/// Checked inspection authority captured by the resident scope owner.
/// The input inventory cannot be assembled or changed by a caller.
#[derive(Clone, Debug)]
pub struct AdmittedInspectionInputs {
    catalog: tidepool_toolchain::toolchain::CatalogSelection,
    view: super::SessionCompileView,
    values: Vec<super::admission::AdmittedValueInterface>,
}

impl AdmittedInspectionInputs {
    pub(super) fn capture(
        view: super::SessionCompileView,
        values: Vec<super::admission::AdmittedValueInterface>,
        catalog: tidepool_toolchain::toolchain::CatalogSelection,
    ) -> Self {
        Self {
            view,
            values,
            catalog,
        }
    }

    pub fn view(&self) -> &super::SessionCompileView {
        &self.view
    }

    fn matches_request(&self, request: &InspectionRequest<'_>) -> bool {
        request.session_root == self.view.session_root()
            && match (
                &request.exact_context,
                self.view.exact_compile_context().as_ref(),
            ) {
                (Some(request), Some(current)) => request.same_authenticated_context(current),
                (None, None) => true,
                _ => false,
            }
            && request.inject_modules == self.view.injected_module_names()
    }
}

/// One runtime-owned observation of a published compiler world. A cold capture
/// couples current binding identities to checked rows, separately from the
/// published primary context.
pub struct AdmittedInspectionWorld {
    pub(super) base: InspectionWorldBase,
    pub(super) custody: InspectionWorldCustody,
    pub(super) inputs: InspectionWorldInputs,
    pub(super) observation:
        Option<std::sync::Arc<tidepool_toolchain::compiler_world::WorldObservationScaffold>>,
}

#[derive(Clone)]
pub(super) struct InspectionWorldBase {
    pub(super) scope: tidepool_codegen::scope::ScopeId,
    pub(super) key: super::persistent::CompileViewKey,
    pub(super) owner: std::sync::Arc<super::admission::RuntimeAdmissionOwner>,
    pub(super) epoch: u64,
    pub(super) source: std::sync::Arc<super::GenerationArtifactReference>,
    pub(super) original_source: InspectionWorldSourceObservation,
    pub(super) target: InspectionWorldLeaseObservation,
}

/// A cache observes exact runtime scope identity without keeping that scope
/// alive. The canonical lease and its prerequisite DAG remain the only issuer.
#[derive(Clone)]
pub(super) struct InspectionWorldLeaseObservation(std::sync::Weak<super::RuntimeLexicalScopeLease>);

#[derive(Clone)]
pub(super) struct InspectionWorldSourceObservation(std::sync::Weak<super::RuntimeOriginalSource>);

impl InspectionWorldSourceObservation {
    pub(super) fn new(source: &std::sync::Arc<super::RuntimeOriginalSource>) -> Self {
        Self(std::sync::Arc::downgrade(source))
    }

    pub(super) fn matches(&self, source: &std::sync::Arc<super::RuntimeOriginalSource>) -> bool {
        self.0.ptr_eq(&std::sync::Arc::downgrade(source))
    }

    pub(super) fn same_observation(&self, other: &Self) -> bool {
        self.0.ptr_eq(&other.0)
    }
}

impl InspectionWorldLeaseObservation {
    pub(super) fn new(lease: &std::sync::Arc<super::RuntimeLexicalScopeLease>) -> Self {
        Self(std::sync::Arc::downgrade(lease))
    }

    pub(super) fn matches(&self, lease: &std::sync::Arc<super::RuntimeLexicalScopeLease>) -> bool {
        self.0.ptr_eq(&std::sync::Arc::downgrade(lease))
    }

    pub(super) fn same_observation(&self, other: &Self) -> bool {
        self.0.ptr_eq(&other.0)
    }
}

/// Real work retains both exact leases across capture, compiler END and
/// installation. Installed associations never own this custody pair.
#[derive(Clone)]
pub(super) struct InspectionWorldCustody {
    pub(super) original_source: std::sync::Arc<super::RuntimeOriginalSource>,
    pub(super) target: std::sync::Arc<super::RuntimeLexicalScopeLease>,
}

impl InspectionWorldBase {
    /// Upgrade exact observations for an existing owner to validate. This
    /// retains lifetime only; the caller still owns admission/settlement gates.
    pub(super) fn observed_custody(
        &self,
    ) -> Result<InspectionWorldCustody, super::SourceAdmissionError> {
        let original_source = self
            .original_source
            .0
            .upgrade()
            .ok_or(super::SourceAdmissionError::StaleSourceLease)?;
        let target = self
            .target
            .0
            .upgrade()
            .ok_or(super::SourceAdmissionError::StaleSourceLease)?;
        super::RuntimeOriginalSource::with_sources_and_targets(
            [original_source.clone()],
            [target.clone()],
            |_, _| {
                Ok(InspectionWorldCustody {
                    original_source,
                    target,
                })
            },
        )
    }

    /// New work must acquire actual Open admission for both exact leases.
    pub(super) fn acquire_custody(
        &self,
    ) -> Result<InspectionWorldCustody, super::SourceAdmissionError> {
        let custody = self.observed_custody()?;
        super::RuntimeOriginalSource::with_sources_and_targets(
            [custody.original_source.clone()],
            [custody.target.clone()],
            |sources, _| {
                super::RuntimeSourceAdmission::with_open_admissions(
                    sources.iter().map(|source| source.source_admission()),
                    || custody,
                )
            },
        )
    }
}

pub(super) enum InspectionWorldInputs {
    Published(std::sync::Arc<tidepool_toolchain::compiler_world::CompilerWorld>),
    Cold {
        primary:
            Option<std::sync::Arc<tidepool_toolchain::declaration_join::ExactDeclarationContext>>,
        bindings: tidepool_toolchain::compiler_world::VisibleWorldBindings,
        lexical: tidepool_toolchain::compiler_world::CompilerWorldLexicalSurface,
    },
}

#[derive(Clone)]
pub(crate) struct InspectionWorldAssociation {
    pub(super) base: InspectionWorldBase,
    pub(super) world: std::sync::Arc<tidepool_toolchain::compiler_world::CompilerWorld>,
}

/// Refusals before a service world exists remain distinct from a closed
/// module absence issued by the compiler service.
#[derive(Debug, thiserror::Error)]
pub enum WorldInspectionError {
    #[error("source owner cannot represent this compiler source domain")]
    UnsupportedSource,
    #[error("compiler source unavailable: {0}")]
    SourceUnavailable(String),
    #[error("compiler source unreadable: {0}")]
    SourceUnreadable(String),
    #[error(transparent)]
    Runtime(#[from] crate::RuntimeError),
    #[error("compiler world observation close is unconfirmed")]
    CompilerClose {
        primary: Option<Box<WorldInspectionError>>,
        close: crate::CompilerTransactionClose,
    },
    #[error("compiler world binding has an unrecognized namespace: {0}")]
    UnknownBindingNamespace(String),
}

impl From<CompileError> for WorldInspectionError {
    fn from(error: CompileError) -> Self {
        Self::Runtime(error.into())
    }
}

/// Only the admitted runner constructs this pairing of a runtime base and a
/// compiler-issued world. Callers cannot attach an unrelated service result.
pub struct PreparedInspectionWorld<A = tidepool_toolchain::compiler_world::WorldAnswer> {
    pub(super) association: std::sync::Arc<InspectionWorldAssociation>,
    pub(super) custody: InspectionWorldCustody,
    origin: crate::CompilerTransactionOrigin,
    answer: A,
    provenance: ScopeProvenance,
}

/// A world answer sealed after the exact physical compiler transaction END.
pub struct CompletedInspectionWorld<A = tidepool_toolchain::compiler_world::WorldAnswer> {
    pub(super) prepared: PreparedInspectionWorld<A>,
    pub(super) cancellation: crate::HostAdmissionCancellation,
}

/// A compiler world acquired for one captured runtime base, without a query.
/// Its source relationship stays owned by the runtime throughout compiler END.
pub struct PreparedCompilerWorld {
    association: std::sync::Arc<InspectionWorldAssociation>,
    pub(super) custody: InspectionWorldCustody,
    origin: crate::CompilerTransactionOrigin,
}

/// Only the matching settled compiler owner can issue an installable world.
pub struct CompletedCompilerWorld {
    pub(super) association: std::sync::Arc<InspectionWorldAssociation>,
    pub(super) custody: InspectionWorldCustody,
    pub(super) cancellation: crate::HostAdmissionCancellation,
}

impl CompletedCompilerWorld {
    pub fn world(&self) -> &std::sync::Arc<tidepool_toolchain::compiler_world::CompilerWorld> {
        &self.association.world
    }
}

pub fn run_admitted_compiler_world(
    owner: &super::RuntimeSourceCompilerTransaction,
    admitted: AdmittedInspectionWorld,
    capture_sources: impl FnOnce() -> Result<
        Option<std::sync::Arc<tidepool_toolchain::compiler_world::CapturedCompilerSources>>,
        WorldInspectionError,
    >,
) -> Result<PreparedCompilerWorld, WorldInspectionError> {
    if !std::sync::Arc::ptr_eq(&owner.source, &admitted.custody.original_source) {
        return Err(
            crate::RuntimeError::from(super::SourceAdmissionError::ForeignTransaction).into(),
        );
    }
    owner
        .admit(Some(admitted.custody.target.clone()))
        .map_err(crate::RuntimeError::from)?;
    let origin = crate::CompilerTransactionOrigin::current().ok_or(
        CompileError::CompilerPublication(crate::CompilerTransactionPublicationError::MissingOwner),
    )?;
    let cold = admitted.needs_source_capture();
    let world = acquire_compiler_world(
        admitted.inputs,
        &admitted.base,
        admitted.observation,
        capture_sources,
    )?;
    tracing::debug!(
        target: "exomonad_harness::timing",
        cold,
        logical_world = ?world.logical_id(),
        source_sha256 = ?admitted.base.source.sha256(),
        scope = ?admitted.base.scope,
        operation = ?origin,
        "compiler world acquired for private execution"
    );
    Ok(PreparedCompilerWorld {
        association: std::sync::Arc::new(InspectionWorldAssociation {
            base: admitted.base,
            world,
        }),
        custody: admitted.custody,
        origin,
    })
}

pub fn finish_admitted_compiler_world(
    settled: crate::CompilerTransactionSettled<Result<PreparedCompilerWorld, WorldInspectionError>>,
) -> Result<CompletedCompilerWorld, WorldInspectionError> {
    let (prepared, cancellation) = finish_world_operation(settled, |prepared| &prepared.origin)?;
    Ok(CompletedCompilerWorld {
        association: prepared.association,
        custody: prepared.custody,
        cancellation,
    })
}

pub(super) fn finish_world_operation<T>(
    settled: crate::CompilerTransactionSettled<Result<T, WorldInspectionError>>,
    origin: impl Fn(&T) -> &crate::CompilerTransactionOrigin,
) -> Result<(T, crate::HostAdmissionCancellation), WorldInspectionError> {
    let same_owner = settled
        .action()
        .as_ref()
        .is_ok_and(|prepared| settled.belongs_to(origin(prepared)));
    let cancellation = settled.host_admission_cancellation().cloned();
    let outcome = settled.into_outcome();
    if !matches!(
        outcome.close,
        crate::CompilerTransactionClose::Clean | crate::CompilerTransactionClose::NotStarted
    ) {
        return Err(WorldInspectionError::CompilerClose {
            primary: outcome.action.err().map(Box::new),
            close: outcome.close,
        });
    }
    let prepared = outcome.action?;
    if !same_owner {
        return Err(CompileError::CompilerPublication(
            crate::CompilerTransactionPublicationError::DifferentOwner,
        )
        .into());
    }
    let cancellation = cancellation.ok_or(CompileError::CompilerPublication(
        crate::CompilerTransactionPublicationError::MissingOwner,
    ))?;
    cancellation.checkpoint().map_err(CompileError::from)?;
    Ok((prepared, cancellation))
}

fn acquire_compiler_world(
    inputs: InspectionWorldInputs,
    base: &InspectionWorldBase,
    observation: Option<
        std::sync::Arc<tidepool_toolchain::compiler_world::WorldObservationScaffold>,
    >,
    capture_sources: impl FnOnce() -> Result<
        Option<std::sync::Arc<tidepool_toolchain::compiler_world::CapturedCompilerSources>>,
        WorldInspectionError,
    >,
) -> Result<std::sync::Arc<tidepool_toolchain::compiler_world::CompilerWorld>, WorldInspectionError>
{
    match inputs {
        InspectionWorldInputs::Published(world) => match observation {
            None => Ok(world),
            Some(observation) => Ok(world.with_observation_scaffold(Some(observation))?),
        },
        InspectionWorldInputs::Cold {
            primary,
            bindings,
            lexical,
        } => {
            let sources = capture_sources()?.ok_or(WorldInspectionError::UnsupportedSource)?;
            if sources.origin() != base.source.bytes() {
                return Err(invalid("cold sources differ from the admitted source owner").into());
            }
            let lexical = match observation {
                None => lexical,
                Some(observation) => lexical.with_observation(Some(observation))?,
            };
            let cold = tidepool_toolchain::artifacts::ModuleCandidateOffer::compiler_world_inputs(
                primary, bindings, sources, lexical,
            )?;
            let command = ExtractCmd::new().map_err(|error| CompileError::Io(error.into()))?;
            let endpoint = tidepool_toolchain::toolchain::AdmittedCompilerEndpoint::from_bound(
                command.bind().map_err(map_spawn)?,
            )
            .map_err(|error| CompileError::ExtractFailed(error.to_string()))?;
            Ok(tidepool_toolchain::compiler_world::open_world(
                cold, &endpoint,
            )?)
        }
    }
}

/// One enumeration answer is emitted as a single guest value. Keep the
/// returned identifiers bounded and report the observed count beside the
/// applied limit so truncation is visible rather than mistaken for the scope.
pub const SCOPE_BINDING_LIMIT: usize = 256;

/// The identifiers bound in one admitted scope, in a stable order.
///
/// This carries identity only. Presentation and row availability need a
/// compiler, so they stay with `info`; claiming them here would be evidence
/// the world does not hold.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ScopeBindingsAnswer {
    pub refs: Vec<IdentifierRef>,
    pub observed: usize,
    pub limit: usize,
}

impl AdmittedInspectionWorld {
    /// Attach source-issued compiler description before any World acquisition.
    /// This does not change the captured runtime/source admission.
    pub fn with_observation_scaffold(
        mut self,
        observation: std::sync::Arc<tidepool_toolchain::compiler_world::WorldObservationScaffold>,
    ) -> Self {
        self.observation = Some(observation);
        self
    }

    pub fn needs_source_capture(&self) -> bool {
        matches!(self.inputs, InspectionWorldInputs::Cold { .. })
    }

    /// Enumerate the admitted scope's published occurrences without compiling
    /// anything and without admitting a compiler transaction. The binding
    /// table is materialized by the publication that owns this world, so a
    /// cold world reads it directly and a borrowed world reads its restored
    /// snapshot facts.
    pub fn scope_bindings(&self) -> Result<ScopeBindingsAnswer, WorldInspectionError> {
        let mut identifiers = match &self.inputs {
            InspectionWorldInputs::Cold { bindings, .. } => bindings
                .names()
                .map(binding_identifier)
                .collect::<Result<Vec<_>, _>>()?,
            InspectionWorldInputs::Published(world) => world
                .restoration()
                .binding_names()
                .map(binding_identifier)
                .collect::<Result<Vec<_>, _>>()?,
        };
        identifiers.sort_by(|left, right| {
            (
                left.module.as_str(),
                left.name.as_str(),
                namespace_rank(left.namespace),
            )
                .cmp(&(
                    right.module.as_str(),
                    right.name.as_str(),
                    namespace_rank(right.namespace),
                ))
        });
        identifiers.dedup();
        let observed = identifiers.len();
        identifiers.truncate(SCOPE_BINDING_LIMIT);
        Ok(ScopeBindingsAnswer {
            refs: identifiers,
            observed,
            limit: SCOPE_BINDING_LIMIT,
        })
    }
}

fn binding_identifier(
    name: tidepool_toolchain::compiler_world::WorldBindingName,
) -> Result<IdentifierRef, WorldInspectionError> {
    Ok(IdentifierRef {
        module: name.module,
        name: name.occurrence,
        namespace: binding_namespace(&name.namespace)?,
    })
}

/// `SymbolIdentity::namespace` is the compiler's own wire vocabulary
/// (`value`/`type`/`constructor`/`field`). An unknown spelling is a refusal,
/// never a guessed value namespace.
fn namespace_rank(namespace: IdentifierNamespace) -> u8 {
    match namespace {
        IdentifierNamespace::Value => 0,
        IdentifierNamespace::Type => 1,
        IdentifierNamespace::Constructor => 2,
        IdentifierNamespace::Field => 3,
    }
}

fn binding_namespace(namespace: &str) -> Result<IdentifierNamespace, WorldInspectionError> {
    match namespace {
        "value" => Ok(IdentifierNamespace::Value),
        "type" => Ok(IdentifierNamespace::Type),
        "constructor" => Ok(IdentifierNamespace::Constructor),
        "field" => Ok(IdentifierNamespace::Field),
        other => Err(WorldInspectionError::UnknownBindingNamespace(
            other.to_owned(),
        )),
    }
}

impl<A> CompletedInspectionWorld<A> {
    pub fn answer(&self) -> &A {
        &self.prepared.answer
    }

    pub fn provenance(&self) -> &ScopeProvenance {
        &self.prepared.provenance
    }
}

impl CompletedInspectionWorld {
    pub fn resolution(&self) -> tidepool_toolchain::compiler_world::WorldModuleResolution {
        self.prepared.answer.resolution()
    }

    pub fn defining_module(
        &self,
    ) -> Option<&tidepool_toolchain::compiler_world::WorldModuleReceipt> {
        self.prepared.answer.defining_module()
    }
}

/// Accept only the completed action from its own clean, uncancelled compiler
/// transaction. An uncertain END cannot attach a world to the runtime base.
pub fn finish_admitted_world_inspection<A>(
    settled: crate::CompilerTransactionSettled<
        Result<PreparedInspectionWorld<A>, WorldInspectionError>,
    >,
) -> Result<CompletedInspectionWorld<A>, WorldInspectionError> {
    let (prepared, cancellation) = finish_world_operation(settled, |prepared| &prepared.origin)?;
    Ok(CompletedInspectionWorld {
        prepared,
        cancellation,
    })
}

struct AdmittedWorldObservation {
    association: std::sync::Arc<InspectionWorldAssociation>,
    custody: InspectionWorldCustody,
    origin: crate::CompilerTransactionOrigin,
    provenance: ScopeProvenance,
}

impl AdmittedWorldObservation {
    fn with_answer<A>(self, answer: A) -> PreparedInspectionWorld<A> {
        PreparedInspectionWorld {
            association: self.association,
            custody: self.custody,
            origin: self.origin,
            answer,
            provenance: self.provenance,
        }
    }
}

fn prepare_world_observation(
    owner: &super::RuntimeSourceCompilerTransaction,
    admitted: AdmittedInspectionWorld,
    observation: InspectionScopeSnapshot,
    scope: &NameScope,
    capture_sources: impl FnOnce() -> Result<
        Option<std::sync::Arc<tidepool_toolchain::compiler_world::CapturedCompilerSources>>,
        WorldInspectionError,
    >,
) -> Result<AdmittedWorldObservation, WorldInspectionError> {
    use std::sync::Arc;
    if !Arc::ptr_eq(&owner.source, &admitted.custody.original_source) {
        return Err(
            crate::RuntimeError::from(super::SourceAdmissionError::ForeignTransaction).into(),
        );
    }
    owner
        .admit(Some(admitted.custody.target.clone()))
        .map_err(crate::RuntimeError::from)?;
    let origin = crate::CompilerTransactionOrigin::current().ok_or(
        CompileError::CompilerPublication(crate::CompilerTransactionPublicationError::MissingOwner),
    )?;
    if !observation.base.matches_key(&admitted.base.key)
        || observation.runtime_scope != admitted.base.scope
        || observation.generation != admitted.base.key.generation().0
        || &observation.scope != scope
    {
        return Err(invalid("inspection observation differs from its admitted base").into());
    }
    let world = acquire_compiler_world(
        admitted.inputs,
        &admitted.base,
        admitted.observation,
        capture_sources,
    )?;
    // Resolve the immutable captured pin only after actual source acquisition
    // succeeds. Source-unavailable observations must not hash the retained view.
    let provenance = observation.provenance();
    Ok(AdmittedWorldObservation {
        association: Arc::new(InspectionWorldAssociation {
            base: admitted.base,
            world,
        }),
        custody: admitted.custody,
        origin,
        provenance,
    })
}

fn world_observation_endpoint(
) -> Result<tidepool_toolchain::toolchain::AdmittedCompilerEndpoint, CompileError> {
    let command = ExtractCmd::new().map_err(|error| CompileError::Io(error.into()))?;
    tidepool_toolchain::toolchain::AdmittedCompilerEndpoint::from_bound(
        command.bind().map_err(map_spawn)?,
    )
    .map_err(|error| CompileError::ExtractFailed(error.to_string()))
}

/// Run one public-module observation against the actual acquired world. The
/// captured semantic pin is transmitted and validated, never stamped afterward.
pub fn run_admitted_world_inspection(
    owner: &super::RuntimeSourceCompilerTransaction,
    admitted: AdmittedInspectionWorld,
    capture_sources: impl FnOnce() -> Result<
        Option<std::sync::Arc<tidepool_toolchain::compiler_world::CapturedCompilerSources>>,
        WorldInspectionError,
    >,
    query: NameQuery,
    observation: InspectionScopeSnapshot,
    kind: tidepool_toolchain::compiler_world::PublicModuleQuestionKind,
) -> Result<PreparedInspectionWorld, WorldInspectionError> {
    use tidepool_toolchain::compiler_world::{
        self, PublicModuleQuestion, PublicModuleQuestionKind,
    };
    let NameScope::PublicModule(module) = &query.scope else {
        return Err(invalid("world inspection requires a public module").into());
    };
    let namespace = inspection_namespace(query.namespace);
    let prepared =
        prepare_world_observation(owner, admitted, observation, &query.scope, capture_sources)?;
    let question = match kind {
        PublicModuleQuestionKind::StructuredInfo => {
            PublicModuleQuestion::structured_info(module.clone(), namespace, query.name)
        }
        PublicModuleQuestionKind::StructuredType => {
            PublicModuleQuestion::structured_type(module.clone(), namespace, query.name)
        }
    }
    .with_provenance(
        prepared.provenance.generation,
        prepared.provenance.fingerprint.clone(),
    );
    let result = compiler_world::query_public_module(
        &world_observation_endpoint()?,
        &prepared.association.world,
        question.clone(),
    )?;
    if result.world().logical_id() != prepared.association.world.logical_id()
        || result.question() != &question
    {
        return Err(invalid("world result differs from its admitted question or world").into());
    }
    Ok(prepared.with_answer(result.into_answer()))
}

/// Observe the actual retained lexical scaffold with task-specific operands.
/// The same source, END and current-view owners govern public and scoped answers.
pub fn run_admitted_world_scope_inspection(
    owner: &super::RuntimeSourceCompilerTransaction,
    admitted: AdmittedInspectionWorld,
    capture_sources: impl FnOnce() -> Result<
        Option<std::sync::Arc<tidepool_toolchain::compiler_world::CapturedCompilerSources>>,
        WorldInspectionError,
    >,
    question: tidepool_toolchain::compiler_world::WorldObservationQuestion,
    observation: InspectionScopeSnapshot,
) -> Result<
    PreparedInspectionWorld<tidepool_toolchain::compiler_world::WorldObservationOutcome>,
    WorldInspectionError,
> {
    use tidepool_toolchain::compiler_world::{self, WorldObservationQuestion};
    if matches!(&question, WorldObservationQuestion::Info(query) | WorldObservationQuestion::Type(query) if query.scope != NameScope::Current)
    {
        return Err(invalid("scoped world inspection requires the current scope").into());
    }
    let prepared = prepare_world_observation(
        owner,
        admitted,
        observation,
        &NameScope::Current,
        capture_sources,
    )?;
    let result = compiler_world::query_scope(
        &world_observation_endpoint()?,
        &prepared.association.world,
        question.clone(),
        &prepared.provenance,
    )?;
    if result.world().logical_id() != prepared.association.world.logical_id()
        || result.question() != &question
        || result.provenance().scope != prepared.provenance.scope
        || !result.provenance().same_view(&prepared.provenance)
    {
        return Err(
            invalid("scoped result differs from its admitted question, world or view").into(),
        );
    }
    Ok(prepared.with_answer(result.into_outcome()))
}

pub struct InspectionRequest<'a> {
    /// Protected declarations and request-local annotations shared by every query in this batch.
    pub exact_context:
        Option<std::sync::Arc<tidepool_toolchain::declaration_join::ExactCompileContext>>,
    pub preamble: &'a str,
    pub imports: &'a str,
    pub include: &'a [&'a Path],
    pub session_root: &'a Path,
    pub inject_modules: &'a [String],
    pub queries: &'a [InspectionQuery],
    /// The actor's GHC effects type alias. Standalone inspections leave it absent.
    pub effects: Option<&'a super::HaskellTypeSource>,
}

/// Inspect an ordered batch without evaluating it or mutating the resident
/// session. Every query sees the same preamble, imports, session modules, and
/// injected value interfaces as the next ordinary turn. The worker serves the
/// batch through one request while isolating GHC rejection per query. A
/// homogeneous batch of type probes is compiled together first; its original
/// singleton sources remain available if that combined source is rejected.
pub fn run_inspections(
    request: InspectionRequest<'_>,
    settlement: &mut dyn FnMut(crate::CompilerTransactionClose),
) -> Result<Vec<InspectionResult>, CompileError> {
    run_inspections_with_policy(request, false, None, settlement)
}

/// Consume the immutable checked value snapshot captured with the caller's view.
/// This grants only inspection inputs; successful answers still need exact receipts.
pub fn run_admitted_inspections(
    request: InspectionRequest<'_>,
    view: &super::SessionCompileView,
    inputs: &AdmittedInspectionInputs,
    settlement: &mut dyn FnMut(crate::CompilerTransactionClose),
) -> Result<Vec<InspectionResult>, CompileError> {
    validate_inspection_view(&request.exact_context, view, inputs)?;
    run_inspections_with_policy(request, false, Some(inputs), settlement)
}

fn validate_inspection_view(
    exact_context: &Option<
        std::sync::Arc<tidepool_toolchain::declaration_join::ExactCompileContext>,
    >,
    view: &super::SessionCompileView,
    inputs: &AdmittedInspectionInputs,
) -> Result<(), CompileError> {
    if view.session() != inputs.view.session()
        || view.lexical_scope() != inputs.view.lexical_scope()
        || !view.is_current_for(&inputs.view)
        || !match (exact_context, view.exact_compile_context().as_ref()) {
            (Some(request), Some(current)) => request.same_authenticated_context(current),
            (None, None) => true,
            _ => false,
        }
    {
        return Err(invalid("inspection inputs belong to another compile view"));
    }
    Ok(())
}

/// Declaration staging variant: source rejection must remain a request-level
/// structured diagnostic so declaration span remapping is preserved.
pub(super) fn run_inspections_strict(
    request: InspectionRequest<'_>,
    settlement: &mut dyn FnMut(crate::CompilerTransactionClose),
) -> Result<Vec<InspectionResult>, CompileError> {
    run_inspections_with_policy(request, true, None, settlement)
}

fn run_inspections_with_policy(
    request: InspectionRequest<'_>,
    strict: bool,
    inputs: Option<&AdmittedInspectionInputs>,
    settlement: &mut dyn FnMut(crate::CompilerTransactionClose),
) -> Result<Vec<InspectionResult>, CompileError> {
    run_inspection_batch(request, strict, inputs, settlement)
}

fn run_inspection_batch(
    request: InspectionRequest<'_>,
    strict: bool,
    inputs: Option<&AdmittedInspectionInputs>,
    settlement: &mut dyn FnMut(crate::CompilerTransactionClose),
) -> Result<Vec<InspectionResult>, CompileError> {
    if inputs.is_some_and(|inputs| !inputs.matches_request(&request)) {
        return Err(invalid(
            "inspection request differs from its admitted scope snapshot",
        ));
    }
    let values = inputs.map(|inputs| inputs.values.as_slice());
    if request.queries.is_empty() {
        return Ok(Vec::new());
    }
    let temp = TempDir::new()?;
    let output_path = temp.path().join("inspection.cbor");

    let mut command = ExtractCmd::new().map_err(|error| CompileError::Io(error.into()))?;
    command
        .output_dir(temp.path())
        .inspect_out(&output_path)
        .includes(request.include)
        .session_root(request.session_root);
    if values.is_none() {
        command.inject_vals(request.inject_modules);
    }
    if strict {
        command.inspection_strict();
    }
    let issued_imports = request.effects.map(|effects| {
        effects
            .source_imports(&super::SourceImports::from_specs([request.imports]))
            .template_text()
    });
    let imports = match request.effects {
        Some(_) => format!(
            "{}\nqualified Data.Proxy\nqualified Tidepool.Effects.Core\nqualified Tidepool.Agent.Contract\n",
            issued_imports
                .as_deref()
                .expect("effects imports assembled")
        ),
        None => request.imports.to_owned(),
    };
    let type_batch = request
        .queries
        .iter()
        .map(|query| match query {
            InspectionQuery::TypeOf(expression) => Some(expression.clone()),
            _ => None,
        })
        .collect::<Option<Vec<_>>>()
        .filter(|expressions| {
            expressions.len() > 1
                && !request.preamble.contains("__tidepool_inspect_")
                && !request.imports.contains("__tidepool_inspect_")
                && !expressions
                    .iter()
                    .any(|expression| expression.contains("__tidepool_inspect_"))
        });
    let mut shared_environment_source = None;
    let mut sources = BTreeMap::new();
    let mut query_sources = Vec::with_capacity(request.queries.len());
    for (index, query) in request.queries.iter().enumerate() {
        let query_dir = temp.path().join(format!("query-{index}"));
        std::fs::create_dir(&query_dir)?;
        let shares_environment = matches!(
            query,
            InspectionQuery::Info(_)
                | InspectionQuery::ScopeBrowse
                | InspectionQuery::Browse { .. }
                | InspectionQuery::StructuredInfo { .. }
                | InspectionQuery::StructuredType { .. }
        );
        let source_path = if shares_environment {
            shared_environment_source
                .get_or_insert_with(|| query_dir.join("Expr.hs"))
                .clone()
        } else {
            query_dir.join("Expr.hs")
        };
        let expressions = match query {
            InspectionQuery::TypeOf(expression) => std::slice::from_ref(expression),
            InspectionQuery::Info(_)
            | InspectionQuery::TypeSearch(_)
            | InspectionQuery::TypeSearchBounded { .. }
            | InspectionQuery::ScopeBrowse
            | InspectionQuery::Browse { .. }
            | InspectionQuery::StructuredInfo { .. }
            | InspectionQuery::StructuredType { .. } => &[],
        };
        let mut source = assemble_inspection_module(request.preamble, &imports, expressions);
        if let Some(effects) = request.effects {
            source.push_str("\n__tidepool_inspection_row :: Data.Proxy.Proxy (");
            source.push_str(effects.expression());
            source.push_str(")\n__tidepool_inspection_row = Data.Proxy.Proxy\n");
        }
        let type_search = match query {
            InspectionQuery::TypeSearch(query) => Some(query),
            InspectionQuery::TypeSearchBounded { pattern, .. } => Some(pattern),
            _ => None,
        };
        if let Some(query) = type_search {
            source.push_str("\n__tidepool_type_search_query :: ");
            source.push_str(query);
            source.push_str("\n__tidepool_type_search_query = __tidepool_type_search_query\n");
        }
        std::fs::write(&source_path, &source)?;
        sources.insert(source_path.clone(), source);
        query_sources.push(source_path.clone());
        command.input(&source_path);
        match query {
            InspectionQuery::TypeOf(expression) => {
                command.inspect_type(expression);
            }
            InspectionQuery::Info(name) => {
                command.inspect_info(name);
            }
            InspectionQuery::TypeSearch(query) => {
                command.inspect_search(query);
            }
            InspectionQuery::TypeSearchBounded { pattern, budget } => {
                command.inspect_search_bounded(pattern, budget_u32(*budget)?);
            }
            InspectionQuery::ScopeBrowse => {
                command.inspect_scope_browse();
            }
            InspectionQuery::Browse { module, expanded } => {
                command.inspect_browse(module, *expanded);
            }
            InspectionQuery::StructuredInfo { query, provenance } => {
                command.inspect_structured_info(structured_request(query, provenance));
            }
            InspectionQuery::StructuredType { query, provenance } => {
                command.inspect_structured_type(structured_request(query, provenance));
            }
        }
    }
    let mut batch_source = None;
    if let Some(expressions) = type_batch {
        let batch_dir = temp.path().join("type-batch");
        std::fs::create_dir(&batch_dir)?;
        let batch_path = batch_dir.join("Expr.hs");
        let mut source = assemble_inspection_module(request.preamble, &imports, &expressions);
        if let Some(effects) = request.effects {
            source.push_str("\n__tidepool_inspection_row :: Data.Proxy.Proxy (");
            source.push_str(effects.expression());
            source.push_str(")\n__tidepool_inspection_row = Data.Proxy.Proxy\n");
        }
        std::fs::write(&batch_path, &source)?;
        sources.insert(batch_path.clone(), source);
        batch_source = Some(batch_path.clone());
        command.inspect_type_batch(&batch_path);
    }

    let endpoint = tidepool_toolchain::toolchain::AdmittedCompilerEndpoint::from_bound(
        command.bind().map_err(map_spawn)?,
    )
    .map_err(|error| CompileError::ExtractFailed(error.to_string()))?;
    let include = request
        .include
        .iter()
        .map(|path| path.to_path_buf())
        .collect::<Vec<_>>();
    let catalog = inputs
        .map(|inputs| &inputs.catalog)
        .cloned()
        .unwrap_or_default()
        .for_deployment(endpoint.deployment())?;
    let offer = match values {
        Some(values) => {
            let certificates = values
                .iter()
                .map(|value| value.checked_artifact().clone())
                .collect::<Vec<_>>();
            let modules = values
                .iter()
                .map(|value| value.module().module_name())
                .collect::<Vec<_>>();
            command.inject_vals(&modules);
            tidepool_toolchain::artifacts::ModuleCandidateOffer::select_inspection_with_catalog(
                &endpoint,
                &include,
                temp.path(),
                request.exact_context.clone(),
                values
                    .iter()
                    .map(|value| (value.module(), value.bytes_owned().clone()))
                    .collect(),
                &certificates,
                &catalog,
            )?
        }
        None => match request.exact_context.clone() {
            Some(context) => {
                tidepool_toolchain::artifacts::ModuleCandidateOffer::select_in_context_with_catalog(
                    &endpoint,
                    &include,
                    temp.path(),
                    context,
                    &catalog,
                )?
            }
            None => {
                tidepool_toolchain::artifacts::ModuleCandidateOffer::select_admitted_with_catalog(
                    &endpoint,
                    &include,
                    temp.path(),
                    &catalog,
                )?
            }
        },
    };
    if let Some(root) = offer.checked_value_root() {
        command.session_root(root);
    }
    crate::paths::apply_admitted_build_products_dir(&mut command, &endpoint);
    let offer = offer.apply_to(command, temp.path())?;
    let command = offer.command();
    #[cfg(test)]
    if std::env::var("TIDEPOOL_KEEP_TEST_LOGS").as_deref() == Ok("1") {
        if let Some(root) = std::env::var_os("TIDEPOOL_TEST_ARTIFACT_ROOT") {
            let retain = || -> std::io::Result<PathBuf> {
                let root = PathBuf::from(root).join("inspection-inputs");
                std::fs::create_dir_all(&root)?;
                let retained = TempDir::new_in(root)?;
                std::fs::write(
                    retained.path().join("worker-request.cbor"),
                    command.request_bytes(),
                )?;
                let mut source_bytes = Vec::new();
                ciborium::ser::into_writer(&sources.iter().collect::<Vec<_>>(), &mut source_bytes)
                    .map_err(|error| std::io::Error::other(error.to_string()))?;
                std::fs::write(retained.path().join("generated-sources.cbor"), source_bytes)?;
                if let Some(scope) = offer.exact_scope_path() {
                    std::fs::copy(scope, retained.path().join("exact-scope.cbor"))?;
                }
                Ok(retained.keep())
            };
            match retain() {
                Ok(path) => eprintln!("owned inspection inputs retained at {}", path.display()),
                Err(error) => eprintln!("could not retain owned inspection inputs: {error}"),
            }
        }
    }
    let run =
        endpoint.execute_with_input_files(&command, offer.input_transport_files(), |close| {
            settlement(close)
        })?;
    timing::record_stage(
        timing::NO_NODE,
        timing::NO_ROUND,
        timing::STAGE_EXTRACT_SPAWN,
        run.elapsed,
        0,
    );
    crate::diag::decode_extract_result(run.success(), &run.output.stdout, &run.output.stderr)
        .map_err(|error| offer.retain_failure(temp.path(), &command, &run.output.stderr, error))?;
    let bytes = std::fs::read(&output_path).map_err(|error| {
        if error.kind() == std::io::ErrorKind::NotFound {
            CompileError::MissingOutput(output_path.clone())
        } else {
            CompileError::Io(error)
        }
    })?;
    let output = decode_inspection_output(&bytes)?;
    if output.len() != request.queries.len() {
        return Err(invalid(
            "inspection output operation coverage differs from the submitted batch",
        ));
    }
    let successes = output
        .iter()
        .map(|result| !matches!(result, InspectionResult::Rejected { .. }))
        .collect::<Vec<_>>();
    #[cfg(test)]
    if std::env::var("TIDEPOOL_KEEP_TEST_LOGS").as_deref() == Ok("1") {
        if let Some(root) = std::env::var_os("TIDEPOOL_TEST_ARTIFACT_ROOT") {
            let retain = || -> std::io::Result<PathBuf> {
                let root = PathBuf::from(root).join("inspection-receipts");
                std::fs::create_dir_all(&root)?;
                let retained = TempDir::new_in(root)?;
                std::fs::copy(&output_path, retained.path().join("inspection.cbor"))?;
                let directories = sources
                    .keys()
                    .filter_map(|path| path.parent())
                    .collect::<BTreeSet<_>>();
                for (index, directory) in directories.into_iter().enumerate() {
                    let originals = directory.join(".exact-compilations");
                    if !originals.try_exists()? {
                        continue;
                    }
                    let destination = retained.path().join(index.to_string());
                    std::fs::create_dir(&destination)?;
                    std::fs::write(
                        destination.join("original-directory.txt"),
                        originals.as_os_str().as_encoded_bytes(),
                    )?;
                    for entry in std::fs::read_dir(originals)? {
                        let entry = entry?;
                        if !entry.file_type()?.is_dir() {
                            continue;
                        }
                        let snapshot = destination.join(entry.file_name());
                        std::fs::create_dir(&snapshot)?;
                        for name in ["source.hs", "receipt.cbor"] {
                            std::fs::copy(entry.path().join(name), snapshot.join(name))?;
                        }
                        let bytes = std::fs::read(snapshot.join("receipt.cbor"))?;
                        if let Ok(CborValue::Array(header)) =
                            ciborium::de::from_reader(bytes.as_slice())
                        {
                            if let Some(CborValue::Text(evidence)) = header.get(7) {
                                std::fs::write(snapshot.join("dependencies.json"), evidence)?;
                            }
                        }
                    }
                }
                Ok(retained.keep())
            };
            match retain() {
                Ok(path) => eprintln!("owned inspection receipts retained at {}", path.display()),
                Err(error) => eprintln!("could not retain owned inspection receipts: {error}"),
            }
        }
    }
    if offer.exact_scope_path().is_some() && successes.iter().any(|success| *success) {
        let validated = (|| {
            let directories = sources
                .keys()
                .filter_map(|path| path.parent())
                .collect::<BTreeSet<_>>();
            let mut admitted = BTreeSet::<PathBuf>::new();
            for directory in directories {
                if !directory.join(".exact-compilations").try_exists()? {
                    continue;
                }
                for witness in offer.validate_exact_outputs(directory)? {
                    let path = witness.source_path();
                    let source = sources.get(path).ok_or_else(|| {
                        invalid("inspection receipt names another source transaction")
                    })?;
                    if !witness.matches_source(path, source) {
                        return Err(invalid(
                            "inspection receipt does not match submitted source",
                        ));
                    }
                    admitted.insert(path.to_path_buf());
                }
            }
            let batch_admitted = batch_source
                .as_ref()
                .is_some_and(|path| admitted.contains(path));
            for (index, success) in successes.iter().enumerate() {
                if *success && !admitted.contains(&query_sources[index]) && !batch_admitted {
                    return Err(invalid(
                        "inspection result lacks its exact consumed source receipt",
                    ));
                }
            }
            Ok::<_, CompileError>(())
        })();
        validated.map_err(|error| {
            offer.retain_failure(temp.path(), &command, &run.output.stderr, error)
        })?;
    }
    Ok(request
        .queries
        .iter()
        .zip(output)
        .map(|(query, result)| match (query, result) {
            (
                InspectionQuery::StructuredType { .. },
                InspectionResult::StructuredInfo(Err(error)),
            ) => InspectionResult::StructuredType(Err(error)),
            (_, result) => result,
        })
        .collect())
}

fn structured_request(
    query: &NameQuery,
    provenance: &ScopeProvenance,
) -> tidepool_extract_cmd::StructuredInspection {
    tidepool_extract_cmd::StructuredInspection {
        scope: request_scope(&query.scope),
        namespace: inspection_namespace(query.namespace),
        name: query.name.clone(),
        generation: provenance.generation,
        fingerprint: provenance.fingerprint.clone(),
    }
}

fn inspection_namespace(namespace: NameNamespace) -> tidepool_extract_cmd::InspectionNamespace {
    match namespace {
        NameNamespace::Any => tidepool_extract_cmd::InspectionNamespace::Any,
        NameNamespace::Value => tidepool_extract_cmd::InspectionNamespace::Value,
        NameNamespace::Type => tidepool_extract_cmd::InspectionNamespace::Type,
        NameNamespace::Constructor => tidepool_extract_cmd::InspectionNamespace::Constructor,
    }
}

fn request_scope(scope: &NameScope) -> tidepool_extract_cmd::InspectionScope {
    match scope {
        NameScope::Current => tidepool_extract_cmd::InspectionScope::Current,
        NameScope::PublicModule(module) => {
            tidepool_extract_cmd::InspectionScope::PublicModule(module.clone())
        }
    }
}

fn map_spawn(error: SpawnError) -> CompileError {
    CompileError::Io(crate::extract_spawn_error(error.source))
}

fn decode_inspections(bytes: &[u8]) -> Result<Vec<InspectionResult>, CompileError> {
    decode_inspection_output(bytes)
}

fn decode_inspection_output(bytes: &[u8]) -> Result<Vec<InspectionResult>, CompileError> {
    let mut reader = std::io::Cursor::new(bytes);
    let value: CborValue = ciborium::de::from_reader(&mut reader)
        .map_err(|error| invalid(format!("malformed CBOR: {error}")))?;
    if reader.position() != bytes.len() as u64 {
        return Err(invalid("trailing CBOR data"));
    }
    let root = array_len(&value, 2, "receipt")?;
    if text(&root[0], "version")? != "TPINSP008" {
        return Err(invalid("unsupported receipt version"));
    }
    let body = array(&root[1], "inspection output")?;
    match body
        .first()
        .ok_or_else(|| invalid("empty inspection output"))
        .and_then(|v| text(v, "output kind"))?
    {
        "Complete" => {
            let fields = array_len(&root[1], 2, "complete output")?;
            array(&fields[1], "results")?
                .iter()
                .map(decode_inspection_result)
                .collect::<Result<_, _>>()
        }
        other => Err(invalid(format!("unknown inspection output kind {other:?}"))),
    }
}

fn decode_inspection_result(value: &CborValue) -> Result<InspectionResult, CompileError> {
    let body = array(value, "body")?;
    let tag = body
        .first()
        .ok_or_else(|| invalid("empty result body"))
        .and_then(|value| text(value, "result tag"))?;
    match tag {
        "Type" => {
            let body = array_len(value, 4, "Type result")?;
            Ok(InspectionResult::Type {
                expression: text(&body[1], "Type expression")?.into(),
                display: text(&body[2], "Type display")?.into(),
                availability: InspectionAvailability::decode(&body[3], "Type availability")?,
            })
        }
        "Info" => {
            let body = array_len(value, 3, "Info result")?;
            let entries = array(&body[2], "Info entries")?
                .iter()
                .map(decode_info_entry)
                .collect::<Result<_, _>>()?;
            Ok(InspectionResult::Info {
                query: text(&body[1], "Info query")?.into(),
                entries,
            })
        }
        "Ambiguous" => {
            let body = array_len(value, 3, "Ambiguous result")?;
            let entries = array(&body[2], "Ambiguous entries")?
                .iter()
                .map(decode_info_entry)
                .collect::<Result<_, _>>()?;
            Ok(InspectionResult::Ambiguous {
                query: text(&body[1], "Ambiguous query")?.into(),
                entries,
            })
        }
        "NotFound" => {
            let body = array_len(value, 2, "NotFound result")?;
            Ok(InspectionResult::NotFound {
                query: text(&body[1], "NotFound query")?.into(),
            })
        }
        "ModuleNotFound" => {
            let body = array_len(value, 3, "ModuleNotFound result")?;
            Ok(InspectionResult::ModuleNotFound {
                module: text(&body[1], "ModuleNotFound module")?.into(),
                scopes: decode_scopes(&body[2])?,
            })
        }
        "Rejected" => {
            let body = array_len(value, 2, "Rejected result")?;
            Ok(InspectionResult::Rejected {
                diagnostic: text(&body[1], "Rejected diagnostic")?.into(),
            })
        }
        "Browse" => {
            let body = array_len(value, 4, "Browse result")?;
            let entries = array(&body[3], "Browse entries")?
                .iter()
                .map(decode_info_entry)
                .collect::<Result<_, _>>()?;
            Ok(InspectionResult::Browse {
                module: text(&body[1], "Browse module")?.into(),
                expanded: boolean(&body[2], "Browse expanded")?,
                entries,
            })
        }
        "TypeMatches" => {
            let body = array_len(value, 3, "TypeMatches result")?;
            let matches = array(&body[2], "TypeMatches matches")?
                .iter()
                .map(decode_type_match)
                .collect::<Result<_, _>>()?;
            Ok(InspectionResult::TypeMatches {
                query: text(&body[1], "TypeMatches query")?.into(),
                matches,
            })
        }
        "StructuredInfoOk" | "StructuredTypeOk" | "StructuredError" => {
            Ok(match decode_structured_result(value)? {
                StructuredReply::Info(info) => InspectionResult::StructuredInfo(Ok(info)),
                StructuredReply::Type(info) => InspectionResult::StructuredType(Ok(info)),
                StructuredReply::Error(error) => InspectionResult::StructuredInfo(Err(error)),
            })
        }
        other => Err(invalid(format!("unknown result tag {other:?}"))),
    }
}

fn boolean(value: &CborValue, what: &str) -> Result<bool, CompileError> {
    match value {
        CborValue::Bool(value) => Ok(*value),
        _ => Err(invalid(format!("{what} must be boolean"))),
    }
}

fn decode_scopes(value: &CborValue) -> Result<Vec<NameScope>, CompileError> {
    let scopes = array(value, "searched scopes")?
        .iter()
        .map(decode_scope)
        .collect::<Result<Vec<_>, _>>()?;
    for (index, scope) in scopes.iter().enumerate() {
        if scopes[..index].contains(scope) {
            return Err(invalid("duplicate searched scope"));
        }
    }
    Ok(scopes)
}

fn decode_info_entry(value: &CborValue) -> Result<InfoEntry, CompileError> {
    let fields = array_len(value, 7, "Info entry")?;
    let module = match &fields[1] {
        CborValue::Null => None,
        value => Some(text(value, "Info module")?.into()),
    };
    Ok(InfoEntry {
        name: text(&fields[0], "Info name")?.into(),
        module,
        kind: text(&fields[2], "Info kind")?.into(),
        display: text(&fields[3], "Info display")?.into(),
        availability: InspectionAvailability::decode(&fields[4], "Info availability")?,
        references: array(&fields[5], "Info references")?
            .iter()
            .map(decode_identifier_ref)
            .collect::<Result<_, _>>()?,
        identity: decode_identifier_ref(&fields[6])?,
    })
}

fn array<'a>(value: &'a CborValue, what: &str) -> Result<&'a [CborValue], CompileError> {
    match value {
        CborValue::Array(values) => Ok(values),
        _ => Err(invalid(format!("{what} must be an array"))),
    }
}

fn array_len<'a>(
    value: &'a CborValue,
    expected: usize,
    what: &str,
) -> Result<&'a [CborValue], CompileError> {
    let values = array(value, what)?;
    if values.len() != expected {
        return Err(invalid(format!(
            "{what} must contain {expected} items, got {}",
            values.len()
        )));
    }
    Ok(values)
}

fn text<'a>(value: &'a CborValue, what: &str) -> Result<&'a str, CompileError> {
    match value {
        CborValue::Text(value) => Ok(value),
        _ => Err(invalid(format!("{what} must be text"))),
    }
}

fn invalid(detail: impl Into<String>) -> CompileError {
    CompileError::ExtractFailed(format!("inspection receipt: {}", detail.into()))
}

/// A query budget crosses the worker wire as a positive `u32`. Zero would be
/// indistinguishable from an omitted budget, so an explicit zero is a caller
/// bug rather than "return nothing".
fn budget_u32(budget: usize) -> Result<u32, CompileError> {
    match u32::try_from(budget) {
        Ok(budget) if budget > 0 => Ok(budget),
        Ok(_) => Err(invalid("inspection budget must be positive")),
        Err(_) => Err(invalid("inspection budget exceeds the worker wire width")),
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn actor_observation_is_bound_by_the_same_scope_identity_owner() {
        let imports = super::super::SourceImports::default();
        let inputs = super::super::RuntimeCompileInputs::default();
        let observe =
            |identity, incarnation, row: &str, request: Option<u64>, model: Option<&str>| {
                InspectionActorObservation::new(
                    tidepool_repr::PrincipalId {
                        identity,
                        incarnation,
                    },
                    row.into(),
                    request.map(InspectionRequestIdentity::new),
                    model.map(Into::into),
                )
            };
        let pin = |actor: &InspectionActorObservation| {
            inspection_provenance(
                [7; 32],
                9,
                b"source-owner",
                &imports,
                &inputs,
                Some(actor),
                NameScope::Current,
            )
        };
        let original = observe(1, 2, "'[]", Some(41), None);
        assert!(pin(&original).same_view(&pin(&original.clone())));
        for changed in [
            observe(3, 2, "'[]", Some(41), None),
            observe(1, 3, "'[]", Some(41), None),
            observe(1, 2, "'[Other]", Some(41), None),
            observe(1, 2, "'[]", Some(42), None),
            observe(1, 2, "'[]", None, None),
            observe(1, 2, "'[]", Some(41), Some("Model Family")),
        ] {
            assert!(!pin(&original).same_view(&pin(&changed)));
        }
    }

    #[test]
    fn admitted_inspection_refuses_sibling_pairing_with_shared_root_and_context() {
        use crate::session::{ModuleEnv, PersistentSession, SessionId, SessionLib};
        let root = tempfile::tempdir().unwrap();
        let lib =
            SessionLib::open(SessionId(902), root.path(), ModuleEnv::standalone_default()).unwrap();
        let mut session = PersistentSession::new(Some(lib), 1024 * 1024);
        let first_scope = session.mint_isolated_scope();
        let second_scope = session.mint_isolated_scope();
        let first = session.compile_view_in(first_scope).unwrap();
        let second = session.compile_view_in(second_scope).unwrap();
        assert_eq!(first.session_root(), second.session_root());
        assert_eq!(
            first.exact_declaration_context(),
            second.exact_declaration_context()
        );
        assert_eq!(
            first.injected_module_names(),
            second.injected_module_names()
        );
        let inputs =
            AdmittedInspectionInputs::capture(first.clone(), Vec::new(), Default::default());
        let injected = first.injected_module_names();
        let request = || InspectionRequest {
            exact_context: first.exact_compile_context(),
            preamble: "",
            imports: "",
            include: &[],
            session_root: first.session_root(),
            inject_modules: &injected,
            queries: &[],
            effects: None,
        };
        assert!(
            tidepool_testing::with_settlement(|settlement| run_admitted_inspections(
                request(),
                &first,
                &inputs,
                settlement
            ))
            .unwrap()
            .is_empty()
        );
        assert!(
            tidepool_testing::with_settlement(|settlement| run_admitted_inspections(
                request(),
                &second,
                &inputs,
                settlement
            ))
            .is_err()
        );
    }
    use super::*;
    use tidepool_testing::eval_harness;

    #[test]
    fn admitted_empty_snapshot_answers_type_search() {
        use crate::session::{ModuleEnv, PersistentSession, SessionId, SessionLib};
        eval_harness::require_extract();
        let root = tempfile::tempdir().unwrap();
        let include = tempfile::tempdir().unwrap();
        assert!(include.path().is_absolute());
        assert!(std::fs::read_dir(include.path()).unwrap().next().is_none());
        let lib =
            SessionLib::open(SessionId(903), root.path(), ModuleEnv::standalone_default()).unwrap();
        let session = PersistentSession::new(Some(lib), 1024 * 1024);
        let view = session
            .compile_view_in(tidepool_codegen::scope::ScopeId::ROOT)
            .unwrap();
        assert!(view.exact_declaration_context().is_none());
        assert!(view.reachable_values().is_empty());
        let values = session.capture_value_interfaces(&view).unwrap();
        assert!(values.is_empty());
        let inputs = AdmittedInspectionInputs::capture(view.clone(), values, Default::default());
        let queries = [InspectionQuery::TypeSearch("Int -> Int".into())];
        let results = tidepool_testing::with_settlement(|settlement| {
            run_admitted_inspections(
                InspectionRequest {
                    exact_context: None,
                    preamble: "module Expr where\nimport Prelude\n",
                    imports: "",
                    include: &[include.path()],
                    session_root: view.session_root(),
                    inject_modules: &[],
                    queries: &queries,
                    effects: None,
                },
                &view,
                &inputs,
                settlement,
            )
        })
        .expect("fresh empty checked snapshot supports a valid type search");
        assert!(
            matches!(&results[..], [InspectionResult::TypeMatches { matches, .. }]
            if matches.iter().any(|entry| entry.name == "id"
                && entry.module.as_deref() == Some("GHC.Internal.Base")
                && entry.quality == TypeMatchQuality::Usable)),
            "imported Prelude.id must match concrete Int -> Int: {results:#?}"
        );
    }

    fn encoded(value: CborValue) -> Vec<u8> {
        let mut bytes = Vec::new();
        ciborium::ser::into_writer(&value, &mut bytes).unwrap();
        bytes
    }

    fn complete_receipt(results: Vec<CborValue>) -> CborValue {
        CborValue::Array(vec![
            CborValue::Text("TPINSP008".into()),
            CborValue::Array(vec![
                CborValue::Text("Complete".into()),
                CborValue::Array(results),
            ]),
        ])
    }

    #[test]
    fn decodes_every_result_shape() {
        let receipt = complete_receipt(vec![
            CborValue::Array(vec![
                CborValue::Text("Type".into()),
                CborValue::Text("fmap".into()),
                CborValue::Text("Functor f => (a -> b) -> f a -> f b".into()),
                CborValue::Text("Unknown".into()),
            ]),
            CborValue::Array(vec![
                CborValue::Text("Info".into()),
                CborValue::Text("Maybe".into()),
                CborValue::Array(vec![CborValue::Array(vec![
                    CborValue::Text("Maybe".into()),
                    CborValue::Text("GHC.Internal.Maybe".into()),
                    CborValue::Text("type".into()),
                    CborValue::Text("data Maybe a = Nothing | Just a".into()),
                    CborValue::Text("Unknown".into()),
                    CborValue::Array(vec![CborValue::Array(vec![
                        CborValue::Text("GHC.Internal.Maybe".into()),
                        CborValue::Text("Just".into()),
                        CborValue::Text("Constructor".into()),
                    ])]),
                    CborValue::Array(vec![
                        CborValue::Text("GHC.Internal.Maybe".into()),
                        CborValue::Text("Maybe".into()),
                        CborValue::Text("Type".into()),
                    ]),
                ])]),
            ]),
            CborValue::Array(vec![
                CborValue::Text("Ambiguous".into()),
                CborValue::Text("Result".into()),
                CborValue::Array(vec![]),
            ]),
            CborValue::Array(vec![
                CborValue::Text("NotFound".into()),
                CborValue::Text("nope".into()),
            ]),
            CborValue::Array(vec![
                CborValue::Text("Browse".into()),
                CborValue::Text("Tidepool.Actors.Exomonad".into()),
                CborValue::Bool(true),
                CborValue::Array(vec![]),
            ]),
        ]);
        let decoded = decode_inspections(&encoded(receipt)).unwrap();
        assert!(matches!(
            decoded[0],
            InspectionResult::Type {
                availability: InspectionAvailability::Unknown,
                ..
            }
        ));
        assert!(
            matches!(decoded[1], InspectionResult::Info { ref entries, .. }
            if entries[0].availability == InspectionAvailability::Unknown
                && entries[0].references == [IdentifierRef {
                    module: "GHC.Internal.Maybe".into(), name: "Just".into(),
                    namespace: IdentifierNamespace::Constructor,
                }])
        );
        assert_eq!(
            decoded[2],
            InspectionResult::Ambiguous {
                query: "Result".into(),
                entries: vec![],
            }
        );
        assert_eq!(
            decoded[3],
            InspectionResult::NotFound {
                query: "nope".into()
            }
        );
        assert!(matches!(
            decoded[4],
            InspectionResult::Browse { expanded: true, .. }
        ));
    }

    #[test]
    fn decodes_structured_type_and_typed_query_error() {
        let scope = CborValue::Array(vec![
            CborValue::Text("PublicModule".into()),
            CborValue::Text("Project.Work".into()),
        ]);
        let query = CborValue::Array(vec![
            scope.clone(),
            CborValue::Text("Value".into()),
            CborValue::Text("work".into()),
        ]);
        let identifier = CborValue::Array(vec![
            CborValue::Text("Project.Work".into()),
            CborValue::Text("work".into()),
            CborValue::Text("Value".into()),
        ]);
        let expression = CborValue::Array(vec![
            CborValue::Text("Eq a => a -> Bool".into()),
            CborValue::Array(vec![CborValue::Text("a".into())]),
            CborValue::Array(vec![CborValue::Text("Eq a".into())]),
        ]);
        let provenance = CborValue::Array(vec![
            scope,
            CborValue::Integer(7.into()),
            CborValue::Text("scope-abc".into()),
        ]);
        let receipt = complete_receipt(vec![
            CborValue::Array(vec![
                CborValue::Text("StructuredTypeOk".into()),
                CborValue::Array(vec![identifier, expression, provenance]),
            ]),
            CborValue::Array(vec![
                CborValue::Text("StructuredError".into()),
                CborValue::Array(vec![CborValue::Text("Unknown".into()), query]),
            ]),
        ]);

        let decoded = decode_inspections(&encoded(receipt)).unwrap();
        let InspectionResult::StructuredType(Ok(info)) = &decoded[0] else {
            panic!("expected structured type: {:?}", decoded[0]);
        };
        assert_eq!(info.identifier.module, "Project.Work");
        assert_eq!(info.expression.variables, ["a"]);
        assert_eq!(info.expression.constraints, ["Eq a"]);
        assert_eq!(info.provenance.generation, 7);
        assert!(matches!(
            &decoded[1],
            InspectionResult::StructuredInfo(Err(QueryError::Unknown(NameQuery {
                scope: NameScope::PublicModule(module),
                namespace: NameNamespace::Value,
                name,
            }))) if module == "Project.Work" && name == "work"
        ));
    }

    #[test]
    fn rejects_version_shape_and_unknown_tag() {
        for value in [
            CborValue::Array(vec![
                CborValue::Text("TPINSP005".into()),
                CborValue::Array(vec![]),
            ]),
            CborValue::Array(vec![
                CborValue::Text("TPINSP004".into()),
                CborValue::Array(vec![CborValue::Text("Other".into())]),
            ]),
            CborValue::Array(vec![CborValue::Text("TPINSP001".into())]),
        ] {
            assert!(decode_inspections(&encoded(value)).is_err());
        }

        let mut trailing = encoded(complete_receipt(vec![CborValue::Array(vec![
            CborValue::Text("NotFound".into()),
            CborValue::Text("x".into()),
        ])]));
        trailing.push(0);
        assert!(decode_inspections(&trailing).is_err());
    }

    #[test]
    fn decodes_row_availability_and_rejects_unknown_values() {
        let receipt = |availability: &str| {
            complete_receipt(vec![CborValue::Array(vec![
                CborValue::Text("TypeMatches".into()),
                CborValue::Text("Eff effects ()".into()),
                CborValue::Array(vec![CborValue::Array(vec![
                    CborValue::Text("run".into()),
                    CborValue::Null,
                    CborValue::Text("Eff effects ()".into()),
                    CborValue::Text("Usable".into()),
                    CborValue::Text(availability.into()),
                    CborValue::Array(vec![]),
                    CborValue::Array(vec![
                        CborValue::Text("Example".into()),
                        CborValue::Text("run".into()),
                        CborValue::Text("Value".into()),
                    ]),
                ])]),
            ])])
        };
        for (encoded_name, expected) in [
            ("Available", InspectionAvailability::Available),
            ("Polymorphic", InspectionAvailability::Polymorphic),
            ("Unavailable", InspectionAvailability::Unavailable),
            ("Unknown", InspectionAvailability::Unknown),
        ] {
            let decoded = decode_inspections(&encoded(receipt(encoded_name))).unwrap();
            assert!(
                matches!(&decoded[0], InspectionResult::TypeMatches { matches, .. }
                if matches[0].availability == expected)
            );
        }
        assert!(decode_inspections(&encoded(receipt("Guess"))).is_err());
    }

    #[test]
    fn qualified_info_and_browse_share_one_checked_target() {
        eval_harness::require_extract();
        // The assertion reads the worker's `tidepool-checked` accounting
        // lines, which a compile daemon filters from the stderr it returns.
        // Bind a direct worker so the evidence is visible on every route.
        // Nextest runs each test in its own process, so this is not shared.
        std::env::remove_var(tidepool_extract_cmd::DAEMON_SOCKET_ENV);
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("Expr.hs");
        std::fs::write(
            &source,
            "module Expr where\nimport qualified Data.Maybe as M\n",
        )
        .unwrap();
        let output = temp.path().join("inspection.cbor");
        let mut command = ExtractCmd::new().unwrap();
        command
            .output_dir(temp.path())
            .inspect_out(&output)
            .input(&source)
            .inspect_info("M.Maybe")
            .input(&source)
            .inspect_browse("Data.Maybe", false);
        let run = command.bind().unwrap().execute(&command).unwrap();
        assert!(run.success(), "{}", run.stderr_lossy());
        let stderr = run.stderr_lossy();
        assert_eq!(
            stderr
                .matches("tidepool-checked module=Expr target=True")
                .count(),
            1,
            "{stderr}"
        );
        assert!(
            !stderr.contains("tidepool-target phase=desugar"),
            "metadata entered executable compilation: {stderr}"
        );
        let results = decode_inspections(&std::fs::read(output).unwrap()).unwrap();
        assert!(matches!(
            results.as_slice(),
            [
                InspectionResult::Info { .. },
                InspectionResult::Browse { .. }
            ]
        ));
    }

    #[test]
    fn one_inspection_compile_answers_type_info_and_browse_queries() {
        eval_harness::require_extract();
        let include = tempfile::tempdir().unwrap();
        let session = tempfile::tempdir().unwrap();
        std::fs::write(
            include.path().join("BrowseFixture.hs"),
            concat!(
                "{-# LANGUAGE TypeFamilies, DuplicateRecordFields #-}\n",
                "module BrowseFixture (Public(..), Record(..), TwinA(..), TwinB(..), Abstract, Opaque, Solo(..), Alias, Family, Service(..), constrained, exportedValue, Maybe(..)) where\n",
                "import Prelude\n",
                "import Data.Maybe (Maybe(..))\n",
                "data Public = First | Second\n",
                "data Record = Record { recordField :: Int }\n",
                "data TwinA = TwinA { sharedField :: Int }\n",
                "data TwinB = TwinB { sharedField :: Bool }\n",
                "data Abstract = Hidden\n",
                "newtype Opaque = Opaque Int\n",
                "data Solo = Solo\n",
                "type Alias a = Maybe a\n",
                "type family Family a\n",
                "class Service a where service :: a -> Int\n",
                "constrained :: Eq a => a -> Bool\n",
                "constrained x = x == x\n",
                "exportedValue :: Int\n",
                "exportedValue = 42\n",
            ),
        )
        .unwrap();
        let preamble = concat!(
            "{-# LANGUAGE NoImplicitPrelude, NoMonomorphismRestriction, PartialTypeSignatures #-}\n",
            "module Expr where\n",
            "import BrowseFixture\n",
            "import qualified BrowseFixture as Alias\n",
            "localPreambleValue = exportedValue\n",
            "__tidepool_fixture = exportedValue\n",
        );
        let current = ScopeProvenance {
            scope: NameScope::Current,
            generation: 11,
            fingerprint: "current-scope".into(),
        };
        let public = ScopeProvenance {
            scope: NameScope::PublicModule("BrowseFixture".into()),
            generation: 11,
            fingerprint: "public-module".into(),
        };
        let current_query = |namespace, name: &str| NameQuery {
            scope: NameScope::Current,
            namespace,
            name: name.into(),
        };
        let public_query = |namespace, name: &str| NameQuery {
            scope: NameScope::PublicModule("BrowseFixture".into()),
            namespace,
            name: name.into(),
        };
        let mut queries = vec![
            InspectionQuery::TypeOf("exportedValue".into()),
            InspectionQuery::Info("Public".into()),
            InspectionQuery::TypeOf("missing + 1".into()),
            InspectionQuery::Browse {
                module: "No.Such.Module".into(),
                expanded: false,
            },
            InspectionQuery::Browse {
                module: "BrowseFixture".into(),
                expanded: false,
            },
            InspectionQuery::Browse {
                module: "BrowseFixture".into(),
                expanded: true,
            },
            InspectionQuery::StructuredType {
                query: current_query(NameNamespace::Value, "constrained"),
                provenance: current.clone(),
            },
            InspectionQuery::StructuredInfo {
                query: public_query(NameNamespace::Type, "Record"),
                provenance: public.clone(),
            },
            InspectionQuery::StructuredInfo {
                query: public_query(NameNamespace::Type, "Abstract"),
                provenance: public.clone(),
            },
            InspectionQuery::StructuredInfo {
                query: public_query(NameNamespace::Type, "Alias"),
                provenance: public.clone(),
            },
            InspectionQuery::StructuredInfo {
                query: current_query(NameNamespace::Any, "Solo"),
                provenance: current.clone(),
            },
            InspectionQuery::StructuredInfo {
                query: current_query(NameNamespace::Constructor, "Solo"),
                provenance: current.clone(),
            },
            InspectionQuery::StructuredInfo {
                query: current_query(NameNamespace::Value, "missingName"),
                provenance: current.clone(),
            },
            InspectionQuery::StructuredInfo {
                query: NameQuery {
                    scope: NameScope::PublicModule("No.Such.Module".into()),
                    namespace: NameNamespace::Type,
                    name: "Missing".into(),
                },
                provenance: ScopeProvenance {
                    scope: NameScope::PublicModule("No.Such.Module".into()),
                    generation: 11,
                    fingerprint: "missing-module".into(),
                },
            },
            InspectionQuery::StructuredInfo {
                query: public_query(NameNamespace::Type, "Family"),
                provenance: public.clone(),
            },
            InspectionQuery::StructuredInfo {
                query: public_query(NameNamespace::Type, "Opaque"),
                provenance: public,
            },
        ];
        queries.push(InspectionQuery::Info("Alias.Public".into()));
        queries.push(InspectionQuery::Info("Alias.exportedValue".into()));
        queries.push(InspectionQuery::Info("Missing.Public".into()));
        queries.push(InspectionQuery::TypeSearch("Public".into()));
        queries.push(InspectionQuery::TypeSearch("Public ->".into()));
        queries.push(InspectionQuery::TypeSearch("Public -> _".into()));
        queries.push(InspectionQuery::ScopeBrowse);
        // A preamble declaration lives only in the typechecked query module;
        // a request's `respond` is bound this way.
        queries.push(InspectionQuery::Info("localPreambleValue".into()));
        let results = tidepool_testing::with_settlement(|settlement| {
            run_inspections(
                InspectionRequest {
                    exact_context: None,
                    preamble,
                    imports: "",
                    include: &[include.path()],
                    session_root: session.path(),
                    inject_modules: &[],
                    queries: &queries,
                    effects: None,
                },
                settlement,
            )
        })
        .unwrap();

        assert_eq!(results.len(), queries.len());
        let InspectionResult::Browse {
            module, entries, ..
        } = &results[22]
        else {
            panic!("expected current-scope browse, got {:?}", results[22]);
        };
        assert!(module.is_empty());
        assert!(entries.iter().any(|entry| entry.name == "exportedValue"));
        assert!(entries.iter().any(|entry| entry.name == "recordField"));
        assert!(!entries.iter().any(|entry| entry.name == "Hidden"));
        assert!(!entries
            .iter()
            .any(|entry| entry.name.starts_with("__tidepool_")));

        assert!(results[0].render().contains("exportedValue :: Int"));
        assert!(results[1].render().contains("data Public"));
        assert!(
            results[23].render().contains("localPreambleValue :: Int"),
            "{:?}",
            results[23]
        );
        assert!(matches!(results[2], InspectionResult::Rejected { .. }));
        assert_eq!(
            results[3],
            InspectionResult::ModuleNotFound {
                module: "No.Such.Module".into(),
                scopes: vec![NameScope::PublicModule("No.Such.Module".into())],
            }
        );
        // Alias.Public / Alias.exportedValue / Missing.Public / the three TypeSearch
        // queries were originally the last six entries pushed onto `queries` and were
        // checked at indices 6-11. The StructuredType/StructuredInfo queries were later
        // spliced into the `vec![...]` literal ahead of those pushes, shifting every
        // pushed query down by ten slots (they now live at indices 16-21) without these
        // assertions being renumbered. Left at 6-11 they instead re-checked the spliced-in
        // StructuredType/StructuredInfo queries, contradicting the dedicated structured
        // assertions for those indices below and failing outright on results[6].
        assert!(results[16].render().contains("data Public"));
        assert!(results[17].render().contains("exportedValue :: Int"));
        assert!(matches!(results[18], InspectionResult::NotFound { .. }));
        assert!(
            matches!(results[19], InspectionResult::TypeMatches { .. }),
            "{:?}",
            results[19]
        );
        assert!(matches!(results[20], InspectionResult::Rejected { .. }));
        assert!(
            matches!(results[21], InspectionResult::TypeMatches { .. }),
            "{:?}",
            results[21]
        );
        let grouped = results[4].render();
        assert!(grouped.starts_with("-- BrowseFixture\n"));
        assert!(grouped.contains("data Public"));
        assert!(grouped.contains("class Service"));
        assert!(grouped.contains("exportedValue :: Int"));
        assert!(!grouped.lines().any(|line| line.starts_with("First ::")));

        // A browse must not advertise a constructor the module keeps to itself.
        // `Abstract` and `Opaque` are exported without `(..)`, so a cell cannot
        // write `Hidden`; showing it sent a live agent into four rounds of
        // recovery after `Data constructor not in scope`.
        assert!(!grouped.contains("Hidden"), "{grouped}");
        assert!(grouped.contains("data Abstract"), "{grouped}");
        // The type `Opaque` is exported and its like-named data constructor is
        // not; they differ by namespace and must be treated separately.
        assert!(
            !grouped.lines().any(|line| line.contains("newtype Opaque")
                && line.contains('=')
                && !line.contains("...")),
            "{grouped}"
        );
        // A constructor the module does export still renders in place.
        assert!(grouped.contains("First"), "{grouped}");
        let InspectionResult::Browse { entries, .. } = &results[5] else {
            panic!("expected expanded browse");
        };
        let references = |name: &str| {
            &entries
                .iter()
                .find(|entry| entry.name == name)
                .unwrap()
                .references
        };
        assert!(references("constrained")
            .iter()
            .any(|reference| reference.name == "Eq"));
        assert!(references("Alias")
            .iter()
            .any(|reference| reference.name == "Maybe"));
        assert!(references("recordField")
            .iter()
            .any(|reference| reference.name == "Record"
                && reference.namespace == IdentifierNamespace::Type));
        assert!(references("Record")
            .iter()
            .any(|reference| reference.name == "recordField"
                && reference.namespace == IdentifierNamespace::Field));
        assert!(references("Abstract").is_empty());
        assert!(references("Opaque").is_empty());
        assert!(references("First")
            .iter()
            .any(|reference| reference.name == "Public"));
        let expanded = results[5].render();
        assert!(!expanded.contains("Hidden"), "{expanded}");
        assert!(
            !expanded.lines().any(|line| line.contains("newtype Opaque")
                && line.contains('=')
                && !line.contains("...")),
            "{expanded}"
        );
        assert!(expanded.contains("First :: Public"), "{expanded}");
        assert!(expanded.contains("service ::"), "{expanded}");
        assert!(expanded.contains("data Maybe"), "{expanded}");

        let InspectionResult::StructuredType(Ok(constrained)) = &results[6] else {
            panic!("expected constrained function type: {:?}", results[6]);
        };
        assert_eq!(constrained.identifier.name, "constrained");
        assert!(
            constrained
                .expression
                .constraints
                .iter()
                .any(|constraint| constraint.contains("Eq")),
            "{:?}",
            constrained.expression
        );
        assert_eq!(constrained.provenance.fingerprint, "current-scope");

        let InspectionResult::StructuredInfo(Ok(record)) = &results[7] else {
            panic!("expected record declaration: {:?}", results[7]);
        };
        let DeclarationInfo::Data { constructors, .. } = &record.declaration else {
            panic!("expected data declaration: {:?}", record.declaration);
        };
        assert_eq!(constructors.len(), 1);
        assert_eq!(constructors[0].fields[0].name, "recordField");

        let InspectionResult::StructuredInfo(Ok(abstract_type)) = &results[8] else {
            panic!("expected abstract type declaration: {:?}", results[8]);
        };
        assert!(matches!(
            &abstract_type.declaration,
            DeclarationInfo::Data { constructors, .. } if constructors.is_empty()
        ));

        let InspectionResult::StructuredInfo(Ok(alias)) = &results[9] else {
            panic!("expected type synonym: {:?}", results[9]);
        };
        assert!(
            matches!(
                &alias.declaration,
                DeclarationInfo::TypeSynonym { parameters, body }
                    if parameters.len() == 1 && body.canonical.contains("Maybe")
            ),
            "{:?}",
            alias.declaration
        );
        assert!(matches!(
            &results[10],
            InspectionResult::StructuredInfo(Err(QueryError::Ambiguous(query, candidates)))
                if query.name == "Solo" && candidates.len() == 2
        ));

        let InspectionResult::StructuredInfo(Ok(constructor)) = &results[11] else {
            panic!("expected constructor declaration: {:?}", results[11]);
        };
        assert!(matches!(
            (&constructor.declaration, &constructor.parent),
            (DeclarationInfo::Constructor { parent, .. }, Some(recorded_parent))
                if parent.name == "Solo" && recorded_parent == parent
        ));
        assert!(matches!(
            &results[12],
            InspectionResult::StructuredInfo(Err(QueryError::Unknown(query)))
                if query.name == "missingName"
        ));
        assert!(matches!(
            &results[13],
            InspectionResult::StructuredInfo(Err(QueryError::UnknownModule(module)))
                if module == "No.Such.Module"
        ));
        assert!(matches!(
            &results[14],
            InspectionResult::StructuredInfo(Err(QueryError::Unsupported(detail)))
                if detail.contains("unsupported")
        ));
        assert!(matches!(
            &results[15],
            InspectionResult::StructuredInfo(Err(QueryError::Unsupported(detail)))
                if detail.contains("abstract newtype constructor")
        ));
    }

    #[test]
    fn info_with_effect_row_imports_its_generated_qualified_names() {
        eval_harness::require_extract();
        let session = tempfile::tempdir().unwrap();
        let mut effects_include = eval_harness::effects_include().to_vec();
        effects_include.push(eval_harness::prelude_path());
        effects_include.push(eval_harness::actor_sources_path());
        let include = effects_include
            .iter()
            .map(std::path::PathBuf::as_path)
            .collect::<Vec<_>>();
        let inspect = |queries: &[InspectionQuery]| {
            tidepool_testing::with_settlement(|settlement| {
                run_inspections(
                    InspectionRequest {
                        exact_context: None,
                        preamble: concat!(
                            "{-# LANGUAGE NoImplicitPrelude, DataKinds, TypeOperators #-}\n",
                            "{-# LANGUAGE FlexibleContexts, TypeFamilies, ConstraintKinds #-}\n",
                            "module Expr where\n",
                            "import Prelude\n",
                            "import qualified Tidepool.Effects.Core as InspectionEffects\n",
                            "import Tidepool.Actors.Spawn (checkpoint, spawnSubagent)\n",
                        ),
                        imports: "",
                        include: &include,
                        session_root: session.path(),
                        inject_modules: &[],
                        queries,
                        effects: Some(&super::super::HaskellTypeSource::from(
                            "Tidepool.Effects.Core.AgentTools ': \
                 Tidepool.Agent.Contract.SyncEffects \
                 (Tidepool.Effects.Core.AgentLaunch ': '[])",
                        )),
                    },
                    settlement,
                )
            })
            .unwrap()
        };
        let names = [
            ("map", None),
            ("checkpoint", Some("ContextCheckpoint")),
            ("spawnSubagent", Some("AgentRef")),
        ];
        let queries = names.map(|(name, _)| InspectionQuery::Info(name.into()));
        let results = inspect(&queries);

        assert_eq!(results.len(), names.len());
        for (result, (name, result_type)) in results.iter().zip(names) {
            assert!(
                matches!(result, InspectionResult::Info { query, entries }
                    if query == name && entries.iter().any(|entry|
                        entry.name == name && entry.kind == "value"
                        && matches!(entry.availability,
                            InspectionAvailability::Available | InspectionAvailability::Polymorphic)
                        && result_type.is_none_or(|result_type|
                            entry.references.iter().any(|reference| reference.name == result_type)))),
                "{result:?}"
            );
        }

        // The homogeneous type-query path assembles an additional shared module.
        let queries =
            ["checkpoint", "spawnSubagent"].map(|name| InspectionQuery::TypeOf(name.into()));
        let results = inspect(&queries);
        assert_eq!(results.len(), 2);
        for (result, (name, result_type)) in results.iter().zip([
            ("checkpoint", "ContextCheckpoint"),
            ("spawnSubagent", "AgentRef"),
        ]) {
            assert!(
                matches!(result, InspectionResult::Type { expression, display, .. }
                    if expression == name && display.contains(result_type)),
                "{result:?}"
            );
        }
    }

    #[test]
    fn type_probe_batch_preserves_independent_types_and_rejected_siblings() {
        eval_harness::require_extract();
        let session = tempfile::tempdir().unwrap();
        let preamble = concat!(
            "{-# LANGUAGE NoImplicitPrelude, ExtendedDefaultRules #-}\n",
            "module Expr where\n",
            "import Prelude\n",
        );
        let valid_queries = [
            InspectionQuery::TypeOf("id".into()),
            InspectionQuery::TypeOf("1".into()),
            InspectionQuery::TypeOf("const".into()),
        ];
        let inspect = |queries: &[InspectionQuery]| {
            tidepool_testing::with_settlement(|settlement| {
                run_inspections(
                    InspectionRequest {
                        exact_context: None,
                        preamble,
                        imports: "",
                        include: &[],
                        session_root: session.path(),
                        inject_modules: &[],
                        queries,
                        effects: None,
                    },
                    settlement,
                )
            })
            .unwrap()
        };

        let valid = inspect(&valid_queries);
        let singleton_results = valid_queries
            .iter()
            .map(|query| inspect(std::slice::from_ref(query)).remove(0))
            .collect::<Vec<_>>();
        assert_eq!(valid, singleton_results);
        assert!(matches!(
            &valid[0],
            InspectionResult::Type { expression, display, .. }
                if expression == "id" && display == "a -> a"
        ));
        assert!(matches!(
            &valid[1],
            InspectionResult::Type { expression, display, .. }
                if expression == "1" && !display.is_empty()
        ));
        assert!(matches!(
            &valid[2],
            InspectionResult::Type { expression, display, .. }
                if expression == "const" && display == "a -> b -> a"
        ));

        let invalid_queries = [
            InspectionQuery::TypeOf("id".into()),
            InspectionQuery::TypeOf("missing + 1".into()),
            InspectionQuery::TypeOf("const".into()),
        ];
        let isolated = inspect(&invalid_queries);
        assert!(
            matches!(&isolated[0], InspectionResult::Type { expression, .. } if expression == "id")
        );
        assert!(matches!(&isolated[1], InspectionResult::Rejected { .. }));
        assert!(
            matches!(&isolated[2], InspectionResult::Type { expression, .. } if expression == "const")
        );

        // Generated probe bindings share a module only in the batch form. If
        // authored source names that private prefix, keep singleton scoping so
        // one query cannot resolve another query's generated binder.
        let generated_name_queries = [
            InspectionQuery::TypeOf("id".into()),
            InspectionQuery::TypeOf("__tidepool_inspect_0 1".into()),
        ];
        let generated_name_results = inspect(&generated_name_queries);
        let generated_name_singletons = generated_name_queries
            .iter()
            .map(|query| inspect(std::slice::from_ref(query)).remove(0))
            .collect::<Vec<_>>();
        assert!(matches!(
            (&generated_name_results[0], &generated_name_singletons[0]),
            (InspectionResult::Type { display: batched, .. }, InspectionResult::Type { display: singleton, .. })
                if batched == singleton
        ));
        assert!(matches!(
            (&generated_name_results[1], &generated_name_singletons[1]),
            (
                InspectionResult::Rejected { .. },
                InspectionResult::Rejected { .. }
            )
        ));
    }

    #[test]
    fn checked_target_keeps_its_boot_interface_for_source_import_cycles() {
        eval_harness::require_extract();
        let temp = tempfile::tempdir().unwrap();
        std::fs::write(
            temp.path().join("Expr.hs-boot"),
            "module Expr where\nimport Prelude\neven' :: Int -> Bool\n",
        )
        .unwrap();
        std::fs::write(
            temp.path().join("Odd.hs"),
            concat!(
                "module Odd where\n",
                "import Prelude\n",
                "import {-# SOURCE #-} qualified Expr\n",
                "odd' :: Int -> Bool\n",
                "odd' n = n /= 0 && Expr.even' (n - 1)\n",
            ),
        )
        .unwrap();
        let source = temp.path().join("Expr.hs");
        std::fs::write(
            &source,
            assemble_inspection_module(
                concat!(
                    "module Expr where\n",
                    "import Prelude\n",
                    "import qualified Odd\n",
                    "even' :: Int -> Bool\n",
                    "even' n = n == 0 || Odd.odd' (n - 1)\n",
                ),
                "",
                &["even'".into()],
            ),
        )
        .unwrap();
        let output = temp.path().join("inspection.cbor");
        let mut command = ExtractCmd::new().unwrap();
        command
            .output_dir(temp.path())
            .inspect_out(&output)
            .includes([temp.path()])
            .session_root(temp.path())
            .input(&source)
            .inspect_type("even'");
        let run = command.bind().unwrap().execute(&command).unwrap();
        assert!(run.success(), "{}", run.stderr_lossy());
        let results = decode_inspections(&std::fs::read(output).unwrap()).unwrap();

        assert!(
            matches!(
                results.as_slice(),
                [InspectionResult::Type { display, .. }] if display == "Int -> Bool"
            ),
            "{results:?}"
        );
    }

    #[test]
    fn ninety_five_type_probes_share_one_valid_batch() {
        eval_harness::require_extract();
        let session = tempfile::tempdir().unwrap();
        let queries = (0..95)
            .map(|_| InspectionQuery::TypeOf("id".into()))
            .collect::<Vec<_>>();
        let results = tidepool_testing::with_settlement(|settlement| {
            run_inspections(
                InspectionRequest {
                    exact_context: None,
                    preamble: concat!(
                        "{-# LANGUAGE NoImplicitPrelude #-}\n",
                        "module Expr where\n",
                        "import Prelude\n",
                    ),
                    imports: "",
                    include: &[],
                    session_root: session.path(),
                    inject_modules: &[],
                    queries: &queries,
                    effects: None,
                },
                settlement,
            )
        })
        .unwrap();

        assert_eq!(results.len(), 95);
        assert!(results.iter().all(|result| matches!(
            result,
            InspectionResult::Type {
                expression,
                display,
                ..
            } if expression == "id" && display.contains("->")
        )));
    }
}
