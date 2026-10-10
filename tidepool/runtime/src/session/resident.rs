//! Resident prepared-STG sessions.
//!
//! Each turn installs or reuses a prepared program in one long-lived machine.
//! Completed turns return the machine to the session; suspended turns park a
//! rooted continuation while later turns and other resumptions remain usable.
//! The machine moves to an evaluation thread only for the duration of an entry.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;

use parking_lot::Mutex;

use tidepool_bridge::HaskellValue;
use tidepool_codegen::binding_table::{BindingEntry, BoundValue};
use tidepool_codegen::prepared_program::{
    session_var_id, DemandedImage, ImageRegistry, InheritedSourceDemand, Parcel, PreparedHandle,
    PreparedOuter, PreparedResult, ProgramId, SourceBinder,
};
use tidepool_repr::execution_schema::{
    CachedHomeOwner, ImportOwner, JsonLayout, PreparedProgram, RuntimeRep, SymbolIdentity,
};

use super::admission::CheckedTurnCompletion;
use super::prepared::{ParkPolicy, PreparedRuntimeError, PreparedSettlement, HOME_UNIT};
use super::turn::{TurnCode, TurnPurpose};
use tidepool_codegen::suspension::{ContinuationId, RealmId, ValueHandle};
use tidepool_effect::dispatch::{
    request_constructor, DeferredEffect, DispatchEffect, EffectContext, EffectDispatch, Response,
};
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
    fresh_completion_sites: std::collections::BTreeSet<u64>,
    native_sites: tidepool_toolchain::checked_cell::SelectedNativeSites,
    // Authentication follows the original compiler bundle through owned roots.
    // Public metadata construction alone cannot authorize a host input mount.
    authenticated_inputs: BTreeMap<u64, AuthenticatedInputContext>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct AuthenticatedInputContext {
    types: Arc<tidepool_toolchain::declaration_join::ExactDeclarationContext>,
    type_identity: [u8; 32],
    execution: OriginalExecutionContexts,
}

impl AuthenticatedInputContext {
    fn capture(
        types: Arc<tidepool_toolchain::declaration_join::ExactDeclarationContext>,
        execution: OriginalExecutionContexts,
    ) -> Self {
        Self {
            type_identity: types.semantic_sha256(),
            types,
            execution,
        }
    }
}

/// Compatible typed sites can travel through more than one original invocation.
/// Composition retains those scopes; only a consumer of original instance
/// visibility may demand a unique one.
#[derive(Clone, Debug, PartialEq, Eq)]
enum OriginalExecutionContexts {
    Unique(Arc<OriginalExecutionContext>),
    Ambiguous(BTreeMap<[u8; 32], Arc<OriginalExecutionContext>>),
}

#[derive(Debug)]
struct OriginalExecutionContext {
    identity: [u8; 32],
    context: Arc<tidepool_toolchain::declaration_join::ExactDeclarationContext>,
}

impl OriginalExecutionContext {
    fn capture(
        context: Arc<tidepool_toolchain::declaration_join::ExactDeclarationContext>,
    ) -> Arc<Self> {
        Arc::new(Self {
            identity: context.semantic_sha256(),
            context,
        })
    }
}

impl PartialEq for OriginalExecutionContext {
    fn eq(&self, other: &Self) -> bool {
        self.identity == other.identity
    }
}

impl Eq for OriginalExecutionContext {}

impl OriginalExecutionContexts {
    fn contexts(&self) -> impl Iterator<Item = &Arc<OriginalExecutionContext>> {
        let (unique, ambiguous) = match self {
            Self::Unique(context) => (Some(context), None),
            Self::Ambiguous(contexts) => (None, Some(contexts)),
        };
        unique
            .into_iter()
            .chain(ambiguous.into_iter().flat_map(|contexts| contexts.values()))
    }

    fn merge(&mut self, other: &Self) {
        for context in other.contexts() {
            match self {
                Self::Unique(previous) if previous.identity == context.identity => {}
                Self::Unique(previous) => {
                    *self = Self::Ambiguous(BTreeMap::from([
                        (previous.identity, Arc::clone(previous)),
                        (context.identity, Arc::clone(context)),
                    ]));
                }
                Self::Ambiguous(contexts) => {
                    contexts
                        .entry(context.identity)
                        .or_insert_with(|| Arc::clone(context));
                }
            }
        }
    }

    fn require_unique(
        &self,
        site: u64,
    ) -> Result<Arc<tidepool_toolchain::declaration_join::ExactDeclarationContext>, ResidentError>
    {
        match self {
            Self::Unique(context) => Ok(Arc::clone(&context.context)),
            Self::Ambiguous(contexts) => {
                Err(ResidentError::AmbiguousActivationInputOriginalContext {
                    site,
                    contexts: contexts.keys().copied().collect(),
                })
            }
        }
    }
}

/// Detached native value with its original immutable compiler provenance.
/// Only this session's custody exporter can pair the two; importing never
/// manufactures compiler observations or authenticated input authority.
#[must_use = "a resident parcel must be imported or deliberately dropped"]
pub struct ResidentParcel {
    native: Parcel,
    provenance: Arc<ProgramProvenance>,
}

static_assertions::assert_not_impl_any!(ResidentParcel: Clone, Copy);

impl ResidentParcel {
    pub fn bytes(&self) -> usize {
        self.native.bytes()
    }
}

#[derive(Clone, Debug, thiserror::Error, PartialEq, Eq)]
pub enum ProgramProvenanceError {
    #[error(transparent)]
    SiteMetadata(#[from] YieldSiteCollision),
    #[error("native site authority conflict: {0}")]
    NativeSiteAuthority(String),
    #[error("typed input authority conflict at site {site}: {existing:?} != {incoming:?}")]
    AuthenticatedInputTypeContext {
        site: u64,
        existing: [u8; 32],
        incoming: [u8; 32],
    },
}

impl ProgramProvenance {
    pub fn from_sites(sites: &[YieldSite]) -> Result<Self, ProgramProvenanceError> {
        let mut provenance = Self::default();
        provenance.extend(sites)?;
        Ok(provenance)
    }

    fn extend(&mut self, sites: &[YieldSite]) -> Result<(), ProgramProvenanceError> {
        for site in sites {
            if let Some(previous) = self.sites.get(&site.site) {
                if !previous.same_metadata(site) {
                    return Err(YieldSiteCollision {
                        site: site.site,
                        first: Box::new(previous.clone()),
                        second: Box::new(site.clone()),
                    }
                    .into());
                }
            } else {
                self.sites.insert(site.site, site.clone());
            }
        }
        Ok(())
    }

    fn merge(&mut self, other: &Self) -> Result<(), ProgramProvenanceError> {
        // A failed composition leaves the original value's evidence unchanged.
        // Site/type compatibility is independent of original execution scope.
        let mut native_sites = self.native_sites.clone();
        native_sites
            .merge(&other.native_sites)
            .map_err(|error| ProgramProvenanceError::NativeSiteAuthority(error.to_string()))?;
        native_sites
            .validate_observations(self.sites.values())
            .map_err(|error| ProgramProvenanceError::NativeSiteAuthority(error.to_string()))?;
        native_sites
            .validate_observations(other.sites.values())
            .map_err(|error| ProgramProvenanceError::NativeSiteAuthority(error.to_string()))?;
        for site in other.sites.values() {
            if let Some(previous) = self.sites.get(&site.site) {
                if !previous.same_metadata(site) {
                    return Err(YieldSiteCollision {
                        site: site.site,
                        first: Box::new(previous.clone()),
                        second: Box::new(site.clone()),
                    }
                    .into());
                }
            }
        }
        for (site, interfaces) in &other.authenticated_inputs {
            if self
                .authenticated_inputs
                .get(site)
                .is_some_and(|previous| previous.type_identity != interfaces.type_identity)
            {
                return Err(ProgramProvenanceError::AuthenticatedInputTypeContext {
                    site: *site,
                    existing: self.authenticated_inputs[site].type_identity,
                    incoming: interfaces.type_identity,
                });
            }
        }
        for site in other.sites.values() {
            self.extend(std::slice::from_ref(site))?;
        }
        for (site, interfaces) in &other.authenticated_inputs {
            self.authenticated_inputs
                .entry(*site)
                .and_modify(|previous| previous.execution.merge(&interfaces.execution))
                .or_insert_with(|| interfaces.clone());
        }
        self.native_sites = native_sites;
        self.fresh_completion_sites
            .extend(other.fresh_completion_sites.iter().copied());
        Ok(())
    }

    /// Completion authority follows the requesting code's issued sites,
    /// including its selected retained native dependencies.
    #[must_use]
    pub fn has_completion_site(&self, site: u64) -> bool {
        self.native_sites.has_completion_site(site) || self.fresh_completion_sites.contains(&site)
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
}

impl HostBindingType {
    pub(super) fn frame_authorization(self, frame: &mut impl FnMut(&[u8])) {
        frame(self.module.as_bytes());
        frame(self.name.as_bytes());
    }
    pub const JSON_VALUE: Self = Self {
        authority: HostBindingAuthority::JsonValue,
        module: "Tidepool.Aeson.Value",
        name: "Value",
    };
    pub const TEXT: Self = Self {
        authority: HostBindingAuthority::Text,
        module: "Data.Text.Internal",
        name: "Text",
    };
    pub const COMMAND_JOB: Self = Self {
        authority: HostBindingAuthority::CommandJob,
        module: "Tidepool.Command.Types",
        name: "Job",
    };
}

/// Complete representation evidence needed by one fixed host builder.
#[derive(Clone, Copy, Debug)]
enum HostRepresentation {
    Json(JsonLayout<DataConId>),
    Text { text: DataConId },
    Job { job: DataConId, text: DataConId },
}

impl HostRepresentation {
    fn host_type(self) -> HostBindingType {
        match self {
            Self::Json(_) => HostBindingType::JSON_VALUE,
            Self::Text { .. } => HostBindingType::TEXT,
            Self::Job { .. } => HostBindingType::COMMAND_JOB,
        }
    }

    fn matches_payload(self, payload: &HostPayload<'_>) -> bool {
        matches!(
            (self, payload),
            (Self::Json(_), HostPayload::Json(_))
                | (Self::Text { .. }, HostPayload::Text(_))
                | (Self::Job { .. }, HostPayload::Job(_))
        )
    }

    fn build(
        self,
        engine: &mut super::prepared::PreparedEngine,
        realm: RealmId,
        payload: HostPayload<'_>,
    ) -> Result<PreparedHandle, PreparedRuntimeError> {
        match (self, payload) {
            (Self::Json(layout), HostPayload::Json(value)) => {
                engine.build_host_json(realm, value, &layout)
            }
            (Self::Text { text }, HostPayload::Text(value)) => {
                engine.build_host_text_exact(realm, value, text)
            }
            (Self::Job { job, text }, HostPayload::Job(value)) => {
                engine.build_host_job_exact(realm, value, job, text)
            }
            _ => Err(PreparedRuntimeError::HostMount {
                detail: "host payload differs from its authenticated carrier".into(),
            }),
        }
    }
}

#[derive(Debug)]
struct HostCarrierShape {
    tier: ValueTier,
    type_display: String,
    root: NominalHead,
}

enum HostCarrierOrigin {
    Reusable(HostCarrierShape),
    Checked {
        admission: Arc<super::RuntimeCheckedItemAdmission>,
        binder: BoundBinder,
    },
    Interface {
        admission: Arc<super::RuntimeHostBindingAdmission>,
        binder: BoundBinder,
        proof: Arc<tidepool_toolchain::checked_cell::ExactHostBindingInterface>,
    },
}

/// Fixed host representation and native type evidence retained with the
/// original checked source owner. This grants no fresh binding by itself.
pub struct HostBindingPrototype {
    compiler: Arc<tidepool_toolchain::checked_cell::ExactHostBindingPrototype>,
    code: Arc<TurnCode<'static>>,
    representation: HostRepresentation,
    authority_digest: [u8; 32],
    include_paths: Vec<std::path::PathBuf>,
    _source_owner: Arc<dyn std::any::Any + Send + Sync>,
}

impl HostBindingPrototype {
    pub fn compiler(&self) -> &Arc<tidepool_toolchain::checked_cell::ExactHostBindingPrototype> {
        &self.compiler
    }
    pub fn authority_digest(&self) -> [u8; 32] {
        self.authority_digest
    }
    pub fn include_paths(&self) -> &[std::path::PathBuf] {
        &self.include_paths
    }
}

/// Complete representation for one fixed host builder. Checked carriers retain
/// their original reservation; interface instances retain a fresh sealed host
/// admission. Reusable carriers provide only generic embedding authority.
pub struct HostCarrier {
    code: Arc<TurnCode<'static>>,
    representation: HostRepresentation,
    origin: HostCarrierOrigin,
}

impl HostCarrier {
    /// Validate a reusable representation from the generic embedding caller's
    /// code. This supplies no checked authority: a checked compilation cannot
    /// be converted into a fresh-name mount or stripped of its reservation.
    pub fn from_compiled(
        binder: &BoundBinder,
        code: TurnCode<'_>,
        host_type: HostBindingType,
    ) -> Result<Self, ResidentError> {
        refuse_checked_turn(&code)?;
        let (root, representation) = host_representation(binder, &code, host_type)?;
        Ok(Self {
            code: Arc::new(own_host_code(code)),
            representation,
            origin: HostCarrierOrigin::Reusable(HostCarrierShape {
                tier: binder.tier,
                type_display: binder.type_display.clone(),
                root,
            }),
        })
    }

    /// Bind complete representation evidence to the original checked item.
    pub fn from_checked(
        admission: Arc<super::RuntimeCheckedItemAdmission>,
        binder: BoundBinder,
        code: TurnCode<'_>,
        expected: HostBindingType,
    ) -> Result<Self, ResidentError> {
        let generation = admission.generation();
        if admission.prefix().admission().host_carrier() != Some((binder.name.as_str(), expected))
            || binder.module != SessionModule::val(generation).module_name()
            || !code.sites.is_empty()
        {
            return Err(ResidentError::UnsupportedCheckedTurn);
        }
        let certification = code
            .certification
            .as_ref()
            .as_ref()
            .ok_or(ResidentError::UnsupportedCheckedTurn)?;
        let execution = certification
            .checked_execution()
            .ok_or(ResidentError::UnsupportedCheckedTurn)?;
        if certification
            .checked_prefix()
            .is_none_or(|prefix| !Arc::ptr_eq(prefix, admission.prefix()))
            || execution.item() != admission.item()
        {
            return Err(ResidentError::UnsupportedCheckedTurn);
        }
        certification
            .validate_checked_table(&code.table)
            .map_err(SessionError::Compile)?;
        certification
            .validate_checked_bind(&code.prepared, generation.0, std::slice::from_ref(&binder))
            .map_err(SessionError::Compile)?;
        let interface = execution
            .value_interface_certificate()
            .ok_or(ResidentError::UnsupportedCheckedTurn)?;
        if interface.owner() != SessionModule::val(generation) {
            return Err(ResidentError::UnsupportedCheckedTurn);
        }
        let (_, representation) = host_representation(&binder, &code, expected)?;
        Ok(Self {
            code: Arc::new(own_host_code(code)),
            representation,
            origin: HostCarrierOrigin::Checked { admission, binder },
        })
    }

    /// Original checked ownership; no caller can substitute another binder.
    pub fn checked_binding(
        &self,
    ) -> Result<(&Arc<super::RuntimeCheckedItemAdmission>, &BoundBinder), ResidentError> {
        match &self.origin {
            HostCarrierOrigin::Checked { admission, binder } => Ok((admission, binder)),
            HostCarrierOrigin::Reusable(_) | HostCarrierOrigin::Interface { .. } => {
                Err(ResidentError::UnsupportedCheckedTurn)
            }
        }
    }

    /// Preserve the original compiler-issued binding across both checked host
    /// admission forms. Reusable generic embeddings have no such authority.
    pub fn binding(&self) -> Result<&BoundBinder, ResidentError> {
        match &self.origin {
            HostCarrierOrigin::Checked { binder, .. }
            | HostCarrierOrigin::Interface { binder, .. } => Ok(binder),
            HostCarrierOrigin::Reusable(_) => Err(ResidentError::UnsupportedCheckedTurn),
        }
    }

    pub fn session_root(&self) -> Result<&Path, ResidentError> {
        match &self.origin {
            HostCarrierOrigin::Checked { admission, .. } => {
                Ok(admission.snapshot().view().session_root())
            }
            HostCarrierOrigin::Interface { admission, .. } => Ok(&admission.session_root),
            HostCarrierOrigin::Reusable(_) => Err(ResidentError::UnsupportedCheckedTurn),
        }
    }

    /// Authenticate one fresh compiler interface against its runtime reservation
    /// and reusable original representation. No authored recipe is fabricated.
    pub fn from_interface(
        admission: Arc<super::RuntimeHostBindingAdmission>,
        proof: Arc<tidepool_toolchain::checked_cell::ExactHostBindingInterface>,
    ) -> Result<Self, ResidentError> {
        let binder =
            super::turn::decode_bound_binder(proof.binder()).map_err(SessionError::Compile)?;
        if !Arc::ptr_eq(proof.prototype(), admission.prototype.compiler())
            || proof.admission_digest() != admission.digest()
            || proof.generation() != admission.generation().0
            || binder.name != admission.binding()
            || binder.module != SessionModule::val(admission.generation()).module_name()
            || binder.var_id != session_var_id(&binder.module, &binder.name)
            || proof.value_interface_certificate().owner()
                != SessionModule::val(admission.generation())
        {
            return Err(ResidentError::UnsupportedCheckedTurn);
        }
        let prototype = &admission.prototype;
        let (_, representation) = host_representation(
            &binder,
            &TurnCode {
                table: std::borrow::Cow::Borrowed(prototype.code.table.as_ref()),
                sites: std::borrow::Cow::Borrowed(prototype.code.sites.as_ref()),
                prepared: std::borrow::Cow::Borrowed(prototype.code.prepared.as_ref()),
                certification: std::borrow::Cow::Borrowed(prototype.code.certification.as_ref()),
            },
            prototype.representation.host_type(),
        )?;
        Ok(Self {
            code: prototype.code.clone(),
            representation,
            origin: HostCarrierOrigin::Interface {
                admission,
                binder,
                proof,
            },
        })
    }

    /// Borrow the immutable compiler proof without changing carrier ownership.
    pub fn code(&self) -> TurnCode<'_> {
        TurnCode {
            table: std::borrow::Cow::Borrowed(self.code.table.as_ref()),
            sites: std::borrow::Cow::Borrowed(self.code.sites.as_ref()),
            prepared: std::borrow::Cow::Borrowed(self.code.prepared.as_ref()),
            certification: std::borrow::Cow::Borrowed(self.code.certification.as_ref()),
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
            ty_module = self.representation.host_type().module,
            ty_name = self.representation.host_type().name,
        )
    }
}

/// Fixed host values accepted by a [`HostCarrier`]. A Job payload is its
/// session ID; the carrier supplies the exact Job and Text constructors.
pub enum HostPayload<'a> {
    Json(&'a serde_json::Value),
    Text(&'a str),
    Job(&'a str),
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

fn own_host_code(code: TurnCode<'_>) -> TurnCode<'static> {
    TurnCode {
        table: std::borrow::Cow::Owned(code.table.into_owned()),
        sites: std::borrow::Cow::Owned(code.sites.into_owned()),
        prepared: std::borrow::Cow::Owned(code.prepared.into_owned()),
        certification: std::borrow::Cow::Owned(code.certification.into_owned()),
    }
}

fn host_mount_failure(detail: impl Into<String>) -> ResidentError {
    ResidentError::Run(RuntimeError::Jit(EffectError::Handler(detail.into())))
}

fn exact_host_constructor(
    code: &TurnCode<'_>,
    root: &NominalHead,
    occurrence: &str,
    fields: &[RuntimeRep],
) -> Result<DataConId, ResidentError> {
    let mut rows = code.prepared.constructors().iter().filter(|row| {
        row.family.unit == root.unit
            && row.family.module == root.module
            && row.family.namespace == "type"
            && row.family.occurrence == root.name
            && row.family.record_parent.is_none()
            && row.identity.unit == root.unit
            && row.identity.module == root.module
            && row.identity.namespace == "constructor"
            && row.identity.occurrence == occurrence
            && row.identity.record_parent.is_none()
    });
    let row = rows.next().ok_or_else(|| {
        host_mount_failure(format!(
            "host mount lacks exact constructor {}:{}:{occurrence}",
            root.unit, root.module
        ))
    })?;
    if rows.any(|other| other != row)
        || row.result_rep != RuntimeRep::LiftedRef
        || row.field_reps != fields
        || row.tag != 1
        || row.family_size != 1
        || code
            .table
            .get(row.host_id)
            .is_none_or(|table| table.rep_arity as usize != fields.len() || table.tag != row.tag)
    {
        return Err(host_mount_failure(
            "host constructor differs from its authenticated table",
        ));
    }
    Ok(row.host_id)
}

// The sealed target retains the jointly admitted Job/Text representation. The
// compiler verifies Job's field against its selected Text owner before seeding
// these rows; the wire carries physical fields, not their boxed nominal types.
// A second same-spelling Text owner makes this bundle ambiguous and is refused.
fn authenticated_text_constructor(code: &TurnCode<'_>) -> Result<DataConId, ResidentError> {
    let mut roots = code.prepared.constructors().iter().filter(|row| {
        !row.family.unit.is_empty()
            && row.family.module == "Data.Text.Internal"
            && row.family.namespace == "type"
            && row.family.occurrence == "Text"
            && row.family.record_parent.is_none()
    });
    let row = roots
        .next()
        .ok_or_else(|| host_mount_failure("host mount has no authenticated Text representation"))?;
    if roots.any(|other| other.family != row.family) {
        return Err(host_mount_failure("host mount has conflicting Text owners"));
    }
    exact_host_constructor(
        code,
        &NominalHead {
            unit: row.family.unit.clone(),
            module: row.family.module.clone(),
            name: row.family.occurrence.clone(),
        },
        "Text",
        &[
            RuntimeRep::UnliftedRef,
            RuntimeRep::Int(64),
            RuntimeRep::Int(64),
        ],
    )
}

fn host_representation(
    binder: &BoundBinder,
    code: &TurnCode<'_>,
    expected: HostBindingType,
) -> Result<(NominalHead, HostRepresentation), ResidentError> {
    require_host_binding_authority(binder, expected)?;
    let root = binder
        .root_head
        .as_ref()
        .ok_or_else(|| host_mount_failure("compiled host binder lacks its nominal root"))?;
    if root.unit.is_empty() || root.module != expected.module || root.name != expected.name {
        return Err(host_mount_failure(
            "compiled host binder differs from its authenticated nominal root",
        ));
    }
    let representation = match expected.authority {
        HostBindingAuthority::JsonValue => {
            let layout = json_runtime_layout(&code.prepared)?;
            let constructors = code.prepared.constructors();
            (*code
                .prepared
                .json_layout()
                .ok_or_else(|| host_mount_failure("host JSON layout missing"))?)
            .try_map(|id| {
                let row = constructors
                    .get(id.0 as usize)
                    .ok_or_else(|| host_mount_failure("host JSON layout constructor missing"))?;
                if code.table.get(row.host_id).is_none_or(|table| {
                    table.rep_arity as usize != row.field_reps.len() || table.tag != row.tag
                }) {
                    return Err(host_mount_failure(
                        "host JSON layout differs from its compiler table",
                    ));
                }
                Ok(())
            })?;
            for id in [
                layout.object,
                layout.array,
                layout.string,
                layout.number,
                layout.bool_,
                layout.null,
            ] {
                if constructors
                    .iter()
                    .find(|row| row.host_id == id)
                    .is_none_or(|row| {
                        row.family.unit != root.unit
                            || row.family.module != root.module
                            || row.family.namespace != "type"
                            || row.family.occurrence != root.name
                            || row.family.record_parent.is_some()
                    })
                {
                    return Err(host_mount_failure(
                        "host JSON layout disagrees with its original nominal root",
                    ));
                }
            }
            HostRepresentation::Json(layout)
        }
        HostBindingAuthority::Text => HostRepresentation::Text {
            text: exact_host_constructor(
                code,
                root,
                "Text",
                &[
                    RuntimeRep::UnliftedRef,
                    RuntimeRep::Int(64),
                    RuntimeRep::Int(64),
                ],
            )?,
        },
        HostBindingAuthority::CommandJob => HostRepresentation::Job {
            job: exact_host_constructor(code, root, "Job", &[RuntimeRep::LiftedRef])?,
            text: authenticated_text_constructor(code)?,
        },
    };
    Ok((root.clone(), representation))
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
/// The session creates this handle when a finalized value leaves a parked frame
/// or when a caller explicitly retains independent custody of a live binding.
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
    cleanup: CustodyLease,
    provenance: Arc<ProgramProvenance>,
}

// Custody must remain exclusive.
static_assertions::assert_not_impl_any!(RootCustody: Clone, Copy);

/// The original live activation input and its compiler-issued request type.
/// It is captured from one parked request boundary, never assembled from an
/// arbitrary type string and an unrelated value handle.
#[derive(Debug)]
pub struct RuntimeActivationInput {
    custody: RootCustody,
    site: u64,
    input_type: String,
    type_evidence: Arc<super::prepared::SiteTypeEvidence>,
    input_type_witness: Arc<tidepool_toolchain::checked_cell::CanonicalInputTypeWitness>,
    prototype: Arc<tidepool_toolchain::checked_cell::ExactHostBindingPrototype>,
    original_execution: Arc<tidepool_toolchain::declaration_join::ExactDeclarationContext>,
    progress_type_witness: Option<Arc<tidepool_toolchain::checked_cell::CanonicalInputTypeWitness>>,
}

static_assertions::assert_not_impl_any!(RuntimeActivationInput: Clone, Copy);

impl RuntimeActivationInput {
    pub fn input_type(&self) -> &str {
        &self.input_type
    }

    pub fn type_evidence(&self) -> &Arc<super::prepared::SiteTypeEvidence> {
        &self.type_evidence
    }

    pub fn progress_type_witness(
        &self,
    ) -> Option<Arc<tidepool_toolchain::checked_cell::CanonicalInputTypeWitness>> {
        self.progress_type_witness.clone()
    }
}

/// A progress root and its canonical type captured from the same authenticated
/// parked helper call. Callers cannot pair an arbitrary root with a witness.
#[derive(Debug)]
pub struct RuntimeProgressPublication {
    custody: RootCustody,
    type_witness: Arc<tidepool_toolchain::checked_cell::CanonicalInputTypeWitness>,
}

static_assertions::assert_not_impl_any!(RuntimeProgressPublication: Clone, Copy);

impl RuntimeProgressPublication {
    pub fn into_parts(
        self,
    ) -> (
        RootCustody,
        Arc<tidepool_toolchain::checked_cell::CanonicalInputTypeWitness>,
    ) {
        (self.custody, self.type_witness)
    }
}

/// One affine original input paired with a fresh type-interface reservation.
pub struct RuntimeActivationInputAdmission {
    input: RuntimeActivationInput,
    reservation: Arc<super::RuntimeBindingInterfaceReservation>,
    run_context: SessionRunContext,
}

impl RuntimeActivationInputAdmission {
    pub fn reservation(&self) -> &Arc<super::RuntimeBindingInterfaceReservation> {
        &self.reservation
    }
}

static_assertions::assert_not_impl_any!(RuntimeActivationInputAdmission: Clone, Copy);

/// A committed original live binding. Type authority does not authorize preview
/// code; that executable is admitted separately after this mount.
pub struct MountedActivationInput {
    interface: Arc<tidepool_toolchain::checked_cell::ExactHostBindingInterface>,
    original_execution: Arc<tidepool_toolchain::declaration_join::ExactDeclarationContext>,
    binding: SessionVarId,
    scope: ScopeId,
    handle: PreparedHandle,
    visibility: super::PublicVisibilitySnapshot,
    run_context: SessionRunContext,
}

impl MountedActivationInput {
    pub fn binding(&self) -> SessionVarId {
        self.binding
    }
    pub fn interface(&self) -> &Arc<tidepool_toolchain::checked_cell::ExactHostBindingInterface> {
        &self.interface
    }
}

static_assertions::assert_not_impl_any!(MountedActivationInput: Clone, Copy);

/// A pure preview is admitted only after binding. It retains original executable
/// evidence independently of the recipient's selected authored source layer.
pub struct RuntimeActivationPreviewAdmission {
    owner: Arc<super::admission::RuntimeAdmissionOwner>,
    owner_epoch: u64,
    mounted: MountedActivationInput,
    view: super::SessionCompileView,
    view_digest: [u8; 32],
    generation: Generation,
    digest: [u8; 32],
    exact_context: Arc<tidepool_toolchain::declaration_join::ExactCompileContext>,
    scope_lease: Arc<super::RuntimeLexicalScopeLease>,
    consumed: AtomicBool,
}

impl RuntimeActivationPreviewAdmission {
    pub fn view(&self) -> &super::SessionCompileView {
        &self.view
    }
    pub fn digest(&self) -> [u8; 32] {
        self.digest
    }
    pub fn generation(&self) -> Generation {
        self.generation
    }
    pub fn input_interface(
        &self,
    ) -> &Arc<tidepool_toolchain::checked_cell::ExactHostBindingInterface> {
        &self.mounted.interface
    }
    pub fn exact_context(&self) -> &Arc<tidepool_toolchain::declaration_join::ExactCompileContext> {
        &self.exact_context
    }
}

/// One protected executable prepared for the original committed input.
pub struct CompiledActivationPreview {
    pub(super) admission: Arc<RuntimeActivationPreviewAdmission>,
    pub(super) compiled: super::turn::CompiledTurn,
    pub(super) proof: Arc<tidepool_toolchain::activation_preview::ExactCompiledActivationPreview>,
}

impl CompiledActivationPreview {
    pub fn proof(
        &self,
    ) -> &Arc<tidepool_toolchain::activation_preview::ExactCompiledActivationPreview> {
        &self.proof
    }
}

static_assertions::assert_not_impl_any!(CompiledActivationPreview: Clone, Copy);

impl RootCustody {
    /// Wrap a handle minted by the resident session.
    fn new(
        handle: ValueHandle,
        cleanup: Arc<CustodyCleanup>,
        provenance: Arc<ProgramProvenance>,
    ) -> Self {
        RootCustody {
            handle: Some(handle),
            cleanup: CustodyLease::new(cleanup),
            provenance,
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
            cleanup: self.cleanup.clone(),
            provenance: Arc::clone(&self.provenance),
            committed: false,
        }
    }
}

impl Drop for RootCustody {
    fn drop(&mut self) {
        if let Some(handle) = self.handle.take() {
            self.cleanup.enqueue(handle);
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
    cleanup: CustodyLease,
}

/// One installed startup program awaiting its actor's durable application
/// boundary. The capsule owns its original install pin and cannot be cloned.
#[must_use]
#[derive(Debug)]
pub struct PreparedStartupEntry {
    program: Option<ProgramId>,
    source_keys: tidepool_codegen::binding_table::SourceScopeAdmission,
    provenance: Arc<ProgramProvenance>,
    admitted: super::PublicVisibilitySnapshot,
    realm: RealmId,
    cleanup: Arc<CustodyCleanup>,
    _lease: BindingLease,
    compile_identity: StartupCompileIdentity,
}

#[derive(Debug)]
enum StartupCompileIdentity {
    Issued(Arc<tidepool_toolchain::artifacts::SealedOriginalCompileInput>),
    #[cfg(test)]
    Fixture,
}

impl PreparedStartupEntry {
    /// Replay-eligible input observation for journal continuity. Completed
    /// originals with untracked inputs can execute without this observation.
    pub fn compile_input_identity(&self) -> Option<&str> {
        match &self.compile_identity {
            StartupCompileIdentity::Issued(identity) => identity.replay_eligible_identity(),
            #[cfg(test)]
            StartupCompileIdentity::Fixture => Some("test-startup-fixture"),
        }
    }
}

static_assertions::assert_not_impl_any!(PreparedStartupEntry: Clone, Copy);

#[derive(Debug)]
struct AbandonedStartupEntry {
    program: ProgramId,
    scope: ScopeId,
    source_keys: tidepool_codegen::binding_table::SourceScopeAdmission,
}

impl Drop for PreparedStartupEntry {
    fn drop(&mut self) {
        if let Some(program) = self.program.take() {
            self.cleanup
                .startup_entries
                .lock()
                .push(AbandonedStartupEntry {
                    program,
                    scope: self.admitted.scope,
                    source_keys: std::mem::take(&mut self.source_keys),
                });
        }
        // The existing binding lease releases custody and signals cleanup
        // after the abandoned native program has entered the queue.
    }
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

/// Counts external ownership independently of temporary Arc references. Drop
/// implementations enqueue their cleanup first; this field then releases the
/// count before waking the owner, including committed transfers.
#[derive(Debug)]
struct CustodyLease(Arc<CustodyCleanup>);

impl CustodyLease {
    fn new(cleanup: Arc<CustodyCleanup>) -> Self {
        cleanup.external_leases.fetch_add(1, Ordering::Relaxed);
        Self(cleanup)
    }
}

impl Clone for CustodyLease {
    fn clone(&self) -> Self {
        Self::new(Arc::clone(&self.0))
    }
}

impl std::ops::Deref for CustodyLease {
    type Target = CustodyCleanup;
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl Drop for CustodyLease {
    fn drop(&mut self) {
        let previous = self.external_leases.fetch_sub(1, Ordering::AcqRel);
        debug_assert!(previous > 0, "external custody accounting underflow");
        self.notify_cleanup();
    }
}

#[derive(Default)]
struct CustodyCleanup {
    abandoned: Mutex<Vec<ValueHandle>>,
    binding_leases: Mutex<Vec<Vec<SessionVarId>>>,
    notify: Mutex<Option<Arc<dyn Fn() + Send + Sync>>>,
    external_leases: AtomicUsize,
    startup_entries: Mutex<Vec<AbandonedStartupEntry>>,
}

impl CustodyCleanup {
    fn enqueue(&self, handle: ValueHandle) {
        self.abandoned.lock().push(handle);
    }

    fn notify_cleanup(&self) {
        let notify = self.notify.lock().clone();
        if let Some(notify) = notify {
            notify();
        }
    }

    fn set_notifier(&self, notify: Arc<dyn Fn() + Send + Sync>) {
        *self.notify.lock() = Some(notify);
    }

    fn take_all(&self) -> Vec<ValueHandle> {
        std::mem::take(&mut *self.abandoned.lock())
    }
}

impl std::fmt::Debug for CustodyCleanup {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CustodyCleanup")
            .field("abandoned", &self.abandoned.lock().len())
            .field("binding_leases", &self.binding_leases.lock().len())
            .field("has_notifier", &self.notify.lock().is_some())
            .finish()
    }
}

struct CustodyTransfer {
    handle: ValueHandle,
    cleanup: CustodyLease,
    provenance: Arc<ProgramProvenance>,
    committed: bool,
}

impl CustodyTransfer {
    fn commit(mut self) {
        self.committed = true;
    }

    fn into_custody(mut self) -> RootCustody {
        self.committed = true;
        RootCustody::new(
            self.handle,
            Arc::clone(&self.cleanup.0),
            Arc::clone(&self.provenance),
        )
    }
}

impl Drop for CustodyTransfer {
    fn drop(&mut self) {
        if !self.committed {
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
    checked: Option<Arc<CheckedTurnCompletion>>,
}

/// See [`ResidentHole`]'s doc — the `Binding` variant's payload.
#[derive(Clone, Debug)]
pub struct BindingHole {
    id: String,
    binder: BoundBinder,
    generation: Generation,
    observation: Option<Vec<tidepool_repr::VarId>>,
    lexical_scope: ScopeId,
    checked: Option<Arc<CheckedTurnCompletion>>,
}

/// See [`ResidentHole`]'s doc — a projected pattern bind retains every GHC
/// binder and its one shared value generation across suspension.
#[derive(Clone, Debug)]
pub struct ProjectedBindingHole {
    id: String,
    binders: Vec<BoundBinder>,
    generation: Generation,
    lexical_scope: ScopeId,
    checked: Option<Arc<CheckedTurnCompletion>>,
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
        let HoleSeed {
            obligation,
            checked,
        } = seed;
        match obligation {
            HoleObligation::Plain => ResidentHole::Plain(PlainHole { id, checked }),
            HoleObligation::Binding {
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
                checked,
            }),
            HoleObligation::ProjectedBinding {
                binders,
                generation,
                lexical_scope,
            } => ResidentHole::ProjectedBinding(ProjectedBindingHole {
                id,
                binders,
                generation,
                lexical_scope,
                checked,
            }),
        }
    }

    /// This hole's own seed — what [`ResidentSession::resume`] re-mints a
    /// fresh hole as, should this resume re-suspend: a Binding hole's chain
    /// of re-suspensions all carry the SAME binder/generation/scope through to
    /// whichever one finally completes.
    fn seed(&self) -> HoleSeed {
        let (obligation, checked) = match self {
            ResidentHole::Plain(h) => (HoleObligation::Plain, h.checked.clone()),
            ResidentHole::Binding(h) => (
                HoleObligation::Binding {
                    binder: h.binder.clone(),
                    generation: h.generation,
                    observation: h.observation.clone(),
                    lexical_scope: h.lexical_scope,
                },
                h.checked.clone(),
            ),
            ResidentHole::ProjectedBinding(h) => (
                HoleObligation::ProjectedBinding {
                    binders: h.binders.clone(),
                    generation: h.generation,
                    lexical_scope: h.lexical_scope,
                },
                h.checked.clone(),
            ),
        };
        HoleSeed {
            obligation,
            checked,
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
        ResidentHole::Plain(PlainHole {
            id: cont_id.into(),
            checked: None,
        })
    }
}

/// The id-free completion obligation and optional checked owner retained
/// when [`ResidentSession::classify_parked`] mints a hole. A completion owner
/// accompanies exactly one obligation; it cannot wrap another checked seed.
#[derive(Clone)]
struct HoleSeed {
    obligation: HoleObligation,
    checked: Option<Arc<CheckedTurnCompletion>>,
}

#[derive(Clone)]
enum HoleObligation {
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
    /// A handler claimed this request and prepared owned external work. The
    /// frame remains parked; a host runs `work` and resumes `hole` with its
    /// structural response after the machine checkout has been released.
    Deferred {
        output: Vec<String>,
        hole: ResidentHole,
        request: HaskellValue,
        work: DeferredEffect,
    },
}

/// Exact continuation changes made during one checked-out host operation.
/// A host can attach an observer while it owns the checkout, so a cancelled
/// caller still learns about a suspension produced by a late blocking run.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ResidentContinuationEvent {
    Parked(String),
    Retired(String),
}

/// Why a resident-session operation was refused or failed.
#[derive(thiserror::Error, Debug)]
pub enum ResidentError {
    /// Checked items must enter a route that validates their sealed recipe
    /// and owns successful prefix settlement.
    #[error("checked turn requires its authenticated execution route")]
    UnsupportedCheckedTurn,
    #[error(transparent)]
    BindingAlias(#[from] BindingAliasError),
    /// A rooted value minted by another resident session was presented to
    /// this machine. Handle ids are session-local and must never be resolved
    /// by numeric coincidence.
    #[error("root custody belongs to a different resident session")]
    ForeignCustody,
    #[error("activation site {site} lacks its original live input/type evidence")]
    InvalidActivationInput { site: u64 },
    #[error("activation site {site} lacks its original canonical input type witness")]
    MissingActivationInputWitness { site: u64 },
    #[error("activation site {site} has multiple original execution contexts: {contexts:?}")]
    AmbiguousActivationInputOriginalContext { site: u64, contexts: Vec<[u8; 32]> },
    #[error("activation site {site} lacks compiler-authenticated original input metadata")]
    UnauthenticatedActivationInputWitness { site: u64 },
    #[error("activation site {site} canonical input type differs: original {original:?}, compiled {compiled:?}")]
    ActivationInputTypeMismatch {
        site: u64,
        original: [u8; 32],
        compiled: [u8; 32],
    },
    #[error("activation input {binding:?} was consumed before mount settlement failed: {source}")]
    ActivationInputConsumed {
        binding: SessionVarId,
        #[source]
        source: Box<ResidentError>,
    },
    #[error("activation input {binding:?} is mounted but subsequent preparation failed: {source}")]
    ActivationMountCommitted {
        binding: SessionVarId,
        #[source]
        source: Box<ResidentError>,
    },
    #[error("activation preview {binding:?} no longer has its original mounted input and compiler owner")]
    ActivationPreviewRefused { binding: SessionVarId },
    #[error("prepared startup entry no longer has its original native authority")]
    StaleStartupEntry,
    #[error("startup entry lacks its sealed compiler bundle identity")]
    UnsealedStartupEntry,
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
    /// Which workbench failure layer this error belongs to — see
    /// [`crate::session::workbench::WorkbenchFailureLayer`]. Install errors
    /// are refused before this prepared program runs; earlier input units may
    /// already have committed effects. `None` covers an ordinary
    /// program-language fault (a pattern match failure, a case trap, a
    /// bootstrap or table-merge failure) that never reached an effect
    /// boundary or the observation step, and any error this classification
    /// does not yet cover.
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
            Self::Prepared(prepared) => match prepared.stage() {
                super::prepared::PreparedFailureStage::Install => {
                    Some(WorkbenchFailureLayer::Install)
                }
                super::prepared::PreparedFailureStage::Run => match prepared {
                    PreparedRuntimeError::Run(
                        tidepool_codegen::prepared_program::ExecutionError::Observation(_),
                    ) => Some(WorkbenchFailureLayer::Observation),
                    PreparedRuntimeError::Handler { .. } => Some(WorkbenchFailureLayer::Effect),
                    _ => None,
                },
            },
            _ => None,
        }
    }
}

#[cfg(test)]
mod failure_layer_tests {
    use super::*;
    use crate::session::prepared::PreparedFailureStage;
    use crate::session::workbench::WorkbenchFailureLayer;
    use tidepool_codegen::prepared_program::{ExecutionError, ObservationFailure};

    #[test]
    fn prepared_install_refusals_have_the_install_layer() {
        let install =
            ResidentError::Prepared(PreparedRuntimeError::Install(ExecutionError::NotQuiescent));
        let compile = ResidentError::Prepared(PreparedRuntimeError::Compile(
            tidepool_codegen::prepared_program::CompileError::RootBlock,
        ));

        assert_eq!(
            install.failure_layer(),
            Some(WorkbenchFailureLayer::Install)
        );
        assert_eq!(
            compile.failure_layer(),
            Some(WorkbenchFailureLayer::Install)
        );
        assert_eq!(
            PreparedRuntimeError::Install(ExecutionError::NotQuiescent).stage(),
            PreparedFailureStage::Install
        );
    }

    #[test]
    fn prepared_run_observation_and_language_layers_keep_their_meaning() {
        let observation = ResidentError::Prepared(PreparedRuntimeError::Run(
            ExecutionError::Observation(ObservationFailure::AllocationFailed),
        ));
        let language =
            ResidentError::Prepared(PreparedRuntimeError::Run(ExecutionError::NotQuiescent));

        assert_eq!(
            observation.failure_layer(),
            Some(WorkbenchFailureLayer::Observation)
        );
        assert_eq!(language.failure_layer(), None);
        assert_eq!(
            PreparedRuntimeError::Run(ExecutionError::NotQuiescent).stage(),
            PreparedFailureStage::Run
        );
    }
}

enum ValueInterfaceSource {
    /// Ordinary checked settlement retains the interface in its deferred prefix.
    Checked,
    /// Checked native mounts commit this owned certificate with the binding.
    Staged(super::persistent::StagedCheckedValueInterface),
    LegacyDisk,
}

impl ValueInterfaceSource {
    fn for_checked(is_checked: bool) -> Self {
        if is_checked {
            Self::Checked
        } else {
            Self::LegacyDisk
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
    snapshot: PendingPreparedSource,
    metadata: PreparedInstallMetadata,
}

struct PreparedInstallMetadata {
    mode: PendingPreparedMode,
    argument: Option<PreparedHandle>,
    provenance: Arc<ProgramProvenance>,
    realm: RealmId,
    lexical_scope: ScopeId,
    park: ParkPolicy,
    checked: Option<PreparedCheckedTurn>,
}

struct PreparedCheckedTurn {
    prefix: Arc<super::RuntimeCheckedPrefix>,
    execution: Arc<tidepool_toolchain::checked_cell::ExactCompiledItem>,
}

enum PreparedExecutionPurpose {
    Authored(Option<Arc<CheckedTurnCompletion>>),
    HostActivationPreview(Arc<super::RuntimeLexicalScopeLease>),
}

fn checked_turn_plan(
    certification: &Option<super::turn::TurnCertification>,
    prepared: &PreparedProgram,
    table: &DataConTable,
    mode: &PreparedTurnMode<'_>,
) -> Result<Option<PreparedCheckedTurn>, ResidentError> {
    let Some(certification) = certification else {
        return Ok(None);
    };
    if certification.checked_activation_preview().is_some() {
        return Err(ResidentError::UnsupportedCheckedTurn);
    }
    certification
        .validate_checked_table(table)
        .map_err(SessionError::Compile)?;
    let (generation, binders) = match mode {
        PreparedTurnMode::Value => (
            certification
                .checked_execution()
                .map_or(0, |execution| execution.generation()),
            &[][..],
        ),
        PreparedTurnMode::Binding {
            binder, generation, ..
        } => (generation.0, std::slice::from_ref(*binder)),
        PreparedTurnMode::Projected {
            binders,
            generation,
        } => (generation.0, *binders),
    };
    certification
        .validate_checked_bind(prepared, generation, binders)
        .map_err(SessionError::Compile)?;
    let TurnPurpose::Execution { execution, prefix } = certification.purpose() else {
        return Ok(None);
    };
    Ok(Some(PreparedCheckedTurn {
        prefix: prefix.clone(),
        execution: execution.clone(),
    }))
}

fn binding_ids_of(mode: &PreparedTurnMode<'_>) -> Vec<SessionVarId> {
    match mode {
        PreparedTurnMode::Value => Vec::new(),
        PreparedTurnMode::Binding { binder, .. } => vec![SessionVarId::from_extract(binder.var_id)],
        PreparedTurnMode::Projected { binders, .. } => binders
            .iter()
            .map(|binder| SessionVarId::from_extract(binder.var_id))
            .collect(),
    }
}

struct ParcelLibraryBinding {
    id: SessionVarId,
    module: SessionModule,
}

fn parcel_library_binding(identity: &SymbolIdentity) -> Option<ParcelLibraryBinding> {
    if identity.unit != HOME_UNIT {
        return None;
    }
    let module = SessionModule::from_module_name(&identity.module)?;
    if module.kind != tidepool_repr::SessionModuleKind::Lib {
        return None;
    }
    Some(ParcelLibraryBinding {
        id: SessionVarId::from_extract(session_var_id(&identity.module, &identity.occurrence)),
        module,
    })
}

fn is_checked_turn(code: &TurnCode<'_>) -> bool {
    code.certification
        .as_ref()
        .as_ref()
        .is_some_and(|certification| !matches!(certification.purpose(), TurnPurpose::Ordinary))
}

fn refuse_checked_turn(code: &TurnCode<'_>) -> Result<(), ResidentError> {
    if is_checked_turn(code) {
        return Err(ResidentError::UnsupportedCheckedTurn);
    }
    Ok(())
}

impl PreparedCheckedTurn {
    fn start(
        self,
        session: &PersistentSession,
        scope: ScopeId,
    ) -> Result<Arc<CheckedTurnCompletion>, SessionError> {
        self.prefix.start(session, scope, self.execution)
    }
}

enum PendingPreparedSource {
    Legacy(super::prepared::InstallSnapshot),
    Certified {
        prepared: PreparedProgram,
        resolved: super::persistent::ResolvedCertifiedTurn,
        registry: Arc<ImageRegistry>,
        admitted_public: super::PublicVisibilitySnapshot,
    },
}

impl PendingPreparedSource {
    fn compile_off_checkout(self) -> Result<ReadyPreparedSource, PreparedRuntimeError> {
        match self {
            Self::Legacy(mut snapshot) => {
                let image = super::prepared::PreparedEngine::compile_off_checkout(&mut snapshot)
                    .map_err(PreparedRuntimeError::Compile)?;
                Ok(ReadyPreparedSource::Legacy { snapshot, image })
            }
            Self::Certified {
                prepared,
                resolved,
                registry,
                admitted_public,
            } => {
                let (target, demanded) = super::prepared::CertifiedTargetImage::compile_scoped(
                    prepared, &resolved, &registry,
                )?;
                Ok(ReadyPreparedSource::Certified {
                    resolved,
                    admitted_public,
                    target,
                    demanded,
                })
            }
        }
    }
}

enum ReadyPreparedSource {
    Legacy {
        snapshot: super::prepared::InstallSnapshot,
        image: Arc<tidepool_codegen::prepared_program::CompiledProgram>,
    },
    Certified {
        resolved: super::persistent::ResolvedCertifiedTurn,
        admitted_public: super::PublicVisibilitySnapshot,
        target: super::prepared::CertifiedTargetImage,
        demanded: Vec<DemandedImage>,
    },
}

/// One compiled install with its original source snapshot and completion
/// metadata. Only consuming a pending install can construct this capsule.
pub struct ReadyPreparedInstall {
    source: ReadyPreparedSource,
    metadata: PreparedInstallMetadata,
}

impl PendingPreparedInstall {
    /// Compile this install off checkout, consuming its source and completion
    /// metadata into one capsule for final revalidation and execution.
    pub fn compile_off_checkout(self) -> Result<ReadyPreparedInstall, PreparedRuntimeError> {
        let source = self.snapshot.compile_off_checkout()?;
        Ok(ReadyPreparedInstall {
            source,
            metadata: self.metadata,
        })
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
    /// The turn requested a typed effect: its continuation is parked under
    /// `id` in the machine's ledger and `request` is the observed request,
    /// ready for the host to route.
    Suspended {
        id: ContinuationId,
        request: HaskellValue,
    },
    Deferred {
        id: ContinuationId,
        request: HaskellValue,
        work: DeferredEffect,
    },
}

/// The hole a suspension of a turn run in `mode` mints, carrying its
/// completion obligation forward across resumes.
fn hole_seed_of(
    mode: &PreparedTurnMode<'_>,
    lexical_scope: ScopeId,
    checked: Option<Arc<CheckedTurnCompletion>>,
) -> HoleSeed {
    let obligation = match mode {
        PreparedTurnMode::Value => HoleObligation::Plain,
        PreparedTurnMode::Binding {
            binder,
            generation,
            observation,
        } => HoleObligation::Binding {
            binder: (*binder).clone(),
            generation: *generation,
            observation: observation.clone(),
            lexical_scope,
        },
        PreparedTurnMode::Projected {
            binders,
            generation,
        } => HoleObligation::ProjectedBinding {
            binders: binders.to_vec(),
            generation: *generation,
            lexical_scope,
        },
    };
    HoleSeed {
        obligation,
        checked,
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
}

/// How deep [`render_retained_layer`] recurses into nested constructors
/// before cutting with `…`, independent of the byte budget -- bounds
/// the walk against a deeply nested value even when each layer prints short.
const RETAINED_PREVIEW_MAX_DEPTH: usize = 8;

/// The note [`truncate_preview_at_line`] appends after a cut: it names the
/// budget, so a reader never mistakes a cut preview for the whole value, and
/// says where the rest is. A preview within budget carries no note.
fn preview_truncated_note(budget: usize) -> String {
    let budget = if budget.is_multiple_of(1024) {
        format!("{} KiB", budget / 1024)
    } else {
        format!("{budget}-byte")
    };
    format!(
        "\n[reply exceeds the {budget} notice budget; preview truncated. \
         `pollResponse` on the retained `Response` has the complete value.]"
    )
}

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

/// Enforce a byte budget including the omission notice. Reserve the notice
/// first, then prefer a whole-line prefix and otherwise a UTF-8 boundary.
/// Small budgets use a compact notice, or `~` when even that does not fit;
/// a zero-byte budget cannot carry either content or an omission notice.
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
    let note = preview_truncated_note(budget);
    let note = if note.len() <= budget {
        note.as_str()
    } else if budget >= "[reply omitted; pollResponse]".len() {
        "[reply omitted; pollResponse]"
    } else if budget >= "[omitted]".len() {
        "[omitted]"
    } else if budget > 0 {
        "~"
    } else {
        ""
    };
    let mut cut = budget - note.len();
    while cut > 0 && !text.is_char_boundary(cut) {
        cut -= 1;
    }
    cut = text[..cut].rfind('\n').unwrap_or(cut);
    let mut truncated = text[..cut].to_string();
    truncated.push_str(note);
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

/// Apply a rooted `Int -> Eff effects a` closure to `argument` through the shared
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
        let dispatch = if park.effect_policy == EffectRunPolicy::SuspendAll {
            EffectDispatch::Unhandled
        } else {
            let cx = EffectContext::with_principal(table, park.principal, captured);
            match handlers.prepare_dispatch(&parked.request, &cx) {
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
        let response = match dispatch {
            EffectDispatch::Unhandled => {
                return Ok(PreparedRun::Suspended {
                    id: parked.id,
                    request: parked.request,
                });
            }
            EffectDispatch::Immediate(response) => response,
            EffectDispatch::Deferred(work) => {
                return Ok(PreparedRun::Deferred {
                    id: parked.id,
                    request: parked.request,
                    work,
                });
            }
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
        // Transient entries materialize their result under a bounded observation
        // budget. Cuts use the oversize sentinel without invalidating completed
        // effects; persistent bindings use Bind or Project below.
        SettlePlan::Observe => match engine.observe_bounded(program, handle) {
            Ok(value) => Ok(PreparedRun::Done { handle, value }),
            Err(error) => {
                engine.release(handle);
                Err(error)
            }
        },
        // Exhausting the observation budget does not invalidate a retained
        // binding or its completed effects. Keep the handle and report the cut.
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

type ContinuationResourceOwner = Arc<dyn std::any::Any + Send + Sync>;

struct ParkedContinuation {
    name: String,
    id: ContinuationId,
    provenance: Arc<ProgramProvenance>,
    // The native frame owns this lease until exact retirement or machine loss.
    resource_owners: Vec<ContinuationResourceOwner>,
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
    /// The parked holes and their resource leases, insertion-ordered. The
    /// machine's continuation
    /// registry is the ground truth; these are the string identities callers
    /// resume/abort against (atomic validate-before-consume). Top = last.
    parked: Vec<ParkedContinuation>,
    continuation_resource_owner: Option<ContinuationResourceOwner>,
    continuation_observer: Option<Arc<dyn Fn(ResidentContinuationEvent) + Send + Sync>>,
    binding_provenance: HashMap<u64, Arc<ProgramProvenance>>,
    /// Host-owned text identities for materialized bindings whose equality is
    /// meaningful to a caller (currently retained command jobs). The binding
    /// table remains the owner of reachability and scope retirement; this map
    /// only records a payload identity for deduplication.
    host_text_bindings: HashMap<SessionVarId, String>,
    /// Private request carriers remain rooted for closures that captured them,
    /// but never become ordinary unqualified workbench vocabulary.
    hidden_host_bindings: HashMap<SessionVarId, ()>,
    /// At most one exact source contract per fixed host kind. Outstanding
    /// instances retain their superseded prototype through their admission.
    host_binding_prototypes: Vec<Arc<HostBindingPrototype>>,
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
    /// Scope every native call made by `action` to this invocation's flag.
    /// Resource retirement remains independently active. Restore the previous
    /// selection even if `action` unwinds; no cancellation flag is reset.
    pub fn with_invocation_cancel<R>(
        &mut self,
        cancel: Arc<AtomicBool>,
        action: impl FnOnce(&mut Self) -> R,
    ) -> R {
        let previous = self.state.replace_invocation_cancel(Some(cancel));
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| action(self)));
        self.state.replace_invocation_cancel(previous);
        match result {
            Ok(result) => result,
            Err(panic) => std::panic::resume_unwind(panic),
        }
    }

    /// Observe the interpreters installed on this exact machine. The caller
    /// holds the session checkout; this does not dispatch an effect or grant
    /// authority over any handler-owned resource.
    #[must_use]
    pub fn handlers(&self) -> &H {
        &self.handlers
    }

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
        Self::with_state(handlers, captured, state)
    }

    fn with_state(handlers: H, captured: O, state: PersistentSession) -> Self {
        ResidentSession {
            state,
            handlers,
            captured,
            cont_id_issuer: MonotonicIdIssuer::new("scont"),
            parked: Vec::new(),
            continuation_resource_owner: None,
            continuation_observer: None,
            binding_provenance: HashMap::new(),
            host_text_bindings: HashMap::new(),
            hidden_host_bindings: HashMap::new(),
            host_binding_prototypes: Vec::new(),
            run_context: SessionRunContext::ROOT,
            custody_cleanup: Arc::new(CustodyCleanup::default()),
        }
    }

    #[cfg(test)]
    pub(crate) fn from_persistent_for_test(
        handlers: H,
        captured: O,
        state: PersistentSession,
    ) -> Self {
        Self::with_state(handlers, captured, state)
    }

    /// Share `registry` with this session's machine, once it has one
    /// (`PreparedEngine::set_image_registry`) -- a no-op before the first
    /// turn bootstraps it, since there is no machine yet to share an image
    /// with. The composition root that owns a run's sibling sessions is the
    /// intended caller.
    pub fn set_catalog_selection(
        &mut self,
        catalog: tidepool_toolchain::toolchain::CatalogSelection,
    ) {
        self.state.set_catalog_selection(catalog);
    }

    pub fn set_image_registry(&mut self, registry: Arc<ImageRegistry>) {
        self.state.set_image_registry(registry);
    }

    pub fn stage_published_source_originals_in(
        &mut self,
        scope: ScopeId,
        selection: Arc<tidepool_toolchain::artifacts::PublishedSourceOriginalSelection>,
    ) -> Result<super::PendingPublishedSourceOriginals, ResidentError> {
        Ok(self
            .state
            .stage_published_source_originals_in(scope, selection)?)
    }

    pub fn publish_source_originals(
        &mut self,
        pending: super::PendingPublishedSourceOriginals,
    ) -> Result<(), ResidentError> {
        Ok(self.state.publish_source_originals(pending)?)
    }

    fn advance_public_visibility(&mut self, scope: ScopeId) {
        self.state.advance_public_visibility(scope);
    }

    /// Capture the full declaration, binding and source-instance authority
    /// under the caller's machine checkout. Model-visible filtering belongs
    /// to [`Self::workbench_bindings_in`] and never changes this identity.
    pub fn public_visibility_snapshot_in(
        &self,
        scope: ScopeId,
    ) -> Option<super::PublicVisibilitySnapshot> {
        self.state.public_visibility_snapshot_in(scope)
    }

    pub fn begin_private_execution(
        &mut self,
        public: ScopeId,
    ) -> Result<super::PrivateExecutionAdmission, SessionError> {
        self.settle_dropped_custody();
        self.state.begin_private_execution(public)
    }

    pub fn begin_ephemeral_private_execution(
        &mut self,
        public: ScopeId,
    ) -> Result<super::PrivateExecutionAdmission, SessionError> {
        self.settle_dropped_custody();
        self.state.begin_ephemeral_private_execution(public)
    }

    pub fn bind_durable_public_scope(
        &mut self,
        owner: super::RecoveryPublicOwner,
        scope: ScopeId,
    ) -> Result<(), SessionError> {
        self.state.bind_durable_public_scope(owner, scope)
    }

    pub fn initialize_durable_public_scope(
        &mut self,
        owner: super::RecoveryPublicOwner,
        scope: ScopeId,
    ) -> Result<super::PublicManifestCommit, SessionError> {
        self.state.initialize_durable_public_scope(owner, scope)
    }

    pub fn begin_durable_public_bootstrap(
        &mut self,
        owner: super::RecoveryPublicOwner,
        scope: ScopeId,
    ) -> Result<super::DurablePublicBootstrap, SessionError> {
        self.settle_dropped_custody();
        self.state.begin_durable_public_bootstrap(owner, scope)
    }

    pub fn publish_durable_public_bootstrap(
        &mut self,
        bootstrap: super::DurablePublicBootstrap,
    ) -> Result<super::PublicManifestCommit, SessionError> {
        self.settle_dropped_custody();
        self.state.publish_durable_public_bootstrap(bootstrap)
    }

    pub fn validate_recovered_public_owner(
        &self,
        owner: &super::RecoveryPublicOwner,
    ) -> Result<bool, SessionError> {
        self.state.validate_recovered_public_owner(owner)
    }

    pub fn transfer_recovered_public_owner(
        &mut self,
        predecessor: &super::RecoveryPublicOwner,
        successor: super::RecoveryPublicOwner,
        scope: ScopeId,
        authority: Arc<dyn super::RecoverySuccessorAuthority>,
    ) -> Result<super::PublicManifestCommit, SessionError> {
        self.state
            .transfer_recovered_public_owner(predecessor, successor, scope, authority)
    }

    pub fn seal_recovery_initialization_scope(
        &mut self,
        scope: ScopeId,
    ) -> Result<(), SessionError> {
        self.state.seal_recovery_initialization_scope(scope)
    }

    pub fn confirm_publication_durability(&mut self) -> Result<(), SessionError> {
        if !self.state.has_lib() {
            return Err(SessionError::MissingDeclarationLibrary);
        }
        self.state.lib_mut().confirm_recovery_durability()
    }

    pub fn durable_public_readiness(
        &self,
        owner: &super::RecoveryPublicOwner,
        scope: ScopeId,
    ) -> Result<Arc<super::RuntimeDurablePublicReadiness>, SessionError> {
        self.state.durable_public_readiness(owner, scope)
    }

    pub fn confirm_durable_public_scope(
        &mut self,
        owner: &super::RecoveryPublicOwner,
        scope: ScopeId,
    ) -> Result<(), SessionError> {
        self.state.confirm_durable_public_scope(owner, scope)
    }

    /// Reserve a fresh interface for one original input. No authored cell or
    /// prepared program is needed to bind the already rooted heap value.
    pub fn admit_activation_input_in(
        &mut self,
        scope: ScopeId,
        input: RuntimeActivationInput,
    ) -> Result<RuntimeActivationInputAdmission, ResidentError> {
        self.settle_dropped_custody();
        if !Arc::ptr_eq(&input.custody.cleanup.0, &self.custody_cleanup) {
            return Err(ResidentError::ForeignCustody);
        }
        let invalid = || ResidentError::InvalidActivationInput { site: input.site };
        let raw = input.custody.handle.ok_or_else(invalid)?;
        if self.run_context.lexical_scope != scope
            || self
                .state
                .require_prepared()?
                .prepared_handle_of(raw)
                .is_none()
        {
            return Err(invalid());
        }
        let mut digest = blake3::Hasher::new();
        digest.update(b"TidepoolOriginalLiveInput2");
        digest.update(&raw.0.to_le_bytes());
        digest.update(&input.site.to_le_bytes());
        digest.update(&input.type_evidence.commitment());
        digest.update(&input.input_type_witness.commitment());
        digest.update(&input.input_type_witness.metadata_digest());
        digest.update(&self.run_context.resource_scope.0.to_le_bytes());
        digest.update(&self.run_context.principal.identity.to_le_bytes());
        digest.update(&self.run_context.principal.incarnation.to_le_bytes());
        let reservation = self.state.admit_binding_interface(
            scope,
            "sessionInput".into(),
            input.prototype.clone(),
            *digest.finalize().as_bytes(),
        )?;
        Ok(RuntimeActivationInputAdmission {
            input,
            reservation,
            run_context: self.run_context,
        })
    }

    /// Admit one compiler-issued binder reserved for a native host payload.
    pub fn admit_host_carrier_cell_in(
        &mut self,
        scope: ScopeId,
        plan: Arc<tidepool_toolchain::cell_plan::ParsedCellPlan>,
        specification: Arc<dyn std::any::Any + Send + Sync>,
        specification_digest: [u8; 32],
        authority_digest: [u8; 32],
        include_paths: Vec<PathBuf>,
        binding: String,
        expected: HostBindingType,
        compile_inputs: Option<super::prepared::RuntimeCompileInputs>,
    ) -> Result<Arc<super::RuntimeCellAdmission>, SessionError> {
        self.settle_dropped_custody();
        self.state.admit_host_carrier_cell_in(
            scope,
            plan,
            specification,
            specification_digest,
            authority_digest,
            include_paths,
            binding,
            expected,
            compile_inputs,
        )
    }

    pub fn begin_durable_private_execution(
        &mut self,
        owner: &super::RecoveryPublicOwner,
        public_scope: ScopeId,
    ) -> Result<super::PrivateExecutionAdmission, SessionError> {
        self.settle_dropped_custody();
        self.state
            .begin_durable_private_execution(owner, public_scope)
    }

    pub fn begin_cell_program(
        &self,
        admission: Arc<super::RuntimeCellAdmission>,
        program: Arc<tidepool_toolchain::checked_cell::CellProgram>,
    ) -> Result<Option<Arc<super::RuntimeCheckedPrefix>>, SessionError> {
        self.state.begin_cell_program(admission, program)
    }

    pub fn admit_native_setup_cell_in(
        &mut self,
        scope: ScopeId,
        plan: Arc<tidepool_toolchain::cell_plan::ParsedCellPlan>,
        specification: Arc<dyn std::any::Any + Send + Sync>,
        specification_digest: [u8; 32],
        authority_digest: [u8; 32],
        include_paths: Vec<PathBuf>,
        compile_inputs: Option<super::prepared::RuntimeCompileInputs>,
    ) -> Result<Arc<super::RuntimeCellAdmission>, SessionError> {
        self.settle_dropped_custody();
        self.state.admit_native_setup_cell_in(
            scope,
            plan,
            specification,
            specification_digest,
            authority_digest,
            include_paths,
            compile_inputs,
        )
    }

    pub fn admit_planned_cell_for_execution(
        &mut self,
        execution: Arc<super::PrivateExecutionAdmission>,
        plan: Arc<tidepool_toolchain::cell_plan::ParsedCellPlan>,
        specification: Arc<dyn std::any::Any + Send + Sync>,
        specification_digest: [u8; 32],
        authority_digest: [u8; 32],
        include_paths: Vec<PathBuf>,
        compile_inputs: Option<super::prepared::RuntimeCompileInputs>,
    ) -> Result<Arc<super::RuntimeCellAdmission>, SessionError> {
        self.settle_dropped_custody();
        self.state.admit_planned_cell_for_execution(
            execution,
            plan,
            specification,
            specification_digest,
            authority_digest,
            include_paths,
            compile_inputs,
        )
    }

    pub fn admit_checked_item(
        &mut self,
        prefix: Arc<super::RuntimeCheckedPrefix>,
        item: tidepool_toolchain::checked_cell::ExactCheckedItem,
    ) -> Result<Arc<super::RuntimeCheckedItemAdmission>, SessionError> {
        self.settle_dropped_custody();
        self.state.admit_checked_item(prefix, item)
    }

    pub fn adopt_checked_declaration(
        &mut self,
        admission: Arc<super::RuntimeCheckedItemAdmission>,
    ) -> Result<super::DeclarationPlaneCommit, SessionError> {
        if admission.prefix().admission().visibility().scope != self.run_context.lexical_scope {
            return Err(SessionError::StaleStagedDeclaration);
        }
        self.settle_dropped_custody();
        self.state.adopt_checked_declaration(admission)
    }

    pub fn freeze_execution_intent(
        &mut self,
        admission: &super::PrivateExecutionAdmission,
        writes: Vec<SessionVarId>,
        sources: Vec<tidepool_codegen::binding_table::SourceLeaseKey>,
    ) -> Result<Arc<super::FinalExecutionIntent>, SessionError> {
        self.settle_dropped_custody();
        self.state
            .freeze_execution_intent(admission, writes, sources)
    }

    /// Finalize the current writes selected by this resident owner. Private
    /// host input carriers retain native dependencies but never become public
    /// value heads.
    pub fn freeze_private_execution(
        &mut self,
        admission: &super::PrivateExecutionAdmission,
    ) -> Result<Arc<super::FinalExecutionIntent>, SessionError> {
        self.settle_dropped_custody();
        let scope = admission.private_scope();
        let snapshot = self
            .state
            .public_visibility_snapshot_in(scope)
            .ok_or(SessionError::DeadScope(scope))?;
        let completed = admission.completed_values.lock();
        let writes = snapshot
            .bindings
            .iter()
            .filter_map(|(_, id)| {
                self.state.bindings().get(*id).and_then(|entry| {
                    (entry.scope == scope
                        && !self.hidden_host_bindings.contains_key(id)
                        && completed.get(id).is_some_and(|proof| proof.matches(entry)))
                    .then_some(*id)
                })
            })
            .collect();
        self.state.freeze_execution_intent_locked(
            admission,
            writes,
            snapshot.source_instances,
            &completed,
        )
    }

    pub fn restage_execution_publication(
        &mut self,
        owner: super::RecoveryPublicOwner,
        intent: Arc<super::FinalExecutionIntent>,
    ) -> Result<super::ExecutionPublication, SessionError> {
        self.state.restage_execution_publication(owner, intent)
    }

    pub fn restage_ephemeral_execution_publication(
        &mut self,
        intent: Arc<super::FinalExecutionIntent>,
    ) -> Result<super::ExecutionPublication, SessionError> {
        self.settle_dropped_custody();
        self.state.restage_ephemeral_execution_publication(intent)
    }

    pub fn revalidate_declaration_rejection(
        &self,
        rejected: &super::RejectedDeclarationPublication,
    ) -> Result<super::DeclarationPublicationRejection, SessionError> {
        self.state.revalidate_declaration_rejection(rejected)
    }

    /// Stage only the original input mounted under this exact private admission.
    /// Binding promotion retains the producer's native reachability; pure preview
    /// programs remain private to their detached scope leases.
    pub fn snapshot_host_binding_publication(
        &mut self,
        execution: &super::PrivateExecutionAdmission,
        input: &RuntimeActivationPreviewAdmission,
    ) -> Result<super::PublicManifestBase, ResidentError> {
        self.settle_dropped_custody();
        self.state.compile_view_for_execution(execution)?;
        self.validate_mounted_activation_input(&input.mounted)?;
        if input.mounted.scope != execution.private_scope()
            || !Arc::ptr_eq(&input.owner, self.state.admission_owner())
            || input.owner_epoch != self.state.admission_owner().epoch()
            || self
                .current_decl_heads_in(execution.admitted_public().scope)
                .iter()
                .any(|(name, _)| name == "sessionInput")
        {
            return Err(ResidentError::ActivationPreviewRefused {
                binding: input.mounted.binding,
            });
        }
        Ok(self.state.snapshot_publication_target(
            execution.durable_owner.clone(),
            execution.admitted_public().scope,
            execution.private_scope(),
            vec![input.mounted.binding],
            Vec::new(),
        )?)
    }

    pub fn publish_staged_public_manifest(
        &mut self,
        ticket: super::StagedPublicManifest,
        decision: &Arc<super::PublicationDecision>,
    ) -> Result<super::PublicManifestCommit, SessionError> {
        self.state.publish_staged_public_manifest(ticket, decision)
    }

    pub fn publish_staged_public_manifest_admitted(
        &mut self,
        ticket: super::StagedPublicManifest,
        claim: impl FnOnce() -> Option<super::PublicationClaim>,
    ) -> Result<super::PublicManifestCommit, SessionError> {
        self.state
            .publish_staged_public_manifest_admitted(ticket, claim)
    }

    /// Admit one compiler-certified target and its demanded source closure
    /// against a single scope snapshot under this session's machine checkout.
    /// The persistent registrar owns every new source root before the target
    /// can be run; failed registration rolls the native batch back.
    pub(crate) fn install_certified_turn_in(
        &mut self,
        scope: ScopeId,
        target: super::prepared::CertifiedTargetImage,
        target_owners: &[ImportOwner],
        source_evidence: &BTreeMap<SourceBinder, (CachedHomeOwner, u32)>,
        demanded: Vec<DemandedImage>,
        inherited_needed: &[InheritedSourceDemand],
    ) -> Result<
        (
            ProgramId,
            tidepool_codegen::binding_table::SourceScopeAdmission,
        ),
        PreparedRuntimeError,
    > {
        let (program, source_keys) = self.state.install_certified_turn_in(
            scope,
            target,
            target_owners,
            source_evidence,
            demanded,
            inherited_needed,
        )?;
        if !source_keys.is_empty() {
            self.advance_public_visibility(scope);
        }
        Ok((program, source_keys))
    }

    fn retire_failed_turn_source_instances(
        &mut self,
        scope: ScopeId,
        keys: &tidepool_codegen::binding_table::SourceScopeAdmission,
    ) {
        if !keys.is_empty() {
            assert!(
                self.state.retire_failed_turn_source_instances(scope, keys),
                "failed turn retains its exact newly registered source roots"
            );
            self.advance_public_visibility(scope);
        }
    }

    /// Observe only the continuation events caused by this checkout's host
    /// operation. Restore the previous observer even if the operation panics;
    /// a later checkout must never inherit another caller's cleanup owner.
    pub fn with_continuation_observer<T>(
        &mut self,
        observer: Arc<dyn Fn(ResidentContinuationEvent) + Send + Sync>,
        operation: impl FnOnce(&mut Self) -> T,
    ) -> T {
        let previous = self.continuation_observer.replace(observer);
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| operation(self)));
        self.continuation_observer = previous;
        match result {
            Ok(value) => value,
            Err(panic) => std::panic::resume_unwind(panic),
        }
    }

    /// Retain the supplied host lease on each native frame parked by this
    /// operation. Exact frame retirement, realm closure, or native session
    /// destruction releases it. The lease must not own this session or its
    /// registry. Restore the prior selection even when the operation panics.
    pub fn with_continuation_resource_owner<T>(
        &mut self,
        owner: ContinuationResourceOwner,
        operation: impl FnOnce(&mut Self) -> T,
    ) -> T {
        let previous = self.continuation_resource_owner.replace(owner);
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| operation(self)));
        self.continuation_resource_owner = previous;
        match result {
            Ok(value) => value,
            Err(panic) => std::panic::resume_unwind(panic),
        }
    }

    /// Attach a host lease to the exact already parked frame. Resume carries
    /// it forward; abort, realm closure and machine loss release that owner.
    pub fn retain_continuation_resource_owner(
        &mut self,
        hole: &ResidentHole,
        owner: Arc<dyn std::any::Any + Send + Sync>,
    ) -> Result<(), ResidentError> {
        let frame = self
            .parked
            .iter_mut()
            .find(|frame| frame.name == hole.cont_id())
            .ok_or_else(|| ResidentError::WrongContinuation {
                attempted: hole.cont_id().to_owned(),
                pending: vec![],
            })?;
        if !frame
            .resource_owners
            .iter()
            .any(|old| Arc::ptr_eq(old, &owner))
        {
            frame.resource_owners.push(owner);
        }
        Ok(())
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
        self.define_scoped_in(ScopeId::ROOT, decls)
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
        &mut self,
        scope: ScopeId,
        receipt: &super::DeclarationReceipt,
        imports: &super::SourceImports,
        source_layer: &[PathBuf],
    ) -> Result<super::StagedDeclaration, SessionError> {
        self.state
            .stage_declarations_in(scope, receipt, imports, source_layer)
    }

    /// The checkout-only half of staging a declaration off-checkout: see
    /// [`PersistentSession::render_declaration_candidate_in`].
    pub fn render_declaration_candidate_in(
        &mut self,
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

    /// The compiler-issued cumulative lexical interface for ROOT.
    pub fn session_import_module(&self) -> Option<String> {
        self.session_import_module_in(ScopeId::ROOT)
    }

    /// The actual cumulative declaration interface retained by this scope.
    pub fn session_import_module_in(&self, scope: ScopeId) -> Option<String> {
        self.state.compile_view_in(scope)?.library_import_module()
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

    /// The existing declaration allocator's high-water mark, captured while
    /// the parent owner is checked out. It grants no lexical scope.
    pub fn declaration_generation_high_water(&self) -> Option<Generation> {
        self.state.has_lib().then(|| self.state.lib().generation())
    }

    /// Initialize a fresh declaration allocator before retained compilation.
    /// The library owner persists the fence without changing any scope tip.
    pub fn initialize_captured_declaration_high_water(
        &mut self,
        generation: Generation,
    ) -> Result<(), SessionError> {
        if !self.state.has_lib() {
            return Err(SessionError::MissingDeclarationLibrary);
        }
        self.state
            .lib_mut()
            .initialize_captured_declaration_high_water(generation)
    }

    /// Raise the value allocator past retained native value identities.
    /// Declaration identities have their own allocator and captured fence.
    pub fn set_val_gen(&mut self, generation: Generation) {
        self.state.set_val_gen(generation);
    }

    /// Retain prepared closure dependencies through the existing binding owner.
    /// Reserved future bindings can be leased before they materialize.
    pub fn lease_bindings(&mut self, referenced: &[tidepool_repr::VarId]) -> BindingLease {
        self.settle_dropped_custody();
        let retained = self
            .state
            .acquire_binding_leases(referenced.iter().copied().map(SessionVarId::from_var))
            .into_iter()
            .collect();
        BindingLease {
            retained,
            cleanup: CustodyLease::new(Arc::clone(&self.custody_cleanup)),
        }
    }

    pub fn retain_lexical_scope(
        &mut self,
        source: ScopeId,
    ) -> Result<Arc<super::RuntimeLexicalScopeLease>, ResidentError> {
        self.settle_dropped_custody();
        Ok(self.state.retain_lexical_scope(source)?)
    }

    pub fn mint_scope_from_lease(
        &mut self,
        lease: &super::RuntimeLexicalScopeLease,
    ) -> Result<ScopeId, ResidentError> {
        self.settle_dropped_custody();
        Ok(self.state.mint_scope_from_lease(lease)?)
    }

    pub fn validate_lexical_scope_lease(
        &self,
        scope: ScopeId,
        lease: &super::RuntimeLexicalScopeLease,
    ) -> Result<(), ResidentError> {
        Ok(self.state.validate_lexical_scope_lease(scope, lease)?)
    }

    pub fn validate_initial_lexical_scope(
        &self,
        lease: &super::RuntimeLexicalScopeLease,
        target: ScopeId,
    ) -> Result<(), ResidentError> {
        Ok(self.state.validate_initial_lexical_scope(lease, target)?)
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
        if !Arc::ptr_eq(&lease.cleanup.0, &self.custody_cleanup) {
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
        // A prepared alias shares the source's handle, but a later
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
        self.advance_public_visibility(scope);
        if let Some(provenance) = provenance {
            self.binding_provenance.insert(id.raw(), provenance);
        }
        self.prune_binding_metadata();
        Ok(committed)
    }

    fn prune_binding_metadata(&mut self) {
        self.binding_provenance.retain(|id, _| {
            self.state
                .bindings()
                .get(SessionVarId::from_extract(*id))
                .is_some()
        });
        self.host_text_bindings
            .retain(|id, _| self.state.bindings().get(*id).is_some());
        self.hidden_host_bindings
            .retain(|id, _| self.state.bindings().get(*id).is_some());
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

    #[cfg(test)]
    pub(super) fn retained_checked_value_artifact(
        &self,
        module: SessionModule,
    ) -> Option<&Arc<tidepool_toolchain::checked_cell::CheckedValueArtifact>> {
        self.state.retained_checked_value_artifact(module)
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

    /// Capture checked value inputs while the matching resident view is owned.
    pub fn capture_inspection_inputs(
        &self,
        view: &super::SessionCompileView,
    ) -> Result<super::AdmittedInspectionInputs, SessionError> {
        let current = self
            .compile_view_in(view.lexical_scope())
            .ok_or(SessionError::DeadScope(view.lexical_scope()))?;
        if current.session() != view.session() || !current.is_current_for(view) {
            return Err(SessionError::StaleStagedDeclaration);
        }
        let values = self.state.capture_value_interfaces(view)?;
        Ok(super::AdmittedInspectionInputs::capture(
            view.clone(),
            values,
            self.state.catalog_selection().clone(),
        ))
    }

    pub fn compile_view_for_execution(
        &self,
        execution: &super::PrivateExecutionAdmission,
    ) -> Result<super::SessionCompileView, SessionError> {
        self.state.compile_view_for_execution(execution)?;
        self.compile_view_in(execution.private_scope())
            .map(super::SessionCompileView::with_scoped_injection)
            .ok_or(SessionError::DeadScope(execution.private_scope()))
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

    pub fn exact_exports_in_namespace(
        &self,
        scope: ScopeId,
        namespace: tidepool_toolchain::declaration_join::ExportNamespace,
        heads: &[&str],
    ) -> Result<super::ExactExportSurface, super::ExactExportError> {
        self.state
            .exact_exports_in_namespace(scope, namespace, heads)
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
        self.parked.last().map(|entry| entry.name.as_str())
    }

    /// Every parked hole, insertion-ordered (oldest first).
    pub fn parked_holes(&self) -> Vec<&str> {
        self.parked
            .iter()
            .map(|entry| entry.name.as_str())
            .collect()
    }

    /// Runtime resource scope owning one parked continuation. Lifecycle
    /// interpreters use this to distinguish installed-program suspensions from
    /// disposable workbench fragments without trusting request payload data.
    #[must_use]
    pub fn parked_realm(&self, hole: &ResidentHole) -> Option<RealmId> {
        self.parked_realm_named(hole.cont_id())
    }

    /// Read the realm of an already registered frame without minting a resume token.
    #[must_use]
    pub fn parked_realm_named(&self, cont_id: &str) -> Option<RealmId> {
        let entry = self.parked.iter().find(|entry| entry.name == cont_id)?;
        self.state.parked_realm(entry.id)
    }

    #[must_use]
    pub fn parked_program_provenance(&self, hole: &ResidentHole) -> Option<Arc<ProgramProvenance>> {
        let entry = self
            .parked
            .iter()
            .find(|entry| entry.name == hole.cont_id())?;
        Some(Arc::clone(&entry.provenance))
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
        self.parked.retain(|entry| survivors.contains(&entry.id));
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
        let Some(entry) = self.parked.iter().find(|entry| entry.name == hole) else {
            return Ok(None);
        };
        let id = entry.id;
        let provenance = Arc::clone(&entry.provenance);
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
        let Some(entry) = self.parked.iter().find(|entry| entry.name == hole) else {
            return Ok(None);
        };
        let id = entry.id;
        let provenance = Arc::clone(&entry.provenance);
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

    /// Capture the live payload and its input type from the same original
    /// parked request. The affine token retains the requesting program's
    /// provenance independently of the actor that will mount the value.
    pub fn capture_activation_input(
        &mut self,
        hole: &ResidentHole,
        realm: RealmId,
        site: u64,
    ) -> Result<RuntimeActivationInput, ResidentError> {
        let invalid = || ResidentError::InvalidActivationInput { site };
        let entry = self
            .parked
            .iter()
            .find(|entry| entry.name == hole.cont_id())
            .ok_or_else(invalid)?;
        let parked_site = self
            .state
            .prepared_mut()
            .and_then(|engine| engine.parked_site(entry.id))
            .ok_or_else(invalid)?;
        if parked_site != site {
            return Err(invalid());
        }
        let provenance = self.parked_program_provenance(hole).ok_or_else(invalid)?;
        if !provenance.authenticated_inputs.contains_key(&site) {
            return Err(ResidentError::UnauthenticatedActivationInputWitness { site });
        }
        let metadata = provenance.sites.get(&site).ok_or_else(invalid)?;
        let input_type = metadata
            .inputs
            .first()
            .filter(|_| metadata.inputs.len() <= 2)
            .ok_or_else(invalid)?
            .ty
            .clone();
        let missing_witness = || ResidentError::MissingActivationInputWitness { site };
        if metadata.input_type_witnesses.len() != metadata.inputs.len() {
            return Err(missing_witness());
        }
        let input_type_witness = metadata
            .input_type_witnesses
            .first()
            .and_then(Option::as_ref)
            .ok_or_else(missing_witness)?
            .clone();
        let progress_type_witness = match metadata.input_type_witnesses.get(1) {
            Some(witness) => Some(Arc::new(
                witness.as_ref().ok_or_else(missing_witness)?.clone(),
            )),
            None => None,
        };
        let signatures = metadata
            .request_type_signatures
            .clone()
            .ok_or_else(invalid)?;
        let interfaces = provenance
            .authenticated_inputs
            .get(&site)
            .ok_or_else(invalid)?;
        let original_execution = interfaces.execution.require_unique(site)?;
        let type_evidence = self
            .request_site_type_evidence(site)
            .ok_or_else(invalid)?
            .authenticate_request_types(signatures, &interfaces.types)
            .map_err(SessionError::Compile)?;
        let prototype =
            tidepool_toolchain::checked_cell::ExactHostBindingPrototype::from_original_input(
                input_type_witness.clone(),
                interfaces.types.clone(),
            )
            .map_err(SessionError::Compile)?;
        let custody = self
            .live_payload_handle_owned_by(hole.cont_id(), realm)?
            .ok_or_else(invalid)?;
        Ok(RuntimeActivationInput {
            custody,
            site,
            input_type,
            type_evidence: Arc::new(type_evidence),
            input_type_witness: Arc::new(input_type_witness),
            prototype,
            original_execution,
            progress_type_witness,
        })
    }

    /// Read progress authority from the actual parked frame, never from a
    /// caller-supplied site number or an erased runtime representation.
    pub fn progress_type_witness(
        &mut self,
        continuation: &str,
    ) -> Result<Arc<tidepool_toolchain::checked_cell::CanonicalInputTypeWitness>, ResidentError>
    {
        let entry = self
            .parked
            .iter()
            .find(|entry| entry.name == continuation)
            .ok_or(ResidentError::InvalidActivationInput { site: 0 })?;
        let site = self
            .state
            .prepared_mut()
            .and_then(|engine| engine.parked_site(entry.id))
            .ok_or(ResidentError::InvalidActivationInput { site: 0 })?;
        let invalid = || ResidentError::InvalidActivationInput { site };
        let provenance = Arc::clone(&entry.provenance);
        if !provenance.authenticated_inputs.contains_key(&site) {
            return Err(ResidentError::UnauthenticatedActivationInputWitness { site });
        }
        let metadata = provenance.sites.get(&site).ok_or_else(invalid)?;
        if metadata.inputs.len() != 1 || metadata.input_type_witnesses.len() != 1 {
            return Err(invalid());
        }
        let witness = metadata.input_type_witnesses[0]
            .as_ref()
            .ok_or(ResidentError::MissingActivationInputWitness { site })?;
        Ok(Arc::new(witness.clone()))
    }

    pub fn capture_progress_publication(
        &mut self,
        hole: &ResidentHole,
        realm: RealmId,
    ) -> Result<RuntimeProgressPublication, ResidentError> {
        let type_witness = self.progress_type_witness(hole.cont_id())?;
        let custody = self
            .live_payload_handle_owned_by(hole.cont_id(), realm)?
            .ok_or(ResidentError::InvalidActivationInput { site: 0 })?;
        Ok(RuntimeProgressPublication {
            custody,
            type_witness,
        })
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

    /// Export the value `custody` roots as a detached [`ResidentParcel`] another
    /// session's machine (sharing this run's [`tidepool_codegen::prepared_program::ImageRegistry`])
    /// can import -- the session-layer half of a value crossing two
    /// [`ResidentSession`]s. Consumes the custody: the export itself is a
    /// non-consuming read of the machine (`PreparedEngine::export_parcel`,
    /// like `inspect_retained`), so once the parcel is safely out this
    /// releases the handle exactly as [`Self::discard_custody`] would --
    /// the parcel is now the value's only owner on this side.
    pub fn export_custody(
        &mut self,
        custody: RootCustody,
    ) -> Result<ResidentParcel, ResidentError> {
        self.settle_dropped_custody();
        if !Arc::ptr_eq(&custody.cleanup.0, &self.custody_cleanup) {
            return Err(ResidentError::ForeignCustody);
        }
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
        let provenance = Arc::clone(&transfer.provenance);
        transfer.commit();
        Ok(ResidentParcel {
            native: parcel,
            provenance,
        })
    }

    /// Export the value `custody` roots as a detached [`ResidentParcel`], WITHOUT
    /// consuming or releasing `custody` -- the borrowing counterpart to
    /// [`Self::export_custody`], for a value more than one destination
    /// machine may need to import independently (a request's published
    /// progress snapshot, read by however many observers poll it, is the
    /// motivating case). `PreparedEngine::export_parcel` is already a
    /// non-consuming read of the machine (see [`Self::export_custody`]'s own
    /// doc); this only differs by skipping the `discard_handle` after it, so
    /// the root stays live here for the next caller to export again.
    pub fn export_shared(
        &mut self,
        custody: &RootCustody,
    ) -> Result<ResidentParcel, ResidentError> {
        self.settle_dropped_custody();
        if !Arc::ptr_eq(&custody.cleanup.0, &self.custody_cleanup) {
            return Err(ResidentError::ForeignCustody);
        }
        let Some(handle) = custody.handle else {
            unreachable!("live custody always contains its handle");
        };
        let Some(engine) = self.state.prepared_mut() else {
            return Err(ResidentError::Run(RuntimeError::Jit(EffectError::Handler(
                format!("cannot export {handle:?}: the prepared machine is not installed"),
            ))));
        };
        Ok(ResidentParcel {
            native: engine.export_parcel(handle)?,
            provenance: Arc::clone(&custody.provenance),
        })
    }

    /// Import the original native value and immutable compiler provenance
    /// under this session's cleanup owner. Imported binding roots retain the
    /// same provenance for later binding capture and cross-session export.
    #[allow(
        clippy::expect_used,
        reason = "exact identities are preflighted and native import mints live handles under this exclusive checkout"
    )]
    pub fn import_parcel(
        &mut self,
        parcel: ResidentParcel,
        owner: RealmId,
    ) -> Result<RootCustody, ResidentError> {
        self.settle_dropped_custody();
        let ResidentParcel {
            native: parcel,
            provenance,
        } = parcel;
        let mut bindings: BTreeMap<SymbolIdentity, ParcelLibraryBinding> = self
            .state
            .prepared()
            .ok_or_else(|| {
                ResidentError::Run(RuntimeError::Jit(EffectError::Handler(
                    "cannot import a parcel: the prepared machine is not installed".into(),
                )))
            })?
            .pending_parcel_import_identities(&parcel)
            .into_iter()
            .filter_map(|identity| {
                parcel_library_binding(&identity).map(|binding| (identity, binding))
            })
            .collect();
        self.state
            .validate_new_binding_ids(bindings.values().map(|binding| binding.id))?;
        let engine = self
            .state
            .prepared_mut()
            .expect("the preflighted engine remains installed");
        let (handle, imports) = engine.import_parcel(parcel, owner)?;
        // Exact imports were checked before native allocation. Adopt their
        // binding handles to ROOT before the caller's realm can close.
        // The native preflight and report enumerate the same missing instance
        // imports, deduplicated by full identity. Consume those original plans;
        // package imports stay rooted by their native installation.
        let resolved: Vec<(SymbolIdentity, ParcelLibraryBinding, PreparedHandle)> = imports
            .into_iter()
            .filter_map(|(identity, imported)| {
                let (identity, binding) = bindings.remove_entry(&identity)?;
                assert!(
                    engine.adopt(imported),
                    "a just-imported binding handle is live"
                );
                Some((identity, binding, imported))
            })
            .collect();
        for (identity, binding, imported) in resolved {
            let ParcelLibraryBinding { id, module } = binding;
            self.state.bind(BindingEntry {
                name: BindingName(identity.occurrence.clone()),
                id,
                module,
                value: BoundValue {
                    handle: imported,
                    identity,
                },
                type_display: None,
                defining_expr: None,
                scope: ScopeId::ROOT,
            }).expect("all library identities preflighted before native import under exclusive checkout");
            self.binding_provenance
                .insert(id.raw(), Arc::clone(&provenance));
            self.advance_public_visibility(ScopeId::ROOT);
        }
        Ok(RootCustody::new(
            handle,
            Arc::clone(&self.custody_cleanup),
            provenance,
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
            obligation = if seed.checked.is_some() {
                "checked"
            } else {
                match &seed.obligation {
                    HoleObligation::Plain => "plain",
                    HoleObligation::Binding { .. } => "binding",
                    HoleObligation::ProjectedBinding { .. } => "projected-binding",
                }
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

    /// Capture the originating request's canonical type graph before its
    /// input leaves this machine session.
    pub fn request_site_type_evidence(
        &mut self,
        site: u64,
    ) -> Option<super::prepared::SiteTypeEvidence> {
        self.state.prepared_mut()?.request_site_type_evidence(site)
    }

    /// Match the captured request graph to this machine's access site.
    pub fn request_scope_types_match(
        &mut self,
        request: &super::prepared::SiteTypeEvidence,
        access_site: u64,
    ) -> Result<bool, PreparedRuntimeError> {
        match self.state.prepared_mut() {
            Some(engine) => engine
                .request_scope_types_match(request, access_site)
                .map_err(|source| PreparedRuntimeError::RequestScopeTypeEvidence {
                    site: access_site,
                    source,
                }),
            None => Ok(false),
        }
    }

    /// Bind the original rooted input under its fresh compiler type interface.
    /// All filesystem work precedes the affine reservation/root consumption.
    pub fn mount_activation_input(
        &mut self,
        owner: RuntimeActivationInputAdmission,
        interface: Arc<tidepool_toolchain::checked_cell::ExactHostBindingInterface>,
    ) -> Result<MountedActivationInput, ResidentError> {
        self.settle_dropped_custody();
        let reservation = &owner.reservation;
        let scope = reservation.scope;
        let generation = reservation.generation;
        let invalid = || ResidentError::InvalidActivationInput {
            site: owner.input.site,
        };
        if !Arc::ptr_eq(&owner.input.custody.cleanup.0, &self.custody_cleanup) {
            return Err(ResidentError::ForeignCustody);
        }
        self.state
            .validate_binding_interface(reservation, &interface)?;
        if self.run_context != owner.run_context
            || interface.purpose()
                != tidepool_toolchain::checked_cell::BindingInterfacePurpose::OriginalLiveInput
            || !Arc::ptr_eq(interface.prototype(), &owner.input.prototype)
        {
            return Err(invalid());
        }
        let witnessed = interface.original_input_type().ok_or_else(invalid)?;
        if owner.input.input_type_witness.as_ref() != witnessed
            || owner.input.input_type_witness.metadata_digest() != witnessed.metadata_digest()
        {
            return Err(ResidentError::ActivationInputTypeMismatch {
                site: owner.input.site,
                original: owner.input.input_type_witness.commitment(),
                compiled: witnessed.commitment(),
            });
        }
        let binder =
            super::turn::decode_bound_binder(interface.binder()).map_err(SessionError::Compile)?;
        if binder.name != reservation.binding
            || binder.module != SessionModule::val(generation).module_name()
            || binder.var_id != session_var_id(&binder.module, &binder.name)
            || binder.host_authority.is_some()
        {
            return Err(invalid());
        }
        let binding = SessionVarId::from_extract(binder.var_id);
        self.state.validate_new_binding_ids([binding])?;
        let raw = owner.input.custody.handle.ok_or_else(invalid)?;
        let handle = self
            .state
            .require_prepared()?
            .prepared_handle_of(raw)
            .ok_or_else(invalid)?;
        if self
            .state
            .require_prepared()?
            .hosting_program(handle)
            .is_none()
        {
            return Err(invalid());
        }
        let staged = self
            .state
            .stage_checked_value_interface(interface.value_interface_certificate())?;
        self.state.validate_staged_value_interface(&staged)?;
        // Epoch fencing does not observe invocation or resource cancellation.
        // A cancellation after transfer is handled by the existing child scope.
        if self.state.mount_cancelled(self.run_context.resource_scope) {
            return Err(PreparedRuntimeError::Cancelled.into());
        }
        self.state
            .consume_binding_interface(reservation, &interface)?;
        self.mount_compiled_binding_prepared(
            scope,
            &binder,
            generation,
            owner.input.custody,
            ValueInterfaceSource::Staged(staged),
        )
        .map_err(|source| ResidentError::ActivationInputConsumed {
            binding,
            source: Box::new(source),
        })?;
        let visibility = self
            .state
            .public_visibility_snapshot_in(scope)
            .ok_or_else(|| ResidentError::ActivationMountCommitted {
                binding,
                source: Box::new(SessionError::DeadScope(scope).into()),
            })?;
        Ok(MountedActivationInput {
            interface,
            original_execution: owner.input.original_execution,
            binding,
            scope,
            handle,
            visibility,
            run_context: self.run_context,
        })
    }

    /// Legacy host mounts use filesystem interfaces. Activation inputs use
    /// their dedicated affine compiler/mount owner above.
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
        if let Err(error) = self
            .state
            .validate_new_binding_ids([SessionVarId::from_extract(binder.var_id)])
        {
            self.discard_custody(custody);
            return Err(error.into());
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
        self.mount_compiled_binding_prepared(
            scope,
            binder,
            gen,
            custody,
            ValueInterfaceSource::LegacyDisk,
        )
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
        refuse_checked_turn(&code)?;
        self.settle_dropped_custody();
        let representation = self.validate_compiled_mount_target(
            scope,
            binder,
            gen,
            &code,
            HostBindingType::JSON_VALUE,
        )?;
        self.mount_host_value_in(scope, binder, gen, code, |engine, realm, _| {
            representation.build(engine, realm, HostPayload::Json(value))
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
        refuse_checked_turn(&code)?;
        self.settle_dropped_custody();
        let representation =
            self.validate_compiled_mount_target(scope, binder, gen, &code, HostBindingType::TEXT)?;
        self.mount_host_value_in(scope, binder, gen, code, |engine, realm, _| {
            representation.build(engine, realm, HostPayload::Text(text))
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
        refuse_checked_turn(&code)?;
        self.settle_dropped_custody();
        self.validate_compiled_mount_target(scope, binder, gen, &code, expected)?;
        self.mount_host_value_in(scope, binder, gen, code, |engine, realm, table| {
            engine.build_host_value(realm, value, table)
        })
    }

    /// Retain a prototype only from this machine's original checked host mount.
    pub fn retain_host_binding_prototype(
        &mut self,
        carrier: &HostCarrier,
    ) -> Result<(), ResidentError> {
        let (admission, _) = carrier.checked_binding()?;
        if !admission.prefix().admission().belongs_to(&self.state) {
            return Err(ResidentError::UnsupportedCheckedTurn);
        }
        let execution = carrier
            .code
            .certification
            .as_ref()
            .as_ref()
            .and_then(|certification| certification.checked_execution())
            .ok_or(ResidentError::UnsupportedCheckedTurn)?;
        let compiler =
            tidepool_toolchain::checked_cell::ExactHostBindingPrototype::from_checked(execution)
                .map_err(SessionError::Compile)?;
        let mut code = own_host_code(carrier.code());
        let certification = code
            .certification
            .as_ref()
            .as_ref()
            .ok_or(ResidentError::UnsupportedCheckedTurn)?
            .host_prototype()
            .map_err(SessionError::Compile)?;
        code.certification = std::borrow::Cow::Owned(Some(certification));
        let prototype = Arc::new(HostBindingPrototype {
            compiler,
            code: Arc::new(code),
            representation: carrier.representation,
            authority_digest: admission.prefix().admission().authority_digest(),
            include_paths: admission.prefix().admission().include_paths().to_vec(),
            _source_owner: admission.prefix().admission().specification().clone(),
        });
        self.host_binding_prototypes
            .retain(|old| old.representation.host_type() != prototype.representation.host_type());
        self.host_binding_prototypes.push(prototype);
        Ok(())
    }

    pub fn host_binding_prototype(
        &self,
        expected: HostBindingType,
        authority_digest: [u8; 32],
    ) -> Option<Arc<HostBindingPrototype>> {
        self.host_binding_prototypes
            .iter()
            .find(|prototype| {
                prototype.representation.host_type() == expected
                    && prototype.authority_digest == authority_digest
            })
            .cloned()
    }

    pub fn admit_host_binding_interface(
        &mut self,
        scope: ScopeId,
        binding: String,
        prototype: Arc<HostBindingPrototype>,
    ) -> Result<Arc<super::RuntimeHostBindingAdmission>, ResidentError> {
        self.settle_dropped_custody();
        if self.run_context.lexical_scope != scope
            || !self
                .host_binding_prototypes
                .iter()
                .any(|original| Arc::ptr_eq(original, &prototype))
        {
            return Err(ResidentError::UnsupportedCheckedTurn);
        }
        self.state
            .admit_host_binding_interface(scope, binding, prototype)
            .map_err(ResidentError::Session)
    }

    fn mount_host_interface_input(
        &mut self,
        carrier: &HostCarrier,
        payload: HostPayload<'_>,
    ) -> Result<super::PendingHostValueWrite, ResidentError> {
        let HostCarrierOrigin::Interface {
            admission,
            binder,
            proof,
        } = &carrier.origin
        else {
            return Err(ResidentError::UnsupportedCheckedTurn);
        };
        if !carrier.representation.matches_payload(&payload)
            || self.run_context.lexical_scope != admission.scope
        {
            return Err(ResidentError::UnsupportedCheckedTurn);
        }
        self.settle_dropped_custody();
        self.validate_host_mount_geometry(admission.scope, binder, admission.generation)?;
        let staged = self
            .state
            .stage_checked_value_interface(proof.value_interface_certificate())?;
        self.state.validate_staged_value_interface(&staged)?;
        if self.state.mount_cancelled(self.run_context.resource_scope) {
            return Err(PreparedRuntimeError::Cancelled.into());
        }
        self.state
            .consume_host_binding_interface(admission, proof)?;
        let representation = carrier.representation;
        self.mount_host_value_with_interface_in(
            admission.scope,
            binder,
            admission.generation,
            carrier.code(),
            ValueInterfaceSource::Staged(staged),
            move |engine, realm, _| representation.build(engine, realm, payload),
        )?;
        match super::PendingHostValueWrite::mounted_host_interface(
            &self.state,
            admission.scope,
            binder,
            proof.clone(),
        ) {
            Ok(write) => Ok(write),
            Err(error) => {
                self.retire_host_binding_owner(&admission.session_root, binder);
                Err(error.into())
            }
        }
    }

    /// Fill the sole compiler-issued host binder without entering a placeholder.
    pub fn mount_checked_host_input(
        &mut self,
        carrier: &HostCarrier,
        payload: HostPayload<'_>,
    ) -> Result<super::PendingHostValueWrite, ResidentError> {
        if matches!(&carrier.origin, HostCarrierOrigin::Interface { .. }) {
            return self.mount_host_interface_input(carrier, payload);
        }
        let (admission, binder) = carrier.checked_binding()?;
        if !carrier.representation.matches_payload(&payload) {
            return Err(ResidentError::UnsupportedCheckedTurn);
        }
        self.settle_dropped_custody();
        let scope = admission.prefix().admission().visibility().scope;
        let generation = admission.generation();
        if self.run_context.lexical_scope != scope {
            return Err(ResidentError::UnsupportedCheckedTurn);
        }
        self.validate_host_mount_geometry(scope, binder, generation)?;
        let code = carrier.code();
        let execution = code
            .certification
            .as_ref()
            .as_ref()
            .and_then(|certification| certification.checked_execution())
            .ok_or(ResidentError::UnsupportedCheckedTurn)?;
        let interface = execution
            .value_interface_certificate()
            .ok_or(ResidentError::UnsupportedCheckedTurn)?;
        let session_root = admission.snapshot().view().session_root().to_path_buf();
        let staged = self.state.stage_checked_value_interface(interface)?;
        self.state.validate_staged_value_interface(&staged)?;
        if self.state.mount_cancelled(self.run_context.resource_scope) {
            return Err(PreparedRuntimeError::Cancelled.into());
        }
        self.state.consume_host_carrier_reservation(
            admission,
            execution,
            &binder.name,
            carrier.representation.host_type(),
        )?;
        let execution = execution.clone();
        let representation = carrier.representation;
        self.mount_host_value_with_interface_in(
            scope,
            binder,
            generation,
            carrier.code(),
            ValueInterfaceSource::Staged(staged),
            move |engine, realm, _| representation.build(engine, realm, payload),
        )?;
        match super::PendingHostValueWrite::mounted(&self.state, scope, binder, execution) {
            Ok(write) => Ok(write),
            Err(error) => {
                self.retire_host_binding_owner(&session_root, binder);
                Err(error.into())
            }
        }
    }

    /// Reuse only an already accepted private host write. A still-guarded mount
    /// with the same job text is not evidence that its effect owner accepted it.
    pub fn accepted_host_text_binding_in(
        &self,
        admission: &super::PrivateExecutionAdmission,
        text: &str,
    ) -> Option<String> {
        if !Arc::ptr_eq(&admission.owner, self.state.admission_owner())
            || admission.owner_epoch != self.state.admission_owner().epoch()
        {
            return None;
        }
        let scope = admission.private_scope();
        let name = self.host_text_binding_in(scope, text)?;
        let entry = self.state.resolve_in(scope, &name)?;
        admission
            .completed_values
            .lock()
            .get(&entry.id)
            .is_some_and(|write| write.matches(entry))
            .then_some(name)
    }

    /// Mount `payload` under a freshly minted `name`/`gen` binder through a
    /// [`HostCarrier`] built once from a real compiled turn, with NO GHC
    /// compile for this mount: the binder's `var_id` is minted directly
    /// ([`tidepool_codegen::prepared_program::session_var_id`]), its
    /// `Val.G<gen>` source is a hand-written stub written at
    /// `<session_root>/Tidepool/Session/Val/G<gen>.hs` (an ordinary home
    /// module a later turn's downsweep finds on the include path -- never
    /// injected via `--inject-val`, since it has no `.hi`). The reusable
    /// carrier already retains validated representation evidence; this mount
    /// validates the fresh binding slot and payload kind.
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
        let HostCarrierOrigin::Reusable(shape) = &carrier.origin else {
            return Err(ResidentError::UnsupportedCheckedTurn);
        };
        if !carrier.representation.matches_payload(&payload) {
            return Err(ResidentError::UnsupportedCheckedTurn);
        }
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
            tier: shape.tier,
            type_display: shape.type_display.clone(),
            root_head: Some(shape.root.clone()),
            host_authority: Some(carrier.representation.host_type().authority),
        };
        self.validate_host_mount_geometry(scope, &binder, gen)?;

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
        let representation = carrier.representation;
        let mount_result =
            self.mount_host_value_in(scope, &binder, gen, carrier.code(), |engine, realm, _| {
                representation.build(engine, realm, payload)
            });
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
        if self.hidden_host_bindings.insert(id, ()).is_none() {
            self.advance_public_visibility(scope);
        }
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
        self.retire_binding_owner(id);
        self.reap_evicted_stub_sources_in(session_root);
    }

    /// Retire an exact binding owner while preserving existing dependency leases.
    pub fn retire_binding_owner(&mut self, id: SessionVarId) {
        self.settle_dropped_custody();
        let scope = self.state.bindings().get(id).map(|entry| entry.scope);
        self.state.retire_binding_owner(id);
        if let Some(scope) = scope {
            self.advance_public_visibility(scope);
        }
        self.hidden_host_bindings.remove(&id);
        self.prune_binding_metadata();
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

    fn validate_host_mount_geometry(
        &self,
        scope: ScopeId,
        binder: &BoundBinder,
        generation: Generation,
    ) -> Result<(), ResidentError> {
        if !self.state.scope_tree().is_live(scope) {
            return Err(SessionError::DeadScope(scope).into());
        }
        self.state
            .validate_new_binding_ids([SessionVarId::from_extract(binder.var_id)])?;
        if binder.module != SessionModule::val(generation).module_name() {
            return Err(host_mount_failure(
                "host binder differs from its mount generation",
            ));
        }
        Ok(())
    }

    fn validate_compiled_mount_target(
        &self,
        scope: ScopeId,
        binder: &BoundBinder,
        gen: Generation,
        code: &TurnCode<'_>,
        expected: HostBindingType,
    ) -> Result<HostRepresentation, ResidentError> {
        self.validate_host_mount_geometry(scope, binder, gen)?;
        let (_, representation) = host_representation(binder, code, expected)?;
        Ok(representation)
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
        refuse_checked_turn(&code)?;
        self.mount_host_value_with_interface_in(
            scope,
            binder,
            gen,
            code,
            ValueInterfaceSource::LegacyDisk,
            build,
        )
    }

    fn mount_host_value_with_interface_in(
        &mut self,
        scope: ScopeId,
        binder: &BoundBinder,
        gen: Generation,
        code: TurnCode<'_>,
        interface_source: ValueInterfaceSource,
        build: impl FnOnce(
            &mut super::prepared::PreparedEngine,
            RealmId,
            &DataConTable,
        ) -> Result<PreparedHandle, PreparedRuntimeError>,
    ) -> Result<(), ResidentError> {
        self.state
            .validate_new_binding_ids([SessionVarId::from_extract(binder.var_id)])?;
        let table = code
            .table
            .with_json_layout(json_runtime_layout_optional(&code.prepared));
        self.state
            .merge_table(&table)
            .map_err(ResidentError::TableCollision)?;
        let prepared = code.prepared.into_owned();
        let (program, source_keys) = if let Some(certification) = code.certification.as_ref() {
            let resolved = self
                .state
                .resolve_certification_in(scope, &prepared, certification)?;
            let registry = self.state.certified_image_registry();
            let (target, demanded) = super::prepared::CertifiedTargetImage::compile_scoped(
                prepared, &resolved, &registry,
            )?;
            self.install_certified_turn_in(
                scope,
                target,
                &resolved.target_owners,
                &resolved.source_evidence,
                demanded,
                &resolved.inherited_needed,
            )?
        } else {
            (self.state.install_prepared(prepared)?, Default::default())
        };
        let realm = self.run_context.resource_scope;
        let mounted = (|| {
            let handle = {
                let engine = self.state.require_prepared()?;
                build(engine, realm, &table)?
            };
            self.mount_host_handle_prepared(scope, binder, gen, handle, interface_source)
        })();
        if let Some(engine) = self.state.prepared_mut() {
            engine.unpin(program);
        }
        if mounted.is_err() {
            self.retire_failed_turn_source_instances(scope, &source_keys);
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
        interface_source: ValueInterfaceSource,
    ) -> Result<(), ResidentError> {
        let engine = self.state.require_prepared()?;
        let Some(program) = engine.hosting_program(handle) else {
            engine.release(handle);
            return Err(ResidentError::Run(RuntimeError::Jit(EffectError::Handler(
                "host binding mount produced a handle with no hosting program".into(),
            ))));
        };
        self.bind_prepared(
            program,
            scope,
            gen,
            &[(binder, handle)],
            None,
            interface_source,
        )?;
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
        interface_source: ValueInterfaceSource,
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
        self.bind_prepared(
            program,
            scope,
            gen,
            &[(binder, handle)],
            None,
            interface_source,
        )?;
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

    /// Freeze an actor's lexical environment into an independent retained root.
    pub fn mint_detached_scope(&mut self, parent: ScopeId) -> Option<ScopeId> {
        self.state.mint_detached_scope(parent)
    }

    pub fn retain_scope_dependencies(&mut self, source: ScopeId, target: ScopeId) -> bool {
        self.state.retain_scope_dependencies(source, target)
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
                        .turn(tidepool_repr::Generation(generation))
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

    /// Exact restored declaration tips and unavailable prior-machine bindings.
    #[must_use]
    pub fn declaration_recovery_report(&self) -> Option<super::DeclarationRecoveryReport> {
        self.state.lib().declaration_recovery_report()
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
        self.prune_binding_metadata();
        self.advance_public_visibility(scope);
        retirement
    }

    fn provenance_for(&self, code: &TurnCode<'_>) -> Result<Arc<ProgramProvenance>, ResidentError> {
        let mut provenance = ProgramProvenance::from_sites(&code.sites)?;
        let certification = code.certification.as_ref().as_ref();
        let original_interfaces = if let Some(certification) = certification {
            let original = match certification.purpose() {
                TurnPurpose::Execution { execution, .. }
                | TurnPurpose::HostPrototype(execution) => Some(
                    execution.original_interface_context(&code.prepared, &code.table, &code.sites),
                ),
                TurnPurpose::ActivationPreview(proof) => {
                    Some(proof.original_interface_context(&code.prepared, &code.table, &code.sites))
                }
                TurnPurpose::Ordinary => {
                    certification.original_compile_input.as_ref().map(|proof| {
                        proof.original_interface_context(
                            &code.prepared,
                            &certification.groups,
                            &certification.target_owners,
                            &certification.package_interfaces,
                            &code.table,
                            &code.sites,
                        )
                    })
                }
            };
            original.transpose().map_err(SessionError::Compile)?
        } else {
            None
        };
        let original_execution = if let Some(certification) = certification {
            let original = match certification.purpose() {
                TurnPurpose::Execution { execution, .. }
                | TurnPurpose::HostPrototype(execution) => Some(
                    execution.original_execution_context(&code.prepared, &code.table, &code.sites),
                ),
                TurnPurpose::ActivationPreview(proof) => {
                    Some(proof.original_execution_context(&code.prepared, &code.table, &code.sites))
                }
                TurnPurpose::Ordinary => {
                    certification.original_compile_input.as_ref().map(|proof| {
                        proof.original_execution_context(
                            &code.prepared,
                            &certification.groups,
                            &certification.target_owners,
                            &certification.package_interfaces,
                            &code.table,
                            &code.sites,
                        )
                    })
                }
            };
            original.transpose().map_err(SessionError::Compile)?
        } else {
            None
        };
        if let Some(original_interfaces) = original_interfaces {
            provenance.fresh_completion_sites.extend(
                code.prepared
                    .sites()
                    .iter()
                    .filter(|site| {
                        site.inputs.is_empty()
                            && provenance.sites.get(&site.site).is_some_and(|observed| {
                                observed.inputs.is_empty()
                                    && observed.origin == site.origin
                                    && observed.ordinal == site.ordinal
                            })
                    })
                    .map(|site| site.site),
            );
            let original_execution =
                original_execution.ok_or(ResidentError::UnsupportedCheckedTurn)?;
            let original_execution = OriginalExecutionContext::capture(original_execution);
            // Site observations and installed native custody can include
            // earlier entries. A typed target retains its exact issued closure.
            let issued_sites =
                certification.and_then(|certification| match certification.purpose() {
                    TurnPurpose::Execution { execution, .. }
                    | TurnPurpose::HostPrototype(execution) => execution.selected_native_sites(),
                    _ => None,
                });
            let mut selected_sites = code
                .prepared
                .sites()
                .iter()
                .map(|site| site.site)
                .collect::<std::collections::BTreeSet<_>>();
            if let Some(issued_sites) = issued_sites {
                issued_sites
                    .validate_observations(code.sites.iter())
                    .map_err(SessionError::Compile)?;
                provenance
                    .native_sites
                    .merge(issued_sites)
                    .map_err(SessionError::Compile)?;
                selected_sites.extend(issued_sites.ids());
            } else {
                selected_sites.extend(
                    certification
                        .into_iter()
                        .flat_map(|certification| certification.groups.iter())
                        .flat_map(|group| group.group().definitions().sites())
                        .map(|site| site.site),
                );
            }
            let descriptors = original_interfaces.artifact_view().descriptors();
            let home_units = descriptors
                .iter()
                .map(|descriptor| descriptor.owner.unit.as_str())
                .collect::<std::collections::BTreeSet<_>>();
            let owners = descriptors
                .iter()
                .map(|descriptor| (&descriptor.owner, descriptor.id))
                .collect::<BTreeMap<_, _>>();
            for site in provenance.sites.values().filter(|site| {
                selected_sites.contains(&site.site)
                    && site.input_type_witnesses.len() == site.inputs.len()
                    && site.input_type_witnesses.iter().any(Option::is_some)
            }) {
                let Some(signatures) = &site.request_type_signatures else {
                    continue;
                };
                let mut roots = std::collections::BTreeSet::new();
                for name in std::iter::once(signatures.reply())
                    .chain(signatures.progress())
                    .flat_map(|signature| signature.names())
                {
                    if home_units.contains(name.unit()) {
                        let owner = tidepool_toolchain::declaration_join::ExactModuleIdentity {
                            unit: name.unit().to_owned(),
                            module: name.module().to_owned(),
                        };
                        let id = owners.get(&owner).ok_or_else(|| SessionError::Compile(crate::CompileError::ExtractFailed(
                            format!("request native type owner {}:{} is outside its sealed interface closure", name.unit(), name.module()),
                        )))?;
                        roots.insert(*id);
                    }
                }
                if let Some(witness) = site.input_type_witnesses.first().and_then(Option::as_ref) {
                    for (unit, module, seal) in witness.interface_seals() {
                        let owner = tidepool_toolchain::declaration_join::ExactModuleIdentity {
                            unit: unit.to_owned(),
                            module: module.to_owned(),
                        };
                        if let Some(id) = owners.get(&owner) {
                            let descriptor = descriptors
                                .iter()
                                .find(|descriptor| descriptor.id == *id)
                                .expect("selected owner descriptor");
                            let actual = descriptor
                                .interface_sha256
                                .iter()
                                .map(|byte| format!("{byte:02x}"))
                                .collect::<String>();
                            if actual != seal {
                                return Err(SessionError::Compile(
                                    crate::CompileError::ExtractFailed(format!(
                                        "request input interface seal differs for {unit}:{module}"
                                    )),
                                )
                                .into());
                            }
                            roots.insert(*id);
                        } else if home_units.contains(unit) {
                            return Err(SessionError::Compile(crate::CompileError::ExtractFailed(
                                format!("request input owner {unit}:{module} is outside its sealed interface closure"),
                            )).into());
                        }
                    }
                }
                let interfaces = original_interfaces
                    .select_interface_roots(roots.into_iter().collect())
                    .map_err(SessionError::Compile)?;
                provenance.authenticated_inputs.insert(
                    site.site,
                    AuthenticatedInputContext::capture(
                        Arc::new(interfaces),
                        OriginalExecutionContexts::Unique(original_execution.clone()),
                    ),
                );
            }
        }
        Ok(Arc::new(provenance))
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

    fn validate_mounted_activation_input(
        &self,
        mounted: &MountedActivationInput,
    ) -> Result<(), ResidentError> {
        if self.run_context != mounted.run_context
            || self
                .state
                .public_visibility_snapshot_in(mounted.scope)
                .as_ref()
                != Some(&mounted.visibility)
            || self
                .state
                .bindings()
                .get(mounted.binding)
                .is_none_or(|entry| {
                    entry.scope != mounted.scope
                        || entry.value.handle != mounted.handle
                        || entry.module != mounted.interface.value_interface_certificate().owner()
                })
            || self
                .state
                .retained_checked_value_artifact(
                    mounted.interface.value_interface_certificate().owner(),
                )
                .is_none_or(|artifact| {
                    !Arc::ptr_eq(artifact, &mounted.interface.value_interface_certificate())
                })
        {
            return Err(ResidentError::ActivationPreviewRefused {
                binding: mounted.binding,
            });
        }
        Ok(())
    }

    /// Capture the exact postmount view while preserving the original code owner.
    pub fn admit_activation_preview(
        &mut self,
        mounted: MountedActivationInput,
        view: super::SessionCompileView,
    ) -> Result<Arc<RuntimeActivationPreviewAdmission>, ResidentError> {
        self.settle_dropped_custody();
        self.validate_mounted_activation_input(&mounted)?;
        let current = self
            .state
            .compile_view_in(mounted.scope)
            .ok_or(SessionError::DeadScope(mounted.scope))?;
        if view.session() != current.session()
            || view.lexical_scope() != mounted.scope
            || view.session_root() != current.session_root()
            || view.reachable_values() != current.reachable_values()
        {
            return Err(ResidentError::ActivationPreviewRefused {
                binding: mounted.binding,
            });
        }
        let view_digest = self
            .state
            .compile_view_digest_in(mounted.scope)
            .ok_or(SessionError::DeadScope(mounted.scope))?;
        let generation = self.state.val_gen().next();
        self.state.set_val_gen(generation);
        let exact_context = Arc::new(
            tidepool_toolchain::declaration_join::ExactCompileContext::new(
                mounted.original_execution.clone(),
            ),
        );
        let owner_epoch = self.state.admission_owner().epoch();
        let mut hash = blake3::Hasher::new();
        hash.update(b"TidepoolActivationPreview1");
        hash.update(&owner_epoch.to_le_bytes());
        hash.update(&view_digest);
        hash.update(&generation.0.to_le_bytes());
        hash.update(&mounted.binding.raw().to_le_bytes());
        hash.update(&mounted.interface.admission_digest());
        hash.update(&mounted.original_execution.semantic_sha256());
        hash.update(&mounted.run_context.principal.identity.to_le_bytes());
        hash.update(&mounted.run_context.principal.incarnation.to_le_bytes());
        hash.update(&mounted.run_context.resource_scope.0.to_le_bytes());
        let scope_lease = self.state.retain_lexical_scope(mounted.scope)?;
        Ok(Arc::new(RuntimeActivationPreviewAdmission {
            owner: self.state.admission_owner().clone(),
            owner_epoch,
            mounted,
            view,
            view_digest,
            generation,
            digest: *hash.finalize().as_bytes(),
            exact_context,
            scope_lease,
            consumed: AtomicBool::new(false),
        }))
    }

    /// Run only a sealed pure preview of this exact original committed root.
    pub fn run_activation_preview(
        &mut self,
        compiled: CompiledActivationPreview,
    ) -> Result<ResidentOutcome, ResidentError> {
        self.settle_dropped_custody();
        let admission = &compiled.admission;
        let mounted = &admission.mounted;
        self.validate_mounted_activation_input(mounted)?;
        if !Arc::ptr_eq(&admission.owner, self.state.admission_owner())
            || admission.owner_epoch != self.state.admission_owner().epoch()
            || self.state.compile_view_digest_in(mounted.scope) != Some(admission.view_digest)
            || compiled.proof.admission_digest() != admission.digest
            || compiled.proof.generation() != admission.generation.0
            || !Arc::ptr_eq(compiled.proof.input_interface(), &mounted.interface)
            || !compiled.proof.matches_target(&compiled.compiled.prepared)
            || compiled
                .compiled
                .certification
                .as_ref()
                .is_none_or(|certification| {
                    certification
                        .checked_activation_preview()
                        .is_none_or(|proof| !Arc::ptr_eq(proof, &compiled.proof))
                })
        {
            return Err(ResidentError::ActivationPreviewRefused {
                binding: mounted.binding,
            });
        }
        compiled
            .proof
            .validate_table(&compiled.compiled.table)
            .map_err(SessionError::Compile)?;
        compiled
            .proof
            .validate_yield_sites(&compiled.compiled.asks)
            .map_err(SessionError::Compile)?;
        if self.state.mount_cancelled(self.run_context.resource_scope) {
            return Err(PreparedRuntimeError::Cancelled.into());
        }
        admission
            .consumed
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .map_err(|_| ResidentError::ActivationPreviewRefused {
                binding: mounted.binding,
            })?;
        let provenance = self.provenance_for(&compiled.compiled.code())?;
        self.run_prepared_for_purpose(
            compiled.compiled.into_code(),
            PreparedTurnMode::Value,
            Some(mounted.handle),
            PreparedExecutionPurpose::HostActivationPreview(admission.scope_lease.clone()),
            provenance,
        )
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
        self.state.validate_new_binding_ids(binding_ids_of(&mode))?;
        let provenance = self.provenance_for(&code)?;
        let checked = checked_turn_plan(
            code.certification.as_ref(),
            code.prepared.as_ref(),
            &code.table,
            &mode,
        )?;
        let lexical_scope = self.run_context.lexical_scope;
        let checked = checked
            .map(|checked| checked.start(&self.state, lexical_scope))
            .transpose()?;
        self.run_prepared_for_purpose(
            code,
            mode,
            argument,
            PreparedExecutionPurpose::Authored(checked),
            provenance,
        )
    }

    fn run_prepared_for_purpose(
        &mut self,
        code: TurnCode<'_>,
        mode: PreparedTurnMode<'_>,
        argument: Option<PreparedHandle>,
        purpose: PreparedExecutionPurpose,
        provenance: Arc<ProgramProvenance>,
    ) -> Result<ResidentOutcome, ResidentError> {
        let (checked, lexical_scope, _execution_scope_lease) = match purpose {
            PreparedExecutionPurpose::Authored(checked) => {
                (checked, self.run_context.lexical_scope, None)
            }
            PreparedExecutionPurpose::HostActivationPreview(lease) => {
                self.state
                    .validate_lexical_scope_lease(lease.scope(), &lease)?;
                let scope = lease.scope();
                (None, scope, Some(lease))
            }
        };
        let prepared = code.prepared.into_owned();
        self.state
            .merge_table(&code.table)
            .map_err(ResidentError::TableCollision)?;
        if let PreparedTurnMode::Binding { generation, .. }
        | PreparedTurnMode::Projected { generation, .. } = &mode
        {
            // Claim the value-module identity before the turn runs.
            self.state.set_val_gen(*generation);
        }
        // Preview source instances belong to its detached authority, so dropping
        // that lease releases them without changing the public source plane.
        let install_prepared_started = std::time::Instant::now();
        let (program, source_keys) = self.install_turn_program_in(
            lexical_scope,
            prepared,
            code.certification.as_ref().as_ref(),
        )?;
        timing::record_stage(
            timing::NO_NODE,
            timing::NO_ROUND,
            timing::STAGE_INSTALL_PREPARED,
            install_prepared_started.elapsed(),
            0,
        );
        let realm = self.run_context.resource_scope;
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
        let run = match ran {
            Ok(Ok(run)) => run,
            Ok(Err(error)) => {
                self.retire_failed_turn_source_instances(lexical_scope, &source_keys);
                return Err(error.into());
            }
            Err(error) => {
                self.retire_failed_turn_source_instances(lexical_scope, &source_keys);
                return Err(error);
            }
        };
        self.complete_prepared(run, mode, program, lexical_scope, provenance, None, checked)
    }

    fn install_turn_program_in(
        &mut self,
        lexical_scope: ScopeId,
        prepared: PreparedProgram,
        certification: Option<&super::turn::TurnCertification>,
    ) -> Result<
        (
            ProgramId,
            tidepool_codegen::binding_table::SourceScopeAdmission,
        ),
        ResidentError,
    > {
        if let Some(certification) = certification {
            let resolved =
                self.state
                    .resolve_certification_in(lexical_scope, &prepared, certification)?;
            let registry = self.state.certified_image_registry();
            let (target, demanded) = super::prepared::CertifiedTargetImage::compile_scoped(
                prepared, &resolved, &registry,
            )?;
            self.install_certified_turn_in(
                lexical_scope,
                target,
                &resolved.target_owners,
                &resolved.source_evidence,
                demanded,
                &resolved.inherited_needed,
            )
            .map_err(Into::into)
        } else {
            Ok((self.state.install_prepared(prepared)?, Default::default()))
        }
    }

    /// Install and retain the original startup program without evaluating any
    /// authored code. Its actor must admit the application before consuming it.
    pub fn prepare_startup_entry(
        &mut self,
        code: TurnCode<'_>,
    ) -> Result<PreparedStartupEntry, ResidentError> {
        refuse_checked_turn(&code)?;
        let certification = code
            .certification
            .as_ref()
            .as_ref()
            .ok_or(ResidentError::UnsealedStartupEntry)?;
        let proof = certification
            .original_compile_input
            .as_ref()
            .ok_or(ResidentError::UnsealedStartupEntry)?;
        if !proof.matches_bundle(
            &code.prepared,
            &certification.groups,
            &certification.target_owners,
            &certification.package_interfaces,
            &code.table,
            &code.sites,
        ) {
            return Err(ResidentError::UnsealedStartupEntry);
        }
        let identity = StartupCompileIdentity::Issued(Arc::clone(proof));
        self.prepare_startup_entry_installed(code, identity)
    }

    fn prepare_startup_entry_installed(
        &mut self,
        code: TurnCode<'_>,
        compile_identity: StartupCompileIdentity,
    ) -> Result<PreparedStartupEntry, ResidentError> {
        self.settle_dropped_custody();
        let scope = self.run_context.lexical_scope;
        if self.state.public_visibility_snapshot_in(scope).is_none() {
            return Err(PreparedRuntimeError::SourceScopeAdmission.into());
        }
        let provenance = self.provenance_for(&code)?;
        self.state
            .merge_table(&code.table)
            .map_err(ResidentError::TableCollision)?;
        let (program, source_keys) = self.install_turn_program_in(
            scope,
            code.prepared.into_owned(),
            code.certification.as_ref().as_ref(),
        )?;
        let admitted = self
            .state
            .public_visibility_snapshot_in(scope)
            .expect("installation preserves its live lexical scope");
        Ok(PreparedStartupEntry {
            program: Some(program),
            source_keys,
            provenance,
            admitted,
            realm: self.run_context.resource_scope,
            cleanup: Arc::clone(&self.custody_cleanup),
            _lease: self.lease_bindings(&[]),
            compile_identity,
        })
    }

    /// Consume the original startup capsule under the admitted actor's current
    /// principal and effect policy. Durable publication may advance its public
    /// epoch and declaration tip: execution uses the retained installed program,
    /// with no compiler lookup. Native bindings and source instances remain exact.
    pub fn run_startup_entry(
        &mut self,
        mut entry: PreparedStartupEntry,
    ) -> Result<ResidentOutcome, ResidentError> {
        self.settle_dropped_custody();
        if !Arc::ptr_eq(&entry.cleanup, &self.custody_cleanup) {
            return Err(ResidentError::ForeignCustody);
        }
        let scope = self.run_context.lexical_scope;
        let current = self
            .state
            .public_visibility_snapshot_in(scope)
            .ok_or(ResidentError::StaleStartupEntry)?;
        if entry.realm != self.run_context.resource_scope
            || current.scope != entry.admitted.scope
            || current.machine_incarnation != entry.admitted.machine_incarnation
            || current.bindings != entry.admitted.bindings
            || current.source_instances != entry.admitted.source_instances
            || current.source_selection != entry.admitted.source_selection
        {
            return Err(ResidentError::StaleStartupEntry);
        }
        let program = entry.program.expect("startup capsule consumes once");
        let provenance = Arc::clone(&entry.provenance);
        let realm = entry.realm;
        let park = ParkPolicy {
            principal: self.run_context.principal,
            effect_policy: self.state.effect_policy(),
            live_payload: self.state.live_payload_policy(),
        };
        let ran = self.on_eval_thread(move |engine, table, handlers, captured| {
            Ok(settle_prepared(
                engine,
                program,
                realm,
                None,
                SettlePlan::Observe,
                park,
                table,
                handlers,
                captured,
            ))
        });
        let run = match ran.and_then(|run| run.map_err(ResidentError::from)) {
            Ok(run) => run,
            Err(error) => {
                drop(entry);
                self.settle_dropped_custody();
                return Err(error);
            }
        };
        // Evaluation now owns its returned value or parked frame. Until this
        // handoff, the capsule also guards unwinds on the calling thread.
        entry.program = None;
        entry.source_keys = Default::default();
        if let Some(engine) = self.state.prepared_mut() {
            engine.unpin(program);
        }
        self.complete_prepared(
            run,
            PreparedTurnMode::Value,
            program,
            scope,
            provenance,
            None,
            None,
        )
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
    /// The caller checks [`Self::prepared_machine_ready`] first. A legacy
    /// turn cannot snapshot before machine bootstrap and gets a typed error;
    /// certified bootstrap uses the single-checkout path.
    pub fn snapshot_run_prepared(
        &mut self,
        code: TurnCode<'static>,
        mode: PendingPreparedMode,
        argument: Option<PreparedHandle>,
    ) -> Result<PendingPreparedInstall, ResidentError> {
        self.state
            .validate_new_binding_ids(binding_ids_of(&mode.as_mode()))?;
        let checked = checked_turn_plan(
            code.certification.as_ref(),
            code.prepared.as_ref(),
            &code.table,
            &mode.as_mode(),
        )?;
        let provenance = self.provenance_for(&code)?;
        let prepared = code.prepared.into_owned();
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
        let lexical_scope = self.run_context.lexical_scope;
        let snapshot = if let Some(certification) = code.certification.as_ref() {
            let admitted_public = self
                .state
                .public_visibility_snapshot_in(lexical_scope)
                .ok_or(PreparedRuntimeError::SourceScopeAdmission)?;
            let resolved =
                self.state
                    .resolve_certification_in(lexical_scope, &prepared, certification)?;
            PendingPreparedSource::Certified {
                prepared,
                resolved,
                registry: self.state.certified_image_registry(),
                admitted_public,
            }
        } else {
            PendingPreparedSource::Legacy(
                self.state
                    .snapshot_install_prepared(prepared)?
                    .ok_or(PreparedRuntimeError::MachineNotInstalled)?,
            )
        };
        let realm = self.run_context.resource_scope;
        let park = ParkPolicy {
            principal: self.run_context.principal,
            effect_policy: self.state.effect_policy(),
            live_payload: self.state.live_payload_policy(),
        };
        Ok(PendingPreparedInstall {
            snapshot,
            metadata: PreparedInstallMetadata {
                mode,
                argument,
                provenance,
                realm,
                lexical_scope,
                park,
                checked,
            },
        })
    }

    /// Revalidate the capsule's original imports against this checkout's
    /// live bindings and, if nothing changed, install its image and run exactly
    /// as [`Self::run_prepared_with_argument`]'s tail does. `Ok(None)`
    /// means revalidation found a stale import (see
    /// `PreparedEngine::revalidate_and_install`): the caller must recompile
    /// from a fresh [`Self::snapshot_run_prepared`], or fall back to the
    /// single-checkout [`Self::run_prepared_with_argument`].
    pub fn revalidate_and_run_prepared(
        &mut self,
        ready: ReadyPreparedInstall,
    ) -> Result<Option<ResidentOutcome>, ResidentError> {
        let ReadyPreparedInstall { source, metadata } = ready;
        let PreparedInstallMetadata {
            mode,
            argument,
            provenance,
            realm,
            lexical_scope,
            park,
            checked,
        } = metadata;
        self.state
            .validate_new_binding_ids(binding_ids_of(&mode.as_mode()))?;
        let install_started = std::time::Instant::now();
        let mut checked_completion = None;
        let (program, source_keys) = match source {
            ReadyPreparedSource::Legacy { snapshot, image } => {
                match self
                    .state
                    .revalidate_and_install_prepared(snapshot, image)?
                {
                    Some(program) => (program, Default::default()),
                    None => return Ok(None),
                }
            }
            ReadyPreparedSource::Certified {
                resolved,
                admitted_public,
                target,
                demanded,
            } => {
                if self
                    .state
                    .public_visibility_snapshot_in(lexical_scope)
                    .as_ref()
                    != Some(&admitted_public)
                {
                    return Ok(None);
                }
                if checked
                    .as_ref()
                    .is_some_and(|checked| !checked.execution.matches_target(target.prepared()))
                {
                    return Err(PreparedRuntimeError::CertifiedTargetOwners.into());
                }
                checked_completion = checked
                    .map(|checked| checked.start(&self.state, lexical_scope))
                    .transpose()?;
                self.install_certified_turn_in(
                    lexical_scope,
                    target,
                    &resolved.target_owners,
                    &resolved.source_evidence,
                    demanded,
                    &resolved.inherited_needed,
                )?
            }
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
        let run = match ran {
            Ok(Ok(run)) => run,
            Ok(Err(error)) => {
                self.retire_failed_turn_source_instances(lexical_scope, &source_keys);
                return Err(error.into());
            }
            Err(error) => {
                self.retire_failed_turn_source_instances(lexical_scope, &source_keys);
                return Err(error);
            }
        };
        Ok(Some(self.complete_prepared(
            run,
            mode.as_mode(),
            program,
            lexical_scope,
            provenance,
            None,
            checked_completion,
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
        checked: Option<Arc<CheckedTurnCompletion>>,
    ) -> Result<ResidentOutcome, ResidentError> {
        let seed = hole_seed_of(&mode, lexical_scope, checked.clone());
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
                            checked.as_deref(),
                            ValueInterfaceSource::for_checked(checked.is_some()),
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
                self.bind_prepared(
                    program,
                    lexical_scope,
                    generation,
                    &bound,
                    checked.as_deref(),
                    ValueInterfaceSource::for_checked(checked.is_some()),
                )?;
                for binder in binders {
                    self.binding_provenance
                        .insert(binder.var_id, Arc::clone(&provenance));
                }
                self.classify_parked(ParkedRun::CompletedProject, resumed, seed, provenance)
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
            PreparedRun::Deferred { id, request, work } => self.classify_parked(
                ParkedRun::Deferred { id, request, work },
                resumed,
                seed,
                provenance,
            ),
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
        if matches!(
            &outcome,
            ResidentOutcome::Completed { .. } | ResidentOutcome::BindingsCommitted { .. }
        ) {
            if let Some(checked) = checked {
                checked.settle(&mut self.state, program)?;
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
        checked: Option<&CheckedTurnCompletion>,
        interface_source: ValueInterfaceSource,
    ) -> Result<(), ResidentError> {
        if let ValueInterfaceSource::Staged(staged) = &interface_source {
            let validated = self
                .state
                .validate_staged_value_interface(staged)
                .and_then(|()| {
                    if staged.module() != SessionModule::val(generation)
                        || bound
                            .iter()
                            .any(|(binder, _)| binder.module != staged.module().module_name())
                    {
                        Err(SessionError::StaleStagedDeclaration)
                    } else {
                        Ok(())
                    }
                });
            if let Err(error) = validated {
                if let Some(engine) = self.state.prepared_mut() {
                    engine.release_all(bound.iter().map(|(_, handle)| *handle));
                }
                return Err(error.into());
            }
        }
        let binders = bound.iter().map(|(binder, _)| *binder).collect::<Vec<_>>();
        let overlay_validation = match checked {
            Some(completion) => completion
                .validates_private_overlay(&self.state, scope, &binders)
                .map(|private| private.then_some(completion)),
            None => Ok(None),
        };
        let private_overlay = match overlay_validation {
            Ok(completion) => completion,
            Err(error) => {
                if let Some(engine) = self.state.prepared_mut() {
                    engine.release_all(bound.iter().map(|(_, handle)| *handle));
                }
                return Err(error.into());
            }
        };
        let scope_is_live = self.state.scope_tree().is_live(scope);
        let engine = self.state.require_prepared()?;
        if !scope_is_live {
            engine.release_all(bound.iter().map(|(_, handle)| *handle));
            return Err(SessionError::DeadScope(scope).into());
        }
        let unit = match &interface_source {
            ValueInterfaceSource::Checked | ValueInterfaceSource::Staged(_) => "main".to_owned(),
            ValueInterfaceSource::LegacyDisk => engine.entry_unit(program).unwrap_or_default(),
        };
        for (_, handle) in bound {
            if !engine.adopt(*handle) {
                engine.release_all(bound.iter().map(|(_, handle)| *handle));
                return Err(PreparedRuntimeError::Run(
                    tidepool_codegen::prepared_program::ExecutionError::UnknownPreparedHandle,
                )
                .into());
            }
        }
        let mut entries = Vec::with_capacity(bound.len());
        for (binder, handle) in bound {
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
                    handle: *handle,
                    identity,
                },
                type_display: Some(binder.type_display.clone()),
                defining_expr: None,
                scope,
            };
            entries.push(entry);
        }
        if let Some(completion) = private_overlay {
            self.state
                .bind_checked_private_values_in(completion, scope, entries, &binders)?;
        } else {
            self.state.bind_replacing_decls_in(scope, entries)?;
        }
        // Ordinary fragments retain their established filesystem interface
        // contract. Checked settlements register their sealed artifact instead.
        match interface_source {
            ValueInterfaceSource::LegacyDisk => self
                .state
                .mark_legacy_value_interface(SessionModule::val(generation)),
            ValueInterfaceSource::Staged(staged) => {
                self.state.commit_staged_value_interface(staged)
            }
            ValueInterfaceSource::Checked => {}
        }
        for _ in bound {
            self.advance_public_visibility(scope);
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

    /// Capture a bare workbench result using the same suspendable
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
        let id = SessionVarId::from_extract(binder.var_id);
        let scope = self.state.bindings().get(id).map(|entry| entry.scope);
        self.state.save_observation(id, dependencies);
        if let Some(scope) = scope {
            self.advance_public_visibility(scope);
        }
        self.prune_binding_metadata();
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
    /// `entry` is a `ValueHandle` over a tenured `Int -> Eff effects a` closure. It is
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
        if !Arc::ptr_eq(&entry.cleanup.0, &self.custody_cleanup) {
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
        if !Arc::ptr_eq(&function.cleanup.0, &self.custody_cleanup)
            || !Arc::ptr_eq(&argument.cleanup.0, &self.custody_cleanup)
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
        byte_budget: usize,
    ) -> Option<String> {
        if !Arc::ptr_eq(&custody.cleanup.0, &self.custody_cleanup) {
            return None;
        }
        let handle = custody.handle?;
        let table = self.state.session_table().clone();
        let engine = self.state.prepared_mut()?;
        // The walk itself runs against a generous internal cap (bounded work
        // regardless of `byte_budget`), then the caller's exact budget is
        // enforced once, at a line boundary, below -- cutting mid-walk would
        // land wherever a field happened to end, not at a readable line.
        let mut walk_budget = byte_budget.saturating_mul(4).max(4096);
        let mut out = String::new();
        render_retained_layer(engine, &table, handle, 0, &mut walk_budget, &mut out);
        Some(truncate_preview_at_line(out, byte_budget))
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
            HoleSeed {
                obligation: HoleObligation::Plain,
                checked: None,
            },
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
        let Some(entry) = self.parked.iter().find(|entry| entry.name == cont_id) else {
            return Err(ResidentResumeError::Rejected(
                ResidentError::WrongContinuation {
                    attempted: cont_id.to_string(),
                    pending: self.parked.iter().map(|entry| entry.name.clone()).collect(),
                },
            ));
        };
        let frame_id = entry.id;
        let mut provenance = (*entry.provenance).clone();
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
        let bindings_before = self.state.bindings().mutation_revision();
        self.state.reap_admission_leases();
        let startup_entries = std::mem::take(&mut *self.custody_cleanup.startup_entries.lock());
        let startup_count = startup_entries.len();
        for entry in startup_entries {
            if let Some(engine) = self.state.prepared_mut() {
                engine.unpin(entry.program);
            }
            if self.state.scope_tree().is_live(entry.scope) {
                let changed = self
                    .state
                    .retire_failed_turn_source_instances(entry.scope, &entry.source_keys);
                if changed {
                    self.advance_public_visibility(entry.scope);
                }
            }
        }
        let leases = std::mem::take(&mut *self.custody_cleanup.binding_leases.lock());
        let mut released = Vec::new();
        for retained in leases {
            released.extend(self.state.release_binding_leases(retained));
        }
        released.extend(self.state.collect_binding_observations());
        let changed_scopes: HashSet<_> = released.iter().map(|entry| entry.scope).collect();
        let binding_count = self.state.release_binding_roots(released);
        for scope in changed_scopes {
            self.advance_public_visibility(scope);
        }
        // Last leases can remove entries after their scopes were retired.
        // Exhausted revision witnesses require pruning on every owner entry.
        if bindings_before.is_none() || self.state.bindings().mutation_revision() != bindings_before
        {
            self.prune_binding_metadata();
        }
        let handles = self.custody_cleanup.take_all();
        let count = handles.len();
        if let Some(engine) = self.state.prepared_mut() {
            for handle in handles {
                engine.discard_handle(handle);
            }
        }
        count + binding_count + startup_count
    }

    /// Install the signal an owning lifecycle manager uses to resume cleanup
    /// after a root is dropped while this session is checked in. The callback
    /// is invoked after cleanup has been queued and the external lease count
    /// released, with no session or cleanup lock held.
    pub fn set_custody_cleanup_notifier(&mut self, notify: Arc<dyn Fn() + Send + Sync>) {
        self.custody_cleanup.set_notifier(notify);
    }

    /// External rooted values, binding leases, and in-flight transfers still
    /// retaining this session. A dropped lease queues cleanup and releases its
    /// count before waking the lifecycle owner, so notification never reports
    /// the final releasing token as an outstanding reader.
    pub fn outstanding_custody(&mut self) -> usize {
        self.settle_dropped_custody();
        self.custody_cleanup.external_leases.load(Ordering::Acquire)
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
        let mut resource_owners = resumed
            .and_then(|name| self.parked.iter().find(|entry| entry.name == name))
            .map(|entry| entry.resource_owners.clone())
            .unwrap_or_default();
        if let Some(owner) = &self.continuation_resource_owner {
            if !resource_owners
                .iter()
                .any(|retained| Arc::ptr_eq(retained, owner))
            {
                resource_owners.push(owner.clone());
            }
        }
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
                self.parked.push(ParkedContinuation {
                    name: cont_id.clone(),
                    id,
                    provenance,
                    resource_owners,
                });
                if let Some(observer) = &self.continuation_observer {
                    observer(ResidentContinuationEvent::Parked(cont_id.clone()));
                }
                let output = self.captured.snapshot();
                ResidentOutcome::Suspended {
                    output,
                    hole: ResidentHole::mint(cont_id, seed),
                    request,
                }
            }
            ParkedRun::Deferred { id, request, work } => {
                self.retire_resumed(resumed);
                let cont_id = self.next_cont_id();
                self.parked.push(ParkedContinuation {
                    name: cont_id.clone(),
                    id,
                    provenance,
                    resource_owners,
                });
                if let Some(observer) = &self.continuation_observer {
                    observer(ResidentContinuationEvent::Parked(cont_id.clone()));
                }
                ResidentOutcome::Deferred {
                    output: self.captured.snapshot(),
                    hole: ResidentHole::mint(cont_id, seed),
                    request,
                    work,
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
            self.parked.retain(|entry| entry.name != cont_id);
            if let Some(observer) = &self.continuation_observer {
                observer(ResidentContinuationEvent::Retired(cont_id.to_owned()));
            }
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
        let HoleSeed {
            obligation,
            checked,
        } = seed;
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
        let (lexical_scope, mode) = match &obligation {
            HoleObligation::Plain => (self.run_context.lexical_scope, PreparedTurnMode::Value),
            HoleObligation::Binding {
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
            HoleObligation::ProjectedBinding {
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
        self.complete_prepared(
            run,
            mode,
            runner,
            lexical_scope,
            provenance,
            Some(cont_id),
            checked,
        )
    }

    /// Retain independent custody of the current root-scope binding.
    pub fn retain_binding_custody(
        &mut self,
        name: &str,
    ) -> Result<Option<RootCustody>, ResidentError> {
        let Some(entry) = self.state.bindings().resolve(name) else {
            return Ok(None);
        };
        self.retain_binding_custody_in(ScopeId::ROOT, name, entry.id)
    }

    /// Retain the exact binding visible from `scope`. The new handle owns a
    /// separate root of the same object and survives retirement of the binding.
    pub fn retain_binding_custody_in(
        &mut self,
        scope: ScopeId,
        name: &str,
        expected: SessionVarId,
    ) -> Result<Option<RootCustody>, ResidentError> {
        let Some((handle, provenance)) = self.exact_binding_source(scope, name, expected)? else {
            return Ok(None);
        };
        let engine = self
            .state
            .prepared_mut()
            .ok_or(PreparedRuntimeError::SourceScopeAdmission)?;
        let prepared = engine
            .prepared_handle_of(handle)
            .ok_or(PreparedRuntimeError::SourceScopeAdmission)?;
        let retained = engine.retain_handle_value(prepared, RealmId::ROOT)?;
        Ok(Some(RootCustody::new(
            retained.raw(),
            Arc::clone(&self.custody_cleanup),
            provenance,
        )))
    }

    fn exact_binding_source(
        &self,
        scope: ScopeId,
        name: &str,
        expected: SessionVarId,
    ) -> Result<Option<(ValueHandle, Arc<ProgramProvenance>)>, ResidentError> {
        if !self.state.scope_tree().is_live(scope) {
            return Err(SessionError::DeadScope(scope).into());
        }
        let Some(entry) = self
            .state
            .resolve_in(scope, name)
            .filter(|entry| entry.id == expected)
        else {
            return Ok(None);
        };
        let reachable = self
            .state
            .bindings()
            .scope_reachable_binding_ids(self.state.scope_tree(), self.run_context.lexical_scope);
        if !reachable.contains(&expected) {
            return Err(PreparedRuntimeError::SourceScopeAdmission.into());
        }
        Ok(Some((
            entry.value.handle.raw(),
            self.binding_provenance
                .get(&expected.raw())
                .cloned()
                .unwrap_or_default(),
        )))
    }

    /// Resolve and borrow an exact mounted binding while delivering one framed
    /// response. Missing or shadowed input leaves the parked hole untouched.
    pub fn resume_framed_binding_sources_classified<T>(
        &mut self,
        hole: &ResidentHole,
        scope: ScopeId,
        name: &str,
        expected: SessionVarId,
        constructor: tidepool_repr::DataConId,
        prefix: Vec<T>,
    ) -> Result<Option<ResidentOutcome>, ResidentResumeError>
    where
        T: tidepool_bridge::ToHaskell + Send + 'static,
    {
        let Some((handle, provenance)) = self
            .exact_binding_source(scope, name, expected)
            .map_err(ResidentResumeError::Rejected)?
        else {
            return Ok(None);
        };
        let seed = hole.seed();
        let cont_id = match hole {
            ResidentHole::Plain(hole) => &hole.id,
            ResidentHole::Binding(hole) => &hole.id,
            ResidentHole::ProjectedBinding(hole) => &hole.id,
        };
        self.reenter(
            cont_id,
            ResidentResumeInput::FramedHandleSources {
                handle,
                constructor,
                prefix: prefix
                    .into_iter()
                    .map(|field| Box::new(field) as Box<dyn tidepool_bridge::ToHaskell + Send>)
                    .collect(),
            },
            seed,
            Some(&provenance),
        )
        .map(Some)
    }

    fn retire_resumed(&mut self, resumed: Option<&str>) {
        let Some(hole) = resumed else {
            return;
        };
        self.parked.retain(|entry| entry.name != hole);
        if let Some(observer) = &self.continuation_observer {
            observer(ResidentContinuationEvent::Retired(hole.to_owned()));
        }
    }
}

#[cfg(test)]
mod custody_release_tests {
    use super::*;

    #[derive(Clone)]
    struct EmptyOutput;
    impl OutputSink for EmptyOutput {
        fn drain(&self) -> Vec<String> {
            Vec::new()
        }
        fn snapshot(&self) -> Vec<String> {
            Vec::new()
        }
    }
    type TestSession = ResidentSession<frunk::HNil, EmptyOutput>;

    #[test]
    fn final_external_lease_is_released_before_its_cleanup_notification() {
        for mode in 0..4 {
            let mut session = TestSession::unbootstrapped(frunk::HNil, EmptyOutput, 1024, None);
            let cleanup = Arc::clone(&session.custody_cleanup);
            let provenance = Arc::new(ProgramProvenance::default());
            let action: Box<dyn FnOnce() + Send> = match mode {
                0 => {
                    let token = RootCustody::new(ValueHandle(1), cleanup.clone(), provenance);
                    Box::new(move || drop(token))
                }
                1..=2 => {
                    let token = RootCustody::new(ValueHandle(1), cleanup.clone(), provenance);
                    let transfer = token.into_transfer();
                    if mode == 2 {
                        Box::new(move || transfer.commit())
                    } else {
                        Box::new(move || drop(transfer))
                    }
                }
                _ => {
                    let token = session.lease_bindings(&[]);
                    Box::new(move || drop(token))
                }
            };
            drop(cleanup);
            assert_eq!(session.outstanding_custody(), 1);
            let (notification, observing) = std::sync::mpsc::sync_channel(0);
            let (checked, after_check) = std::sync::mpsc::sync_channel(0);
            let after_check = Mutex::new(after_check);
            session.set_custody_cleanup_notifier(Arc::new(move || {
                notification.send(()).unwrap();
                after_check
                    .lock()
                    .recv_timeout(std::time::Duration::from_secs(2))
                    .unwrap();
            }));
            let dropping = std::thread::spawn(action);
            // Force cleanup's observer to run while the releasing token's
            // destructor is blocked inside its notification callback.
            observing
                .recv_timeout(std::time::Duration::from_secs(2))
                .unwrap();
            let outstanding = session.outstanding_custody();
            let queued_roots = session.custody_cleanup.abandoned.lock().len();
            let queued_bindings = session.custody_cleanup.binding_leases.lock().len();
            checked.send(()).unwrap();
            dropping.join().unwrap();
            assert_eq!(outstanding, 0, "release mode {mode}");
            assert_eq!(
                queued_roots, 0,
                "notification permits draining queued roots"
            );
            assert_eq!(
                queued_bindings, 0,
                "notification permits draining binding leases"
            );
        }
    }

    fn bind_fixture(session: &mut TestSession, scope: ScopeId, generation: u64) -> SessionVarId {
        let mut binding = crate::session::prepared::tests::evaluated_publication_fixture(
            &mut session.state,
            "x",
            generation,
        );
        binding.scope = scope;
        let id = binding.id;
        session.state.bind_in(scope, binding).unwrap();
        id
    }

    fn major_collect(session: &mut TestSession) {
        session
            .state
            .require_prepared()
            .unwrap()
            .quiesce_and_collect_now()
            .unwrap();
    }

    fn assert_live_binding(session: &mut TestSession, scope: ScopeId, id: SessionVarId) {
        let entry = session.state.resolve_in(scope, "x").unwrap();
        assert_eq!(entry.id, id);
        let handle = entry.value.handle.raw();
        assert!(
            matches!(session.state.prepared_mut().unwrap().inspect_retained(handle).unwrap(),
            PreparedOuter::Constructor { fields, .. } if matches!(fields.as_slice(),
                [PreparedResult::Scalar(99)]))
        );
    }

    #[test]
    fn exclusive_binding_custody_consumption_preserves_source_after_major_gc() {
        let mut session = TestSession::unbootstrapped(frunk::HNil, EmptyOutput, 1024, None);
        let id = bind_fixture(&mut session, ScopeId::ROOT, 41);
        let provenance = Arc::new(ProgramProvenance::default());
        session
            .binding_provenance
            .insert(id.raw(), provenance.clone());
        let original = session.value_handle_count();
        let custody = session.retain_binding_custody("x").unwrap().unwrap();
        assert!(Arc::ptr_eq(&custody.provenance, &provenance));
        assert_eq!(session.value_handle_count(), original + 1);
        assert!(session.discard_custody(custody));
        major_collect(&mut session);
        assert_live_binding(&mut session, ScopeId::ROOT, id);
        let custody = session.retain_binding_custody("x").unwrap().unwrap();
        let parcel = session.export_custody(custody).unwrap();
        major_collect(&mut session);
        assert_live_binding(&mut session, ScopeId::ROOT, id);
        drop(parcel);
        let custody = session.retain_binding_custody("x").unwrap().unwrap();
        let custody = session.rehome_custody(custody, RealmId::fresh()).unwrap();
        assert!(session.discard_custody(custody));
        major_collect(&mut session);
        assert_live_binding(&mut session, ScopeId::ROOT, id);
        let custody = session.retain_binding_custody("x").unwrap().unwrap();
        let owner = RealmId::fresh();
        let custody = session.rehome_custody(custody, owner).unwrap();
        assert_eq!(session.close_realm(owner), (0, 1));
        drop(custody);
        major_collect(&mut session);
        assert_live_binding(&mut session, ScopeId::ROOT, id);
        assert_eq!(session.value_handle_count(), original);
    }

    #[test]
    fn exclusive_binding_custody_survives_source_scope_retirement() {
        for (retain_binding, retain_lexical) in [(false, false), (true, false), (false, true)] {
            let mut session = TestSession::unbootstrapped(frunk::HNil, EmptyOutput, 1024, None);
            let other_scope = session.mint_scope(ScopeId::ROOT).unwrap();
            let other_id = bind_fixture(&mut session, other_scope, 43);
            let other_provenance = Arc::new(ProgramProvenance::default());
            session
                .binding_provenance
                .insert(other_id.raw(), Arc::clone(&other_provenance));
            let scope = session.mint_scope(ScopeId::ROOT).unwrap();
            let id = bind_fixture(&mut session, scope, 42);
            let provenance = Arc::new(ProgramProvenance::default());
            let weak = Arc::downgrade(&provenance);
            session
                .binding_provenance
                .insert(id.raw(), Arc::clone(&provenance));
            let binding_lease = retain_binding.then(|| session.lease_bindings(&[id.var()]));
            let lexical_lease =
                retain_lexical.then(|| session.retain_lexical_scope(scope).unwrap());
            let retained_scope = lexical_lease.as_ref().map(|lease| lease.scope());
            let leased = retain_binding || retain_lexical;
            session
                .set_run_context(SessionRunContext {
                    lexical_scope: scope,
                    ..SessionRunContext::ROOT
                })
                .unwrap();
            let custody = session
                .retain_binding_custody_in(scope, "x", id)
                .unwrap()
                .unwrap();
            assert!(Arc::ptr_eq(&custody.provenance, &provenance));
            drop(provenance);
            let retained = custody.handle.unwrap();
            session.set_run_context(SessionRunContext::ROOT).unwrap();
            session.retire_scope(scope);
            assert_eq!(session.state.bindings().get(id).is_some(), leased);
            assert_eq!(session.binding_provenance.contains_key(&id.raw()), leased);
            assert!(Arc::ptr_eq(
                &session.binding_provenance[&other_id.raw()],
                &other_provenance
            ));
            assert!(
                Arc::ptr_eq(&weak.upgrade().unwrap(), &custody.provenance),
                "retained value keeps its original compiler metadata independently"
            );
            major_collect(&mut session);
            assert!(
                matches!(session.state.prepared_mut().unwrap().inspect_retained(retained).unwrap(),
                PreparedOuter::Constructor { fields, .. } if matches!(fields.as_slice(),
                    [PreparedResult::Scalar(99)]))
            );
            assert_live_binding(&mut session, other_scope, other_id);
            assert!(matches!(
                session.retain_binding_custody_in(scope, "x", id),
                Err(ResidentError::Session(SessionError::DeadScope(_)))
            ));
            assert!(session.discard_custody(custody));
            assert_eq!(weak.upgrade().is_some(), leased);
            drop(binding_lease);
            drop(lexical_lease);
            assert_eq!(session.outstanding_custody(), 0);
            if let Some(retained_scope) = retained_scope {
                assert!(!session.state.scope_tree().is_live(retained_scope));
            }
            assert!(session.state.bindings().get(id).is_none());
            assert!(!session.binding_provenance.contains_key(&id.raw()));
            assert!(
                weak.upgrade().is_none(),
                "last metadata share retires after the owning lease cleanup"
            );
            assert!(Arc::ptr_eq(
                &session.binding_provenance[&other_id.raw()],
                &other_provenance
            ));
            major_collect(&mut session);
            assert_live_binding(&mut session, other_scope, other_id);
            session.retire_scope(other_scope);
            assert!(!session.binding_provenance.contains_key(&other_id.raw()));
            major_collect(&mut session);
            assert_eq!(session.value_handle_count(), 0);
        }
    }

    #[test]
    fn exact_borrowed_binding_refusals_precede_hole_access_and_root_allocation() {
        let mut session = TestSession::unbootstrapped(frunk::HNil, EmptyOutput, 1024, None);
        let id = bind_fixture(&mut session, ScopeId::ROOT, 43);
        let capture = session.mint_detached_scope(ScopeId::ROOT).unwrap();
        session
            .set_run_context(SessionRunContext {
                lexical_scope: capture,
                ..SessionRunContext::ROOT
            })
            .unwrap();
        let hole = ResidentHole::mint(
            "not-a-parked-hole".into(),
            HoleSeed {
                obligation: HoleObligation::Plain,
                checked: None,
            },
        );
        let before = session.value_handle_count();
        assert!(matches!(
            session.resume_framed_binding_sources_classified(
                &hole,
                ScopeId::ROOT,
                "x",
                id,
                DataConId(0),
                Vec::<i64>::new()
            ),
            Err(ResidentResumeError::Rejected(
                ResidentError::WrongContinuation { .. }
            ))
        ));
        assert_eq!(session.value_handle_count(), before);
        assert_eq!(session.outstanding_custody(), 0);
        // The detached capture retains the old root but original-name lookup
        // still refuses a later replacement of that exact request input.
        session.set_run_context(SessionRunContext::ROOT).unwrap();
        bind_fixture(&mut session, ScopeId::ROOT, 44);
        session
            .set_run_context(SessionRunContext {
                lexical_scope: capture,
                ..SessionRunContext::ROOT
            })
            .unwrap();
        assert!(session
            .resume_framed_binding_sources_classified(
                &hole,
                ScopeId::ROOT,
                "x",
                id,
                DataConId(0),
                Vec::<i64>::new()
            )
            .unwrap()
            .is_none());
        let foreign_scope = session.mint_isolated_scope();
        let foreign_id = bind_fixture(&mut session, foreign_scope, 45);
        assert!(matches!(
            session.resume_framed_binding_sources_classified(
                &hole,
                foreign_scope,
                "x",
                foreign_id,
                DataConId(0),
                Vec::<i64>::new()
            ),
            Err(ResidentResumeError::Rejected(ResidentError::Prepared(
                PreparedRuntimeError::SourceScopeAdmission
            )))
        ));
        session.retire_scope(foreign_scope);
        assert!(matches!(
            session.resume_framed_binding_sources_classified(
                &hole,
                foreign_scope,
                "x",
                foreign_id,
                DataConId(0),
                Vec::<i64>::new()
            ),
            Err(ResidentResumeError::Rejected(ResidentError::Session(
                SessionError::DeadScope(_)
            )))
        ));
        assert_eq!(session.outstanding_custody(), 0);
    }

    #[test]
    fn transfer_keeps_exact_lease_count_until_final_release() {
        let mut session = TestSession::unbootstrapped(frunk::HNil, EmptyOutput, 1024, None);
        let cleanup = Arc::clone(&session.custody_cleanup);
        let token = RootCustody::new(ValueHandle(1), cleanup.clone(), Arc::default());
        drop(cleanup);
        assert_eq!(session.outstanding_custody(), 1);
        let token = token.into_transfer().into_custody();
        assert_eq!(session.outstanding_custody(), 1);
        assert!(session.custody_cleanup.abandoned.lock().is_empty());
        drop(token);
        assert_eq!(
            session.custody_cleanup.abandoned.lock().as_slice(),
            &[ValueHandle(1)]
        );
        assert_eq!(session.outstanding_custody(), 0);
    }
}

#[cfg(test)]
mod authored_publication_tests {
    use super::*;
    use crate::session::{recovery, ModuleEnv, SessionId};

    #[derive(Clone)]
    struct EmptyOutput;

    impl OutputSink for EmptyOutput {
        fn drain(&self) -> Vec<String> {
            Vec::new()
        }
        fn snapshot(&self) -> Vec<String> {
            Vec::new()
        }
    }

    type TestSession = ResidentSession<frunk::HNil, EmptyOutput>;

    #[test]
    fn checked_interface_publication_conflict_preserves_private_prefix() {
        checked_interface_publication_failure(false);
    }

    #[test]
    fn checked_interface_publication_write_failure_preserves_private_prefix() {
        checked_interface_publication_failure(true);
    }

    fn checked_interface_publication_failure(write_failure: bool) {
        use crate::session::turn::{compile_cell_program_admitted, consume_cell_program_item};
        use crate::session::{
            resident_cell_check_template, resident_workbench_templates, SourceImports, TurnResult,
        };
        use std::os::unix::fs::PermissionsExt;
        use tidepool_testing::effect_surface::TestEffectSurface;
        use tidepool_toolchain::checked_cell::CheckedCellSpecification;
        tidepool_testing::eval_harness::require_extract();
        let root = tempfile::tempdir().unwrap();
        let effects = TestEffectSurface::minimal(&[]).unwrap();
        let library =
            SessionLib::open(SessionId(999), root.path(), ModuleEnv::standalone_default())
                .unwrap()
                .with_validation_include(effects.include_paths().to_vec());
        let mut state = PersistentSession::new(Some(library), 1024 * 1024);
        let public = state.mint_scope(ScopeId::ROOT).unwrap();
        let execution = Arc::new(state.begin_private_execution(public).unwrap());
        let view = state.compile_view_for_execution(&execution).unwrap();
        let imports = view.turn_imports(&SourceImports::from_specs(["qualified Prelude as P"]));
        let template = resident_cell_check_template(effects.preamble(), effects.row(), &imports);
        let templates = resident_workbench_templates(effects.preamble(), effects.row(), &imports);
        let source = include_str!("fixtures/checked-interface-publication.hs");
        let specification = CheckedCellSpecification {
            admission_digest: [0; 32],
            cell_source: source.into(),
            template_source: template.clone(),
            turn_templates: templates
                .iter()
                .map(|template| {
                    let kind = match template.kind {
                        crate::session::TemplateSelector::Decl => "decl",
                        crate::session::TemplateSelector::Bind => "bind",
                        crate::session::TemplateSelector::BindDiscard => "binddiscard",
                        crate::session::TemplateSelector::Expr => "expr",
                    };
                    (kind.into(), template.source.clone())
                })
                .collect(),
            injected_modules: view.injected_module_names(),
            reserved_declaration_modules: vec![],
        };
        let admission = state
            .admit_planned_cell_for_execution(
                execution.clone(),
                tidepool_toolchain::artifacts::parse_cell_plan(
                    Arc::new(specification.clone()),
                    &(view.include_paths(effects.include_paths())),
                )
                .unwrap(),
                Arc::new(specification.clone()),
                specification.specification_digest(),
                [1; 32],
                view.include_paths(effects.include_paths()),
                None,
            )
            .unwrap();
        let (checked, program) = compile_cell_program_admitted(admission.clone()).unwrap();
        let first = checked.checked_item(0).unwrap();
        let prefix = state
            .begin_cell_program(admission, program)
            .unwrap()
            .unwrap();
        let mut resident = TestSession::from_persistent_for_test(frunk::HNil, EmptyOutput, state);
        resident
            .set_run_context(SessionRunContext {
                lexical_scope: execution.private_scope(),
                ..Default::default()
            })
            .unwrap();
        let reservation = resident
            .admit_checked_item(prefix.clone(), first.clone())
            .unwrap();
        let TurnResult::Bind {
            bound, compiled, ..
        } = consume_cell_program_item(reservation.clone()).unwrap()
        else {
            panic!("expected checked binding");
        };
        let module = tidepool_repr::SessionModule::val(reservation.generation());
        let path = root.path().join(module.relative_hi_path());
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        struct RestorePermissions(PathBuf, std::fs::Permissions);
        impl Drop for RestorePermissions {
            fn drop(&mut self) {
                std::fs::set_permissions(&self.0, self.1.clone()).unwrap();
            }
        }
        let restore = if write_failure {
            if path.exists() {
                std::fs::remove_file(&path).unwrap();
            }
            let parent = path.parent().unwrap().to_path_buf();
            let original = std::fs::metadata(&parent).unwrap().permissions();
            std::fs::set_permissions(&parent, std::fs::Permissions::from_mode(0o500)).unwrap();
            Some(RestorePermissions(parent, original))
        } else {
            std::fs::write(&path, b"conflicting immutable interface").unwrap();
            None
        };
        let before = prefix.snapshot();
        let public_bindings = resident.workbench_bindings_in(public);
        let outcome = resident.run_bind_with_sites(
            "checkedPublication",
            compiled.code(),
            &bound[0],
            reservation.generation(),
        );
        drop(restore);
        let error = outcome.err().expect("interface publication must fail");
        let ResidentError::Session(SessionError::Io(error)) = error else {
            panic!("expected owning I/O refusal, got {error:?}");
        };
        assert_eq!(
            error.kind(),
            if write_failure {
                std::io::ErrorKind::PermissionDenied
            } else {
                std::io::ErrorKind::InvalidData
            }
        );
        assert!(Arc::ptr_eq(&before, &prefix.snapshot()));
        assert!(resident.state.retained_value_interface(module).is_none());
        assert_eq!(resident.workbench_bindings_in(public), public_bindings);
        if write_failure {
            assert!(!path.exists());
        } else {
            assert_eq!(
                std::fs::read(&path).unwrap(),
                b"conflicting immutable interface"
            );
        }
        let id = SessionVarId::from_extract(bound[0].var_id);
        assert!(
            resident.state.bindings().get(id).is_some(),
            "failed publication still has private native custody"
        );
        drop(before);
        drop(compiled);
        drop(checked);
        drop(first);
        drop(reservation);
        drop(prefix);
        drop(execution);
        resident.state.reap_admission_leases();
        assert!(
            resident.state.bindings().get(id).is_none(),
            "last private lease release retires native binding"
        );
    }

    fn startup_session() -> (tempfile::TempDir, TestSession) {
        let root = tempfile::tempdir().unwrap();
        let library =
            SessionLib::open(SessionId(402), root.path(), ModuleEnv::standalone_default()).unwrap();
        let session = TestSession::unbootstrapped(frunk::HNil, EmptyOutput, 1024, Some(library));
        (root, session)
    }

    #[test]
    fn inspection_capture_fences_visible_drift_and_foreign_sessions() {
        let (_root, mut session) = startup_session();
        let view = session.compile_view_in(ScopeId::ROOT).unwrap();
        assert!(session
            .capture_inspection_inputs(&view)
            .unwrap()
            .view()
            .reachable_values()
            .is_empty());
        let foreign_root = tempfile::tempdir().unwrap();
        let foreign_lib = SessionLib::open(
            SessionId(403),
            foreign_root.path(),
            ModuleEnv::standalone_default(),
        )
        .unwrap();
        let foreign =
            TestSession::unbootstrapped(frunk::HNil, EmptyOutput, 1024, Some(foreign_lib));
        assert!(matches!(
            foreign.capture_inspection_inputs(&view),
            Err(SessionError::StaleStagedDeclaration)
        ));
        let sibling = session.mint_isolated_scope();
        let mut value = crate::session::prepared::tests::rooted_publication_fixture(
            &mut session.state,
            "sibling",
            917,
        );
        value.value.identity.module = value.module.module_name();
        value.scope = sibling;
        session.state.bind_in(sibling, value).unwrap();
        assert!(session
            .capture_inspection_inputs(&view)
            .unwrap()
            .view()
            .reachable_values()
            .is_empty());
        let mut value = crate::session::prepared::tests::rooted_publication_fixture(
            &mut session.state,
            "visible",
            918,
        );
        value.value.identity.module = value.module.module_name();
        let hidden_id = value.id;
        session.state.bind(value).unwrap();
        assert!(matches!(
            session.capture_inspection_inputs(&view),
            Err(SessionError::StaleStagedDeclaration)
        ));
        session.hidden_host_bindings.insert(hidden_id, ());
        let normalized = session.compile_view_in(ScopeId::ROOT).unwrap();
        assert!(matches!(
            session.capture_inspection_inputs(&normalized),
            Err(SessionError::MissingRetainedValueInterface(_))
        ));
        let unhidden = session.state.compile_view_in(ScopeId::ROOT).unwrap();
        assert!(matches!(
            session.capture_inspection_inputs(&unhidden),
            Err(SessionError::StaleStagedDeclaration)
        ));
    }

    fn startup_code(fail: bool) -> TurnCode<'static> {
        startup_code_with_value(fail, None)
    }

    fn startup_code_with_value(fail: bool, value: Option<i64>) -> TurnCode<'static> {
        use std::borrow::Cow;
        use tidepool_repr::execution_schema::{
            testing, Atom, CheckedLayout, ConstructorDecl, ConstructorId, ExprFrame, FieldLayout,
            Group, HeapBinding, HeapRhs, ResultContract, RuntimeRep, ScalarLiteral, ValueId,
            ValueRef,
        };
        let mut wire = testing::wire_program();
        wire.signatures[0].results = ResultContract::Returns(vec![RuntimeRep::LiftedRef]);
        let payload = if value.is_some() {
            ("I#", 1)
        } else {
            ("Unit", 0)
        };
        for (index, (name, fields)) in [("Done", 1), ("Suspended", 2), payload]
            .into_iter()
            .enumerate()
        {
            let module = if index < 2 {
                "Tidepool.Internal.Resume"
            } else if value.is_some() {
                "GHC.Types"
            } else {
                "Fixture"
            };
            let rep = if index == 2 && value.is_some() {
                RuntimeRep::Int(64)
            } else {
                RuntimeRep::LiftedRef
            };
            let mut identity = testing::identity(module, name);
            identity.namespace = "constructor".into();
            let mut family = testing::identity(
                module,
                if index < 2 {
                    "Settled"
                } else if value.is_some() {
                    "Int"
                } else {
                    "Unit"
                },
            );
            family.namespace = "type".into();
            wire.constructors.push(ConstructorDecl {
                identity,
                family,
                host_id: DataConId(900 + index as u64),
                result_rep: RuntimeRep::LiftedRef,
                tag: if index == 1 { 2 } else { 1 },
                family_size: if index < 2 { 2 } else { 1 },
                field_reps: vec![rep.clone(); fields],
                strict_fields: vec![index == 2 && value.is_some(); fields],
                layout: CheckedLayout {
                    fields: (0..fields)
                        .map(|field| FieldLayout {
                            rep: rep.clone(),
                            offset: field as u32 * 8,
                        })
                        .collect(),
                    alignment: if fields == 0 { 1 } else { 8 },
                    payload_size: fields as u32 * 8,
                    root_mask: vec![matches!(rep, RuntimeRep::LiftedRef); fields],
                },
            });
        }
        wire.expressions.nodes = if fail {
            vec![ExprFrame::Enter {
                callee: Atom::Rubbish(RuntimeRep::LiftedRef),
                signature: tidepool_repr::execution_schema::SignatureId(0),
            }]
        } else {
            vec![
                ExprFrame::Construct {
                    constructor: ConstructorId(0),
                    fields: vec![Atom::Ref(ValueRef::Local(ValueId(1)))],
                },
                ExprFrame::Let {
                    bindings: Group::NonRecursive(HeapBinding {
                        id: ValueId(1),
                        rhs: HeapRhs::Constructor {
                            constructor: ConstructorId(2),
                            fields: value
                                .into_iter()
                                .map(|value| {
                                    Atom::Scalar(ScalarLiteral::Int {
                                        bits: 64,
                                        bytes: value.to_be_bytes().to_vec(),
                                    })
                                })
                                .collect(),
                        },
                    }),
                    body: 0,
                },
            ]
        };
        let Group::NonRecursive(top) = &mut wire.bindings[0] else {
            unreachable!()
        };
        let HeapRhs::Function { body, .. } = &mut top.binding.rhs else {
            unreachable!()
        };
        *body = if fail { 0 } else { 1 };
        let mut table = DataConTable::new();
        for constructor in &wire.constructors {
            table.insert(tidepool_repr::DataCon {
                id: constructor.host_id,
                name: constructor.identity.occurrence.clone(),
                tag: constructor.tag,
                rep_arity: constructor.field_reps.len() as u32,
                field_bangs: vec![],
                qualified_name: Some(format!(
                    "{}.{}",
                    constructor.identity.module, constructor.identity.occurrence
                )),
                type_name: constructor.family.occurrence.clone(),
            });
        }
        TurnCode {
            prepared: Cow::Owned(testing::prepare(wire).unwrap()),
            table: Cow::Owned(table),
            sites: Cow::Borrowed(&[]),
            certification: Cow::Owned(None),
        }
    }

    fn integer_capsule_session() -> (tempfile::TempDir, TestSession) {
        let (root, mut session) = startup_session();
        let entry = session
            .prepare_startup_entry_installed(
                startup_code_with_value(false, Some(0)),
                StartupCompileIdentity::Fixture,
            )
            .unwrap();
        session.run_startup_entry(entry).unwrap();
        (root, session)
    }

    #[test]
    fn prepared_capsules_keep_same_shape_images_and_original_binding_metadata_together() {
        let (_root, mut session) = integer_capsule_session();
        let binder = BoundBinder {
            name: "originalCapsuleBinding".into(),
            var_id: 1401,
            module: SessionModule::val(Generation(41)).module_name(),
            tier: ValueTier::ForceData,
            type_display: "Int".into(),
            root_head: None,
            host_authority: None,
        };
        let a = session
            .snapshot_run_prepared(
                startup_code_with_value(false, Some(11)),
                PendingPreparedMode::Binding {
                    binder: binder.clone(),
                    generation: Generation(41),
                    observation: None,
                },
                None,
            )
            .unwrap();
        let b = session
            .snapshot_run_prepared(
                startup_code_with_value(false, Some(22)),
                PendingPreparedMode::Value,
                None,
            )
            .unwrap();
        // Both programs are import-free and have the same constructor shape.
        // Compile in the opposite order to installation.
        let b = b.compile_off_checkout().unwrap();
        let a = a.compile_off_checkout().unwrap();
        let Some(ResidentOutcome::Completed { result, .. }) =
            session.revalidate_and_run_prepared(a).unwrap()
        else {
            panic!("original binding capsule must complete");
        };
        assert_eq!(result.to_json(), serde_json::json!(11));
        let bindings = session.workbench_bindings_in(ScopeId::ROOT);
        let original = bindings.iter().find(|row| row.name == binder.name).unwrap();
        assert_eq!(
            session
                .state
                .resolve_in(ScopeId::ROOT, &binder.name)
                .unwrap()
                .id,
            SessionVarId::from_extract(binder.var_id),
        );
        assert_eq!(original.defining_generation(), Some(41));
        assert_eq!(original.type_display.as_deref(), Some("Int"));
        let Some(ResidentOutcome::Completed { result, .. }) =
            session.revalidate_and_run_prepared(b).unwrap()
        else {
            panic!("value capsule must complete");
        };
        assert_eq!(result.to_json(), serde_json::json!(22));
    }

    #[test]
    fn dropping_uninstalled_prepared_capsules_does_not_execute_or_retain_metadata() {
        let (_root, mut session) = integer_capsule_session();
        for compile in [false, true] {
            let pending = session
                .snapshot_run_prepared(
                    startup_code_with_value(true, Some(77)),
                    PendingPreparedMode::Value,
                    None,
                )
                .unwrap();
            let provenance = Arc::downgrade(&pending.metadata.provenance);
            let residency = session.residency();
            let public = session.public_visibility_snapshot_in(ScopeId::ROOT);
            if compile {
                drop(pending.compile_off_checkout().unwrap());
            } else {
                drop(pending);
            }
            assert!(provenance.upgrade().is_none());
            assert_eq!(session.residency(), residency);
            assert_eq!(session.public_visibility_snapshot_in(ScopeId::ROOT), public);
            assert!(session.workbench_bindings_in(ScopeId::ROOT).is_empty());
            assert!(session.parked.is_empty());
        }
    }

    #[test]
    fn startup_requires_compiler_seal_before_native_install() {
        let (_root, mut session) = startup_session();
        assert!(matches!(
            session.prepare_startup_entry(startup_code(false)),
            Err(ResidentError::UnsealedStartupEntry)
        ));
        assert!(!session.prepared_machine_ready());
    }

    #[test]
    fn startup_prepare_defers_authored_failure_until_consumption() {
        let (_root, mut session) = startup_session();
        let entry = session
            .prepare_startup_entry_installed(startup_code(true), StartupCompileIdentity::Fixture)
            .unwrap();
        assert!(
            session.parked.is_empty(),
            "preparation does not run an authored entry"
        );
        assert!(
            session.run_startup_entry(entry).is_err(),
            "the original native entry fails when consumed"
        );
    }

    #[test]
    fn startup_accepts_public_epoch_activation_but_rejects_native_mutation() {
        let (_root, mut session) = startup_session();
        let entry = session
            .prepare_startup_entry_installed(startup_code(false), StartupCompileIdentity::Fixture)
            .unwrap();
        session.state.advance_public_visibility(ScopeId::ROOT);
        session
            .state
            .invalidate_execution_admissions_after_owner_transfer(1);
        session
            .state
            .require_prepared()
            .unwrap()
            .quiesce_and_collect_now()
            .unwrap();
        assert!(matches!(
            session.run_startup_entry(entry),
            Ok(ResidentOutcome::Completed { .. })
        ));
        let entry = session
            .prepare_startup_entry_installed(startup_code(false), StartupCompileIdentity::Fixture)
            .unwrap();
        let binding = crate::session::prepared::tests::rooted_publication_fixture(
            &mut session.state,
            "new",
            401,
        );
        session.state.bind(binding).unwrap();
        assert!(matches!(
            session.run_startup_entry(entry),
            Err(ResidentError::StaleStartupEntry)
        ));
    }

    #[test]
    fn startup_accepts_authenticated_nonzero_successor_transfer_without_native_remint() {
        use crate::session::{
            CertifiedDeclarationPublication, PersistentSession, PublicManifestCommit,
            PublicationDecision, RecoveryPublicOwner, RecoveryRunAuthority,
            RecoverySuccessorAuthority,
        };

        struct RunOwner {
            root: std::path::PathBuf,
            _lock: std::fs::File,
        }
        impl RecoveryRunAuthority for RunOwner {
            fn owns_run(&self, root: &std::path::Path) -> std::io::Result<bool> {
                Ok(root.canonicalize()? == self.root)
            }
        }
        struct Successor {
            run: Arc<RunOwner>,
            predecessor: RecoveryPublicOwner,
            successor: RecoveryPublicOwner,
            session: SessionId,
            scope: ScopeId,
        }
        impl RecoverySuccessorAuthority for Successor {
            fn validate_successor(
                &self,
                root: &std::path::Path,
                predecessor: &RecoveryPublicOwner,
                successor: &RecoveryPublicOwner,
                session: SessionId,
                scope: ScopeId,
            ) -> std::io::Result<bool> {
                Ok(self.run.owns_run(root)?
                    && predecessor == &self.predecessor
                    && successor == &self.successor
                    && session == self.session
                    && scope == self.scope)
            }
        }

        tidepool_testing::eval_harness::require_extract();
        let durable = tempfile::tempdir().unwrap();
        let producer_source = tempfile::tempdir().unwrap();
        let manifest = durable.path().join("declarations.json");
        let lock = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(durable.path().join("run-owner.lock"))
            .unwrap();
        lock.try_lock().unwrap();
        let run = Arc::new(RunOwner {
            root: durable.path().canonicalize().unwrap(),
            _lock: lock,
        });
        let owner = |incarnation| {
            RecoveryPublicOwner::new(
                &tidepool_repr::ActorPath::parse("root/startup-transfer").unwrap(),
                incarnation,
            )
            .unwrap()
        };
        let library = |id, source: &std::path::Path| {
            let mut lib = SessionLib::open(id, source, ModuleEnv::standalone_default())
                .unwrap()
                .with_validation_include(vec![tidepool_testing::eval_harness::prelude_path()]);
            lib.attach_owned_recovery_graph_v3(&manifest, run.clone())
                .unwrap();
            lib
        };
        let mut producer = PersistentSession::new(
            Some(library(SessionId(4501), producer_source.path())),
            1024 * 1024,
        );
        let public = producer.mint_isolated_scope();
        producer
            .initialize_durable_public_scope(owner(1), public)
            .unwrap();
        let admission = producer.begin_private_execution(public).unwrap();
        producer
            .define_scoped_in(
                admission.private_scope(),
                &[include_str!("fixtures/recovery-original.hs")],
            )
            .unwrap();
        let intent = producer
            .freeze_execution_intent(&admission, vec![], vec![])
            .unwrap();
        let CertifiedDeclarationPublication::Accepted(accepted) = producer
            .restage_declaration_publication(owner(1), intent)
            .unwrap()
            .certify()
            .unwrap()
        else {
            panic!("original declarations must publish");
        };
        assert_eq!(
            producer
                .publish_staged_public_manifest(
                    accepted.stage().unwrap(),
                    &PublicationDecision::new()
                )
                .unwrap(),
            PublicManifestCommit::Durable
        );
        let published = producer.public_visibility_snapshot_in(public).unwrap();
        assert_ne!(published.declaration_tip, Generation(0));
        let predecessor_bytes = std::fs::read(&manifest).unwrap();
        drop(producer);
        drop(producer_source);

        for uncertain in [false, true] {
            std::fs::write(&manifest, &predecessor_bytes).unwrap();
            let source = tempfile::tempdir().unwrap();
            let mut session = TestSession::unbootstrapped(
                frunk::HNil,
                EmptyOutput,
                1024 * 1024,
                Some(library(SessionId(4502), source.path())),
            );
            let scope = session.mint_isolated_scope();
            session
                .set_run_context(SessionRunContext {
                    lexical_scope: scope,
                    ..SessionRunContext::ROOT
                })
                .unwrap();
            let (producer, _) = crate::session::prepared::tests::install_source_publication_fixture(
                &mut session.state,
                scope,
            );
            let entry = session
                .prepare_startup_entry_installed(
                    startup_code(false),
                    StartupCompileIdentity::Fixture,
                )
                .unwrap();
            let program = entry.program.unwrap();
            let original = session.public_visibility_snapshot_in(scope).unwrap();
            assert_eq!(original.declaration_tip, Generation(0));
            assert!(!original.source_instances.is_empty());
            session.seal_recovery_initialization_scope(scope).unwrap();
            let compiled = session.state.prepared().unwrap().codegen_totals();
            session.state.lib_mut().fail_recovery_durability_once = uncertain;
            let commit = session
                .transfer_recovered_public_owner(
                    &owner(1),
                    owner(2),
                    scope,
                    Arc::new(Successor {
                        run: run.clone(),
                        predecessor: owner(1),
                        successor: owner(2),
                        session: SessionId(4502),
                        scope,
                    }),
                )
                .unwrap();
            if uncertain {
                assert!(matches!(
                    commit,
                    PublicManifestCommit::PublishedDurabilityUnconfirmed { .. }
                ));
                session
                    .confirm_durable_public_scope(&owner(2), scope)
                    .unwrap();
            } else {
                assert_eq!(commit, PublicManifestCommit::Durable);
            }
            let transferred = session.public_visibility_snapshot_in(scope).unwrap();
            assert_eq!(transferred.declaration_tip, published.declaration_tip);
            assert_eq!(transferred.epoch, published.epoch + 1);
            assert_eq!(transferred.scope, original.scope);
            assert_eq!(
                transferred.machine_incarnation,
                original.machine_incarnation
            );
            assert_eq!(transferred.bindings, original.bindings);
            assert_eq!(transferred.source_instances, original.source_instances);
            assert_eq!(transferred.source_selection, original.source_selection);
            assert_eq!(session.state.prepared().unwrap().codegen_totals(), compiled);
            assert_eq!(entry.program, Some(program));
            let committed_bytes = std::fs::read(&manifest).unwrap();
            session
                .state
                .require_prepared()
                .unwrap()
                .quiesce_and_collect_now()
                .unwrap();
            assert!(matches!(
                session.run_startup_entry(entry),
                Ok(ResidentOutcome::Completed { .. })
            ));
            assert_eq!(session.outstanding_custody(), 0);
            assert_eq!(std::fs::read(&manifest).unwrap(), committed_bytes);
            assert!(!session.state.require_prepared().unwrap().unpin(program));
            session.state.require_prepared().unwrap().unpin(producer);
        }
    }

    #[test]
    fn startup_rejects_changed_original_source_inventory() {
        let (_root, mut session) = startup_session();
        let (producer, keys) = crate::session::prepared::tests::install_source_publication_fixture(
            &mut session.state,
            ScopeId::ROOT,
        );
        let entry = session
            .prepare_startup_entry_installed(startup_code(false), StartupCompileIdentity::Fixture)
            .unwrap();
        assert!(session
            .state
            .retire_failed_turn_source_instances(ScopeId::ROOT, &keys));
        assert!(matches!(
            session.run_startup_entry(entry),
            Err(ResidentError::StaleStartupEntry)
        ));
        session.state.require_prepared().unwrap().unpin(producer);
    }

    #[test]
    fn startup_rejects_foreign_machine_scope_and_realm() {
        let (_root, mut session) = startup_session();
        let (_foreign_root, mut foreign) = startup_session();
        let entry = session
            .prepare_startup_entry_installed(startup_code(false), StartupCompileIdentity::Fixture)
            .unwrap();
        assert!(matches!(
            foreign.run_startup_entry(entry),
            Err(ResidentError::ForeignCustody)
        ));
        session.settle_dropped_custody();
        let entry = session
            .prepare_startup_entry_installed(startup_code(false), StartupCompileIdentity::Fixture)
            .unwrap();
        session
            .set_run_context(SessionRunContext {
                resource_scope: RealmId::fresh(),
                ..SessionRunContext::ROOT
            })
            .unwrap();
        assert!(matches!(
            session.run_startup_entry(entry),
            Err(ResidentError::StaleStartupEntry)
        ));
        session.set_run_context(SessionRunContext::ROOT).unwrap();
        session.settle_dropped_custody();
        let entry = session
            .prepare_startup_entry_installed(startup_code(false), StartupCompileIdentity::Fixture)
            .unwrap();
        let scope = session.mint_isolated_scope();
        session
            .set_run_context(SessionRunContext {
                lexical_scope: scope,
                ..SessionRunContext::ROOT
            })
            .unwrap();
        assert!(matches!(
            session.run_startup_entry(entry),
            Err(ResidentError::StaleStartupEntry)
        ));
    }

    #[test]
    fn abandoned_startup_releases_original_install_pin() {
        let (_root, mut session) = startup_session();
        let entry = session
            .prepare_startup_entry_installed(startup_code(false), StartupCompileIdentity::Fixture)
            .unwrap();
        assert_eq!(session.outstanding_custody(), 1);
        let program = entry.program.unwrap();
        drop(entry);
        assert_eq!(session.settle_dropped_custody(), 1);
        assert!(
            !session.state.require_prepared().unwrap().unpin(program),
            "the queued capsule already released its pin"
        );
    }

    #[test]
    fn startup_clone_unwind_releases_only_its_original_install_and_source_admission() {
        struct PanicCloneOutput;
        impl Clone for PanicCloneOutput {
            fn clone(&self) -> Self {
                panic!("startup output clone panic");
            }
        }
        impl OutputSink for PanicCloneOutput {
            fn drain(&self) -> Vec<String> {
                Vec::new()
            }
            fn snapshot(&self) -> Vec<String> {
                Vec::new()
            }
        }

        let root = tempfile::tempdir().unwrap();
        let lib =
            SessionLib::open(SessionId(403), root.path(), ModuleEnv::standalone_default()).unwrap();
        let mut session =
            ResidentSession::unbootstrapped(frunk::HNil, PanicCloneOutput, 1024 * 1024, Some(lib));
        let (prior, _) = crate::session::prepared::tests::install_source_publication_fixture(
            &mut session.state,
            ScopeId::ROOT,
        );
        let before = session
            .public_visibility_snapshot_in(ScopeId::ROOT)
            .unwrap();
        let (program, source_keys) =
            crate::session::prepared::tests::install_selected_source_fixture(
                &mut session.state,
                ScopeId::ROOT,
                "late",
            );
        assert_ne!(program, prior);
        assert!(!source_keys.is_empty());
        let installed_keys = source_keys.to_vec();
        // The native fixture issuer returns this exact install and admission
        // together; the test exercises capsule custody without compiler claims.
        let entry = PreparedStartupEntry {
            program: Some(program),
            source_keys,
            provenance: Arc::default(),
            admitted: session
                .public_visibility_snapshot_in(ScopeId::ROOT)
                .unwrap(),
            realm: RealmId::ROOT,
            cleanup: Arc::clone(&session.custody_cleanup),
            _lease: session.lease_bindings(&[]),
            compile_identity: StartupCompileIdentity::Fixture,
        };
        let unwind = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            session.run_startup_entry(entry)
        }))
        .expect_err("output Clone panics before native execution");
        assert_eq!(
            unwind.downcast_ref::<&str>().copied(),
            Some("startup output clone panic")
        );
        assert!(session.prepared_machine_ready());
        {
            let abandoned = session.custody_cleanup.startup_entries.lock();
            assert_eq!(abandoned.len(), 1);
            assert_eq!(abandoned[0].program, program);
            assert_eq!(abandoned[0].scope, ScopeId::ROOT);
            assert_eq!(abandoned[0].source_keys.to_vec(), installed_keys);
        }
        assert_eq!(session.settle_dropped_custody(), 1);
        assert_eq!(session.outstanding_custody(), 0);
        let after = session
            .public_visibility_snapshot_in(ScopeId::ROOT)
            .unwrap();
        assert_eq!(after.source_instances, before.source_instances);
        assert_eq!(after.source_selection, before.source_selection);
        assert!(session.parked_holes().is_empty());
        let engine = session.state.require_prepared().unwrap();
        assert!(
            !engine.unpin(program),
            "capsule cleanup released its install pin"
        );
        assert!(
            engine.unpin(prior),
            "the independent install retains its pin"
        );
    }

    #[test]
    fn invocation_cancel_scope_survives_bootstrap_and_restores_after_nested_panic() {
        let mut session = TestSession::unbootstrapped(frunk::HNil, EmptyOutput, 1024, None);
        let cancelled = Arc::new(AtomicBool::new(true));
        session.with_invocation_cancel(Arc::clone(&cancelled), |session| {
            session
                .state
                .install_prepared(
                    tidepool_repr::execution_schema::testing::prepare(
                        tidepool_repr::execution_schema::testing::wire_program(),
                    )
                    .unwrap(),
                )
                .unwrap();
            assert!(session
                .state
                .require_prepared()
                .unwrap()
                .cancellation_requested(RealmId::ROOT));
            let panic = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                session.with_invocation_cancel(Arc::new(AtomicBool::new(false)), |session| {
                    assert!(!session
                        .state
                        .require_prepared()
                        .unwrap()
                        .cancellation_requested(RealmId::ROOT));
                    panic!("nested invocation unwinds");
                });
            }));
            assert!(panic.is_err());
            assert!(session
                .state
                .require_prepared()
                .unwrap()
                .cancellation_requested(RealmId::ROOT));
        });
        assert!(!session
            .state
            .require_prepared()
            .unwrap()
            .cancellation_requested(RealmId::ROOT));
        assert!(cancelled.load(std::sync::atomic::Ordering::Relaxed));
    }

    fn parcel_source_fixture() -> (
        TestSession,
        ResidentParcel,
        PreparedHandle,
        Vec<SymbolIdentity>,
    ) {
        parcel_source_fixture_in_unit(HOME_UNIT)
    }

    fn parcel_source_fixture_in_unit(
        unit: &str,
    ) -> (
        TestSession,
        ResidentParcel,
        PreparedHandle,
        Vec<SymbolIdentity>,
    ) {
        use tidepool_repr::execution_schema::{
            testing, Atom, ExprFrame, GlobalDecl, GlobalId, Group, HeapRhs, ResultContract,
            RuntimeRep, Signature, SignatureId, UpdatePolicy, ValueRef,
        };
        let mut source = TestSession::unbootstrapped(frunk::HNil, EmptyOutput, 1024, None);
        let mut identities = Vec::new();
        for (generation, name) in [(481, "first"), (482, "second")] {
            let mut entry = crate::session::prepared::tests::rooted_publication_fixture(
                &mut source.state,
                name,
                generation,
            );
            entry.module = SessionModule::lib(Generation(generation));
            entry.value.identity = testing::identity(&entry.module.module_name(), name);
            entry.value.identity.unit = unit.into();
            entry.id = SessionVarId::from_extract(session_var_id(
                &entry.value.identity.module,
                &entry.value.identity.occurrence,
            ));
            identities.push(entry.value.identity.clone());
            source.state.bind(entry).unwrap();
        }
        let mut wire = testing::wire_program();
        wire.signatures[0] = Signature {
            arguments: Vec::new(),
            results: ResultContract::Returns(vec![RuntimeRep::LiftedRef]),
        };
        wire.globals = identities
            .iter()
            .zip([481, 482])
            .map(|(identity, generation)| GlobalDecl {
                identity: identity.clone(),
                rep: RuntimeRep::LiftedRef,
                entry_signature: None,
                required_evaluated: false,
                required_generation: Some(generation),
            })
            .collect();
        wire.expressions.nodes[0] =
            ExprFrame::Return(vec![Atom::Ref(ValueRef::Global(GlobalId(0)))]);
        let Group::NonRecursive(top) = &mut wire.bindings[0] else {
            unreachable!()
        };
        top.identity = testing::identity("Parcel.Consumer", "payload");
        top.identity.unit = "main".into();
        top.binding.rhs = HeapRhs::Thunk {
            signature: SignatureId(0),
            update: UpdatePolicy::Memoize,
            captures: Vec::new(),
            body: 0,
        };
        let entry = crate::session::prepared::tests::rooted_program_fixture(
            &mut source.state,
            "payload",
            483,
            testing::prepare(wire).unwrap(),
        );
        let handle = entry.value.handle;
        source.state.bind(entry).unwrap();
        let custody = source.retain_binding_custody("payload").unwrap().unwrap();
        let parcel = source.export_shared(&custody).unwrap();
        (source, parcel, handle, identities)
    }

    #[test]
    fn parcel_foreign_unit_library_spelling_keeps_native_identity_without_home_publication() {
        let (_source, parcel, _payload, identities) =
            parcel_source_fixture_in_unit("parcel-package");
        let mut destination = TestSession::unbootstrapped(frunk::HNil, EmptyOutput, 1024, None);
        let mut home = crate::session::prepared::tests::rooted_publication_fixture(
            &mut destination.state,
            "first",
            481,
        );
        home.module = SessionModule::lib(Generation(481));
        home.value.identity = identities[0].clone();
        home.value.identity.unit = HOME_UNIT.into();
        home.id = SessionVarId::from_extract(session_var_id(
            &home.value.identity.module,
            &home.value.identity.occurrence,
        ));
        let home_id = home.id;
        let home_identity = home.value.identity.clone();
        let home_handle = home.value.handle;
        destination.state.bind(home).unwrap();
        assert_eq!(
            destination
                .state
                .prepared()
                .unwrap()
                .pending_parcel_import_identities(&parcel.native),
            identities
        );
        let revision = destination.state.bindings().mutation_revision();
        let modules = destination.state.live_val_modules();
        let provenance = Arc::clone(&parcel.provenance);
        // The package and home symbols have the same module/occurrence and
        // therefore the same legacy VarId, but only the home unit owns a row.
        let custody = destination.import_parcel(parcel, RealmId(481)).unwrap();
        assert_eq!(destination.state.bindings().mutation_revision(), revision);
        assert_eq!(destination.state.live_val_modules(), modules);
        let home = destination.state.bindings().get(home_id).unwrap();
        assert_eq!(home.value.identity, home_identity);
        assert_eq!(home.value.handle, home_handle);
        assert!(destination
            .state
            .bindings()
            .get(SessionVarId::from_extract(session_var_id(
                &identities[1].module,
                &identities[1].occurrence,
            )))
            .is_none());
        assert!(destination.binding_provenance.is_empty());
        let reexported = destination.export_shared(&custody).unwrap();
        let carried = reexported
            .native
            .images()
            .iter()
            .flat_map(|image| image.imports.iter().map(|(identity, _)| identity.clone()))
            .collect::<std::collections::BTreeSet<_>>();
        assert_eq!(carried, identities.into_iter().collect());
        assert!(Arc::ptr_eq(&reexported.provenance, &provenance));
        assert!(destination.discard_custody(custody));
    }

    #[test]
    fn parcel_binding_conflict_refuses_before_native_import_and_partial_visibility() {
        let (_source, parcel, _payload, identities) = parcel_source_fixture();
        let mut destination = TestSession::unbootstrapped(frunk::HNil, EmptyOutput, 1024, None);
        let mut existing = crate::session::prepared::tests::rooted_publication_fixture(
            &mut destination.state,
            "occupied",
            484,
        );
        let id = parcel_library_binding(&identities[1]).unwrap().id;
        existing.id = id;
        let handle = existing.value.handle;
        destination.state.bind(existing).unwrap();
        assert_eq!(
            destination
                .state
                .prepared()
                .unwrap()
                .pending_parcel_import_identities(&parcel.native),
            identities
        );
        let residency = destination.residency();
        let revision = destination.state.bindings().mutation_revision();
        let modules = destination.state.live_val_modules();
        assert!(matches!(destination.import_parcel(parcel, RealmId(481)),
            Err(ResidentError::Session(SessionError::InvalidBindingIdentity(error))) if error.id == id));
        assert_eq!(destination.residency(), residency);
        assert_eq!(destination.state.bindings().mutation_revision(), revision);
        assert_eq!(destination.state.live_val_modules(), modules);
        assert!(destination
            .state
            .bindings()
            .get(parcel_library_binding(&identities[0]).unwrap().id)
            .is_none());
        assert_eq!(
            destination
                .state
                .prepared()
                .unwrap()
                .prepared_handle_of(handle.raw()),
            Some(handle)
        );
    }

    #[test]
    fn parcel_imported_binding_handles_survive_caller_realm_and_repeat_import() {
        let (mut source, parcel, _payload, identities) = parcel_source_fixture();
        let mut destination = TestSession::unbootstrapped(frunk::HNil, EmptyOutput, 1024, None);
        let bootstrap = crate::session::prepared::tests::rooted_publication_fixture(
            &mut destination.state,
            "bootstrap",
            485,
        );
        destination.state.bind(bootstrap).unwrap();
        let owner = RealmId(481);
        let provenance = Arc::clone(&parcel.provenance);
        let custody = destination.import_parcel(parcel, owner).unwrap();
        let revision = destination.state.bindings().mutation_revision();
        let source_custody = source.retain_binding_custody("payload").unwrap().unwrap();
        let repeated = source.export_shared(&source_custody).unwrap();
        assert!(destination
            .state
            .prepared()
            .unwrap()
            .pending_parcel_import_identities(&repeated.native)
            .is_empty());
        let repeated_custody = destination.import_parcel(repeated, owner).unwrap();
        assert_eq!(destination.state.bindings().mutation_revision(), revision);
        destination.close_realm(owner);
        for identity in &identities {
            let entry = destination
                .state
                .bindings()
                .get(parcel_library_binding(identity).unwrap().id)
                .unwrap();
            let handle = entry.value.handle;
            let imported_id = entry.id;
            let binding = destination
                .retain_binding_custody_in(ScopeId::ROOT, &identity.occurrence, imported_id)
                .unwrap()
                .unwrap();
            assert!(
                Arc::ptr_eq(&binding.provenance, &provenance),
                "imported binding capture retains the original carrier"
            );
            let exported = destination.export_shared(&binding).unwrap();
            let mut third = TestSession::unbootstrapped(frunk::HNil, EmptyOutput, 1024, None);
            let bootstrap = crate::session::prepared::tests::rooted_publication_fixture(
                &mut third.state,
                "bootstrap",
                486,
            );
            third.state.bind(bootstrap).unwrap();
            let arrived = third.import_parcel(exported, RealmId::ROOT).unwrap();
            assert!(
                Arc::ptr_eq(&arrived.provenance, &provenance),
                "binding re-export to a third machine cannot replace its provenance"
            );
            assert!(third.discard_custody(arrived));
            assert_eq!(
                &destination
                    .state
                    .bindings()
                    .get(imported_id)
                    .unwrap()
                    .value
                    .identity,
                identity
            );
            assert_eq!(
                destination
                    .state
                    .prepared()
                    .unwrap()
                    .prepared_handle_of(handle.raw()),
                Some(handle)
            );
            // Trace the still-live copied closure after realm collection.
            destination
                .state
                .prepared_mut()
                .unwrap()
                .export_parcel(handle.raw())
                .unwrap();
        }
        drop(custody);
        drop(repeated_custody);
        destination.settle_dropped_custody();
        for identity in identities {
            assert!(destination
                .state
                .bindings()
                .get(parcel_library_binding(&identity).unwrap().id)
                .is_some());
        }
    }

    #[test]
    fn certified_authored_capture_freeze_join_preserves_original_domains() {
        use crate::session::{
            CertifiedDeclarationPublication, ExecutionPublication, SourceImports,
        };
        use tidepool_codegen::prepared_program::{
            PendingGroupInventory, SourceBinder, SourceGroupOutline,
        };
        use tidepool_toolchain::certified_products::{
            certify_inherited_products, InheritedProductInput, PendingImportOwner,
        };
        use tidepool_toolchain::recovery_artifacts::{
            materialize_certified_products, verify_materialized_ref,
        };

        tidepool_testing::eval_harness::require_extract();
        let root = tempfile::tempdir().unwrap();
        let mut lib = SessionLib::open(
            SessionId(4487),
            root.path(),
            ModuleEnv::standalone_default(),
        )
        .unwrap()
        .with_validation_include(vec![tidepool_testing::eval_harness::prelude_path()]);
        lib.attach_recovery_graph_v2(root.path().join("declarations.json"))
            .unwrap();
        let mut session =
            TestSession::unbootstrapped(frunk::HNil, EmptyOutput, 1024 * 1024, Some(lib));
        let public = session.state.mint_scope(ScopeId::ROOT).unwrap();
        let owner = super::super::RecoveryPublicOwner::new(
            &tidepool_repr::ActorPath::parse("root/certified-domain").unwrap(),
            1,
        )
        .unwrap();
        session
            .bind_durable_public_scope(owner.clone(), public)
            .unwrap();
        let d = session.begin_private_execution(public).unwrap();
        let d_generation = session
            .define_scoped_with_imports_in(
                d.private_scope(),
                &["d :: Int -> Int\nd x = x + 41\n{-# NOINLINE d #-}"],
                &SourceImports::new(),
            )
            .unwrap();
        let original_d = session
            .state
            .lib()
            .log
            .certified_authored_arc_at(d_generation)
            .unwrap();
        let d_owner = original_d.product().owner().clone();
        let intent = session.freeze_private_execution(&d).unwrap();
        let ExecutionPublication::Declarations(base) = session
            .restage_execution_publication(owner.clone(), intent)
            .unwrap()
        else {
            panic!("certified D must produce a declaration publication");
        };
        let CertifiedDeclarationPublication::Accepted(joined) = base.certify().unwrap() else {
            panic!("certified D join rejected");
        };
        assert_eq!(
            session
                .publish_staged_public_manifest(
                    joined.stage().unwrap(),
                    &super::super::PublicationDecision::new()
                )
                .unwrap(),
            super::super::PublicManifestCommit::Durable
        );
        let public_before = session
            .state
            .bindings()
            .source_domain_selection_in(session.state.scope_tree(), public)
            .unwrap();
        let public_d = public_before
            .domain_for_owner(public_before.current(), &d_owner)
            .unwrap();
        let e = session.begin_private_execution(public).unwrap();
        let captured = session
            .state
            .bindings()
            .source_domain_selection_in(session.state.scope_tree(), e.private_scope())
            .unwrap();
        assert!(
            captured.inherited().is_empty(),
            "D has no native instance at capture"
        );
        assert!(session.state.prepared().is_none());
        assert_ne!(
            captured
                .domain_for_owner(captured.current(), &d_owner)
                .unwrap(),
            public_d
        );
        let e_generation = session
            .define_scoped_with_imports_in(
                e.private_scope(),
                &["e :: Int -> Int\ne x = d x + 1\n{-# NOINLINE e #-}"],
                &SourceImports::new(),
            )
            .unwrap();
        let original_e = session
            .state
            .lib()
            .log
            .certified_authored_arc_at(e_generation)
            .unwrap();
        let e_owner = original_e.product().owner().clone();
        let captured_d = original_e
            .recovery_products()
            .into_iter()
            .find(|p| p.owner() == &d_owner)
            .unwrap();
        assert_eq!(
            captured_d,
            *original_d.product(),
            "capture preserves full immutable original D"
        );
        let intent = session.freeze_private_execution(&e).unwrap();
        let ExecutionPublication::Declarations(base) = session
            .restage_execution_publication(owner, intent)
            .unwrap()
        else {
            panic!("certified E must produce a declaration publication");
        };
        let CertifiedDeclarationPublication::Accepted(joined) = base.certify().unwrap() else {
            panic!("certified E join rejected");
        };
        assert_eq!(
            session
                .publish_staged_public_manifest(
                    joined.stage().unwrap(),
                    &super::super::PublicationDecision::new()
                )
                .unwrap(),
            super::super::PublicManifestCommit::Durable
        );
        session.retire_scope(d.private_scope());
        session.retire_scope(e.private_scope());
        let selection = session
            .state
            .bindings()
            .source_domain_selection_in(session.state.scope_tree(), public)
            .unwrap();
        assert_eq!(
            selection
                .domain_for_owner(selection.current(), &d_owner)
                .unwrap(),
            public_d
        );
        let public_e = selection
            .domain_for_owner(selection.current(), &e_owner)
            .unwrap();
        let nested_d = selection.domain_for_owner(public_e, &d_owner).unwrap();
        assert_ne!(
            nested_d, public_d,
            "E retains its captured D domain after both origins retire"
        );
        assert!(selection.inherited().is_empty());

        // Re-admit the actual durable original products, then demand both
        // public heads through the same domain planner used by the installer.
        let view = session.state.compile_view_in(public).unwrap();
        let context = view.exact_declaration_context().unwrap();
        let products = context.recovery_products();
        assert!(products.iter().any(|p| p == original_d.product()));
        assert!(products.iter().any(|p| p == original_e.product()));
        let scratch = tempfile::tempdir().unwrap();
        let references = materialize_certified_products(
            scratch.path(),
            context.toolchain_identity_sha256(),
            &products,
        )
        .unwrap();
        let artifacts = references
            .iter()
            .map(|r| verify_materialized_ref(scratch.path(), r).unwrap())
            .collect::<Vec<_>>();
        let inputs = artifacts
            .iter()
            .map(|artifact| InheritedProductInput { artifact })
            .collect::<Vec<_>>();
        let groups = certify_inherited_products(&inputs, &[]).unwrap();
        let binder = |owner: &tidepool_repr::execution_schema::CachedHomeOwner,
                      occurrence: &str| {
            let identity = groups
                .iter()
                .filter(|g| g.owner() == owner)
                .flat_map(|g| g.group().binders())
                .find(|b| b.occurrence == occurrence)
                .unwrap()
                .clone();
            SourceBinder {
                version: owner.module_version.clone(),
                binder: identity,
            }
        };
        let d_root = binder(&d_owner, "d");
        let e_root = binder(&e_owner, "e");
        let outlines = groups
            .iter()
            .map(|g| {
                let imports = g
                    .imports()
                    .iter()
                    .filter_map(|i| match i {
                        PendingImportOwner::Source { owner, binder, .. } => Some(SourceBinder {
                            version: owner.module_version.clone(),
                            binder: binder.clone(),
                        }),
                        _ => None,
                    })
                    .collect();
                SourceGroupOutline::from_projected(g.owner().clone(), g.group(), imports).unwrap()
            })
            .collect();
        let (demanded, inherited, roots) = PendingGroupInventory::new(outlines)
            .unwrap()
            .seal_in_domains([d_root.clone(), e_root.clone()], &selection)
            .unwrap()
            .into_parts();
        assert!(inherited.is_empty());
        assert_eq!(roots[&d_root].domain, public_d);
        assert_eq!(roots[&e_root].domain, public_e);
        let d_demands = demanded
            .iter()
            .filter(|g| g.owner() == &d_owner)
            .collect::<Vec<_>>();
        assert!(d_demands.iter().any(|g| g.domain() == public_d));
        assert!(
            d_demands.iter().any(|g| g.domain() == nested_d),
            "late E demand traverses its captured D, not ambient public D; demanded={:?}; E imports={:?}",
            demanded.iter().map(|g| (&g.owner().module, g.original_ordinal(), g.domain())).collect::<Vec<_>>(),
            groups.iter().filter(|g| g.owner() == &e_owner).map(|g| (g.group().binders(), g.imports())).collect::<Vec<_>>()
        );
        session.retire_scope(public);
        assert_eq!(session.persistent_roots_count(), 0);
        assert!(session.residency().is_none());
    }

    #[test]
    fn native_binding_identity_conflicts_refuse_before_install_and_snapshot() {
        use std::borrow::Cow;
        use tidepool_repr::execution_schema::testing;

        let root = tempfile::tempdir().unwrap();
        let lib =
            SessionLib::open(SessionId(407), root.path(), ModuleEnv::standalone_default()).unwrap();
        let mut session = TestSession::unbootstrapped(frunk::HNil, EmptyOutput, 1024, Some(lib));
        let entry = crate::session::prepared::tests::rooted_publication_fixture(
            &mut session.state,
            "existing",
            407,
        );
        let id = entry.id;
        session.state.bind(entry).unwrap();
        let before = session
            .state
            .public_visibility_snapshot_in(ScopeId::ROOT)
            .unwrap();
        let residency = session.residency();
        let roots = session.state.persistent_roots_count();
        let table = session.state.session_table().clone();
        let prepared = testing::prepare(testing::wire_program()).unwrap();
        let code = TurnCode {
            prepared: Cow::Owned(prepared),
            table: Cow::Owned(table.clone()),
            sites: Cow::Borrowed(&[]),
            certification: Cow::Owned(None),
        };
        let occupied = BoundBinder {
            name: "replacement".into(),
            var_id: id.raw(),
            module: SessionModule::val(Generation(408)).module_name(),
            tier: ValueTier::RetainOpaque,
            type_display: "Int".into(),
            root_head: None,
            host_authority: None,
        };
        let fresh = BoundBinder {
            name: "fresh".into(),
            var_id: 408,
            ..occupied.clone()
        };
        assert!(matches!(
            session.run_projected_bind_with_sites("projected", code.clone(), &[fresh, occupied.clone()], Generation(408)),
            Err(ResidentError::Session(SessionError::InvalidBindingIdentity(error))) if error.id == id
        ));
        assert!(matches!(
            session.run_bind_with_sites("replacement", code.clone(), &occupied, Generation(408)),
            Err(ResidentError::Session(SessionError::InvalidBindingIdentity(error))) if error.id == id
        ));
        assert!(matches!(
            session.snapshot_run_prepared(code, PendingPreparedMode::Binding {
                binder: occupied, generation: Generation(408), observation: None,
            }, None),
            Err(ResidentError::Session(SessionError::InvalidBindingIdentity(error))) if error.id == id
        ));
        assert_eq!(session.residency(), residency);
        assert_eq!(session.state.persistent_roots_count(), roots);
        assert_eq!(session.state.session_table(), &table);
        assert_eq!(
            session
                .state
                .public_visibility_snapshot_in(ScopeId::ROOT)
                .unwrap(),
            before
        );
    }

    #[test]
    fn unsupported_checked_routes_refuse_before_native_install_and_prefix_settlement() {
        use crate::session::turn::{
            compile_cell_program_admitted, consume_cell_program_item, TemplateSelector,
        };
        use crate::session::{
            resident_cell_check_template, resident_workbench_templates, TurnResult,
        };
        use tidepool_testing::effect_surface::TestEffectSurface;
        use tidepool_toolchain::checked_cell::CheckedCellSpecification;

        tidepool_testing::eval_harness::require_extract();
        let root = tempfile::tempdir().unwrap();
        let effects = TestEffectSurface::minimal(&[]).unwrap();
        let mut lib =
            SessionLib::open(SessionId(993), root.path(), ModuleEnv::standalone_default())
                .unwrap()
                .with_validation_include(effects.include_paths().to_vec());
        lib.attach_recovery_graph_v2(root.path().join("declarations.json"))
            .unwrap();
        let mut session = TestSession::unbootstrapped(frunk::HNil, EmptyOutput, 1024, Some(lib));
        session
            .define_scoped_with_imports_in(
                ScopeId::ROOT,
                &["seed :: Int\nseed = 1"],
                &super::super::SourceImports::new(),
            )
            .unwrap();
        let private = Arc::new(session.begin_private_execution(ScopeId::ROOT).unwrap());
        let scope = private.private_scope();
        session.run_context.lexical_scope = scope;
        let view = session.state.compile_view_in(scope).unwrap();
        let imports = view.turn_imports(&super::super::SourceImports::new());
        let template = resident_cell_check_template(effects.preamble(), effects.row(), &imports);
        let templates = resident_workbench_templates(effects.preamble(), effects.row(), &imports);
        let source = "let page = (42 :: Int)";
        let injected = view.injected_module_names();
        let specification = CheckedCellSpecification {
            admission_digest: [0; 32],
            cell_source: source.into(),
            template_source: template.clone(),
            turn_templates: templates
                .iter()
                .map(|template| {
                    let kind = match template.kind {
                        TemplateSelector::Decl => "decl",
                        TemplateSelector::Bind => "bind",
                        TemplateSelector::BindDiscard => "binddiscard",
                        TemplateSelector::Expr => "expr",
                    };
                    (kind.into(), template.source.clone())
                })
                .collect(),
            injected_modules: injected.clone(),
            reserved_declaration_modules: Vec::new(),
        };
        let includes = view.include_paths(effects.include_paths());
        let admission = session
            .admit_planned_cell_for_execution(
                private,
                tidepool_toolchain::artifacts::parse_cell_plan(
                    Arc::new(specification.clone()),
                    &(includes.clone()),
                )
                .unwrap(),
                Arc::new(specification.clone()),
                specification.specification_digest(),
                [0; 32],
                includes.clone(),
                None,
            )
            .unwrap();
        let (checked, program) = compile_cell_program_admitted(admission.clone()).unwrap();
        let item = checked.checked_item(0).unwrap();
        let prefix = session
            .begin_cell_program(admission, program)
            .unwrap()
            .unwrap();
        let item_admission = session.admit_checked_item(prefix.clone(), item).unwrap();
        let generation = item_admission.generation();
        let TurnResult::Bind {
            bound, compiled, ..
        } = consume_cell_program_item(item_admission.clone()).unwrap()
        else {
            panic!("expected compiler-owned binders");
        };
        let page = bound.iter().find(|binder| binder.name == "page").unwrap();
        let code = compiled.into_code();
        assert!(code
            .certification
            .as_ref()
            .as_ref()
            .unwrap()
            .checked_execution()
            .is_some());
        let before = session.state.public_visibility_snapshot_in(scope).unwrap();
        let before_view = session
            .state
            .compile_view_in(scope)
            .unwrap()
            .admission_digest();
        let before_table = session.state.session_table().clone();
        let before_prefix = prefix.snapshot();

        assert!(matches!(
            HostCarrier::from_checked(
                item_admission,
                page.clone(),
                code.clone(),
                HostBindingType::JSON_VALUE
            ),
            Err(ResidentError::UnsupportedCheckedTurn)
        ));

        assert!(matches!(
            session.mount_json_binding_in(
                scope,
                page,
                generation,
                code.clone(),
                &serde_json::Value::Null
            ),
            Err(ResidentError::UnsupportedCheckedTurn)
        ));
        assert!(matches!(
            session.mount_text_binding_in(scope, page, generation, code.clone(), "host value"),
            Err(ResidentError::UnsupportedCheckedTurn)
        ));
        assert!(matches!(
            session.mount_typed_binding_in(
                scope,
                page,
                generation,
                code.clone(),
                HostBindingType::TEXT,
                &()
            ),
            Err(ResidentError::UnsupportedCheckedTurn)
        ));
        assert!(matches!(
            session.mount_host_value_in(scope, page, generation, code.clone(), |_, _, _| panic!(
                "unsupported checked recipe must not build a host value"
            )),
            Err(ResidentError::UnsupportedCheckedTurn)
        ));
        assert!(matches!(
            HostCarrier::from_compiled(page, code.clone(), HostBindingType::TEXT),
            Err(ResidentError::UnsupportedCheckedTurn)
        ));
        assert!(
            !session.prepared_machine_ready(),
            "refusal must precede native installation"
        );
        assert_eq!(session.state.session_table(), &before_table);
        assert_eq!(
            session.state.public_visibility_snapshot_in(scope).unwrap(),
            before
        );
        assert_eq!(
            session
                .state
                .compile_view_in(scope)
                .unwrap()
                .admission_digest(),
            before_view
        );
        assert!(Arc::ptr_eq(&prefix.snapshot(), &before_prefix));
        assert_eq!(prefix.snapshot().compiler_prefix().next_item(), 0);

        // Refused routes cannot consume the item reservation. Its authenticated
        // bind route still executes and alone advances the sealed prefix.
        assert!(matches!(
            session.run_bind_with_sites("checked bind", code, page, generation),
            Ok(ResidentOutcome::Completed { .. })
        ));
        assert_eq!(prefix.snapshot().compiler_prefix().next_item(), 1);
    }

    fn authored_lib(root: &Path) -> SessionLib {
        tidepool_testing::eval_harness::require_extract();
        let mut lib = SessionLib::open(SessionId(991), root, ModuleEnv::standalone_default())
            .unwrap()
            .with_validation_include(vec![tidepool_testing::eval_harness::prelude_path()]);
        lib.attach_recovery_graph_v2(root.join("declarations.json"))
            .unwrap();
        lib
    }

    fn resident_with_replaced_binding(lib: SessionLib) -> TestSession {
        let mut session =
            ResidentSession::unbootstrapped(frunk::HNil, EmptyOutput, 1024, Some(lib));
        let binding = crate::session::prepared::tests::rooted_publication_fixture(
            &mut session.state,
            "answer",
            71,
        );
        session.state.bind(binding).unwrap();
        session.state.mark_stub_generation(Generation(71));
        session
    }

    #[test]
    fn certified_install_fences_hidden_binding_replacement() {
        use crate::session::{resident_workbench_templates, run_turn, TurnRequest, TurnResult};
        use tidepool_testing::effect_surface::TestEffectSurface;

        tidepool_testing::eval_harness::require_extract();
        let root = tempfile::tempdir().unwrap();
        let effects = TestEffectSurface::minimal(&[]).unwrap();
        let lib =
            SessionLib::open(SessionId(992), root.path(), ModuleEnv::standalone_default()).unwrap();
        let mut session =
            ResidentSession::unbootstrapped(frunk::HNil, EmptyOutput, 1024, Some(lib));
        let binding = crate::session::prepared::tests::rooted_publication_fixture(
            &mut session.state,
            "requestCarrier",
            201,
        );
        let old_id = binding.id;
        session.state.bind(binding).unwrap();
        session.hidden_host_bindings.insert(old_id, ());
        let templates = resident_workbench_templates(effects.preamble(), effects.row(), "");
        let mut includes = effects.include_paths().to_vec();
        includes.push(root.path().to_path_buf());
        let includes = includes.iter().map(PathBuf::as_path).collect::<Vec<_>>();
        let TurnResult::Bind { compiled, .. } = run_turn(TurnRequest {
            exact_context: None,
            session_id: None,
            turn_text: "let retained = (42 :: Int)",
            templates: &templates,
            include: &includes,
            session_root: root.path(),
            inject_modules: &[],
            gen: 1,
            verdict: None,
            target: None,
            retained_imports: &[],
        })
        .unwrap() else {
            panic!("expected compiler-owned binding");
        };
        assert!(compiled.certification.is_some());
        let code = compiled.into_code();
        let pending = session
            .snapshot_run_prepared(code.clone(), PendingPreparedMode::Value, None)
            .unwrap();
        let compiled = pending.compile_off_checkout().unwrap();
        let admitted = session
            .public_visibility_snapshot_in(ScopeId::ROOT)
            .unwrap();
        let internal = session
            .state
            .public_visibility_snapshot_in(ScopeId::ROOT)
            .unwrap();
        assert_eq!(admitted, internal);
        assert!(session.workbench_bindings_in(ScopeId::ROOT).is_empty());
        assert_eq!(internal.bindings, vec![("requestCarrier".into(), old_id)]);
        assert!(internal.machine_incarnation.is_some());

        let replacement = crate::session::prepared::tests::rooted_publication_fixture(
            &mut session.state,
            "requestCarrier",
            202,
        );
        let new_id = replacement.id;
        session.state.bind(replacement).unwrap();
        session.hidden_host_bindings.insert(new_id, ());
        // Exact identities fence low-level native updates independently of
        // the displayed names or the notification epoch.
        assert!(session.workbench_bindings_in(ScopeId::ROOT).is_empty());
        let current = session
            .state
            .public_visibility_snapshot_in(ScopeId::ROOT)
            .unwrap();
        assert_eq!(current.epoch, internal.epoch);
        assert_eq!(current.machine_incarnation, internal.machine_incarnation);
        assert_ne!(current.bindings, internal.bindings);
        assert_eq!(
            session
                .public_visibility_snapshot_in(ScopeId::ROOT)
                .unwrap(),
            current
        );
        let residency = session.residency();
        assert!(session
            .revalidate_and_run_prepared(compiled)
            .unwrap()
            .is_none());
        assert_eq!(session.residency(), residency);
        assert_eq!(
            session
                .state
                .resolve_in(ScopeId::ROOT, "requestCarrier")
                .unwrap()
                .id,
            new_id
        );
    }

    fn assert_published_and_confirm_only(
        session: &mut TestSession,
        root: &Path,
        error: &SessionError,
    ) {
        let commit = error.published_declaration_commit().unwrap_or_else(|| {
            panic!("post-rename failure retains published commit facts: {error:?}")
        });
        assert_eq!(commit.generation, Generation(1));
        assert_eq!(commit.evicted_values, ["answer"]);
        assert!(session.state.resolve_in(ScopeId::ROOT, "answer").is_none());
        let snapshot = session
            .public_visibility_snapshot_in(ScopeId::ROOT)
            .unwrap();
        assert_eq!(snapshot.epoch, 1);
        assert_eq!(snapshot.declaration_tip, Generation(1));
        assert!(snapshot.bindings.is_empty());
        assert!(session
            .state
            .lib()
            .durable_graph
            .as_ref()
            .unwrap()
            .unconfirmed
            .is_some());
        let manifest = root.join("declarations.json");
        let graph = recovery::read_v2(&manifest, root).unwrap().unwrap().graph;
        assert_eq!(graph.nodes().count(), 1);
        assert_eq!(graph.nodes().next().unwrap().id, Generation(1));
        assert!(session
            .state
            .lib()
            .log
            .certified_authored_at(Generation(1))
            .is_some());
        let bytes = std::fs::read(&manifest).unwrap();
        session
            .state
            .lib_mut()
            .confirm_recovery_durability()
            .unwrap();
        session
            .state
            .lib_mut()
            .confirm_recovery_durability()
            .unwrap();
        assert_eq!(std::fs::read(&manifest).unwrap(), bytes);
        assert_eq!(session.state.lib().generation(), Generation(1));
        assert_eq!(
            session.public_visibility_snapshot_in(ScopeId::ROOT),
            Some(snapshot)
        );
        assert!(session
            .state
            .lib()
            .durable_graph
            .as_ref()
            .unwrap()
            .unconfirmed
            .is_none());
    }

    #[test]
    fn staged_authored_uncertainty_finalizes_binding_visibility_and_epoch() {
        let root = tempfile::tempdir().unwrap();
        let lib = authored_lib(root.path());
        let mut session = resident_with_replaced_binding(lib);
        let receipt = session
            .state
            .lib()
            .declaration_receipt(&["answer :: Int\nanswer = 42"])
            .unwrap()
            .unwrap();
        let staged = session
            .stage_declarations_in(ScopeId::ROOT, &receipt, &SourceImports::new(), &[])
            .unwrap_or_else(|error| {
                panic!("stage against the actual binding environment: {error:?}")
            });
        assert_eq!(staged.generation(), Generation(1));
        let before = session
            .public_visibility_snapshot_in(ScopeId::ROOT)
            .unwrap();
        assert_eq!(before.epoch, 0);
        assert_eq!(before.bindings.len(), 1);
        session.state.lib_mut().fail_recovery_durability_once = true;
        let error = session
            .adopt_staged_declaration_in(staged.clone())
            .unwrap_err();
        assert_published_and_confirm_only(&mut session, root.path(), &error);
        assert!(matches!(
            session.adopt_staged_declaration_in(staged.clone()),
            Err(SessionError::StaleStagedDeclaration)
        ));
        session.discard_staged_declaration(&staged);
        assert!(root
            .path()
            .join(staged.module().relative_hs_path())
            .exists());
        assert_eq!(
            session
                .public_visibility_snapshot_in(ScopeId::ROOT)
                .unwrap()
                .epoch,
            1
        );
    }

    #[test]
    fn direct_authored_reservation_uncertainty_preserves_visibility_until_confirmed_retry() {
        let root = tempfile::tempdir().unwrap();
        let lib = authored_lib(root.path());
        let mut session = resident_with_replaced_binding(lib);
        let before = session
            .public_visibility_snapshot_in(ScopeId::ROOT)
            .unwrap();
        let original = session
            .state
            .resolve_in(ScopeId::ROOT, "answer")
            .unwrap()
            .id;
        assert_eq!(before.epoch, 0);
        assert_eq!(before.declaration_tip, Generation(0));
        assert_eq!(before.bindings, vec![("answer".into(), original)]);
        session.state.lib_mut().fail_recovery_durability_once = true;
        let error = session
            .define_scoped_with_imports_in(
                ScopeId::ROOT,
                &["answer :: Int\nanswer = 42"],
                &SourceImports::new(),
            )
            .unwrap_err();
        // Direct define must first publish its identity reservation. At that
        // boundary no authored product or visibility replacement exists yet.
        assert!(
            matches!(&error, SessionError::RecoveryManifest { .. }),
            "{error:?}"
        );
        assert!(error.published_declaration_commit().is_none(), "{error:?}");
        assert_eq!(
            session.public_visibility_snapshot_in(ScopeId::ROOT),
            Some(before.clone())
        );
        assert_eq!(
            session
                .state
                .resolve_in(ScopeId::ROOT, "answer")
                .unwrap()
                .id,
            original
        );
        assert_eq!(session.state.lib().generation(), Generation(1));
        assert_eq!(session.state.lib().scope_tip(ScopeId::ROOT), Generation(0));
        assert!(session
            .state
            .lib()
            .log
            .certified_authored_at(Generation(1))
            .is_none());
        let manifest = root.path().join("declarations.json");
        let graph = recovery::read_v2(&manifest, root.path())
            .unwrap()
            .unwrap()
            .graph;
        assert_eq!(graph.high_water(), Generation(1));
        assert_eq!(graph.nodes().count(), 0);
        let reserved_bytes = std::fs::read(&manifest).unwrap();
        assert!(session
            .state
            .lib()
            .durable_graph
            .as_ref()
            .unwrap()
            .unconfirmed
            .is_some());
        assert!(matches!(
            session
                .state
                .lib_mut()
                .reserve_declaration_generation_durable(),
            Err(SessionError::RecoveryManifest { .. })
        ));
        assert_eq!(session.state.lib().generation(), Generation(1));
        session
            .state
            .lib_mut()
            .confirm_recovery_durability()
            .unwrap();
        session
            .state
            .lib_mut()
            .confirm_recovery_durability()
            .unwrap();
        assert_eq!(std::fs::read(&manifest).unwrap(), reserved_bytes);
        assert_eq!(
            session.public_visibility_snapshot_in(ScopeId::ROOT),
            Some(before)
        );

        // Retrying confirmed direct source is new admission: slot1 stays burned,
        // and actual validated adoption at slot2 alone evicts the live value.
        let committed = session
            .define_scoped_with_imports_in(
                ScopeId::ROOT,
                &["answer :: Int\nanswer = 42"],
                &SourceImports::new(),
            )
            .unwrap_or_else(|error| {
                panic!("confirmed direct retry must adopt the real declaration: {error:?}")
            });
        assert_eq!(committed, Generation(2));
        assert!(session.state.resolve_in(ScopeId::ROOT, "answer").is_none());
        assert!(session
            .current_decl_heads_in(ScopeId::ROOT)
            .iter()
            .any(|(name, _)| name == "answer"));
        let published = session
            .public_visibility_snapshot_in(ScopeId::ROOT)
            .unwrap();
        assert_eq!(published.epoch, 1);
        assert_eq!(published.declaration_tip, Generation(2));
        assert!(published.bindings.is_empty());
        assert!(session
            .state
            .lib()
            .log
            .certified_authored_at(Generation(2))
            .is_some());
        let graph = recovery::read_v2(&manifest, root.path())
            .unwrap()
            .unwrap()
            .graph;
        assert_eq!(graph.high_water(), Generation(2));
        assert_eq!(graph.nodes().count(), 1);
        assert_eq!(graph.nodes().next().unwrap().id, Generation(2));
        assert!(session
            .state
            .lib()
            .durable_graph
            .as_ref()
            .unwrap()
            .unconfirmed
            .is_none());
        let published_bytes = std::fs::read(&manifest).unwrap();
        session
            .state
            .lib_mut()
            .confirm_recovery_durability()
            .unwrap();
        assert_eq!(std::fs::read(&manifest).unwrap(), published_bytes);
        assert_eq!(
            session.public_visibility_snapshot_in(ScopeId::ROOT),
            Some(published)
        );
    }

    #[test]
    fn authored_native_readiness_shares_confirmation_and_rejects_transferred_epoch() {
        use std::os::unix::fs::PermissionsExt;
        struct RunOwner {
            root: PathBuf,
            _lock: std::fs::File,
        }
        impl crate::session::RecoveryRunAuthority for RunOwner {
            fn owns_run(&self, root: &Path) -> std::io::Result<bool> {
                Ok(root.canonicalize()? == self.root)
            }
        }
        tidepool_testing::eval_harness::require_extract();
        let root = tempfile::tempdir().unwrap();
        let lock = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(root.path().join("run-owner.lock"))
            .unwrap();
        lock.try_lock().unwrap();
        let mut lib =
            SessionLib::open(SessionId(997), root.path(), ModuleEnv::standalone_default())
                .unwrap()
                .with_validation_include(vec![tidepool_testing::eval_harness::prelude_path()]);
        lib.attach_owned_recovery_graph_v3(
            root.path().join("declarations.json"),
            Arc::new(RunOwner {
                root: root.path().canonicalize().unwrap(),
                _lock: lock,
            }),
        )
        .unwrap();
        let mut session =
            ResidentSession::unbootstrapped(frunk::HNil, EmptyOutput, 1024, Some(lib));
        let public = session.mint_scope(ScopeId::ROOT).unwrap();
        let owner = crate::session::RecoveryPublicOwner::new(
            &tidepool_repr::ActorPath::parse("root/authored").unwrap(),
            1,
        )
        .unwrap();
        session
            .initialize_durable_public_scope(owner.clone(), public)
            .unwrap();
        let readiness = session.durable_public_readiness(&owner, public).unwrap();
        let sibling_scope = session.mint_scope(ScopeId::ROOT).unwrap();
        let sibling_owner = crate::session::RecoveryPublicOwner::new(
            &tidepool_repr::ActorPath::parse("root/sibling").unwrap(),
            1,
        )
        .unwrap();
        session
            .initialize_durable_public_scope(sibling_owner.clone(), sibling_scope)
            .unwrap();
        let sibling = session
            .durable_public_readiness(&sibling_owner, sibling_scope)
            .unwrap();
        let public_before = session.public_visibility_snapshot_in(public).unwrap();
        let sibling_before = session
            .public_visibility_snapshot_in(sibling_scope)
            .unwrap();
        let execution = session.begin_private_execution(public).unwrap();
        let private = execution.private_scope();
        assert!(readiness.is_ready());
        assert!(sibling.is_ready());

        // The first manifest write reserves an identity, before declaration
        // validation or adoption. Its uncertainty must not claim a declaration
        // commit, and the burned identity must remain unavailable afterward.
        session.state.lib_mut().fail_recovery_durability_once = true;
        let error = session
            .define_scoped_with_imports_in(
                private,
                &["answer :: Int\nanswer = 42"],
                &SourceImports::new(),
            )
            .unwrap_err();
        assert!(
            matches!(&error, SessionError::RecoveryManifest { .. }),
            "{error:?}"
        );
        assert!(error.published_declaration_commit().is_none(), "{error:?}");
        assert_eq!(session.state.lib().generation(), Generation(1));
        assert_eq!(session.state.lib().scope_tip(private), Generation(0));
        let manifest = root.path().join("declarations.json");
        let reserved = recovery::read_v2(&manifest, root.path())
            .unwrap()
            .unwrap()
            .graph;
        assert_eq!(reserved.high_water(), Generation(1));
        assert_eq!(reserved.nodes().count(), 0);
        assert_eq!(
            session.public_visibility_snapshot_in(public),
            Some(public_before.clone())
        );
        assert_eq!(
            session.public_visibility_snapshot_in(sibling_scope),
            Some(sibling_before.clone())
        );
        assert!(!readiness.is_ready());
        assert!(!sibling.is_ready());
        assert!(readiness.is_current());
        assert!(sibling.is_current());
        session
            .confirm_durable_public_scope(&owner, public)
            .unwrap();
        assert!(readiness.is_ready());
        assert!(sibling.is_ready());

        let receipt = session
            .state
            .lib()
            .declaration_receipt(&["answer :: Int\nanswer = 42"])
            .unwrap()
            .unwrap();
        let staged = session
            .stage_declarations_in(private, &receipt, &SourceImports::new(), &[])
            .unwrap();
        assert_eq!(staged.generation(), Generation(2));
        session.state.lib_mut().fail_recovery_durability_once = true;
        let error = session.adopt_staged_declaration_in(staged).unwrap_err();
        let commit = error.published_declaration_commit().unwrap_or_else(|| {
            panic!("validated adoption must retain its published declaration commit: {error:?}")
        });
        assert_eq!(commit.generation, Generation(2));
        assert_eq!(session.state.lib().scope_tip(private), Generation(2));
        assert_eq!(
            session.public_visibility_snapshot_in(public),
            Some(public_before.clone())
        );
        assert_eq!(
            session.public_visibility_snapshot_in(sibling_scope),
            Some(sibling_before.clone())
        );
        let published = recovery::read_v2(&manifest, root.path())
            .unwrap()
            .unwrap()
            .graph;
        assert_eq!(published.high_water(), Generation(2));
        assert_eq!(published.nodes().count(), 1);
        assert_eq!(published.nodes().next().unwrap().id, Generation(2));
        for surface in published.public_surfaces() {
            assert!(surface.declaration_root.is_none());
        }
        let published_bytes = std::fs::read(&manifest).unwrap();
        assert!(!readiness.is_ready());
        assert!(!sibling.is_ready());
        assert!(readiness.is_current());
        assert!(sibling.is_current());
        std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o300)).unwrap();
        assert!(session
            .confirm_durable_public_scope(&owner, public)
            .is_err());
        std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        assert!(!readiness.is_ready());
        assert!(!sibling.is_ready());
        session
            .confirm_durable_public_scope(&owner, public)
            .unwrap();
        session
            .confirm_durable_public_scope(&sibling_owner, sibling_scope)
            .unwrap();
        assert_eq!(std::fs::read(&manifest).unwrap(), published_bytes);
        assert!(readiness.is_ready());
        assert!(sibling.is_ready());
        let next = session
            .state
            .prepare_execution_admission_epoch_advance()
            .unwrap();
        session
            .state
            .invalidate_execution_admissions_after_owner_transfer(next);
        assert!(!readiness.is_ready());
        assert!(!sibling.is_ready());
        assert!(!readiness.is_current());
        assert!(!sibling.is_current());
        let current = session.durable_public_readiness(&owner, public).unwrap();
        assert!(current.is_ready());
        drop(session);
        assert!(!current.is_ready());
        assert!(!current.is_current());
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

/// The result crossing the eval-thread boundary carries typed [`ValueHandle`]
/// identities. The owning machine retains their values under the turn's
/// resource scope until publication adopts them into ROOT.
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
    Deferred {
        id: ContinuationId,
        request: HaskellValue,
        work: DeferredEffect,
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

#[cfg(test)]
#[path = "activation_input_tests.rs"]
mod activation_input_tests;

#[cfg(test)]
mod preview_budget_tests {
    use super::truncate_preview_at_line;
    use proptest::prelude::*;

    fn config() -> proptest::test_runner::Config {
        let mut config = proptest::test_runner::Config::default();
        if let Some(path) = option_env!("TIDEPOOL_PROPTEST_REGRESSIONS") {
            config.failure_persistence = Some(Box::new(
                proptest::test_runner::FileFailurePersistence::Direct(path),
            ));
        }
        config
    }

    proptest! {
        #![proptest_config(config())]
        #[test]
        fn arbitrary_unicode_previews_obey_the_whole_byte_budget(
            characters in prop::collection::vec(any::<char>(), 0..2048),
            budget in 0usize..8193,
        ) {
            let text: String = characters.into_iter().collect();
            let preview = truncate_preview_at_line(text.clone(), budget);
            prop_assert!(preview.len() <= budget);
            if text.len() <= budget {
                prop_assert_eq!(preview, text);
            } else if budget == 0 {
                prop_assert!(preview.is_empty());
            } else {
                // The content is an exact prefix; notices carry no invented
                // output and are included in the measured transport bytes.
                let body = if let Some((body, _)) = preview.split_once("\n[reply exceeds ") {
                    body
                } else {
                    preview.strip_suffix("[reply omitted; pollResponse]")
                        .or_else(|| preview.strip_suffix("[omitted]"))
                        .or_else(|| preview.strip_suffix('~'))
                        .expect("an explicit omission marker")
                };
                prop_assert!(text.starts_with(body));
                prop_assert_eq!(truncate_preview_at_line(preview.clone(), budget), preview);
            }
        }
    }

    #[test]
    fn reply_within_budget_renders_whole_without_a_note() {
        let reply = "Candidate {\n  candidateCommit = 3f2a\n}".to_owned();
        assert_eq!(truncate_preview_at_line(reply.clone(), 8192), reply);
    }

    #[test]
    fn reply_over_budget_is_cut_at_a_line_and_names_the_budget() {
        let line = "x".repeat(99);
        let reply = vec![line.as_str(); 100].join("\n");
        let rendered = truncate_preview_at_line(reply, 8192);
        let (body, note) = rendered.split_once("\n[").expect("a truncation note");
        assert!(rendered.len() <= 8192 && body.ends_with(&line), "{body}");
        assert_eq!(
            note,
            "reply exceeds the 8 KiB notice budget; preview truncated. \
             `pollResponse` on the retained `Response` has the complete value.]"
        );
    }

    #[test]
    fn unicode_at_the_production_limit_and_tiny_budgets_never_split_codepoints() {
        let text = format!("x{}", "λ".repeat(8192));
        for budget in [0, 1, 2, 3, 8, 9, 27, 28, 8191, 8192, usize::MAX] {
            let preview = truncate_preview_at_line(text.clone(), budget);
            assert!(preview.len() <= budget);
            if text.len() > budget && budget > 0 {
                assert!(!preview.is_empty());
            }
        }
        assert_eq!(truncate_preview_at_line("λ".into(), 1), "~");
    }
}
