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
use tidepool_codegen::prepared_program::{
    session_var_id, ImageRegistry, Parcel, PreparedHandle, PreparedOuter, PreparedResult, ProgramId,
};
use tidepool_repr::execution_schema::{JsonLayout, PreparedProgram, SymbolIdentity};

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
use crate::{NominalHead, RuntimeError, YieldSite, YieldSiteCollision, EVAL_STACK_SIZE};

enum ResidentResumeInput {
    Response(Response),
    Handle(ValueHandle),
    FramedHandle {
        handle: ValueHandle,
        constructor: tidepool_repr::DataConId,
        prefix: Vec<HaskellValue>,
    },
    FramedHandleSources {
        handle: ValueHandle,
        constructor: tidepool_repr::DataConId,
        prefix: Vec<Box<dyn tidepool_bridge::ToHaskell + Send>>,
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
        constructors: &[],
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

/// The generation-independent facts a compiled host-binder shape carries,
/// reused verbatim by every [`HostCarrier`] mount rather than re-derived
/// from a fresh compile.
#[derive(Clone, Debug)]
struct HostCarrierShape {
    tier: ValueTier,
    type_display: String,
    root_head: Option<NominalHead>,
    host_authority: Option<HostBindingAuthority>,
}

/// A compiled host-binder program, built once from a real `(BoundBinder,
/// CompiledTurn)` and mounted under as many fresh names/generations as the
/// caller needs with no further GHC compile.
///
/// A compiled binder's `table`/`prepared` carry no generation-specific fact
/// (see [`Self::from_compiled`]); only the binder's `name`, `module`
/// (`Val.G<gen>`), and `var_id` are per-mount. [`ResidentSession::mount_carrier_in`]
/// mints those three from `name`/`gen` with
/// [`tidepool_codegen::prepared_program::session_var_id`] instead of asking
/// GHC to mint a fresh binder.
pub struct HostCarrier {
    table: DataConTable,
    prepared: PreparedProgram,
    shape: HostCarrierShape,
    host_type: HostBindingType,
}

impl HostCarrier {
    /// Build a carrier from one real compiled `(BoundBinder, CompiledTurn)`
    /// (any generation — its own module/var_id are discarded; only the
    /// binder's shape and the turn's table/program are kept). `host_type`
    /// is the authenticated host surface this binder was compiled against
    /// ([`Self::mount_carrier_in`]'s validation target).
    #[must_use]
    pub fn from_compiled(
        binder: &BoundBinder,
        code: TurnCode<'_>,
        host_type: HostBindingType,
    ) -> Self {
        HostCarrier {
            table: code.table.into_owned(),
            prepared: code.prepared.into_owned(),
            shape: HostCarrierShape {
                tier: binder.tier,
                type_display: binder.type_display.clone(),
                root_head: binder.root_head.clone(),
                host_authority: binder.host_authority,
            },
            host_type,
        }
    }

    fn code(&self) -> TurnCode<'_> {
        TurnCode {
            table: std::borrow::Cow::Borrowed(&self.table),
            sites: std::borrow::Cow::Borrowed(&[]),
            prepared: std::borrow::Cow::Borrowed(&self.prepared),
        }
    }

    /// The hand-written source stub a mount under `module_name`/`binder_name`
    /// writes at `<session_root>/<relative_hs_path>` — an ordinary home
    /// module GHC's downsweep finds on the session include path, never
    /// passed as `--inject-val` (it has no `.hi`). Its self-referential
    /// `NOINLINE` body is never evaluated: only the binder's `Name` (and
    /// through it, its `stableVarId`) is real; the persistent binding store
    /// supplies the actual value at link time.
    ///
    /// The `GHC.Magic.lazy` wrapper is load-bearing, not cosmetic. The
    /// session pipeline compiles every home module (this stub included) at
    /// `OptimizeEveryModule`, and a bare self-reference `x = x` at that tier
    /// gets a bottoming strictness signature from GHC's demand analysis (it
    /// provably loops if forced). A reference turn compiled in the same
    /// multi-module session sees that signature -- no cross-module
    /// unfolding needed for it, a signature alone drives case-of-bottom --
    /// and simplifies a later `case binder_name of { Con ... }` down to
    /// just `binder_name`, dropping every alternative; at runtime
    /// `binder_name` resolves to the real, non-bottoming retained value and
    /// the dropped case has nothing to dispatch to (a JIT `CaseMiss`).
    /// `OPTIONS_GHC -fomit-interface-pragmas` on the stub does NOT fix this:
    /// `canonicalizeDFlags`'s unconditional `updOptLevel 2` (re-applied per
    /// module, `GhcPipeline.hs`) resets `Opt_OmitInterfacePragmas` back to
    /// its `-O2` default regardless of a per-module pragma requesting
    /// otherwise. `GHC.Magic.lazy` instead defeats the *inference itself*:
    /// it is a standard, safe (no `unsafeCoerce`) library primitive whose
    /// specific purpose is to make demand analysis not see through an
    /// expression, so `x = lazy x` never earns a bottoming signature to
    /// begin with, regardless of optimization tier.
    fn stub_source(&self, module_name: &str, binder_name: &str) -> String {
        format!(
            "module {module_name} ({binder_name}) where\n\
             import qualified {ty_module} as TidepoolCarrierType\n\
             import qualified GHC.Magic as TidepoolCarrierMagic\n\
             {{-# NOINLINE {binder_name} #-}}\n\
             {binder_name} :: TidepoolCarrierType.{ty_name}\n\
             {binder_name} = TidepoolCarrierMagic.lazy {binder_name}\n",
            ty_module = self.host_type.module,
            ty_name = self.host_type.name,
        )
    }
}

/// One host value to mount through a [`HostCarrier`]. Mirrors the payload
/// shapes [`ResidentSession::mount_json_binding_in`],
/// [`ResidentSession::mount_text_binding_in`], and
/// [`ResidentSession::mount_typed_binding_in`] already accept.
pub enum HostPayload<'a> {
    Json(&'a serde_json::Value),
    Text(&'a str),
    Job(&'a dyn tidepool_bridge::ToHaskell),
}

fn json_runtime_layout_optional(prepared: &PreparedProgram) -> Option<JsonLayout<DataConId>> {
    prepared.json_layout().and_then(|layout| {
        let constructors = prepared.constructors();
        (*layout)
            .try_map(|constructor| {
                constructors
                    .get(constructor.0 as usize)
                    .map(|row| row.host_id)
                    .ok_or(())
            })
            .ok()
    })
}

fn json_runtime_layout(prepared: &PreparedProgram) -> Result<JsonLayout<DataConId>, ResidentError> {
    json_runtime_layout_optional(prepared).ok_or_else(|| {
        ResidentError::Run(RuntimeError::Jit(EffectError::Handler(
            "compiled host mount has no authenticated JSON layout".into(),
        )))
    })
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

/// Exclusive owned handle for one machine-rooted value.
///
/// The session creates this handle when a finalized value leaves a parked frame.
/// Consuming operations may deliver it once, adopt it into a binding, move it
/// to another resource scope, or discard it. Raw [`ValueHandle`] access stays
/// inside this module, so external callers cannot duplicate an ownership
/// token through a numeric ID. Dropping the handle queues its root for release
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
    /// (non-shared) handle's whole contract is "abandon it and its root is
    /// released" -- exactly wrong for an alias, since abandoning the ALIAS
    /// must not touch the root the other owner still needs. Sharing only
    /// changes what an unconsumed drop does; every consuming operation
    /// (delivery, mount, discard) behaves exactly as it does for an
    /// exclusive ownership.
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
/// Drop uses the session's handle cleanup queue, including cancellation before
/// prepared work is returned. Reclamation occurs on the next session entry or
/// at teardown, as it does for a dropped value handle.
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
    /// same as an ordinary abandoned handle, so this must agree.
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
/// a binding turn's hole must materialize its binder into the persistent binding store on
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
    /// dropping its binding-store materialization — is still impossible
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
/// The page itself is installed in the persistent binding store before this metadata is
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
    /// A persistent declaration environment operation failed while materializing a value bind — the
    /// cross-store shadow retract (a value bind evicting a same-name declaration head).
    #[error(transparent)]
    Session(#[from] SessionError),
    /// The prepared engine refused or failed the turn.
    #[error("prepared engine: {0}")]
    Prepared(#[from] PreparedRuntimeError),
}

impl ResidentError {
    /// Whether this failure is only an observation budget running out
    /// somewhere in the turn — materializing a display value, or observing a
    /// suspended effect's own request so it can be classified and dispatched
    /// — rather than a fault in the program, the heap, or the value. A
    /// caller that only needs to know whether the size limit tripped (as
    /// opposed to reading the specific failure) uses this instead of
    /// matching the `Prepared`/`Run`/`Observation` chain itself.
    pub fn is_observation_budget_exhausted(&self) -> bool {
        matches!(self, Self::Prepared(error) if is_observation_budget_exhausted(error))
    }
}

/// A failed continuation response classified by the parked frame's ground
/// truth. `Rejected` leaves the original frame available for retry or abort;
/// `Consumed` means delivery crossed the continuation boundary before the
/// resumed computation failed.
#[derive(Debug, thiserror::Error)]
pub enum ResidentResumeError {
    #[error("resident response was rejected before consuming its continuation: {0}")]
    Rejected(ResidentError),
    #[error("resident computation failed after consuming its response: {0}")]
    Consumed(ResidentError),
}

impl ResidentResumeError {
    pub fn into_inner(self) -> ResidentError {
        match self {
            Self::Rejected(error) | Self::Consumed(error) => error,
        }
    }
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
    /// Observe the value and bind it into the persistent binding store as `binder`.
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

/// Owned counterpart of [`PreparedTurnMode`], built from data the caller
/// already owns (a `TurnResult::Bind`'s `bound`/`generation`), so it can
/// cross the async gap between [`ResidentSession::snapshot_run_prepared`]'s
/// checkout and [`ResidentSession::revalidate_and_run_prepared`]'s. `as_mode`
/// borrows back into a [`PreparedTurnMode`] at the point of use, exactly as
/// a caller of the single-checkout [`ResidentSession::run_prepared`] would
/// have constructed one directly.
pub enum PendingPreparedMode {
    Value,
    Binding {
        binder: BoundBinder,
        generation: Generation,
        observation: Option<Vec<tidepool_repr::VarId>>,
    },
    Projected {
        binders: Vec<BoundBinder>,
        generation: Generation,
    },
}

impl PendingPreparedMode {
    fn as_mode(&self) -> PreparedTurnMode<'_> {
        match self {
            PendingPreparedMode::Value => PreparedTurnMode::Value,
            PendingPreparedMode::Binding {
                binder,
                generation,
                observation,
            } => PreparedTurnMode::Binding {
                binder,
                generation: *generation,
                observation: observation.clone(),
            },
            PendingPreparedMode::Projected {
                binders,
                generation,
            } => PreparedTurnMode::Projected {
                binders,
                generation: *generation,
            },
        }
    }

    fn generation(&self) -> Option<Generation> {
        match self {
            PendingPreparedMode::Value => None,
            PendingPreparedMode::Binding { generation, .. }
            | PendingPreparedMode::Projected { generation, .. } => Some(*generation),
        }
    }
}

/// Everything [`ResidentSession::snapshot_run_prepared`] produces under a
/// machine checkout for an off-checkout Cranelift compile: owned data with
/// no reference to the machine or its checkout, `Send` so it can cross the
/// gap to a blocking-pool compile and back. Finish it with
/// [`Self::compile_off_checkout`] then
/// [`ResidentSession::revalidate_and_run_prepared`].
pub struct PendingPreparedInstall {
    snapshot: super::prepared::InstallSnapshot,
    mode: PendingPreparedMode,
    argument: Option<PreparedHandle>,
    provenance: Arc<ProgramProvenance>,
    realm: RealmId,
    lexical_scope: ScopeId,
    park: ParkPolicy,
}

impl PendingPreparedInstall {
    /// Step (b): compile this pending install's linked program off any
    /// checkout. No machine access; safe to run on a blocking thread while
    /// other turns hold the checkout this snapshot was taken under.
    pub fn compile_off_checkout(
        &mut self,
    ) -> Result<
        std::sync::Arc<tidepool_codegen::prepared_program::CompiledProgram>,
        tidepool_codegen::prepared_program::CompileError,
    > {
        super::prepared::PreparedEngine::compile_off_checkout(&mut self.snapshot)
    }
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
    /// lazy until the page has entered the persistent binding store.
    Display(ValueTier),
}

/// How deep [`render_retained_layer`] recurses into nested constructors
/// before cutting with `…`, independent of the character budget -- bounds
/// the walk against a deeply nested value even when each layer prints short.
const RETAINED_PREVIEW_MAX_DEPTH: usize = 8;

/// Note [`truncate_preview_at_line`] appends after a cut, so a reader never
/// mistakes a truncated preview for the whole value.
const PREVIEW_TRUNCATED_NOTE: &str = "\n[reply preview truncated]";

/// Append `text` to `out`, spending it from `budget` one byte at a time; once
/// `budget` reaches zero the remainder is dropped and `…` is appended in its
/// place (once -- repeated calls after exhaustion append nothing further).
/// This is only the walk's OWN safety valve against runaway work; the
/// caller-facing budget is enforced once more, at a line boundary, by
/// [`truncate_preview_at_line`].
fn push_bounded(out: &mut String, text: &str, budget: &mut usize) {
    if *budget == 0 {
        return;
    }
    if text.len() <= *budget {
        out.push_str(text);
        *budget -= text.len();
    } else {
        let mut cut = *budget;
        while cut > 0 && !text.is_char_boundary(cut) {
            cut -= 1;
        }
        out.push_str(&text[..cut]);
        out.push('…');
        *budget = 0;
    }
}

/// Enforce `budget` characters (bytes) on a finished preview, cutting at the
/// last line boundary at or before the limit rather than mid-line, and
/// appending [`PREVIEW_TRUNCATED_NOTE`] when anything was cut. A value with
/// no newline before `budget` cuts at the nearest earlier char boundary
/// instead -- still bounded, just without a line to cut at.
///
/// The one truncation implementation for a settlement notice's reply
/// preview, whichever side rendered the untruncated text: this session's own
/// non-forcing retained-heap walk, or a preview the replying Haskell program
/// rendered itself (`Tidepool.Agent.Reply.Internal.reply`) and carried
/// across the boundary untruncated.
pub fn truncate_preview_at_line(text: String, budget: usize) -> String {
    if text.len() <= budget {
        return text;
    }
    let mut cut = text[..budget].rfind('\n').unwrap_or(budget);
    while cut > 0 && !text.is_char_boundary(cut) {
        cut -= 1;
    }
    let mut truncated = text[..cut].to_string();
    truncated.push_str(PREVIEW_TRUNCATED_NOTE);
    truncated
}

/// How many list elements [`render_char_list_string`] decodes before
/// stopping regardless of budget -- a runaway spine (a lazily unfolding
/// infinite list, say) must not walk forever even under a generous budget.
const RETAINED_PREVIEW_MAX_LIST_ELEMENTS: usize = 4096;

/// One layer of [`ResidentSession::render_retained_preview`]'s walk: read
/// `handle`'s constructor through [`super::prepared::PreparedEngine::inspect_retained`]
/// and append its rendering to `out`. Every field handle this call mints is
/// released before it returns, whether or not the field was itself entered.
fn render_retained_layer(
    engine: &mut super::prepared::PreparedEngine,
    table: &DataConTable,
    handle: ValueHandle,
    depth: usize,
    budget: &mut usize,
    out: &mut String,
) {
    if *budget == 0 {
        return;
    }
    if depth > RETAINED_PREVIEW_MAX_DEPTH {
        push_bounded(out, "…", budget);
        return;
    }
    let PreparedOuter::Constructor { identity, fields } = match engine.inspect_retained(handle) {
        Ok(outer) => outer,
        Err(_) => {
            push_bounded(out, "…", budget);
            return;
        }
    };
    let name = table
        .get(identity)
        .map(|dc| dc.name.as_str())
        .unwrap_or("?")
        .to_string();
    // `String`/`[Char]` is the common "text" reply shape this walk can
    // actually read: unlike `Data.Text` (a packed byte array behind a
    // Constructor this walk cannot see into), a Haskell string is ordinary
    // cons cells over boxed `Char`s, so it prints as one quoted string
    // instead of a wall of nested `(: 'o' (: 'r' ...))`.
    if name == ":" && fields.len() == 2 {
        let [head, tail]: [_; 2] = match fields.try_into() {
            Ok(pair) => pair,
            Err(_) => {
                push_bounded(out, "?", budget);
                return;
            }
        };
        if is_char_element(engine, table, &head) {
            render_char_list_string(engine, table, head, tail, budget, out);
            return;
        }
        push_bounded(out, "(", budget);
        push_bounded(out, &name, budget);
        push_bounded(out, " ", budget);
        render_field(engine, table, head, depth + 1, budget, out);
        push_bounded(out, " ", budget);
        render_field(engine, table, tail, depth + 1, budget, out);
        push_bounded(out, ")", budget);
        return;
    }
    if fields.is_empty() {
        push_bounded(out, &name, budget);
        return;
    }
    // A boxed primitive literal (`I# 3`, `C# 'a'`, `W# 7`) prints as its bare
    // scalar: these are the common case in an ordinary reply value, and the
    // wrapper constructor name is noise a reader of the preview never wants.
    if let [PreparedResult::Scalar(word)] = fields.as_slice() {
        match name.as_str() {
            "I#" => {
                push_bounded(out, &(*word as i64).to_string(), budget);
                return;
            }
            "W#" => {
                push_bounded(out, &word.to_string(), budget);
                return;
            }
            "C#" => {
                push_bounded(out, &render_char_literal(*word), budget);
                return;
            }
            _ => {}
        }
    }
    push_bounded(out, "(", budget);
    push_bounded(out, &name, budget);
    for field in fields {
        push_bounded(out, " ", budget);
        if *budget == 0 {
            break;
        }
        render_field(engine, table, field, depth + 1, budget, out);
    }
    push_bounded(out, ")", budget);
}

/// Render one already-classified field: a scalar prints as its bare word, a
/// managed field recurses through [`render_retained_layer`] and releases the
/// fresh handle [`super::prepared::PreparedEngine::inspect_retained`] minted
/// for it once that recursion returns.
fn render_field(
    engine: &mut super::prepared::PreparedEngine,
    table: &DataConTable,
    field: PreparedResult,
    depth: usize,
    budget: &mut usize,
    out: &mut String,
) {
    match field {
        PreparedResult::Void => push_bounded(out, "()", budget),
        PreparedResult::Scalar(word) => push_bounded(out, &word.to_string(), budget),
        PreparedResult::Managed(field_handle) => {
            let raw = field_handle.raw();
            render_retained_layer(engine, table, raw, depth, budget, out);
            engine.release(field_handle);
        }
    }
}

fn render_char_literal(word: u64) -> String {
    char::from_u32(word as u32)
        .map(|c| format!("{c:?}"))
        .unwrap_or_else(|| word.to_string())
}

/// Whether `field` is a boxed `Char` (`C#`) -- a single peek at its
/// constructor, releasing anything that peek minted. Used only to decide
/// whether a cons cell opens a string; [`render_char_list_string`] repeats
/// the read for the elements it actually prints.
fn is_char_element(
    engine: &mut super::prepared::PreparedEngine,
    table: &DataConTable,
    field: &PreparedResult,
) -> bool {
    let PreparedResult::Managed(handle) = field else {
        return false;
    };
    match engine.inspect_retained(handle.raw()) {
        Ok(PreparedOuter::Constructor { identity, fields }) => {
            let is_char =
                fields.len() == 1 && table.get(identity).map(|dc| dc.name.as_str()) == Some("C#");
            for field in fields {
                if let PreparedResult::Managed(minted) = field {
                    engine.release(minted);
                }
            }
            is_char
        }
        Err(_) => false,
    }
}

/// Render a `:`-spine starting at `head`/`tail` (already known, by
/// [`is_char_element`], to open on a `Char`) as one quoted Haskell string.
/// Stops -- marking the cut with a trailing `…` inside the quotes -- at the
/// budget, [`RETAINED_PREVIEW_MAX_LIST_ELEMENTS`], the proper `[]` end, or
/// the first element that turns out not to be a `Char` after all (a
/// heterogeneous or partially-forced list this walk does not force to
/// check). Every handle this walk mints along the spine is released.
fn render_char_list_string(
    engine: &mut super::prepared::PreparedEngine,
    table: &DataConTable,
    head: PreparedResult,
    mut tail: PreparedResult,
    budget: &mut usize,
    out: &mut String,
) {
    push_bounded(out, "\"", budget);
    let mut next_head = Some(head);
    let mut count = 0usize;
    loop {
        let Some(PreparedResult::Managed(handle)) = next_head.take() else {
            break;
        };
        let stop = match engine.inspect_retained(handle.raw()) {
            Ok(PreparedOuter::Constructor { identity, fields }) => {
                let is_char = table.get(identity).map(|dc| dc.name.as_str()) == Some("C#");
                let mut stop = !is_char;
                if let [PreparedResult::Scalar(word)] = fields.as_slice() {
                    if is_char {
                        match char::from_u32(*word as u32) {
                            Some('"') => push_bounded(out, "\\\"", budget),
                            Some('\\') => push_bounded(out, "\\\\", budget),
                            Some(c) => push_bounded(out, &c.to_string(), budget),
                            None => stop = true,
                        }
                    }
                } else {
                    stop = true;
                }
                engine.release(handle);
                stop
            }
            Err(_) => {
                engine.release(handle);
                true
            }
        };
        count += 1;
        if stop || *budget == 0 || count >= RETAINED_PREVIEW_MAX_LIST_ELEMENTS {
            // The element itself is already released either way; `tail`
            // (still unread on every path here) is released below.
            if count >= RETAINED_PREVIEW_MAX_LIST_ELEMENTS || *budget == 0 {
                push_bounded(out, "…", budget);
            }
            if let PreparedResult::Managed(tail_handle) = tail {
                engine.release(tail_handle);
            }
            break;
        }
        let PreparedResult::Managed(tail_handle) = tail else {
            break;
        };
        match engine.inspect_retained(tail_handle.raw()) {
            Ok(PreparedOuter::Constructor {
                identity: tail_identity,
                fields: tail_fields,
            }) => {
                let tail_name = table
                    .get(tail_identity)
                    .map(|dc| dc.name.as_str())
                    .unwrap_or("?");
                engine.release(tail_handle);
                if let (":", Ok([head, rest])) = (tail_name, <[_; 2]>::try_from(tail_fields)) {
                    next_head = Some(head);
                    tail = rest;
                } else {
                    // The proper `[]` end, or anything else: either way the
                    // spine ends here.
                    break;
                }
            }
            Err(_) => {
                engine.release(tail_handle);
                break;
            }
        }
    }
    push_bounded(out, "\"", budget);
}

/// Run `program`'s settled scaffold on the eval thread and finish it there:
/// a completed value is prepared under `plan`, a suspension is parked under
/// `park`. The invocation's resource-scope cancellation flag governs the run and any forcing
/// observation.
#[allow(clippy::too_many_arguments)]
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
#[allow(clippy::too_many_arguments)]
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
#[allow(clippy::too_many_arguments)]
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
                    if let Err(abort_error) = engine.abort_parked(parked.id) {
                        tracing::warn!(
                            ?abort_error,
                            id = ?parked.id,
                            "failed to abort parked frame after handler error"
                        );
                    }
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
                if let Err(abort_error) = engine.abort_parked(parked.id) {
                    tracing::warn!(
                        ?abort_error,
                        id = ?parked.id,
                        "failed to abort parked frame after resume refusal"
                    );
                }
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
        // discard a run whose effects are already committed. So this always
        // observes under `BudgetPolicy::Bounded`, which stops where the
        // budget runs out and leaves `OVERSIZE_SENTINEL` at each cut instead
        // of failing, rather than a rejection. The handle is kept on that
        // path exactly as on the successful one, so the part the cut omitted
        // stays reachable through the binding the caller installs (a workbench
        // expression is named `observationN` and bound before it is shown).
        //
        // `Bounded` and `Complete` walk identically until a budget-exceeded
        // node is reached — every OTHER observation failure still propagates
        // either way — so there is no second, redone walk here: `Bounded`
        // subsumes `Complete`'s success case in one pass.
        SettlePlan::Observe => match engine.observe_bounded(program, handle) {
            Ok(value) => Ok(PreparedRun::Done { handle, value }),
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
                // Forcing only checks for a genuine error here; the forced
                // value itself is discarded. `Bounded` subsumes `Complete`'s
                // success case, so one pass suffices -- see `SettlePlan::Observe`.
                if let Err(error) = engine.observe_bounded(program, page) {
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
    matches!(error, PreparedRuntimeError::Run(inner) if inner.is_observation_budget_exhausted())
}

/// A resident JIT session: one long-lived [`PreparedEngine`] whose heap and
/// effect state persist across turns.
///
/// Generic over the effect handler stack `H` and the output sink `O` so it
/// stays below the server crate that owns the concrete buffer, exactly like
/// [`super::PreparedEngine`]. The registry (`exomonad-harness`) instantiates
/// `Slot<ResidentSession<H, O>>`.
pub struct ResidentSession<H, O> {
    /// The shared persistent session state (machine + accumulated table + the two
    /// stores). The harness does not (yet) accumulate declarations or value bindings —
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
    /// Deferred releases produced when an affine handle is dropped away from a
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

    /// Share `registry` with this session's machine, once it has one
    /// (`PreparedEngine::set_image_registry`) -- a no-op before the first
    /// turn bootstraps it, since there is no machine yet to share an image
    /// with. The composition root that owns a run's sibling sessions is the
    /// intended caller.
    pub fn set_image_registry(&mut self, registry: Arc<ImageRegistry>) {
        if let Some(engine) = self.state.prepared_mut() {
            engine.set_image_registry(registry);
        }
    }

    /// Accumulate `decls` on the persistent declaration environment (mirrors the repl's
    /// `Session::define_scoped`): a declaration turn appends to the gen-versioned
    /// `Lib.G<g>` module a later turn imports. Requires a persistent declaration environment (`Some(lib)`
    /// at bootstrap). Each node's declaration environment is independent, so a parent's accumulated
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

    /// The checkout-only half of staging a declaration off-checkout: see
    /// [`PersistentSession::render_declaration_candidate_in`].
    pub fn render_declaration_candidate_in(
        &self,
        scope: ScopeId,
        receipt: &super::DeclarationReceipt,
        imports: &super::SourceImports,
    ) -> Result<
        (
            super::DeclarationCandidateRender,
            Vec<(tidepool_repr::SessionVarId, String)>,
        ),
        SessionError,
    > {
        self.state
            .render_declaration_candidate_in(scope, receipt, imports)
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

    /// The current persistent declaration environment module name (`Tidepool.Session.Lib.G<g>`) a later
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

    /// The persistent declaration environment include directory to add to a later turn's compile search
    /// path (so `import Lib.G<g>` resolves), or `None` with no persistent declaration environment.
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
    /// `(0, 0)` when the machine is not yet booted or the resource scope owns
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
    /// so a waiter's handle survives the thread's own resource scope later closing —
    /// see [`ResidentSession::run_rooted_entry`]). Same
    /// frame-stays-parked semantics; `None` under the same conditions.
    /// Returns a [`RootCustody`] token, exactly as [`Self::live_payload_handle`]
    /// does: minting under a different resource scope changes WHO owns the root, never
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

    /// Export the value `custody` roots as a detached [`Parcel`] another
    /// session's machine (sharing this run's [`tidepool_codegen::prepared_program::ImageRegistry`])
    /// can import -- the session-layer half of a value crossing two
    /// [`ResidentSession`]s. Consumes the custody: the export itself is a
    /// non-consuming read of the machine (`PreparedEngine::export_parcel`,
    /// like `inspect_retained`), so once the parcel is safely out this
    /// releases the handle exactly as [`Self::discard_custody`] would --
    /// the parcel is now the value's only owner on this side.
    pub fn export_custody(&mut self, custody: RootCustody) -> Result<Parcel, ResidentError> {
        self.settle_dropped_custody();
        let transfer = custody.into_transfer();
        let handle = transfer.handle;
        let Some(engine) = self.state.prepared_mut() else {
            return Err(ResidentError::Run(RuntimeError::Jit(EffectError::Handler(
                format!("cannot export {handle:?}: the prepared machine is not installed"),
            ))));
        };
        let parcel = engine.export_parcel(handle)?;
        let released = engine.discard_handle(handle);
        debug_assert!(
            released,
            "a handle just exported must still be live to release"
        );
        transfer.commit();
        Ok(parcel)
    }

    /// Export the value `custody` roots as a detached [`Parcel`], WITHOUT
    /// consuming or releasing `custody` -- the borrowing counterpart to
    /// [`Self::export_custody`], for a value more than one destination
    /// machine may need to import independently (a request's published
    /// progress snapshot, read by however many observers poll it, is the
    /// motivating case). `PreparedEngine::export_parcel` is already a
    /// non-consuming read of the machine (see [`Self::export_custody`]'s own
    /// doc); this only differs by skipping the `discard_handle` after it, so
    /// the root stays live here for the next caller to export again.
    pub fn export_shared(&mut self, custody: &RootCustody) -> Result<Parcel, ResidentError> {
        self.settle_dropped_custody();
        let Some(handle) = custody.handle else {
            unreachable!("live custody always contains its handle");
        };
        let Some(engine) = self.state.prepared_mut() else {
            return Err(ResidentError::Run(RuntimeError::Jit(EffectError::Handler(
                format!("cannot export {handle:?}: the prepared machine is not installed"),
            ))));
        };
        Ok(engine.export_parcel(handle)?)
    }

    /// Import `parcel` under `owner`, rooting its value as a new old-space
    /// arena in this session's machine (`PreparedEngine::import_parcel`),
    /// and mint a [`RootCustody`] over it with this session's own
    /// cleanup/provenance -- the session-layer half of a value crossing two
    /// [`ResidentSession`]s, mirroring how [`Self::live_payload_handle_owned_by`]
    /// mints custody for a handle taken under another resource scope.
    ///
    /// The imported value never passed through one of THIS session's yield
    /// sites -- it was not produced by resuming a parked frame here -- so its
    /// provenance starts empty, the same choice already made for a value
    /// minted without a parked frame behind it (see
    /// [`Self::prepared_binding_handle`]).
    pub fn import_parcel(
        &mut self,
        parcel: Parcel,
        owner: RealmId,
    ) -> Result<RootCustody, ResidentError> {
        self.settle_dropped_custody();
        let Some(engine) = self.state.prepared_mut() else {
            return Err(ResidentError::Run(RuntimeError::Jit(EffectError::Handler(
                "cannot import a parcel: the prepared machine is not installed".to_string(),
            ))));
        };
        let handle = engine.import_parcel(parcel, owner)?;
        Ok(RootCustody::new(
            handle,
            Arc::clone(&self.custody_cleanup),
            Arc::new(ProgramProvenance::default()),
        ))
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
    /// of the owned-handle crossing (see that type's doc): the delivery itself
    /// does not release the handle from the machine's own registry (a resume
    /// is a scope-owned BORROW at the machine layer, same as `observe_handle`),
    /// so without the token nothing at this layer stops a caller from also
    /// mounting the same raw handle. The token is unwrapped once, here, at
    /// the moment its handle is spent.
    pub fn resume_handle(
        &mut self,
        hole: ResidentHole,
        custody: RootCustody,
    ) -> Result<ResidentOutcome, ResidentError> {
        self.resume_handle_classified(hole, custody)
            .map_err(ResidentResumeError::into_inner)
    }

    /// [`Self::resume_handle`] retaining whether the parked frame consumed
    /// the delivered handle before a failure.
    pub fn resume_handle_classified(
        &mut self,
        hole: ResidentHole,
        custody: RootCustody,
    ) -> Result<ResidentOutcome, ResidentResumeError> {
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
    /// its exact identity. This operation joins that type-module identity to a
    /// same-typed in-heap value supplied under affine custody. It is the mount
    /// path for actor inputs and messages: the authoritative value already
    /// exists, so running `undefined`, a guessed inhabitant, or a second copy
    /// merely to create the binding would be both wasteful and semantically
    /// wrong.
    ///
    /// Validation and table merge happen before ownership transfers. On
    /// success ownership transfers from the handle registry to the scoped
    /// persistent binding store exactly once.
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
        let layout = json_runtime_layout(&code.prepared)?;
        self.mount_host_value_in(scope, binder, gen, code, |engine, realm, _| {
            engine.build_host_json(realm, value, &layout)
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

    /// Mount `payload` under a freshly minted `name`/`gen` binder through a
    /// [`HostCarrier`] built once from a real compiled turn, with NO GHC
    /// compile for this mount: the binder's `var_id` is minted directly
    /// ([`tidepool_codegen::prepared_program::session_var_id`]), its
    /// `Val.G<gen>` source is a hand-written stub written at
    /// `<session_root>/Tidepool/Session/Val/G<gen>.hs` (an ordinary home
    /// module a later turn's downsweep finds on the include path -- never
    /// injected via `--inject-val`, since it has no `.hi`), and it is
    /// validated exactly as a compiler-issued binder
    /// ([`Self::validate_compiled_mount_target`]).
    ///
    /// `gen`'s module is recorded as a stub generation
    /// ([`super::persistent::PersistentSession::mark_stub_generation`]) so
    /// `compile_view_in` and [`Self::inject_val_modules`] never name it as
    /// `--inject-val`.
    pub fn mount_carrier_in(
        &mut self,
        session_root: &Path,
        scope: ScopeId,
        name: &str,
        gen: Generation,
        carrier: &HostCarrier,
        payload: HostPayload<'_>,
    ) -> Result<BoundBinder, ResidentError> {
        self.settle_dropped_custody();
        // Reap any stub sources a prior eviction (in this or an earlier
        // call) left pending -- opportunistic, since this call already has
        // the session root a reap needs.
        self.reap_evicted_stub_sources_in(session_root);
        let module = SessionModule::val(gen).module_name();
        let binder = BoundBinder {
            name: name.to_string(),
            var_id: session_var_id(&module, name),
            module: module.clone(),
            tier: carrier.shape.tier,
            type_display: carrier.shape.type_display.clone(),
            root_head: carrier.shape.root_head.clone(),
            host_authority: carrier.shape.host_authority,
        };
        self.validate_compiled_mount_target(
            scope,
            &binder,
            gen,
            &carrier.code(),
            carrier.host_type,
        )?;
        if carrier.host_type == HostBindingType::TEXT {
            self.validate_text_runtime_constructor(&carrier.code())?;
        }

        let stub_source = carrier.stub_source(&module, name);
        let stub_path = session_root.join(SessionModule::val(gen).relative_hs_path());
        if let Some(parent) = stub_path.parent() {
            std::fs::create_dir_all(parent).map_err(|error| {
                ResidentError::Run(RuntimeError::Jit(EffectError::Handler(format!(
                    "host carrier stub directory {}: {error}",
                    parent.display()
                ))))
            })?;
        }
        tidepool_atomic_write::write_durable(&stub_path, stub_source.as_bytes()).map_err(
            |error| {
                ResidentError::Run(RuntimeError::Jit(EffectError::Handler(format!(
                    "host carrier stub {}: {error}",
                    error.path.display()
                ))))
            },
        )?;

        // write + mount is one transaction: a failed mount below must not
        // leave a stub source on disk with no binding to authorize it --
        // that source has never been marked a stub generation, so it would
        // sit on the include path as an ordinary, undocumented home module.
        let mount_result = match payload {
            HostPayload::Json(value) => json_runtime_layout(&carrier.prepared).and_then(|layout| {
                self.mount_host_value_in(scope, &binder, gen, carrier.code(), |engine, realm, _| {
                    engine.build_host_json(realm, value, &layout)
                })
            }),
            HostPayload::Text(text) => self.mount_host_value_in(
                scope,
                &binder,
                gen,
                carrier.code(),
                |engine, realm, table| engine.build_host_text(realm, text, table),
            ),
            HostPayload::Job(value) => self.mount_host_value_in(
                scope,
                &binder,
                gen,
                carrier.code(),
                |engine, realm, table| engine.build_host_value(realm, value, table),
            ),
        };
        if let Err(error) = mount_result {
            std::fs::remove_file(&stub_path).ok();
            return Err(error);
        }
        self.state.mark_stub_generation(gen);
        Ok(binder)
    }

    /// Delete the on-disk `.hs` source for every stub generation
    /// [`super::persistent::PersistentSession::release_binding_roots`] has
    /// found fully unreferenced since the last reap (any eviction path: a
    /// request-carrier retire, a scope close, a declaration replacing a
    /// same-scope name, an expired observation -- see
    /// [`PersistentSession::take_retired_stub_sources`]). Best-effort: a
    /// generation with no file (already reaped, or never on this session
    /// incarnation -- see [`SessionLib::open`]'s stale-stub sweep) is not an
    /// error.
    ///
    /// [`PersistentSession::take_retired_stub_sources`]: super::persistent::PersistentSession::take_retired_stub_sources
    /// [`SessionLib::open`]: super::SessionLib::open
    pub fn reap_evicted_stub_sources_in(&mut self, session_root: &Path) {
        for gen in self.state.take_retired_stub_sources() {
            let path = session_root.join(SessionModule::val(gen).relative_hs_path());
            std::fs::remove_file(path).ok();
        }
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
    ///
    /// `session_root` is used only to reap a stub generation's `.hs` source
    /// if this retirement is what drops it to zero live references (see
    /// [`Self::reap_evicted_stub_sources_in`]); an ordinary compiler-issued
    /// binding has no stub source and this is then a no-op past the retire
    /// itself.
    pub fn retire_host_binding_owner(&mut self, session_root: &Path, binder: &BoundBinder) {
        let id = SessionVarId::from_extract(binder.var_id);
        self.state.retire_binding_owner(id);
        self.hidden_host_bindings.remove(&id);
        self.reap_evicted_stub_sources_in(session_root);
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
        if expected == HostBindingType::JSON_VALUE {
            return self.validate_json_mount_layout(&code.prepared, root, binder);
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

    fn validate_json_mount_layout(
        &self,
        prepared: &PreparedProgram,
        root: &NominalHead,
        binder: &BoundBinder,
    ) -> Result<(), ResidentError> {
        let layout = json_runtime_layout(prepared)?;
        let check = |host_id: DataConId| {
            let family = prepared
                .constructors()
                .iter()
                .find(|declaration| declaration.host_id == host_id)
                .map(|declaration| &declaration.family)
                .ok_or_else(|| {
                    ResidentError::Run(RuntimeError::Jit(EffectError::Handler(
                        "authenticated JSON layout names no prepared constructor".into(),
                    )))
                })?;
            if family.unit != root.unit
                || family.module != root.module
                || family.namespace != "type"
                || family.occurrence != root.name
                || family.record_parent.is_some()
            {
                return Err(ResidentError::Run(RuntimeError::Jit(EffectError::Handler(
                    format!(
                        "compiled binder `{}` root evidence disagrees with authenticated JSON layout",
                        binder.name,
                    ),
                ))));
            }
            Ok(())
        };
        check(layout.object)?;
        check(layout.array)?;
        check(layout.string)?;
        check(layout.number)?;
        check(layout.bool_)?;
        check(layout.null)
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
        let table = code
            .table
            .with_json_layout(json_runtime_layout_optional(&code.prepared));
        self.state
            .merge_table(&table)
            .map_err(ResidentError::TableCollision)?;
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
        self.bind_prepared(program, scope, gen, &[(binder, handle)])?;
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
    /// The caller keeps the handle alive through resumption; the resulting heap
    /// value has ordinary Haskell reachability independent of that root.
    pub fn resume_framed_custody(
        &mut self,
        hole: ResidentHole,
        custody: &RootCustody,
        constructor: tidepool_repr::DataConId,
        prefix: Vec<HaskellValue>,
    ) -> Result<ResidentOutcome, ResidentError> {
        self.resume_framed_custody_classified(hole, custody, constructor, prefix)
            .map_err(ResidentResumeError::into_inner)
    }

    /// [`Self::resume_framed_custody`] retaining whether the parked frame
    /// consumed the framed handle before a failure.
    pub fn resume_framed_custody_classified(
        &mut self,
        hole: ResidentHole,
        custody: &RootCustody,
        constructor: tidepool_repr::DataConId,
        prefix: Vec<HaskellValue>,
    ) -> Result<ResidentOutcome, ResidentResumeError> {
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

    /// Structurally build each owned prefix field directly into the framed
    /// answer, then append the borrowed custody field. This keeps the prefix
    /// out of a collected `HaskellValue` tree while preserving the existing
    /// frame-consumption classification and borrowed-root lifetime.
    pub fn resume_framed_custody_sources_classified<T>(
        &mut self,
        hole: ResidentHole,
        custody: &RootCustody,
        constructor: tidepool_repr::DataConId,
        prefix: Vec<T>,
    ) -> Result<ResidentOutcome, ResidentResumeError>
    where
        T: tidepool_bridge::ToHaskell + Send + 'static,
    {
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
            ResidentResumeInput::FramedHandleSources {
                handle,
                constructor,
                prefix: prefix
                    .into_iter()
                    .map(|field| Box::new(field) as Box<dyn tidepool_bridge::ToHaskell + Send>)
                    .collect(),
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
    /// Move the persistent declaration environment out for a machine rotation — see
    /// [`super::persistent::PersistentSession::take_lib`].
    pub fn take_lib(&mut self) -> Option<crate::session::SessionLib> {
        self.state.take_lib()
    }

    /// Number of live [`ValueHandle`]s outstanding on this session's machine
    /// (0 before the machine is bootstrapped) — the mount seam's ownership-
    /// accounting read: a handle minted over a finalize payload
    /// ([`Self::live_payload_handle`]) counts here until a compiled-binding mount
    /// ([`Self::mount_compiled_binding_in`]), an ordinary bind completion, or
    /// a resource-scope close releases it.
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

    /// The CURRENT persistent binding names (newest gen per name) — what a
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

    /// The persistent binding names visible at `scope`: its own mutable frame over
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

    /// Retire `scope` and its subtree: drop their binding-store frames and
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
    /// into the persistent binding store, or report the suspension whose frame the machine
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

    /// Whether this session already has a resident machine to snapshot an
    /// install against. `false` only for a session's first turn, whose
    /// `install_prepared` bootstraps the machine from that first program --
    /// nothing exists yet to take an off-checkout compile snapshot from, so
    /// that turn has no split path and stays on
    /// [`Self::run_prepared`]/[`Self::run_prepared_with_argument`].
    #[must_use]
    pub fn prepared_machine_ready(&self) -> bool {
        self.state.is_bootstrapped()
    }

    /// Step (a) of the off-checkout split install for a prepared turn (see
    /// `tidepool_codegen::prepared_program::PreparedCompileSnapshot` and
    /// `PreparedEngine::snapshot_install` for what this snapshots and why
    /// it needs no further machine access to compile): merge `code`'s
    /// table, claim the turn's value-module generation, and resolve+plan
    /// the install exactly as [`Self::run_prepared_with_argument`] does,
    /// stopping short of the Cranelift compile. Run this under a machine
    /// checkout; the caller compiles the returned [`PendingPreparedInstall`]
    /// off any checkout ([`PendingPreparedInstall::compile_off_checkout`])
    /// and finishes the turn through [`Self::revalidate_and_run_prepared`]
    /// under a fresh checkout.
    ///
    /// The caller must check [`Self::prepared_machine_ready`] first: this
    /// panics if the session has no machine yet, since bootstrap has
    /// nothing to snapshot (see that method's doc).
    pub fn snapshot_run_prepared(
        &mut self,
        code: TurnCode<'static>,
        mode: PendingPreparedMode,
        argument: Option<PreparedHandle>,
    ) -> Result<PendingPreparedInstall, ResidentError> {
        let prepared = code.prepared;
        let provenance = self.provenance_for(&code.sites)?;
        self.state
            .merge_table(&code.table)
            .map_err(ResidentError::TableCollision)?;
        if let Some(generation) = mode.generation() {
            // Claim the value-module identity before the turn runs.
            self.state.set_val_gen(generation);
        }
        // The caller is documented to check `prepared_machine_ready` first;
        // this typed refusal (rather than a panic) is the fallback if that
        // contract is not honored, since `PreparedRuntimeError` already has
        // a variant for exactly this precondition.
        let snapshot = self
            .state
            .snapshot_install_prepared(prepared.into_owned())?
            .ok_or(PreparedRuntimeError::MachineNotInstalled)?;
        let realm = self.run_context.resource_scope;
        let lexical_scope = self.run_context.lexical_scope;
        let park = ParkPolicy {
            principal: self.run_context.principal,
            effect_policy: self.state.effect_policy(),
            live_payload: self.state.live_payload_policy(),
        };
        Ok(PendingPreparedInstall {
            snapshot,
            mode,
            argument,
            provenance,
            realm,
            lexical_scope,
            park,
        })
    }

    /// Step (c) of the off-checkout split install: revalidate `pending`'s
    /// imports against this (possibly different) checkout's live bindings
    /// and, if nothing changed, install `compiled` and run the turn exactly
    /// as [`Self::run_prepared_with_argument`]'s tail does. `Ok(None)`
    /// means revalidation found a stale import (see
    /// `PreparedEngine::revalidate_and_install`): the caller must recompile
    /// from a fresh [`Self::snapshot_run_prepared`], or fall back to the
    /// single-checkout [`Self::run_prepared_with_argument`].
    pub fn revalidate_and_run_prepared(
        &mut self,
        pending: PendingPreparedInstall,
        compiled: std::sync::Arc<tidepool_codegen::prepared_program::CompiledProgram>,
    ) -> Result<Option<ResidentOutcome>, ResidentError> {
        let PendingPreparedInstall {
            snapshot,
            mode,
            argument,
            provenance,
            realm,
            lexical_scope,
            park,
        } = pending;
        let install_started = std::time::Instant::now();
        let program = match self
            .state
            .revalidate_and_install_prepared(snapshot, compiled)?
        {
            Some(program) => program,
            None => return Ok(None),
        };
        timing::record_stage(
            timing::NO_NODE,
            timing::NO_ROUND,
            timing::STAGE_INSTALL_PREPARED,
            install_started.elapsed(),
            0,
        );
        let borrowed_mode = mode.as_mode();
        let plan = settle_plan_of(&borrowed_mode);
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
        if let Some(engine) = self.state.prepared_mut() {
            engine.unpin(program);
        }
        Ok(Some(self.complete_prepared(
            ran??,
            mode.as_mode(),
            program,
            lexical_scope,
            provenance,
            None,
        )?))
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

    /// Bind retained prepared handles into the persistent binding store at `scope`, one
    /// entry per `(binder, handle)`, all at `generation`. Every handle is
    /// adopted into the machine's ROOT scope first (no resource-scope close releases
    /// it), so the binding owns its lifetime and scope retirement releases
    /// it. The lexical scope is validated before any handle is adopted; a
    /// failure releases every handle not yet bound, so nothing is left rooted
    /// outside both the resource-scope registry and the binding table.
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

    /// Run a persistent-binding BIND turn (`x <- e`): seed the env from prior bindings,
    /// add the fragment, and drive it through the suspendable BIND path
    /// (tenure-on-completion). On completion, materialize `binder` into the value
    /// store at `gen` (the SAME generation the extract stamped into
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
            // `Bounded` subsumes `Complete`'s success case in one pass -- see
            // `SettlePlan::Observe`.
            let observed = engine.observe_bounded(program, metadata);
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

    /// A bounded, non-forcing text preview of a retained value's shape:
    /// constructor names (through this session's own constructor table) and
    /// literal fields, walked through [`PreparedEngine::inspect_retained`].
    /// No Haskell compiles and no thunk forces -- a still-unevaluated field
    /// or a callable (function/PAP) shape prints as `…` rather than being
    /// entered. `custody` is only borrowed; it remains usable afterward.
    ///
    /// `None` when there is no live machine to read from, `custody` belongs
    /// to a different session, or the root itself cannot be inspected (for
    /// example a bare function value with no constructor layer at all).
    #[must_use]
    pub fn render_retained_preview(
        &mut self,
        custody: &RootCustody,
        char_budget: usize,
    ) -> Option<String> {
        if !Arc::ptr_eq(&custody.cleanup, &self.custody_cleanup) {
            return None;
        }
        let handle = custody.handle?;
        let table = self.state.session_table().clone();
        let engine = self.state.prepared_mut()?;
        // The walk itself runs against a generous internal cap (bounded work
        // regardless of `char_budget`), then the caller's exact budget is
        // enforced once, at a line boundary, below -- cutting mid-walk would
        // land wherever a field happened to end, not at a readable line.
        let mut walk_budget = char_budget.saturating_mul(4).max(4096);
        let mut out = String::new();
        render_retained_layer(engine, &table, handle, 0, &mut walk_budget, &mut out);
        Some(truncate_preview_at_line(out, char_budget))
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
    /// and returned (never bound into the persistent binding store — a rooted apply is
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
    /// persistent binding store on completion, using the SAME binder/generation its
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
        self.resume_classified(hole, answer)
            .map_err(ResidentResumeError::into_inner)
    }

    /// [`Self::resume`] with authoritative pre-consume versus post-consume
    /// failure classification for callers that publish effect disposition.
    pub fn resume_classified<T>(
        &mut self,
        hole: ResidentHole,
        answer: T,
    ) -> Result<ResidentOutcome, ResidentResumeError>
    where
        T: tidepool_bridge::ToHaskell + Send + 'static,
    {
        self.resume_response_classified(hole, Response::new(answer))
    }

    /// Resume a suspended turn from one owned structural source. Conversion
    /// errors are classified at the validate-before-consume boundary, while
    /// the parked continuation is still available to retry or abort.
    pub fn resume_response(
        &mut self,
        hole: ResidentHole,
        answer: Response,
    ) -> Result<ResidentOutcome, ResidentError> {
        self.resume_response_classified(hole, answer)
            .map_err(ResidentResumeError::into_inner)
    }

    /// [`Self::resume_response`] retaining whether the original continuation
    /// was consumed. The resident registry is the sole authority for this
    /// distinction; callers must not infer it from error text or variants.
    pub fn resume_response_classified(
        &mut self,
        hole: ResidentHole,
        answer: Response,
    ) -> Result<ResidentOutcome, ResidentResumeError> {
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
        .map_err(ResidentResumeError::into_inner)
    }

    fn reenter(
        &mut self,
        cont_id: &str,
        input: ResidentResumeInput,
        seed: HoleSeed,
        additional_provenance: Option<&ProgramProvenance>,
    ) -> Result<ResidentOutcome, ResidentResumeError> {
        // Validate BEFORE consuming: `cont_id` must be a MEMBER of the parked
        // set (any-order resume — the machine imposes no order and neither do
        // we). A mismatch leaves every parked frame intact.
        let Some(&(_, frame_id)) = self.parked.iter().find(|(h, _)| h == cont_id) else {
            return Err(ResidentResumeError::Rejected(
                ResidentError::WrongContinuation {
                    attempted: cont_id.to_string(),
                    pending: self.parked.iter().map(|(h, _)| h.clone()).collect(),
                },
            ));
        };
        let mut provenance = self
            .parked_provenance
            .get(&frame_id)
            .map(|value| (**value).clone())
            .unwrap_or_default();
        if let Some(additional) = additional_provenance {
            provenance
                .merge(additional)
                .map_err(ResidentError::from)
                .map_err(ResidentResumeError::Rejected)?;
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
            .map_err(|error| {
                if self.state.parked_ids().contains(&frame_id) {
                    ResidentResumeError::Rejected(error)
                } else {
                    ResidentResumeError::Consumed(error)
                }
            })
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

    /// Release affine roots whose handles were dropped while the machine was
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
                ResidentResumeInput::FramedHandleSources {
                    handle,
                    constructor,
                    prefix,
                } => engine.resume_with_framed_handle_sources(
                    frame_id,
                    handle,
                    constructor,
                    &prefix,
                    table,
                ),
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
#[allow(
    clippy::items_after_test_module,
    reason = "this test module sits next to the host-binding-authority code it covers; the \
              suspension-seam impl and eval-thread types below it belong at module end"
)]
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
