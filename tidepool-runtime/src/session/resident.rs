//! Resident prepared-STG sessions.
//!
//! Each turn installs or reuses a prepared program in one long-lived machine.
//! Completed turns return the machine to the session; suspended turns park a
//! rooted continuation while later turns and other resumptions remain usable.
//! The machine moves to an evaluation thread only for the duration of an entry.

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use parking_lot::Mutex;

use tidepool_bridge::HaskellValue;
use tidepool_codegen::binding_table::{BindingEntry, BoundValue};
use tidepool_codegen::prepared_program::{PreparedHandle, ProgramId};
use tidepool_repr::execution_schema::SymbolIdentity;

use super::prepared::{ParkPolicy, PreparedRuntimeError, PreparedSettlement};
use super::turn::TurnCode;
use tidepool_codegen::suspension::{ContinuationId, RealmId, ValueHandle};
use tidepool_effect::dispatch::{request_constructor, DispatchEffect, EffectContext, Response};
use tidepool_effect::error::EffectError;
use tidepool_effect::{EffectRunPolicy, LivePayloadPolicy};
use tidepool_repr::{
    BindingName, DataConId, DataConTable, Generation, MonotonicIdIssuer, SessionModule,
    SessionVarId,
};

use crate::render::EvalResult;
use crate::timing;
use crate::{RuntimeError, YieldSite, YieldSiteCollision, EVAL_STACK_SIZE};

enum ResidentResumeInput {
    Response(Response),
    Handle(ValueHandle),
    FramedHandle {
        handle: ValueHandle,
        constructor: tidepool_repr::DataConId,
        prefix: Vec<HaskellValue>,
    },
    Abort(String),
}

/// Immutable compiler provenance that travels with live Haskell programs.
/// Sites are globally stable, while the map makes accidental hash collisions
/// loud before a continuation can be resumed against the wrong type.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ProgramProvenance {
    sites: BTreeMap<u64, YieldSite>,
}

pub type ProgramProvenanceError = YieldSiteCollision;

impl ProgramProvenance {
    pub fn from_sites(sites: &[YieldSite]) -> Result<Self, ProgramProvenanceError> {
        let mut provenance = Self::default();
        provenance.extend(sites)?;
        Ok(provenance)
    }

    fn extend(&mut self, sites: &[YieldSite]) -> Result<(), ProgramProvenanceError> {
        for site in sites {
            if let Some(previous) = self.sites.get(&site.site) {
                if previous != site {
                    return Err(YieldSiteCollision {
                        site: site.site,
                        first: Box::new(previous.clone()),
                        second: Box::new(site.clone()),
                    });
                }
            } else {
                self.sites.insert(site.site, site.clone());
            }
        }
        Ok(())
    }

    fn merge(&mut self, other: &Self) -> Result<(), ProgramProvenanceError> {
        for site in other.sites.values() {
            self.extend(std::slice::from_ref(site))?;
        }
        Ok(())
    }

    #[must_use]
    pub fn sites(&self) -> Vec<YieldSite> {
        self.sites.values().cloned().collect()
    }
}

use tidepool_codegen::scope::ScopeId;
use tidepool_repr::PrincipalId;

use super::persistent::{PersistentSession, ScopeRetirement};
use super::turn::{BoundBinder, HostBindingAuthority, ValueTier};
use super::OutputSink;
use super::{SessionError, SessionLib, SourceImports};

/// Runtime context applied to every entry into a resident session.
///
/// These fields describe one logical execution window: `resource_scope`
/// owns parked frames and live handles, while `lexical_scope` selects the
/// declarations and bindings visible to compilation, and `principal` names
/// the exact runtime authority used by effect handlers. Keeping them in one
/// value prevents a shared session from combining one caller's heap ownership,
/// lexical environment, and privileges with another caller's.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SessionRunContext {
    pub resource_scope: RealmId,
    pub lexical_scope: ScopeId,
    pub principal: PrincipalId,
}

/// A compiler-issued host mount must have this exact outer nominal type. The
/// unit is authenticated by [`BoundBinder::host_authority`]; module and type
/// constructor identify the shipped surface the host builder knows how to
/// construct.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HostBindingType {
    authority: HostBindingAuthority,
    module: &'static str,
    name: &'static str,
    constructors: &'static [&'static str],
}

impl HostBindingType {
    pub const JSON_VALUE: Self = Self {
        authority: HostBindingAuthority::JsonValue,
        module: "Tidepool.Aeson.Value",
        name: "Value",
        constructors: &[
            "Tidepool.Aeson.Value.Object",
            "Tidepool.Aeson.Value.Array",
            "Tidepool.Aeson.Value.String",
            "Tidepool.Aeson.Value.Number",
            "Tidepool.Aeson.Value.Bool",
            "Tidepool.Aeson.Value.Null",
        ],
    };
    pub const TEXT: Self = Self {
        authority: HostBindingAuthority::Text,
        module: "Data.Text.Internal",
        name: "Text",
        constructors: &["Data.Text.Text"],
    };
    pub const COMMAND_JOB: Self = Self {
        authority: HostBindingAuthority::CommandJob,
        module: "Tidepool.Command.Types",
        name: "Job",
        constructors: &["Tidepool.Command.Types.Job"],
    };
}

/// Require the compiler-issued sidecar before any host mount can merge a
/// constructor table or install its carrier program. A same-spelling type from
/// another unit has no sidecar, because the extractor compares its exact GHC
/// module identity while minting this tag.
fn require_host_binding_authority(
    binder: &BoundBinder,
    expected: HostBindingType,
) -> Result<(), ResidentError> {
    if binder.host_authority == Some(expected.authority) {
        return Ok(());
    }
    Err(ResidentError::Run(RuntimeError::Jit(EffectError::Handler(
        format!(
            "compiled binder `{}` has host authority {:?}; host mount requires {:?}",
            binder.name, binder.host_authority, expected.authority,
        ),
    ))))
}

impl SessionRunContext {
    pub const ROOT: Self = Self {
        resource_scope: RealmId::ROOT,
        lexical_scope: ScopeId::ROOT,
        principal: PrincipalId::SYSTEM,
    };

    #[must_use]
    pub const fn new(
        resource_scope: RealmId,
        lexical_scope: ScopeId,
        principal: PrincipalId,
    ) -> Self {
        Self {
            resource_scope,
            lexical_scope,
            principal,
        }
    }
}

impl Default for SessionRunContext {
    fn default() -> Self {
        Self::ROOT
    }
}

/// Exclusive custody of one machine-rooted value.
///
/// The session creates custody when a finalized value leaves a parked frame.
/// Consuming operations may deliver it once, adopt it into a binding, move it
/// to another resource scope, or discard it. Raw [`ValueHandle`] access stays
/// inside this module, so external callers cannot duplicate an ownership
/// token through a numeric ID. Dropping custody queues its root for release
/// at the next mutable entry into its originating session; resource-scope or
/// machine teardown remains the final cleanup backstop if the session is
/// never entered again.
///
#[must_use = "custody must be delivered, mounted, retained, or deliberately discarded"]
#[derive(Debug)]
pub struct RootCustody {
    handle: Option<ValueHandle>,
    cleanup: Arc<CustodyCleanup>,
    provenance: Arc<ProgramProvenance>,
    /// `true` when this token ALIASES a handle another owner (a live
    /// binding, at present -- see [`ResidentSession::prepared_binding_handle`])
    /// already keeps rooted, rather than exclusively owning it. An ordinary
    /// (non-shared) custody's whole contract is "abandon it and its root is
    /// released" -- exactly wrong for an alias, since abandoning the ALIAS
    /// must not touch the root the other owner still needs. Sharing only
    /// changes what an unconsumed drop does; every consuming operation
    /// (delivery, mount, discard) behaves exactly as it does for an
    /// exclusive custody.
    shared: bool,
}

// Custody must remain exclusive.
static_assertions::assert_not_impl_any!(RootCustody: Clone, Copy);

impl RootCustody {
    /// Wrap a handle minted by the resident session.
    fn new(
        handle: ValueHandle,
        cleanup: Arc<CustodyCleanup>,
        provenance: Arc<ProgramProvenance>,
    ) -> Self {
        RootCustody {
            handle: Some(handle),
            cleanup,
            provenance,
            shared: false,
        }
    }

    /// [`Self::new`], but the wrapped handle aliases a root some other
    /// owner already keeps alive (see the `shared` field doc) — dropping
    /// this token unconsumed must not enqueue that root for release.
    fn shared(
        handle: ValueHandle,
        cleanup: Arc<CustodyCleanup>,
        provenance: Arc<ProgramProvenance>,
    ) -> Self {
        RootCustody {
            handle: Some(handle),
            cleanup,
            provenance,
            shared: true,
        }
    }

    #[must_use]
    pub fn provenance(&self) -> &ProgramProvenance {
        &self.provenance
    }

    fn into_transfer(mut self) -> CustodyTransfer {
        let Some(handle) = self.handle.take() else {
            unreachable!("live custody always contains its handle");
        };
        CustodyTransfer {
            handle,
            cleanup: Arc::clone(&self.cleanup),
            provenance: Arc::clone(&self.provenance),
            committed: false,
            shared: self.shared,
        }
    }
}

impl Drop for RootCustody {
    fn drop(&mut self) {
        if let Some(handle) = self.handle.take() {
            if !self.shared {
                self.cleanup.enqueue(handle);
            }
        }
    }
}

/// Retains exact binding identities while compiled work waits to execute.
/// Drop uses the session's custody cleanup queue, including cancellation before
/// prepared work is returned. Reclamation occurs on the next session entry or
/// at teardown, as it does for dropped value custody.
#[must_use]
#[derive(Debug)]
pub struct BindingLease {
    retained: Vec<SessionVarId>,
    cleanup: Arc<CustodyCleanup>,
}

#[derive(thiserror::Error, Debug)]
pub enum BindingAliasError {
    #[error("binding alias lease belongs to a different resident session")]
    ForeignLease,
    #[error("binding alias source {0:?} is not retained by its lease")]
    SourceNotLeased(SessionVarId),
    #[error("binding alias source {0:?} is not materialized")]
    MissingSource(SessionVarId),
    #[error("binding alias source {binding:?} belongs to scope {actual:?}, not {expected:?}")]
    WrongScope {
        binding: SessionVarId,
        actual: ScopeId,
        expected: ScopeId,
    },
    #[error("binding alias identity {0:?} is already materialized")]
    IdentityInUse(SessionVarId),
    #[error("binding alias compiler module does not match its reserved generation")]
    WrongModule,
    #[error("binding alias generation does not supersede the visible name")]
    StaleGeneration,
}

impl Drop for BindingLease {
    fn drop(&mut self) {
        self.cleanup
            .binding_leases
            .lock()
            .push(std::mem::take(&mut self.retained));
    }
}

#[derive(Debug, Default)]
struct CustodyCleanup {
    abandoned: Mutex<Vec<ValueHandle>>,
    binding_leases: Mutex<Vec<Vec<SessionVarId>>>,
}

impl CustodyCleanup {
    fn enqueue(&self, handle: ValueHandle) {
        self.abandoned.lock().push(handle);
    }

    fn take_all(&self) -> Vec<ValueHandle> {
        std::mem::take(&mut *self.abandoned.lock())
    }
}

struct CustodyTransfer {
    handle: ValueHandle,
    cleanup: Arc<CustodyCleanup>,
    provenance: Arc<ProgramProvenance>,
    committed: bool,
    /// Carried from the source [`RootCustody`] — see that type's `shared`
    /// field doc. A transfer that fails before `commit` drops uncommitted,
    /// same as an ordinary abandoned custody, so this must agree.
    shared: bool,
}

impl CustodyTransfer {
    fn commit(mut self) {
        self.committed = true;
    }

    fn into_custody(mut self) -> RootCustody {
        self.committed = true;
        if self.shared {
            RootCustody::shared(
                self.handle,
                Arc::clone(&self.cleanup),
                Arc::clone(&self.provenance),
            )
        } else {
            RootCustody::new(
                self.handle,
                Arc::clone(&self.cleanup),
                Arc::clone(&self.provenance),
            )
        }
    }
}

impl Drop for CustodyTransfer {
    fn drop(&mut self) {
        if !self.committed && !self.shared {
            self.cleanup.enqueue(self.handle);
        }
    }
}

/// A parked turn's own completion obligation, carried on the token
/// [`ResidentSession::run_with_sites`]/[`ResidentSession::run_bind_with_sites`]/[`ResidentSession::run_rooted_entry`]
/// hand back on suspension: a value turn's hole needs nothing extra to resume;
/// a binding turn's hole must materialize its binder into the value plane on
/// completion; and a projected hole must atomically materialize every
/// GHC-reported pattern binder. Binding
/// obligations retain the SAME binder metadata, generation, and lexical scope
/// carried by the initiating operation.
///
/// None of the hole payloads have public constructors or fields. The session
/// creates them at suspension time, keeping each completion obligation
/// inseparable from the token consumed by [`ResidentSession::resume`].
#[derive(Clone, Debug)]
pub struct PlainHole {
    id: String,
}

/// See [`ResidentHole`]'s doc — the `Binding` variant's payload.
#[derive(Clone, Debug)]
pub struct BindingHole {
    id: String,
    binder: BoundBinder,
    generation: Generation,
    observation: Option<Vec<tidepool_repr::VarId>>,
    lexical_scope: ScopeId,
}

/// See [`ResidentHole`]'s doc — a projected pattern bind retains every GHC
/// binder and its one shared value generation across suspension.
#[derive(Clone, Debug)]
pub struct ProjectedBindingHole {
    id: String,
    binders: Vec<BoundBinder>,
    generation: Generation,
    lexical_scope: ScopeId,
}

/// The public continuation token: a sum over a parked turn's completion
/// obligation. See the hole payload docs for why no variant is externally
/// constructible.
#[derive(Clone, Debug)]
pub enum ResidentHole {
    Plain(PlainHole),
    Binding(BindingHole),
    ProjectedBinding(ProjectedBindingHole),
}

impl ResidentHole {
    /// The minted continuation id this hole was parked under — the same
    /// identity [`ResidentSession::pending_continuation`]/[`ResidentSession::parked_holes`]
    /// read, for display/logging/tree-bookkeeping purposes that don't need
    /// (and shouldn't carry) the resume obligation itself.
    pub fn cont_id(&self) -> &str {
        match self {
            ResidentHole::Plain(h) => &h.id,
            ResidentHole::Binding(h) => &h.id,
            ResidentHole::ProjectedBinding(h) => &h.id,
        }
    }

    fn mint(id: String, seed: HoleSeed) -> Self {
        match seed {
            HoleSeed::Plain => ResidentHole::Plain(PlainHole { id }),
            HoleSeed::Binding {
                binder,
                generation,
                observation,
                lexical_scope,
            } => ResidentHole::Binding(BindingHole {
                id,
                binder,
                generation,
                observation,
                lexical_scope,
            }),
            HoleSeed::ProjectedBinding {
                binders,
                generation,
                lexical_scope,
            } => ResidentHole::ProjectedBinding(ProjectedBindingHole {
                id,
                binders,
                generation,
                lexical_scope,
            }),
        }
    }

    /// This hole's own seed — what [`ResidentSession::resume`] re-mints a
    /// fresh hole as, should this resume re-suspend: a Binding hole's chain
    /// of re-suspensions all carry the SAME binder/generation/scope through to
    /// whichever one finally completes.
    fn seed(&self) -> HoleSeed {
        match self {
            ResidentHole::Plain(_) => HoleSeed::Plain,
            ResidentHole::Binding(h) => HoleSeed::Binding {
                binder: h.binder.clone(),
                generation: h.generation,
                observation: h.observation.clone(),
                lexical_scope: h.lexical_scope,
            },
            ResidentHole::ProjectedBinding(h) => HoleSeed::ProjectedBinding {
                binders: h.binders.clone(),
                generation: h.generation,
                lexical_scope: h.lexical_scope,
            },
        }
    }

    /// Construct a `Plain` hole from a bare continuation id, for a caller
    /// whose own bookkeeping stores just the id string (the self-iterating
    /// harness driver's `render`/`loop`/green-thread threads — every one of
    /// those goes through [`ResidentSession::run`]/[`ResidentSession::run_rooted_entry`],
    /// never [`ResidentSession::run_bind`]) rather than the [`ResidentHole`]
    /// this API otherwise hands back. NOT a backdoor around the
    /// completion-obligation guarantee: the one failure mode this type
    /// exists to prevent — a `Binding` hole silently resumed as `Plain`,
    /// dropping its value-plane materialization — is still impossible
    /// through this constructor, because it can only ever produce `Plain`.
    /// There is no way to fabricate a `Binding` hole from a bare string; a
    /// real suspension through `run_bind` is the only source of one.
    pub fn plain(cont_id: impl Into<String>) -> Self {
        ResidentHole::Plain(PlainHole { id: cont_id.into() })
    }
}

/// What kind of hole [`ResidentSession::classify_parked`] mints on a fresh
/// suspension — [`ResidentHole`] minus the id, which is minted alongside it.
#[derive(Clone)]
enum HoleSeed {
    Plain,
    Binding {
        binder: BoundBinder,
        generation: Generation,
        observation: Option<Vec<tidepool_repr::VarId>>,
        lexical_scope: ScopeId,
    },
    ProjectedBinding {
        binders: Vec<BoundBinder>,
        generation: Generation,
        lexical_scope: ScopeId,
    },
}

/// The classified result of driving a resident turn to its first yield.
///
/// The suspend-and-completion shape mirrors [`super::TurnOutcome`], but a
/// resident turn is driven by direct `run_*` calls (not the oneshot engine), so
/// this is a distinct, smaller enum: no `Paused`/`TimedOut` (timeout-yield is
/// permanently excluded from the stowable resident path, by design),
/// and completion distinguishes value-producing turns from projected binds
/// whose products were installed directly into the lexical scope.
// `Completed`'s `EvalResult` is the large variant; this is a transient
// boundary carrier destructured immediately by the caller, so the size
// asymmetry is inherent, not a leak.
#[derive(Debug)]
#[allow(clippy::large_enum_variant)]
pub enum ResidentOutcome {
    /// The turn ran to completion. `result` is the bridged result value; the
    /// machine is back in its slot, ready for the next turn.
    Completed {
        output: Vec<String>,
        result: EvalResult,
    },
    /// A projected pattern bind completed and its named values were installed
    /// in the resident lexical scope. Unlike an expression completion, this
    /// operation has no result value of its own.
    BindingsCommitted { output: Vec<String> },
    /// The turn suspended at an `Ask`. The machine holds the continuation
    /// internally (stowed as data); call [`ResidentSession::resume`] with the
    /// answer. `request` is the bridged `Ask` request; `hole` is the minted
    /// continuation id.
    Suspended {
        output: Vec<String>,
        hole: ResidentHole,
        request: HaskellValue,
    },
}

/// The rendered metadata from one compiler-produced display bundle.
///
/// The page itself is installed in the value plane before this metadata is
/// forced.  Consequently a renderer failure leaves the captured observation
/// available to the caller, while the compiler-provided `cellDisplay` alias is
/// only published after metadata observation succeeds.
#[derive(Debug)]
pub struct ResidentDisplayBundle {
    result: EvalResult,
}

impl ResidentDisplayBundle {
    /// The strict `(Text, Bool, Bool)` display summary produced by the bundle.
    #[must_use]
    pub fn result(&self) -> &EvalResult {
        &self.result
    }
}

/// Why a resident-session operation was refused or failed.
#[derive(thiserror::Error, Debug)]
pub enum ResidentError {
    #[error(transparent)]
    BindingAlias(#[from] BindingAliasError),
    /// A rooted value minted by another resident session was presented to
    /// this machine. Handle ids are session-local and must never be resolved
    /// by numeric coincidence.
    #[error("root custody belongs to a different resident session")]
    ForeignCustody,
    /// A `resume`/`abort` referenced a continuation id that is not among this
    /// session's parked holes. Atomic validate-before-consume: no parked
    /// frame is touched.
    #[error("no continuation {attempted} parked{}", if .pending.is_empty() {
        " (session has no parked continuations)".to_string()
    } else {
        format!(" (parked: {})", .pending.join(", "))
    })]
    WrongContinuation {
        attempted: String,
        pending: Vec<String>,
    },
    /// The turn errored during the run (a runtime fault, a caught panic, or an
    /// ask-protocol error).
    #[error("turn run failed: {0}")]
    Run(RuntimeError),
    /// The resident eval thread could not be spawned (a transient OS
    /// resource failure — thread-limit exhaustion, out of memory). The
    /// session's machine is restored before this is returned; a retry is
    /// safe.
    #[error("failed to spawn resident eval thread: {0}")]
    EvalThread(std::io::Error),
    /// Merging this turn's constructor metadata into the session table hit a
    /// collision (a Haskell-side DataCon-scheme regression, mirroring the repl's
    /// `merge_table`).
    #[error("session DataConTable collision: {0}")]
    TableCollision(String),
    #[error(transparent)]
    ProgramProvenance(#[from] ProgramProvenanceError),
    /// A decl-plane operation failed while materializing a value bind — the
    /// cross-plane shadow retract (a value bind evicting a same-name decl head).
    #[error(transparent)]
    Session(#[from] SessionError),
    /// The prepared engine refused or failed the turn.
    #[error("prepared engine: {0}")]
    Prepared(#[from] PreparedRuntimeError),
}

impl ResidentError {
    /// Which of the two post-compile failure layers this error belongs to —
    /// see [`crate::session::workbench::WorkbenchFailureLayer`]. `None` covers an
    /// ordinary program-language fault (a pattern match failure, a case
    /// trap, a bootstrap or table-merge failure) that never reached either
    /// an effect boundary or the observation step, and any error this
    /// classification does not yet cover.
    ///
    /// The prepared engine distinguishes handler failures from observation
    /// failures. The tolerated
    /// budget-exhausted case [`is_observation_budget_exhausted`] handles
    /// separately, before a hard error like this one is ever produced).
    #[must_use]
    pub fn failure_layer(&self) -> Option<crate::session::workbench::WorkbenchFailureLayer> {
        use crate::session::workbench::WorkbenchFailureLayer;
        match self {
            Self::Run(RuntimeError::Jit(_)) => Some(WorkbenchFailureLayer::Effect),
            Self::Prepared(PreparedRuntimeError::Run(
                tidepool_codegen::prepared_program::ExecutionError::Observation(_),
            )) => Some(WorkbenchFailureLayer::Observation),
            Self::Prepared(PreparedRuntimeError::Handler { .. }) => {
                Some(WorkbenchFailureLayer::Effect)
            }
            _ => None,
        }
    }
}

/// What the prepared arm of a resident turn does with its settled value.
enum PreparedTurnMode<'a> {
    /// Observe the value and return it as the turn's result.
    Value,
    /// Observe the value and bind it into the value plane as `binder`.
    Binding {
        binder: &'a BoundBinder,
        generation: Generation,
        observation: Option<Vec<tidepool_repr::VarId>>,
    },
    /// The value is the tuple the extractor projected a pattern bind's
    /// binders into: bind its fields, in order, as `binders`.
    Projected {
        binders: &'a [BoundBinder],
        generation: Generation,
    },
}

/// The `Send` projection of one prepared run that crosses the eval thread:
/// handles are ids, the observed value is an owned tree.
pub(crate) enum PreparedRun {
    Done {
        handle: PreparedHandle,
        value: HaskellValue,
    },
    /// A projected tuple split into one retained handle per binder.
    Projected { fields: Vec<PreparedHandle> },
    /// A display bundle has three compiler-owned tuple fields: the retained
    /// page, its lazy metadata tuple, and a fresh alias identity for the page.
    /// The session binds and observes them in that order so metadata failure
    /// cannot lose the already-captured observation.
    Display {
        page: PreparedHandle,
        metadata: PreparedHandle,
        alias: PreparedHandle,
    },
    /// The turn requested a typed effect: its continuation is parked under
    /// `id` in the machine's ledger and `request` is the observed request,
    /// ready for the host to route.
    Suspended {
        id: ContinuationId,
        request: HaskellValue,
    },
}

/// The hole a suspension of a turn run in `mode` mints, carrying its
/// completion obligation forward across resumes.
fn hole_seed_of(mode: &PreparedTurnMode<'_>, lexical_scope: ScopeId) -> HoleSeed {
    match mode {
        PreparedTurnMode::Value => HoleSeed::Plain,
        PreparedTurnMode::Binding {
            binder,
            generation,
            observation,
        } => HoleSeed::Binding {
            binder: (*binder).clone(),
            generation: *generation,
            observation: observation.clone(),
            lexical_scope,
        },
        PreparedTurnMode::Projected {
            binders,
            generation,
        } => HoleSeed::ProjectedBinding {
            binders: binders.to_vec(),
            generation: *generation,
            lexical_scope,
        },
    }
}

/// The eval-thread preparation plan for a turn run in `mode`.
fn settle_plan_of(mode: &PreparedTurnMode<'_>) -> SettlePlan {
    match mode {
        PreparedTurnMode::Value => SettlePlan::Observe,
        PreparedTurnMode::Binding { binder, .. } => SettlePlan::Bind(binder.tier),
        PreparedTurnMode::Projected { binders, .. } => {
            SettlePlan::Project(binders.iter().map(|binder| binder.tier).collect())
        }
    }
}

/// What the eval thread does with a completed prepared value. Tier-0 data is deep-forced
/// (a forcing observation) before it is tenured; a Tier-1 closure is tenured
/// as-is, since a function cannot be observed without applying it.
#[derive(Clone)]
pub(crate) enum SettlePlan {
    /// Observe the value and return it as the turn's result.
    Observe,
    /// One binder at this tier.
    Bind(ValueTier),
    /// A projected tuple: one binder per field, each at its tier.
    Project(Vec<ValueTier>),
    /// Project a compiler-generated `(page, metadata, alias)` tuple.  Only
    /// the page receives the normal bind-tier forcing here; metadata remains
    /// lazy until the page has entered the value plane.
    Display(ValueTier),
}

/// Run `program`'s settled scaffold on the eval thread and finish it there:
/// a completed value is prepared under `plan`, a suspension is parked under
/// `park`. The invocation's realm cancel flag governs the run and any forcing
/// observation.
fn settle_prepared<H: DispatchEffect<O>, O>(
    engine: &mut super::prepared::PreparedEngine,
    program: ProgramId,
    realm: RealmId,
    argument: Option<PreparedHandle>,
    plan: SettlePlan,
    park: ParkPolicy,
    table: &DataConTable,
    handlers: &mut H,
    captured: &O,
) -> Result<PreparedRun, PreparedRuntimeError> {
    let settlement = match argument {
        Some(handle) => engine.run_settled_with_inputs(
            program,
            realm,
            &[tidepool_codegen::prepared_program::PreparedInput::Managed(
                handle,
            )],
        )?,
        None => engine.run_settled(program, realm)?,
    };
    finish_prepared(
        engine, program, realm, plan, park, table, handlers, captured, settlement,
    )
}

/// Apply a rooted `Int -> M a` closure to `argument` through the shared
/// `__applyEntry` scaffold root instead of a turn's own settled scaffold, and
/// finish the settled layer through [`finish_prepared`] — the prepared-route
/// arm of [`ResidentSession::run_rooted_entry_borrowed`]. `entry` is a bare
/// machine handle; BORROWED throughout (never released on any path,
/// success or failure).
fn settle_rooted_entry<H: DispatchEffect<O>, O>(
    engine: &mut super::prepared::PreparedEngine,
    entry: ValueHandle,
    argument: i64,
    realm: RealmId,
    park: ParkPolicy,
    table: &DataConTable,
    handlers: &mut H,
    captured: &O,
) -> Result<(ProgramId, PreparedRun), PreparedRuntimeError> {
    let f = engine
        .prepared_handle_of(entry)
        .ok_or(PreparedRuntimeError::UnknownHandle)?;
    let (program, settlement) = engine.run_rooted_entry(f, argument, realm)?;
    let run = finish_prepared(
        engine,
        program,
        realm,
        SettlePlan::Observe,
        park,
        table,
        handlers,
        captured,
        settlement,
    )?;
    Ok((program, run))
}

/// [`settle_rooted_entry`], but applying one rooted value to another through
/// `__applyValue` — the prepared-route arm of
/// [`ResidentSession::run_rooted_application`]. Both handles are BORROWED.
fn settle_rooted_application<H: DispatchEffect<O>, O>(
    engine: &mut super::prepared::PreparedEngine,
    function: ValueHandle,
    argument: ValueHandle,
    realm: RealmId,
    park: ParkPolicy,
    table: &DataConTable,
    handlers: &mut H,
    captured: &O,
) -> Result<(ProgramId, PreparedRun), PreparedRuntimeError> {
    let f = engine
        .prepared_handle_of(function)
        .ok_or(PreparedRuntimeError::UnknownHandle)?;
    let x = engine
        .prepared_handle_of(argument)
        .ok_or(PreparedRuntimeError::UnknownHandle)?;
    let (program, settlement) = engine.run_rooted_application(f, x, realm)?;
    let run = finish_prepared(
        engine,
        program,
        realm,
        SettlePlan::Observe,
        park,
        table,
        handlers,
        captured,
        settlement,
    )?;
    Ok((program, run))
}

/// The one completion routine for a settled layer, whichever entry produced
/// it (the initial scaffold or a resume): a completed value is prepared per
/// binder tier; a suspension is parked with the run's policy, then offered to
/// the session's handler stack. A handler that claims the request answers the parked frame through
/// the host-answer path and the resumed layer is finished here in turn; a
/// request no handler claims is reported parked.
#[allow(clippy::too_many_arguments)]
pub(crate) fn finish_prepared<H: DispatchEffect<O>, O>(
    engine: &mut super::prepared::PreparedEngine,
    mut program: ProgramId,
    mut realm: RealmId,
    plan: SettlePlan,
    park: ParkPolicy,
    table: &DataConTable,
    handlers: &mut H,
    captured: &O,
    mut settlement: PreparedSettlement,
) -> Result<PreparedRun, PreparedRuntimeError> {
    let handle = loop {
        let (request, continuation) = match settlement {
            PreparedSettlement::Done { value } => break value,
            PreparedSettlement::Suspended {
                request,
                continuation,
            } => (request, continuation),
        };
        let parked = engine.park_suspension(program, realm, park, request, continuation, table)?;
        // `SuspendAll` parks without consulting handlers; `HandleOrError`
        // never reaches here (`park_suspension` refuses it). The handler
        // stack sees the observed request, the run's principal and the
        // session's output sink.
        let response = if park.effect_policy == EffectRunPolicy::SuspendAll {
            None
        } else {
            let cx = EffectContext::with_principal(table, park.principal, captured);
            match handlers.dispatch(&parked.request, &cx) {
                Ok(response) => response,
                Err(error) => {
                    let constructor = request_constructor(&parked.request, table);
                    // The frame cannot be re-entered by anyone else: release it.
                    let _ = engine.abort_parked(parked.id);
                    return Err(PreparedRuntimeError::Handler {
                        constructor,
                        detail: error.to_string(),
                    });
                }
            }
        };
        let Some(response) = response else {
            return Ok(PreparedRun::Suspended {
                id: parked.id,
                request: parked.request,
            });
        };
        let resumed = match engine.resume_with_structural_answer(parked.id, &response, table) {
            Ok(resumed) => resumed,
            Err(error) => {
                // A refusal before the take leaves the frame parked; a
                // handled request has no other owner, so drop it here rather
                // than leak it. A failure after the take already consumed it.
                let _ = engine.abort_parked(parked.id);
                return Err(error);
            }
        };
        program = resumed.runner;
        realm = resumed.realm;
        settlement = resumed.settlement;
    };
    match plan {
        // A bare expression's value is DISPLAYED, so unlike a bind this arm
        // really does need a `HaskellValue`. But the budget that bounds
        // materialization is a display limit, and a display limit must not
        // discard a run whose effects are already committed. So an exhausted
        // budget is answered with a bounded SELECTION of the value rather than
        // a rejection: the walk is redone under
        // `BudgetPolicy::Bounded`, which stops where the budget ran out and
        // leaves `OVERSIZE_SENTINEL` at each cut. The handle is kept on that
        // path exactly as on the successful one, so the part the cut omitted
        // stays reachable through the binding the caller installs (a workbench
        // expression is named `observationN` and bound before it is shown).
        //
        // The full materializer runs FIRST and unchanged: every observation
        // the budget can afford behaves to the byte as it always did, and the
        // second, bounded walk only ever happens where the old code was about
        // to fail outright. Forcing is memoized, so redoing the walk re-reads
        // what the first one already evaluated instead of recomputing it.
        SettlePlan::Observe => match engine.observe(program, handle) {
            Ok(value) => Ok(PreparedRun::Done { handle, value }),
            Err(error) if is_observation_budget_exhausted(&error) => {
                match engine.observe_bounded(program, handle) {
                    Ok(value) => Ok(PreparedRun::Done { handle, value }),
                    Err(error) => {
                        engine.release(handle);
                        Err(error)
                    }
                }
            }
            Err(error) => {
                engine.release(handle);
                Err(error)
            }
        },
        // A bind's value is the RETAINED HANDLE, which
        // `run_entry_retained` produced without consulting any budget, and
        // whose receipt renders binder names rather than the value
        // (`WorkbenchDisplay::Binding`). Observation here is a forcing step
        // whose materialized product this arm hands on but the binding does
        // not need. So an exhausted observation budget is not a failure of
        // anything: the program ran, its effects are committed, and the
        // binding is sound. Rejecting the unit would discard both and demand
        // that every committed effect be replayed to get the value back —
        // a live session lost 45 committed operations (10 Jev calls, 35
        // command jobs) that way, and `reflect 3` cannot bind at all, since
        // three turns of conversation exceed 100_000 bytes on their own.
        // Keep the handle and report the size, exactly as the closure tier
        // below keeps a value that has no `HaskellValue` representation.
        SettlePlan::Bind(ValueTier::ForceData) => match engine.observe(program, handle) {
            Ok(value) => Ok(PreparedRun::Done { handle, value }),
            Err(error) if is_observation_budget_exhausted(&error) => Ok(PreparedRun::Done {
                handle,
                value: HaskellValue::Con(
                    tidepool_codegen::observation::OVERSIZE_SENTINEL,
                    Vec::new(),
                ),
            }),
            Err(error) => {
                engine.release(handle);
                Err(error)
            }
        },
        SettlePlan::Bind(ValueTier::RetainOpaque) => Ok(PreparedRun::Done {
            handle,
            value: HaskellValue::Con(tidepool_codegen::observation::CLOSURE_SENTINEL, Vec::new()),
        }),
        SettlePlan::Project(tiers) => {
            let fields = engine.fields(handle, realm, tiers.len());
            engine.release(handle);
            let fields = fields?;
            for (index, (field, tier)) in fields.iter().zip(&tiers).enumerate() {
                if *tier != ValueTier::ForceData {
                    continue;
                }
                // This lane discards the observed value outright — it forces
                // and checks for an error. An exhausted budget is tolerated
                // for the same reason as the whole-value bind above: each
                // field stays retained and is a sound binding.
                match engine.observe(program, *field) {
                    Ok(_) => {}
                    Err(error) if is_observation_budget_exhausted(&error) => {}
                    Err(error) => {
                        engine.release(*field);
                        // The failing field is released; release the rest.
                        engine.release_all(
                            fields
                                .iter()
                                .enumerate()
                                .filter(|(other, _)| *other != index)
                                .map(|(_, field)| *field),
                        );
                        return Err(error);
                    }
                }
            }
            Ok(PreparedRun::Projected { fields })
        }
        SettlePlan::Display(page_tier) => {
            let fields = engine.fields(handle, realm, 3);
            engine.release(handle);
            let mut fields = fields?;
            let Some(alias) = fields.pop() else {
                return Err(PreparedRuntimeError::ProjectionShape {
                    binders: 3,
                    fields: 0,
                });
            };
            let Some(metadata) = fields.pop() else {
                engine.release(alias);
                return Err(PreparedRuntimeError::ProjectionShape {
                    binders: 3,
                    fields: 1,
                });
            };
            let Some(page) = fields.pop() else {
                engine.release_all([metadata, alias]);
                return Err(PreparedRuntimeError::ProjectionShape {
                    binders: 3,
                    fields: 2,
                });
            };
            if !fields.is_empty() {
                engine.release_all(fields.into_iter().chain([page, metadata, alias]));
                return Err(PreparedRuntimeError::ProjectionShape {
                    binders: 3,
                    fields: 4,
                });
            }
            if page_tier == ValueTier::ForceData {
                let forced = match engine.observe(program, page) {
                    Ok(value) => Ok(value),
                    Err(error) if is_observation_budget_exhausted(&error) => {
                        engine.observe_bounded(program, page)
                    }
                    Err(error) => Err(error),
                };
                if let Err(error) = forced {
                    engine.release_all([page, metadata, alias]);
                    return Err(error);
                }
            }
            Ok(PreparedRun::Display {
                page,
                metadata,
                alias,
            })
        }
    }
}

/// Whether `error` is only the observation budget running out while
/// materializing a value for display.
///
/// This is the one observation failure that says nothing about the program,
/// the heap, or the value: the traversal simply stopped. Every other variant
/// — an unauthenticated address, a descriptor integrity error, an
/// unobservable object kind — reports that something is actually wrong, and
/// must still fail the unit.
fn is_observation_budget_exhausted(error: &PreparedRuntimeError) -> bool {
    matches!(
        error,
        PreparedRuntimeError::Run(
            tidepool_codegen::prepared_program::ExecutionError::Observation(
                tidepool_codegen::prepared_program::ObservationFailure::BudgetExceeded { .. }
            )
        )
    )
}

/// A resident JIT session: one long-lived [`PreparedEngine`] whose heap and
/// effect-plane state persist across turns.
///
/// Generic over the effect handler stack `H` and the output sink `O` so it
/// stays below the server crate that owns the concrete buffer, exactly like
/// [`super::PreparedEngine`]. The registry (`tidepool-harness`) instantiates
/// `Slot<ResidentSession<H, O>>`.
pub struct ResidentSession<H, O> {
    /// The shared persistent session state (machine + accumulated table + the two
    /// planes). The harness does not (yet) accumulate on the decl/value planes —
    /// they sit empty here until enabled — but the machine lifecycle + table
    /// merge + fragment-run primitives all live in the session state, shared with the
    /// repl's resident session.
    state: PersistentSession,
    /// The effect handler stack, borrowed by each turn's eval thread.
    handlers: H,
    /// The console-output buffer turns write into.
    captured: O,
    /// Monotonic continuation-id counter (prefix `scont` for the resident
    /// surface).
    cont_id_issuer: MonotonicIdIssuer,
    /// The parked holes, insertion-ordered: `(hole string, machine
    /// ContinuationId)` per live parked frame. The machine's continuation
    /// registry is the ground truth; these are the string identities callers
    /// resume/abort against (atomic validate-before-consume). Top = last.
    parked: Vec<(String, ContinuationId)>,
    parked_provenance: HashMap<ContinuationId, Arc<ProgramProvenance>>,
    binding_provenance: HashMap<u64, Arc<ProgramProvenance>>,
    /// Host-owned text identities for materialized bindings whose equality is
    /// meaningful to a caller (currently retained command jobs). The binding
    /// table remains the owner of reachability and scope retirement; this map
    /// only records a payload identity for deduplication.
    host_text_bindings: HashMap<SessionVarId, String>,
    /// Private request carriers remain rooted for closures that captured them,
    /// but never become ordinary unqualified workbench vocabulary.
    hidden_host_bindings: HashMap<SessionVarId, ()>,
    /// The resource and lexical scopes for the next session entry. Callers
    /// sharing a machine replace this atomically at checkout boundaries.
    run_context: SessionRunContext,
    /// Deferred releases produced when affine custody is dropped away from a
    /// machine checkout. The next mutable session entry settles them.
    custody_cleanup: Arc<CustodyCleanup>,
}

impl<H, O> ResidentSession<H, O>
where
    H: DispatchEffect<O> + Send,
    // `Sync` so the per-turn eval thread can borrow the shared sink (every
    // real sink is `Arc`-backed and already `Sync`; the `OutputSink` trait
    // itself only requires `Clone + Send`).
    O: OutputSink + Sync,
{
    /// Build a resident session with no live machine yet. Construction cannot
    /// fail or compile a seed program. The machine comes up on the first real
    /// turn ([`Self::run_with_sites`]/[`Self::run_bind_with_sites`]/
    /// [`Self::run_child`]/[`Self::run_child_pure`]) when that turn's prepared
    /// program is installed.
    #[allow(clippy::too_many_arguments)]
    pub fn unbootstrapped(
        handlers: H,
        captured: O,
        nursery_size: usize,
        lib: Option<SessionLib>,
    ) -> Self {
        let state = PersistentSession::new(lib, nursery_size);
        ResidentSession {
            state,
            handlers,
            captured,
            cont_id_issuer: MonotonicIdIssuer::new("scont"),
            parked: Vec::new(),
            parked_provenance: HashMap::new(),
            binding_provenance: HashMap::new(),
            host_text_bindings: HashMap::new(),
            hidden_host_bindings: HashMap::new(),
            run_context: SessionRunContext::ROOT,
            custody_cleanup: Arc::new(CustodyCleanup::default()),
        }
    }

    /// Accumulate `decls` on the decl plane (mirrors the repl's
    /// `Session::define_scoped`): a declaration turn appends to the gen-versioned
    /// `Lib.G<g>` module a later turn imports. Requires a decl plane (`Some(lib)`
    /// at bootstrap). Each node's plane is independent, so a parent's accumulated
    /// declarations survive across a child run on a different node.
    pub fn define_scoped(
        &mut self,
        decls: &[&str],
    ) -> Result<tidepool_repr::Generation, SessionError> {
        self.state.define_scoped(decls)
    }

    /// Scoped [`Self::define_scoped`]: append to `scope`'s own decl tip, which
    /// already re-exports its ancestors' — so the definition is visible to
    /// `scope` and its descendants and to nobody else. `define_scoped(d) ==
    /// define_scoped_in(ScopeId::ROOT, d)`.
    pub fn define_scoped_in(
        &mut self,
        scope: ScopeId,
        decls: &[&str],
    ) -> Result<tidepool_repr::Generation, SessionError> {
        self.state.define_scoped_in(scope, decls)
    }

    /// Commit declarations against frontend-owned imports without recording
    /// those trusted imports as user-authored workbench state.
    pub fn define_scoped_with_imports_in(
        &mut self,
        scope: ScopeId,
        decls: &[&str],
        imports: &SourceImports,
    ) -> Result<tidepool_repr::Generation, SessionError> {
        self.state
            .define_scoped_with_imports_in(scope, decls, imports)
    }

    pub fn stage_declarations_in(
        &self,
        scope: ScopeId,
        receipt: &super::DeclarationReceipt,
        imports: &super::SourceImports,
    ) -> Result<super::StagedDeclaration, SessionError> {
        self.state.stage_declarations_in(scope, receipt, imports)
    }

    pub fn commit_declaration_receipt_in(
        &mut self,
        scope: ScopeId,
        receipt: &super::DeclarationReceipt,
        imports: &SourceImports,
    ) -> Result<super::DeclarationPlaneCommit, SessionError> {
        self.state
            .commit_declaration_receipt_in(scope, receipt, imports)
    }

    pub fn adopt_staged_declaration_in(
        &mut self,
        staged: super::StagedDeclaration,
    ) -> Result<super::DeclarationPlaneCommit, SessionError> {
        self.state.adopt_staged_declaration_in(staged)
    }

    pub fn discard_staged_declaration(&self, staged: &super::StagedDeclaration) {
        self.state.discard_staged_declaration(staged);
    }

    /// The current decl-plane module name (`Tidepool.Session.Lib.G<g>`) a later
    /// turn imports to see accumulated declarations, or `None` before any decl.
    pub fn session_import_module(&self) -> Option<String> {
        self.state.current_lib_module().map(|m| m.module_name())
    }

    /// Scoped [`Self::session_import_module`]: the `Lib.G<g>` module at
    /// `scope`'s tip. A turn compiled in a child scope imports THIS, not
    /// ROOT's — which is the whole of "parent declarations callable in every
    /// child" on the real compile path, since the child's tip module re-exports
    /// its parent's chain.
    pub fn session_import_module_in(&self, scope: ScopeId) -> Option<String> {
        self.state
            .current_lib_module_in(scope)
            .map(|m| m.module_name())
    }

    #[must_use]
    pub fn next_declaration_module(&self) -> Option<tidepool_repr::SessionModule> {
        self.state.next_lib_module()
    }

    /// The decl-plane include directory to add to a later turn's compile search
    /// path (so `import Lib.G<g>` resolves), or `None` with no decl plane.
    pub fn lib_include_dir(&self) -> Option<PathBuf> {
        self.state.lib_include_dir().map(Path::to_path_buf)
    }

    /// The current value-binding generation. The caller mints the NEXT one
    /// (`val_gen().next()`) BEFORE compiling a bind turn — the extract stamps that
    /// generation into `Val.G<g>`, and [`Self::run_bind`]/[`Self::resume`]
    /// (via a [`ResidentHole::Binding`]) materialize at the same `g`.
    pub fn val_gen(&self) -> Generation {
        self.state.val_gen()
    }

    /// Retain prepared closure dependencies through the existing binding owner.
    /// Reserved future bindings can be leased before they materialize.
    pub fn lease_bindings(&mut self, referenced: &[tidepool_repr::VarId]) -> BindingLease {
        self.settle_dropped_custody();
        let retained = self
            .state
            .bindings_mut()
            .acquire_leases(referenced.iter().copied().map(SessionVarId::from_var))
            .into_iter()
            .collect();
        BindingLease {
            retained,
            cleanup: Arc::clone(&self.custody_cleanup),
        }
    }

    /// The materialized bindings visible while a compiled cell waits to run.
    /// A later item in that same cell may still import one of these identities
    /// after an earlier item shadows its public name, so preparation retains
    /// this exact source environment through the cell's execution prefix.
    #[must_use]
    pub fn visible_binding_ids_in(&self, scope: ScopeId) -> Vec<tidepool_repr::VarId> {
        self.state
            .bindings()
            .iter_current_in(self.state.scope_tree(), scope)
            .into_iter()
            .map(|(_, entry)| entry.id.var())
            .collect()
    }

    /// Publish a GHC-typed alias of an already captured value in this actor's
    /// lexical scope. The compiled alias interface supplies the new name,
    /// identity, module and type; this path shares the source's registered
    /// root slot and never evaluates or roots the value again. The source and
    /// its dependencies remain leased until the caller drops `lease`, after
    /// which the binding table's alias dependency tracks the current name.
    pub fn publish_captured_alias_in(
        &mut self,
        scope: ScopeId,
        source: SessionVarId,
        alias: &BoundBinder,
        generation: Generation,
        lease: &BindingLease,
    ) -> Result<super::ValuePlaneCommit, ResidentError> {
        self.settle_dropped_custody();
        if !self.state.scope_tree().is_live(scope) {
            return Err(SessionError::DeadScope(scope).into());
        }
        if !Arc::ptr_eq(&lease.cleanup, &self.custody_cleanup) {
            return Err(BindingAliasError::ForeignLease.into());
        }
        if !lease.retained.contains(&source) {
            return Err(BindingAliasError::SourceNotLeased(source).into());
        }
        let source_entry = self
            .state
            .bindings()
            .get(source)
            .ok_or(BindingAliasError::MissingSource(source))?;
        if source_entry.scope != scope {
            return Err(BindingAliasError::WrongScope {
                binding: source,
                actual: source_entry.scope,
                expected: scope,
            }
            .into());
        }
        let mut value = source_entry.value.clone();
        // A prepared alias shares the source's root and handle, but a later
        // turn imports it by ITS OWN thin value module and name, so the
        // recorded identity is re-minted for the alias.
        let BoundValue { identity, .. } = &mut value;
        identity.module = alias.module.clone();
        identity.occurrence = alias.name.clone();
        let id = SessionVarId::from_extract(alias.var_id);
        if self.state.bindings().get(id).is_some() {
            return Err(BindingAliasError::IdentityInUse(id).into());
        }
        let module = SessionModule::val(generation);
        if alias.module != module.module_name() {
            return Err(BindingAliasError::WrongModule.into());
        }
        if self
            .state
            .resolve_in(scope, &alias.name)
            .is_some_and(|entry| entry.module.gen().0 >= generation.0)
        {
            return Err(BindingAliasError::StaleGeneration.into());
        }
        let provenance = self.binding_provenance.get(&source.raw()).cloned();
        let committed = self.state.publish_alias_in(
            scope,
            BindingEntry {
                name: BindingName(alias.name.clone()),
                id,
                module,
                value,
                type_display: Some(alias.type_display.clone()),
                defining_expr: None,
                scope,
            },
            source,
        )?;
        self.state.set_val_gen(generation);
        if let Some(provenance) = provenance {
            self.binding_provenance.insert(id.raw(), provenance);
        }
        self.binding_provenance.retain(|id, _| {
            self.state
                .bindings()
                .get(SessionVarId::from_extract(*id))
                .is_some()
        });
        Ok(committed)
    }

    /// Reserve identities for compiled cell values before releasing exclusive
    /// session access. Aborted cells leave gaps; reserved identities are never reused.
    pub fn reserve_value_generations_through(&mut self, generation: Generation) {
        self.state.set_val_gen(generation);
    }

    /// The live `Val.G<g>` module names to inject (`--inject-val`) so a turn can
    /// reference earlier value bindings — ALL live gens (incl. shadowed).
    pub fn inject_val_modules(&self) -> Vec<String> {
        self.state.live_val_modules()
    }

    /// The CURRENT `Val.G<g>` module per still-live name — what a turn IMPORTS
    /// (unqualified) so the reference typechecks. Excludes shadowed older gens
    /// (those are injected but not imported, to avoid an ambiguous occurrence).
    pub fn current_val_modules(&self) -> Vec<String> {
        self.state.current_val_modules()
    }

    /// Scoped [`Self::current_val_modules`]: the `Val.G<g>` module per name
    /// VISIBLE at `scope` — its own frame first, then each ancestor's, nearest
    /// frame winning. A sibling scope's bindings are never in this list, so a
    /// turn compiled here cannot even name them.
    pub fn current_val_modules_in(&self, scope: ScopeId) -> Vec<String> {
        self.state.current_val_modules_in(scope)
    }

    /// Immutable compile environment for `scope`, suitable for carrying out of
    /// a registry peek before a blocking GHC invocation.
    pub fn compile_view_in(&self, scope: ScopeId) -> Option<super::SessionCompileView> {
        let hidden = self
            .state
            .bindings()
            .iter_current_in(self.state.scope_tree(), scope)
            .into_iter()
            .filter(|(_, entry)| self.hidden_host_bindings.contains_key(&entry.id))
            .map(|(name, _)| name.0.clone())
            .collect::<Vec<_>>();
        self.state
            .compile_view_in(scope)
            .map(|view| view.hide_value_names(&hidden))
    }

    /// Capture an exact, selective declaration surface from `scope` for a
    /// fresh actor's model-visible environment.
    pub fn exact_exports_in(
        &self,
        scope: ScopeId,
        heads: &[&str],
    ) -> Result<super::ExactExportSurface, super::ExactExportError> {
        self.state.exact_exports_in(scope, heads)
    }

    /// Exact declaration-head incarnations visible from `scope`.
    ///
    /// Actor sealing pairs this with compiler-produced nominal heads so a
    /// same-spelled declaration introduced after a live program was compiled
    /// cannot replace the program's original type.
    #[must_use]
    pub fn current_decl_heads_in(&self, scope: ScopeId) -> Vec<(String, u64)> {
        self.state.lib().current_decl_heads_in(scope)
    }

    /// The most recently parked hole (top of the stack), if any.
    ///
    /// Use [`Self::parked_holes`] when the caller needs the complete registry.
    pub fn pending_continuation(&self) -> Option<&str> {
        self.parked.last().map(|(h, _)| h.as_str())
    }

    /// Every parked hole, insertion-ordered (oldest first).
    pub fn parked_holes(&self) -> Vec<&str> {
        self.parked.iter().map(|(h, _)| h.as_str()).collect()
    }

    /// Runtime resource scope owning one parked continuation. Lifecycle
    /// interpreters use this to distinguish installed-program suspensions from
    /// disposable workbench fragments without trusting request payload data.
    #[must_use]
    pub fn parked_realm(&self, hole: &ResidentHole) -> Option<RealmId> {
        let &(_, id) = self
            .parked
            .iter()
            .find(|(name, _)| name == hole.cont_id())?;
        self.state.parked_realm(id)
    }

    #[must_use]
    pub fn parked_program_provenance(&self, hole: &ResidentHole) -> Option<Arc<ProgramProvenance>> {
        let (_, id) = self
            .parked
            .iter()
            .find(|(name, _)| name == hole.cont_id())?;
        self.parked_provenance.get(id).cloned()
    }

    /// Whether the session has no parked frames (ready and quiescent).
    pub fn is_idle(&self) -> bool {
        self.parked.is_empty()
    }

    /// Select the resource ownership and lexical environment for subsequent
    /// work on this checkout.
    ///
    /// Validation happens before assignment, so a dead lexical scope leaves
    /// both halves of the previous context unchanged.
    pub fn set_run_context(&mut self, context: SessionRunContext) -> Result<(), ResidentError> {
        if !self.state.scope_tree().is_live(context.lexical_scope) {
            return Err(SessionError::DeadScope(context.lexical_scope).into());
        }
        self.run_context = context;
        Ok(())
    }

    /// Select request routing and live-value crossing without changing the
    /// current execution principal or resource scopes.
    pub fn set_effect_execution(
        &mut self,
        effect_policy: EffectRunPolicy,
        live_payload: LivePayloadPolicy,
    ) {
        self.state.set_effect_execution(effect_policy, live_payload);
    }

    /// Atomically select one actor's authority/scopes and request policy.
    /// Validation precedes both assignments, so a dead lexical scope
    /// cannot leave half of another actor's execution contract installed.
    pub fn set_actor_execution(
        &mut self,
        context: SessionRunContext,
        effect_policy: EffectRunPolicy,
        live_payload: LivePayloadPolicy,
    ) -> Result<(), ResidentError> {
        if !self.state.scope_tree().is_live(context.lexical_scope) {
            return Err(SessionError::DeadScope(context.lexical_scope).into());
        }
        self.run_context = context;
        self.set_effect_execution(effect_policy, live_payload);
        Ok(())
    }

    /// The context currently selected for resident-session entries.
    #[must_use]
    pub fn run_context(&self) -> SessionRunContext {
        self.run_context
    }

    /// Request policy currently selected for resident entries.
    #[must_use]
    pub fn effect_policy(&self) -> EffectRunPolicy {
        self.state.effect_policy()
    }

    /// Live-value crossing policy currently installed with the effect stack.
    #[must_use]
    pub fn live_payload_policy(&self) -> LivePayloadPolicy {
        self.state.live_payload_policy()
    }

    /// Scope exit for `realm`: close the realm
    /// on the machine (frames dropped, roots deregistered, handles released)
    /// and RECONCILE this session's parked-hole list against the machine's
    /// surviving frame ids — the machine is the ground truth, so holes whose
    /// frames the close dropped disappear here too, and sibling realms'
    /// holes are untouched. Returns `(frames_dropped, handles_released)`;
    /// `(0, 0)` when the machine is not yet booted or the realm owns
    /// nothing (idempotent).
    pub fn close_realm(&mut self, realm: RealmId) -> (usize, usize) {
        self.settle_dropped_custody();
        let counts = self.state.close_realm(realm);
        let survivors = self.state.parked_ids();
        self.parked.retain(|(_, id)| survivors.contains(id));
        self.parked_provenance
            .retain(|id, _| survivors.contains(id));
        counts
    }

    /// Prepared-machine residency counters, or `None` before bootstrap.
    #[must_use]
    pub fn residency(&self) -> Option<tidepool_codegen::prepared_program::ResidencyCounts> {
        self.state.residency()
    }

    /// Lifetime `(functions, code_bytes)` of Cranelift work this session's
    /// prepared installs have caused; `None` before bootstrap. Diff across a turn to attribute that
    /// turn's code generation.
    #[must_use]
    pub fn codegen_totals(&self) -> Option<(u64, u64)> {
        self.state.codegen_totals()
    }

    /// How many package tops this session's machine can hand a later turn
    /// instead of recompiling; `None` before bootstrap.
    #[must_use]
    pub fn code_export_count(&self) -> Option<usize> {
        self.state.code_export_count()
    }

    /// Prepared old-space bytes as of the last successful between-turn
    /// collection, or `None` before bootstrap.
    #[must_use]
    pub fn old_bytes(&self) -> Option<usize> {
        self.state.old_bytes()
    }

    /// Mint a [`ValueHandle`] over the declared live payload of the frame
    /// parked on `hole` (the payload never bridges to a
    /// data `HaskellValue`; the `Send` handle is how it is passed around and
    /// eventually DELIVERED into a sibling hole via [`Self::resume_handle`]).
    /// The frame stays parked; the handle is owned by the frame's realm.
    /// `None` when `hole` is not parked or its frame holds no untaken live
    /// payload.
    pub fn live_payload_handle(
        &mut self,
        hole: &str,
    ) -> Result<Option<RootCustody>, ResidentError> {
        self.settle_dropped_custody();
        let Some(&(_, id)) = self.parked.iter().find(|(h, _)| h == hole) else {
            return Ok(None);
        };
        let provenance = self.parked_provenance.get(&id).cloned().unwrap_or_default();
        let handle = match self.state.prepared_mut() {
            Some(engine) => engine.live_payload_handle(id)?,
            None => return Ok(None),
        };
        Ok(handle
            .map(|handle| RootCustody::new(handle, Arc::clone(&self.custody_cleanup), provenance)))
    }

    /// [`Self::live_payload_handle`]'s sibling for a result that must outlive
    /// the frame's OWN realm: mint the handle owned by `realm` instead (a
    /// green thread's `AsyncDoneWith` payload, owned by the SESSION's realm
    /// so a waiter's handle survives the thread's own realm later closing —
    /// see [`ResidentSession::run_rooted_entry`]). Same
    /// frame-stays-parked semantics; `None` under the same conditions.
    /// Returns a [`RootCustody`] token, exactly as [`Self::live_payload_handle`]
    /// does: minting under a different realm changes WHO owns the root, never
    /// whether the handle needs consuming exactly once.
    pub fn live_payload_handle_owned_by(
        &mut self,
        hole: &str,
        realm: RealmId,
    ) -> Result<Option<RootCustody>, ResidentError> {
        self.settle_dropped_custody();
        let Some(&(_, id)) = self.parked.iter().find(|(h, _)| h == hole) else {
            return Ok(None);
        };
        let provenance = self.parked_provenance.get(&id).cloned().unwrap_or_default();
        let handle = match self.state.prepared_mut() {
            Some(engine) => match engine.live_payload_handle_owned_by(id, realm)? {
                Some(handle) => handle,
                None => return Ok(None),
            },
            None => return Ok(None),
        };
        tracing::debug!(
            hole,
            frame = ?id,
            owner = ?realm,
            ?handle,
            "claimed parked live payload"
        );
        Ok(Some(RootCustody::new(
            handle,
            Arc::clone(&self.custody_cleanup),
            provenance,
        )))
    }

    /// Transfer a rooted value to another runtime resource scope.
    ///
    /// This is the ownership operation used when a live value outlives the
    /// scope that produced it, such as a queued message or detached child.
    /// The value and handle identity are unchanged; only the scope responsible
    /// for eventual cleanup changes.
    pub fn rehome_custody(
        &mut self,
        custody: RootCustody,
        owner: RealmId,
    ) -> Result<RootCustody, ResidentError> {
        self.settle_dropped_custody();
        let transfer = custody.into_transfer();
        let handle = transfer.handle;
        let moved = self
            .state
            .prepared_mut()
            .is_some_and(|engine| engine.rehome_handle(handle, owner));
        if !moved {
            return Err(ResidentError::Run(RuntimeError::Jit(EffectError::Handler(
                format!("cannot transfer {handle:?}: handle is not live on this machine"),
            ))));
        }
        Ok(transfer.into_custody())
    }

    /// Abandon a rooted value deliberately, releasing its root immediately.
    pub fn discard_custody(&mut self, custody: RootCustody) -> bool {
        self.settle_dropped_custody();
        let transfer = custody.into_transfer();
        let discarded = self
            .state
            .prepared_mut()
            .is_some_and(|engine| engine.discard_handle(transfer.handle));
        if discarded {
            transfer.commit();
        }
        discarded
    }

    /// Resume the turn represented by `hole` by DELIVERING a machine-side
    /// rooted value — the handle's payload feeds the continuation verbatim,
    /// no materialization, closures included. The authored loop receives
    /// closure-valued state transitions this way. Same
    /// validate-before-consume and ground-truth reconciliation as
    /// [`Self::resume`]. The typed hole carries the same binding-completion
    /// obligation as the ordinary value path; handle delivery cannot silently
    /// turn a suspended bind into a plain fragment.
    ///
    /// Takes the [`RootCustody`] token by value — this IS the consuming half
    /// of the custody crossing (see that type's doc): the delivery itself
    /// does not release the handle from the machine's own registry (a resume
    /// is a scope-owned BORROW at the machine layer, same as `observe_handle`),
    /// so without the token nothing at this layer stops a caller from also
    /// mounting the same raw handle. The token is unwrapped once, here, at
    /// the moment its custody is spent.
    pub fn resume_handle(
        &mut self,
        hole: ResidentHole,
        custody: RootCustody,
    ) -> Result<ResidentOutcome, ResidentError> {
        self.settle_dropped_custody();
        let seed = hole.seed();
        let cont_id = match hole {
            ResidentHole::Plain(hole) => hole.id,
            ResidentHole::Binding(hole) => hole.id,
            ResidentHole::ProjectedBinding(hole) => hole.id,
        };
        let transfer = custody.into_transfer();
        tracing::debug!(
            continuation = %cont_id,
            handle = ?transfer.handle,
            obligation = match &seed {
                HoleSeed::Plain => "plain",
                HoleSeed::Binding { .. } => "binding",
                HoleSeed::ProjectedBinding { .. } => "projected-binding",
            },
            actor_scope = ?self.run_context.lexical_scope,
            actor_realm = ?self.run_context.resource_scope,
            "resuming resident continuation with rooted value"
        );
        let provenance = Arc::clone(&transfer.provenance);
        let result = self.reenter(
            &cont_id,
            ResidentResumeInput::Handle(transfer.handle),
            seed,
            Some(&provenance),
        );
        if result.is_ok() {
            transfer.commit();
        }
        result
    }

    /// Scoped read-only query: the binding `name` resolves to as seen FROM
    /// `scope` — its own frame first, then each ancestor up to its lexical
    /// root, so a child reads a parent's mounts and a local mount shadows an
    /// inherited one. A plain existence/identity probe for callers that need
    /// to know what `name` is bound to without mounting anything.
    pub fn current_binding_in(
        &self,
        scope: ScopeId,
        name: &str,
    ) -> Option<(SessionVarId, SessionModule, ValueTier, Option<String>)> {
        let entry = self.state.resolve_in(scope, name)?;
        let tier = ValueTier::RetainOpaque;
        Some((entry.id, entry.module, tier, entry.type_display.clone()))
    }

    /// Install a rooted live value under a binder GHC has already compiled,
    /// without evaluating a throwaway placeholder of that type.
    ///
    /// `run_turn` writes the binder's thin `Val.G<gen>` interface and returns
    /// its exact identity. This operation joins that type-plane identity to a
    /// same-typed in-heap value supplied under affine custody. It is the mount
    /// path for actor inputs and messages: the authoritative value already
    /// exists, so running `undefined`, a guessed inhabitant, or a second copy
    /// merely to create the binding would be both wasteful and semantically
    /// wrong.
    ///
    /// Validation and table merge happen before custody is consumed. On
    /// success ownership transfers from the handle registry to the scoped
    /// value plane exactly once.
    pub fn mount_compiled_binding_in(
        &mut self,
        scope: ScopeId,
        binder: &BoundBinder,
        gen: Generation,
        table: &DataConTable,
        custody: RootCustody,
    ) -> Result<(), ResidentError> {
        self.settle_dropped_custody();
        if !self.state.scope_tree().is_live(scope) {
            self.discard_custody(custody);
            return Err(SessionError::DeadScope(scope).into());
        }
        let expected_module = SessionModule::val(gen).module_name();
        if binder.module != expected_module {
            self.discard_custody(custody);
            return Err(ResidentError::Run(RuntimeError::Jit(EffectError::Handler(
                format!(
                    "compiled binder `{}` belongs to {}, expected {expected_module}",
                    binder.name, binder.module
                ),
            ))));
        }
        self.state
            .merge_table(table)
            .map_err(ResidentError::TableCollision)?;
        self.mount_compiled_binding_prepared(scope, binder, gen, custody)
    }

    /// Build and mount a compiler-typed JSON value without putting the
    /// payload into generated Haskell source. `binder` is the one binder the
    /// compiler produced for the payload-independent `Aeson.Value` interface;
    /// its `Val.G<gen>` module remains the sole type authority.
    pub fn mount_json_binding_in(
        &mut self,
        scope: ScopeId,
        binder: &BoundBinder,
        gen: Generation,
        code: TurnCode<'_>,
        value: &serde_json::Value,
    ) -> Result<(), ResidentError> {
        self.settle_dropped_custody();
        self.validate_compiled_mount_target(
            scope,
            binder,
            gen,
            &code,
            HostBindingType::JSON_VALUE,
        )?;
        self.mount_host_value_in(scope, binder, gen, code, |engine, realm, table| {
            engine.build_host_json(realm, value, table)
        })
    }

    /// The `Text` sibling of [`Self::mount_json_binding_in`]. It is for host
    /// strings whose compiler-produced binder has type `Text`; the UTF-8
    /// bytes stream directly into the same managed builder and never become a
    /// Haskell source literal.
    pub fn mount_text_binding_in(
        &mut self,
        scope: ScopeId,
        binder: &BoundBinder,
        gen: Generation,
        code: TurnCode<'_>,
        text: &str,
    ) -> Result<(), ResidentError> {
        self.settle_dropped_custody();
        self.validate_compiled_mount_target(scope, binder, gen, &code, HostBindingType::TEXT)?;
        self.validate_text_runtime_constructor(&code)?;
        self.mount_host_value_in(scope, binder, gen, code, |engine, realm, table| {
            engine.build_host_text(realm, text, table)
        })
    }

    /// Build and mount a compiler-typed structural host value. The caller's
    /// compiler-issued binder and constructor table are the type authority;
    /// the host value is never rendered as Haskell source.
    pub fn mount_typed_binding_in<T>(
        &mut self,
        scope: ScopeId,
        binder: &BoundBinder,
        gen: Generation,
        code: TurnCode<'_>,
        expected: HostBindingType,
        value: &T,
    ) -> Result<(), ResidentError>
    where
        T: tidepool_bridge::ToHaskell,
    {
        self.settle_dropped_custody();
        self.validate_compiled_mount_target(scope, binder, gen, &code, expected)?;
        self.mount_host_value_in(scope, binder, gen, code, |engine, realm, table| {
            engine.build_host_value(realm, value, table)
        })
    }

    /// Record a host `Text` identity after its freshly minted compiler binder
    /// has been mounted. This supports deduplication without comparing source
    /// text. The identity disappears when its value binding leaves the live
    /// table.
    pub fn tag_host_text_binding_in(
        &mut self,
        scope: ScopeId,
        binder: &BoundBinder,
        text: String,
    ) -> Result<(), ResidentError> {
        let id = SessionVarId::from_extract(binder.var_id);
        let current = self
            .state
            .resolve_in(scope, &binder.name)
            .filter(|entry| entry.scope == scope && entry.id == id)
            .ok_or_else(|| {
                ResidentError::Run(RuntimeError::Jit(EffectError::Handler(format!(
                    "host text binding `{}` is not current in its lexical scope",
                    binder.name
                ))))
            })?;
        if current.id != id {
            return Err(ResidentError::Run(RuntimeError::Jit(EffectError::Handler(
                "host text binding identity changed before tagging".into(),
            ))));
        }
        self.host_text_bindings.insert(id, text);
        Ok(())
    }

    /// Make a freshly mounted request carrier private to the source preamble
    /// that aliases it. It remains injected and rooted for closures compiled
    /// during that request, but ordinary future cells cannot name it.
    pub fn hide_host_binding_in(
        &mut self,
        scope: ScopeId,
        binder: &BoundBinder,
    ) -> Result<(), ResidentError> {
        let id = SessionVarId::from_extract(binder.var_id);
        let current = self
            .state
            .resolve_in(scope, &binder.name)
            .filter(|entry| entry.scope == scope && entry.id == id)
            .ok_or_else(|| {
                ResidentError::Run(RuntimeError::Jit(EffectError::Handler(format!(
                    "host binding `{}` is not current in its lexical scope",
                    binder.name
                ))))
            })?;
        if current.id != id {
            return Err(ResidentError::Run(RuntimeError::Jit(EffectError::Handler(
                "host binding identity changed before hiding".into(),
            ))));
        }
        self.hidden_host_bindings.insert(id, ());
        Ok(())
    }

    /// Withdraw a private request carrier from future source views while any
    /// prepared work that already leased it retains its exact generation.
    pub fn retire_host_binding_owner(&mut self, binder: &BoundBinder) {
        let id = SessionVarId::from_extract(binder.var_id);
        self.state.retire_binding_owner(id);
        self.hidden_host_bindings.remove(&id);
    }

    /// The current materialized binding in `scope` carrying this exact host
    /// text identity. This compares retained host data, never rendered source.
    #[must_use]
    pub fn host_text_binding_in(&self, scope: ScopeId, text: &str) -> Option<String> {
        self.state
            .bindings()
            .iter_current_in(self.state.scope_tree(), scope)
            .into_iter()
            .find_map(|(name, entry)| {
                (entry.scope == scope
                    && self
                        .host_text_bindings
                        .get(&entry.id)
                        .is_some_and(|identity| identity == text))
                .then(|| name.0.clone())
            })
    }

    fn validate_compiled_mount_target(
        &self,
        scope: ScopeId,
        binder: &BoundBinder,
        gen: Generation,
        code: &TurnCode<'_>,
        expected: HostBindingType,
    ) -> Result<(), ResidentError> {
        if !self.state.scope_tree().is_live(scope) {
            return Err(SessionError::DeadScope(scope).into());
        }
        let expected_module = SessionModule::val(gen).module_name();
        if binder.module != expected_module {
            return Err(ResidentError::Run(RuntimeError::Jit(EffectError::Handler(
                format!(
                    "compiled binder `{}` belongs to {}, expected {expected_module}",
                    binder.name, binder.module
                ),
            ))));
        }
        require_host_binding_authority(binder, expected)?;
        let Some(root) = &binder.root_head else {
            return Err(ResidentError::Run(RuntimeError::Jit(EffectError::Handler(
                format!(
                    "compiled binder `{}` has no nominal root type evidence",
                    binder.name
                ),
            ))));
        };
        if root.unit.is_empty() || root.module != expected.module || root.name != expected.name {
            return Err(ResidentError::Run(RuntimeError::Jit(EffectError::Handler(
                format!(
                    "compiled binder `{}` has root {}:{}:{}; host mount requires {}.{}",
                    binder.name, root.unit, root.module, root.name, expected.module, expected.name,
                ),
            ))));
        }
        for qualified in expected.constructors {
            let id = self
                .host_constructor_id(&code.table, expected, qualified)
                .ok_or_else(|| {
                    ResidentError::Run(RuntimeError::Jit(EffectError::Handler(format!(
                        "host mount requires compiler constructor {qualified}"
                    ))))
                })?;
            let family = code
                .prepared
                .constructors()
                .iter()
                .find(|declaration| declaration.host_id == id)
                .map(|declaration| &declaration.family)
                .ok_or_else(|| {
                    ResidentError::Run(RuntimeError::Jit(EffectError::Handler(format!(
                        "host mount constructor {qualified} has no prepared family identity"
                    ))))
                })?;
            if family.unit != root.unit
                || family.module != root.module
                || family.namespace != "type"
                || family.occurrence != root.name
                || family.record_parent.is_some()
            {
                return Err(ResidentError::Run(RuntimeError::Jit(EffectError::Handler(
                    format!(
                        "compiled binder `{}` root {}:{}:{} does not match constructor family {}:{}:{}:{}",
                        binder.name,
                        root.unit,
                        root.module,
                        root.name,
                        family.unit,
                        family.module,
                        family.namespace,
                        family.occurrence,
                    ),
                ))));
            }
        }
        Ok(())
    }

    /// `Text` is authenticated by both its compiler table id and prepared
    /// family identity before a direct host mount allocates it.
    fn validate_text_runtime_constructor(&self, code: &TurnCode<'_>) -> Result<(), ResidentError> {
        let qualified = "Data.Text.Text";
        let id = self
            .host_constructor_id(&code.table, HostBindingType::TEXT, qualified)
            .ok_or_else(|| {
                ResidentError::Run(RuntimeError::Jit(EffectError::Handler(format!(
                    "host Text mount requires compiler constructor {qualified}"
                ))))
            })?;
        let family = code
            .prepared
            .constructors()
            .iter()
            .find(|declaration| declaration.host_id == id)
            .map(|declaration| &declaration.family)
            .ok_or_else(|| {
                ResidentError::Run(RuntimeError::Jit(EffectError::Handler(
                    "host Text constructor has no prepared family identity".into(),
                )))
            })?;
        if family.unit.is_empty()
            || family.module != "Data.Text.Internal"
            || family.namespace != "type"
            || family.occurrence != "Text"
            || family.record_parent.is_some()
        {
            return Err(ResidentError::Run(RuntimeError::Jit(EffectError::Handler(
                format!(
                    "host Text constructor has unexpected family {}:{}:{}:{}",
                    family.unit, family.module, family.namespace, family.occurrence,
                ),
            ))));
        }
        Ok(())
    }

    fn host_constructor_id(
        &self,
        table: &DataConTable,
        expected: HostBindingType,
        qualified: &str,
    ) -> Option<DataConId> {
        table.get_by_qualified_name(qualified).or_else(|| {
            (expected == HostBindingType::TEXT && qualified == "Data.Text.Text")
                .then(|| table.get_by_qualified_name("Data.Text.Internal.Text"))
                .flatten()
        })
    }

    /// Install, build, bind, and unpin one payload-independent interface as
    /// one transaction. A carrier program is needed to bootstrap a fresh
    /// machine, but it owns no live value after the handle is adopted by the
    /// binding table, so its install pin must never escape this method.
    fn mount_host_value_in(
        &mut self,
        scope: ScopeId,
        binder: &BoundBinder,
        gen: Generation,
        code: TurnCode<'_>,
        build: impl FnOnce(
            &mut super::prepared::PreparedEngine,
            RealmId,
            &DataConTable,
        ) -> Result<PreparedHandle, PreparedRuntimeError>,
    ) -> Result<(), ResidentError> {
        self.state
            .merge_table(&code.table)
            .map_err(ResidentError::TableCollision)?;
        let table = code.table;
        let program = self.state.install_prepared(code.prepared.into_owned())?;
        let realm = self.run_context.resource_scope;
        let mounted = (|| {
            let handle = {
                let engine = self.state.require_prepared()?;
                build(engine, realm, &table)?
            };
            self.mount_host_handle_prepared(scope, binder, gen, handle)
        })();
        if let Some(engine) = self.state.prepared_mut() {
            engine.unpin(program);
        }
        mounted
    }

    /// Install a just-built host handle. Unlike externally supplied custody,
    /// this method owns the handle outright, so each failure releases it in
    /// this same checkout instead of relying on deferred custody cleanup.
    fn mount_host_handle_prepared(
        &mut self,
        scope: ScopeId,
        binder: &BoundBinder,
        gen: Generation,
        handle: PreparedHandle,
    ) -> Result<(), ResidentError> {
        let engine = self.state.require_prepared()?;
        let Some(program) = engine.hosting_program(handle) else {
            engine.release(handle);
            return Err(ResidentError::Run(RuntimeError::Jit(EffectError::Handler(
                "host binding mount produced a handle with no hosting program".into(),
            ))));
        };
        if let Err(error) = self.bind_prepared(program, scope, gen, &[(binder, handle)]) {
            return Err(error);
        }
        self.binding_provenance
            .insert(binder.var_id, Arc::new(ProgramProvenance::default()));
        Ok(())
    }

    /// The prepared arm of [`Self::mount_compiled_binding_in`]: the handle
    /// becomes a prepared binding under the binder's value-module identity,
    /// exactly as a prepared bind turn records it ([`Self::bind_prepared`]).
    fn mount_compiled_binding_prepared(
        &mut self,
        scope: ScopeId,
        binder: &BoundBinder,
        gen: Generation,
        custody: RootCustody,
    ) -> Result<(), ResidentError> {
        let transfer = custody.into_transfer();
        let provenance = Arc::clone(&transfer.provenance);
        let raw = transfer.handle;
        let engine = self.state.require_prepared()?;
        let located = engine
            .prepared_handle_of(raw)
            .and_then(|handle| Some((handle, engine.hosting_program(handle)?)));
        let Some((handle, program)) = located else {
            return Err(ResidentError::Run(RuntimeError::Jit(EffectError::Handler(
                "compiled binding mount received an unknown or already-consumed handle".into(),
            ))));
        };
        // `bind_prepared` owns the handle from here, releasing it on failure.
        transfer.commit();
        self.bind_prepared(program, scope, gen, &[(binder, handle)])?;
        self.binding_provenance.insert(binder.var_id, provenance);
        Ok(())
    }

    /// Borrow a retained value as the final field of a typed constructor.
    /// The caller keeps custody alive through resumption; the resulting heap
    /// value has ordinary Haskell reachability independent of that root.
    pub fn resume_framed_custody(
        &mut self,
        hole: ResidentHole,
        custody: &RootCustody,
        constructor: tidepool_repr::DataConId,
        prefix: Vec<HaskellValue>,
    ) -> Result<ResidentOutcome, ResidentError> {
        let Some(handle) = custody.handle else {
            unreachable!("live custody always contains its handle");
        };
        let seed = hole.seed();
        let cont_id = match hole {
            ResidentHole::Plain(hole) => hole.id,
            ResidentHole::Binding(hole) => hole.id,
            ResidentHole::ProjectedBinding(hole) => hole.id,
        };
        self.reenter(
            &cont_id,
            ResidentResumeInput::FramedHandle {
                handle,
                constructor,
                prefix,
            },
            seed,
            Some(&custody.provenance),
        )
    }

    /// Whether the resident machine has been bootstrapped yet. `false` from
    /// [`Self::unbootstrapped`] until the session's first real turn brings the
    /// machine up (`run_with_sites`/`run_bind_with_sites`/`run_child`/
    /// `run_child_pure`).
    pub fn is_bootstrapped(&self) -> bool {
        self.state.is_bootstrapped()
    }

    /// Typed machine-integrity observation for the host reentry boundary.
    /// Registry ownership remains outside this value, so callers must still
    /// acquire an ordinary checkout before executing anything.
    #[must_use]
    pub fn machine_disposition(&self) -> Option<tidepool_codegen::machine::MachineDisposition> {
        self.state.machine_disposition()
    }

    #[must_use]
    pub fn data_con_table(&self) -> &DataConTable {
        self.state.session_table()
    }

    /// Read-only heap/GC snapshot of this session's live machine (observatory
    /// heap pane) — `None` either before the machine is bootstrapped (see
    /// [`Self::unbootstrapped`]/[`Self::is_bootstrapped`]) or during the
    /// transient window a turn is running on its own eval thread (the machine
    /// moved out; see [`Self::on_eval_thread`]).
    /// Move the decl plane out for a machine rotation — see
    /// [`super::persistent::PersistentSession::take_lib`].
    pub fn take_lib(&mut self) -> Option<crate::session::SessionLib> {
        self.state.take_lib()
    }

    /// Number of live [`ValueHandle`]s outstanding on this session's machine
    /// (0 before the machine is bootstrapped) — the mount seam's ownership-
    /// accounting read: a handle minted over a finalize payload
    /// ([`Self::live_payload_handle`]) counts here until a compiled-binding mount
    /// ([`Self::mount_compiled_binding_in`]), an ordinary bind completion, or
    /// a realm close releases it.
    pub fn value_handle_count(&mut self) -> usize {
        self.settle_dropped_custody();
        self.state.value_handle_count()
    }

    /// Whether the resident compiler has an incomplete, unusable module.
    pub fn compilation_failed(&self) -> bool {
        self.state.machine_disposition().is_some_and(|disposition| {
            disposition == tidepool_codegen::machine_state::MachineDisposition::Unavailable
        })
    }

    pub fn heap_stats(&self) -> Option<tidepool_codegen::machine::HeapStats> {
        self.state.heap_stats()
    }

    /// The CURRENT value-plane binding names (newest gen per name) — what a
    /// machine rotation would lose (enumerated, legible loss, never silent).
    pub fn binding_names(&self) -> Vec<String> {
        self.state
            .bindings()
            .iter_current()
            .map(|(name, _)| name.0.clone())
            .collect()
    }

    // -- scopes --------------------------------------------------------------

    /// Mint a fresh child scope of `parent` ([`ScopeId::ROOT`] for a top-level
    /// invocation scope). `None` if `parent` is not live.
    pub fn mint_scope(&mut self, parent: ScopeId) -> Option<ScopeId> {
        self.state.mint_scope(parent)
    }

    /// Immutable value-binding snapshot captured when `scope` was minted.
    #[must_use]
    pub fn binding_tip_id(
        &self,
        scope: ScopeId,
    ) -> Option<tidepool_codegen::binding_table::BindingTipId> {
        self.state.binding_tip_id(scope)
    }

    /// Mint a fresh actor lexical root with no ambient declaration or value
    /// ancestry. Program visibility must be supplied through exact imports.
    pub fn mint_isolated_scope(&mut self) -> ScopeId {
        self.state.mint_isolated_scope()
    }

    /// The value-plane names visible at `scope`: its own mutable frame over
    /// the immutable inherited tip captured when the scope was minted.
    /// `binding_names_in(ScopeId::ROOT)` is [`Self::binding_names`]'s set.
    pub fn binding_names_in(&self, scope: ScopeId) -> Vec<String> {
        self.state
            .bindings()
            .iter_current_in(self.state.scope_tree(), scope)
            .into_iter()
            .filter(|(_, entry)| !self.hidden_host_bindings.contains_key(&entry.id))
            .map(|(name, _)| name.0.clone())
            .collect()
    }

    /// Term-level names visible to a GHCi-style `:bindings` query.
    ///
    /// The declaration environment and materialized binding store are one
    /// lexical view. Materialized names win on collision, matching ordinary
    /// turn compilation, and the returned order is deterministic. This query
    /// never forces a live value.
    pub fn workbench_bindings_in(&self, scope: ScopeId) -> Vec<super::WorkbenchBinding> {
        let mut bindings = std::collections::BTreeMap::new();
        for (item, generation) in self.state.lib().current_declarations_in(scope) {
            if let super::ExportItem::Value { name } = &item {
                let binding = match self.state.lib().declaration_value_type(generation, name) {
                    Some(ty) => {
                        super::WorkbenchBinding::typed_declaration(name.clone(), ty.to_owned())
                    }
                    None => super::WorkbenchBinding::declaration(name.clone(), item.render_entry()),
                };
                bindings.insert(name.clone(), binding.with_generation(Some(generation)));
            }
        }
        for name in self.binding_names_in(scope) {
            let (generation, type_display) = self
                .current_binding_in(scope, &name)
                .map(|(_, module, _, type_display)| (Some(module.gen().0), type_display))
                .unwrap_or_default();
            bindings.insert(
                name.clone(),
                super::WorkbenchBinding::materialized(name, type_display)
                    .with_generation(generation),
            );
        }
        bindings.into_values().collect()
    }

    /// Retain one compatibility inspection batch against the exact declaration
    /// generations it observed. Stale entries are ignored by the declaration
    /// log owner.
    pub fn retain_declaration_value_types_in(
        &mut self,
        scope: ScopeId,
        types: &[(String, u64, String)],
    ) {
        self.state
            .lib_mut()
            .retain_declaration_value_types_in(scope, types);
    }

    /// Check exact current declaration source without evaluating a live value.
    /// Materialized bindings shadow declarations in the resident lexical view.
    pub fn workbench_declaration_matches_in(
        &self,
        scope: ScopeId,
        name: &str,
        source: &str,
        required_imports: &super::SourceImports,
    ) -> bool {
        if self.current_binding_in(scope, name).is_some() {
            return false;
        }
        let library = self.state.lib();
        library
            .current_declarations_in(scope)
            .into_iter()
            .any(|(item, generation)| {
                matches!(item, super::ExportItem::Value { name: ref declared } if declared == name)
                    && library
                        .log
                        .turns
                        .get(generation.saturating_sub(1) as usize)
                        .is_some_and(|turn| {
                            let mut imports = turn.external_imports.clone();
                            imports.extend(&turn.normalized.prologue.workbench_imports());
                            required_imports
                                .specs()
                                .iter()
                                .all(|required| imports.specs().contains(required))
                                && turn.normalized.body.trim() == source.trim()
                        })
            })
    }

    /// Source-only declaration recovery facts for this machine incarnation.
    /// Live values and handles are intentionally absent because they cannot
    /// survive machine replacement.
    #[must_use]
    pub fn declaration_recovery_report(&self) -> Option<&super::DeclarationRecoveryReport> {
        self.state.lib().declaration_recovery_report()
    }

    /// A manifest publication failure that happened after a successful
    /// declaration commit, if durability has not recovered since.
    #[must_use]
    pub fn recovery_manifest_warning(&self) -> Option<&str> {
        self.state.lib().recovery_manifest_warning()
    }

    /// How many names `scope`'s OWN frame binds (accounting class 3, per
    /// scope — inherited names are not counted, only locally-bound ones).
    /// Returns to 0 when the scope retires.
    pub fn scope_binding_count(&self, scope: ScopeId) -> usize {
        self.state.scope_binding_count(scope)
    }

    /// Number of persistent GC roots registered on this session's machine
    /// (accounting class 4 — the GC ROOT LEDGER; 0 before the machine
    /// bootstraps). This is the WITNESS for [`Self::retire_scope`]: it drops
    /// by exactly the receipt's `roots_released` and by nothing else.
    ///
    /// Deliberately separate from classes 1 (`stowed_roots_count() ==
    /// parked_count()`) and 2 ([`Self::value_handle_count`]), which a scope
    /// retirement leaves untouched — folding them together is what makes a
    /// leak invisible.
    pub fn persistent_roots_count(&mut self) -> usize {
        self.settle_dropped_custody();
        self.state.persistent_roots_count()
    }

    /// Accounting class 1 — the PARKED-CONTINUATION roots, as the pair that
    /// must always agree (`stowed_roots_count() == parked_count()`, the
    /// machine's own quiescence invariant). 0 before the machine bootstraps.
    /// A scope retirement must leave both UNCHANGED: a parked frame's root is
    /// a realm's, not a scope's, and folding the two classes together is how a
    /// leak becomes invisible.
    pub fn stowed_roots_count(&self) -> usize {
        self.state.stowed_roots_count()
    }

    /// The parked-frame half of accounting class 1 — see
    /// [`Self::stowed_roots_count`].
    pub fn parked_count(&self) -> usize {
        self.state.parked_count()
    }

    /// Retire `scope` and its subtree: drop their value-plane frames and
    /// release the GC roots those bindings solely owned. See
    /// [`PersistentSession::retire_scope`] for the sole-ownership rule and the
    /// deregistered-is-not-reclaimed bound; retiring ROOT or an already-retired
    /// scope is a no-op returning an all-zero receipt.
    pub fn retire_scope(&mut self, scope: ScopeId) -> ScopeRetirement {
        let retirement = self.state.retire_scope(scope);
        self.host_text_bindings
            .retain(|id, _| self.state.bindings().get(*id).is_some());
        self.hidden_host_bindings
            .retain(|id, _| self.state.bindings().get(*id).is_some());
        retirement
    }

    fn provenance_for(&self, sites: &[YieldSite]) -> Result<Arc<ProgramProvenance>, ResidentError> {
        Ok(Arc::new(ProgramProvenance::from_sites(sites)?))
    }

    fn next_cont_id(&self) -> String {
        self.cont_id_issuer.next_id()
    }

    /// Run one prepared turn. A suspended session rejects this because nested
    /// runs are owned by the child-run path.
    ///
    /// `table` is this turn's constructor metadata; it is merged into the
    /// session table (later turns are a subset, so the merge is monotone).
    pub fn run_with_sites(
        &mut self,
        name_hint: &str,
        code: TurnCode<'_>,
    ) -> Result<ResidentOutcome, ResidentError> {
        self.run_transient_with_sites(name_hint, code)
    }

    /// Evaluate a compiler-checked pure inspection of retained values without
    /// extending their lifetime. The caller must compile a pure result lifted
    /// into Eff, so this run cannot export slot-dependent closures via effects.
    pub fn run_inspection_with_sites(
        &mut self,
        code: TurnCode<'_>,
    ) -> Result<ResidentOutcome, ResidentError> {
        self.run_transient_with_sites("actor_observation_preview", code)
    }

    /// Run a compiler-generated pure preview against a committed binding.
    /// The mounted binding retains its value if rendering fails.
    pub fn run_mounted_inspection_with_sites(
        &mut self,
        code: TurnCode<'_>,
        binding: SessionVarId,
    ) -> Result<ResidentOutcome, ResidentError> {
        let entry = self
            .state
            .bindings()
            .get(binding)
            .ok_or(BindingAliasError::MissingSource(binding))?;
        let BoundValue { handle, .. } = &entry.value;
        self.run_prepared_with_argument(code, PreparedTurnMode::Value, Some(*handle))
    }

    /// The live prepared bindings a later turn compiles against
    /// ([`PersistentSession::prepared_retained`]).
    #[must_use]
    pub fn prepared_retained(&self) -> Vec<(SymbolIdentity, u64)> {
        self.state.prepared_retained()
    }

    /// The prepared arm of every resident turn: install the turn's program
    /// against the session's live prepared bindings, run its settled scaffold
    /// on the eval thread, and either return the observed value, bind it
    /// into the value plane, or report the suspension whose frame the machine
    /// parked.
    fn run_prepared(
        &mut self,
        code: TurnCode<'_>,
        mode: PreparedTurnMode<'_>,
    ) -> Result<ResidentOutcome, ResidentError> {
        self.run_prepared_with_argument(code, mode, None)
    }

    fn run_prepared_with_argument(
        &mut self,
        code: TurnCode<'_>,
        mode: PreparedTurnMode<'_>,
        argument: Option<PreparedHandle>,
    ) -> Result<ResidentOutcome, ResidentError> {
        let prepared = code.prepared;
        let provenance = self.provenance_for(&code.sites)?;
        self.state
            .merge_table(&code.table)
            .map_err(ResidentError::TableCollision)?;
        if let PreparedTurnMode::Binding { generation, .. }
        | PreparedTurnMode::Projected { generation, .. } = &mode
        {
            // Claim the value-module identity before the turn runs.
            self.state.set_val_gen(*generation);
        }
        let install_prepared_started = std::time::Instant::now();
        let program = self.state.install_prepared(prepared.into_owned())?;
        timing::record_stage(
            timing::NO_NODE,
            timing::NO_ROUND,
            timing::STAGE_INSTALL_PREPARED,
            install_prepared_started.elapsed(),
            0,
        );
        let realm = self.run_context.resource_scope;
        let lexical_scope = self.run_context.lexical_scope;
        let plan = settle_plan_of(&mode);
        let park = ParkPolicy {
            principal: self.run_context.principal,
            effect_policy: self.state.effect_policy(),
            live_payload: self.state.live_payload_policy(),
        };
        let run_exec_started = std::time::Instant::now();
        let ran = self.on_eval_thread(move |engine, table, handlers, captured| {
            Ok(settle_prepared(
                engine, program, realm, argument, plan, park, table, handlers, captured,
            ))
        });
        timing::record_stage(
            timing::NO_NODE,
            timing::NO_ROUND,
            timing::STAGE_RUN_EXEC,
            run_exec_started.elapsed(),
            0,
        );
        // The install-to-first-run gap this turn's `install_prepared` pinned
        // against is closed here, whatever `ran` turned out to be: a
        // completed or parked outcome is protected from here on by the
        // session's persistent roots or the parked frame's own evidence, and
        // a failed run has nothing left to protect.
        if let Some(engine) = self.state.prepared_mut() {
            engine.unpin(program);
        }
        self.complete_prepared(ran??, mode, program, lexical_scope, provenance, None)
    }

    /// Finish one prepared run on the session thread, whichever entry
    /// produced it: bind or return a completed value per `mode`, or classify a
    /// parked suspension. `resumed` names the hole this run answered (a
    /// resume) and is retired on a real outcome; `None` for a fresh turn.
    fn complete_prepared(
        &mut self,
        run: PreparedRun,
        mode: PreparedTurnMode<'_>,
        program: ProgramId,
        lexical_scope: ScopeId,
        provenance: Arc<ProgramProvenance>,
        resumed: Option<&str>,
    ) -> Result<ResidentOutcome, ResidentError> {
        let seed = hole_seed_of(&mode, lexical_scope);
        let outcome = match run {
            PreparedRun::Done { handle, value } => {
                let engine = self.state.require_prepared()?;
                match mode {
                    PreparedTurnMode::Value => {
                        engine.release(handle);
                    }
                    PreparedTurnMode::Binding {
                        binder,
                        generation,
                        observation,
                    } => {
                        self.bind_prepared(
                            program,
                            lexical_scope,
                            generation,
                            &[(binder, handle)],
                        )?;
                        self.binding_provenance
                            .insert(binder.var_id, Arc::clone(&provenance));
                        if let Some(dependencies) = observation {
                            self.finish_observation(binder, &dependencies);
                        }
                    }
                    PreparedTurnMode::Projected { .. } => {
                        // The eval thread splits a projected tuple
                        // (`SettlePlan::Project`); a whole value here is a
                        // settlement mismatch.
                        engine.release(handle);
                        return Err(PreparedRuntimeError::UnsettledEntry {
                            program,
                            detail: "a whole-value settlement for a pattern turn",
                        }
                        .into());
                    }
                }
                self.classify_parked(ParkedRun::CompletedValue(value), resumed, seed, provenance)
            }
            PreparedRun::Projected { fields } => {
                let PreparedTurnMode::Projected {
                    binders,
                    generation,
                } = mode
                else {
                    if let Some(engine) = self.state.prepared_mut() {
                        engine.release_all(fields);
                    }
                    return Err(PreparedRuntimeError::UnsettledEntry {
                        program,
                        detail: "a projected settlement for a non-pattern turn",
                    }
                    .into());
                };
                let bound: Vec<(&BoundBinder, PreparedHandle)> =
                    binders.iter().zip(fields).collect();
                self.bind_prepared(program, lexical_scope, generation, &bound)?;
                for binder in binders {
                    self.binding_provenance
                        .insert(binder.var_id, Arc::clone(&provenance));
                }
                self.classify_parked(ParkedRun::CompletedProject, resumed, seed, provenance)
            }
            PreparedRun::Display {
                page,
                metadata,
                alias,
            } => {
                if let Some(engine) = self.state.prepared_mut() {
                    engine.release_all([page, metadata, alias]);
                }
                return Err(PreparedRuntimeError::UnsettledEntry {
                    program,
                    detail: "a display bundle reached the ordinary turn completion path",
                }
                .into());
            }
            PreparedRun::Suspended { id, request } => {
                // The frame is parked in the machine's ledger; the hole
                // carries the turn's completion obligation forward.
                self.classify_parked(
                    ParkedRun::Suspended { id, request },
                    resumed,
                    seed,
                    provenance,
                )
            }
        };
        // The between-turn quiescent point: a fresh or resumed turn that
        // settled or parked just released whatever it retired-in-place, so
        // this is where a major collection -- if the collection policy
        // judges one due (install-count window closed, or root-block bytes
        // grew enough since the last one) and the machine happens to be
        // quiescent -- drains its retirement receipt. Most turns are not
        // due and this returns immediately without touching the machine
        // (`PreparedEngine::quiesce_and_collect`). Not reached on an error
        // path above -- an aborted/failed turn leaves nothing settled to
        // quiesce over, and the next successful turn drains instead. A
        // collection failure (anything but "not quiescent yet") is this
        // turn's failure.
        if let Some(engine) = self.state.prepared_mut() {
            if engine.disposition() == tidepool_codegen::machine::MachineDisposition::Reusable {
                engine.quiesce_and_collect()?;
            }
        }
        Ok(outcome)
    }

    /// Bind retained prepared handles into the value plane at `scope`, one
    /// entry per `(binder, handle)`, all at `generation`. Every handle is
    /// adopted into the machine's ROOT scope first (no realm close releases
    /// it), so the binding owns its lifetime and scope retirement releases
    /// it. The lexical scope is validated before any handle is adopted; a
    /// failure releases every handle not yet bound, so nothing is left rooted
    /// outside both the realm ledger and the binding table.
    fn bind_prepared(
        &mut self,
        program: ProgramId,
        scope: ScopeId,
        generation: Generation,
        bound: &[(&BoundBinder, PreparedHandle)],
    ) -> Result<(), ResidentError> {
        let scope_is_live = self.state.scope_tree().is_live(scope);
        let engine = self.state.require_prepared()?;
        if !scope_is_live {
            engine.release_all(bound.iter().map(|(_, handle)| *handle));
            return Err(SessionError::DeadScope(scope).into());
        }
        let unit = engine.entry_unit(program).unwrap_or_default();
        let mut roots = Vec::with_capacity(bound.len());
        for (_, handle) in bound {
            match engine.adopt(*handle) {
                Some(root) => roots.push(root),
                None => {
                    engine.release_all(bound.iter().map(|(_, handle)| *handle));
                    return Err(PreparedRuntimeError::Run(
                        tidepool_codegen::prepared_program::ExecutionError::UnknownPreparedHandle,
                    )
                    .into());
                }
            }
        }
        for (index, ((binder, handle), root)) in bound.iter().zip(roots).enumerate() {
            // The identity a later turn's `GlobalDecl` names when it imports
            // this binder: its thin value module and name.
            let identity = SymbolIdentity {
                unit: unit.clone(),
                module: binder.module.clone(),
                namespace: "value".into(),
                occurrence: binder.name.clone(),
                record_parent: None,
            };
            let entry = BindingEntry {
                name: BindingName(binder.name.clone()),
                id: SessionVarId::from_extract(binder.var_id),
                module: SessionModule::val(generation),
                value: BoundValue {
                    root,
                    handle: *handle,
                    identity,
                },
                type_display: Some(binder.type_display.clone()),
                defining_expr: None,
                scope,
            };
            if let Err(error) = self.state.bind_replacing_decl_in(scope, entry) {
                if let Some(engine) = self.state.prepared_mut() {
                    engine.release_all(bound[index..].iter().map(|(_, handle)| *handle));
                }
                return Err(error.into());
            }
        }
        self.state.set_val_gen(generation);
        Ok(())
    }

    fn run_transient_with_sites(
        &mut self,
        _name_hint: &str,
        code: TurnCode<'_>,
    ) -> Result<ResidentOutcome, ResidentError> {
        self.run_prepared(code, PreparedTurnMode::Value)
    }

    /// Run a value-plane BIND turn (`x <- e`): seed the env from prior bindings,
    /// add the fragment, and drive it through the suspendable BIND path
    /// (tenure-on-completion). On completion, materialize `binder` into the value
    /// plane at `gen` (the SAME generation the extract stamped into
    /// `binder.module` — mint it once at compile, thread it here). A fork bind
    /// SUSPENDS here (no value yet); the returned [`ResidentHole::Binding`]
    /// carries `binder`/`gen` forward, so the eventual [`Self::resume`] on
    /// that hole materializes it without the caller re-supplying either.
    pub fn run_bind_with_sites(
        &mut self,
        name_hint: &str,
        code: TurnCode<'_>,
        binder: &BoundBinder,
        gen: Generation,
    ) -> Result<ResidentOutcome, ResidentError> {
        self.run_binding_with_sites(name_hint, code, binder, gen, None)
    }

    /// Capture an automatic workbench observation using the same suspendable
    /// binding path. Effectful expressions can export slot-dependent closures,
    /// so their dependencies acquire the ordinary persistent lifetime first.
    pub fn run_observation_with_sites(
        &mut self,
        code: TurnCode<'_>,
        binder: &BoundBinder,
        gen: Generation,
        effectful: bool,
    ) -> Result<ResidentOutcome, ResidentError> {
        let _ = effectful;
        self.run_binding_with_sites("actor_observation", code, binder, gen, Some(Vec::new()))
    }

    /// Run one generated display bundle.  The compiler supplies all three
    /// binder identities in one `Val.G<generation>` interface: a captured
    /// page, its `(Text, hasMore, unavailable)` metadata, and `cellDisplay`.
    ///
    /// The page is bound before metadata is forced.  This preserves the
    /// workbench promise that a display failure never loses an expression that
    /// has already run.  The final alias is deliberately published through
    /// [`Self::publish_captured_alias_in`] rather than materializing the tuple's
    /// third field: it must share the page's existing root and dependency edge.
    pub fn run_display_bundle_with_sites(
        &mut self,
        code: TurnCode<'_>,
        page: &BoundBinder,
        metadata: &BoundBinder,
        alias: &BoundBinder,
        generation: Generation,
    ) -> Result<ResidentDisplayBundle, ResidentError> {
        if page.module != metadata.module || page.module != alias.module {
            return Err(ResidentError::Run(RuntimeError::Jit(EffectError::Handler(
                "display bundle binders do not share one compiler value module".into(),
            ))));
        }
        let prepared = code.prepared;
        let provenance = self.provenance_for(&code.sites)?;
        self.state
            .merge_table(&code.table)
            .map_err(ResidentError::TableCollision)?;
        self.state.set_val_gen(generation);
        let install_prepared_started = std::time::Instant::now();
        let program = self.state.install_prepared(prepared.into_owned())?;
        timing::record_stage(
            timing::NO_NODE,
            timing::NO_ROUND,
            timing::STAGE_INSTALL_PREPARED,
            install_prepared_started.elapsed(),
            0,
        );
        let realm = self.run_context.resource_scope;
        let lexical_scope = self.run_context.lexical_scope;
        let park = ParkPolicy {
            principal: self.run_context.principal,
            effect_policy: self.state.effect_policy(),
            live_payload: self.state.live_payload_policy(),
        };
        let run_exec_started = std::time::Instant::now();
        let ran = self.on_eval_thread(move |engine, table, handlers, captured| {
            Ok(settle_prepared(
                engine,
                program,
                realm,
                None,
                SettlePlan::Display(page.tier),
                park,
                table,
                handlers,
                captured,
            ))
        });
        timing::record_stage(
            timing::NO_NODE,
            timing::NO_ROUND,
            timing::STAGE_RUN_EXEC,
            run_exec_started.elapsed(),
            0,
        );
        if let Some(engine) = self.state.prepared_mut() {
            engine.unpin(program);
        }
        let (page_handle, metadata_handle, alias_handle) = match ran?? {
            PreparedRun::Display {
                page,
                metadata,
                alias,
            } => (page, metadata, alias),
            PreparedRun::Done { handle, .. } => {
                self.on_eval_thread(move |engine, _, _, _| {
                    engine.release(handle);
                    Ok(())
                })?;
                return Err(PreparedRuntimeError::UnsettledEntry {
                    program,
                    detail: "display bundle settled as a whole value",
                }
                .into());
            }
            PreparedRun::Projected { fields } => {
                self.on_eval_thread(move |engine, _, _, _| {
                    engine.release_all(fields);
                    Ok(())
                })?;
                return Err(PreparedRuntimeError::UnsettledEntry {
                    program,
                    detail: "display bundle settled as an ordinary projected bind",
                }
                .into());
            }
            PreparedRun::Suspended { id, .. } => {
                self.on_eval_thread(move |engine, _, _, _| {
                    engine
                        .abort_parked(id)
                        .map_err(|error| EffectError::Handler(error.to_string()))?;
                    Ok(())
                })?;
                return Err(PreparedRuntimeError::UnsettledEntry {
                    program,
                    detail: "display bundle suspended while constructing a pure page",
                }
                .into());
            }
        };
        if let Err(error) =
            self.bind_prepared(program, lexical_scope, generation, &[(page, page_handle)])
        {
            self.release_display_fields(metadata_handle, alias_handle)?;
            return Err(error);
        }
        self.binding_provenance
            .insert(page.var_id, Arc::clone(&provenance));
        self.finish_observation(page, &[]);

        let metadata_value =
            self.observe_display_metadata(program, metadata_handle, alias_handle)?;
        let lease = self.lease_bindings(&[tidepool_repr::VarId(page.var_id)]);
        self.publish_captured_alias_in(
            lexical_scope,
            SessionVarId::from_extract(page.var_id),
            alias,
            generation,
            &lease,
        )?;
        self.binding_provenance.insert(alias.var_id, provenance);
        self.settle_dropped_custody();
        if let Some(engine) = self.state.prepared_mut() {
            if engine.disposition() == tidepool_codegen::machine::MachineDisposition::Reusable {
                engine.quiesce_and_collect()?;
            }
        }
        Ok(ResidentDisplayBundle {
            result: EvalResult::new(
                metadata_value,
                self.state.session_table().clone(),
                Vec::new(),
            ),
        })
    }

    fn release_display_fields(
        &mut self,
        metadata: PreparedHandle,
        alias: PreparedHandle,
    ) -> Result<(), ResidentError> {
        self.on_eval_thread(move |engine, _, _, _| {
            engine.release_all([metadata, alias]);
            Ok(())
        })
    }

    fn observe_display_metadata(
        &mut self,
        program: ProgramId,
        metadata: PreparedHandle,
        alias: PreparedHandle,
    ) -> Result<HaskellValue, ResidentError> {
        self.on_eval_thread(move |engine, _, _, _| {
            let observed = match engine.observe(program, metadata) {
                Ok(value) => Ok(value),
                Err(error) if is_observation_budget_exhausted(&error) => {
                    engine.observe_bounded(program, metadata)
                }
                Err(error) => Err(error),
            };
            engine.release_all([metadata, alias]);
            Ok(observed)
        })?
        .map_err(Into::into)
    }

    fn run_binding_with_sites(
        &mut self,
        _name_hint: &str,
        code: TurnCode<'_>,
        binder: &BoundBinder,
        gen: Generation,
        observation: Option<Vec<tidepool_repr::VarId>>,
    ) -> Result<ResidentOutcome, ResidentError> {
        self.run_prepared(
            code,
            PreparedTurnMode::Binding {
                binder,
                generation: gen,
                observation,
            },
        )
    }

    fn finish_observation(&mut self, binder: &BoundBinder, dependencies: &[tidepool_repr::VarId]) {
        self.state
            .save_observation(SessionVarId::from_extract(binder.var_id), dependencies);
        self.binding_provenance.retain(|id, _| {
            self.state
                .bindings()
                .get(SessionVarId::from_extract(*id))
                .is_some()
        });
    }

    /// Run one GHC-classified pattern bind and materialize every projected
    /// component atomically into the current lexical scope. The JIT owns tuple
    /// projection; Rust receives only GHC's binder metadata and never parses
    /// the authored pattern.
    pub fn run_projected_bind_with_sites(
        &mut self,
        _name_hint: &str,
        code: TurnCode<'_>,
        binders: &[BoundBinder],
        gen: Generation,
    ) -> Result<ResidentOutcome, ResidentError> {
        if binders.is_empty() {
            return Err(PreparedRuntimeError::ProjectionShape {
                binders: 0,
                fields: 0,
            }
            .into());
        }
        self.run_prepared(
            code,
            PreparedTurnMode::Projected {
                binders,
                generation: gen,
            },
        )
    }

    /// Apply a handle-rooted entry closure to an integer and run it as a new
    /// suspension-capable top-level computation under `realm`.
    ///
    /// This operation has a suspension-shaped result: a parked frame joins
    /// the ordinary continuation registry and can be resumed by identity in
    /// any order.
    ///
    /// `entry` is a `ValueHandle` over a tenured `Int -> M a` closure. It is
    /// applied through an `App(Var, Lit)` synthesis: `FINALIZED_VAR` (any
    /// `VarId` not otherwise bound in the fragment) resolves through an
    /// `ExternalEnv`-seeded slot over the entry's root, and the argument
    /// crosses as a bare unboxed `Lit`, so it does not depend on a
    /// caller-owned wrapper-constructor id. Execution goes through the
    /// canonical suspension entry and registry.
    ///
    /// **`realm` is the thread's, and it propagates.** `resume_continuation` replays
    /// a frame's OWN realm, so every later suspension of this thread parks under
    /// `realm` too — which is what makes `close_realm(realm)` a complete
    /// cancellation rather than a first-frame one.
    ///
    /// The result contract belongs to the entry program. The async adapter, for
    /// example, ends by suspending on `AsyncDoneWith`; actor startup can use the
    /// same rooted entry without acquiring a second execution primitive.
    pub fn run_rooted_entry(
        &mut self,
        name_hint: &str,
        entry: RootCustody,
        argument: i64,
        realm: RealmId,
        run_table: Option<&DataConTable>,
    ) -> Result<ResidentOutcome, ResidentError> {
        let outcome =
            self.run_rooted_entry_borrowed(name_hint, &entry, argument, realm, run_table)?;
        entry.into_transfer().commit();
        Ok(outcome)
    }

    /// Invoke retained code without transferring its root to the execution.
    pub fn run_rooted_entry_borrowed(
        &mut self,
        _name_hint: &str,
        entry: &RootCustody,
        argument: i64,
        realm: RealmId,
        run_table: Option<&DataConTable>,
    ) -> Result<ResidentOutcome, ResidentError> {
        self.settle_dropped_custody();
        if !Arc::ptr_eq(&entry.cleanup, &self.custody_cleanup) {
            return Err(ResidentError::ForeignCustody);
        }
        let provenance = Arc::clone(&entry.provenance);
        let Some(entry) = entry.handle else {
            unreachable!("live custody contains its handle");
        };

        self.run_rooted_entry_prepared(entry, argument, realm, run_table, provenance)
    }

    /// Apply one rooted Haskell function to one rooted Haskell argument and
    /// run the resulting `Eff` computation as a suspension-capable top-level
    /// turn. Both values remain opaque: no bridge, serialization, constructor
    /// inspection, or type-directed Rust code sits on this path.
    ///
    /// This is the value-to-code counterpart of [`Self::run_rooted_entry`]. It
    /// exists for boundaries such as actor mailboxes where both the handler
    /// and its protocol-indexed request are live Haskell values. The caller
    /// retains both roots across success or failure. A suspended computation
    /// owns its reachable values independently of these borrowed roots.
    pub fn run_rooted_application(
        &mut self,
        _name_hint: &str,
        function: &RootCustody,
        argument: &RootCustody,
        realm: RealmId,
        run_table: Option<&DataConTable>,
    ) -> Result<ResidentOutcome, ResidentError> {
        self.settle_dropped_custody();
        if !Arc::ptr_eq(&function.cleanup, &self.custody_cleanup)
            || !Arc::ptr_eq(&argument.cleanup, &self.custody_cleanup)
        {
            return Err(ResidentError::ForeignCustody);
        }

        let Some(function_handle) = function.handle else {
            unreachable!("live custody always contains its handle");
        };
        let Some(argument_handle) = argument.handle else {
            unreachable!("live custody always contains its handle");
        };

        let mut provenance = (*function.provenance).clone();
        provenance.merge(&argument.provenance)?;
        self.run_rooted_application_prepared(
            function_handle,
            argument_handle,
            realm,
            run_table,
            Arc::new(provenance),
        )
    }

    /// The prepared-route arm of [`Self::run_rooted_entry_borrowed`]: apply
    /// the rooted closure through the shared `__applyEntry` scaffold root
    /// (`settle_rooted_entry`), then finish through
    /// [`Self::finish_rooted_prepared`] exactly as an ordinary prepared turn
    /// finishes: a suspension parks under `realm` in the machine's ordinary
    /// continuation registry and resumes through [`Self::reenter_prepared`]
    /// like any other prepared frame, whatever produced it.
    fn run_rooted_entry_prepared(
        &mut self,
        entry: ValueHandle,
        argument: i64,
        realm: RealmId,
        run_table: Option<&DataConTable>,
        provenance: Arc<ProgramProvenance>,
    ) -> Result<ResidentOutcome, ResidentError> {
        let table = run_table
            .cloned()
            .unwrap_or_else(|| self.state.session_table().clone());
        let park = ParkPolicy {
            principal: self.run_context.principal,
            effect_policy: self.state.effect_policy(),
            live_payload: self.state.live_payload_policy(),
        };
        let ran = self.on_eval_thread(move |engine, _table, handlers, captured| {
            Ok(settle_rooted_entry(
                engine, entry, argument, realm, park, &table, handlers, captured,
            ))
        })?;
        let (program, run) = ran?;
        self.finish_rooted_prepared(run, program, provenance)
    }

    /// [`Self::run_rooted_entry_prepared`], but applying one rooted value to
    /// another through `__applyValue` (`settle_rooted_application`) — the
    /// prepared-route arm of [`Self::run_rooted_application`].
    fn run_rooted_application_prepared(
        &mut self,
        function: ValueHandle,
        argument: ValueHandle,
        realm: RealmId,
        run_table: Option<&DataConTable>,
        provenance: Arc<ProgramProvenance>,
    ) -> Result<ResidentOutcome, ResidentError> {
        let table = run_table
            .cloned()
            .unwrap_or_else(|| self.state.session_table().clone());
        let park = ParkPolicy {
            principal: self.run_context.principal,
            effect_policy: self.state.effect_policy(),
            live_payload: self.state.live_payload_policy(),
        };
        let ran = self.on_eval_thread(move |engine, _table, handlers, captured| {
            Ok(settle_rooted_application(
                engine, function, argument, realm, park, &table, handlers, captured,
            ))
        })?;
        let (program, run) = ran?;
        self.finish_rooted_prepared(run, program, provenance)
    }

    /// Finish a rooted apply's settled layer through the same
    /// [`Self::complete_prepared`] a turn's own settled scaffold finishes
    /// through, in [`PreparedTurnMode::Value`]: a `Done` value is observed
    /// and returned (never bound into the value plane — a rooted apply is
    /// not a session turn), and a `Suspended` frame is classified exactly
    /// like any other prepared suspension. `program` is the rooted apply's
    /// hosting program, carried only for `complete_prepared`'s own
    /// diagnostics — the parked frame's own evidence (not `program`) is what
    /// a later resume actually re-enters through.
    fn finish_rooted_prepared(
        &mut self,
        run: PreparedRun,
        program: ProgramId,
        provenance: Arc<ProgramProvenance>,
    ) -> Result<ResidentOutcome, ResidentError> {
        let lexical_scope = self.run_context.lexical_scope;
        self.complete_prepared(
            run,
            PreparedTurnMode::Value,
            program,
            lexical_scope,
            provenance,
            None,
        )
    }

    /// Resume the suspended turn `hole` answered with `answer`, driving the
    /// fragment to its next suspension or completion. Atomic
    /// validate-before-consume: `hole`'s id must match the pending
    /// continuation or the pending one is untouched
    /// ([`ResidentError::WrongContinuation`], mirroring `engine.rs`:684–698 and
    /// the repl server's three-way resume errors).
    ///
    /// The ONE consuming entry point — replaces the old `resume`/`resume_bind`
    /// split. `hole` carries its own completion obligation ([`ResidentHole`]'s
    /// doc): a [`ResidentHole::Binding`] materializes its binder into the
    /// value plane on completion, using the SAME binder/generation its
    /// initiating [`Self::run_bind`] carried; a [`ResidentHole::Plain`] does
    /// nothing extra. There is no external "is this pending a bind" flag left
    /// for a caller to get out of sync with which method it calls — there is
    /// only this one method, and the hole itself says what it owes.
    pub fn resume<T>(
        &mut self,
        hole: ResidentHole,
        answer: T,
    ) -> Result<ResidentOutcome, ResidentError>
    where
        T: tidepool_bridge::ToHaskell + Send + 'static,
    {
        self.resume_response(hole, Response::new(answer))
    }

    /// Resume a suspended turn from one owned structural source. Conversion
    /// errors are classified at the validate-before-consume boundary, while
    /// the parked continuation is still available to retry or abort.
    pub fn resume_response(
        &mut self,
        hole: ResidentHole,
        answer: Response,
    ) -> Result<ResidentOutcome, ResidentError> {
        let seed = hole.seed();
        let id = match hole {
            ResidentHole::Plain(h) => h.id,
            ResidentHole::Binding(h) => h.id,
            ResidentHole::ProjectedBinding(h) => h.id,
        };
        self.reenter(&id, ResidentResumeInput::Response(answer), seed, None)
    }

    /// Abort the suspended turn WITHOUT running the continuation — the ask
    /// itself fails (byte-identically to the engine's stowed-abort path). Same
    /// validate-before-consume as [`Self::resume`]. Keyed by the raw
    /// continuation id (not a [`ResidentHole`]) — an abort never materializes
    /// a bind regardless of the hole's own kind, so it carries no obligation
    /// to preserve.
    pub fn abort(
        &mut self,
        cont_id: &str,
        reason: String,
    ) -> Result<ResidentOutcome, ResidentError> {
        self.reenter(
            cont_id,
            ResidentResumeInput::Abort(reason),
            HoleSeed::Plain,
            None,
        )
    }

    fn reenter(
        &mut self,
        cont_id: &str,
        input: ResidentResumeInput,
        seed: HoleSeed,
        additional_provenance: Option<&ProgramProvenance>,
    ) -> Result<ResidentOutcome, ResidentError> {
        // Validate BEFORE consuming: `cont_id` must be a MEMBER of the parked
        // set (any-order resume — the machine imposes no order and neither do
        // we). A mismatch leaves every parked frame intact.
        let Some(&(_, frame_id)) = self.parked.iter().find(|(h, _)| h == cont_id) else {
            return Err(ResidentError::WrongContinuation {
                attempted: cont_id.to_string(),
                pending: self.parked.iter().map(|(h, _)| h.clone()).collect(),
            });
        };
        let mut provenance = self
            .parked_provenance
            .get(&frame_id)
            .map(|value| (**value).clone())
            .unwrap_or_default();
        if let Some(additional) = additional_provenance {
            provenance.merge(additional)?;
        }
        let provenance = Arc::new(provenance);
        // The machine is authoritative on whether the frame was actually
        // consumed: `resume_continuation` NF-forces a data-kinded answer BEFORE
        // removing the frame (A5), and on a retryable rejection leaves it
        // parked and rooted — this hole must NOT be cleared here, or a
        // retryable failure wedges the session. `classify_parked` (on `Ok`)
        // is the sole owner of the parked set on a real outcome. The frame
        // replays its own kind/table/tag, so bind-vs-plain needs no
        // re-declaration here (`bind` is only used for materialization
        // below). Completion handles likewise belong to the frame's retained
        // realm, which must be captured before resume consumes that frame.
        self.reenter_prepared(cont_id, frame_id, input, seed, provenance)
    }

    /// Move the machine onto a stack-sized eval thread, run `body`, and move the
    /// machine back. The threadless mechanism's `run_fragment`/`resume`
    /// re-install the machine's per-thread
    /// reach and re-point GC state at the retained heap. Only the machine (and
    /// the accumulated table) crosses to the thread; the rest of the session
    /// core is `!Send` (raw-pointer roots) and stays here.
    ///
    /// The machine is taken via [`PersistentSession::lease_machine`], whose
    /// [`super::MachineLease`] restores it into `self.state` on EVERY exit from
    /// this function — success, an effect error, a caught panic, or a failed
    /// thread spawn (a transient OS resource failure, not a bug) — so no path
    /// can leave the session permanently machineless.
    fn on_eval_thread<F, T>(&mut self, body: F) -> Result<T, ResidentError>
    where
        T: Send,
        F: FnOnce(
                &mut super::prepared::PreparedEngine,
                &DataConTable,
                &mut H,
                &O,
            ) -> Result<T, EffectError>
            + Send,
    {
        self.on_eval_thread_with_stack(EVAL_STACK_SIZE, body)
    }

    /// [`Self::on_eval_thread`], with the eval thread's stack size as a
    /// parameter rather than the hardcoded [`EVAL_STACK_SIZE`] — split out
    /// so a test can force `spawn_scoped` to fail deterministically (an
    /// absurd stack size) without changing production eval-thread semantics,
    /// which always go through [`Self::on_eval_thread`]'s fixed constant.
    fn on_eval_thread_with_stack<F, T>(
        &mut self,
        stack_size: usize,
        body: F,
    ) -> Result<T, ResidentError>
    where
        T: Send,
        F: FnOnce(
                &mut super::prepared::PreparedEngine,
                &DataConTable,
                &mut H,
                &O,
            ) -> Result<T, EffectError>
            + Send,
    {
        self.settle_dropped_custody();
        let mut lease = self.state.lease_machine();
        let (machine_ref, table) = lease.parts();
        let handlers = &mut self.handlers;
        // The sink is Arc-backed (`OutputSink: Clone + Send`) and shares its
        // buffer; move a clone onto the thread rather than requiring `O: Sync`
        // for a borrow — matches the oneshot engine's `captured.clone()`.
        let captured = self.captured.clone();

        // A scoped thread borrows `machine_ref`/`handlers`/`table`/`captured`
        // from this frame. `EVAL_STACK_SIZE` matches the oneshot eval thread
        // (deep JIT recursion needs it), so `Builder::spawn_scoped` (the
        // stack-sized form of `scope.spawn`) is used. Unlike the oneshot form,
        // a failed spawn here is reported through `outcome`, not `.expect()` —
        // `guard` is still alive and restores the machine either way.
        let outcome = std::thread::scope(|scope| {
            match std::thread::Builder::new()
                .name("tidepool-resident-eval".into())
                .stack_size(stack_size)
                .spawn_scoped(scope, || {
                    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                        body(machine_ref, table, handlers, &captured)
                    }))
                }) {
                Ok(handle) => match handle.join() {
                    Ok(Ok(body_result)) => EvalThreadOutcome::Ran(body_result),
                    Ok(Err(panic)) => EvalThreadOutcome::Panicked(panic),
                    Err(join_panic) => EvalThreadOutcome::Panicked(join_panic),
                },
                Err(spawn_err) => EvalThreadOutcome::SpawnFailed(spawn_err),
            }
        });

        // `lease` drops here (function-end, on every path above), restoring
        // the machine into `self.state` regardless of how `outcome` resolved.
        match outcome {
            EvalThreadOutcome::Ran(Ok(t)) => Ok(t),
            EvalThreadOutcome::Ran(Err(e)) => Err(ResidentError::Run(RuntimeError::Jit(e))),
            EvalThreadOutcome::Panicked(payload) => Err(panic_to_run_error(payload)),
            EvalThreadOutcome::SpawnFailed(e) => Err(ResidentError::EvalThread(e)),
        }
    }

    /// Release affine roots whose custody was dropped while the machine was
    /// checked into a registry or otherwise unavailable to the token itself.
    fn settle_dropped_custody(&mut self) -> usize {
        let leases = std::mem::take(&mut *self.custody_cleanup.binding_leases.lock());
        let mut released = Vec::new();
        for retained in leases {
            released.extend(self.state.bindings_mut().release_leases(retained));
        }
        released.extend(self.state.bindings_mut().collect_observations());
        let binding_count = self.state.release_binding_roots(released);
        self.host_text_bindings
            .retain(|id, _| self.state.bindings().get(*id).is_some());
        self.hidden_host_bindings
            .retain(|id, _| self.state.bindings().get(*id).is_some());
        let handles = self.custody_cleanup.take_all();
        let count = handles.len();
        if let Some(engine) = self.state.prepared_mut() {
            for handle in handles {
                engine.discard_handle(handle);
            }
        }
        count + binding_count
    }

    /// Classify a projected parked outcome into a [`ResidentOutcome`]:
    /// completion retires `resumed` (the hole this outcome answered — `None`
    /// for a fresh run, which retires nothing), suspension mints a hole of
    /// `seed`'s obligation and pushes `(id string, id)` onto the parked set.
    /// Output is drained on completion and snapshotted on suspension, same as
    /// the engine.
    fn classify_parked(
        &mut self,
        outcome: ParkedRun,
        resumed: Option<&str>,
        seed: HoleSeed,
        provenance: Arc<ProgramProvenance>,
    ) -> ResidentOutcome {
        match outcome {
            ParkedRun::CompletedValue(value) => {
                self.retire_resumed(resumed);
                let output = self.captured.drain();
                ResidentOutcome::Completed {
                    output,
                    result: EvalResult::new(value, self.state.session_table().clone(), Vec::new()),
                }
            }
            ParkedRun::CompletedProject => {
                self.retire_resumed(resumed);
                ResidentOutcome::BindingsCommitted {
                    output: self.captured.drain(),
                }
            }
            ParkedRun::Suspended { id, request } => {
                // A resume that re-suspended: the OLD hole is spent (the
                // frame was consumed; a fresh frame parked under a FRESH id —
                // ids are never reused) and the new one replaces it.
                self.retire_resumed(resumed);
                let cont_id = self.next_cont_id();
                self.parked.push((cont_id.clone(), id));
                self.parked_provenance.insert(id, provenance);
                let output = self.captured.snapshot();
                ResidentOutcome::Suspended {
                    output,
                    hole: ResidentHole::mint(cont_id, seed),
                    request,
                }
            }
        }
    }

    /// After a failed re-entry, reconcile the hole against the machine's
    /// ground truth BY IDENTITY: if the frame is gone from the registry, it
    /// WAS consumed before the failure (a genuine mid-run error, or an abort)
    /// and the hole is spent. If it is still parked, this was a retryable
    /// rejection by the prepared validator and the hole stays
    /// untouched. A boolean "is the machine suspended" cannot answer this
    /// with N frames parked; membership can.
    fn reconcile_failed_reentry(&mut self, cont_id: &str, frame_id: ContinuationId) {
        let still_parked = self.state.parked_ids().contains(&frame_id);
        if !still_parked {
            self.parked_provenance.remove(&frame_id);
            self.parked.retain(|(h, _)| h != cont_id);
        }
    }

    /// The prepared arm of [`Self::reenter`]. A host-built `Answer` is
    /// validated against the frame's site evidence and built before the frame
    /// is taken, then the runner's resume entry re-enters the continuation
    /// and the settled layer finishes through the same routine as the initial
    /// run ([`finish_prepared`], [`Self::complete_prepared`]); a refused
    /// answer leaves the frame parked and the hole open. `Abort` consumes the
    /// frame without entering it and fails the ask through the normal abort contract.
    /// `Handle`/`FramedHandle` deliver an already-retained value by borrow,
    /// framed-custody delivery (`docs/continuation-parking-contract.md`): the handle's root
    /// is untouched by the resume either way.
    fn reenter_prepared(
        &mut self,
        cont_id: &str,
        frame_id: ContinuationId,
        input: ResidentResumeInput,
        seed: HoleSeed,
        provenance: Arc<ProgramProvenance>,
    ) -> Result<ResidentOutcome, ResidentError> {
        let input = match input {
            ResidentResumeInput::Abort(reason) => {
                let aborted = self.on_eval_thread(move |engine, _table, _handlers, _captured| {
                    Ok(engine.abort_parked(frame_id))
                });
                self.reconcile_failed_reentry(cont_id, frame_id);
                aborted??;
                return Err(ResidentError::Run(RuntimeError::Jit(EffectError::Handler(
                    format!("ask aborted by caller: {reason}"),
                ))));
            }
            other => other,
        };
        // The hole's own obligation says how the resumed run completes; the
        // frame carries the runner whose entry re-enters the continuation.
        let (lexical_scope, mode) = match &seed {
            HoleSeed::Plain => (self.run_context.lexical_scope, PreparedTurnMode::Value),
            HoleSeed::Binding {
                binder,
                generation,
                observation,
                lexical_scope,
            } => (
                *lexical_scope,
                PreparedTurnMode::Binding {
                    binder,
                    generation: *generation,
                    observation: observation.clone(),
                },
            ),
            HoleSeed::ProjectedBinding {
                binders,
                generation,
                lexical_scope,
            } => (
                *lexical_scope,
                PreparedTurnMode::Projected {
                    binders,
                    generation: *generation,
                },
            ),
        };
        let plan = settle_plan_of(&mode);
        let park = ParkPolicy {
            principal: self.run_context.principal,
            effect_policy: self.state.effect_policy(),
            live_payload: self.state.live_payload_policy(),
        };
        let resumed = self.on_eval_thread(move |engine, table, handlers, captured| {
            let outcome = match input {
                ResidentResumeInput::Response(response) => {
                    engine.resume_with_structural_answer(frame_id, &response, table)
                }
                ResidentResumeInput::Handle(handle) => engine.resume_with_handle(frame_id, handle),
                ResidentResumeInput::FramedHandle {
                    handle,
                    constructor,
                    prefix,
                } => engine.resume_with_framed_handle(frame_id, handle, constructor, prefix, table),
                ResidentResumeInput::Abort(_) => {
                    unreachable!("Abort is handled before the frame is touched")
                }
            };
            Ok(outcome.and_then(|resumed| {
                let runner = resumed.runner;
                finish_prepared(
                    engine,
                    runner,
                    resumed.realm,
                    plan,
                    park,
                    table,
                    handlers,
                    captured,
                    resumed.settlement,
                )
                .map(|run| (runner, run))
            }))
        });
        let (runner, run) = match resumed {
            Ok(Ok(resumed)) => resumed,
            Ok(Err(error)) => {
                self.reconcile_failed_reentry(cont_id, frame_id);
                return Err(error.into());
            }
            Err(error) => {
                self.reconcile_failed_reentry(cont_id, frame_id);
                return Err(error);
            }
        };
        self.complete_prepared(run, mode, runner, lexical_scope, provenance, Some(cont_id))
    }

    /// The `ValueHandle` custody of a prepared-route binding named `name`,
    /// for delivering an already-bound value into another parked frame by
    /// handle ([`Self::resume_handle`]/[`Self::resume_framed_custody`]) —
    /// [`Self::reenter_prepared`]'s `Handle`/`FramedHandle` branches' test
    /// surface. Reuses `BoundValue`'s own linking handle rather than minting a fresh one, so
    /// custody moves without disturbing the binding's root -- and, because
    /// the binding table (not this token) is the handle's real owner, the
    /// returned custody is [`RootCustody::shared`]: a caller that only ever
    /// borrows it (`resume_framed_custody`'s `&RootCustody`) and then drops
    /// it leaves `name`'s binding exactly as it was, same as never calling
    /// this at all. Returns `None` for an unknown binding.
    pub fn prepared_binding_handle(&self, name: &str) -> Option<RootCustody> {
        let entry = self.state.bindings().resolve(name)?;
        let BoundValue { handle, .. } = &entry.value;
        Some(RootCustody::shared(
            handle.raw(),
            Arc::clone(&self.custody_cleanup),
            Arc::new(ProgramProvenance::default()),
        ))
    }

    fn retire_resumed(&mut self, resumed: Option<&str>) {
        let Some(hole) = resumed else {
            return;
        };
        if let Some((_, id)) = self.parked.iter().find(|(name, _)| name == hole) {
            self.parked_provenance.remove(id);
        }
        self.parked.retain(|(name, _)| name != hole);
    }
}

#[cfg(test)]
mod host_binding_authority_tests {
    use super::*;
    use crate::NominalHead;

    #[test]
    fn same_name_from_an_untrusted_unit_cannot_mount_as_json() {
        let binder = BoundBinder {
            name: "input".into(),
            var_id: 1,
            module: "Tidepool.Session.Val.G1".into(),
            tier: ValueTier::ForceData,
            type_display: "Tidepool.Aeson.Value.Value".into(),
            root_head: Some(NominalHead {
                unit: "shadowed-value-0.1".into(),
                module: "Tidepool.Aeson.Value".into(),
                name: "Value".into(),
            }),
            // The extractor refuses to mint JsonValue for the wrong unit.
            host_authority: None,
        };
        assert!(require_host_binding_authority(&binder, HostBindingType::JSON_VALUE).is_err());
    }
}

/// The kernel's generalized suspension seam
/// ([`super::kernel::SuspendableSession`]), implemented directly against this
/// session's own [`Self::resume`]/[`Self::abort`]. This type already has the
/// shape the kernel requires: one obligation-carrying [`ResidentHole`] token
/// and one resume/abort entry point per token. `Context = ()`: this session owns
/// its captured-output buffer and handler stack as fields, so a call needs
/// nothing extra beyond the hole and the answer.
impl<H, O> super::kernel::SuspendableSession for ResidentSession<H, O>
where
    H: DispatchEffect<O> + Send,
    O: OutputSink + Sync,
{
    type Hole = ResidentHole;
    type Answer = HaskellValue;
    type Context = ();
    type Outcome = ResidentOutcome;
    type Error = ResidentError;

    fn resume(
        &mut self,
        hole: Self::Hole,
        answer: Self::Answer,
        (): Self::Context,
    ) -> Result<Self::Outcome, Self::Error> {
        Self::resume(self, hole, answer)
    }

    fn abort(
        &mut self,
        hole: Self::Hole,
        reason: String,
        (): Self::Context,
    ) -> Result<Self::Outcome, Self::Error> {
        Self::abort(self, hole.cont_id(), reason)
    }
}

/// The `Send` result that crosses the eval-thread boundary: a bind's tenured
/// `!Send` `RootSlot` is minted into a
/// [`ValueHandle`] IN-THREAD (`realm`-owned) and the id crosses instead —
/// resolved back to its slot by `materialize_binder` on the session thread.
/// `CompletedProject` is the multi-binder lane; `CompletedRender` remains
/// unreachable because resident turns do not use render parking. Live-payload
/// presence is dropped because the payload itself is acquired explicitly from
/// its frame.
enum ParkedRun {
    CompletedValue(HaskellValue),
    CompletedProject,
    Suspended {
        id: ContinuationId,
        request: HaskellValue,
    },
}

/// The three ways a resident eval thread's lifecycle can resolve — spawn
/// failure, a caught panic, or a completed run of `body` (itself carrying its
/// own `Result`). Distinct from `SpawnError`/join-panic being conflated into
/// one `.expect()`, which is exactly what let a spawn failure escape as an
/// unguarded panic.
enum EvalThreadOutcome<T> {
    Ran(Result<T, EffectError>),
    Panicked(Box<dyn std::any::Any + Send>),
    SpawnFailed(std::io::Error),
}

/// Map a caught panic payload (a Rust-level fault that unwound past the JIT's
/// own `with_signal_protection` — a genuine bug, not a language-level error) to
/// a run error carrying the payload string.
fn panic_to_run_error(payload: Box<dyn std::any::Any + Send>) -> ResidentError {
    let detail = crate::panic_payload_message(payload);
    ResidentError::Run(RuntimeError::Jit(EffectError::Handler(format!(
        "resident turn panicked: {detail}"
    ))))
}
