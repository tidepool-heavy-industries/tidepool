//! Runtime-owned immutable compilation and private execution admission.

use std::any::Any;
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::{Arc, OnceLock};

use tidepool_codegen::binding_table::{BindingTipId, SourceLeaseKey};
use tidepool_codegen::scope::ScopeId;
use tidepool_repr::execution_schema::SymbolIdentity;
use tidepool_repr::Generation;
use tidepool_toolchain::compiler_world::{
    PrivateCheckedItemProgress, PrivateCompilerProgress, PrivateHostBindingProgress,
    PrivateSettledFrontier,
};

use super::{PersistentSession, PublicVisibilitySnapshot, SessionCompileView, SessionError};

/// Native execution admission for one originally admitted durable public owner.
/// The attachment owns its lifetime. A surviving shared run store does not
/// keep a dropped or replaced machine attachment ready.
pub struct RuntimeDurablePublicReadiness {
    owner: Arc<RuntimeAdmissionOwner>,
    owner_epoch: u64,
    session: super::SessionId,
    durable: super::RecoveryPublicOwner,
    scope: ScopeId,
    store: std::sync::Weak<super::RunRecoveryStore>,
    attachment: std::sync::Weak<()>,
}

impl RuntimeDurablePublicReadiness {
    /// A run fence blocks execution while a committed placement can remain
    /// current. Currency alone never authorizes executing work.
    pub fn is_current(&self) -> bool {
        self.owner.epoch() == self.owner_epoch
            && self.attachment.upgrade().is_some()
            && self.store.upgrade().is_some()
    }

    pub fn is_ready(&self) -> bool {
        self.owner.epoch() == self.owner_epoch
            && self.attachment.upgrade().is_some()
            && self
                .store
                .upgrade()
                .is_some_and(|store| store.ensure_ready().is_ok())
    }
    pub async fn begin_provider_transition(
        &self,
    ) -> Result<super::ProviderTransition, SessionError> {
        let attachment = self
            .attachment
            .upgrade()
            .ok_or(SessionError::WrongPublicManifestTicket)?;
        let store = self
            .store
            .upgrade()
            .ok_or(SessionError::WrongPublicManifestTicket)?;
        let ticket = store
            .begin_provider_transition()
            .await
            .map_err(|error| SessionError::RecoveryStore(Box::new(error)))?;
        if self.owner.epoch() != self.owner_epoch {
            ticket
                .refuse()
                .await
                .map_err(|error| SessionError::RecoveryStore(Box::new(error)))?;
            return Err(SessionError::WrongPublicManifestTicket);
        }
        Ok(ticket.retain_attachment(attachment, store))
    }

    pub async fn check_ready_async(&self) -> Result<(), SessionError> {
        let _attachment = self
            .attachment
            .upgrade()
            .ok_or(SessionError::WrongPublicManifestTicket)?;
        let store = self
            .store
            .upgrade()
            .ok_or(SessionError::WrongPublicManifestTicket)?;
        store
            .ensure_ready_async()
            .await
            .map_err(|error| SessionError::RecoveryStore(Box::new(error)))?;
        if self.owner.epoch() != self.owner_epoch {
            return Err(SessionError::WrongPublicManifestTicket);
        }
        Ok(())
    }

    pub fn session(&self) -> super::SessionId {
        self.session
    }
    pub fn scope(&self) -> ScopeId {
        self.scope
    }
    pub fn durable_owner(&self) -> &super::RecoveryPublicOwner {
        &self.durable
    }
}

#[derive(Debug, thiserror::Error)]
pub enum NativeSetupAdmissionFailure {
    #[error("expected one binderless Bind, items={items}, first={first:?}, binders={binders}")]
    Shape {
        items: usize,
        first: Option<tidepool_toolchain::cell_plan::ParsedCellPlanKind>,
        binders: usize,
    },
    #[error("host carrier differs from its single original binder recipe: items={items}, first={first:?}, binders={binders}")]
    HostCarrierShape {
        items: usize,
        first: Option<tidepool_toolchain::cell_plan::ParsedCellPlanKind>,
        binders: usize,
    },
    #[error("specification digest differs: planned={planned:?}, admitted={admitted:?}")]
    SpecificationDigest {
        planned: [u8; 32],
        admitted: [u8; 32],
    },
    #[error("ordered include paths differ: planned={planned:?}, admitted={admitted:?}")]
    IncludePaths {
        planned: NativeSetupInputInventory,
        admitted: NativeSetupInputInventory,
    },
    #[error("ordered injection differs: planned={planned:?}, reachable={reachable:?}")]
    InjectionInventory {
        planned: NativeSetupInputInventory,
        reachable: NativeSetupInputInventory,
    },
    #[error("parser item index differs at {position}: observed={observed}")]
    ItemOrder { position: usize, observed: usize },
}

/// A bounded diagnostic commitment to the complete ordered input inventory.
#[derive(Debug)]
pub struct NativeSetupInputInventory {
    pub count: usize,
    pub digest: [u8; 32],
    pub first: Vec<String>,
}

impl NativeSetupInputInventory {
    fn new<'a>(values: impl IntoIterator<Item = &'a [u8]>) -> Self {
        let mut hash = blake3::Hasher::new();
        hash.update(b"Tidepool.NativeSetupInputInventory.v1");
        let mut count = 0;
        let mut first = Vec::new();
        for value in values {
            count += 1;
            hash.update(&(value.len() as u64).to_le_bytes());
            hash.update(value);
            if first.len() < 8 {
                first.push(String::from_utf8_lossy(value).chars().take(128).collect());
            }
        }
        hash.update(&(count as u64).to_le_bytes());
        Self {
            count,
            digest: *hash.finalize().as_bytes(),
            first,
        }
    }
}

/// A detached lexical owner captured in the same checkout as its public base.
/// Its scope retains exact binding and native shares through the existing
/// binding-store owner; completion must retire that scope once.
pub struct PrivateExecutionAdmission {
    pub(super) library: uuid::Uuid,
    session: super::SessionId,
    pub(super) owner: Arc<RuntimeAdmissionOwner>,
    pub(super) owner_epoch: u64,
    pub(super) durable_owner: Option<super::RecoveryPublicOwner>,
    pub(super) scope_lease: Arc<RuntimeLexicalScopeLease>,
    public_scope: ScopeId,
    private_scope: ScopeId,
    compiler_basis: PrivateExecutionCompilerBasis,
    binding_tip: BindingTipId,
    pub(super) completed_values: parking_lot::Mutex<
        std::collections::HashMap<tidepool_repr::SessionVarId, CertifiedPrivateValueWrite>,
    >,
    state: Arc<parking_lot::Mutex<PrivateExecutionState>>,
    pub(super) final_intent: OnceLock<Arc<super::FinalExecutionIntent>>,
}

struct PrivateExecutionState {
    phase: PrivateExecutionPhase,
    authored: PrivateAuthoredProgram,
    accepted_visible_revision: u64,
    applied_visible_revision: u64,
    unapplied_visible_hosts: std::collections::HashSet<tidepool_repr::SessionVarId>,
    accepted_host_progress: Option<Arc<PrivateHostBindingProgress>>,
    pending_hosts: std::collections::HashMap<tidepool_repr::SessionVarId, PendingHostKind>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PrivateExecutionPhase {
    Open,
    Freezing,
    Frozen,
}

enum PrivateAuthoredProgram {
    Ready(Arc<PrivateSettledFrontier>),
    Active {
        admission_digest: [u8; 32],
        settled: Arc<PrivateSettledFrontier>,
        predecessor: Arc<tidepool_toolchain::compiler_world::CompilerWorld>,
    },
    Failed,
}

/// A freeze borrows its exact completed private owner. Compiler evidence alone
/// cannot construct this runtime frontier or substitute a different admission.
pub(super) struct CompletedPrivateFrontier<'a> {
    admission: &'a PrivateExecutionAdmission,
    frontier: Arc<PrivateSettledFrontier>,
    scope_lease: Arc<RuntimeLexicalScopeLease>,
}

impl<'a> CompletedPrivateFrontier<'a> {
    pub(super) fn admission(&self) -> &'a PrivateExecutionAdmission {
        self.admission
    }

    pub(super) fn publication(&self) -> PrivateCompilerProgress {
        self.frontier.publication()
    }

    pub(super) fn scope_lease(&self) -> &Arc<RuntimeLexicalScopeLease> {
        &self.scope_lease
    }
}

static_assertions::assert_not_impl_any!(CompletedPrivateFrontier<'static>: Clone, Copy);

impl std::fmt::Debug for PrivateAuthoredProgram {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Ready(frontier) => formatter.debug_tuple("Ready").field(frontier).finish(),
            Self::Active {
                admission_digest,
                predecessor,
                ..
            } => formatter
                .debug_struct("Active")
                .field("admission_digest", admission_digest)
                .field("predecessor", predecessor.logical_id())
                .finish(),
            Self::Failed => formatter.write_str("Failed"),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum PendingHostKind {
    Hidden,
    Visible,
}

#[derive(Clone, Copy, Debug)]
enum PrivateHostDeltaDisposition {
    Applied,
    Superseded,
}

pub(super) struct PrivateFreezeTransition<'a> {
    admission: &'a PrivateExecutionAdmission,
    committed: bool,
}

impl PrivateFreezeTransition<'_> {
    pub(super) fn commit(mut self) {
        self.admission.state.lock().phase = PrivateExecutionPhase::Frozen;
        self.committed = true;
    }
}

impl Drop for PrivateFreezeTransition<'_> {
    fn drop(&mut self) {
        if !self.committed {
            let mut state = self.admission.state.lock();
            if state.phase == PrivateExecutionPhase::Freezing {
                state.phase = PrivateExecutionPhase::Open;
            }
        }
    }
}

/// One exact authored preparation offer. It names the session's current World
/// association and accepted/applied watermarks without owning mutable state.
pub struct PrivatePreparationSnapshot {
    execution: Arc<PrivateExecutionAdmission>,
    association: Arc<super::inspection::InspectionWorldAssociation>,
    accepted_revision: u64,
    applied_revision: u64,
    frontier: Arc<PrivateSettledFrontier>,
    selected_input: Option<tidepool_repr::SessionVarId>,
}

/// One readiness issuance, including the exact optional hidden operand.
/// Neither part is minted against a different compiler predecessor.
pub struct PrivateCellPreparation {
    pub(super) snapshot: Arc<PrivatePreparationSnapshot>,
    pub(super) operand: Option<Arc<RuntimeWorldCellOperand>>,
}

impl PrivateCellPreparation {
    pub fn snapshot(&self) -> &Arc<PrivatePreparationSnapshot> {
        &self.snapshot
    }

    pub fn into_parts(
        self,
    ) -> (
        Arc<PrivatePreparationSnapshot>,
        Option<Arc<RuntimeWorldCellOperand>>,
    ) {
        (self.snapshot, self.operand)
    }
}

/// A narrow internal carrier uses the current idle World or the active
/// program's exact parked predecessor, without draining accepted host deltas.
pub struct PrivateHostCarrierPreparation {
    execution: Arc<PrivateExecutionAdmission>,
    association: Arc<super::inspection::InspectionWorldAssociation>,
    mode: PrivateCarrierMode,
}

enum PrivateCarrierMode {
    Ready(Arc<PrivateSettledFrontier>),
    Active { admission_digest: [u8; 32] },
}

impl PrivateHostCarrierPreparation {
    pub fn world(&self) -> &Arc<tidepool_toolchain::compiler_world::CompilerWorld> {
        &self.association.world
    }

    pub(super) fn scope(&self) -> ScopeId {
        self.execution.private_scope()
    }
}

impl PrivatePreparationSnapshot {
    pub fn world(&self) -> &Arc<tidepool_toolchain::compiler_world::CompilerWorld> {
        &self.association.world
    }

    pub(super) fn association(&self) -> &Arc<super::inspection::InspectionWorldAssociation> {
        &self.association
    }
}

struct PrivateExecutionCompilerBasis {
    published: Arc<super::inspection::InspectionWorldAssociation>,
    private_key: super::persistent::CompileViewKey,
    source: Arc<super::GenerationArtifactReference>,
    original_source: Arc<RuntimeOriginalSource>,
    public_target: Arc<RuntimeLexicalScopeLease>,
    admitted: PublicVisibilitySnapshot,
    admitted_context: Option<Arc<tidepool_toolchain::declaration_join::ExactDeclarationContext>>,
}

fn private_pending_is_ready(
    state: &PrivateExecutionState,
    selected: Option<tidepool_repr::SessionVarId>,
) -> bool {
    match selected {
        Some(id) => {
            state.pending_hosts.len() == 1
                && state.pending_hosts.get(&id) == Some(&PendingHostKind::Hidden)
        }
        None => state.pending_hosts.is_empty(),
    }
}

fn execution_snapshot_is_ready(
    state: &PrivateExecutionState,
    snapshot: &PrivatePreparationSnapshot,
) -> Result<(), SessionError> {
    if state.phase != PrivateExecutionPhase::Open
        || state.accepted_visible_revision != snapshot.accepted_revision
        || state.applied_visible_revision != snapshot.applied_revision
        || snapshot.accepted_revision != snapshot.applied_revision
        || !private_pending_is_ready(state, snapshot.selected_input)
        || !matches!(&state.authored, PrivateAuthoredProgram::Ready(frontier)
            if Arc::ptr_eq(frontier, &snapshot.frontier))
    {
        return Err(SessionError::StaleStagedDeclaration);
    }
    Ok(())
}

/// Exact facts captured when a private execution borrows a published World.
/// Cloning them preserves the SAME admitted association and source Arcs; none
/// of these facts reconstruct a compiler view or issue execution authority.
pub(super) struct WorldExecutionAdmissionFacts {
    pub(super) published: Arc<super::inspection::InspectionWorldAssociation>,
    pub(super) source: Arc<super::GenerationArtifactReference>,
    pub(super) original_source: Arc<RuntimeOriginalSource>,
    pub(super) public_target: Arc<RuntimeLexicalScopeLease>,
    pub(super) admitted: PublicVisibilitySnapshot,
    pub(super) admitted_context:
        Option<Arc<tidepool_toolchain::declaration_join::ExactDeclarationContext>>,
}

#[derive(Clone)]
pub(super) struct CertifiedPrivateValueWrite {
    name: String,
    identity: SymbolIdentity,
    generation: Generation,
    root_id: u64,
    pub(super) proof: CertifiedPrivateValueProof,
    visible_host: Option<PrivateVisibleHostPublication>,
}

#[derive(Clone)]
pub(super) enum CertifiedPrivateValueProof {
    Execution(Arc<tidepool_toolchain::checked_cell::ExactCompiledItem>),
    HostInterface(Arc<tidepool_toolchain::checked_cell::ExactHostBindingInterface>),
}

impl CertifiedPrivateValueProof {
    fn generation(&self) -> u64 {
        match self {
            Self::Execution(proof) => proof.generation(),
            Self::HostInterface(proof) => proof.generation(),
        }
    }
    fn same_original(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Execution(a), Self::Execution(b)) => Arc::ptr_eq(a, b),
            (Self::HostInterface(a), Self::HostInterface(b)) => Arc::ptr_eq(a, b),
            _ => false,
        }
    }
}

enum BindingInterfaceTarget {
    Fresh,
    Activation {
        execution: Arc<PrivateExecutionAdmission>,
        predecessor: ActivationBindingPredecessor,
    },
}

enum ActivationBindingPredecessor {
    Fresh,
    ReplaceInherited {
        id: tidepool_repr::SessionVarId,
        scope: ScopeId,
        module: tidepool_repr::SessionModule,
        identity: SymbolIdentity,
        root_id: u64,
    },
}

/// One thin-interface slot with its issued native binding target.
/// Issuance cannot consume this reservation or publish a binding.
pub struct RuntimeBindingInterfaceReservation {
    owner: Arc<RuntimeAdmissionOwner>,
    owner_epoch: u64,
    pub(super) scope: ScopeId,
    pub(super) session_root: PathBuf,
    visibility: PublicVisibilitySnapshot,
    view_digest: [u8; 32],
    pub(super) generation: Generation,
    pub(super) binding: String,
    pub(super) prototype: Arc<tidepool_toolchain::checked_cell::ExactHostBindingPrototype>,
    digest: [u8; 32],
    consumed: std::sync::atomic::AtomicBool,
    target: BindingInterfaceTarget,
}

impl RuntimeBindingInterfaceReservation {
    pub fn digest(&self) -> [u8; 32] {
        self.digest
    }
    pub fn generation(&self) -> Generation {
        self.generation
    }
    pub fn binding(&self) -> &str {
        &self.binding
    }
    pub fn prototype(&self) -> &Arc<tidepool_toolchain::checked_cell::ExactHostBindingPrototype> {
        &self.prototype
    }
}

/// A host builder keeps its representation and executable owner separately
/// from the shared type-interface reservation.
pub struct RuntimeHostBindingAdmission {
    reservation: Arc<RuntimeBindingInterfaceReservation>,
    pub(super) prototype: Arc<super::resident::HostBindingPrototype>,
    private_preparation: Option<Arc<PrivateHostCarrierPreparation>>,
}

impl std::ops::Deref for RuntimeHostBindingAdmission {
    type Target = RuntimeBindingInterfaceReservation;
    fn deref(&self) -> &Self::Target {
        &self.reservation
    }
}

impl RuntimeHostBindingAdmission {
    pub fn prototype(&self) -> &Arc<super::resident::HostBindingPrototype> {
        &self.prototype
    }
}

impl PersistentSession {
    pub(super) fn validate_fresh_host_binding_in(
        &self,
        scope: ScopeId,
        binding: &str,
    ) -> Result<(), SessionError> {
        if !self.scope_tree().is_live(scope) {
            return Err(SessionError::DeadScope(scope));
        }
        if self.resolve_in(scope, binding).is_some()
            || self.has_lib()
                && self
                    .lib()
                    .current_declarations_in(scope)
                    .iter()
                    .any(|(item, _)| item.value_names().any(|name| name == binding))
        {
            return Err(SessionError::HostBindingNameCollision {
                scope,
                binding: binding.to_owned(),
            });
        }
        Ok(())
    }

    pub(super) fn admit_binding_interface(
        &mut self,
        scope: ScopeId,
        binding: String,
        prototype: Arc<tidepool_toolchain::checked_cell::ExactHostBindingPrototype>,
        authority_digest: [u8; 32],
    ) -> Result<Arc<RuntimeBindingInterfaceReservation>, SessionError> {
        if self
            .source_lease_in(scope)?
            .private_lifecycle
            .get()
            .is_some()
        {
            return Err(SessionError::WorldCellRequiresServicePreparation);
        }
        self.admit_binding_interface_target(
            scope,
            binding,
            prototype,
            authority_digest,
            BindingInterfaceTarget::Fresh,
        )
    }

    pub(super) fn admit_activation_binding_interface(
        &mut self,
        execution: Arc<PrivateExecutionAdmission>,
        snapshot: &PrivatePreparationSnapshot,
        prototype: Arc<tidepool_toolchain::checked_cell::ExactHostBindingPrototype>,
        authority_digest: [u8; 32],
    ) -> Result<Arc<RuntimeBindingInterfaceReservation>, SessionError> {
        execution.ensure_snapshot_current(snapshot)?;
        if !Arc::ptr_eq(&snapshot.execution, &execution)
            || !Arc::ptr_eq(
                &self.current_private_world_association(&execution)?,
                snapshot.association(),
            )
        {
            return Err(SessionError::StaleStagedDeclaration);
        }
        let binding = super::resident::RuntimeActivationInput::binding_name().to_owned();
        let predecessor = match self.resolve_in(execution.private_scope(), &binding) {
            None => ActivationBindingPredecessor::Fresh,
            Some(entry)
                if entry.scope != execution.private_scope()
                    && execution
                        .admitted_public()?
                        .bindings
                        .iter()
                        .any(|(name, id)| name == &binding && *id == entry.id) =>
            {
                ActivationBindingPredecessor::ReplaceInherited {
                    id: entry.id,
                    scope: entry.scope,
                    module: entry.module,
                    identity: entry.value.identity.clone(),
                    root_id: entry.value.handle.raw().0,
                }
            }
            Some(_) => {
                return Err(SessionError::HostBindingNameCollision {
                    scope: execution.private_scope(),
                    binding,
                })
            }
        };
        self.admit_binding_interface_target(
            execution.private_scope(),
            binding,
            prototype,
            authority_digest,
            BindingInterfaceTarget::Activation {
                execution,
                predecessor,
            },
        )
    }

    fn admit_binding_interface_target(
        &mut self,
        scope: ScopeId,
        binding: String,
        prototype: Arc<tidepool_toolchain::checked_cell::ExactHostBindingPrototype>,
        authority_digest: [u8; 32],
        target: BindingInterfaceTarget,
    ) -> Result<Arc<RuntimeBindingInterfaceReservation>, SessionError> {
        if !self.scope_tree().is_live(scope) {
            return Err(SessionError::DeadScope(scope));
        }
        self.admit_source_work_in(scope)?;
        self.validate_binding_target(scope, &binding, &target)?;
        let (generation, _) = self.reserve_value_generations(1)?;
        let visibility = self
            .public_visibility_snapshot_in(scope)
            .ok_or(SessionError::StaleStagedDeclaration)?;
        let view_digest = self
            .compile_view_digest_in(scope)
            .ok_or(SessionError::StaleStagedDeclaration)?;
        let session_root = self
            .compile_view_in(scope)
            .ok_or(SessionError::StaleStagedDeclaration)?
            .session_root()
            .to_path_buf();
        let owner_epoch = self.admission_owner().epoch();
        let mut digest = blake3::Hasher::new();
        digest.update(b"TidepoolRuntimeBindingInterface2");
        digest.update(self.admission_owner().identity.as_bytes());
        digest.update(&owner_epoch.to_le_bytes());
        digest.update(&scope.0.to_le_bytes());
        digest.update(&view_digest);
        digest.update(&generation.0.to_le_bytes());
        digest.update(&(binding.len() as u64).to_le_bytes());
        digest.update(binding.as_bytes());
        digest.update(&prototype.digest());
        digest.update(&authority_digest);
        Ok(Arc::new(RuntimeBindingInterfaceReservation {
            owner: self.admission_owner().clone(),
            owner_epoch,
            scope,
            session_root,
            visibility,
            view_digest,
            generation,
            binding,
            prototype,
            digest: *digest.finalize().as_bytes(),
            consumed: std::sync::atomic::AtomicBool::new(false),
            target,
        }))
    }

    pub(super) fn admit_host_binding_interface(
        &mut self,
        scope: ScopeId,
        binding: String,
        prototype: Arc<super::resident::HostBindingPrototype>,
    ) -> Result<Arc<RuntimeHostBindingAdmission>, SessionError> {
        self.admit_host_binding_interface_with_preparation(scope, binding, prototype, None)
    }

    pub(super) fn admit_private_host_binding_interface(
        &mut self,
        preparation: Arc<PrivateHostCarrierPreparation>,
        binding: String,
        prototype: Arc<super::resident::HostBindingPrototype>,
    ) -> Result<Arc<RuntimeHostBindingAdmission>, SessionError> {
        self.validate_private_host_carrier(&preparation)?;
        self.admit_host_binding_interface_with_preparation(
            preparation.scope(),
            binding,
            prototype,
            Some(preparation),
        )
    }

    fn admit_host_binding_interface_with_preparation(
        &mut self,
        scope: ScopeId,
        binding: String,
        prototype: Arc<super::resident::HostBindingPrototype>,
        private_preparation: Option<Arc<PrivateHostCarrierPreparation>>,
    ) -> Result<Arc<RuntimeHostBindingAdmission>, SessionError> {
        if private_preparation.is_none()
            && self
                .source_lease_in(scope)?
                .private_lifecycle
                .get()
                .is_some()
        {
            return Err(SessionError::WorldCellRequiresServicePreparation);
        }
        let reservation = self.admit_binding_interface_target(
            scope,
            binding,
            prototype.compiler().clone(),
            prototype.authority_digest(),
            BindingInterfaceTarget::Fresh,
        )?;
        Ok(Arc::new(RuntimeHostBindingAdmission {
            reservation,
            prototype,
            private_preparation,
        }))
    }

    fn validate_binding_target(
        &self,
        scope: ScopeId,
        binding: &str,
        target: &BindingInterfaceTarget,
    ) -> Result<(), SessionError> {
        let BindingInterfaceTarget::Activation {
            execution,
            predecessor,
        } = target
        else {
            return self.validate_fresh_host_binding_in(scope, binding);
        };
        execution.ensure_open()?;
        if binding != super::resident::RuntimeActivationInput::binding_name()
            || scope != execution.private_scope()
            || !Arc::ptr_eq(&execution.owner, self.admission_owner())
            || execution.owner_epoch != self.admission_owner().epoch()
            || self
                .lib()
                .current_declarations_in(scope)
                .iter()
                .any(|(item, _)| item.value_names().any(|name| name == binding))
        {
            return Err(SessionError::StaleStagedDeclaration);
        }
        self.current_private_world_association(execution)?;
        let current = self.resolve_in(scope, binding);
        let valid = match (predecessor, current) {
            (ActivationBindingPredecessor::Fresh, None) => true,
            (
                ActivationBindingPredecessor::ReplaceInherited {
                    id,
                    scope: expected_scope,
                    module,
                    identity,
                    root_id,
                },
                Some(entry),
            ) => {
                entry.id == *id
                    && entry.scope == *expected_scope
                    && entry.scope != scope
                    && entry.module == *module
                    && entry.value.identity == *identity
                    && entry.value.handle.raw().0 == *root_id
            }
            _ => false,
        };
        if valid {
            Ok(())
        } else {
            Err(SessionError::StaleStagedDeclaration)
        }
    }

    pub(super) fn validate_binding_interface_target(
        &self,
        admission: &RuntimeBindingInterfaceReservation,
    ) -> Result<(), SessionError> {
        self.validate_binding_target(admission.scope, &admission.binding, &admission.target)
    }

    pub(super) fn validate_binding_interface(
        &self,
        admission: &RuntimeBindingInterfaceReservation,
        interface: &tidepool_toolchain::checked_cell::ExactHostBindingInterface,
    ) -> Result<(), SessionError> {
        if !Arc::ptr_eq(&admission.owner, self.admission_owner())
            || admission.owner_epoch != self.admission_owner().epoch()
            || !Arc::ptr_eq(interface.prototype(), &admission.prototype)
            || interface.admission_digest() != admission.digest
            || interface.generation() != admission.generation.0
            || interface.value_interface_certificate().owner()
                != tidepool_repr::SessionModule::val(admission.generation)
            || self.public_visibility_snapshot_in(admission.scope).as_ref()
                != Some(&admission.visibility)
            || self.compile_view_digest_in(admission.scope) != Some(admission.view_digest)
            || admission
                .consumed
                .load(std::sync::atomic::Ordering::Acquire)
        {
            return Err(SessionError::StaleStagedDeclaration);
        }
        self.validate_binding_interface_target(admission)
    }

    pub(super) fn consume_binding_interface(
        &self,
        admission: &RuntimeBindingInterfaceReservation,
        interface: &tidepool_toolchain::checked_cell::ExactHostBindingInterface,
    ) -> Result<(), SessionError> {
        self.validate_binding_interface(admission, interface)?;
        admission
            .consumed
            .compare_exchange(
                false,
                true,
                std::sync::atomic::Ordering::AcqRel,
                std::sync::atomic::Ordering::Acquire,
            )
            .map_err(|_| SessionError::StaleStagedDeclaration)?;
        Ok(())
    }

    pub(super) fn consume_host_binding_interface(
        &self,
        admission: &RuntimeHostBindingAdmission,
        interface: &tidepool_toolchain::checked_cell::ExactHostBindingInterface,
    ) -> Result<(), SessionError> {
        if let Some(preparation) = &admission.private_preparation {
            self.validate_private_host_carrier(preparation)?;
        }
        self.consume_binding_interface(&admission.reservation, interface)
    }
}
impl CertifiedPrivateValueWrite {
    pub(super) fn name(&self) -> &str {
        &self.name
    }

    pub(super) fn visible_host(&self) -> Option<&PrivateVisibleHostPublication> {
        self.visible_host.as_ref()
    }

    pub(super) fn matches(&self, entry: &tidepool_codegen::binding_table::BindingEntry) -> bool {
        entry.module == tidepool_repr::SessionModule::val(self.generation)
            && entry.value.identity == self.identity
            && entry.value.handle.raw().0 == self.root_id
            && entry.name.0 == self.name
    }
}

/// An original checked host mount awaiting acceptance by its effect owner.
/// This proves a native write, not completion of the surrounding Haskell item.
/// Dropping it does not accept publication; the caller retains the mount's
/// ordinary abandonment guard until accepting this receipt.
pub struct PendingHostValueWrite {
    binder: super::BoundBinder,
    owner: Arc<RuntimeAdmissionOwner>,
    owner_epoch: u64,
    scope: ScopeId,
    id: tidepool_repr::SessionVarId,
    write: CertifiedPrivateValueWrite,
    private_mount: Option<PrivateHostMount>,
}

/// Prevalidated activation receipt recipe retained across the native mount.
/// Only a committed mount supplies its actual root and successor association.
pub(super) struct PreparedActivationMountWrite {
    owner: Arc<RuntimeAdmissionOwner>,
    owner_epoch: u64,
    execution: Arc<PrivateExecutionAdmission>,
    binder: super::BoundBinder,
    interface: Arc<tidepool_toolchain::checked_cell::ExactHostBindingInterface>,
    before: Arc<super::inspection::InspectionWorldAssociation>,
}

impl PreparedActivationMountWrite {
    pub(super) fn committed(
        self,
        root_id: u64,
        after: Arc<super::inspection::InspectionWorldAssociation>,
    ) -> PendingHostValueWrite {
        PendingHostValueWrite {
            binder: self.binder.clone(),
            owner: self.owner,
            owner_epoch: self.owner_epoch,
            scope: self.execution.private_scope(),
            id: tidepool_repr::SessionVarId::from_extract(self.binder.var_id),
            write: CertifiedPrivateValueWrite {
                name: self.binder.name.clone(),
                identity: SymbolIdentity {
                    unit: "main".into(),
                    module: self.binder.module,
                    namespace: "value".into(),
                    occurrence: self.binder.name,
                    record_parent: None,
                },
                generation: self.interface.value_interface_certificate().owner().gen,
                root_id,
                proof: CertifiedPrivateValueProof::HostInterface(self.interface),
                visible_host: None,
            },
            private_mount: Some(PrivateHostMount {
                execution: self.execution,
                before: self.before,
                after,
                kind: PendingHostKind::Visible,
            }),
        }
    }
}

/// The actual runtime owner's transition across one fresh private input.
struct PrivateHostMount {
    execution: Arc<PrivateExecutionAdmission>,
    before: Arc<super::inspection::InspectionWorldAssociation>,
    after: Arc<super::inspection::InspectionWorldAssociation>,
    kind: PendingHostKind,
}

#[derive(Clone)]
pub(super) struct PrivateVisibleHostPublication {
    pub(super) before: Arc<super::inspection::InspectionWorldAssociation>,
    pub(super) after: Arc<super::inspection::InspectionWorldAssociation>,
}

/// One actual private host input, paired with its compiler interface and roots.
/// This never adds the input to the public compiler World.
pub struct RuntimeWorldCellOperand {
    pub(super) compiler: Arc<tidepool_toolchain::compiler_world::CompilerWorldCellOperand>,
    execution: Arc<PrivateExecutionAdmission>,
    id: tidepool_repr::SessionVarId,
    write: CertifiedPrivateValueWrite,
    _binding: super::resident::BindingLease,
}

impl RuntimeWorldCellOperand {
    pub(super) fn admit_preparation(
        &self,
        source: &super::RuntimeSourceCompilerTransaction,
        _world: &tidepool_toolchain::compiler_world::CompilerWorld,
    ) -> Result<(), SessionError> {
        let expected = self.execution.original_source()?;
        source.admit_original(&expected, Some(self.execution.retained_lexical_scope()))?;
        Ok(())
    }

    fn matches(
        &self,
        execution: &Arc<PrivateExecutionAdmission>,
        input: &PendingHostValueWrite,
        compiler: &Arc<tidepool_toolchain::compiler_world::CompilerWorldCellOperand>,
    ) -> bool {
        Arc::ptr_eq(&self.execution, execution)
            && Arc::ptr_eq(&self.compiler, compiler)
            && self.id == input.id
            && self.write.name == input.write.name
            && self.write.identity == input.write.identity
            && self.write.generation == input.write.generation
            && self.write.root_id == input.write.root_id
            && self.write.proof.same_original(&input.write.proof)
    }
}

impl std::fmt::Debug for PendingHostValueWrite {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("PendingHostValueWrite")
            .field("scope", &self.scope)
            .field("id", &self.id)
            .finish_non_exhaustive()
    }
}

impl PendingHostValueWrite {
    pub(super) fn private_mount_owner(
        &self,
    ) -> Option<(&Arc<PrivateExecutionAdmission>, &super::BoundBinder)> {
        self.private_mount
            .as_ref()
            .map(|mount| (&mount.execution, &self.binder))
    }

    pub(super) fn accept_original_mount(&mut self) -> Result<(), SessionError> {
        let execution = self
            .private_mount
            .as_ref()
            .ok_or(SessionError::StaleStagedDeclaration)?
            .execution
            .clone();
        self.accept(&execution)
    }

    /// Actual scope retirement releases binding ownership even while a compiled
    /// consumer still leases its root. It does not require the source to reopen.
    pub(super) fn settle_retired_scope(
        &self,
        session: &PersistentSession,
        execution: &PrivateExecutionAdmission,
    ) -> Result<(), SessionError> {
        let mount = self
            .private_mount
            .as_ref()
            .filter(|mount| std::ptr::eq(mount.execution.as_ref(), execution))
            .ok_or(SessionError::StaleStagedDeclaration)?;
        if !Arc::ptr_eq(&self.owner, session.admission_owner())
            || !Arc::ptr_eq(&self.owner, &execution.owner)
            || self.owner_epoch != execution.owner_epoch
            || self.scope != execution.private_scope()
            || session.scope_tree().is_live(self.scope)
            || session
                .bindings()
                .get(self.id)
                .is_some_and(|entry| entry.scope != self.scope || !self.write.matches(entry))
        {
            return Err(SessionError::StaleStagedDeclaration);
        }
        execution.settle_pending_host(self.id, mount.kind)
    }

    pub(super) fn prepare_activation_mount(
        session: &PersistentSession,
        execution: Arc<PrivateExecutionAdmission>,
        binder: &super::BoundBinder,
        interface: Arc<tidepool_toolchain::checked_cell::ExactHostBindingInterface>,
        before: Arc<super::inspection::InspectionWorldAssociation>,
    ) -> Result<PreparedActivationMountWrite, SessionError> {
        let certified =
            super::turn::decode_bound_binder(interface.binder()).map_err(SessionError::Compile)?;
        if !Arc::ptr_eq(&execution.owner, session.admission_owner())
            || execution.owner_epoch != session.admission_owner().epoch()
            || session
                .lib()
                .tips
                .get(&execution.private_scope())
                .and_then(|tip| tip.inspection_world.as_ref())
                .is_none_or(|live| !Arc::ptr_eq(live, &before))
            || certified.name != binder.name
            || certified.var_id != binder.var_id
            || certified.module != binder.module
        {
            return Err(SessionError::StaleStagedDeclaration);
        }
        Ok(PreparedActivationMountWrite {
            owner: session.admission_owner().clone(),
            owner_epoch: session.admission_owner().epoch(),
            execution,
            binder: binder.clone(),
            interface,
            before,
        })
    }

    pub(super) fn id(&self) -> tidepool_repr::SessionVarId {
        self.id
    }

    pub(super) fn world_cell_operand(
        &self,
        session: &PersistentSession,
        execution: Arc<PrivateExecutionAdmission>,
        snapshot: &PrivatePreparationSnapshot,
        binding: super::resident::BindingLease,
    ) -> Result<Arc<RuntimeWorldCellOperand>, SessionError> {
        self.private_hidden_mount_key(session, &execution)?;
        if !Arc::ptr_eq(&snapshot.execution, &execution)
            || snapshot.selected_input != Some(self.id)
            || !binding.retained_bindings().contains(&self.id)
        {
            return Err(SessionError::StaleStagedDeclaration);
        }
        let world = snapshot.world();
        use tidepool_toolchain::compiler_world::{
            CompilerWorldCellOperand, WorldCellOperandExposure,
        };
        let compiler = match &self.write.proof {
            CertifiedPrivateValueProof::Execution(proof) => {
                CompilerWorldCellOperand::from_checked_host_execution(
                    world,
                    WorldCellOperandExposure::Input,
                    self.scope.0,
                    proof,
                )?
            }
            CertifiedPrivateValueProof::HostInterface(proof) => {
                CompilerWorldCellOperand::from_host_interface(
                    world,
                    WorldCellOperandExposure::Input,
                    self.scope.0,
                    proof,
                )?
            }
        };
        if compiler.session_var() != self.id
            || compiler.symbol() != &self.write.identity
            || compiler.generation() != self.write.generation.0
            || compiler.defining_scope() != self.scope.0
        {
            return Err(SessionError::StaleStagedDeclaration);
        }
        Ok(Arc::new(RuntimeWorldCellOperand {
            compiler,
            execution,
            id: self.id,
            write: self.write.clone(),
            _binding: binding,
        }))
    }

    pub(super) fn mounted(
        session: &PersistentSession,
        scope: ScopeId,
        binder: &super::BoundBinder,
        execution: Arc<tidepool_toolchain::checked_cell::ExactCompiledItem>,
    ) -> Result<Self, SessionError> {
        Self::mounted_with_proof(
            session,
            scope,
            binder,
            CertifiedPrivateValueProof::Execution(execution),
        )
    }

    pub(super) fn mounted_host_interface(
        session: &PersistentSession,
        scope: ScopeId,
        binder: &super::BoundBinder,
        interface: Arc<tidepool_toolchain::checked_cell::ExactHostBindingInterface>,
    ) -> Result<Self, SessionError> {
        Self::mounted_with_proof(
            session,
            scope,
            binder,
            CertifiedPrivateValueProof::HostInterface(interface),
        )
    }

    fn mounted_with_proof(
        session: &PersistentSession,
        scope: ScopeId,
        binder: &super::BoundBinder,
        proof: CertifiedPrivateValueProof,
    ) -> Result<Self, SessionError> {
        let id = tidepool_repr::SessionVarId::from_extract(binder.var_id);
        let entry = session
            .resolve_in(scope, &binder.name)
            .filter(|entry| {
                entry.id == id && entry.scope == scope && entry.module.gen.0 == proof.generation()
            })
            .ok_or(SessionError::StaleStagedDeclaration)?;
        Ok(Self {
            binder: binder.clone(),
            owner: session.admission_owner().clone(),
            owner_epoch: session.admission_owner().epoch(),
            scope,
            id,
            write: CertifiedPrivateValueWrite {
                name: entry.name.0.clone(),
                identity: entry.value.identity.clone(),
                generation: entry.module.gen,
                root_id: entry.value.handle.raw().0,
                proof,
                visible_host: None,
            },
            private_mount: None,
        })
    }

    pub(super) fn retain_private_hidden_mount(
        mut self,
        session: &PersistentSession,
        execution: Arc<PrivateExecutionAdmission>,
        before: Arc<super::inspection::InspectionWorldAssociation>,
        after: Arc<super::inspection::InspectionWorldAssociation>,
    ) -> Result<Self, SessionError> {
        if !self.matches_private_mount(session, &execution)
            || !Arc::ptr_eq(&before.world, &after.world)
            || !Arc::ptr_eq(&before.base.source, &after.base.source)
            || !before
                .base
                .original_source
                .same_observation(&after.base.original_source)
            || !before.base.target.same_observation(&after.base.target)
            || !session.inspection_world_base_is_current(
                &after.base,
                &after.base.source,
                &execution.original_source()?,
            )
            || !session
                .lib()
                .tips
                .get(&self.scope)
                .and_then(|tip| tip.inspection_world.as_ref())
                .is_some_and(|live| Arc::ptr_eq(live, &after))
        {
            return Err(SessionError::StaleStagedDeclaration);
        }
        self.private_mount = Some(PrivateHostMount {
            execution,
            before,
            after,
            kind: PendingHostKind::Hidden,
        });
        Ok(self)
    }

    pub(super) fn retain_private_visible_mount(
        mut self,
        session: &PersistentSession,
        execution: Arc<PrivateExecutionAdmission>,
        before: Arc<super::inspection::InspectionWorldAssociation>,
        after: Arc<super::inspection::InspectionWorldAssociation>,
    ) -> Result<Self, SessionError> {
        if !self.matches_private_mount(session, &execution)
            || !Arc::ptr_eq(&before.world, &after.world)
            || !Arc::ptr_eq(&before.base.source, &after.base.source)
            || !before
                .base
                .original_source
                .same_observation(&after.base.original_source)
            || !before.base.target.same_observation(&after.base.target)
            || !session.inspection_world_base_is_current(
                &after.base,
                &after.base.source,
                &execution.original_source()?,
            )
            || !session
                .lib()
                .tips
                .get(&self.scope)
                .and_then(|tip| tip.inspection_world.as_ref())
                .is_some_and(|live| Arc::ptr_eq(live, &after))
        {
            return Err(SessionError::StaleStagedDeclaration);
        }
        self.private_mount = Some(PrivateHostMount {
            execution,
            before,
            after,
            kind: PendingHostKind::Visible,
        });
        Ok(self)
    }

    pub(super) fn matches_private_mount(
        &self,
        session: &PersistentSession,
        execution: &PrivateExecutionAdmission,
    ) -> bool {
        Arc::ptr_eq(&self.owner, &execution.owner)
            && Arc::ptr_eq(&self.owner, session.admission_owner())
            && self.owner_epoch == execution.owner_epoch
            && self.owner_epoch == self.owner.epoch()
            && self.scope == execution.private_scope()
            && session
                .bindings()
                .get(self.id)
                .is_some_and(|entry| entry.scope == self.scope && self.write.matches(entry))
            && self
                .private_mount
                .as_ref()
                .is_none_or(|mount| std::ptr::eq(mount.execution.as_ref(), execution))
    }

    pub(super) fn matches_private_hidden_mount(
        &self,
        session: &PersistentSession,
        execution: &PrivateExecutionAdmission,
        id: tidepool_repr::SessionVarId,
    ) -> bool {
        self.id == id
            && self.matches_private_mount(session, execution)
            && self
                .private_mount
                .as_ref()
                .is_some_and(|mount| mount.kind == PendingHostKind::Hidden)
    }

    pub(super) fn matches_private_visible_mount(
        &self,
        session: &PersistentSession,
        execution: &PrivateExecutionAdmission,
        id: tidepool_repr::SessionVarId,
    ) -> bool {
        self.id == id
            && self.matches_private_mount(session, execution)
            && self
                .private_mount
                .as_ref()
                .is_some_and(|mount| mount.kind == PendingHostKind::Visible)
    }

    pub(super) fn private_hidden_mount_key(
        &self,
        session: &PersistentSession,
        execution: &Arc<PrivateExecutionAdmission>,
    ) -> Result<super::persistent::CompileViewKey, SessionError> {
        let mount = self
            .private_mount
            .as_ref()
            .filter(|mount| mount.kind == PendingHostKind::Hidden)
            .ok_or(SessionError::StaleStagedDeclaration)?;
        let original_source = execution.original_source()?;
        if !Arc::ptr_eq(&mount.execution, execution)
            || !self.matches_private_mount(session, execution)
            || !session
                .lib()
                .tips
                .get(&self.scope)
                .and_then(|tip| tip.inspection_world.as_ref())
                .is_some_and(|live| {
                    Arc::ptr_eq(&live.base.owner, session.admission_owner())
                        && live.base.epoch == session.admission_owner().epoch()
                        && live.base.source.sha256() == mount.after.base.source.sha256()
                        && live.base.original_source.matches(&original_source)
                        && live.base.target.matches(&execution.scope_lease)
                })
        {
            return Err(SessionError::StaleStagedDeclaration);
        }
        session
            .compile_view_key_in(self.scope)
            .ok_or(SessionError::StaleStagedDeclaration)
    }

    pub(super) fn has_private_visible_mount(&self) -> bool {
        self.private_mount
            .as_ref()
            .is_some_and(|mount| mount.kind == PendingHostKind::Visible)
    }

    /// Transfer an accepted effect's exact write to its existing private owner.
    /// Final publication revalidates the native identity under machine checkout.
    pub fn accept(&mut self, admission: &PrivateExecutionAdmission) -> Result<(), SessionError> {
        let mount = self
            .private_mount
            .as_ref()
            .filter(|mount| mount.kind == PendingHostKind::Visible)
            .ok_or(SessionError::StaleStagedDeclaration)?;
        if !Arc::ptr_eq(&self.owner, &admission.owner)
            || self.owner_epoch != admission.owner_epoch
            || self.owner_epoch != self.owner.epoch()
            || self.scope != admission.private_scope
            || !std::ptr::eq(mount.execution.as_ref(), admission)
        {
            return Err(SessionError::StaleStagedDeclaration);
        }

        let mut state = admission.state.lock();
        if state.phase != PrivateExecutionPhase::Open
            || state.pending_hosts.get(&self.id) != Some(&PendingHostKind::Visible)
            || admission.final_intent.get().is_some()
        {
            return Err(SessionError::StaleStagedDeclaration);
        }
        let accepted_revision = state
            .accepted_visible_revision
            .checked_add(1)
            .ok_or(SessionError::StaleStagedDeclaration)?;
        let mut completed = admission.completed_values.lock();
        if let Some(prior) = completed.get(&self.id) {
            if prior.name != self.write.name
                || prior.identity != self.write.identity
                || prior.generation != self.write.generation
                || prior.root_id != self.write.root_id
                || !prior.proof.same_original(&self.write.proof)
                || prior.visible_host.as_ref().is_none_or(|prior| {
                    !Arc::ptr_eq(&prior.before, &mount.before)
                        || !Arc::ptr_eq(&prior.after, &mount.after)
                })
            {
                return Err(SessionError::StaleStagedDeclaration);
            }
        } else {
            let mut write = self.write.clone();
            write.visible_host = Some(PrivateVisibleHostPublication {
                before: mount.before.clone(),
                after: mount.after.clone(),
            });
            completed.insert(self.id, write);
            state.unapplied_visible_hosts.insert(self.id);
            state.accepted_visible_revision = accepted_revision;
        }
        state.pending_hosts.remove(&self.id);
        drop(completed);
        drop(state);
        self.private_mount.take();
        Ok(())
    }
}

impl PrivateExecutionAdmission {
    pub fn checked_progress(&self) -> PrivateCheckedItemProgress {
        match &self.state.lock().authored {
            PrivateAuthoredProgram::Ready(frontier) => match frontier.publication() {
                PrivateCompilerProgress::Checked(_) => PrivateCheckedItemProgress::Completed,
                _ => PrivateCheckedItemProgress::NotStarted,
            },
            PrivateAuthoredProgram::Active { .. } | PrivateAuthoredProgram::Failed => {
                PrivateCheckedItemProgress::Started
            }
        }
    }

    pub(super) fn completed_frontier(
        &self,
        world: &Arc<tidepool_toolchain::compiler_world::CompilerWorld>,
    ) -> Result<CompletedPrivateFrontier<'_>, SessionError> {
        let state = self.state.lock();
        if !matches!(
            state.phase,
            PrivateExecutionPhase::Freezing | PrivateExecutionPhase::Frozen
        ) {
            return Err(super::SessionTransitionReason::WrongPhase.into());
        }
        match &state.authored {
            PrivateAuthoredProgram::Ready(frontier)
                if frontier.matches_world(world)
                    && state.accepted_visible_revision == state.applied_visible_revision
                    && state.pending_hosts.is_empty() =>
            {
                Ok(CompletedPrivateFrontier {
                    admission: self,
                    frontier: Arc::clone(frontier),
                    scope_lease: Arc::clone(&self.scope_lease),
                })
            }
            _ => Err(SessionError::WorldCellPublicationIncomplete),
        }
    }

    pub(super) fn begin_freeze(&self) -> Result<Option<PrivateFreezeTransition<'_>>, SessionError> {
        let mut state = self.state.lock();
        match state.phase {
            PrivateExecutionPhase::Frozen => {
                return self
                    .final_intent
                    .get()
                    .map(|_| None)
                    .ok_or(SessionError::StaleStagedDeclaration);
            }
            PrivateExecutionPhase::Freezing => {
                return Err(SessionError::StaleStagedDeclaration);
            }
            PrivateExecutionPhase::Open => {}
        }
        if !state.pending_hosts.is_empty()
            || !matches!(state.authored, PrivateAuthoredProgram::Ready(_))
        {
            tracing::debug!(target: "tidepool::session::private_host_mount",
                scope = self.private_scope.0, phase = ?state.phase, progress = ?state.authored,
                pending_hosts = state.pending_hosts.len(),
                accepted_revision = state.accepted_visible_revision,
                applied_revision = state.applied_visible_revision,
                "private freeze refused before reconciliation");
            return Err(SessionError::WorldCellPublicationIncomplete);
        }
        state.phase = PrivateExecutionPhase::Freezing;
        Ok(Some(PrivateFreezeTransition {
            admission: self,
            committed: false,
        }))
    }

    pub(super) fn ensure_open(&self) -> Result<(), SessionError> {
        let state = self.state.lock();
        if state.phase != PrivateExecutionPhase::Open {
            tracing::debug!(
                target: "tidepool::session::private_host_mount",
                progress = ?state.authored,
                phase = ?state.phase,
                "private execution open check refused after freeze"
            );
            Err(SessionError::StaleStagedDeclaration)
        } else {
            Ok(())
        }
    }

    pub(super) fn register_pending_host(
        &self,
        id: tidepool_repr::SessionVarId,
        kind: PendingHostKind,
    ) -> Result<(), SessionError> {
        let mut state = self.state.lock();
        if state.phase != PrivateExecutionPhase::Open || state.pending_hosts.contains_key(&id) {
            return Err(SessionError::StaleStagedDeclaration);
        }
        state.pending_hosts.insert(id, kind);
        Ok(())
    }

    pub(super) fn settle_pending_host(
        &self,
        id: tidepool_repr::SessionVarId,
        kind: PendingHostKind,
    ) -> Result<(), SessionError> {
        let mut state = self.state.lock();
        if state.pending_hosts.get(&id) == Some(&kind) {
            state.pending_hosts.remove(&id);
            Ok(())
        } else {
            Err(SessionError::StaleStagedDeclaration)
        }
    }

    /// Commit one exact private retirement while excluding freeze and
    /// acceptance for the whole transition. The binding, World association
    /// and pending-host ownership become observable together.
    pub(super) fn commit_pending_retirement(
        &self,
        id: tidepool_repr::SessionVarId,
        kind: PendingHostKind,
        commit: impl FnOnce() -> Result<(), SessionError>,
    ) -> Result<(), SessionError> {
        let mut state = self.state.lock();
        if state.phase != PrivateExecutionPhase::Open || state.pending_hosts.get(&id) != Some(&kind)
        {
            return Err(SessionError::StaleStagedDeclaration);
        }
        commit()?;
        state.pending_hosts.remove(&id);
        Ok(())
    }

    pub(super) fn claim_authored_program(
        &self,
        admission_digest: [u8; 32],
        snapshot: &PrivatePreparationSnapshot,
    ) -> Result<(), SessionError> {
        let mut state = self.state.lock();
        if state.phase != PrivateExecutionPhase::Open {
            return Err(SessionError::StaleStagedDeclaration);
        }
        if matches!(&state.authored, PrivateAuthoredProgram::Active { admission_digest: active, .. }
            if *active == admission_digest)
        {
            return Ok(());
        }
        execution_snapshot_is_ready(&state, snapshot)?;
        state.authored = PrivateAuthoredProgram::Active {
            admission_digest,
            settled: snapshot.frontier.clone(),
            predecessor: snapshot.world().clone(),
        };
        Ok(())
    }

    fn active_terminal(
        state: &PrivateExecutionState,
        admission_digest: [u8; 32],
    ) -> Result<&Arc<PrivateSettledFrontier>, SessionError> {
        match &state.authored {
            PrivateAuthoredProgram::Active {
                admission_digest: active,
                settled,
                ..
            } if state.phase == PrivateExecutionPhase::Open && *active == admission_digest => {
                Ok(settled)
            }
            _ => Err(SessionError::StaleStagedDeclaration),
        }
    }

    fn fail_authored_program(&self, admission_digest: [u8; 32]) {
        let mut state = self.state.lock();
        if matches!(&state.authored,
            PrivateAuthoredProgram::Active { admission_digest: active, .. }
                if *active == admission_digest)
        {
            state.authored = PrivateAuthoredProgram::Failed;
        }
    }

    pub(super) fn ensure_snapshot_current(
        &self,
        snapshot: &PrivatePreparationSnapshot,
    ) -> Result<(), SessionError> {
        execution_snapshot_is_ready(&self.state.lock(), snapshot)
    }

    pub(super) fn accepted_host_progress(&self) -> Option<Arc<PrivateHostBindingProgress>> {
        self.state.lock().accepted_host_progress.clone()
    }

    pub fn pending_host_count(&self) -> usize {
        self.state.lock().pending_hosts.len()
    }

    pub(super) fn current_visible_watermarks(&self) -> (u64, u64) {
        let state = self.state.lock();
        (
            state.accepted_visible_revision,
            state.applied_visible_revision,
        )
    }

    /// Share the exact admitted scope without minting a different selection.
    pub fn retained_lexical_scope(&self) -> Arc<RuntimeLexicalScopeLease> {
        self.scope_lease.clone()
    }

    /// The semantic public base captured when this execution began. A World
    /// execution keeps the admitted visibility snapshot even though checking
    /// consumes the issued world key and binding tip instead of a rebuilt view.
    pub fn admitted_public(&self) -> Result<&PublicVisibilitySnapshot, SessionError> {
        Ok(&self.compiler_basis.admitted)
    }
    pub fn public_scope(&self) -> ScopeId {
        self.public_scope
    }
    pub fn private_scope(&self) -> ScopeId {
        self.private_scope
    }
    pub fn session(&self) -> super::SessionId {
        self.session
    }
    pub fn view(&self) -> Result<&SessionCompileView, SessionError> {
        Err(SessionError::WorldCellRequiresServicePreparation)
    }
    pub(super) fn world_basis(
        &self,
    ) -> Result<
        (
            &Arc<tidepool_toolchain::compiler_world::CompilerWorld>,
            &super::persistent::CompileViewKey,
        ),
        SessionError,
    > {
        Ok((
            &self.compiler_basis.published.world,
            &self.compiler_basis.private_key,
        ))
    }

    // boundary: see CELL-PREPARATION-BOUNDARY.md
    /// The held compiler world this execution borrowed. Planning uses it to
    /// parse authored source; holding it confers no checked output authority.
    pub fn compiler_world(
        &self,
    ) -> Result<&Arc<tidepool_toolchain::compiler_world::CompilerWorld>, SessionError> {
        self.world_basis().map(|(world, _)| world)
    }

    // boundary: see CELL-PREPARATION-BOUNDARY.md
    /// Original source A captured with the published world. The private
    /// placement's lease is target B and stays separately reachable through
    /// [`Self::retained_lexical_scope`].
    pub fn original_source(&self) -> Result<Arc<RuntimeOriginalSource>, SessionError> {
        Ok(self.compiler_basis.original_source.clone())
    }

    /// The exact published association and actual source/public target custody
    /// captured by this execution's sole World birth owner.
    pub(super) fn world_execution_facts(&self) -> WorldExecutionAdmissionFacts {
        let basis = &self.compiler_basis;
        WorldExecutionAdmissionFacts {
            published: basis.published.clone(),
            source: basis.source.clone(),
            original_source: basis.original_source.clone(),
            public_target: basis.public_target.clone(),
            admitted: basis.admitted.clone(),
            admitted_context: basis.admitted_context.clone(),
        }
    }

    pub fn binding_tip(&self) -> BindingTipId {
        self.binding_tip
    }
    pub fn durable_owner(&self) -> Option<&super::RecoveryPublicOwner> {
        self.durable_owner.as_ref()
    }
}

/// One runtime-owned cell admission, retaining either finite compiler inputs
/// or a reservation from its held compiler world. Source leases and execution
/// purpose remain runtime authority; parser receipts supply expectations only.
/// Original declaration and value identities are burned before preparation.
pub struct RuntimeCellAdmission {
    owner: Arc<RuntimeAdmissionOwner>,
    owner_epoch: u64,
    session: super::SessionId,
    purpose: RuntimeCellPurpose,
    _retained_scope: Arc<RuntimeLexicalScopeLease>,
    prefix_started: std::sync::atomic::AtomicBool,
    scope: ScopeId,
    reserved_generations: Vec<Generation>,
    initial_value_generation: Generation,
    compiler_basis: RuntimeCellCompilerBasis,
    declarations: RuntimeCellDeclarations,
    digest: tidepool_toolchain::checked_cell::AdmissionDigest,
}

struct RuntimeCellDeclarations {
    root: PathBuf,
    declaration_baseline: Option<super::lexical_projection::DeclarationProjectionBaseline>,
    prepared_declarations: std::sync::OnceLock<
        Vec<(
            tidepool_toolchain::checked_cell::ExactCheckedItem,
            Arc<super::lexical_projection::PreparedAuthoredDeclaration>,
        )>,
    >,
}

enum RuntimeCellCompilerBasis {
    Finite(RuntimeFiniteCellInputs),
    World {
        reservation: Arc<tidepool_toolchain::cell_plan::CompilerWorldCellReservation>,
        _operand: Option<Arc<RuntimeWorldCellOperand>>,
        preparation: Option<Arc<PrivatePreparationSnapshot>>,
        private_key: super::persistent::CompileViewKey,
        source: Arc<super::GenerationArtifactReference>,
        /// Original A from the issuing source transaction; the cell's retained
        /// scope is the separately captured receiving target B.
        original_source: Arc<RuntimeOriginalSource>,
    },
}

/// Only the finite preparation route owns these compiler input projections.
/// A held-world admission never constructs or populates them.
pub(super) struct RuntimeFiniteCellInputs {
    visibility: PublicVisibilitySnapshot,
    catalog_selection: tidepool_toolchain::toolchain::CatalogSelection,
    compile_inputs: super::prepared::RuntimeCompileInputs,
    view: SessionCompileView,
    view_digest: [u8; 32],
    planned: Option<Arc<super::RuntimeCellPlanReservation>>,
    native_shares: Vec<SourceLeaseKey>,
    native_imports: Arc<AdmittedNativeImports>,
    interfaces: Vec<AdmittedValueInterface>,
    specification: Arc<dyn Any + Send + Sync>,
    specification_digest: [u8; 32],
    authority_digest: [u8; 32],
    include_paths: Vec<PathBuf>,
    /// The one issued request for this admission's exact recipe. A consumer
    /// can read it but cannot relabel it with another admission digest.
    checked_request: Option<Arc<tidepool_toolchain::checked_cell::CheckedCellRequest>>,
}

/// One runtime-issued execution purpose. Component controls can capture an
/// observation without making an executable cell admission.
enum RuntimeCellPurpose {
    #[cfg(test)]
    Observation,
    PrivateExecution(Arc<PrivateExecutionAdmission>),
    NativeSetup,
    HostCarrier {
        binding: String,
        expected: super::resident::HostBindingType,
        authority_digest: [u8; 32],
        private_preparation: Option<Arc<PrivateHostCarrierPreparation>>,
    },
}

/// Source custody for a reusable host type prototype, without runtime target custody.
pub(super) enum RuntimeHostPrototypeSource {
    Finite {
        _specification: Arc<dyn Any + Send + Sync>,
    },
    World {
        _original_source: Arc<RuntimeOriginalSource>,
        _reservation: Arc<tidepool_toolchain::cell_plan::CompilerWorldCellReservation>,
    },
}

pub(super) struct RuntimeHostPrototypeReceipt {
    pub(super) authority_digest: [u8; 32],
    pub(super) source: RuntimeHostPrototypeSource,
}

impl RuntimeCellPurpose {
    fn frame_authorization(&self, frame: &mut impl FnMut(&[u8])) {
        match self {
            #[cfg(test)]
            Self::Observation => {}
            Self::PrivateExecution(_) => frame(b"TidepoolPrivateExecutionAdmission1"),
            Self::NativeSetup => frame(b"TidepoolNativeSetupAdmission1"),
            Self::HostCarrier {
                binding,
                expected,
                authority_digest,
                ..
            } => {
                frame(b"TidepoolHostCarrierAdmission2");
                frame(authority_digest);
                frame(binding.as_bytes());
                expected.frame_authorization(frame);
            }
        }
    }
}

/// An admission lifetime exists before the lazy machine bootstrap and is
/// distinct from recoverable source-session IDs and per-store counters.
pub(super) struct RuntimeAdmissionOwner {
    identity: uuid::Uuid,
    epoch: std::sync::atomic::AtomicU64,
    source_issuance: parking_lot::RwLock<()>,
    retired: parking_lot::Mutex<Vec<RuntimeAdmissionRelease>>,
    lexical_leases: parking_lot::Mutex<BTreeMap<ScopeId, RuntimeLexicalScopeRecord>>,
    session_live: std::sync::atomic::AtomicBool,
    uncertain_scopes: std::sync::Weak<parking_lot::Mutex<RuntimeUnconfirmedSourceCustody>>,
}

struct RuntimeLexicalScopeRecord {
    owner_epoch: u64,
    lexical_live: bool,
    lease: Option<std::sync::Weak<RuntimeLexicalScopeLease>>,
    original_source: Option<std::sync::Weak<RuntimeOriginalSource>>,
    owns_source_admission: bool,
    source_admission: super::RuntimeSourceAdmission,
    // The actual canonical lease owns strong prerequisites. Keeping them
    // strong in the issuer's own record would form owner→lease→owner cycles.
    prerequisites: Vec<std::sync::Weak<RuntimeLexicalScopeLease>>,
    source_prerequisites: Vec<std::sync::Weak<RuntimeOriginalSource>>,
}

#[derive(Default)]
pub(super) struct RuntimeUnconfirmedSourceCustody {
    pub(super) originals: Vec<Arc<RuntimeOriginalSource>>,
    pub(super) targets: Vec<Arc<RuntimeLexicalScopeLease>>,
}

enum RuntimeAdmissionRelease {
    Lexical { scope: ScopeId, owner_epoch: u64 },
    OriginalSource { scope: ScopeId, owner_epoch: u64 },
}

/// Canonical original-source authority, independent of mutable lexical roots.
/// Its scope is a diagnostic identity and does not assert lexical liveness.
pub struct RuntimeOriginalSource {
    owner: Arc<RuntimeAdmissionOwner>,
    owner_epoch: u64,
    scope: ScopeId,
    source_admission: super::RuntimeSourceAdmission,
    prerequisites: Vec<Arc<RuntimeOriginalSource>>,
}

impl RuntimeOriginalSource {
    fn validate_local_source_issuer(&self) -> Result<(), super::SourceAdmissionError> {
        let canonical = self
            .owner
            .session_live
            .load(std::sync::atomic::Ordering::Acquire)
            && self.owner_epoch == self.owner.epoch()
            && self
                .owner
                .lexical_leases
                .lock()
                .get(&self.scope)
                .is_some_and(|record| {
                    record.owner_epoch == self.owner_epoch
                        && record.source_admission.same_owner(&self.source_admission)
                        && record
                            .original_source
                            .as_ref()
                            .and_then(std::sync::Weak::upgrade)
                            .is_some_and(|source| std::ptr::eq(source.as_ref(), self))
                });
        if canonical {
            Ok(())
        } else {
            Err(super::SourceAdmissionError::StaleSourceLease)
        }
    }

    pub fn validate_source_issuer(self: &Arc<Self>) -> Result<(), super::SourceAdmissionError> {
        Self::with_sources_and_targets([self.clone()], [], |_, _| Ok(()))
    }

    /// Validate both immutable source DAGs and actual lexical custody under one
    /// total order of issuer locks. Inner validators never acquire issuer locks.
    pub(crate) fn with_sources_and_targets<T>(
        originals: impl IntoIterator<Item = Arc<Self>>,
        targets: impl IntoIterator<Item = Arc<RuntimeLexicalScopeLease>>,
        action: impl FnOnce(
            &[Arc<Self>],
            &[Arc<RuntimeLexicalScopeLease>],
        ) -> Result<T, super::SourceAdmissionError>,
    ) -> Result<T, super::SourceAdmissionError> {
        fn source_dag(
            source: Arc<RuntimeOriginalSource>,
            sources: &mut Vec<Arc<RuntimeOriginalSource>>,
        ) {
            if sources.iter().any(|held| Arc::ptr_eq(held, &source)) {
                return;
            }
            for prerequisite in &source.prerequisites {
                source_dag(prerequisite.clone(), sources);
            }
            sources.push(source);
        }
        fn lexical_dag(
            lease: Arc<RuntimeLexicalScopeLease>,
            leases: &mut Vec<Arc<RuntimeLexicalScopeLease>>,
        ) {
            if leases.iter().any(|held| Arc::ptr_eq(held, &lease)) {
                return;
            }
            for prerequisite in &lease.prerequisites {
                lexical_dag(prerequisite.clone(), leases);
            }
            leases.push(lease);
        }
        let mut sources = Vec::new();
        for source in originals {
            source_dag(source, &mut sources);
        }
        let mut leases = Vec::new();
        for target in targets {
            lexical_dag(target, &mut leases);
        }
        for lease in &leases {
            source_dag(lease.original_source.clone(), &mut sources);
        }
        let mut owners = sources
            .iter()
            .map(|source| source.owner.clone())
            .chain(leases.iter().map(|lease| lease.owner.clone()))
            .collect::<Vec<_>>();
        owners.sort_unstable_by_key(|owner| owner.identity());
        owners.dedup_by(|a, b| Arc::ptr_eq(a, b));
        let _issuance = owners
            .iter()
            .map(|owner| owner.source_issuance.read())
            .collect::<Vec<_>>();
        let validate = || {
            for source in &sources {
                source.validate_local_source_issuer()?;
            }
            for lease in &leases {
                lease.validate_local_source_issuer()?;
            }
            Ok::<_, super::SourceAdmissionError>(())
        };
        validate()?;
        let result = action(&sources, &leases)?;
        validate()?;
        Ok(result)
    }

    pub fn retain_unconfirmed_source_custody(self: &Arc<Self>) {
        for prerequisite in &self.prerequisites {
            prerequisite.retain_unconfirmed_source_custody();
        }
        let Some(custody) = self.owner.uncertain_scopes.upgrade() else {
            return;
        };
        let _issuance = self.owner.source_issuance.read();
        let canonical = self
            .owner
            .session_live
            .load(std::sync::atomic::Ordering::Acquire)
            && self
                .owner
                .lexical_leases
                .lock()
                .get(&self.scope)
                .is_some_and(|record| {
                    record
                        .original_source
                        .as_ref()
                        .and_then(std::sync::Weak::upgrade)
                        .is_some_and(|source| Arc::ptr_eq(&source, self))
                });
        let mut held = custody.lock();
        if canonical
            && !held
                .originals
                .iter()
                .any(|source| Arc::ptr_eq(source, self))
        {
            held.originals.push(self.clone());
        }
    }

    pub fn with_open_target<T>(
        self: &Arc<Self>,
        target: &Arc<RuntimeLexicalScopeLease>,
        action: impl FnOnce() -> T,
    ) -> Result<T, super::SourceAdmissionError> {
        Self::with_sources_and_targets([self.clone()], [target.clone()], |sources, _| {
            super::RuntimeSourceAdmission::with_open_admissions(
                sources.iter().map(|source| source.source_admission()),
                action,
            )
        })
    }

    pub fn with_open_source_admission<T>(
        self: &Arc<Self>,
        action: impl FnOnce() -> T,
    ) -> Result<T, super::SourceAdmissionError> {
        Self::with_sources_and_targets([self.clone()], [], |sources, _| {
            super::RuntimeSourceAdmission::with_open_admissions(
                sources.iter().map(|source| source.source_admission()),
                action,
            )
        })
    }

    pub fn admit_source(
        self: &Arc<Self>,
    ) -> Result<super::RuntimeSourceLeaseClaim, super::SourceAdmissionError> {
        Self::with_sources_and_targets([self.clone()], [], |sources, targets| {
            super::RuntimeSourceLeaseClaim::admit(sources, targets)
        })
    }

    pub fn source_admission(&self) -> &super::RuntimeSourceAdmission {
        &self.source_admission
    }
    pub fn scope(&self) -> ScopeId {
        self.scope
    }
}

impl std::fmt::Debug for RuntimeOriginalSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RuntimeOriginalSource")
            .field("scope", &self.scope)
            .finish_non_exhaustive()
    }
}

impl Drop for RuntimeOriginalSource {
    fn drop(&mut self) {
        self.owner
            .retired
            .lock()
            .push(RuntimeAdmissionRelease::OriginalSource {
                scope: self.scope,
                owner_epoch: self.owner_epoch,
            });
    }
}

/// Synchronous exclusion with registrations. Unknown commit or unwinding
/// invalidates the preflighted epoch before the writer unlocks; only an actual
/// no-effects refusal preserves old authority. This never issues readiness.
pub(super) struct RuntimeSourceTransition<'a> {
    owner: &'a RuntimeAdmissionOwner,
    unknown_epoch: std::cell::Cell<Option<u64>>,
}
impl RuntimeSourceTransition<'_> {
    pub(super) fn advance_epoch(&self, next: u64) {
        self.owner
            .epoch
            .store(next, std::sync::atomic::Ordering::Release);
    }
    pub(super) fn refused(&self) {
        self.unknown_epoch.set(None);
    }
}
impl Drop for RuntimeSourceTransition<'_> {
    fn drop(&mut self) {
        if let Some(next) = self.unknown_epoch.get() {
            self.advance_epoch(next);
        }
    }
}

impl RuntimeAdmissionOwner {
    pub(super) fn identity(&self) -> uuid::Uuid {
        self.identity
    }
    pub(super) fn epoch(&self) -> u64 {
        self.epoch.load(std::sync::atomic::Ordering::Acquire)
    }
    pub(super) fn new(
        uncertain_scopes: &Arc<parking_lot::Mutex<RuntimeUnconfirmedSourceCustody>>,
    ) -> Self {
        Self {
            identity: uuid::Uuid::new_v4(),
            epoch: std::sync::atomic::AtomicU64::new(0),
            source_issuance: parking_lot::RwLock::new(()),
            retired: parking_lot::Mutex::new(Vec::new()),
            lexical_leases: parking_lot::Mutex::new(BTreeMap::new()),
            session_live: std::sync::atomic::AtomicBool::new(true),
            uncertain_scopes: Arc::downgrade(uncertain_scopes),
        }
    }
    pub(super) fn with_source_transition<T>(
        &self,
        action: impl FnOnce(&RuntimeSourceTransition<'_>) -> T,
    ) -> T {
        let _issuance = self.source_issuance.write();
        let transition = RuntimeSourceTransition {
            owner: self,
            unknown_epoch: std::cell::Cell::new(None),
        };
        action(&transition)
    }
    pub(super) fn with_source_owner_transfer<T>(
        &self,
        next: u64,
        action: impl FnOnce(&RuntimeSourceTransition<'_>) -> T,
    ) -> T {
        let _issuance = self.source_issuance.write();
        let transition = RuntimeSourceTransition {
            owner: self,
            unknown_epoch: std::cell::Cell::new(Some(next)),
        };
        action(&transition)
    }
    pub(super) fn seal_session(&self) {
        let _issuance = self.source_issuance.write();
        self.session_live
            .store(false, std::sync::atomic::Ordering::Release);
        self.lexical_leases.lock().clear();
    }
}

/// Shared exact lexical lifetime, issued by its existing runtime owner.
/// The detached scope retains the binding and native shares captured at mint.
pub struct RuntimeLexicalScopeLease {
    owner: Arc<RuntimeAdmissionOwner>,
    owner_epoch: u64,
    scope: ScopeId,
    original_source: Arc<RuntimeOriginalSource>,
    source_admission: super::RuntimeSourceAdmission,
    prerequisites: Vec<Arc<RuntimeLexicalScopeLease>>,
    private_lifecycle: OnceLock<std::sync::Weak<parking_lot::Mutex<PrivateExecutionState>>>,
}

#[derive(Clone, Copy)]
pub(super) enum CompilerWorldAdmission {
    Install,
    Observe,
}

impl RuntimeLexicalScopeLease {
    pub(super) fn with_compiler_world_admission<T>(
        &self,
        world: &Arc<tidepool_toolchain::compiler_world::CompilerWorld>,
        admission: CompilerWorldAdmission,
        action: impl FnOnce(bool) -> T,
    ) -> Result<T, SessionError> {
        let Some(private) = self.private_lifecycle.get() else {
            return Ok(action(true));
        };
        let private = private
            .upgrade()
            .ok_or(SessionError::StaleStagedDeclaration)?;
        let state = private.lock();
        if state.phase != PrivateExecutionPhase::Open {
            return Err(SessionError::StaleStagedDeclaration);
        }
        let install = match admission {
            CompilerWorldAdmission::Observe => false,
            CompilerWorldAdmission::Install => {
                let same_predecessor = match &state.authored {
                    PrivateAuthoredProgram::Ready(frontier) => frontier.matches_world(world),
                    PrivateAuthoredProgram::Active { predecessor, .. } => {
                        Arc::ptr_eq(predecessor, world)
                    }
                    PrivateAuthoredProgram::Failed => false,
                };
                if !same_predecessor {
                    return Err(SessionError::StaleStagedDeclaration);
                }
                true
            }
        };
        Ok(action(install))
    }

    pub fn original_source(&self) -> &Arc<RuntimeOriginalSource> {
        &self.original_source
    }
    fn validate_local_source_issuer(&self) -> Result<(), super::SourceAdmissionError> {
        if self.owner_epoch != self.owner.epoch() || !self.has_live_canonical_custody() {
            return Err(super::SourceAdmissionError::StaleSourceLease);
        }
        Ok(())
    }

    fn has_live_canonical_custody(&self) -> bool {
        self.owner
            .session_live
            .load(std::sync::atomic::Ordering::Acquire)
            && self
                .owner
                .lexical_leases
                .lock()
                .get(&self.scope)
                .is_some_and(|record| {
                    record.lexical_live
                        && record.source_admission.same_owner(&self.source_admission)
                        && record
                            .lease
                            .as_ref()
                            .and_then(std::sync::Weak::upgrade)
                            .is_some_and(|lease| std::ptr::eq(lease.as_ref(), self))
                })
    }

    pub fn validate_lexical_issuer(&self) -> Result<(), super::SourceAdmissionError> {
        let _issuance = self.owner.source_issuance.read();
        self.validate_local_source_issuer()?;
        drop(_issuance);
        for prerequisite in &self.prerequisites {
            prerequisite.validate_lexical_issuer()?;
        }
        Ok(())
    }

    pub fn prerequisites(&self) -> &[Arc<Self>] {
        &self.prerequisites
    }

    /// Exact leases form an immutable DAG. Custody is distinct from gate identity:
    /// two scopes sharing one gate still retain two native/binding roots.
    // Order: transaction holds / actor World accounting, then issuer read
    // guards sorted by UUID, then gate state. No guard crosses END or await.
    pub(super) fn with_source_leases<T>(
        roots: impl IntoIterator<Item = Arc<Self>>,
        action: impl FnOnce(&[Arc<Self>]) -> Result<T, super::SourceAdmissionError>,
    ) -> Result<T, super::SourceAdmissionError> {
        RuntimeOriginalSource::with_sources_and_targets([], roots, |_, leases| action(leases))
    }

    pub fn with_open_source_admission<T>(
        self: &Arc<Self>,
        action: impl FnOnce() -> T,
    ) -> Result<T, super::SourceAdmissionError> {
        RuntimeOriginalSource::with_sources_and_targets([], [self.clone()], |sources, _| {
            super::RuntimeSourceAdmission::with_open_admissions(
                sources.iter().map(|source| source.source_admission()),
                action,
            )
        })
    }

    pub fn admit_source(
        self: &Arc<Self>,
    ) -> Result<super::RuntimeSourceLeaseClaim, super::SourceAdmissionError> {
        RuntimeOriginalSource::with_sources_and_targets([], [self.clone()], |sources, leases| {
            super::RuntimeSourceLeaseClaim::admit(sources, leases)
        })
    }

    /// Retain each admitted exact scope in its own session until explicit scope
    /// retirement or teardown. A later epoch change removes authority, not an
    /// already earned uncertain native obligation.
    pub fn retain_unconfirmed_source_custody(self: &Arc<Self>) {
        for prerequisite in &self.prerequisites {
            prerequisite.retain_unconfirmed_source_custody();
        }
        let Some(custody) = self.owner.uncertain_scopes.upgrade() else {
            return;
        };
        let _issuance = self.owner.source_issuance.read();
        let mut held = custody.lock();
        if self.has_live_canonical_custody()
            && !held.targets.iter().any(|lease| Arc::ptr_eq(lease, self))
        {
            held.targets.push(self.clone());
        }
    }

    pub fn source_admission(&self) -> &super::RuntimeSourceAdmission {
        &self.source_admission
    }
    pub fn scope(&self) -> ScopeId {
        self.scope
    }
}
impl std::fmt::Debug for RuntimeLexicalScopeLease {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RuntimeLexicalScopeLease")
            .field("scope", &self.scope)
            .finish_non_exhaustive()
    }
}
/// Sole pre-reservation disposal right for a freshly issued notebook source.
/// Observing or cloning its lease does not grant this right.
#[must_use]
pub struct UnpublishedNotebookScope {
    lease: Option<Arc<RuntimeLexicalScopeLease>>,
}

/// Neutral custody after the actual publication owner has fenced this source.
/// Dropping this value cannot retire a possibly visible generation.
#[must_use]
pub struct ReservedNotebookScope {
    lease: Option<Arc<RuntimeLexicalScopeLease>>,
    attempt: u64,
}

impl UnpublishedNotebookScope {
    pub fn lease(&self) -> &Arc<RuntimeLexicalScopeLease> {
        self.lease
            .as_ref()
            .expect("unpublished notebook owns its lease")
    }

    pub fn into_reserved(
        mut self,
        fence: &super::RuntimeSourceFence,
    ) -> Result<ReservedNotebookScope, (Self, super::SourceAdmissionError)> {
        let attempt = match fence.reservation_for(self.lease()) {
            Ok(attempt) => attempt,
            Err(error) => return Err((self, error)),
        };
        Ok(ReservedNotebookScope {
            lease: self.lease.take(),
            attempt,
        })
    }
}

impl Drop for UnpublishedNotebookScope {
    fn drop(&mut self) {
        if let Some(lease) = self.lease.take() {
            // Cut off only the new unpublished source. Physical resources stay
            // behind actual compiler/continuation holds and the existing reaper.
            lease.source_admission.retire_scope(lease.scope);
            drop(lease);
        }
    }
}

impl ReservedNotebookScope {
    fn validate(
        &self,
        witness: &super::OwnSourceFenceReopened,
    ) -> Result<(), super::SourceAdmissionError> {
        let Some(lease) = &self.lease else {
            return Err(super::SourceAdmissionError::StaleFence);
        };
        if !witness.matches(lease.source_admission(), self.attempt) {
            return Err(super::SourceAdmissionError::StaleFence);
        }
        Ok(())
    }

    pub fn settle_published(
        &mut self,
        witness: &super::ConfirmedSourcePublication,
    ) -> Result<(), super::SourceAdmissionError> {
        let Some(lease) = &self.lease else {
            return Err(super::SourceAdmissionError::StaleFence);
        };
        if !witness.matches(lease.source_admission(), self.attempt) {
            return Err(super::SourceAdmissionError::StaleFence);
        }
        drop(self.lease.take());
        Ok(())
    }

    /// Cut off this own reopened source. Only the publication guard's known
    /// nonpublication branch may invoke this; the witness proves exact reopen,
    /// not the absence of publication or physical cleanup.
    pub fn close_reopened_source(
        &mut self,
        witness: &super::OwnSourceFenceReopened,
    ) -> Result<(), super::SourceAdmissionError> {
        self.validate(witness)?;
        witness.discard_unpublished()?;
        drop(self.lease.take());
        Ok(())
    }
}

static_assertions::assert_not_impl_any!(UnpublishedNotebookScope: Clone, Copy);
static_assertions::assert_not_impl_any!(ReservedNotebookScope: Clone, Copy);

impl Drop for RuntimeLexicalScopeLease {
    fn drop(&mut self) {
        self.owner
            .retired
            .lock()
            .push(RuntimeAdmissionRelease::Lexical {
                scope: self.scope,
                owner_epoch: self.owner_epoch,
            });
    }
}

/// Exact injected interface bytes captured by the runtime owner. These
/// snapshots travel with an admission rather than being re-read from a mutable
/// session include tree when the compiler offer runs off checkout.
#[derive(Clone, Debug)]
pub struct AdmittedValueInterface {
    snapshot: ValueInterfaceSnapshot,
    checked_artifact: Arc<tidepool_toolchain::checked_cell::CheckedValueArtifact>,
}

/// Byte identity used by the immutable interface sequence. It carries no authority.
#[derive(Clone, Debug)]
struct ValueInterfaceSnapshot {
    module: tidepool_repr::SessionModule,
    bytes: Arc<[u8]>,
}

impl AsRef<ValueInterfaceSnapshot> for AdmittedValueInterface {
    fn as_ref(&self) -> &ValueInterfaceSnapshot {
        &self.snapshot
    }
}

#[cfg(test)]
impl AsRef<ValueInterfaceSnapshot> for ValueInterfaceSnapshot {
    fn as_ref(&self) -> &Self {
        self
    }
}

/// The original native import ledger remains fixed for the whole cell.
/// Completed items add only their sealed compiler-owned native identities.
#[derive(Debug)]
struct AdmittedNativeImports {
    entries: Vec<(SymbolIdentity, u64)>,
    owners: std::collections::BTreeMap<(SymbolIdentity, u64), u64>,
    commitment: [u8; 32],
}

/// A checked cell borrows its immutable admission ledger and indexes only
/// native owners added by its own settled items. Snapshots retain the ledger
/// independently; this mutable suffix remains under the prefix state lock.
#[derive(Debug)]
struct CheckedNativeIndex {
    base: Arc<AdmittedNativeImports>,
    added: std::collections::BTreeMap<(SymbolIdentity, u64), u64>,
}

impl CheckedNativeIndex {
    fn new(base: Arc<AdmittedNativeImports>) -> Self {
        Self {
            base,
            added: std::collections::BTreeMap::new(),
        }
    }

    fn get(&self, key: &(SymbolIdentity, u64)) -> Option<&u64> {
        self.added.get(key).or_else(|| self.base.owners.get(key))
    }

    fn extend(&mut self, added: Vec<((SymbolIdentity, u64), u64)>) {
        // append has already checked every identity, generation, and root.
        // Existing owners are never replaced, including package generation 0.
        for (key, root) in added {
            debug_assert!(self.get(&key).is_none());
            self.added.insert(key, root);
        }
    }
}

#[derive(Clone, Debug)]
struct CheckedNativeImports {
    base: Arc<AdmittedNativeImports>,
    tail: Option<Arc<CheckedNativeDelta>>,
    commitment: [u8; 32],
}

#[derive(Debug)]
struct CheckedNativeDelta {
    previous: Option<Arc<CheckedNativeDelta>>,
    imports: Vec<SettledNativeImport>,
    bindings: Vec<SettledNativeBinding>,
}

impl Drop for CheckedNativeDelta {
    fn drop(&mut self) {
        let mut previous = self.previous.take();
        while let Some(parent) = previous {
            let Some(mut delta) = Arc::into_inner(parent) else {
                break;
            };
            previous = delta.previous.take();
        }
    }
}

#[derive(Debug)]
struct SettledNativeImport {
    identity: SymbolIdentity,
    generation: u64,
    root_id: u64,
}

#[derive(Debug)]
struct SettledNativeBinding {
    name: String,
    id: tidepool_repr::SessionVarId,
    import: SettledNativeImport,
}

#[derive(Debug, Default)]
struct CapturedNativeDelta {
    imports: Vec<SettledNativeImport>,
    bindings: Vec<SettledNativeBinding>,
}

impl CheckedNativeImports {
    fn base(base: Arc<AdmittedNativeImports>) -> Self {
        Self {
            commitment: base.commitment,
            base,
            tail: None,
        }
    }

    fn deltas(&self) -> impl Iterator<Item = &CheckedNativeDelta> {
        let mut suffix = Vec::new();
        let mut node = self.tail.as_deref();
        while let Some(delta) = node {
            suffix.push(delta);
            node = delta.previous.as_deref();
        }
        suffix.into_iter().rev()
    }

    fn imports(&self) -> impl Iterator<Item = (&SymbolIdentity, u64)> {
        self.base
            .entries
            .iter()
            .map(|(identity, generation)| (identity, *generation))
            .chain(
                self.deltas()
                    .flat_map(|delta| delta.imports.iter())
                    .map(|import| (&import.identity, import.generation)),
            )
    }

    fn append(
        &self,
        delta: CapturedNativeDelta,
        index: &CheckedNativeIndex,
    ) -> Result<(Self, Vec<((SymbolIdentity, u64), u64)>), SessionError> {
        let mut added = Vec::new();
        let mut imports = Vec::new();
        for import in delta.imports {
            let key = (import.identity.clone(), import.generation);
            match index.get(&key) {
                Some(root) if *root != import.root_id => {
                    return Err(SessionError::StaleStagedDeclaration)
                }
                Some(_) => {}
                None => {
                    added.push((key, import.root_id));
                    imports.push(import);
                }
            }
        }
        if imports.is_empty() && delta.bindings.is_empty() {
            return Ok((self.clone(), added));
        }
        let mut hash = blake3::Hasher::new();
        hash.update(b"TidepoolSettledNativeDelta1");
        hash.update(&self.commitment);
        hash.update(&(imports.len() as u64).to_le_bytes());
        for import in &imports {
            frame_native_identity(&mut hash, &import.identity);
            hash.update(&import.generation.to_le_bytes());
            hash.update(&import.root_id.to_le_bytes());
        }
        hash.update(&(delta.bindings.len() as u64).to_le_bytes());
        for binding in &delta.bindings {
            hash.update(&(binding.name.len() as u64).to_le_bytes());
            hash.update(binding.name.as_bytes());
            hash.update(&binding.id.raw().to_le_bytes());
            frame_native_identity(&mut hash, &binding.import.identity);
            hash.update(&binding.import.generation.to_le_bytes());
            hash.update(&binding.import.root_id.to_le_bytes());
        }
        Ok((
            Self {
                base: self.base.clone(),
                tail: Some(Arc::new(CheckedNativeDelta {
                    previous: self.tail.clone(),
                    imports,
                    bindings: delta.bindings,
                })),
                commitment: *hash.finalize().as_bytes(),
            },
            added,
        ))
    }
}

fn frame_native_identity(hash: &mut blake3::Hasher, identity: &SymbolIdentity) {
    for part in [
        identity.unit.as_bytes(),
        identity.module.as_bytes(),
        identity.namespace.as_bytes(),
        identity.occurrence.as_bytes(),
    ] {
        hash.update(&(part.len() as u64).to_le_bytes());
        hash.update(part);
    }
    match &identity.record_parent {
        None => {
            hash.update(&[0]);
        }
        Some(parent) => {
            hash.update(&[1]);
            hash.update(&(parent.len() as u64).to_le_bytes());
            hash.update(parent.as_bytes());
        }
    }
}

/// Interfaces retain compiler-owned byte Arcs. Each successful item appends
/// one immutable delta; its commitment binds ordered exact module membership
/// without copying or rehashing the preceding interfaces.
#[derive(Clone, Debug)]
struct CheckedInterfaces<I = AdmittedValueInterface> {
    base: Arc<[I]>,
    tail: Option<Arc<CheckedInterfaceDelta<I>>>,
    commitment: [u8; 32],
    #[cfg(test)]
    bytes_hashed: Arc<std::sync::atomic::AtomicUsize>,
}
#[derive(Debug)]
struct CheckedInterfaceDelta<I = AdmittedValueInterface> {
    previous: Option<Arc<CheckedInterfaceDelta<I>>>,
    interface: I,
}
impl<I> Drop for CheckedInterfaceDelta<I> {
    fn drop(&mut self) {
        let mut previous = self.previous.take();
        while let Some(parent) = previous {
            let Some(mut delta) = Arc::into_inner(parent) else {
                break;
            };
            previous = delta.previous.take();
        }
    }
}
impl<I: Clone + AsRef<ValueInterfaceSnapshot>> CheckedInterfaces<I> {
    fn base(interfaces: &[I]) -> (Self, std::collections::BTreeMap<u64, [u8; 32]>) {
        let mut index = std::collections::BTreeMap::new();
        let mut commitment = *blake3::hash(b"TidepoolCheckedInterfaces1").as_bytes();
        for interface in interfaces {
            let interface = interface.as_ref();
            let digest = *blake3::hash(&interface.bytes).as_bytes();
            index.insert(interface.module.gen.0, digest);
            commitment = Self::extend_commitment(commitment, interface.module, digest);
        }
        (
            Self {
                base: Arc::from(interfaces),
                tail: None,
                commitment,
                #[cfg(test)]
                bytes_hashed: Arc::new(std::sync::atomic::AtomicUsize::new(
                    interfaces
                        .iter()
                        .map(|interface| interface.as_ref().bytes.len())
                        .sum(),
                )),
            },
            index,
        )
    }
    fn interface_digest(&self, bytes: &[u8]) -> [u8; 32] {
        #[cfg(test)]
        self.bytes_hashed
            .fetch_add(bytes.len(), std::sync::atomic::Ordering::Relaxed);
        *blake3::hash(bytes).as_bytes()
    }
    fn extend_commitment(
        previous: [u8; 32],
        module: tidepool_repr::SessionModule,
        digest: [u8; 32],
    ) -> [u8; 32] {
        let mut hash = blake3::Hasher::new();
        hash.update(b"TidepoolCheckedInterfaceDelta1");
        hash.update(&previous);
        hash.update(&module.gen.0.to_le_bytes());
        hash.update(&digest);
        *hash.finalize().as_bytes()
    }
    fn append(&self, interface: I, digest: [u8; 32]) -> Self {
        Self {
            base: self.base.clone(),
            commitment: Self::extend_commitment(self.commitment, interface.as_ref().module, digest),
            #[cfg(test)]
            bytes_hashed: self.bytes_hashed.clone(),
            tail: Some(Arc::new(CheckedInterfaceDelta {
                previous: self.tail.clone(),
                interface,
            })),
        }
    }
    fn iter(&self) -> impl Iterator<Item = &I> {
        let mut suffix = Vec::new();
        let mut node = self.tail.as_deref();
        while let Some(delta) = node {
            suffix.push(&delta.interface);
            node = delta.previous.as_deref();
        }
        self.base.iter().chain(suffix.into_iter().rev())
    }
}

/// Only resident settlement can extend this same-cell execution prefix.
/// Snapshots remain immutable while compilation runs off checkout.
#[derive(Debug)]
pub struct RuntimeCheckedPrefix {
    settled: super::SettledCellProgram,
    admission: Arc<RuntimeCellAdmission>,
    first_item: tidepool_toolchain::checked_cell::ExactCheckedItem,
    program: Arc<tidepool_toolchain::checked_cell::CellProgram>,
    retained_inputs: Option<BoundRetainedInputs>,
    state: parking_lot::Mutex<RuntimeCheckedState>,
}

/// Runtime access is issued only after the stage's selected dependency
/// occurrence is paired with this lease's actual scoped checked value.
#[derive(Debug)]
struct BoundRetainedInput {
    terminal: tidepool_toolchain::compiler_world::SelectedDependencyTerminal,
    id: tidepool_repr::SessionVarId,
    defining_scope: ScopeId,
    handle: tidepool_codegen::prepared_program::PreparedHandle,
    row: Arc<tidepool_toolchain::checked_cell::CheckedValueArtifact>,
}

#[derive(Debug)]
struct BoundRetainedInputs {
    scope: Arc<RuntimeLexicalScopeLease>,
    stages: BTreeMap<usize, Vec<BoundRetainedInput>>,
}

impl BoundRetainedInputs {
    fn validate_item(
        &self,
        session: &PersistentSession,
        item: &tidepool_toolchain::checked_cell::ExactCheckedItem,
    ) -> Result<(), SessionError> {
        session.validate_lexical_scope_lease(self.scope.scope, &self.scope)?;
        let stage = item
            .world_selected_runtime_inputs()
            .ok_or(SessionError::StaleStagedDeclaration)?
            .stage_first_ordinal();
        let bound = self
            .stages
            .get(&stage)
            .ok_or(SessionError::StaleStagedDeclaration)?;
        if bound.is_empty() {
            return Ok(());
        }
        let reachable = session
            .bindings()
            .scope_reachable_binding_ids(session.scope_tree(), self.scope.scope)
            .into_iter()
            .collect::<std::collections::HashSet<_>>();
        for input in bound {
            let entry = session
                .bindings()
                .get(input.id)
                .filter(|entry| {
                    reachable.contains(&entry.id)
                        && entry.scope == input.defining_scope
                        && entry.value.identity == *input.terminal.identity()
                        && entry.module
                            == tidepool_repr::SessionModule::val(Generation(
                                input.terminal.generation(),
                            ))
                        && entry.value.handle == input.handle
                })
                .ok_or(SessionError::StaleStagedDeclaration)?;
            if session
                .prepared()
                .and_then(|engine| engine.prepared_handle_of(input.handle.raw()))
                != Some(input.handle)
                || session
                    .retained_checked_value_artifact(entry.module)
                    .is_none_or(|row| !Arc::ptr_eq(row, &input.row))
            {
                return Err(SessionError::StaleStagedDeclaration);
            }
        }
        Ok(())
    }
}

#[derive(Debug)]
struct RuntimeCheckedState {
    snapshot: Arc<RuntimeCheckedPrefixSnapshot>,
    in_flight: Option<Arc<tidepool_toolchain::checked_cell::ExactCompiledItem>>,
    reservation: Option<CheckedItemReservation>,
    interface_index: std::collections::BTreeMap<u64, [u8; 32]>,
    native_index: CheckedNativeIndex,
    compile_inputs: Option<super::prepared::RuntimeCompileInputs>,
}

impl Drop for RuntimeCheckedPrefix {
    fn drop(&mut self) {
        if let Some(private) = self.admission.private_execution() {
            private.fail_authored_program(self.admission.digest());
        }
    }
}

#[derive(Debug)]
struct CheckedItemReservation {
    item: tidepool_toolchain::checked_cell::ExactCheckedItem,
    generation: Generation,
    digest: [u8; 32],
}

/// Runtime-owned exact item and value identity reserved before compilation.
#[derive(Debug)]
pub struct RuntimeCheckedItemAdmission {
    prefix: Arc<RuntimeCheckedPrefix>,
    snapshot: Arc<RuntimeCheckedPrefixSnapshot>,
    item: tidepool_toolchain::checked_cell::ExactCheckedItem,
    generation: Generation,
    observation_name: Option<String>,
    digest: [u8; 32],
}

impl RuntimeCheckedItemAdmission {
    pub fn prefix(&self) -> &Arc<RuntimeCheckedPrefix> {
        &self.prefix
    }
    pub fn snapshot(&self) -> &Arc<RuntimeCheckedPrefixSnapshot> {
        &self.snapshot
    }
    pub fn item(&self) -> &tidepool_toolchain::checked_cell::ExactCheckedItem {
        &self.item
    }
    pub fn generation(&self) -> Generation {
        self.generation
    }
    pub fn observation_name(&self) -> Option<&str> {
        self.observation_name.as_deref()
    }
    pub fn digest(&self) -> [u8; 32] {
        self.digest
    }
}

#[derive(Clone, Debug)]
pub struct RuntimeCheckedPrefixSnapshot {
    basis: RuntimeCheckedPrefixBasis,
    session_root: PathBuf,
    visibility: PublicVisibilitySnapshot,
    interfaces: CheckedInterfaces,
    _retained_scope: Arc<RuntimeLexicalScopeLease>,
    native_shares: Vec<SourceLeaseKey>,
    native_imports: CheckedNativeImports,
    compiler_prefix: tidepool_toolchain::checked_cell::ExactCompiledPrefix,
    digest: [u8; 32],
}

#[derive(Clone)]
enum RuntimeCheckedPrefixBasis {
    Finite {
        view: SessionCompileView,
        digest: [u8; 32],
    },
    World {
        world: Arc<tidepool_toolchain::compiler_world::CompilerWorld>,
        key: super::persistent::CompileViewKey,
    },
}

impl std::fmt::Debug for RuntimeCheckedPrefixBasis {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Finite { digest, .. } => f.debug_tuple("Finite").field(digest).finish(),
            Self::World { world, .. } => f.debug_tuple("World").field(world.logical_id()).finish(),
        }
    }
}

impl RuntimeCheckedPrefixSnapshot {
    pub fn view(&self) -> Result<&SessionCompileView, SessionError> {
        match &self.basis {
            RuntimeCheckedPrefixBasis::Finite { view, .. } => Ok(view),
            RuntimeCheckedPrefixBasis::World { .. } => {
                Err(SessionError::WorldCellRequiresServicePreparation)
            }
        }
    }
    pub fn world(&self) -> Option<&Arc<tidepool_toolchain::compiler_world::CompilerWorld>> {
        match &self.basis {
            RuntimeCheckedPrefixBasis::World { world, .. } => Some(world),
            RuntimeCheckedPrefixBasis::Finite { .. } => None,
        }
    }
    pub fn session_root(&self) -> &std::path::Path {
        &self.session_root
    }
    fn is_current(&self, session: &PersistentSession, scope: ScopeId) -> bool {
        session.public_visibility_snapshot_in(scope).as_ref() == Some(&self.visibility)
            && match &self.basis {
                RuntimeCheckedPrefixBasis::Finite { digest, .. } => {
                    session.compile_view_digest_in(scope) == Some(*digest)
                }
                RuntimeCheckedPrefixBasis::World { key, .. } => {
                    session.compile_view_key_in(scope).as_ref() == Some(key)
                }
            }
    }
    fn is_current_predecessor(
        &self,
        admission: &RuntimeCellAdmission,
        session: &PersistentSession,
        scope: ScopeId,
    ) -> bool {
        let continuing = admission.private_execution().is_some_and(|private| {
            let state = private.state.lock();
            state.phase == PrivateExecutionPhase::Open
                && matches!(&state.authored, PrivateAuthoredProgram::Active { admission_digest, .. }
                    if *admission_digest == admission.digest())
        });
        if !continuing {
            return self.is_current(session, scope);
        }
        let RuntimeCheckedPrefixBasis::World { world, .. } = &self.basis else {
            return self.is_current(session, scope);
        };
        let (Ok(original), Ok(source)) = (
            admission.original_source(),
            admission.world_source_reference(),
        ) else {
            return false;
        };
        session
            .lib()
            .tips
            .get(&scope)
            .and_then(|tip| tip.inspection_world.as_ref())
            .is_some_and(|association| {
                Arc::ptr_eq(&association.world, world)
                    && session.inspection_world_base_is_current(
                        &association.base,
                        source,
                        &original,
                    )
                    && association.base.target.matches(&admission._retained_scope)
            })
    }

    pub fn interfaces(&self) -> impl Iterator<Item = &AdmittedValueInterface> {
        self.interfaces.iter()
    }
    pub fn native_shares(&self) -> &[SourceLeaseKey] {
        &self.native_shares
    }
    /// The original baseline remains available independently of settlements.
    pub fn admitted_retained_imports(&self) -> &[(SymbolIdentity, u64)] {
        &self.native_imports.base.entries
    }
    /// Only the admitted baseline and exports actually installed by this
    /// prefix's sealed native targets.
    pub fn actual_retained_imports(&self) -> impl Iterator<Item = (&SymbolIdentity, u64)> {
        self.native_imports.imports()
    }
    /// Actual same-prefix bindings in completion order, for selecting native
    /// lexical winners from the exact installed targets.
    pub fn settled_native_bindings(
        &self,
    ) -> impl Iterator<Item = (&str, &SymbolIdentity, u64, u64)> {
        self.native_imports
            .deltas()
            .flat_map(|delta| delta.bindings.iter())
            .map(|binding| {
                (
                    binding.name.as_str(),
                    &binding.import.identity,
                    binding.import.generation,
                    binding.id.raw(),
                )
            })
    }
    pub fn compiler_prefix(&self) -> &tidepool_toolchain::checked_cell::ExactCompiledPrefix {
        &self.compiler_prefix
    }
    pub fn digest(&self) -> [u8; 32] {
        self.digest
    }
}

impl RuntimeCheckedPrefix {
    pub(super) fn captured_source_origin(&self) -> &super::resident::CapturedSourceOrigin {
        self.settled.captured_source_origin()
    }

    pub fn cell_program(&self) -> &Arc<tidepool_toolchain::checked_cell::CellProgram> {
        &self.program
    }
    pub fn admission(&self) -> &Arc<RuntimeCellAdmission> {
        &self.admission
    }
    pub fn snapshot(&self) -> Arc<RuntimeCheckedPrefixSnapshot> {
        self.state.lock().snapshot.clone()
    }

    pub(crate) fn start(
        self: &Arc<Self>,
        session: &PersistentSession,
        scope: ScopeId,
        execution: Arc<tidepool_toolchain::checked_cell::ExactCompiledItem>,
    ) -> Result<Arc<CheckedTurnCompletion>, SessionError> {
        if let Some(private) = self.admission.private_execution() {
            private.ensure_open()?;
        }
        self.settled.checkpoint()?;
        session.validate_lexical_scope_lease(scope, &self.admission._retained_scope)?;
        if self.admission.world_reservation().is_some() {
            self.admission.original_source()?.validate_source_issuer()?;
        }
        let store = session.held_admission_store()?;
        let mut state = self.state.lock();
        let stale_reason = if self.admission.host_carrier().is_some() {
            Some("host carrier")
        } else if !self.admission.belongs_to(session) {
            Some("admission owner")
        } else if self.admission.scope != scope {
            Some("scope")
        } else if state.in_flight.is_some() {
            Some("in flight")
        } else if state.reservation.as_ref().is_none_or(|reservation| {
            reservation.item != *execution.item()
                || reservation.generation.0 != execution.generation()
        }) {
            Some("reservation")
        } else if {
            self.program
                .items()
                .get(execution.item().index())
                .and_then(|item| item.native())
                .is_none_or(|native| !Arc::ptr_eq(native, &execution))
        } {
            Some("execution")
        } else if execution.item().admission_digest() != self.admission.digest() {
            Some("admission digest")
        } else if !state
            .snapshot
            .is_current_predecessor(&self.admission, session, scope)
        {
            Some("snapshot")
        } else {
            None
        };
        if let Some(reason) = stale_reason {
            tracing::debug!(
                target: "tidepool::session::private_host_mount",
                %reason,
                "checked prefix start refused"
            );
            return Err(SessionError::StaleStagedDeclaration);
        }
        let reservation = state
            .reservation
            .as_ref()
            .expect("checked reservation preflighted");
        execution
            .validate_runtime_admission(reservation.digest, self.admission.digest())
            .map_err(|error| {
                tracing::debug!(
                    target: "tidepool::session::private_host_mount",
                    %error,
                    "checked execution runtime admission refused"
                );
                error
            })?;
        if let Some(retained) = &self.retained_inputs {
            retained.validate_item(session, execution.item())?;
        }
        execution
            .validate_required_native_imports(state.snapshot.actual_retained_imports())
            .map_err(|error| {
                tracing::debug!(
                    target: "tidepool::session::private_host_mount",
                    %error,
                    "checked execution native imports refused"
                );
                error
            })?;
        execution
            .validate_settled_native_bindings(state.snapshot.settled_native_bindings())
            .map_err(|error| {
                tracing::debug!(
                    target: "tidepool::session::private_host_mount",
                    %error,
                    "checked execution native bindings refused"
                );
                error
            })?;
        // Appending checks the compiler-owned same-cell identity and order.
        validate_private_value_overlay(
            self,
            session,
            &state.snapshot,
            execution.private_value_overlay_binders(),
        )
        .map_err(|error| {
            tracing::debug!(
                target: "tidepool::session::private_host_mount",
                %error,
                "checked private value overlay refused"
            );
            error
        })?;
        state.snapshot.compiler_prefix.append(execution.clone())?;
        let prepared = Arc::new(CheckedTurnCompletion {
            prefix: self.clone(),
            execution: execution.clone(),
            scope,
        });
        let admission = store
            .as_ref()
            .map(|store| store.begin_native_transition())
            .transpose()
            .map_err(|error| SessionError::RecoveryStore(Box::new(error)))?;
        self.admission.with_open_sources(|| {
            if !state
                .snapshot
                .is_current_predecessor(&self.admission, session, scope)
            {
                return Err(SessionError::StaleStagedDeclaration);
            }
            state.in_flight = Some(execution);
            Ok(())
        })??;
        super::complete_native_transition(admission)?;
        Ok(prepared)
    }
}

fn validate_private_value_overlay<'a>(
    prefix: &RuntimeCheckedPrefix,
    session: &PersistentSession,
    snapshot: &RuntimeCheckedPrefixSnapshot,
    names: impl Iterator<Item = &'a str>,
) -> Result<(), SessionError> {
    if prefix.admission.private_execution().is_none() {
        return Ok(());
    }
    let names = names.collect::<std::collections::BTreeSet<_>>();
    // A native Value winner can hide a plain declaration Value head while
    // retaining its qualified original. Type-family members require their
    // complete certified export policy and remain conservatively refused.
    if super::paired_publication::declaration_value_members(
        session.lib(),
        snapshot.visibility.declaration_tip,
    )?
    .iter()
    .any(|name| names.contains(name.as_str()))
    {
        return Err(SessionError::UnsupportedPrivateValueReplacement);
    }
    Ok(())
}

/// Travels with the existing resident continuation token. It records no
/// completed prefix while an effect is parked or an execution has failed.
#[derive(Debug)]
pub(crate) struct CheckedTurnCompletion {
    prefix: Arc<RuntimeCheckedPrefix>,
    execution: Arc<tidepool_toolchain::checked_cell::ExactCompiledItem>,
    scope: ScopeId,
}

impl CheckedTurnCompletion {
    /// Native installation changes runtime selection and custody, not the
    /// compiler prefix from which this exact item was prepared. Keep its
    /// association current before evaluation can suspend on a compiler query.
    pub(crate) fn install_native_turn_in(
        &self,
        session: &mut PersistentSession,
        scope: ScopeId,
        metadata: &tidepool_repr::DataConTable,
        resolved: super::persistent::ResolvedNativeTurn,
    ) -> Result<super::persistent::InstalledCertifiedTurn, super::resident::ResidentError> {
        self.install_native_in(session, scope, |session| {
            session.install_native_turn_in(scope, metadata, resolved)
        })
    }

    pub(crate) fn install_closed_turn_in(
        &self,
        session: &mut PersistentSession,
        scope: ScopeId,
        metadata: &tidepool_repr::DataConTable,
        plan: &super::prepared::ClosedInstantiationPlan,
    ) -> Result<super::persistent::InstalledCertifiedTurn, super::resident::ResidentError> {
        self.install_native_in(session, scope, |session| {
            session.install_closed_turn_in(
                scope,
                metadata,
                plan,
                self.prefix.captured_source_origin(),
            )
        })
    }

    fn install_native_in(
        &self,
        session: &mut PersistentSession,
        scope: ScopeId,
        install: impl FnOnce(
            &mut PersistentSession,
        ) -> Result<
            super::persistent::InstalledCertifiedTurn,
            super::prepared::PreparedRuntimeError,
        >,
    ) -> Result<super::persistent::InstalledCertifiedTurn, super::resident::ResidentError> {
        if let Some(private) = self.prefix.admission.private_execution() {
            private.ensure_open()?;
        }
        let association = {
            let state = self.prefix.state.lock();
            if !self.prefix.admission.belongs_to(session)
                || self.scope != scope
                || state
                    .in_flight
                    .as_ref()
                    .is_none_or(|execution| !Arc::ptr_eq(execution, &self.execution))
                || !state
                    .snapshot
                    .is_current_predecessor(&self.prefix.admission, session, scope)
            {
                return Err(SessionError::StaleStagedDeclaration.into());
            }
            match state.snapshot.world() {
                None => None,
                Some(predecessor) => {
                    let association = session
                        .lib()
                        .tips
                        .get(&scope)
                        .and_then(|tip| tip.inspection_world.as_ref())
                        .ok_or(SessionError::MissingPublishedCompilerWorld(scope))?;
                    let source = self.prefix.admission.world_source_reference()?;
                    let original = self.prefix.admission.original_source()?;
                    if !Arc::ptr_eq(&association.world, predecessor)
                        || !session.inspection_world_base_is_current(
                            &association.base,
                            source,
                            &original,
                        )
                    {
                        return Err(SessionError::StaleStagedDeclaration.into());
                    }
                    Some(association.clone())
                }
            }
        };
        // This owner earns and retains the actual source transaction claims.
        // They permit finishing an admitted installation across a later fence;
        // an outer gate lock here would recursively lock the same admissions.
        let installed = install(session)?;
        if let Some(association) = association {
            let refreshed = session.compile_view_key_in(scope).map(|key| {
                let mut base = association.base.clone();
                base.key = key;
                Arc::new(super::inspection::InspectionWorldAssociation {
                    base,
                    world: association.world.clone(),
                })
            });
            // Only native registration ran under this exclusive checkout.
            // Keep all original issuer observations; never refresh their epochs.
            // Witness exhaustion disables reuse without losing installed-product
            // ownership through a new fallible postcommit check.
            session
                .lib_mut()
                .tips
                .get_mut(&scope)
                .expect("checked installation preserves its admitted scope")
                .inspection_world = refreshed;
        }
        Ok(installed)
    }

    pub(crate) fn validates_private_overlay(
        &self,
        session: &PersistentSession,
        scope: ScopeId,
        binders: &[&super::BoundBinder],
    ) -> Result<bool, SessionError> {
        let Some(private) = self.prefix.admission.private_execution() else {
            return Ok(false);
        };
        if !self.prefix.admission.belongs_to(session)
            || private.private_scope != scope
            || self.scope != scope
            || self
                .prefix
                .state
                .lock()
                .in_flight
                .as_ref()
                .is_none_or(|execution| !Arc::ptr_eq(execution, &self.execution))
        {
            return Err(SessionError::StaleStagedDeclaration);
        }
        self.execution.validate_bound_binders(
            &binders
                .iter()
                .map(|binder| super::turn::encode_bound_binder_authority(binder))
                .collect::<Vec<_>>(),
        )?;
        Ok(true)
    }

    pub(crate) fn settle(&self, session: &mut PersistentSession) -> Result<(), SessionError> {
        if let Some(private) = self.prefix.admission.private_execution() {
            private.ensure_open()?;
        }
        session.validate_lexical_scope_lease(self.scope, &self.prefix.admission._retained_scope)?;
        if self.prefix.admission.world_reservation().is_some() {
            self.prefix
                .admission
                .original_source()?
                .validate_source_issuer()?;
        }
        let mut state = self.prefix.state.lock();
        if !self.prefix.admission.belongs_to(session)
            || state
                .in_flight
                .as_ref()
                .is_none_or(|execution| !Arc::ptr_eq(execution, &self.execution))
        {
            return Err(SessionError::StaleStagedDeclaration);
        }
        let world = self.prefix.admission.world_reservation().is_some();
        if world {
            let private = self
                .prefix
                .admission
                .private_execution()
                .ok_or(SessionError::StaleStagedDeclaration)?;
            if private.private_scope() != self.scope
                || self.execution.item().index() != state.snapshot.compiler_prefix.next_item()
            {
                return Err(SessionError::StaleStagedDeclaration);
            }
        }
        let compiler_prefix = state
            .snapshot
            .compiler_prefix
            .append(self.execution.clone())?;
        let native = session.capture_checked_native_delta(
            self.scope,
            self.execution.generation(),
            self.execution.bound_binder_identities(),
        )?;
        let completed_values = if let Some(private) = self.prefix.admission.private_execution() {
            let mut values = Vec::new();
            for name in self.execution.private_value_overlay_binders() {
                let entry = session
                    .resolve_in(self.scope, name)
                    .filter(|entry| {
                        entry.scope == self.scope
                            && entry.module.gen.0 == self.execution.generation()
                    })
                    .ok_or(SessionError::StaleStagedDeclaration)?;
                values.push((
                    entry.id,
                    CertifiedPrivateValueWrite {
                        name: name.to_owned(),
                        identity: entry.value.identity.clone(),
                        generation: entry.module.gen,
                        root_id: entry.value.handle.raw().0,
                        proof: CertifiedPrivateValueProof::Execution(self.execution.clone()),
                        visible_host: None,
                    },
                ));
            }
            Some((private, values))
        } else {
            None
        };
        let successor = if let Some(predecessor) = state.snapshot.world() {
            let interface = self.execution.value_interface_certificate();
            let mut bindings = Vec::with_capacity(native.bindings.len());
            for bound in &native.bindings {
                let entry = session
                    .bindings()
                    .get(bound.id)
                    .ok_or(SessionError::StaleStagedDeclaration)?;
                let row = interface
                    .as_ref()
                    .ok_or(SessionError::StaleStagedDeclaration)?;
                if entry.scope != self.scope
                    || entry.value.identity != bound.import.identity
                    || entry.module.gen.0 != bound.import.generation
                    || entry.value.handle.raw().0 != bound.import.root_id
                {
                    return Err(SessionError::StaleStagedDeclaration);
                }
                bindings.push(
                    tidepool_toolchain::compiler_world::CompilerWorldBinding::from_checked_value(
                        &bound.name,
                        &entry.value.identity,
                        entry.id,
                        entry.scope.0,
                        row.clone(),
                    ),
                );
            }
            Some(self.prefix.program.item_successor(
                predecessor,
                self.execution.item(),
                bindings,
            )?)
        } else {
            None
        };
        let predecessor_world = state.snapshot.world().map(|world| *world.logical_id());
        let bound_binding_ids = native
            .bindings
            .iter()
            .map(|binding| binding.id.raw())
            .collect::<Vec<_>>();
        let cell_complete = compiler_prefix.next_item() == self.prefix.program.items().len();
        let private = self.prefix.admission.private_execution();
        let mut lifecycle = private.map(|private| private.state.lock());
        let terminal = lifecycle
            .as_ref()
            .map(|lifecycle| -> Result<_, SessionError> {
                let frontier = PrivateExecutionAdmission::active_terminal(
                    lifecycle,
                    self.prefix.admission.digest(),
                )?;
                if cell_complete {
                    Ok(Some(
                        frontier.complete_program(
                            &self.prefix.program,
                            successor
                                .as_ref()
                                .ok_or(SessionError::StaleStagedDeclaration)?,
                        )?,
                    ))
                } else {
                    Ok(None)
                }
            })
            .transpose()?;
        settle_checked_snapshot(
            session,
            &mut state,
            self.scope,
            compiler_prefix,
            &self.prefix.admission,
            self.execution.value_interface_certificate(),
            Some(native),
            successor,
        )?;
        if let Some(lifecycle) = lifecycle.as_mut() {
            match terminal {
                Some(Some(frontier)) => {
                    lifecycle.authored = PrivateAuthoredProgram::Ready(frontier)
                }
                _ => {
                    if let PrivateAuthoredProgram::Active { predecessor, .. } =
                        &mut lifecycle.authored
                    {
                        *predecessor = state
                            .snapshot
                            .world()
                            .expect("World active successor")
                            .clone();
                    }
                }
            }
        }
        if let Some((private, values)) = completed_values {
            private.completed_values.lock().extend(values);
        }
        if let (
            Some(predecessor_world),
            Some(successor),
            tidepool_toolchain::checked_cell::CellProgramBasis::World(output),
        ) = (
            predecessor_world,
            state.snapshot.world(),
            self.prefix.program.basis(),
        ) {
            tracing::debug!(target: "exomonad_harness::timing",
                base_logical_world = ?output.logical().logical_id(),
                predecessor_world = ?predecessor_world,
                successor_world = ?successor.logical_id(),
                output_certificate = ?output.certificate_sha256(),
                item_ordinal = self.execution.item().index(),
                bound_binding_ids = ?bound_binding_ids,
                "checked World item successor installed");
        }
        state.in_flight = None;
        state.reservation = None;
        Ok(())
    }
}

fn settle_checked_snapshot(
    session: &mut PersistentSession,
    state: &mut RuntimeCheckedState,
    scope: ScopeId,
    compiler_prefix: tidepool_toolchain::checked_cell::ExactCompiledPrefix,
    admission: &RuntimeCellAdmission,
    interface: Option<Arc<tidepool_toolchain::checked_cell::CheckedValueArtifact>>,
    native_delta: Option<CapturedNativeDelta>,
    successor: Option<tidepool_toolchain::compiler_world::CheckedWorldSuccessor>,
) -> Result<(), SessionError> {
    let visibility = session
        .public_visibility_snapshot_in(scope)
        .ok_or(SessionError::DeadScope(scope))?;
    let mut interfaces = state.snapshot.interfaces.clone();
    let mut added_interface = None;
    if let Some(interface) = &interface {
        let module = interface.owner();
        let bytes = interface.bytes_owned();
        let digest = interfaces.interface_digest(bytes);
        match state.interface_index.get(&module.gen.0) {
            Some(existing) if existing != &digest => {
                return Err(SessionError::StaleStagedDeclaration)
            }
            Some(_) => {}
            None => {
                interfaces = interfaces.append(
                    AdmittedValueInterface::from_checked(interface.clone()),
                    digest,
                );
                added_interface = Some((module.gen.0, digest));
            }
        }
    }
    // The next snapshot owns the complete exact dependency closure before
    // the previous snapshot can drop. Outstanding compiler owners
    // keep their own old snapshot and lexical lease until they finish.
    // Settlement must retain the scope validated by this operation without
    // retiring it through housekeeping after its visible writes have committed.
    let retained = session.retain_current_lexical_scope(scope)?;
    let (native_imports, added_native) = match native_delta {
        Some(delta) => state
            .snapshot
            .native_imports
            .append(delta, &state.native_index)?,
        None => (state.snapshot.native_imports.clone(), Vec::new()),
    };
    if let Some(interface) = interface {
        session.retain_checked_value_interface(interface)?;
    }
    let snapshot = match (&state.snapshot.basis, successor.as_ref()) {
        (RuntimeCheckedPrefixBasis::World { .. }, Some(successor)) => {
            Arc::new(world_checked_snapshot(
                Arc::clone(successor.world()),
                session
                    .compile_view_key_in(scope)
                    .ok_or(SessionError::DeadScope(scope))?,
                state.snapshot.session_root.clone(),
                visibility,
                interfaces,
                compiler_prefix,
                admission.digest(),
                retained,
                native_imports,
            ))
        }
        (RuntimeCheckedPrefixBasis::Finite { .. }, None) => {
            let view = session
                .compile_view_in(scope)
                .ok_or(SessionError::DeadScope(scope))?
                .with_scoped_injection()
                .with_compile_inputs(
                    state
                        .compile_inputs
                        .as_ref()
                        .ok_or(SessionError::StaleStagedDeclaration)?,
                )?;
            let view_digest = session
                .compile_view_digest_in(scope)
                .ok_or(SessionError::DeadScope(scope))?;
            Arc::new(checked_snapshot(
                view,
                view_digest,
                visibility,
                interfaces,
                compiler_prefix,
                admission.digest(),
                retained,
                native_imports,
            ))
        }
        _ => return Err(SessionError::StaleStagedDeclaration),
    };
    if let (Some(predecessor), Some(successor)) = (state.snapshot.world(), successor.as_ref()) {
        session.adopt_checked_world_successor_in(scope, admission, predecessor, successor)?;
    }
    if let Some((generation, digest)) = added_interface {
        state.interface_index.insert(generation, digest);
    }
    state.native_index.extend(added_native);
    state.snapshot = snapshot;
    Ok(())
}

fn checked_snapshot(
    view: SessionCompileView,
    view_digest: [u8; 32],
    visibility: PublicVisibilitySnapshot,
    interfaces: CheckedInterfaces,
    compiler_prefix: tidepool_toolchain::checked_cell::ExactCompiledPrefix,
    admission: [u8; 32],
    retained_scope: Arc<RuntimeLexicalScopeLease>,
    native_imports: CheckedNativeImports,
) -> RuntimeCheckedPrefixSnapshot {
    let session_root = view.session_root().to_path_buf();
    checked_snapshot_with_basis(
        RuntimeCheckedPrefixBasis::Finite {
            view,
            digest: view_digest,
        },
        session_root,
        visibility,
        interfaces,
        compiler_prefix,
        admission,
        retained_scope,
        native_imports,
    )
}

fn checked_snapshot_with_basis(
    basis: RuntimeCheckedPrefixBasis,
    session_root: PathBuf,
    visibility: PublicVisibilitySnapshot,
    interfaces: CheckedInterfaces,
    compiler_prefix: tidepool_toolchain::checked_cell::ExactCompiledPrefix,
    admission: [u8; 32],
    retained_scope: Arc<RuntimeLexicalScopeLease>,
    native_imports: CheckedNativeImports,
) -> RuntimeCheckedPrefixSnapshot {
    let native_shares = visibility.source_instances.clone();
    let mut digest = blake3::Hasher::new();
    let mut frame = |bytes: &[u8]| {
        digest.update(&(bytes.len() as u64).to_le_bytes());
        digest.update(bytes);
    };
    frame(b"TidepoolRuntimeCheckedPrefix2");
    frame(&admission);
    frame(&(compiler_prefix.next_item() as u64).to_le_bytes());
    match &basis {
        RuntimeCheckedPrefixBasis::Finite { digest, .. } => {
            frame(b"finite");
            frame(digest);
        }
        RuntimeCheckedPrefixBasis::World { world, .. } => {
            frame(b"world");
            frame(world.logical_id());
        }
    }
    frame(&visibility.epoch.to_le_bytes());
    frame(&visibility.declaration_tip.0.to_le_bytes());
    match visibility.machine_incarnation {
        Some(incarnation) => {
            frame(&[1]);
            frame(&incarnation.0.to_le_bytes());
        }
        None => frame(&[0]),
    }
    for (name, id) in &visibility.bindings {
        frame(b"binding");
        frame(name.as_bytes());
        frame(&id.raw().to_le_bytes());
    }
    frame(b"interfaces");
    frame(&interfaces.commitment);
    frame(b"actual-native-imports");
    frame(&native_imports.commitment);
    for key in &native_shares {
        frame(b"native");
        frame(&key.instance.raw().to_le_bytes());
        frame(&key.value.0.to_le_bytes());
    }
    RuntimeCheckedPrefixSnapshot {
        session_root,
        basis,
        visibility,
        interfaces,
        _retained_scope: retained_scope,
        native_shares,
        native_imports,
        compiler_prefix,
        digest: *digest.finalize().as_bytes(),
    }
}

fn world_checked_snapshot(
    world: Arc<tidepool_toolchain::compiler_world::CompilerWorld>,
    key: super::persistent::CompileViewKey,
    session_root: PathBuf,
    visibility: PublicVisibilitySnapshot,
    interfaces: CheckedInterfaces,
    compiler_prefix: tidepool_toolchain::checked_cell::ExactCompiledPrefix,
    admission: [u8; 32],
    retained_scope: Arc<RuntimeLexicalScopeLease>,
    native_imports: CheckedNativeImports,
) -> RuntimeCheckedPrefixSnapshot {
    checked_snapshot_with_basis(
        RuntimeCheckedPrefixBasis::World { world, key },
        session_root,
        visibility,
        interfaces,
        compiler_prefix,
        admission,
        retained_scope,
        native_imports,
    )
}

impl AdmittedValueInterface {
    fn from_checked(
        checked_artifact: Arc<tidepool_toolchain::checked_cell::CheckedValueArtifact>,
    ) -> Self {
        Self {
            snapshot: ValueInterfaceSnapshot {
                module: checked_artifact.owner(),
                bytes: checked_artifact.bytes_owned().clone(),
            },
            checked_artifact,
        }
    }
    pub fn checked_artifact(&self) -> &Arc<tidepool_toolchain::checked_cell::CheckedValueArtifact> {
        &self.checked_artifact
    }
    pub fn module(&self) -> tidepool_repr::SessionModule {
        self.snapshot.module
    }
    pub fn bytes(&self) -> &[u8] {
        &self.snapshot.bytes
    }
    pub fn bytes_owned(&self) -> &Arc<[u8]> {
        &self.snapshot.bytes
    }
}

fn initial_interfaces_match<'a, I: AsRef<ValueInterfaceSnapshot>>(
    admitted: &[I],
    consumed: impl IntoIterator<Item = (&'a tidepool_repr::SessionModule, &'a Arc<[u8]>)>,
) -> bool {
    let mut inventory = consumed.into_iter();
    let mut seen = std::collections::BTreeSet::new();
    for interface in admitted {
        let interface = interface.as_ref();
        let Some((module, bytes)) = inventory.next() else {
            return false;
        };
        if !seen.insert(interface.module.module_name())
            || interface.module != *module
            || (!Arc::ptr_eq(&interface.bytes, bytes) && interface.bytes.as_ref() != bytes.as_ref())
        {
            return false;
        }
    }
    inventory.next().is_none()
}

impl std::fmt::Debug for RuntimeCellAdmission {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let mut debug = f.debug_struct("RuntimeCellAdmission");
        debug
            .field("session", &self.session)
            .field("scope", &self.scope)
            .field("reserved_generations", &self.reserved_generations)
            .field("digest", &self.digest);
        match &self.compiler_basis {
            RuntimeCellCompilerBasis::Finite(inputs) => {
                debug
                    .field("visibility", &inputs.visibility)
                    .field("finite_view", &inputs.view)
                    .field("native_shares", &inputs.native_shares);
            }
            RuntimeCellCompilerBasis::World {
                reservation,
                private_key,
                ..
            } => {
                debug
                    .field("world_reservation", reservation)
                    .field("private_declaration_tip", &private_key.generation());
            }
        }
        debug.finish_non_exhaustive()
    }
}

impl RuntimeCellAdmission {
    fn with_open_sources<T>(
        &self,
        action: impl FnOnce() -> T,
    ) -> Result<T, super::SourceAdmissionError> {
        let originals = match &self.compiler_basis {
            RuntimeCellCompilerBasis::World {
                original_source, ..
            } => Some(original_source.clone()),
            RuntimeCellCompilerBasis::Finite(_) => None,
        };
        RuntimeOriginalSource::with_sources_and_targets(
            originals,
            [self._retained_scope.clone()],
            |sources, _| {
                super::RuntimeSourceAdmission::with_open_admissions(
                    sources.iter().map(|source| source.source_admission()),
                    action,
                )
            },
        )
    }

    pub(super) fn world_source_reference(
        &self,
    ) -> Result<&Arc<super::GenerationArtifactReference>, SessionError> {
        match &self.compiler_basis {
            RuntimeCellCompilerBasis::World { source, .. } => Ok(source),
            RuntimeCellCompilerBasis::Finite(_) => {
                Err(SessionError::WorldCellRequiresServicePreparation)
            }
        }
    }

    pub(super) fn original_source(&self) -> Result<Arc<RuntimeOriginalSource>, SessionError> {
        match &self.compiler_basis {
            RuntimeCellCompilerBasis::World {
                original_source, ..
            } => Ok(original_source.clone()),
            RuntimeCellCompilerBasis::Finite(_) => {
                Err(SessionError::WorldCellRequiresServicePreparation)
            }
        }
    }

    pub(super) fn admit_compiler_preparation(
        &self,
        source: &super::RuntimeSourceCompilerTransaction,
    ) -> Result<(), super::SourceAdmissionError> {
        if let RuntimeCellCompilerBasis::World {
            original_source, ..
        } = &self.compiler_basis
        {
            return source.admit_original(original_source, Some(self.source_lease()));
        }
        source.admit(Some(self.source_lease()))
    }

    pub fn source_lease(&self) -> Arc<RuntimeLexicalScopeLease> {
        self._retained_scope.clone()
    }

    pub fn session(&self) -> super::SessionId {
        self.session
    }

    pub(super) fn finite_inputs(&self) -> Result<&RuntimeFiniteCellInputs, SessionError> {
        match &self.compiler_basis {
            RuntimeCellCompilerBasis::Finite(inputs) => Ok(inputs),
            RuntimeCellCompilerBasis::World { .. } => {
                Err(SessionError::WorldCellRequiresServicePreparation)
            }
        }
    }

    pub fn world_reservation(
        &self,
    ) -> Option<&Arc<tidepool_toolchain::cell_plan::CompilerWorldCellReservation>> {
        match &self.compiler_basis {
            RuntimeCellCompilerBasis::World { reservation, .. } => Some(reservation),
            RuntimeCellCompilerBasis::Finite(_) => None,
        }
    }

    pub fn catalog_selection(
        &self,
    ) -> Result<&tidepool_toolchain::toolchain::CatalogSelection, SessionError> {
        Ok(&self.finite_inputs()?.catalog_selection)
    }

    /// Issue cumulative lexical projections while the machine and prefix
    /// mutex are stowed. Only this admission's immutable checked items enter
    /// the prepared sequence consumed by declaration adoption.
    #[tracing::instrument(
        target = "exomonad_harness::timing",
        name = "cell_program.declaration_projections",
        skip_all,
        fields(inclusive = true)
    )]
    pub(super) fn prepare_declaration_projections(
        &self,
        cell: &Arc<tidepool_toolchain::checked_cell::ExactCheckedCell>,
        settlement: &mut dyn FnMut(crate::CompilerTransactionClose),
    ) -> Result<(), SessionError> {
        let mut baseline = self.declarations.declaration_baseline.clone();
        let mut prepared = Vec::new();
        for index in 0..cell.item_count() {
            let item = cell.item(index)?;
            if item.admission_digest() != self.digest() {
                return Err(SessionError::StaleStagedDeclaration);
            }
            let Some(evidence) = item.planned_declaration() else {
                continue;
            };
            let generation = match &self.compiler_basis {
                RuntimeCellCompilerBasis::Finite(finite) => match &finite.planned {
                    Some(plan) => plan
                        .items()
                        .get(index)
                        .and_then(|row| row.declaration_generation()),
                    None if index == 0 => self.reserved_generations.first().copied(),
                    None => None,
                },
                RuntimeCellCompilerBasis::World { reservation, .. } => {
                    use tidepool_toolchain::checked_cell::CheckedPlannedCellSlot as Slot;
                    match reservation.slots().get(index) {
                        Some(
                            Slot::Prologue { declaration } | Slot::Declaration { declaration },
                        ) => Some(Generation(*declaration)),
                        _ => None,
                    }
                }
            }
            .ok_or(SessionError::StaleStagedDeclaration)?;
            let _join = tracing::debug_span!(target: "exomonad_harness::timing",
                "cell_program.authenticated_declaration_join", inclusive = true,
                item_ordinal = index, generation = generation.0,
                baseline_export_count = baseline.as_ref().map_or(0, |prior| prior.exports.len()),
                introduced_export_count = evidence.introduced_exports().len(),
            )
            .entered();
            let includes = match &self.compiler_basis {
                RuntimeCellCompilerBasis::Finite(finite) => finite.include_paths.as_slice(),
                RuntimeCellCompilerBasis::World { .. } => item.include_paths(),
            };
            let projection = super::lexical_projection::prepare_authored_projection(
                baseline.as_ref(),
                generation,
                evidence.clone(),
                &[],
                includes,
                &self.declarations.root,
                settlement,
            )?;
            baseline = Some(projection.next_baseline());
            prepared.push((item, projection));
        }
        self.declarations
            .prepared_declarations
            .set(prepared)
            .map_err(|_| SessionError::StaleStagedDeclaration)
    }

    pub(super) fn retained_declaration_projections(
        &self,
    ) -> Result<Vec<Arc<tidepool_toolchain::declaration_join::AcceptedJoin>>, SessionError> {
        Ok(self
            .finite_inputs()?
            .compile_inputs
            .projections()
            .iter()
            .map(|projection| projection.receipt().clone())
            .collect())
    }

    pub(super) fn prepared_declaration(
        &self,
        item: &tidepool_toolchain::checked_cell::ExactCheckedItem,
    ) -> Result<Arc<super::lexical_projection::PreparedAuthoredDeclaration>, SessionError> {
        self.declarations
            .prepared_declarations
            .get()
            .and_then(|rows| rows.iter().find(|(prepared_item, _)| prepared_item == item))
            .map(|(_, prepared)| prepared.clone())
            .ok_or(SessionError::StaleStagedDeclaration)
    }

    pub fn private_execution(&self) -> Option<&Arc<PrivateExecutionAdmission>> {
        match &self.purpose {
            RuntimeCellPurpose::PrivateExecution(execution) => Some(execution),
            _ => None,
        }
    }

    pub(super) fn has_execution_purpose(&self) -> bool {
        match &self.purpose {
            #[cfg(test)]
            RuntimeCellPurpose::Observation => false,
            RuntimeCellPurpose::PrivateExecution(_)
            | RuntimeCellPurpose::NativeSetup
            | RuntimeCellPurpose::HostCarrier { .. } => true,
        }
    }
    pub(super) fn host_carrier(&self) -> Option<(&str, super::resident::HostBindingType)> {
        match &self.purpose {
            RuntimeCellPurpose::HostCarrier {
                binding, expected, ..
            } => Some((binding, *expected)),
            _ => None,
        }
    }
    pub(super) fn host_prototype_receipt(
        &self,
    ) -> Result<RuntimeHostPrototypeReceipt, SessionError> {
        let RuntimeCellPurpose::HostCarrier {
            authority_digest, ..
        } = &self.purpose
        else {
            return Err(SessionError::StaleStagedDeclaration);
        };
        let source = match &self.compiler_basis {
            RuntimeCellCompilerBasis::Finite(inputs) => RuntimeHostPrototypeSource::Finite {
                _specification: inputs.specification.clone(),
            },
            RuntimeCellCompilerBasis::World {
                original_source,
                reservation,
                ..
            } => RuntimeHostPrototypeSource::World {
                _original_source: original_source.clone(),
                _reservation: reservation.clone(),
            },
        };
        Ok(RuntimeHostPrototypeReceipt {
            authority_digest: *authority_digest,
            source,
        })
    }
    pub fn view(&self) -> Result<&SessionCompileView, SessionError> {
        Ok(&self.finite_inputs()?.view)
    }
    pub fn lexical_scope(&self) -> ScopeId {
        self.scope
    }
    pub fn visibility(&self) -> Result<&PublicVisibilitySnapshot, SessionError> {
        Ok(&self.finite_inputs()?.visibility)
    }
    pub fn reserved_generations(&self) -> &[Generation] {
        &self.reserved_generations
    }
    pub fn plan_reservation(
        &self,
    ) -> Result<Option<&Arc<super::RuntimeCellPlanReservation>>, SessionError> {
        Ok(self.finite_inputs()?.planned.as_ref())
    }
    pub fn initial_value_generation(&self) -> Generation {
        self.initial_value_generation
    }
    pub fn native_shares(&self) -> Result<&[SourceLeaseKey], SessionError> {
        Ok(&self.finite_inputs()?.native_shares)
    }
    pub fn admitted_retained_imports(&self) -> Result<&[(SymbolIdentity, u64)], SessionError> {
        Ok(&self.finite_inputs()?.native_imports.entries)
    }
    pub fn interfaces(&self) -> Result<&[AdmittedValueInterface], SessionError> {
        Ok(&self.finite_inputs()?.interfaces)
    }
    pub fn digest(&self) -> [u8; 32] {
        self.digest.bytes()
    }
    /// The issued checked-cell request for this admission's exact recipe.
    /// Issued once, before the admission was shared; consumers read it only.
    pub fn checked_request(
        &self,
    ) -> Result<&Arc<tidepool_toolchain::checked_cell::CheckedCellRequest>, SessionError> {
        self.finite_inputs()?
            .checked_request
            .as_ref()
            .ok_or(SessionError::StaleStagedDeclaration)
    }
    pub fn specification_digest(&self) -> Result<[u8; 32], SessionError> {
        Ok(self.finite_inputs()?.specification_digest)
    }
    pub fn authority_digest(&self) -> Result<[u8; 32], SessionError> {
        Ok(self.finite_inputs()?.authority_digest)
    }
    /// Ordered search inputs selected by the original specification owner.
    /// The source/tool capsule retains their lifetime; compiler evidence
    /// independently validates every consumed byte and resolution witness.
    pub fn include_paths(&self) -> Result<&[PathBuf], SessionError> {
        Ok(&self.finite_inputs()?.include_paths)
    }
    pub(super) fn belongs_to(&self, session: &PersistentSession) -> bool {
        Arc::ptr_eq(&self.owner, session.admission_owner())
            && self.owner_epoch == session.admission_owner().epoch()
            && session.has_lib()
            && self.session == session.lib().session_id()
    }
    pub fn specification(&self) -> Result<&Arc<dyn Any + Send + Sync>, SessionError> {
        Ok(&self.finite_inputs()?.specification)
    }
}

impl PersistentSession {
    fn held_admission_store(
        &self,
    ) -> Result<Option<super::recovery_store::RunRecoveryStore>, SessionError> {
        if !self.has_lib() {
            return Err(SessionError::MissingDeclarationLibrary);
        }
        self.lib().held_recovery_store()
    }

    // Tickets issued before the first native installation must name the same
    // machine lifetime that installation adopts, without allocating an engine.
    pub(super) fn reserve_admitted_machine_incarnation(
        &mut self,
        store: Option<&super::recovery_store::RunRecoveryStore>,
    ) -> Result<(), SessionError> {
        let admission = store
            .map(|store| store.begin_native_transition())
            .transpose()
            .map_err(|error| SessionError::RecoveryStore(Box::new(error)))?;
        self.ensure_machine_incarnation();
        super::complete_native_transition(admission)
    }

    pub(super) fn capture_value_interfaces(
        &self,
        view: &super::SessionCompileView,
    ) -> Result<Vec<AdmittedValueInterface>, SessionError> {
        tidepool_toolchain::timing::record_host_work(
            tidepool_toolchain::timing::HostWorkCounter::ValueInterfaceCapture,
            1,
        );
        view.reachable_values()
            .iter()
            .map(|module| {
                tidepool_toolchain::timing::record_host_work(
                    tidepool_toolchain::timing::HostWorkCounter::ReachableValueVisit,
                    1,
                );
                let artifact = self
                    .retained_checked_value_artifact(*module)
                    .ok_or(SessionError::MissingRetainedValueInterface(*module))?;
                Ok(AdmittedValueInterface::from_checked(artifact.clone()))
            })
            .collect()
    }
    pub fn prepare_private_host_carrier(
        &self,
        execution: Arc<PrivateExecutionAdmission>,
    ) -> Result<Arc<PrivateHostCarrierPreparation>, SessionError> {
        let association = self.current_private_world_association(&execution)?;
        let state = execution.state.lock();
        if state.phase != PrivateExecutionPhase::Open {
            return Err(SessionError::StaleStagedDeclaration);
        }
        let mode = match &state.authored {
            PrivateAuthoredProgram::Ready(frontier)
                if frontier.matches_world(&association.world) =>
            {
                PrivateCarrierMode::Ready(frontier.clone())
            }
            PrivateAuthoredProgram::Active {
                admission_digest,
                predecessor,
                ..
            } if Arc::ptr_eq(predecessor, &association.world) => PrivateCarrierMode::Active {
                admission_digest: *admission_digest,
            },
            _ => return Err(SessionError::WorldCellPublicationIncomplete),
        };
        drop(state);
        Ok(Arc::new(PrivateHostCarrierPreparation {
            execution,
            association,
            mode,
        }))
    }

    fn validate_private_host_carrier(
        &self,
        preparation: &PrivateHostCarrierPreparation,
    ) -> Result<(), SessionError> {
        let current = self.current_private_world_association(&preparation.execution)?;
        let state = preparation.execution.state.lock();
        let same_mode = match (&preparation.mode, &state.authored) {
            (PrivateCarrierMode::Ready(captured), PrivateAuthoredProgram::Ready(current)) => {
                Arc::ptr_eq(captured, current)
            }
            (
                PrivateCarrierMode::Active {
                    admission_digest: captured,
                },
                PrivateAuthoredProgram::Active {
                    admission_digest: current,
                    predecessor,
                    ..
                },
            ) => captured == current && Arc::ptr_eq(predecessor, preparation.world()),
            _ => false,
        };
        if state.phase != PrivateExecutionPhase::Open
            || !same_mode
            || !Arc::ptr_eq(&current.world, preparation.world())
        {
            return Err(SessionError::StaleStagedDeclaration);
        }
        Ok(())
    }

    pub(super) fn consume_host_carrier_reservation(
        &self,
        admission: &Arc<RuntimeCheckedItemAdmission>,
        execution: &Arc<tidepool_toolchain::checked_cell::ExactCompiledItem>,
        binding: &str,
        expected: super::resident::HostBindingType,
    ) -> Result<(), SessionError> {
        let store = self.held_admission_store()?;
        let prefix = admission.prefix();
        prefix.settled.checkpoint()?;
        if let RuntimeCellPurpose::HostCarrier {
            private_preparation: Some(preparation),
            ..
        } = &prefix.admission.purpose
        {
            self.validate_private_host_carrier(preparation)?;
        }
        let mut state = prefix.state.lock();
        let scope = prefix.admission.scope;
        if prefix.admission.host_carrier() != Some((binding, expected))
            || !prefix.admission.belongs_to(self)
            || !Arc::ptr_eq(&state.snapshot, admission.snapshot())
            || state.in_flight.is_some()
            || state.reservation.as_ref().is_none_or(|reservation| {
                reservation.item != *execution.item()
                    || reservation.generation.0 != execution.generation()
                    || reservation.digest != admission.digest()
            })
            || prefix.program.items().len() != 1
            || prefix.program.items()[0]
                .native()
                .is_none_or(|original| !Arc::ptr_eq(original, execution))
            || !state
                .snapshot
                .is_current_predecessor(&prefix.admission, self, scope)
        {
            return Err(SessionError::StaleStagedDeclaration);
        }
        execution.validate_runtime_admission(admission.digest(), prefix.admission.digest())?;
        if let Some(retained) = &prefix.retained_inputs {
            retained.validate_item(self, execution.item())?;
        }
        execution.validate_required_native_imports(state.snapshot.actual_retained_imports())?;
        execution.validate_settled_native_bindings(state.snapshot.settled_native_bindings())?;
        let admission = store
            .as_ref()
            .map(|store| store.begin_native_transition())
            .transpose()
            .map_err(|error| SessionError::RecoveryStore(Box::new(error)))?;
        // Mounting a host payload never executes or completes the placeholder.
        state.reservation = None;
        super::complete_native_transition(admission)?;
        Ok(())
    }

    /// Check exhaustion before the authoritative owner manifest rename.
    pub(crate) fn prepare_execution_admission_epoch_advance(&self) -> Result<u64, SessionError> {
        self.admission_owner()
            .epoch()
            .checked_add(1)
            .ok_or_else(|| SessionError::RecoveryManifest {
                path: self.lib().root.clone(),
                detail: "execution admission epoch exhausted".into(),
            })
    }

    /// Successful durable owner transfer fences every offer and publication
    /// issued before that transfer, while their scopes remain alive for abort.
    /// The next epoch was preflighted under this same exclusive checkout.
    #[cfg(test)]
    pub(crate) fn invalidate_execution_admissions_after_owner_transfer(&mut self, next: u64) {
        self.admission_owner()
            .with_source_transition(|transition| transition.advance_epoch(next));
    }

    pub fn admit_checked_item(
        &mut self,
        prefix: Arc<RuntimeCheckedPrefix>,
        item: tidepool_toolchain::checked_cell::ExactCheckedItem,
    ) -> Result<Arc<RuntimeCheckedItemAdmission>, SessionError> {
        prefix.settled.checkpoint()?;
        let scope = prefix.admission.scope;
        self.validate_lexical_scope_lease(scope, &prefix.admission._retained_scope)?;
        if prefix.admission.world_reservation().is_some() {
            prefix
                .admission
                .original_source()?
                .validate_source_issuer()?;
        }
        let store = self.held_admission_store()?;
        let mut state = prefix.state.lock();
        if !prefix.admission.belongs_to(self)
            || !item.same_cell(&prefix.first_item)
            || {
                prefix
                    .program
                    .items()
                    .get(item.index())
                    .is_none_or(|prepared| prepared.checked_item() != &item)
            }
            || item.index() != state.snapshot.compiler_prefix.next_item()
            || state.in_flight.is_some()
            || state.reservation.is_some()
            || !state
                .snapshot
                .is_current_predecessor(&prefix.admission, self, scope)
        {
            return Err(SessionError::StaleStagedDeclaration);
        }
        let snapshot = state.snapshot.clone();
        use tidepool_toolchain::checked_cell::{
            CheckedItemKind as CheckedKind, CheckedPlannedCellSlot as Slot,
        };
        let (generation, observation_name) =
            if let Some(reservation) = prefix.admission.world_reservation() {
                let slot = reservation
                    .slots()
                    .get(item.index())
                    .ok_or(SessionError::StaleStagedDeclaration)?;
                let (generation, observation) = match (slot, item.kind()) {
                    (
                        Slot::Prologue { declaration } | Slot::Declaration { declaration },
                        CheckedKind::Declaration,
                    ) => (*declaration, None),
                    (Slot::Bind { value }, CheckedKind::Bind) => (*value, None),
                    (
                        Slot::Expression {
                            capture,
                            observation_name,
                        },
                        CheckedKind::Expression,
                    ) => (*capture, Some(observation_name.clone())),
                    _ => return Err(SessionError::StaleStagedDeclaration),
                };
                (Generation(generation), observation)
            } else {
                let planned_row = prefix
                    .admission
                    .plan_reservation()?
                    .ok_or(SessionError::StaleStagedDeclaration)?
                    .items()
                    .get(item.index())
                    .ok_or(SessionError::StaleStagedDeclaration)?;
                use super::RuntimePlannedCellItemKind as Kind;
                if !matches!(
                    (planned_row.kind(), item.kind()),
                    (Kind::Prologue | Kind::Declaration, CheckedKind::Declaration)
                        | (Kind::Bind, CheckedKind::Bind)
                        | (Kind::Expression, CheckedKind::Expression)
                ) {
                    return Err(SessionError::StaleStagedDeclaration);
                }
                (
                    planned_row
                        .value_generation()
                        .or_else(|| planned_row.declaration_generation())
                        .ok_or(SessionError::StaleStagedDeclaration)?,
                    planned_row.observation_name().map(str::to_owned),
                )
            };
        let mut digest = blake3::Hasher::new();
        digest.update(b"TidepoolRuntimeCheckedItem1");
        digest.update(&snapshot.digest());
        digest.update(&(item.index() as u64).to_le_bytes());
        digest.update(&generation.0.to_le_bytes());
        match &observation_name {
            Some(name) => {
                digest.update(b"observation");
                digest.update(&(name.len() as u64).to_le_bytes());
                digest.update(name.as_bytes());
            }
            None => {
                digest.update(b"no-observation");
            }
        }
        let digest = *digest.finalize().as_bytes();
        let reservation = CheckedItemReservation {
            item: item.clone(),
            generation,
            digest,
        };
        let prepared = Arc::new(RuntimeCheckedItemAdmission {
            prefix: prefix.clone(),
            snapshot,
            item,
            generation,
            observation_name,
            digest,
        });
        let admission = store
            .as_ref()
            .map(|store| store.begin_native_transition())
            .transpose()
            .map_err(|error| SessionError::RecoveryStore(Box::new(error)))?;
        prefix.admission.with_open_sources(|| {
            if !state
                .snapshot
                .is_current_predecessor(&prefix.admission, self, scope)
            {
                return Err(SessionError::StaleStagedDeclaration);
            }
            if let Some(execution) = prefix.admission.private_execution() {
                let RuntimeCellCompilerBasis::World {
                    preparation: Some(preparation),
                    ..
                } = &prefix.admission.compiler_basis
                else {
                    return Err(SessionError::StaleStagedDeclaration);
                };
                execution.claim_authored_program(prefix.admission.digest(), preparation)?;
            }
            state.reservation = Some(reservation);
            Ok(())
        })??;
        drop(state);
        super::complete_native_transition(admission)?;
        Ok(prepared)
    }
    /// Adopt the compiler-owned original declaration from this same checked
    /// offer. Its reserved identity and bytes are never rendered or recompiled.
    pub fn adopt_checked_declaration(
        &mut self,
        admission: Arc<RuntimeCheckedItemAdmission>,
    ) -> Result<super::DeclarationPlaneCommit, SessionError> {
        // A lease dropped off checkout revokes its scope at the next entry,
        // before any declaration can commit against that scope's old view.
        self.reap_admission_leases();
        let prefix = &admission.prefix;
        prefix.settled.checkpoint()?;
        let mut state = prefix.state.lock();
        let scope = prefix.admission.scope;
        let item = &admission.item;
        if !prefix.admission.belongs_to(self)
            || !Arc::ptr_eq(&state.snapshot, &admission.snapshot)
            || state.in_flight.is_some()
            || state.reservation.as_ref().is_none_or(|reserved| {
                reserved.item != *item || reserved.generation != admission.generation
            })
            || !state
                .snapshot
                .is_current_predecessor(&prefix.admission, self, scope)
        {
            return Err(SessionError::StaleStagedDeclaration);
        }
        let certificate = item
            .planned_declaration()
            .ok_or(SessionError::StaleStagedDeclaration)?;
        let original_source = item
            .planned_declaration_source()
            .ok_or(SessionError::StaleStagedDeclaration)?;
        let generation = if let Some(reservation) = prefix.admission.world_reservation() {
            use tidepool_toolchain::checked_cell::CheckedPlannedCellSlot as Slot;
            match reservation.slots().get(item.index()) {
                Some(Slot::Prologue { declaration } | Slot::Declaration { declaration }) => {
                    Generation(*declaration)
                }
                _ => return Err(SessionError::StaleStagedDeclaration),
            }
        } else {
            prefix
                .admission
                .plan_reservation()?
                .ok_or(SessionError::StaleStagedDeclaration)?
                .items()
                .get(item.index())
                .and_then(|row| row.declaration_generation())
                .ok_or(SessionError::StaleStagedDeclaration)?
        };
        let prepared = prefix.admission.prepared_declaration(item)?;
        let module = tidepool_repr::SessionModule::lib(generation);
        if prepared.generation != generation
            || prepared.parent != self.lib().scope_tip(scope)
            || prepared.evidence.as_ref() != certificate.as_ref()
            || certificate.product().owner().unit != "main"
            || certificate.product().owner().module != module.module_name()
            || !self.lib().log.is_reserved(generation)
        {
            return Err(SessionError::StaleStagedDeclaration);
        }
        // Decode only the immutable observations owned by this exact item.
        // Caller-editable CellCheck fields never supply adoption authority.
        let checked = super::turn::decode_cell_out(item.cell_observations(), "", 0, "")?;
        let observation = checked
            .items
            .get(item.index())
            .ok_or(SessionError::StaleStagedDeclaration)?;
        if observation.verdict.kind != super::TurnKind::Decl || observation.source != item.source()
        {
            return Err(SessionError::StaleStagedDeclaration);
        }
        let receipt = super::DeclarationReceipt {
            source: super::DeclarationSource {
                prologue: checked.prologue,
                body: observation.source.clone(),
            },
            binders: observation.verdict.binders.clone(),
            items: observation.verdict.items.clone(),
        };
        let external = match state.snapshot.world() {
            Some(_) => self.lib().workbench_imports_in(scope),
            None => state.snapshot.view()?.persistent_imports().clone(),
        };
        let (external_imports, _, visible_values) =
            self.declaration_staging_context_in(scope, &receipt, &external)?;
        let base_tip = self.lib().scope_tip(scope);
        let turn = super::render::DeclTurn {
            normalized: receipt.source.clone(),
            sources: vec![receipt.source.replay_source(&external_imports)],
            external_imports,
            workbench_imports: receipt.source.prologue.workbench_imports(),
            items: receipt.items.clone(),
            value_types: std::collections::BTreeMap::new(),
            retracts: Vec::new(),
            parent: (base_tip.0 > 0).then_some(base_tip),
        };
        let staged = super::StagedDeclaration {
            generation,
            reserved: true,
            persistence: if prefix
                .admission
                .private_execution()
                .is_some_and(|private| private.durable_owner.is_some())
            {
                super::DeclarationPersistence::Durable
            } else {
                super::DeclarationPersistence::Ephemeral
            },
            module,
            receipt,
            exact_context: self.lib().current_exact_context_in(scope),
            session_id: self.lib().id,
            root: self.lib().root.clone(),
            scope,
            base_generation: generation,
            base_tip,
            turn,
            visible_values,
            rendered: super::render::RenderedModule {
                module,
                source: original_source.to_owned(),
                body_line: 0,
                hoisted_lines: false,
            },
            certified_authored: Some(prepared.clone()),
        };
        let compiler_prefix = state
            .snapshot
            .compiler_prefix
            .append_declaration_with_projection(item.clone(), prepared.projection.composition())?;
        let final_item = item.index() + 1 == prefix.program.items().len();
        let private = prefix.admission.private_execution();
        let mut lifecycle = private.map(|private| private.state.lock());
        let settled_frontier = lifecycle
            .as_ref()
            .map(|lifecycle| {
                PrivateExecutionAdmission::active_terminal(lifecycle, prefix.admission.digest())
                    .cloned()
            })
            .transpose()?;
        let commit = if let Some(predecessor) = state.snapshot.world().cloned() {
            let source = prefix.admission.world_source_reference()?.clone();
            let original_source = prefix.admission.original_source()?;
            original_source.validate_source_issuer()?;
            let target = prefix.admission._retained_scope.clone();
            self.validate_lexical_scope_lease(scope, &target)?;
            if self
                .lib()
                .tips
                .get(&scope)
                .and_then(|tip| tip.inspection_world.as_ref())
                .is_none_or(|association| !Arc::ptr_eq(&association.world, &predecessor))
            {
                return Err(SessionError::StaleStagedDeclaration);
            }
            let mut imports = self.lib().workbench_imports_in(scope);
            imports.extend(&staged.turn.workbench_imports);
            let import_lines = imports.source_lines();
            let prior = state.snapshot.clone();
            let interfaces = prior.interfaces.clone();
            let native_imports = prior.native_imports.clone();
            let retained = prior._retained_scope.clone();
            let owner = self.admission_owner().clone();
            let epoch = owner.epoch();
            // Direct stage observations are checked independently. Lifetime
            // roots come from the accepted join's full native body closure.
            let selected = item
                .world_selected_runtime_inputs()
                .ok_or(SessionError::StaleStagedDeclaration)?;
            if let Some(retained) = &prefix.retained_inputs {
                retained.validate_item(self, item)?;
            }
            for input in selected.inputs() {
                let entry = self
                    .bindings()
                    .get(input.session_var())
                    .filter(|entry| {
                        entry.value.identity == *input.symbol()
                            && entry.scope.0 == input.defining_scope()
                            && entry.module
                                == tidepool_repr::SessionModule::val(Generation(input.generation()))
                    })
                    .ok_or(SessionError::StaleStagedDeclaration)?;
                let handle = entry.value.handle;
                if self
                    .prepared()
                    .and_then(|engine| engine.prepared_handle_of(handle.raw()))
                    != Some(handle)
                {
                    return Err(SessionError::StaleStagedDeclaration);
                }
            }
            self.adopt_staged_declaration_with_world_activation(
                staged,
                &prior.visibility,
                |key, visibility, delta| {
                    let successor = prefix.program.declaration_item_successor(
                        &predecessor,
                        item,
                        prepared.projection.composition(),
                        import_lines,
                        delta
                            .iter()
                            .map(|(name, binding)| (name.as_str(), binding.as_ref().map(|binding| {
                                tidepool_toolchain::compiler_world::CompilerWorldBinding::from_checked_value(
                                    name, &binding.identity, binding.id, binding.defining_scope, binding.row.clone(),
                                )
                            }))),
                    )?;
                    let terminal = if final_item {
                        settled_frontier.as_ref().map(|frontier| {
                            frontier.complete_program(&prefix.program, &successor)
                        }).transpose()?
                    } else { None };
                    let association = Arc::new(super::inspection::InspectionWorldAssociation {
                        base: super::inspection::InspectionWorldBase {
                            scope,
                            key: key.clone(),
                            owner,
                            epoch,
                            source,
                            original_source:
                                super::inspection::InspectionWorldSourceObservation::new(
                                    &original_source,
                                ),
                            target: super::inspection::InspectionWorldLeaseObservation::new(
                                &target,
                            ),
                        },
                        world: Arc::clone(successor.world()),
                    });
                    let context = Arc::clone(successor.compiler_context());
                    let next = Arc::new(world_checked_snapshot(
                        Arc::clone(successor.world()),
                        key,
                        prior.session_root.clone(),
                        visibility,
                        interfaces,
                        compiler_prefix,
                        prefix.admission.digest(),
                        retained,
                        native_imports,
                    ));
                    Ok(move |lib: &mut super::SessionLib| {
                        let tip = lib.tips.get_mut(&scope).expect("prepared declaration scope");
                        tip.exact_context = Some(super::recovery::ScopedCompilerContext::new(context));
                        tip.inspection_world = Some(association);
                        state.snapshot = next;
                        state.reservation = None;
                        if let Some(lifecycle) = lifecycle.as_mut() {
                            if let Some(frontier) = terminal {
                                lifecycle.authored = PrivateAuthoredProgram::Ready(frontier);
                            } else if let PrivateAuthoredProgram::Active { predecessor, .. } = &mut lifecycle.authored {
                                *predecessor = state.snapshot.world().expect("World declaration successor").clone();
                            }
                        }
                    })
                },
            )?
        } else {
            let compile_inputs = state
                .compile_inputs
                .clone()
                .ok_or(SessionError::WorldCellRequiresServicePreparation)?;
            let interfaces = state.snapshot.interfaces.clone();
            let native_imports = state.snapshot.native_imports.clone();
            self.adopt_staged_declaration_with_activation(staged, |view, visibility, retained| {
                let view_digest = view.admission_commitment().0;
                let view = view
                    .with_scoped_injection()
                    .with_compile_inputs(&compile_inputs)
                    .map_err(SessionError::Compile)?;
                let next = Arc::new(checked_snapshot(
                    view,
                    view_digest,
                    visibility,
                    interfaces,
                    compiler_prefix,
                    prefix.admission.digest(),
                    retained,
                    native_imports,
                ));
                Ok(move |_: &mut super::SessionLib| {
                    state.snapshot = next;
                    state.reservation = None;
                })
            })?
        };
        Ok(commit)
    }

    pub fn retain_lexical_scope(
        &mut self,
        source: ScopeId,
    ) -> Result<Arc<RuntimeLexicalScopeLease>, SessionError> {
        self.reap_admission_leases();
        self.retain_current_lexical_scope(source)
    }

    /// Retain the current operation's scope without processing off-checkout
    /// retirements in the middle of a commit and its snapshot settlement.
    fn retain_current_lexical_scope(
        &mut self,
        source: ScopeId,
    ) -> Result<Arc<RuntimeLexicalScopeLease>, SessionError> {
        let scope = self
            .mint_detached_scope(source)
            .ok_or(SessionError::DeadScope(source))?;
        self.prepared_lexical_scope_lease(scope)
    }

    pub(super) fn mint_unpublished_notebook_scope(
        &mut self,
        prerequisite: Option<Arc<RuntimeLexicalScopeLease>>,
    ) -> Result<UnpublishedNotebookScope, SessionError> {
        if self.source_transaction.is_some() {
            return Err(super::SourceAdmissionError::ForeignTransaction.into());
        }
        let lease = self.mint_notebook_scope_with_lease(prerequisite)?;
        Ok(UnpublishedNotebookScope { lease: Some(lease) })
    }

    pub(super) fn prepared_lexical_scope_lease(
        &self,
        scope: ScopeId,
    ) -> Result<Arc<RuntimeLexicalScopeLease>, SessionError> {
        self.prepared_lexical_scope_lease_with_source(scope, None)
    }

    pub(super) fn prepared_lexical_scope_lease_with_source(
        &self,
        scope: ScopeId,
        prerequisite: Option<Arc<RuntimeLexicalScopeLease>>,
    ) -> Result<Arc<RuntimeLexicalScopeLease>, SessionError> {
        if !self.scope_tree().is_live(scope) {
            return Err(SessionError::DeadScope(scope));
        }
        if let Some(prerequisite) = &prerequisite {
            prerequisite.validate_lexical_issuer()?;
        }
        let mut leases = self.admission_owner().lexical_leases.lock();
        if let Some(existing) = leases.get(&scope).and_then(|record| record.lease.as_ref()) {
            let lease = existing.upgrade().ok_or(SessionError::DeadScope(scope))?;
            if lease.owner_epoch != self.admission_owner().epoch() {
                return Err(SessionError::WrongPublicManifestTicket);
            }
            drop(leases);
            lease.validate_lexical_issuer()?;
            return Ok(lease);
        }
        let owns_source_admission = leases
            .get(&scope)
            .is_none_or(|record| record.owns_source_admission);
        let prerequisites = match leases.get(&scope) {
            Some(record) => record
                .prerequisites
                .iter()
                .map(|lease| {
                    lease
                        .upgrade()
                        .ok_or(super::SourceAdmissionError::StaleSourceLease)
                })
                .collect::<Result<Vec<_>, _>>()?,
            None => prerequisite.into_iter().collect(),
        };
        let source_admission = leases
            .get(&scope)
            .map(|record| record.source_admission.clone())
            .unwrap_or_else(|| {
                super::RuntimeSourceAdmission::new(
                    scope,
                    prerequisites
                        .first()
                        .map(|lease| lease.source_admission().clone()),
                )
            });
        let mut source_prerequisites = match leases.get(&scope) {
            Some(record) => record
                .source_prerequisites
                .iter()
                .map(|source| {
                    source
                        .upgrade()
                        .ok_or(super::SourceAdmissionError::StaleSourceLease)
                })
                .collect::<Result<Vec<_>, _>>()?,
            None => prerequisites
                .iter()
                .map(|lease| lease.original_source.clone())
                .collect(),
        };
        if let Some(transaction) = &self.source_transaction {
            if !source_prerequisites
                .iter()
                .any(|source| Arc::ptr_eq(source, &transaction.source))
            {
                source_prerequisites.push(transaction.source.clone());
            }
        }
        let owner_epoch = self.admission_owner().epoch();
        let original_source = match leases
            .get(&scope)
            .and_then(|record| record.original_source.as_ref())
        {
            Some(source) => source
                .upgrade()
                .ok_or(super::SourceAdmissionError::StaleSourceLease)?,
            None => Arc::new(RuntimeOriginalSource {
                owner: self.admission_owner().clone(),
                owner_epoch,
                scope,
                source_admission: source_admission.clone(),
                prerequisites: source_prerequisites,
            }),
        };
        let lease = Arc::new(RuntimeLexicalScopeLease {
            owner: self.admission_owner().clone(),
            owner_epoch,
            scope,
            original_source,
            source_admission: source_admission.clone(),
            prerequisites,
            private_lifecycle: OnceLock::new(),
        });
        leases.insert(
            scope,
            RuntimeLexicalScopeRecord {
                owner_epoch,
                lexical_live: true,
                lease: Some(Arc::downgrade(&lease)),
                original_source: Some(Arc::downgrade(&lease.original_source)),
                owns_source_admission,
                prerequisites: lease.prerequisites.iter().map(Arc::downgrade).collect(),
                source_prerequisites: lease
                    .original_source
                    .prerequisites
                    .iter()
                    .map(Arc::downgrade)
                    .collect(),
                source_admission,
            },
        );
        drop(leases);
        lease.validate_lexical_issuer()?;
        Ok(lease)
    }

    pub(super) fn source_lease_in(
        &self,
        scope: ScopeId,
    ) -> Result<Arc<RuntimeLexicalScopeLease>, super::SourceAdmissionError> {
        if !self.scope_tree().is_live(scope) {
            return Err(super::SourceAdmissionError::MissingTargetLease { scope });
        }
        self.admission_owner()
            .lexical_leases
            .lock()
            .get(&scope)
            .and_then(|record| record.lease.as_ref())
            .and_then(std::sync::Weak::upgrade)
            .ok_or(super::SourceAdmissionError::MissingTargetLease { scope })
    }

    pub(super) fn prepared_notebook_scope_lease(
        &self,
        scope: ScopeId,
        prerequisite: Option<Arc<RuntimeLexicalScopeLease>>,
    ) -> Result<Arc<RuntimeLexicalScopeLease>, SessionError> {
        if let Some(prerequisite) = &prerequisite {
            prerequisite.validate_lexical_issuer()?;
        }
        // An unleased captured scope has custody provenance, but has not yet
        // selected its own executable world. Only initial notebook adoption
        // replaces that inherited admission with the actual issuer's gate.
        {
            let mut scopes = self.admission_owner().lexical_leases.lock();
            if scopes
                .get(&scope)
                .is_some_and(|record| record.lease.is_none())
            {
                scopes.remove(&scope);
            }
        }
        let lease = self.prepared_lexical_scope_lease_with_source(scope, prerequisite.clone())?;
        let expected = prerequisite.into_iter().collect::<Vec<_>>();
        if lease.prerequisites.len() != expected.len()
            || !lease
                .prerequisites
                .iter()
                .zip(expected.iter())
                .all(|(a, b)| Arc::ptr_eq(a, b))
        {
            return Err(super::SourceAdmissionError::StaleSourceLease.into());
        }
        Ok(lease)
    }

    pub(super) fn inherit_source_admission(&self, source: ScopeId, target: ScopeId) {
        let inherited = {
            let records = self.admission_owner().lexical_leases.lock();
            records.get(&source).map(|record| {
                let prerequisites = match &record.lease {
                    Some(lease) => vec![lease.clone()],
                    None => record.prerequisites.clone(),
                };
                (
                    record.source_admission.clone(),
                    prerequisites,
                    match &record.original_source {
                        Some(source) => vec![source.clone()],
                        None => record.source_prerequisites.clone(),
                    },
                )
            })
        }
        .or_else(|| {
            self.source_transaction.as_ref().map(|owner| {
                (
                    owner.source.source_admission().clone(),
                    Vec::new(),
                    vec![Arc::downgrade(&owner.source)],
                )
            })
        });
        if let Some((source_admission, prerequisites, source_prerequisites)) = inherited {
            self.admission_owner().lexical_leases.lock().insert(
                target,
                RuntimeLexicalScopeRecord {
                    owner_epoch: self.admission_owner().epoch(),
                    lexical_live: true,
                    lease: None,
                    original_source: None,
                    owns_source_admission: false,
                    source_admission,
                    prerequisites,
                    source_prerequisites,
                },
            );
        }
    }

    pub(super) fn retire_source_admission(&self, scope: ScopeId) {
        let record = self.admission_owner().lexical_leases.lock().remove(&scope);
        if let Some(record) = record {
            if record.owns_source_admission {
                record.source_admission.retire_scope(scope);
            }
        }
        // Drop leases outside the custody lock: lease Drop queues retirement.
        let released = {
            let mut held = self.uncertain_source_scopes.lock();
            let mut released = Vec::new();
            let mut index = 0;
            while index < held.targets.len() {
                if held.targets[index].scope == scope {
                    released.push(held.targets.swap_remove(index));
                } else {
                    index += 1;
                }
            }
            let mut original_released = Vec::new();
            let mut index = 0;
            while index < held.originals.len() {
                if held.originals[index].scope == scope {
                    original_released.push(held.originals.swap_remove(index));
                } else {
                    index += 1;
                }
            }
            (released, original_released)
        };
        drop(released);
    }

    pub(super) fn release_unheld_original_source_record(&self, scope: ScopeId) {
        let released = {
            let mut records = self.admission_owner().lexical_leases.lock();
            let unheld = records.get(&scope).is_some_and(|record| {
                record
                    .lease
                    .as_ref()
                    .is_none_or(|lease| lease.upgrade().is_none())
                    && record
                        .original_source
                        .as_ref()
                        .is_none_or(|source| source.upgrade().is_none())
            });
            unheld.then(|| records.remove(&scope)).flatten()
        };
        if let Some(record) = released {
            if record.owns_source_admission {
                record.source_admission.retire_scope(scope);
            }
        }
    }

    pub(super) fn invalidate_lexical_scope_custody(&self, scope: ScopeId) {
        if let Some(record) = self.admission_owner().lexical_leases.lock().get_mut(&scope) {
            record.lexical_live = false;
        }
    }
    /// Validate a retained placement without minting another scope or changing
    /// its custody. Owner transfer invalidates an unconsumed placement grant.
    pub fn validate_lexical_scope_lease(
        &self,
        scope: ScopeId,
        lease: &RuntimeLexicalScopeLease,
    ) -> Result<(), SessionError> {
        if !Arc::ptr_eq(&lease.owner, self.admission_owner())
            || lease.owner_epoch != self.admission_owner().epoch()
            || lease.scope != scope
        {
            return Err(SessionError::StaleStagedDeclaration);
        }
        if !self.scope_tree().is_live(scope) {
            return Err(SessionError::DeadScope(scope));
        }
        lease.validate_lexical_issuer()?;
        Ok(())
    }
    pub fn mint_scope_from_lease(
        &mut self,
        lease: &RuntimeLexicalScopeLease,
    ) -> Result<ScopeId, SessionError> {
        self.reap_admission_leases();
        self.validate_lexical_scope_lease(lease.scope, lease)?;
        self.mint_detached_scope(lease.scope)
            .ok_or(SessionError::DeadScope(lease.scope))
    }

    /// Check the first child bootstrap before that child can advance its own
    /// declarations. The opaque capture authenticates its owner; the exact
    /// tip authenticates the full inherited declaration selection, including
    /// instances, family declarations and retractions.
    pub fn validate_initial_lexical_scope(
        &self,
        lease: &RuntimeLexicalScopeLease,
        target: ScopeId,
    ) -> Result<(), SessionError> {
        self.validate_lexical_scope_lease(lease.scope, lease)?;
        if !self.scope_tree().is_live(target) {
            return Err(SessionError::DeadScope(target));
        }
        let captured = self
            .compile_view_in(lease.scope)
            .ok_or(SessionError::DeadScope(lease.scope))?;
        let derived = self
            .compile_view_in(target)
            .ok_or(SessionError::DeadScope(target))?;
        if captured.library() != derived.library()
            || captured.exact_declaration_context() != derived.exact_declaration_context()
        {
            return Err(SessionError::StaleStagedDeclaration);
        }
        Ok(())
    }

    pub fn begin_cell_program(
        &self,
        settled: super::SettledCellProgram,
    ) -> Result<Option<Arc<RuntimeCheckedPrefix>>, SessionError> {
        settled.checkpoint()?;
        let admission = settled.admission().clone();
        let program = settled.program().clone();
        if !admission.has_execution_purpose() {
            return Err(SessionError::StaleStagedDeclaration);
        }
        if let Some(private) = admission.private_execution() {
            private.ensure_open()?;
            if admission.world_reservation().is_none() {
                return Err(SessionError::WorldCellRequiresServicePreparation);
            }
        }
        if admission.world_reservation().is_some() {
            return self.begin_world_cell_program(admission, program, settled);
        }
        let finite = admission.finite_inputs()?;
        let planned = admission
            .plan_reservation()?
            .ok_or(SessionError::StaleStagedDeclaration)?;
        let program_plan = match program.basis() {
            tidepool_toolchain::checked_cell::CellProgramBasis::Finite(plan) => plan,
            tidepool_toolchain::checked_cell::CellProgramBasis::World(_) => {
                return Err(SessionError::StaleStagedDeclaration);
            }
        };
        program
            .validate_typed_entries()
            .map_err(SessionError::Compile)?;
        if program.admission_digest() != admission.digest()
            || !Arc::ptr_eq(program_plan, planned.plan())
            || program.slots() != planned.compiler_specification().slots.as_slice()
            || program.items().len() != planned.items().len()
            || program
                .items()
                .iter()
                .enumerate()
                .any(|(index, item)| item.checked_item().index() != index)
        {
            return Err(SessionError::StaleStagedDeclaration);
        }
        let Some(first) = program.items().first() else {
            if !admission.belongs_to(self)
                || self.public_visibility_snapshot_in(admission.scope).as_ref()
                    != Some(&finite.visibility)
                || self.compile_view_digest_in(admission.scope) != Some(finite.view_digest)
            {
                return Err(SessionError::StaleStagedDeclaration);
            }
            admission
                .prefix_started
                .compare_exchange(
                    false,
                    true,
                    std::sync::atomic::Ordering::AcqRel,
                    std::sync::atomic::Ordering::Acquire,
                )
                .map_err(|_| SessionError::StaleStagedDeclaration)?;
            return Ok(None);
        };
        self.begin_cell_program_prefix(admission, first.checked_item().clone(), program, settled)
            .map(Some)
    }

    fn begin_world_cell_program(
        &self,
        admission: Arc<RuntimeCellAdmission>,
        program: Arc<tidepool_toolchain::checked_cell::CellProgram>,
        settled: super::SettledCellProgram,
    ) -> Result<Option<Arc<RuntimeCheckedPrefix>>, SessionError> {
        use tidepool_toolchain::checked_cell::CellProgramBasis;
        let CellProgramBasis::World(checked) = program.basis() else {
            return Err(SessionError::StaleStagedDeclaration);
        };
        let RuntimeCellCompilerBasis::World {
            reservation,
            preparation,
            private_key,
            original_source,
            ..
        } = &admission.compiler_basis
        else {
            return Err(SessionError::StaleStagedDeclaration);
        };
        if let RuntimeCellPurpose::HostCarrier {
            private_preparation: Some(preparation),
            ..
        } = &admission.purpose
        {
            self.validate_private_host_carrier(preparation)?;
        }
        let execution = admission.private_execution();
        let preparation = match (execution, preparation.as_ref()) {
            (Some(execution), Some(snapshot)) if Arc::ptr_eq(&snapshot.execution, execution) => {
                execution.ensure_snapshot_current(snapshot)?;
                Some(snapshot)
            }
            (None, None) if admission.host_carrier().is_some() => None,
            _ => return Err(SessionError::StaleStagedDeclaration),
        };
        let scope = admission.scope;
        self.validate_lexical_scope_lease(scope, &admission._retained_scope)?;
        original_source.validate_source_issuer()?;
        program.validate_typed_entries()?;
        if !admission.belongs_to(self)
            || !Arc::ptr_eq(checked.reservation(), reservation)
            || program.admission_digest() != admission.digest()
            || program.slots() != reservation.slots()
            || program.items().len() != checked.items().len()
            || program.items().iter().enumerate().any(|(ordinal, item)| {
                item.checked_item().index() != ordinal
                    || item.checked_item().admission_digest() != admission.digest()
            })
            || self.compile_view_key_in(scope).as_ref() != Some(private_key)
            || preparation.is_some_and(|preparation| {
                self.lib()
                    .tips
                    .get(&scope)
                    .and_then(|tip| tip.inspection_world.as_ref())
                    .is_none_or(|association| !Arc::ptr_eq(association, preparation.association()))
            })
        {
            return Err(SessionError::StaleStagedDeclaration);
        }
        let Some(first) = program.items().first() else {
            admission.with_open_sources(|| {
                admission
                    .prefix_started
                    .compare_exchange(
                        false,
                        true,
                        std::sync::atomic::Ordering::AcqRel,
                        std::sync::atomic::Ordering::Acquire,
                    )
                    .map_err(|_| SessionError::StaleStagedDeclaration)?;
                if let (Some(execution), Some(preparation)) = (execution, preparation) {
                    let mut lifecycle = execution.state.lock();
                    execution_snapshot_is_ready(&lifecycle, preparation)?;
                    let frontier = preparation.frontier.complete_empty_program(&program)?;
                    lifecycle.authored = PrivateAuthoredProgram::Ready(frontier);
                }
                Ok::<(), SessionError>(())
            })??;
            return Ok(None);
        };
        let first_item = first.checked_item().clone();
        let baseline = first_item
            .baseline_checked_value_interfaces()
            .cloned()
            .map(AdmittedValueInterface::from_checked)
            .collect::<Vec<_>>();
        let (interfaces, interface_index) = CheckedInterfaces::base(&baseline);
        let retained_inputs =
            self.bind_world_retained_inputs(&program, Arc::clone(&admission._retained_scope))?;
        let native_imports =
            Arc::new(self.capture_world_native_imports(scope, &program, &retained_inputs)?);
        let snapshot = Arc::new(world_checked_snapshot(
            reservation.plan().world_owned().clone(),
            private_key.clone(),
            self.lib().root.clone(),
            self.public_visibility_snapshot_in(scope)
                .ok_or(SessionError::DeadScope(scope))?,
            interfaces,
            first_item.initial_prefix()?,
            admission.digest(),
            admission._retained_scope.clone(),
            CheckedNativeImports::base(native_imports.clone()),
        ));
        admission.with_open_sources(|| {
            if !snapshot.is_current(self, scope) {
                return Err(SessionError::StaleStagedDeclaration);
            }
            admission
                .prefix_started
                .compare_exchange(
                    false,
                    true,
                    std::sync::atomic::Ordering::AcqRel,
                    std::sync::atomic::Ordering::Acquire,
                )
                .map_err(|_| SessionError::StaleStagedDeclaration)?;
            Ok(())
        })??;
        Ok(Some(Arc::new(RuntimeCheckedPrefix {
            settled,
            admission,
            first_item,
            program,
            retained_inputs: Some(retained_inputs),
            state: parking_lot::Mutex::new(RuntimeCheckedState {
                snapshot,
                in_flight: None,
                reservation: None,
                interface_index,
                native_index: CheckedNativeIndex::new(native_imports),
                compile_inputs: None,
            }),
        })))
    }

    fn begin_cell_program_prefix(
        &self,
        admission: Arc<RuntimeCellAdmission>,
        first_item: tidepool_toolchain::checked_cell::ExactCheckedItem,
        program: Arc<tidepool_toolchain::checked_cell::CellProgram>,
        settled: super::SettledCellProgram,
    ) -> Result<Arc<RuntimeCheckedPrefix>, SessionError> {
        if admission.world_reservation().is_some() {
            return Err(SessionError::WorldCellCheckedProgramCapabilityMissing);
        }
        let finite = admission.finite_inputs()?;
        if !admission.belongs_to(self)
            || first_item.admission_digest() != admission.digest()
            || first_item.specification_digest() != admission.specification_digest()?
            || first_item.include_paths().len() != admission.include_paths()?.len()
            || !first_item
                .include_paths()
                .iter()
                .zip(admission.include_paths()?)
                .all(|(consumed, admitted)| consumed.as_os_str() == admitted.as_os_str())
            || first_item.reserved_declaration_modules().len()
                != admission.reserved_generations.len()
            || !admission
                .reserved_generations
                .iter()
                .zip(first_item.reserved_declaration_modules())
                .all(|(generation, module)| {
                    tidepool_repr::SessionModule::lib(*generation)
                        .module_name()
                        .eq(module)
                })
            || first_item.injected_modules().len() != finite.view.injected_values.len()
            || !finite
                .view
                .injected_values
                .iter()
                .zip(first_item.injected_modules())
                .all(|(module, supplied)| module.module_name().eq(supplied))
            || !initial_interfaces_match(&finite.interfaces, first_item.baseline_value_interfaces())
            || self.public_visibility_snapshot_in(admission.scope).as_ref()
                != Some(&finite.visibility)
            || self.compile_view_digest_in(admission.scope) != Some(finite.view_digest)
        {
            return Err(SessionError::StaleStagedDeclaration);
        }
        let (interfaces, interface_index) = CheckedInterfaces::base(&finite.interfaces);
        let snapshot = Arc::new(checked_snapshot(
            finite.view.clone(),
            finite.view_digest,
            finite.visibility.clone(),
            interfaces,
            first_item.initial_prefix()?,
            admission.digest(),
            admission._retained_scope.clone(),
            CheckedNativeImports::base(finite.native_imports.clone()),
        ));
        admission
            .prefix_started
            .compare_exchange(
                false,
                true,
                std::sync::atomic::Ordering::AcqRel,
                std::sync::atomic::Ordering::Acquire,
            )
            .map_err(|_| SessionError::StaleStagedDeclaration)?;
        let native_index = CheckedNativeIndex::new(finite.native_imports.clone());
        let compile_inputs = finite.compile_inputs.clone();
        Ok(Arc::new(RuntimeCheckedPrefix {
            settled,
            admission,
            first_item,
            program,
            retained_inputs: None,
            state: parking_lot::Mutex::new(RuntimeCheckedState {
                snapshot,
                in_flight: None,
                reservation: None,
                interface_index,
                native_index,
                compile_inputs: Some(compile_inputs),
            }),
        }))
    }
    /// Capsule drops happen outside checkout. The next owner entry retires
    /// their detached lease scopes through the same binding/native owner.
    pub(crate) fn reap_admission_leases(&mut self) {
        let retired = std::mem::take(&mut *self.admission_owner().retired.lock());
        for event in retired {
            let (scope, owner_epoch, lexical) = match event {
                RuntimeAdmissionRelease::Lexical { scope, owner_epoch } => {
                    (scope, owner_epoch, true)
                }
                RuntimeAdmissionRelease::OriginalSource { scope, owner_epoch } => {
                    (scope, owner_epoch, false)
                }
            };
            let current = self
                .admission_owner()
                .lexical_leases
                .lock()
                .get(&scope)
                .is_some_and(|record| {
                    record.owner_epoch == owner_epoch
                        && (!lexical
                            || record
                                .lease
                                .as_ref()
                                .is_some_and(|lease| lease.upgrade().is_none()))
                });
            if !current {
                continue;
            }
            if lexical {
                self.release_lexical_scope_resources(scope);
            }
            self.release_unheld_original_source_record(scope);
        }
    }

    fn current_private_world_association(
        &self,
        execution: &PrivateExecutionAdmission,
    ) -> Result<Arc<super::inspection::InspectionWorldAssociation>, SessionError> {
        if !Arc::ptr_eq(&execution.owner, self.admission_owner())
            || execution.owner_epoch != self.admission_owner().epoch()
            || !self.has_lib()
            || execution.session() != self.lib().session_id()
            || execution.library != self.lib().compile_view_identity
        {
            return Err(SessionError::StaleStagedDeclaration);
        }
        let scope = execution.private_scope();
        let association = self
            .lib()
            .tips
            .get(&scope)
            .and_then(|tip| tip.inspection_world.as_ref())
            .cloned()
            .ok_or(SessionError::MissingPublishedCompilerWorld(scope))?;
        let original_source = execution.original_source()?;
        original_source.validate_source_issuer()?;
        self.validate_lexical_scope_lease(scope, &execution.scope_lease)?;
        if !Arc::ptr_eq(&association.base.owner, self.admission_owner())
            || association.base.epoch != self.admission_owner().epoch()
            || !self.inspection_world_base_is_current(
                &association.base,
                &execution.compiler_basis.source,
                &original_source,
            )
            || !association.base.target.matches(&execution.scope_lease)
            || self.compile_view_key_in(scope).as_ref() != Some(&association.base.key)
        {
            return Err(SessionError::WorldCellPublicationBaseStale);
        }
        Ok(association)
    }

    pub(super) fn reconcile_private_visible_hosts(
        &mut self,
        execution: &PrivateExecutionAdmission,
        freezing: bool,
    ) -> Result<Arc<super::inspection::InspectionWorldAssociation>, SessionError> {
        let association = self.current_private_world_association(execution)?;
        let (accepted_revision, unapplied_visible_hosts, prior_progress, frontier) = {
            let state = execution.state.lock();
            let phase_ok = state.phase == PrivateExecutionPhase::Open
                || freezing && state.phase == PrivateExecutionPhase::Freezing;
            let PrivateAuthoredProgram::Ready(frontier) = &state.authored else {
                return Err(SessionError::WorldCellPublicationIncomplete);
            };
            if !phase_ok || !frontier.matches_world(&association.world) {
                return Err(SessionError::StaleStagedDeclaration);
            }
            if state.accepted_visible_revision == state.applied_visible_revision {
                return Ok(association);
            }
            (
                state.accepted_visible_revision,
                state.unapplied_visible_hosts.clone(),
                state.accepted_host_progress.clone(),
                frontier.clone(),
            )
        };
        let completed = execution.completed_values.lock();
        let mut rows = unapplied_visible_hosts
            .iter()
            .map(|id| {
                completed
                    .get(id)
                    .cloned()
                    .map(|write| (*id, write))
                    .ok_or(SessionError::StaleStagedDeclaration)
            })
            .collect::<Result<Vec<_>, _>>()?;
        drop(completed);
        rows.sort_by_key(|(id, _)| id.raw());

        let mut bindings = Vec::with_capacity(rows.len());
        let mut processed = Vec::with_capacity(rows.len());
        for (id, write) in &rows {
            if self
                .resolve_in(execution.private_scope(), write.name())
                .is_none_or(|entry| entry.id != *id)
            {
                processed.push((*id, PrivateHostDeltaDisposition::Superseded));
                continue;
            }
            let entry = self
                .bindings()
                .get(*id)
                .filter(|entry| entry.scope == execution.private_scope() && write.matches(entry))
                .ok_or(SessionError::StaleStagedDeclaration)?;
            let mount = write
                .visible_host()
                .ok_or(SessionError::StaleStagedDeclaration)?;
            if mount.before.base.scope != execution.private_scope()
                || mount.after.base.scope != execution.private_scope()
                || !Arc::ptr_eq(&mount.before.world, &mount.after.world)
            {
                return Err(SessionError::StaleStagedDeclaration);
            }
            processed.push((*id, PrivateHostDeltaDisposition::Applied));
            bindings.push(match &write.proof {
                CertifiedPrivateValueProof::Execution(proof) => {
                    tidepool_toolchain::compiler_world::CompilerWorldHostBinding::from_checked_execution(
                        write.name(),
                        &entry.value.identity,
                        *id,
                        entry.scope.0,
                        proof.clone(),
                    )
                }
                CertifiedPrivateValueProof::HostInterface(proof) => {
                    tidepool_toolchain::compiler_world::CompilerWorldHostBinding::from_host_interface(
                        write.name(),
                        &entry.value.identity,
                        *id,
                        entry.scope.0,
                        proof.clone(),
                    )
                }
            });
        }
        let successor = association.world.with_visible_host_bindings_resuming(
            association.world.binding_scope(),
            bindings,
            prior_progress,
        )?;
        let successor_frontier = frontier.apply_host_successor(&successor)?;
        let successor_association = Arc::new(super::inspection::InspectionWorldAssociation {
            base: association.base.clone(),
            world: successor.world().clone(),
        });

        {
            let mut state = execution.state.lock();
            let phase_ok = state.phase == PrivateExecutionPhase::Open
                || freezing && state.phase == PrivateExecutionPhase::Freezing;
            if !phase_ok
                || state.accepted_visible_revision != accepted_revision
                || !matches!(&state.authored, PrivateAuthoredProgram::Ready(current)
                    if Arc::ptr_eq(current, &frontier))
                || self
                    .lib()
                    .tips
                    .get(&execution.private_scope())
                    .and_then(|tip| tip.inspection_world.as_ref())
                    .is_none_or(|current| !Arc::ptr_eq(current, &association))
            {
                return Err(SessionError::StaleStagedDeclaration);
            }
            self.lib_mut()
                .tips
                .get_mut(&execution.private_scope())
                .expect("validated private World association")
                .inspection_world = Some(successor_association.clone());
            for (id, disposition) in &processed {
                state.unapplied_visible_hosts.remove(id);
                tracing::debug!(target: "tidepool::session::private_host_mount",
                    binding = id.raw(), ?disposition, accepted_revision,
                    "private accepted host delta processed");
            }
            state.applied_visible_revision = accepted_revision;
            state.accepted_host_progress = Some(successor.progress().clone());
            state.authored = PrivateAuthoredProgram::Ready(successor_frontier);
        }
        tracing::debug!(
            target: "tidepool::session::private_host_mount",
            accepted_revision,
            applied_hosts = rows.len(),
            total_host_bindings = successor.progress().len(),
            "private preparation applied accepted visible hosts"
        );
        Ok(successor_association)
    }

    /// Issue the exact current prep state for a program with no hidden operand.
    pub fn prepare_private_execution_snapshot(
        &mut self,
        execution: Arc<PrivateExecutionAdmission>,
    ) -> Result<Arc<PrivatePreparationSnapshot>, SessionError> {
        self.prepare_private_execution_snapshot_with_input(execution, None)
    }

    pub(super) fn prepare_private_execution_snapshot_with_input(
        &mut self,
        execution: Arc<PrivateExecutionAdmission>,
        input: Option<&PendingHostValueWrite>,
    ) -> Result<Arc<PrivatePreparationSnapshot>, SessionError> {
        let selected_input = input
            .map(|input| {
                input
                    .private_hidden_mount_key(self, &execution)
                    .map(|_| input.id)
            })
            .transpose()?;
        {
            let state = execution.state.lock();
            if state.phase != PrivateExecutionPhase::Open
                || !matches!(state.authored, PrivateAuthoredProgram::Ready(_))
                || !private_pending_is_ready(&state, selected_input)
            {
                return Err(SessionError::WorldCellPublicationIncomplete);
            }
        }
        let association = self.reconcile_private_visible_hosts(&execution, false)?;
        let state = execution.state.lock();
        let frontier = match &state.authored {
            PrivateAuthoredProgram::Ready(frontier)
                if state.phase == PrivateExecutionPhase::Open
                    && state.accepted_visible_revision == state.applied_visible_revision
                    && frontier.matches_world(&association.world)
                    && private_pending_is_ready(&state, selected_input) =>
            {
                frontier.clone()
            }
            _ => return Err(SessionError::StaleStagedDeclaration),
        };
        let snapshot = Arc::new(PrivatePreparationSnapshot {
            execution: execution.clone(),
            association,
            accepted_revision: state.accepted_visible_revision,
            applied_revision: state.applied_visible_revision,
            frontier,
            selected_input,
        });
        Ok(snapshot)
    }

    pub fn private_publication_diagnostic(
        &self,
        execution: &PrivateExecutionAdmission,
    ) -> Result<tidepool_toolchain::compiler_world::PrivatePublicationDiagnostic, SessionError>
    {
        let association = self.current_private_world_association(execution)?;
        Ok(
            tidepool_toolchain::compiler_world::CompilerWorld::diagnose_private_publication(
                &association.world,
                &execution.compiler_basis.published.world,
                execution.accepted_host_progress(),
            ),
        )
    }

    /// The durable cell producer requires an already published exact owner
    /// surface. A local owner-to-scope association cannot authorize execution.
    pub fn begin_durable_private_execution(
        &mut self,
        owner: &super::RecoveryPublicOwner,
        public_scope: ScopeId,
    ) -> Result<PrivateExecutionAdmission, SessionError> {
        self.validate_durable_public_admission(owner, public_scope)?;
        self.begin_private_execution(public_scope)
    }

    pub fn durable_public_readiness(
        &self,
        owner: &super::RecoveryPublicOwner,
        public_scope: ScopeId,
    ) -> Result<Arc<RuntimeDurablePublicReadiness>, SessionError> {
        self.validate_durable_public_admission(owner, public_scope)?;
        Ok(self.prepare_durable_public_readiness(
            owner,
            public_scope,
            self.admission_owner().epoch(),
        ))
    }

    pub(super) fn prepare_durable_public_readiness(
        &self,
        owner: &super::RecoveryPublicOwner,
        public_scope: ScopeId,
        owner_epoch: u64,
    ) -> Arc<RuntimeDurablePublicReadiness> {
        let state = self
            .lib()
            .durable_graph
            .as_ref()
            .expect("validated durable graph");
        Arc::new(RuntimeDurablePublicReadiness {
            owner: self.admission_owner().clone(),
            owner_epoch,
            session: self.lib().session_id(),
            durable: owner.clone(),
            scope: public_scope,
            store: Arc::downgrade(&state.store),
            attachment: Arc::downgrade(&state.readiness_lifetime),
        })
    }

    pub(super) fn validate_durable_public_admission(
        &self,
        owner: &super::RecoveryPublicOwner,
        public_scope: ScopeId,
    ) -> Result<PublicVisibilitySnapshot, SessionError> {
        use super::DurablePublicAdmissionFailure as Failure;
        let fail = |reason| SessionError::InvalidDurablePublicAdmission {
            owner: owner.clone(),
            scope: public_scope,
            reason,
        };
        let lib = self.lib();
        let state = lib
            .durable_graph
            .as_ref()
            .ok_or_else(|| fail(Failure::MissingGraph))?;
        let retained = state
            .owner
            .as_ref()
            .ok_or_else(|| fail(Failure::MissingRunOwner))?;
        retained.validate_owner()?;
        let snapshot = self
            .public_visibility_snapshot_in(public_scope)
            .ok_or(SessionError::DeadScope(public_scope))?;
        if !lib.validate_recovered_public_owner(owner)? {
            return Err(fail(Failure::MissingSurface));
        }
        let surface = state
            .graph
            .surface(owner)
            .map_err(|error| super::recovery::graph_error(&state.path, error))?
            .ok_or_else(|| fail(Failure::MissingSurface))?;
        let mapped = lib.durable_public_scopes.get(owner).copied();
        if mapped != Some(public_scope) {
            return Err(fail(Failure::Scope { mapped }));
        }
        let current =
            (snapshot.declaration_tip != Generation(0)).then_some(snapshot.declaration_tip);
        if surface.declaration_root != current {
            return Err(fail(Failure::DeclarationTip {
                published: surface.declaration_root,
                current,
            }));
        }
        if surface.epoch != snapshot.epoch {
            return Err(fail(Failure::Epoch {
                published: surface.epoch,
                current: snapshot.epoch,
            }));
        }
        retained.validate_owner()?;
        Ok(snapshot)
    }

    // boundary: see CELL-PREPARATION-BOUNDARY.md
    /// Begin private execution from the session's actual published World.
    /// Original source A is the issuing source transaction's lease; target B
    /// is the detached private placement captured below. A missing world is a
    /// missing realization, not a finite logical base: the existing source and
    /// inspection-world owner must publish one before this entry is used.
    pub fn begin_private_execution(
        &mut self,
        public_scope: ScopeId,
    ) -> Result<PrivateExecutionAdmission, SessionError> {
        let original_source = self
            .source_transaction
            .as_ref()
            .ok_or(super::SourceAdmissionError::MissingTransaction)?
            .source
            .clone();
        let source = self
            .lib()
            .tips
            .get(&public_scope)
            .and_then(|state| state.inspection_world.as_ref())
            .map(|association| association.base.source.clone())
            .ok_or(SessionError::MissingPublishedCompilerWorld(public_scope))?;
        self.begin_world_private_execution(public_scope, &source, &original_source)
    }

    /// Borrow the published world only for the actual requesting source and
    /// original canonical lease. Neither compiler inputs nor visibility are rebuilt.
    // boundary: see CELL-PREPARATION-BOUNDARY.md
    pub(crate) fn begin_world_private_execution(
        &mut self,
        public_scope: ScopeId,
        source: &super::GenerationArtifactReference,
        original_source: &Arc<RuntimeOriginalSource>,
    ) -> Result<PrivateExecutionAdmission, SessionError> {
        let transaction = self
            .source_transaction
            .as_ref()
            .ok_or(super::SourceAdmissionError::MissingTransaction)?;
        transaction.admit_original(original_source, None)?;
        if !self.has_lib() {
            return Err(SessionError::MissingDeclarationLibrary);
        }
        let published = self
            .lib()
            .tips
            .get(&public_scope)
            .and_then(|state| state.inspection_world.as_ref())
            .filter(|association| {
                self.inspection_world_base_is_current(&association.base, source, original_source)
            })
            .cloned()
            .ok_or(SessionError::MissingPublishedCompilerWorld(public_scope))?;
        let custody = published.base.acquire_custody()?;
        let admitted = self
            .public_visibility_snapshot_in(public_scope)
            .ok_or(SessionError::DeadScope(public_scope))?;
        let admitted_context = self.lib().current_exact_context_in(public_scope);
        let source = published.base.source.clone();
        self.begin_private_execution_with_world(
            public_scope,
            WorldExecutionAdmissionFacts {
                published,
                source,
                original_source: custody.original_source,
                public_target: custody.target,
                admitted,
                admitted_context,
            },
        )
    }

    fn begin_private_execution_with_world(
        &mut self,
        public_scope: ScopeId,
        world: WorldExecutionAdmissionFacts,
    ) -> Result<PrivateExecutionAdmission, SessionError> {
        self.admit_source_work_in(public_scope)?;

        let store = self.held_admission_store()?;
        super::generation_lease::requested_end(self.val_gen(), 1)?;
        self.reap_admission_leases();
        let durable_owner = self
            .lib()
            .durable_public_scopes
            .iter()
            .find_map(|(owner, scope)| (*scope == public_scope).then(|| owner.clone()));
        let public_tip = self.lib().scope_tip(public_scope);
        let private_scope = {
            let admission = store
                .as_ref()
                .map(|store| store.begin_native_transition())
                .transpose()
                .map_err(|error| SessionError::RecoveryStore(Box::new(error)))?;
            self.lib_mut().seed_scope(public_scope, public_tip);
            let private_scope =
                self.mint_world_execution_scope(public_scope, &world.admitted.source_selection)?;
            super::complete_native_transition(admission)?;
            private_scope
        };
        let scope_lease = self.prepared_lexical_scope_lease(private_scope)?;
        let compiler_basis = PrivateExecutionCompilerBasis {
            published: world.published,
            private_key: self
                .compile_view_key_in(private_scope)
                .ok_or(SessionError::StaleStagedDeclaration)?,
            source: world.source,
            original_source: world.original_source,
            public_target: world.public_target,
            admitted: world.admitted,
            admitted_context: world.admitted_context,
        };
        let binding_tip = self
            .binding_tip_id(private_scope)
            .expect("detached scope captures a binding tip");
        let frontier = compiler_basis.published.world.private_frontier();
        let prepared = PrivateExecutionAdmission {
            library: self.lib().compile_view_identity,
            session: self.lib().session_id(),
            owner: self.admission_owner().clone(),
            owner_epoch: self.admission_owner().epoch(),
            durable_owner,
            scope_lease,
            public_scope,
            private_scope,
            compiler_basis,
            binding_tip,
            completed_values: parking_lot::Mutex::new(std::collections::HashMap::new()),
            state: Arc::new(parking_lot::Mutex::new(PrivateExecutionState {
                phase: PrivateExecutionPhase::Open,
                authored: PrivateAuthoredProgram::Ready(frontier),
                accepted_visible_revision: 0,
                applied_visible_revision: 0,
                unapplied_visible_hosts: std::collections::HashSet::new(),
                accepted_host_progress: None,
                pending_hosts: std::collections::HashMap::new(),
            })),
            final_intent: OnceLock::new(),
        };
        prepared
            .scope_lease
            .private_lifecycle
            .set(Arc::downgrade(&prepared.state))
            .map_err(|_| SessionError::StaleStagedDeclaration)?;
        let admission = store
            .as_ref()
            .map(|store| store.begin_native_transition())
            .transpose()
            .map_err(|error| SessionError::RecoveryStore(Box::new(error)))?;
        super::complete_native_transition(admission)?;
        // Detached runtime placement borrows the SAME published compiler World.
        // A different runtime scope does not invent a compiler product or ID.
        let facts = prepared.world_execution_facts();
        RuntimeOriginalSource::with_sources_and_targets(
            [facts.original_source.clone()],
            [prepared.scope_lease.clone(), facts.public_target.clone()],
            |sources, _| {
                super::RuntimeSourceAdmission::with_open_admissions(
                    sources.iter().map(|source| source.source_admission()),
                    || {
                        if !Arc::ptr_eq(&prepared.owner, self.admission_owner())
                            || prepared.owner_epoch != self.admission_owner().epoch()
                            || !self.inspection_world_base_is_current(
                                &facts.published.base,
                                &facts.source,
                                &facts.original_source,
                            )
                        {
                            return Err(SessionError::WorldCellPublicationBaseStale);
                        }
                        let key = self
                            .compile_view_key_in(private_scope)
                            .ok_or(SessionError::StaleStagedDeclaration)?;
                        if &key != prepared.world_basis()?.1 {
                            return Err(SessionError::StaleStagedDeclaration);
                        }
                        let association = Arc::new(super::inspection::InspectionWorldAssociation {
                            base: super::inspection::InspectionWorldBase {
                                scope: private_scope,
                                key,
                                owner: prepared.owner.clone(),
                                epoch: prepared.owner_epoch,
                                source: facts.source.clone(),
                                original_source:
                                    super::inspection::InspectionWorldSourceObservation::new(
                                        &facts.original_source,
                                    ),
                                target: super::inspection::InspectionWorldLeaseObservation::new(
                                    &prepared.scope_lease,
                                ),
                            },
                            world: facts.published.world.clone(),
                        });
                        self.lib_mut()
                            .tips
                            .entry(private_scope)
                            .or_default()
                            .inspection_world = Some(association);
                        Ok(())
                    },
                )
            },
        )??;
        Ok(prepared)
    }

    /// An explicitly local public surface cannot reuse an initialized durable
    /// owner's scope. The admission remembers this choice through finalization.
    pub fn begin_ephemeral_private_execution(
        &mut self,
        public_scope: ScopeId,
    ) -> Result<PrivateExecutionAdmission, SessionError> {
        if self
            .lib()
            .durable_public_scopes
            .values()
            .any(|scope| *scope == public_scope)
        {
            return Err(SessionError::WrongPublicManifestTicket);
        }
        self.begin_private_execution(public_scope)
    }

    #[cfg(test)]
    pub(super) fn admit_cell_in(
        &mut self,
        scope: ScopeId,
        declaration_count: usize,
        specification: Arc<dyn Any + Send + Sync>,
        specification_digest: [u8; 32],
        authority_digest: [u8; 32],
        include_paths: Vec<PathBuf>,
    ) -> Result<Arc<RuntimeCellAdmission>, SessionError> {
        self.admit_cell_with_plan(
            scope,
            declaration_count,
            specification,
            specification_digest,
            authority_digest,
            include_paths,
            None,
            RuntimeCellPurpose::Observation,
            None,
        )
    }

    /// Admit trusted setup under its existing scope with the same ordered
    /// parser and exact environment seals used by private authored execution.
    pub fn admit_native_setup_cell_in(
        &mut self,
        scope: ScopeId,
        plan: Arc<tidepool_toolchain::cell_plan::ParsedCellPlan>,
        specification: Arc<dyn Any + Send + Sync>,
        specification_digest: [u8; 32],
        authority_digest: [u8; 32],
        include_paths: Vec<PathBuf>,
        compile_inputs: Option<super::prepared::RuntimeCompileInputs>,
    ) -> Result<Arc<RuntimeCellAdmission>, SessionError> {
        use tidepool_toolchain::cell_plan::ParsedCellPlanKind as Kind;
        if plan.items().len() != 1
            || plan.items()[0].kind() != Kind::Bind
            || !plan.items()[0].binders().is_empty()
        {
            return Err(self.native_setup_refusal(
                scope,
                NativeSetupAdmissionFailure::Shape {
                    items: plan.items().len(),
                    first: plan.items().first().map(|item| item.kind()),
                    binders: plan.items().first().map_or(0, |item| item.binders().len()),
                },
            ));
        }
        self.admit_cell_with_plan(
            scope,
            0,
            specification,
            specification_digest,
            authority_digest,
            include_paths,
            Some(plan),
            RuntimeCellPurpose::NativeSetup,
            compile_inputs,
        )
    }

    pub(super) fn admit_host_carrier_cell_in(
        &mut self,
        scope: ScopeId,
        plan: Arc<tidepool_toolchain::cell_plan::ParsedCellPlan>,
        specification: Arc<dyn Any + Send + Sync>,
        specification_digest: [u8; 32],
        authority_digest: [u8; 32],
        include_paths: Vec<PathBuf>,
        binding: String,
        expected: super::resident::HostBindingType,
        compile_inputs: Option<super::prepared::RuntimeCompileInputs>,
    ) -> Result<Arc<RuntimeCellAdmission>, SessionError> {
        if self
            .source_lease_in(scope)?
            .private_lifecycle
            .get()
            .is_some()
        {
            return Err(SessionError::WorldCellRequiresServicePreparation);
        }
        use tidepool_toolchain::cell_plan::ParsedCellPlanKind as Kind;
        if plan.items().len() != 1
            || plan.items()[0].kind() != Kind::Bind
            || plan.items()[0].binders() != [binding.as_str()]
        {
            return Err(self.native_setup_refusal(
                scope,
                NativeSetupAdmissionFailure::HostCarrierShape {
                    items: plan.items().len(),
                    first: plan.items().first().map(|item| item.kind()),
                    binders: plan.items().first().map_or(0, |item| item.binders().len()),
                },
            ));
        }
        self.admit_cell_with_plan(
            scope,
            0,
            specification,
            specification_digest,
            authority_digest,
            include_paths,
            Some(plan),
            RuntimeCellPurpose::HostCarrier {
                binding,
                expected,
                authority_digest,
                private_preparation: None,
            },
            compile_inputs,
        )
    }

    fn native_setup_refusal(
        &self,
        scope: ScopeId,
        reason: NativeSetupAdmissionFailure,
    ) -> SessionError {
        SessionError::InvalidNativeSetupAdmission {
            owner: self.admission_owner().identity,
            owner_epoch: self.admission_owner().epoch(),
            scope,
            reason,
        }
    }

    fn admit_cell_with_plan(
        &mut self,
        scope: ScopeId,
        declaration_count: usize,
        specification: Arc<dyn Any + Send + Sync>,
        specification_digest: [u8; 32],
        authority_digest: [u8; 32],
        include_paths: Vec<PathBuf>,
        plan: Option<Arc<tidepool_toolchain::cell_plan::ParsedCellPlan>>,
        purpose: RuntimeCellPurpose,
        compile_inputs: Option<super::prepared::RuntimeCompileInputs>,
    ) -> Result<Arc<RuntimeCellAdmission>, SessionError> {
        self.admit_source_work_in(scope)?;

        let store = self.held_admission_store()?;
        super::generation_lease::requested_end(self.val_gen(), 1)?;
        self.reap_admission_leases();
        let view = self
            .compile_view_in(scope)
            .ok_or(SessionError::DeadScope(scope))?
            .with_scoped_injection();
        let compile_inputs = compile_inputs.unwrap_or_default();
        let view = view
            .with_compile_inputs(&compile_inputs)
            .map_err(SessionError::Compile)?;
        let declaration_baseline = if declaration_count > 0 {
            super::lexical_projection::DeclarationProjectionBaseline::capture(
                self.lib(),
                self.lib().scope_tip(scope),
            )?
        } else {
            None
        };
        if let Some(plan) = &plan {
            let injected = view
                .injected_values()
                .iter()
                .map(|module| module.module_name())
                .collect::<Vec<_>>();
            let refusal = if plan.specification_digest() != specification_digest {
                Some(NativeSetupAdmissionFailure::SpecificationDigest {
                    planned: plan.specification_digest(),
                    admitted: specification_digest,
                })
            } else if plan.include_paths().len() != include_paths.len()
                || !plan
                    .include_paths()
                    .iter()
                    .zip(&include_paths)
                    .all(|(a, b)| a.as_os_str() == b.as_os_str())
            {
                Some(NativeSetupAdmissionFailure::IncludePaths {
                    planned: NativeSetupInputInventory::new(
                        plan.include_paths()
                            .iter()
                            .map(|path| path.as_os_str().as_encoded_bytes()),
                    ),
                    admitted: NativeSetupInputInventory::new(
                        include_paths
                            .iter()
                            .map(|path| path.as_os_str().as_encoded_bytes()),
                    ),
                })
            } else if plan.injected_modules() != injected {
                Some(NativeSetupAdmissionFailure::InjectionInventory {
                    planned: NativeSetupInputInventory::new(
                        plan.injected_modules()
                            .iter()
                            .map(|module| module.as_bytes()),
                    ),
                    reachable: NativeSetupInputInventory::new(
                        injected.iter().map(|module| module.as_bytes()),
                    ),
                })
            } else {
                plan.items()
                    .iter()
                    .enumerate()
                    .find(|(index, item)| item.index() != *index)
                    .map(|(position, item)| NativeSetupAdmissionFailure::ItemOrder {
                        position,
                        observed: item.index(),
                    })
            };
            if let Some(reason) = refusal {
                return Err(if matches!(purpose, RuntimeCellPurpose::NativeSetup) {
                    self.native_setup_refusal(scope, reason)
                } else {
                    SessionError::StaleStagedDeclaration
                });
            }
        }
        let view_digest = self
            .compile_view_digest_in(scope)
            .ok_or(SessionError::DeadScope(scope))?;
        // The lazy machine has one reserved lifetime identity before any
        // compiler offer captures it. Bootstrap adopts this identity rather
        // than invalidating another independently admitted private cell.
        self.reserve_admitted_machine_incarnation(store.as_ref())?;
        let visibility = self
            .public_visibility_snapshot_in(scope)
            .ok_or(SessionError::DeadScope(scope))?;
        let native_shares = self
            .bindings()
            .source_instance_keys_in(self.scope_tree(), scope)
            .into_iter()
            .collect::<std::collections::BTreeSet<_>>()
            .into_iter()
            .collect::<Vec<_>>();
        let native_imports = Arc::new(self.capture_admitted_native_imports(scope)?);
        let interfaces = self.capture_value_interfaces(&view)?;
        let native_count = match &plan {
            Some(plan) => plan.items().iter().try_fold(0u64, |count, item| {
                use tidepool_toolchain::cell_plan::ParsedCellPlanKind as Kind;
                count.checked_add(match item.kind() {
                    Kind::Prologue | Kind::Declaration => 0,
                    Kind::Bind | Kind::Expression => 1,
                })
            }),
            None => Some(1),
        }
        .ok_or(SessionError::StaleStagedDeclaration)?;
        let last_value_generation =
            super::generation_lease::requested_end(self.val_gen(), native_count)?;
        super::generation_lease::requested_end(last_value_generation, 1)?;
        let count_u64 =
            u64::try_from(declaration_count).map_err(|_| SessionError::StaleStagedDeclaration)?;
        self.lib()
            .log
            .generation()
            .0
            .checked_add(count_u64)
            .ok_or(SessionError::StaleStagedDeclaration)?;
        let mut reserved_generations = Vec::new();
        reserved_generations
            .try_reserve_exact(declaration_count)
            .map_err(|_| SessionError::StaleStagedDeclaration)?;
        let mut planned_items = Vec::new();
        if let Some(plan) = &plan {
            planned_items
                .try_reserve_exact(plan.items().len())
                .map_err(|_| SessionError::StaleStagedDeclaration)?;
        }
        let retained_scope = {
            let admission = store
                .as_ref()
                .map(|store| store.begin_native_transition())
                .transpose()
                .map_err(|error| SessionError::RecoveryStore(Box::new(error)))?;
            let retained = self.retain_lexical_scope(scope)?;
            super::complete_native_transition(admission)?;
            retained
        };
        if self.lib().durable_graph.is_some() {
            reserved_generations = self
                .lib_mut()
                .reserve_declaration_generations_durable(declaration_count)?;
        } else {
            for _ in 0..declaration_count {
                reserved_generations.push(self.lib_mut().log.reserve());
            }
        }
        let (initial_value_generation, _) = self.reserve_value_generations(native_count)?;
        let view =
            view.with_request_inventory(view.injected_values().to_vec(), initial_value_generation);
        let planned = plan.map(|plan| {
            use super::planned_cell::{RuntimePlannedCellItem, RuntimePlannedCellSlot as Slot};
            use tidepool_toolchain::cell_plan::ParsedCellPlanKind as Kind;
            let mut declarations = reserved_generations.iter().copied();
            let mut next_value = initial_value_generation.0;
            let mut names = self
                .bindings()
                .iter_current_in(self.scope_tree(), scope)
                .into_iter()
                .map(|(name, _)| name.0.clone())
                .collect::<std::collections::BTreeSet<_>>();
            names.extend(
                plan.items()
                    .iter()
                    .flat_map(|item| item.binders().iter().cloned()),
            );
            names.extend(
                self.lib()
                    .log
                    .current_items_at(visibility.declaration_tip)
                    .into_iter()
                    .flat_map(|(item, _)| {
                        item.value_names().map(str::to_owned).collect::<Vec<_>>()
                    }),
            );
            for item in plan.items() {
                let slot = match item.kind() {
                    Kind::Prologue => Slot::Prologue {
                        declaration: declarations
                            .next()
                            .expect("parser declaration count preflighted"),
                    },
                    Kind::Declaration => Slot::Declaration {
                        declaration: declarations
                            .next()
                            .expect("parser declaration count preflighted"),
                    },
                    Kind::Bind => {
                        let value = Generation(next_value);
                        next_value += 1;
                        Slot::Bind { value }
                    }
                    Kind::Expression => {
                        let capture = Generation(next_value);
                        next_value += 1;
                        let mut observation_name = format!("observation{}", capture.0);
                        while names.contains(&observation_name) {
                            observation_name.push('_');
                        }
                        names.insert(observation_name.clone());
                        Slot::Expression {
                            capture,
                            observation_name,
                        }
                    }
                };
                planned_items.push(RuntimePlannedCellItem {
                    index: item.index(),
                    slot,
                });
            }
            (plan, planned_items)
        });
        let mut digest = blake3::Hasher::new();
        let mut frame = |bytes: &[u8]| {
            digest.update(&(bytes.len() as u64).to_le_bytes());
            digest.update(bytes);
        };
        frame(b"TidepoolRuntimeCellAdmission2");
        frame(self.admission_owner().identity.as_bytes());
        frame(&self.admission_owner().epoch().to_le_bytes());
        frame(&specification_digest);
        frame(&authority_digest);
        frame(&(include_paths.len() as u64).to_le_bytes());
        for path in &include_paths {
            frame(path.as_os_str().as_encoded_bytes());
        }
        frame(&view.session().0.to_le_bytes());
        frame(&scope.0.to_le_bytes());
        frame(&visibility.epoch.to_le_bytes());
        frame(&visibility.declaration_tip.0.to_le_bytes());
        match visibility.machine_incarnation {
            Some(incarnation) => {
                frame(&[1]);
                frame(&incarnation.0.to_le_bytes());
            }
            None => frame(&[0]),
        }
        frame(&view.next_value_generation().0.to_le_bytes());
        frame(&view_digest);
        frame(&native_imports.commitment);
        if let Some(context) = view.exact_declaration_context() {
            frame(&context.semantic_sha256());
        }
        for (name, id) in &visibility.bindings {
            frame(name.as_bytes());
            frame(&id.raw().to_le_bytes());
        }
        for generation in &reserved_generations {
            frame(&generation.0.to_le_bytes());
        }
        if let Some((plan, items)) = &planned {
            frame(b"TidepoolRuntimeOrderedCell1");
            frame(&plan.digest());
            frame(&plan.producer_sha256());
            for item in items {
                frame(&(item.index() as u64).to_le_bytes());
                frame(&[item.kind() as u8]);
                for generation in [item.declaration_generation(), item.value_generation()] {
                    match generation {
                        Some(generation) => {
                            frame(&[1]);
                            frame(&generation.0.to_le_bytes());
                        }
                        None => frame(&[0]),
                    }
                }
                match item.observation_name() {
                    Some(name) => {
                        frame(&[1]);
                        frame(name.as_bytes());
                    }
                    None => frame(&[0]),
                }
            }
        }
        for interface in &interfaces {
            frame(interface.module().module_name().as_bytes());
            frame(interface.bytes());
        }
        for key in &native_shares {
            frame(&key.instance.raw().to_le_bytes());
            frame(&key.value.0.to_le_bytes());
        }
        purpose.frame_authorization(&mut frame);
        compile_inputs.frame_authorization(&mut frame);
        let digest = tidepool_toolchain::checked_cell::AdmissionDigest::finalize(
            *digest.finalize().as_bytes(),
        )
        .map_err(SessionError::Compile)?;
        let checked_request = match &planned {
            Some((plan, _)) => {
                let reserved = reserved_generations
                    .iter()
                    .map(|generation| tidepool_repr::SessionModule::lib(*generation).module_name())
                    .collect::<Vec<_>>();
                Some(Arc::new(
                    tidepool_toolchain::checked_cell::CheckedCellRequest::issue(
                        Arc::clone(plan.specification()),
                        digest,
                        reserved,
                    )
                    .map_err(SessionError::Compile)?,
                ))
            }
            None => None,
        };
        let planned = planned.map(|(plan, items)| {
            Arc::new(super::planned_cell::RuntimeCellPlanReservation {
                plan,
                items,
                digest: digest.bytes(),
            })
        });
        let prepared = Arc::new(RuntimeCellAdmission {
            owner: self.admission_owner().clone(),
            owner_epoch: self.admission_owner().epoch(),
            session: view.session(),
            purpose,
            _retained_scope: retained_scope,
            prefix_started: std::sync::atomic::AtomicBool::new(false),
            scope,
            reserved_generations,
            initial_value_generation,
            declarations: RuntimeCellDeclarations {
                root: view.session_root().to_path_buf(),
                declaration_baseline,
                prepared_declarations: std::sync::OnceLock::new(),
            },
            compiler_basis: RuntimeCellCompilerBasis::Finite(RuntimeFiniteCellInputs {
                visibility,
                catalog_selection: self.catalog_selection().clone(),
                compile_inputs,
                view,
                view_digest,
                planned,
                native_shares,
                native_imports,
                interfaces,
                specification,
                specification_digest,
                authority_digest,
                include_paths,
                checked_request,
            }),
            digest,
        });
        Ok(prepared)
    }

    fn capture_admitted_native_imports(
        &self,
        scope: ScopeId,
    ) -> Result<AdmittedNativeImports, SessionError> {
        if !self.scope_tree().is_live(scope) {
            return Err(SessionError::DeadScope(scope));
        }
        let mut entries = std::collections::BTreeSet::new();
        let mut owners = std::collections::BTreeMap::new();
        let mut hash = blake3::Hasher::new();
        hash.update(b"TidepoolAdmittedNativeImports1");
        for id in self
            .bindings()
            .scope_reachable_binding_ids(self.scope_tree(), scope)
        {
            let entry = self
                .bindings()
                .get(id)
                .ok_or(SessionError::StaleStagedDeclaration)?;
            let handle = entry.value.handle;
            if self
                .prepared()
                .and_then(|engine| engine.prepared_handle_of(handle.raw()))
                != Some(handle)
            {
                return Err(SessionError::StaleStagedDeclaration);
            }
            hash.update(b"binding");
            hash.update(&id.raw().to_le_bytes());
            hash.update(&entry.module.gen.0.to_le_bytes());
            hash.update(&handle.raw().0.to_le_bytes());
            frame_native_identity(&mut hash, &entry.value.identity);
            entries.insert((entry.value.identity.clone(), entry.module.gen.0));
            owners.insert(
                (entry.value.identity.clone(), entry.module.gen.0),
                handle.raw().0,
            );
        }
        Ok(AdmittedNativeImports {
            entries: entries.into_iter().collect(),
            owners,
            commitment: *hash.finalize().as_bytes(),
        })
    }

    fn adopt_checked_world_successor_in(
        &mut self,
        scope: ScopeId,
        admission: &RuntimeCellAdmission,
        predecessor: &Arc<tidepool_toolchain::compiler_world::CompilerWorld>,
        successor: &tidepool_toolchain::compiler_world::CheckedWorldSuccessor,
    ) -> Result<(), SessionError> {
        let RuntimeCellCompilerBasis::World {
            original_source, ..
        } = &admission.compiler_basis
        else {
            return Err(SessionError::StaleStagedDeclaration);
        };
        let source = admission.world_source_reference()?.clone();
        let original_source = original_source.clone();
        let target = admission._retained_scope.clone();
        let replaced = RuntimeOriginalSource::with_sources_and_targets(
            [original_source.clone()],
            [target.clone()],
            |sources, _| {
                super::RuntimeSourceAdmission::with_open_admissions(
                    sources.iter().map(|source| source.source_admission()),
                    || {
                        if !admission.belongs_to(self)
                            || admission.scope != scope
                            || !self
                                .source_lease_in(scope)
                                .is_ok_and(|lease| Arc::ptr_eq(&lease, &target))
                            || self
                                .lib()
                                .tips
                                .get(&scope)
                                .and_then(|tip| tip.inspection_world.as_ref())
                                .is_none_or(|association| {
                                    !Arc::ptr_eq(&association.world, predecessor)
                                })
                        {
                            return Err(SessionError::StaleStagedDeclaration);
                        }
                        let key = self
                            .compile_view_key_in(scope)
                            .ok_or(SessionError::DeadScope(scope))?;
                        let association = Arc::new(super::inspection::InspectionWorldAssociation {
                            base: super::inspection::InspectionWorldBase {
                                scope,
                                key,
                                owner: self.admission_owner().clone(),
                                epoch: self.admission_owner().epoch(),
                                source: source.clone(),
                                original_source:
                                    super::inspection::InspectionWorldSourceObservation::new(
                                        &original_source,
                                    ),
                                target: super::inspection::InspectionWorldLeaseObservation::new(
                                    &target,
                                ),
                            },
                            world: Arc::clone(successor.world()),
                        });
                        let tip = self.lib_mut().tips.entry(scope).or_default();
                        let old_context =
                            tip.exact_context
                                .replace(super::recovery::ScopedCompilerContext::new(Arc::clone(
                                    successor.compiler_context(),
                                )));
                        let old_world = tip.inspection_world.replace(association);
                        Ok((old_context, old_world))
                    },
                )
            },
        )??;
        // Completion changes runtime compiler custody even when the item's
        // Haskell names and TYPE input are unchanged. Future captures must read
        // the newly installed context rather than a cached predecessor view.
        self.invalidate_compile_view_in(scope);
        drop(replaced);
        Ok(())
    }

    fn bind_world_retained_inputs(
        &self,
        program: &tidepool_toolchain::checked_cell::CellProgram,
        scope: Arc<RuntimeLexicalScopeLease>,
    ) -> Result<BoundRetainedInputs, SessionError> {
        self.validate_lexical_scope_lease(scope.scope, &scope)?;
        let scoped = self.scoped_prepared_bindings_in(scope.scope);
        let mut stages = BTreeMap::new();
        for item in program.items() {
            let checked = item.checked_item();
            let stage = checked
                .world_selected_runtime_inputs()
                .ok_or(SessionError::StaleStagedDeclaration)?
                .stage_first_ordinal();
            if stages.contains_key(&stage) {
                continue;
            }
            let selected = checked.world_selected_dependency_terminals()?;
            let mut bound = Vec::with_capacity(selected.len());
            for terminal in selected {
                let module = tidepool_repr::SessionModule::val(Generation(terminal.generation()));
                let entry = scoped
                    .get(&(terminal.identity(), module))
                    .ok_or(SessionError::StaleStagedDeclaration)?;
                let row = self
                    .retained_checked_value_artifact(entry.module)
                    .ok_or(SessionError::MissingRetainedValueInterface(entry.module))?;
                let interface = row.certified_interface().interface();
                let handle = entry.value.handle;
                if row.owner() != entry.module
                    || interface.unit() != terminal.identity().unit
                    || interface.module() != terminal.identity().module
                    || !terminal.matches_checked_value(row)?
                    || self
                        .prepared()
                        .and_then(|engine| engine.prepared_handle_of(handle.raw()))
                        != Some(handle)
                {
                    return Err(SessionError::StaleStagedDeclaration);
                }
                bound.push(BoundRetainedInput {
                    terminal,
                    id: entry.id,
                    defining_scope: entry.scope,
                    handle,
                    row: Arc::clone(row),
                });
            }
            stages.insert(stage, bound);
        }
        Ok(BoundRetainedInputs { scope, stages })
    }

    fn capture_world_native_imports(
        &self,
        scope: ScopeId,
        program: &tidepool_toolchain::checked_cell::CellProgram,
        retained: &BoundRetainedInputs,
    ) -> Result<AdmittedNativeImports, SessionError> {
        use tidepool_toolchain::checked_cell::CheckedPlannedCellSlot as Slot;
        let own_values = program
            .slots()
            .iter()
            .filter_map(|slot| match slot {
                Slot::Bind { value } => Some(*value),
                Slot::Expression { capture, .. } => Some(*capture),
                _ => None,
            })
            .collect::<std::collections::BTreeSet<_>>();
        self.capture_selected_native_imports(
            scope,
            program
                .items()
                .iter()
                .filter_map(|item| item.native())
                .flat_map(|native| native.required_native_imports())
                .filter(|(identity, generation)| {
                    // Same-cell values become imports only after their own item succeeds.
                    !(own_values.contains(generation)
                        && identity.unit == "main"
                        && identity.module
                            == tidepool_repr::SessionModule::val(Generation(*generation))
                                .module_name())
                })
                .chain(
                    retained
                        .stages
                        .values()
                        .flatten()
                        .map(|input| (input.terminal.identity(), input.terminal.generation())),
                ),
        )
    }

    fn capture_selected_native_imports<'a>(
        &self,
        scope: ScopeId,
        required: impl IntoIterator<Item = (&'a SymbolIdentity, u64)>,
    ) -> Result<AdmittedNativeImports, SessionError> {
        if !self.scope_tree().is_live(scope) {
            return Err(SessionError::DeadScope(scope));
        }
        let scoped = self.scoped_prepared_bindings_in(scope);
        let mut owners = BTreeMap::new();
        for (identity, generation) in required {
            let entry = scoped
                .get(&(
                    identity,
                    tidepool_repr::SessionModule::val(Generation(generation)),
                ))
                .ok_or(SessionError::StaleStagedDeclaration)?;
            let handle = entry.value.handle;
            if self
                .prepared()
                .and_then(|engine| engine.prepared_handle_of(handle.raw()))
                != Some(handle)
            {
                return Err(SessionError::StaleStagedDeclaration);
            }
            let root = handle.raw().0;
            let key = (identity.clone(), generation);
            if owners.insert(key, root).is_some_and(|old| old != root) {
                return Err(SessionError::StaleStagedDeclaration);
            }
        }
        let mut hash = blake3::Hasher::new();
        hash.update(b"TidepoolWorldAdmittedNativeImports1");
        for ((identity, generation), root) in &owners {
            frame_native_identity(&mut hash, identity);
            hash.update(&generation.to_le_bytes());
            hash.update(&root.to_le_bytes());
        }
        Ok(AdmittedNativeImports {
            entries: owners.keys().cloned().collect(),
            owners,
            commitment: *hash.finalize().as_bytes(),
        })
    }

    fn capture_checked_native_delta<'a>(
        &self,
        scope: ScopeId,
        generation: u64,
        bound_rows: impl Iterator<Item = (&'a str, u64)>,
    ) -> Result<CapturedNativeDelta, SessionError> {
        if !self.scope_tree().is_live(scope) {
            return Err(SessionError::DeadScope(scope));
        }
        let engine = self
            .prepared()
            .ok_or(SessionError::StaleStagedDeclaration)?;
        let mut imports = std::collections::BTreeMap::new();
        let mut bindings = Vec::new();
        for (name, id) in bound_rows {
            let id = tidepool_repr::SessionVarId::from_extract(id);
            let Some(entry) = self.bindings().get(id) else {
                return Err(SessionError::StaleStagedDeclaration);
            };
            let identity = &entry.value.identity;
            let module = tidepool_repr::SessionModule::val(Generation(generation));
            if entry.scope != scope
                || entry.name.0 != name
                || entry.module != module
                || identity.module != module.module_name()
                || identity.namespace != "value"
                || identity.occurrence != name
                || identity.record_parent.is_some()
                || engine.prepared_handle_of(entry.value.handle.raw()) != Some(entry.value.handle)
            {
                return Err(SessionError::StaleStagedDeclaration);
            }
            let root_id = entry.value.handle.raw().0;
            imports.insert((identity.clone(), generation), root_id);
            bindings.push(SettledNativeBinding {
                name: name.to_owned(),
                id,
                import: SettledNativeImport {
                    identity: identity.clone(),
                    generation,
                    root_id,
                },
            });
        }
        Ok(CapturedNativeDelta {
            imports: imports
                .into_iter()
                .map(|((identity, generation), root_id)| SettledNativeImport {
                    identity,
                    generation,
                    root_id,
                })
                .collect(),
            bindings,
        })
    }

    /// Capture private admission facts for owner-component controls without
    /// a parser or compiler. This cannot produce an executable program prefix.
    #[cfg(test)]
    pub(super) fn admit_cell_for_execution(
        &mut self,
        execution: Arc<PrivateExecutionAdmission>,
        declaration_count: usize,
        specification: Arc<dyn Any + Send + Sync>,
        specification_digest: [u8; 32],
        authority_digest: [u8; 32],
        include_paths: Vec<PathBuf>,
    ) -> Result<Arc<RuntimeCellAdmission>, SessionError> {
        self.compile_view_for_execution(&execution)?;
        self.admit_cell_with_plan(
            execution.private_scope(),
            declaration_count,
            specification,
            specification_digest,
            authority_digest,
            include_paths,
            None,
            RuntimeCellPurpose::PrivateExecution(execution),
            None,
        )
    }

    /// Reserve every ordered source item under the original private admission.
    /// The parser capability supplies kinds and order; callers cannot guess a
    /// declaration count or assign generations. Authoritative checking must
    /// subsequently bind this exact reservation before prefix creation.
    pub fn admit_planned_cell_for_execution(
        &mut self,
        execution: Arc<PrivateExecutionAdmission>,
        plan: Arc<tidepool_toolchain::cell_plan::ParsedCellPlan>,
        specification: Arc<dyn Any + Send + Sync>,
        specification_digest: [u8; 32],
        authority_digest: [u8; 32],
        include_paths: Vec<PathBuf>,
        compile_inputs: Option<super::prepared::RuntimeCompileInputs>,
    ) -> Result<Arc<RuntimeCellAdmission>, SessionError> {
        self.compile_view_for_execution(&execution)?;
        use tidepool_toolchain::cell_plan::ParsedCellPlanKind as Kind;
        let declaration_count = plan
            .items()
            .iter()
            .filter(|item| matches!(item.kind(), Kind::Prologue | Kind::Declaration))
            .count();
        self.admit_cell_with_plan(
            execution.private_scope(),
            declaration_count,
            specification,
            specification_digest,
            authority_digest,
            include_paths,
            Some(plan),
            RuntimeCellPurpose::PrivateExecution(execution),
            compile_inputs,
        )
    }

    // boundary: see CELL-PREPARATION-BOUNDARY.md
    /// Reserve runtime identities for the actual parser plan of the held
    /// private compiler world. This issues expectations, never checked output.
    pub fn admit_world_planned_cell_for_execution(
        &mut self,
        execution: Arc<PrivateExecutionAdmission>,
        plan: super::turn::RuntimeWorldCellPlan,
    ) -> Result<Arc<RuntimeCellAdmission>, SessionError> {
        let snapshot = self.prepare_private_execution_snapshot(execution.clone())?;
        self.admit_world_planned_cell_with_snapshot(execution, snapshot, plan, None)
    }

    pub fn admit_world_planned_cell_with_input(
        &mut self,
        execution: Arc<PrivateExecutionAdmission>,
        plan: super::turn::RuntimeWorldCellPlan,
        input: Option<&PendingHostValueWrite>,
    ) -> Result<Arc<RuntimeCellAdmission>, SessionError> {
        let snapshot =
            self.prepare_private_execution_snapshot_with_input(execution.clone(), input)?;
        self.admit_world_planned_cell_with_snapshot(execution, snapshot, plan, input)
    }

    pub fn admit_world_planned_cell_with_snapshot(
        &mut self,
        execution: Arc<PrivateExecutionAdmission>,
        snapshot: Arc<PrivatePreparationSnapshot>,
        plan: super::turn::RuntimeWorldCellPlan,
        input: Option<&PendingHostValueWrite>,
    ) -> Result<Arc<RuntimeCellAdmission>, SessionError> {
        execution.ensure_snapshot_current(&snapshot)?;
        if !Arc::ptr_eq(&snapshot.execution, &execution) {
            return Err(SessionError::StaleStagedDeclaration);
        }
        let (plan, operand) = plan.into_parts();
        match (&operand, input, plan.operand()) {
            (Some(operand), Some(input), Some(compiler))
                if operand.matches(&execution, input, compiler) => {}
            (None, None, None) => {}
            _ => {
                tracing::debug!(
                    target: "tidepool::session::private_host_mount",
                    "private cell operand differs from its mount"
                );
                return Err(SessionError::StaleStagedDeclaration);
            }
        }
        let scope = execution.private_scope();
        self.reap_admission_leases();
        let stale_reason = if !Arc::ptr_eq(&execution.owner, self.admission_owner()) {
            Some("owner")
        } else if execution.owner_epoch != self.admission_owner().epoch() {
            Some("epoch")
        } else if !self.has_lib() {
            Some("library absent")
        } else if execution.session() != self.lib().session_id() {
            Some("session")
        } else if execution.library != self.lib().compile_view_identity {
            Some("compile view")
        } else {
            None
        };
        if let Some(reason) = stale_reason {
            tracing::debug!(
                target: "tidepool::session::private_host_mount",
                %reason,
                "private cell admission identity refused"
            );
            return Err(SessionError::StaleStagedDeclaration);
        }
        let original_source = execution.compiler_basis.original_source.clone();
        let source = execution.compiler_basis.source.clone();
        let world = snapshot.world();
        let current_private_key = self
            .compile_view_key_in(scope)
            .ok_or(SessionError::StaleStagedDeclaration)?;
        let private_key = match input {
            Some(input) => input.private_hidden_mount_key(self, &execution)?,
            None => current_private_key.clone(),
        };
        let stale_reason = if current_private_key != private_key {
            Some("private key")
        } else if self
            .lib()
            .tips
            .get(&scope)
            .and_then(|tip| tip.inspection_world.as_ref())
            .is_none_or(|association| !Arc::ptr_eq(association, snapshot.association()))
        {
            Some("preparation association")
        } else if !Arc::ptr_eq(plan.world().restoration(), world.restoration()) {
            Some("world restoration")
        } else if plan.world().logical_id() != world.logical_id() {
            Some("world identity")
        } else {
            None
        };
        if let Some(reason) = stale_reason {
            tracing::debug!(
                target: "tidepool::session::private_host_mount",
                %reason,
                "private cell admission world refused"
            );
            return Err(SessionError::StaleStagedDeclaration);
        }
        self.reserve_world_cell(
            scope,
            plan,
            operand,
            RuntimeCellPurpose::PrivateExecution(execution.clone()),
            execution.retained_lexical_scope(),
            original_source,
            source,
            execution.session(),
            private_key,
            Some(snapshot),
        )
    }

    // boundary: see CELL-PREPARATION-BOUNDARY.md
    /// Reserve every ordered source item of a published-world host carrier.
    /// The carrier's own scope is target B; original source A is the current
    /// source transaction lease that the published base validated.
    pub fn admit_world_planned_host_carrier_cell_in(
        &mut self,
        scope: ScopeId,
        plan: super::turn::RuntimeWorldCellPlan,
        binding: String,
        expected: super::resident::HostBindingType,
        authority_digest: [u8; 32],
    ) -> Result<Arc<RuntimeCellAdmission>, SessionError> {
        if self
            .source_lease_in(scope)?
            .private_lifecycle
            .get()
            .is_some()
        {
            return Err(SessionError::WorldCellRequiresServicePreparation);
        }
        self.admit_world_planned_host_carrier(
            scope,
            plan,
            binding,
            expected,
            authority_digest,
            None,
        )
    }

    pub fn admit_world_planned_private_host_carrier(
        &mut self,
        preparation: Arc<PrivateHostCarrierPreparation>,
        plan: super::turn::RuntimeWorldCellPlan,
        binding: String,
        expected: super::resident::HostBindingType,
        authority_digest: [u8; 32],
    ) -> Result<Arc<RuntimeCellAdmission>, SessionError> {
        self.validate_private_host_carrier(&preparation)?;
        self.admit_world_planned_host_carrier(
            preparation.scope(),
            plan,
            binding,
            expected,
            authority_digest,
            Some(preparation),
        )
    }

    fn admit_world_planned_host_carrier(
        &mut self,
        scope: ScopeId,
        plan: super::turn::RuntimeWorldCellPlan,
        binding: String,
        expected: super::resident::HostBindingType,
        authority_digest: [u8; 32],
        private_preparation: Option<Arc<PrivateHostCarrierPreparation>>,
    ) -> Result<Arc<RuntimeCellAdmission>, SessionError> {
        let (plan, operand) = plan.into_parts();
        if operand.is_some() || plan.operand().is_some() {
            return Err(SessionError::StaleStagedDeclaration);
        }
        use tidepool_toolchain::cell_plan::ParsedCellPlanKind as Kind;
        self.reap_admission_leases();
        if !self.has_lib() {
            return Err(SessionError::MissingDeclarationLibrary);
        }
        if !self.scope_tree().is_live(scope) {
            return Err(SessionError::DeadScope(scope));
        }
        let original_source = self
            .source_transaction
            .as_ref()
            .ok_or(super::SourceAdmissionError::MissingTransaction)?
            .source
            .clone();
        let published = self
            .lib()
            .tips
            .get(&scope)
            .and_then(|state| state.inspection_world.as_ref())
            .filter(|association| {
                self.inspection_world_base_is_current(
                    &association.base,
                    &association.base.source,
                    &original_source,
                )
            })
            .cloned()
            .ok_or(SessionError::MissingPublishedCompilerWorld(scope))?;
        let shape_refusal = plan
            .items()
            .iter()
            .enumerate()
            .find(|(index, item)| item.index() != *index)
            .map(|(position, item)| NativeSetupAdmissionFailure::ItemOrder {
                position,
                observed: item.index(),
            })
            .or_else(|| {
                (plan.items().len() != 1
                    || plan.items()[0].kind() != Kind::Bind
                    || plan.items()[0].binders() != [binding.as_str()])
                .then(|| NativeSetupAdmissionFailure::HostCarrierShape {
                    items: plan.items().len(),
                    first: plan.items().first().map(|item| item.kind()),
                    binders: plan.items().first().map_or(0, |item| item.binders().len()),
                })
            });
        if let Some(reason) = shape_refusal {
            return Err(self.native_setup_refusal(scope, reason));
        }
        if !Arc::ptr_eq(plan.world().restoration(), published.world.restoration())
            || plan.world().logical_id() != published.world.logical_id()
        {
            return Err(SessionError::StaleStagedDeclaration);
        }
        let private_key = self
            .compile_view_key_in(scope)
            .ok_or(SessionError::StaleStagedDeclaration)?;
        let retained_scope = self.prepared_lexical_scope_lease(scope)?;
        let session = self.lib().session_id();
        self.reserve_world_cell(
            scope,
            plan,
            None,
            RuntimeCellPurpose::HostCarrier {
                binding,
                expected,
                authority_digest,
                private_preparation,
            },
            retained_scope,
            original_source,
            published.base.source.clone(),
            session,
            private_key,
            None,
        )
    }

    /// Shared reservation kernel for a parser plan issued in a held World.
    /// Every ordered item is reserved once against the current source
    /// transaction origin; the plan claims its one-shot issuance gate.
    fn reserve_world_cell(
        &mut self,
        scope: ScopeId,
        plan: Arc<tidepool_toolchain::cell_plan::CompilerWorldCellPlan>,
        operand: Option<Arc<RuntimeWorldCellOperand>>,
        purpose: RuntimeCellPurpose,
        retained_scope: Arc<RuntimeLexicalScopeLease>,
        original_source: Arc<RuntimeOriginalSource>,
        source: Arc<super::GenerationArtifactReference>,
        session: super::SessionId,
        private_key: super::persistent::CompileViewKey,
        preparation: Option<Arc<PrivatePreparationSnapshot>>,
    ) -> Result<Arc<RuntimeCellAdmission>, SessionError> {
        use tidepool_toolchain::cell_plan::ParsedCellPlanKind as Kind;
        use tidepool_toolchain::checked_cell::CheckedPlannedCellSlot as Slot;

        self.validate_lexical_scope_lease(scope, &retained_scope)?;
        let transaction = self
            .source_transaction
            .as_ref()
            .ok_or(super::SourceAdmissionError::MissingTransaction)?;
        transaction.admit_original(&original_source, Some(retained_scope.clone()))?;
        if plan
            .items()
            .iter()
            .enumerate()
            .any(|(index, item)| item.index() != index)
        {
            return Err(SessionError::StaleStagedDeclaration);
        }
        // The held compiler issues fresh expression names. Check that witness
        // before reserving generations; runtime never scans historical names.
        for item in plan.items() {
            if item.kind() == Kind::Expression
                && plan.expression_observation_name(item.index()).is_none()
            {
                return Err(SessionError::StaleStagedDeclaration);
            }
        }
        let store = self.held_admission_store()?;
        let declaration_count = plan
            .items()
            .iter()
            .filter(|item| matches!(item.kind(), Kind::Prologue | Kind::Declaration))
            .count();
        let native_count = u64::try_from(plan.items().len() - declaration_count)
            .map_err(|_| SessionError::StaleStagedDeclaration)?;
        let last_value = super::generation_lease::requested_end(self.val_gen(), native_count)?;
        super::generation_lease::requested_end(last_value, 1)?;
        let declaration_count_u64 =
            u64::try_from(declaration_count).map_err(|_| SessionError::StaleStagedDeclaration)?;
        self.lib()
            .log
            .generation()
            .0
            .checked_add(declaration_count_u64)
            .ok_or(SessionError::StaleStagedDeclaration)?;
        let mut reserved_generations = Vec::new();
        reserved_generations
            .try_reserve_exact(declaration_count)
            .map_err(|_| SessionError::StaleStagedDeclaration)?;
        let mut slots = Vec::new();
        slots
            .try_reserve_exact(plan.items().len())
            .map_err(|_| SessionError::StaleStagedDeclaration)?;
        let mut expected_bindings = Vec::new();
        expected_bindings
            .try_reserve_exact(plan.items().len())
            .map_err(|_| SessionError::StaleStagedDeclaration)?;
        for item in plan.items() {
            let mut bindings = Vec::new();
            bindings
                .try_reserve_exact(if item.kind() == Kind::Expression {
                    1
                } else {
                    item.binders().len()
                })
                .map_err(|_| SessionError::StaleStagedDeclaration)?;
            expected_bindings.push(bindings);
        }
        {
            let admission = store
                .as_ref()
                .map(|store| store.begin_native_transition())
                .transpose()
                .map_err(|error| SessionError::RecoveryStore(Box::new(error)))?;
            self.ensure_machine_incarnation();
            super::complete_native_transition(admission)?;
        }
        if self.lib().durable_graph.is_some() {
            reserved_generations = self
                .lib_mut()
                .reserve_declaration_generations_durable(declaration_count)?;
        } else {
            for _ in 0..declaration_count {
                reserved_generations.push(self.lib_mut().log.reserve());
            }
        }
        let (initial_value_generation, _) = self.reserve_value_generations(native_count)?;
        let mut declarations = reserved_generations.iter().copied();
        let mut next_value = initial_value_generation.0;
        for (item, expected) in plan.items().iter().zip(&mut expected_bindings) {
            let slot = match item.kind() {
                Kind::Prologue => Slot::Prologue {
                    declaration: declarations
                        .next()
                        .ok_or(SessionError::StaleStagedDeclaration)?
                        .0,
                },
                Kind::Declaration => Slot::Declaration {
                    declaration: declarations
                        .next()
                        .ok_or(SessionError::StaleStagedDeclaration)?
                        .0,
                },
                Kind::Bind => {
                    let value = next_value;
                    next_value += 1;
                    let module = tidepool_repr::SessionModule::val(Generation(value)).module_name();
                    for name in item.binders() {
                        let id = tidepool_repr::SessionVarId::from_extract(
                            tidepool_codegen::prepared_program::session_var_id(&module, name),
                        );
                        expected.push((name.clone(), id));
                    }
                    Slot::Bind { value }
                }
                Kind::Expression => {
                    let capture = next_value;
                    next_value += 1;
                    let observation_name = plan
                        .expression_observation_name(item.index())
                        .ok_or(SessionError::StaleStagedDeclaration)?
                        .to_owned();
                    let module =
                        tidepool_repr::SessionModule::val(Generation(capture)).module_name();
                    let id = tidepool_repr::SessionVarId::from_extract(
                        tidepool_codegen::prepared_program::session_var_id(
                            &module,
                            &observation_name,
                        ),
                    );
                    expected.push((observation_name.clone(), id));
                    Slot::Expression {
                        capture,
                        observation_name,
                    }
                }
            };
            slots.push(slot);
        }
        self.validate_new_binding_ids(expected_bindings.iter().flatten().map(|(_, id)| *id))?;
        let mut authorization = Vec::new();
        purpose.frame_authorization(&mut |bytes| authorization.push(bytes.to_vec()));
        let transaction = self
            .source_transaction
            .as_ref()
            .ok_or(super::SourceAdmissionError::MissingTransaction)?;
        let subject =
            tidepool_toolchain::cell_plan::WorldCellAdmissionSubject::for_source_transaction(
                transaction.origin(),
                *self.admission_owner().identity.as_bytes(),
                self.admission_owner().epoch(),
                session.0,
                scope.0,
                authorization,
            )?;
        let token = plan.issue_admission_token(subject, slots, expected_bindings)?;
        let reservation = plan.reserve(token)?;
        let digest = tidepool_toolchain::checked_cell::AdmissionDigest::finalize(
            *reservation.admission_digest(),
        )
        .map_err(SessionError::Compile)?;
        Ok(Arc::new(RuntimeCellAdmission {
            owner: self.admission_owner().clone(),
            owner_epoch: self.admission_owner().epoch(),
            session,
            purpose,
            _retained_scope: retained_scope,
            prefix_started: std::sync::atomic::AtomicBool::new(false),
            scope,
            reserved_generations,
            initial_value_generation,
            declarations: RuntimeCellDeclarations {
                root: self.lib().root.clone(),
                declaration_baseline: if !reservation.slots().iter().any(|slot| {
                    matches!(slot,
                    tidepool_toolchain::checked_cell::CheckedPlannedCellSlot::Prologue { .. } |
                    tidepool_toolchain::checked_cell::CheckedPlannedCellSlot::Declaration { .. })
                }) {
                    None
                } else {
                    super::lexical_projection::DeclarationProjectionBaseline::capture(
                        self.lib(),
                        self.lib().scope_tip(scope),
                    )?
                },
                prepared_declarations: std::sync::OnceLock::new(),
            },
            compiler_basis: RuntimeCellCompilerBasis::World {
                reservation,
                _operand: operand,
                preparation,
                private_key,
                source,
                original_source,
            },
            digest,
        }))
    }

    /// Capture the current private environment before freezing its recipe.
    /// Trusted setup mounts may extend this scope before whole-cell admission.
    pub fn compile_view_for_execution(
        &self,
        execution: &PrivateExecutionAdmission,
    ) -> Result<SessionCompileView, SessionError> {
        if !Arc::ptr_eq(&execution.owner, self.admission_owner())
            || execution.owner_epoch != self.admission_owner().epoch()
            || !self.has_lib()
            || execution.session() != self.lib().session_id()
            || execution.library != self.lib().compile_view_identity
        {
            return Err(SessionError::StaleStagedDeclaration);
        }
        execution.view()?;
        super::generation_lease::requested_end(self.val_gen(), 1)?;
        self.compile_view_in(execution.private_scope())
            .map(SessionCompileView::with_scoped_injection)
            .ok_or(SessionError::DeadScope(execution.private_scope()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::{ModuleEnv, SessionId, SessionLib};

    fn native_source_action<T>(
        session: &mut PersistentSession,
        source: &Arc<RuntimeLexicalScopeLease>,
        action: impl FnOnce(&mut PersistentSession) -> T,
    ) -> T {
        let settled = session
            .with_source_compiler_transaction(
                source.original_source().clone(),
                tidepool_toolchain::artifacts::CompileWorkload::Foreground,
                crate::CompilerTransactionCancellation::new(),
                None,
                |_| {},
                |session, _| action(session),
            )
            .unwrap()
            .into_outcome();
        assert_eq!(settled.close, crate::CompilerTransactionClose::NotStarted);
        settled.action
    }

    #[test]
    fn original_source_survives_lexical_resource_release_without_retaining_native_roots() {
        let root = tempfile::tempdir().unwrap();
        let lib =
            SessionLib::open(SessionId(984), root.path(), ModuleEnv::standalone_default()).unwrap();
        let mut session = PersistentSession::new(Some(lib), 1024 * 1024);
        let lease = session.mint_isolated_scope_with_lease().unwrap();
        let scope = lease.scope();
        let original = lease.original_source().clone();
        let gate = original.source_admission().clone();
        let (id, handle) = native_source_action(&mut session, &lease, |session| {
            let value = crate::session::prepared::tests::rooted_publication_fixture(
                session,
                "sourceOwned",
                992,
            );
            let id = value.id;
            let handle = value.value.handle;
            session.bind_in(scope, value).unwrap();
            (id, handle)
        });
        assert!(session.bindings().get(id).is_some());
        let roots = session.persistent_roots_count();
        assert!(roots > 0);
        drop(lease);
        session.reap_admission_leases();
        assert!(!session.scope_tree().is_live(scope));
        assert!(session.bindings().get(id).is_none());
        assert!(session
            .prepared()
            .unwrap()
            .prepared_handle_of(handle.raw())
            .is_none());
        assert!(session.persistent_roots_count() < roots);
        original.with_open_source_admission(|| ()).unwrap();
        drop(original);
        session.reap_admission_leases();
        assert!(!session
            .admission_owner()
            .lexical_leases
            .lock()
            .contains_key(&scope));
        assert!(gate.admit().is_err());
    }

    #[test]
    fn original_source_and_target_guard_refuses_cutoff_and_stale_target_independently() {
        let mut session = PersistentSession::new(None, 1024);
        let lease = session.mint_isolated_scope_with_lease().unwrap();
        let original = lease.original_source().clone();
        let scope = lease.scope();
        let target = session.mint_isolated_scope_with_lease().unwrap();
        original.with_open_target(&target, || ()).unwrap();
        drop(lease);
        session.reap_admission_leases();
        original.with_open_target(&target, || ()).unwrap();
        let run = || {
            original.with_source_compiler_transaction(
                tidepool_toolchain::artifacts::CompileWorkload::Foreground,
                crate::CompilerTransactionCancellation::new(),
                None,
                |_| {},
                |owner, _| owner.admit(Some(target.clone())),
            )
        };
        let admitted = run().unwrap().into_outcome();
        assert_eq!(admitted.close, crate::CompilerTransactionClose::NotStarted);
        admitted.action.unwrap();
        let _fence = original.source_admission().fence().unwrap();
        assert!(matches!(
            run().unwrap().into_outcome().action,
            Err(super::super::SourceAdmissionError::Closed { .. })
        ));
        let retired = session.retire_scope(scope);
        assert_eq!(retired.scopes_retired, 0);
        assert_eq!(retired.bindings_retired, 0);
        assert_eq!(retired.roots_released, 0);
        assert!(matches!(
            original.with_open_target(&target, || ()),
            Err(super::super::SourceAdmissionError::StaleSourceLease)
        ));
        assert!(matches!(
            run(),
            Err(super::super::SourceAdmissionError::StaleSourceLease)
        ));
        target
            .original_source()
            .with_open_target(&target, || ())
            .unwrap();
        session.retire_scope(target.scope());
        assert!(target
            .original_source()
            .with_open_target(&target, || ())
            .is_err());
    }

    #[test]
    fn automatic_ancestor_reap_invalidates_child_target_but_keeps_its_source() {
        let mut session = PersistentSession::new(None, 1024);
        let parent = session.mint_isolated_scope();
        let child = session.mint_scope(parent).unwrap();
        let child_lease = session.adopt_initial_notebook_scope(child).unwrap();
        assert!(child_lease.prerequisites().is_empty());
        let child_source = child_lease.original_source().clone();
        let parent_lease = session.adopt_initial_notebook_scope(parent).unwrap();
        let target = session.mint_isolated_scope_with_lease().unwrap();
        child_source.with_open_target(&child_lease, || ()).unwrap();
        drop(parent_lease);
        session.reap_admission_leases();
        assert!(!session.scope_tree().is_live(parent));
        assert!(!session.scope_tree().is_live(child));
        assert!(child_lease.validate_lexical_issuer().is_err());
        assert!(child_source.with_open_target(&child_lease, || ()).is_err());
        child_source.with_open_target(&target, || ()).unwrap();
        assert!(session.adopt_initial_notebook_scope(child).is_err());
        child_source.with_open_target(&target, || ()).unwrap();
        let retired = session.retire_scope(child);
        assert_eq!(retired.scopes_retired, 0);
        assert_eq!(retired.roots_released, 0);
        assert!(child_source.with_open_target(&target, || ()).is_err());
    }

    #[test]
    fn token_only_transaction_adds_source_prerequisite_without_a_lexical_parent() {
        let mut session = PersistentSession::new(None, 1024);
        let original_lease = session.mint_isolated_scope_with_lease().unwrap();
        let original_scope = original_lease.scope();
        let original = original_lease.original_source().clone();
        let settled = session
            .with_source_compiler_transaction(
                original.clone(),
                tidepool_toolchain::artifacts::CompileWorkload::Foreground,
                crate::CompilerTransactionCancellation::new(),
                None,
                |_| {},
                |session, _| session.mint_isolated_scope_with_lease(),
            )
            .unwrap()
            .into_outcome();
        assert_eq!(settled.close, crate::CompilerTransactionClose::NotStarted);
        let target = settled.action.unwrap();
        assert!(target.prerequisites().is_empty());
        assert!(target
            .original_source()
            .prerequisites
            .iter()
            .any(|source| Arc::ptr_eq(source, &original)));
        drop(original_lease);
        session.reap_admission_leases();
        assert!(!session.scope_tree().is_live(original_scope));
        target.validate_lexical_issuer().unwrap();
        target.with_open_source_admission(|| ()).unwrap();
        let fence = original.source_admission().fence().unwrap();
        assert!(target.with_open_source_admission(|| ()).is_err());
        target.validate_lexical_issuer().unwrap();
        fence.reopen(&original).unwrap();
        target.with_open_source_admission(|| ()).unwrap();
    }

    #[test]
    fn uncertain_token_transaction_keeps_source_authority_and_target_resources_separately() {
        let mut session = PersistentSession::new(None, 1024);
        let original_lease = session.mint_isolated_scope_with_lease().unwrap();
        let original_scope = original_lease.scope();
        let original = original_lease.original_source().clone();
        let target = session.mint_isolated_scope_with_lease().unwrap();
        let target_scope = target.scope();
        let source_observation = Arc::downgrade(&original);
        let target_observation = Arc::downgrade(&target);
        drop(original_lease);
        session.reap_admission_leases();
        assert!(!session.scope_tree().is_live(original_scope));
        let close = std::cell::Cell::new(None);
        let panic = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _ = original.with_source_compiler_transaction(
                tidepool_toolchain::artifacts::CompileWorkload::Foreground,
                crate::CompilerTransactionCancellation::new(),
                None,
                |settled| close.set(Some(settled)),
                |owner, _| -> () {
                    owner.admit(Some(target.clone())).unwrap();
                    panic!("admitted operation unwinds before returning");
                },
            );
        }));
        assert!(panic.is_err());
        assert_eq!(
            close.take(),
            Some(crate::CompilerTransactionClose::NotStarted)
        );
        drop(original);
        drop(target);
        session.reap_admission_leases();
        let source = source_observation.upgrade().unwrap();
        let retained_target = target_observation.upgrade().unwrap();
        assert!(!session.scope_tree().is_live(original_scope));
        assert!(session.scope_tree().is_live(target_scope));
        assert!(matches!(
            source.with_open_target(&retained_target, || ()),
            Err(super::super::SourceAdmissionError::Unconfirmed { .. })
        ));
        drop(retained_target);
        session.retire_scope(target_scope);
        assert!(target_observation.upgrade().is_none());
        drop(source);
        session.retire_scope(original_scope);
        assert!(source_observation.upgrade().is_none());
    }

    #[test]
    fn retired_inherited_source_cannot_reappear_through_its_open_shared_gate() {
        let mut session = PersistentSession::new(None, 1024);
        let parent = session.mint_notebook_scope_with_lease(None).unwrap();
        let child_scope = session.mint_scope(parent.scope()).unwrap();
        let child = session.adopt_initial_notebook_scope(child_scope).unwrap();
        let source = child.original_source().clone();
        let target = session.mint_isolated_scope_with_lease().unwrap();
        assert!(source
            .source_admission()
            .same_owner(parent.source_admission()));
        assert!(!Arc::ptr_eq(&source, parent.original_source()));
        source.with_open_target(&target, || ()).unwrap();
        session.retire_scope(child_scope);
        assert!(source.with_open_target(&target, || ()).is_err());
        parent
            .original_source()
            .with_open_target(&target, || ())
            .unwrap();
        assert!(session.adopt_initial_notebook_scope(child_scope).is_err());
        assert!(source.with_open_target(&target, || ()).is_err());
        parent
            .original_source()
            .with_open_target(&target, || ())
            .unwrap();
    }

    #[test]
    fn world_private_admission_requires_the_actual_transaction_original() {
        let root = tempfile::tempdir().unwrap();
        let lib =
            SessionLib::open(SessionId(984), root.path(), ModuleEnv::standalone_default()).unwrap();
        let mut session = PersistentSession::new(Some(lib), 1024 * 1024);
        let scope_a = session.mint_isolated_scope();
        let source_a = session.adopt_initial_notebook_scope(scope_a).unwrap();
        let scope_b = session.mint_isolated_scope();
        let source_b = session.adopt_initial_notebook_scope(scope_b).unwrap();
        let reference = super::super::GenerationArtifactReference::capture(b"unopened".to_vec());
        let scope_count = session.scope_tree().len();
        let settled = session
            .with_source_compiler_transaction(
                source_b.original_source().clone(),
                tidepool_toolchain::artifacts::CompileWorkload::Foreground,
                crate::CompilerTransactionCancellation::new(),
                None,
                |_| {},
                |session, _| {
                    let foreign = session.begin_world_private_execution(
                        scope_a,
                        &reference,
                        source_a.original_source(),
                    );
                    let matching = session.begin_world_private_execution(
                        scope_b,
                        &reference,
                        source_b.original_source(),
                    );
                    (foreign, matching)
                },
            )
            .unwrap()
            .into_outcome();
        assert_eq!(settled.close, crate::CompilerTransactionClose::NotStarted);
        assert!(matches!(
            settled.action.0,
            Err(SessionError::SourceAdmission(
                super::super::SourceAdmissionError::DifferentOriginalSource
            ))
        ));
        assert!(
            matches!(settled.action.1, Err(SessionError::MissingPublishedCompilerWorld(scope))
            if scope == scope_b)
        );
        assert_eq!(session.scope_tree().len(), scope_count);
    }

    #[test]
    fn owner_transfer_guard_preserves_refusal_and_fences_uncertain_or_unwinding_epochs() {
        for disposition in 0..3 {
            let mut session = PersistentSession::new(None, 1024);
            let scope = session.mint_isolated_scope_with_lease().unwrap();
            let owner = session.admission_owner().clone();
            let next = owner.epoch() + 1;
            // Exercise the actual guard used around durable commit. Unknown
            // and panic here are disposition injection, not physical IO loss.
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                owner.with_source_owner_transfer(next, |transition| match disposition {
                    0 => transition.refused(),
                    1 => (),
                    _ => panic!("injected owner commit unwind"),
                });
            }));
            assert_eq!(result.is_err(), disposition == 2);
            let mut enqueued = false;
            let admission = scope.with_open_source_admission(|| enqueued = true);
            assert_eq!(enqueued, disposition == 0);
            if disposition == 0 {
                admission.unwrap();
            } else {
                assert!(matches!(
                    admission,
                    Err(super::super::SourceAdmissionError::StaleSourceLease)
                ));
            }
            assert!(
                scope.source_admission().admit().is_ok(),
                "the raw gate remaining open must not substitute for the exact issuer epoch"
            );
        }
    }

    #[test]
    fn live_input_commitment_binds_native_payload_and_semantic_input_identity() {
        use ciborium::value::Value;
        let text = |value: &str| Value::Text(value.into());
        let witness = |payload: u8, shape: &str| {
            let mut structure = Vec::new();
            ciborium::into_writer(
                &Value::Array(vec![text("literal"), text("nat"), text(shape)]),
                &mut structure,
            )
            .unwrap();
            let mut bytes = Vec::new();
            // Structural codec evidence; this native payload is not executed.
            ciborium::into_writer(
                &Value::Array(vec![
                    text("TPCANONICALINPUTTYPE1"),
                    text("1"),
                    Value::Array(vec![
                        text("TPCHECKEDSIGNATURE2"),
                        text("activation-input"),
                        text("presentation"),
                        Value::Bytes(vec![payload]),
                        Value::Array(vec![]),
                    ]),
                    Value::Bytes(structure),
                    Value::Array(vec![]),
                ]),
                &mut bytes,
            )
            .unwrap();
            let encoded = bytes
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect::<String>();
            Arc::new(serde_json::from_value::<tidepool_toolchain::checked_cell::CanonicalInputTypeWitness>(serde_json::Value::String(encoded)).unwrap())
        };
        let original = witness(1, "1");
        let payload_changed = witness(2, "1");
        let semantic_changed = witness(1, "2");
        assert_eq!(original, payload_changed);
        assert_eq!(original.commitment(), payload_changed.commitment());
        assert_ne!(
            original.metadata_digest(),
            payload_changed.metadata_digest()
        );
        assert_ne!(original.commitment(), semantic_changed.commitment());
        let digest = |witness: Arc<tidepool_toolchain::checked_cell::CanonicalInputTypeWitness>| {
            let mut digest = blake3::Hasher::new();
            for bytes in [witness.metadata_digest(), witness.commitment()] {
                digest.update(&bytes);
            }
            *digest.finalize().as_bytes()
        };
        assert_ne!(digest(original.clone()), digest(payload_changed));
        assert_ne!(digest(original), digest(semantic_changed));
    }

    #[test]
    fn initial_interface_inventory_refuses_missing_extra_duplicate_and_changed_bytes() {
        let first = ValueInterfaceSnapshot {
            module: tidepool_repr::SessionModule::val(Generation(1)),
            bytes: Arc::from([1, 2, 3]),
        };
        let second = ValueInterfaceSnapshot {
            module: tidepool_repr::SessionModule::val(Generation(2)),
            bytes: Arc::from([4, 5, 6]),
        };
        let admitted = [first.clone(), second.clone()];
        assert!(!initial_interfaces_match(
            &admitted,
            [
                (&second.module, &second.bytes),
                (&first.module, &first.bytes)
            ],
        ));
        assert!(initial_interfaces_match(
            &admitted,
            [
                (&first.module, &first.bytes),
                (&second.module, &second.bytes)
            ],
        ));
        let equal_bytes: Arc<[u8]> = Arc::from([1, 2, 3]);
        assert!(!Arc::ptr_eq(&first.bytes, &equal_bytes));
        assert!(initial_interfaces_match(
            &admitted,
            [
                (&first.module, &equal_bytes),
                (&second.module, &second.bytes)
            ],
        ));
        assert!(!initial_interfaces_match(
            &admitted,
            [(&first.module, &first.bytes)],
        ));
        let extra = tidepool_repr::SessionModule::val(Generation(3));
        assert!(!initial_interfaces_match(
            &admitted,
            [
                (&first.module, &first.bytes),
                (&second.module, &second.bytes),
                (&extra, &first.bytes),
            ],
        ));
        assert!(!initial_interfaces_match(
            &admitted,
            [(&first.module, &first.bytes), (&first.module, &first.bytes)],
        ));
        let changed: Arc<[u8]> = Arc::from([1, 2, 4]);
        assert!(!initial_interfaces_match(
            &admitted,
            [(&first.module, &changed), (&second.module, &second.bytes)],
        ));
        assert!(!initial_interfaces_match(
            &[first.clone(), first],
            [(&second.module, &second.bytes)],
        ));
        assert!(initial_interfaces_match::<ValueInterfaceSnapshot>(&[], []));
        assert!(!initial_interfaces_match::<ValueInterfaceSnapshot>(
            &[],
            [(&second.module, &second.bytes)],
        ));
    }

    #[test]
    fn native_admission_uses_exact_scoped_roots_and_keeps_its_original_ledger() {
        use tidepool_repr::{SessionModule, SessionVarId};

        let root = tempfile::tempdir().unwrap();
        let lib =
            SessionLib::open(SessionId(990), root.path(), ModuleEnv::standalone_default()).unwrap();
        let mut session = PersistentSession::new(Some(lib), 1024 * 1024);
        let source = session.mint_notebook_scope_with_lease(None).unwrap();
        let (public, retained, ledger, original_id, _sibling_source) =
            native_source_action(&mut session, &source, |session| {
                let public = session.mint_scope(ScopeId::ROOT).unwrap();
                let _public_source = session.adopt_initial_notebook_scope(public).unwrap();
                let sibling = session.mint_scope(ScopeId::ROOT).unwrap();
                let sibling_source = session.adopt_initial_notebook_scope(sibling).unwrap();
                let mut original =
                    crate::session::prepared::tests::rooted_publication_fixture(session, "x", 910);
                original.value.identity.module = original.module.module_name();
                let original_identity = original.value.identity.clone();
                let original_id = original.id;
                session.bind_in(public, original).unwrap();
                let mut historical =
                    crate::session::prepared::tests::rooted_publication_fixture(session, "x", 911);
                historical.value.identity.module = historical.module.module_name();
                let historical_identity = historical.value.identity.clone();
                session.bind_in(public, historical).unwrap();

                // A sibling sharing a module is still a different exact binding owner.
                let mut foreign = crate::session::prepared::tests::rooted_publication_fixture(
                    session, "foreign", 912,
                );
                foreign.module = SessionModule::val(Generation(910));
                foreign.value.identity.module = foreign.module.module_name();
                foreign.value.identity.occurrence = "foreign".into();
                let foreign_identity = foreign.value.identity.clone();
                session.bind_in(sibling, foreign).unwrap();
                let retained = session.retain_lexical_scope(public).unwrap();
                let ledger = Arc::new(session.capture_admitted_native_imports(public).unwrap());
                assert!(ledger.entries.contains(&(original_identity.clone(), 910)));
                assert!(ledger.entries.contains(&(historical_identity, 911)));
                assert!(!ledger
                    .entries
                    .iter()
                    .any(|(identity, _)| identity == &foreign_identity));

                let mut replacement =
                    crate::session::prepared::tests::rooted_publication_fixture(session, "x", 913);
                replacement.value.identity.module = replacement.module.module_name();
                let replacement_identity = replacement.value.identity.clone();
                session.bind_in(public, replacement).unwrap();
                assert!(session
                    .capture_admitted_native_imports(public)
                    .unwrap()
                    .entries
                    .contains(&(replacement_identity.clone(), 913)));
                assert!(!ledger.entries.contains(&(replacement_identity, 913)));
                assert!(ledger.entries.contains(&(original_identity.clone(), 910)));
                (public, retained, ledger, original_id, sibling_source)
            });
        session.retire_scope(public);
        assert!(session.bindings().get(original_id).is_some());
        drop(ledger);
        drop(retained);
        session.reap_admission_leases();
        assert!(session.bindings().get(original_id).is_none());
        assert!(session
            .bindings()
            .get(SessionVarId::from_extract(912))
            .is_some());
    }

    #[test]
    fn raw_interface_owner_tracks_hidden_binding_leases_without_checked_admission() {
        let root = tempfile::tempdir().unwrap();
        let lib =
            SessionLib::open(SessionId(988), root.path(), ModuleEnv::standalone_default()).unwrap();
        let mut session = PersistentSession::new(Some(lib), 1024 * 1024);
        let public = session.mint_scope(ScopeId::ROOT).unwrap();
        let mut value = crate::session::prepared::tests::rooted_publication_fixture(
            &mut session,
            "captured",
            910,
        );
        value.value.identity.module = value.module.module_name();
        let module = value.module;
        let id = value.id;
        session.bind_in(public, value).unwrap();
        let scratch = tempfile::tempdir().unwrap();
        let input = scratch.path().join("interface.hi");
        std::fs::write(&input, [1, 2, 3]).unwrap();
        // Fixture bytes test runtime custody. Production receives the opaque
        // stamped CheckedValueArtifact after matching the actual native delta.
        let bytes: Arc<[u8]> = std::fs::read(input).unwrap().into();
        let weak = Arc::downgrade(&bytes);
        session.retain_fixture_value_interface(module, bytes.clone());
        let source = session.bindings().get(id).unwrap();
        let mut alias_value = source.value.clone();
        alias_value.identity.occurrence = "capturedAlias".into();
        let alias = tidepool_codegen::binding_table::BindingEntry {
            name: tidepool_repr::BindingName("capturedAlias".into()),
            id: tidepool_repr::SessionVarId::from_extract(911),
            module,
            value: alias_value,
            type_display: source.type_display.clone(),
            defining_expr: None,
            scope: public,
        };
        tidepool_testing::with_settlement(|settlement| {
            session.publish_alias_in(public, alias, id, settlement)
        })
        .unwrap();
        let captured_lease = session.retain_lexical_scope(public).unwrap();
        drop(bytes);
        drop(scratch);
        assert!(!root.path().join(module.relative_hi_path()).exists());
        let private = Arc::new(session.begin_private_execution(public).unwrap());
        assert!(matches!(session.admit_cell_for_execution(
            private.clone(), 0, Arc::new(()), [7; 32], [8; 32], Vec::new(),
        ), Err(SessionError::MissingRetainedValueInterface(owner)) if owner == module));
        session.retire_scope(public);
        assert!(session.bindings().get(id).is_some());
        assert!(session.retained_value_interface(module).is_some());
        drop(private);
        session.reap_admission_leases();
        // The captured alias keeps the hidden original and its type evidence.
        assert!(session.bindings().get(id).is_some());
        assert!(session.retained_value_interface(module).is_some());
        drop(captured_lease);
        session.reap_admission_leases();
        assert!(session.bindings().get(id).is_none());
        assert!(session.retained_value_interface(module).is_none());
        assert!(weak.upgrade().is_none());
        let lib =
            SessionLib::open(SessionId(989), root.path(), ModuleEnv::standalone_default()).unwrap();
        let mut restarted = PersistentSession::new(Some(lib), 1024 * 1024);
        let private = Arc::new(restarted.begin_private_execution(ScopeId::ROOT).unwrap());
        let admitted = restarted
            .admit_cell_for_execution(private, 0, Arc::new(()), [7; 32], [8; 32], Vec::new())
            .unwrap();
        assert!(admitted.interfaces().unwrap().is_empty());
    }

    #[test]
    fn checked_admission_refuses_disk_only_interface_for_live_value() {
        let root = tempfile::tempdir().unwrap();
        let lib =
            SessionLib::open(SessionId(987), root.path(), ModuleEnv::standalone_default()).unwrap();
        let mut session = PersistentSession::new(Some(lib), 1024 * 1024);
        let mut value = crate::session::prepared::tests::rooted_publication_fixture(
            &mut session,
            "unregistered",
            912,
        );
        value.value.identity.module = value.module.module_name();
        let module = value.module;
        session.bind(value).unwrap();
        let path = root.path().join(module.relative_hi_path());
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, [9, 9, 9]).unwrap();
        let private = Arc::new(session.begin_private_execution(ScopeId::ROOT).unwrap());
        assert!(matches!(
            session.admit_cell_for_execution(private, 0, Arc::new(()), [7; 32], [8; 32], Vec::new()),
            Err(SessionError::MissingRetainedValueInterface(owner)) if owner == module
        ));
    }

    #[test]
    fn inspection_capture_refuses_uncertified_retained_and_legacy_inputs() {
        let root = tempfile::tempdir().unwrap();
        let lib =
            SessionLib::open(SessionId(985), root.path(), ModuleEnv::standalone_default()).unwrap();
        let mut session = PersistentSession::new(Some(lib), 1024 * 1024);
        let mut value = crate::session::prepared::tests::rooted_publication_fixture(
            &mut session,
            "untrusted",
            915,
        );
        value.value.identity.module = value.module.module_name();
        let module = value.module;
        session.bind(value).unwrap();
        session.retain_fixture_value_interface(module, Arc::from([1, 2, 3]));
        let view = session.compile_view_in(ScopeId::ROOT).unwrap();
        assert!(matches!(session.capture_value_interfaces(&view),
            Err(SessionError::MissingRetainedValueInterface(owner)) if owner == module));
        session.mark_legacy_value_interface(module);
        assert!(matches!(session.capture_value_interfaces(&view),
            Err(SessionError::MissingRetainedValueInterface(owner)) if owner == module));
    }

    #[test]
    fn checked_admission_refuses_explicit_legacy_disk_origin() {
        let root = tempfile::tempdir().unwrap();
        let lib =
            SessionLib::open(SessionId(986), root.path(), ModuleEnv::standalone_default()).unwrap();
        let mut session = PersistentSession::new(Some(lib), 1024 * 1024);
        let mut value = crate::session::prepared::tests::rooted_publication_fixture(
            &mut session,
            "ordinary",
            914,
        );
        value.value.identity.module = value.module.module_name();
        let module = value.module;
        session.bind(value).unwrap();
        session.mark_legacy_value_interface(module);
        let path = root.path().join(module.relative_hi_path());
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, [9, 8, 7]).unwrap();
        let private = Arc::new(session.begin_private_execution(ScopeId::ROOT).unwrap());
        assert!(session.retained_checked_value_artifact(module).is_none());
        assert!(matches!(session.admit_cell_for_execution(
            private, 0, Arc::new(()), [7; 32], [8; 32], Vec::new(),
        ), Err(SessionError::MissingRetainedValueInterface(owner)) if owner == module));
    }

    #[test]
    fn long_settled_delta_release_is_iterative_and_preserves_older_consumers() {
        std::thread::Builder::new()
            .stack_size(64 * 1024)
            .spawn(|| {
                let mut native = None;
                let mut interfaces = None;
                let bytes: Arc<[u8]> = Arc::from([1]);
                let mut retained = None;
                let mut oldest = None;
                for generation in 0..50_000 {
                    native = Some(Arc::new(CheckedNativeDelta {
                        previous: native,
                        imports: Vec::new(),
                        bindings: Vec::new(),
                    }));
                    interfaces = Some(Arc::new(CheckedInterfaceDelta {
                        previous: interfaces,
                        interface: ValueInterfaceSnapshot {
                            module: tidepool_repr::SessionModule::val(Generation(generation)),
                            bytes: bytes.clone(),
                        },
                    }));
                    if generation == 0 {
                        oldest = Some((
                            Arc::downgrade(native.as_ref().unwrap()),
                            Arc::downgrade(interfaces.as_ref().unwrap()),
                        ));
                    }
                    if generation == 25_000 {
                        retained = Some((native.clone(), interfaces.clone()));
                    }
                }
                let (old_native, old_interface) = oldest.unwrap();
                drop(native);
                drop(interfaces);
                assert!(old_native.upgrade().is_some());
                assert!(old_interface.upgrade().is_some());
                drop(retained);
                assert!(old_native.upgrade().is_none());
                assert!(old_interface.upgrade().is_none());
                assert_eq!(Arc::strong_count(&bytes), 1);
            })
            .unwrap()
            .join()
            .unwrap();
    }

    #[test]
    fn settled_delta_release_handles_concurrent_last_readers_on_small_stacks() {
        let mut native = None;
        let mut interfaces = None;
        let mut readers = Vec::new();
        let bytes: Arc<[u8]> = Arc::from([1]);
        for generation in 0..50_000 {
            native = Some(Arc::new(CheckedNativeDelta {
                previous: native,
                imports: Vec::new(),
                bindings: Vec::new(),
            }));
            interfaces = Some(Arc::new(CheckedInterfaceDelta {
                previous: interfaces,
                interface: ValueInterfaceSnapshot {
                    module: tidepool_repr::SessionModule::val(Generation(generation)),
                    bytes: bytes.clone(),
                },
            }));
            readers.push((native.clone(), interfaces.clone()));
        }
        let old_native = Arc::downgrade(readers[0].0.as_ref().unwrap());
        let old_interface = Arc::downgrade(readers[0].1.as_ref().unwrap());
        let barrier = Arc::new(std::sync::Barrier::new(2));
        let release_barrier = barrier.clone();
        let release = std::thread::Builder::new()
            .stack_size(64 * 1024)
            .spawn(move || {
                release_barrier.wait();
                drop(native);
                drop(interfaces);
            })
            .unwrap();
        let readers = std::thread::Builder::new()
            .stack_size(64 * 1024)
            .spawn(move || {
                barrier.wait();
                for reader in readers.into_iter().rev() {
                    drop(reader);
                }
            })
            .unwrap();
        release.join().unwrap();
        readers.join().unwrap();
        assert!(old_native.upgrade().is_none());
        assert!(old_interface.upgrade().is_none());
        assert_eq!(Arc::strong_count(&bytes), 1);
    }

    #[test]
    fn selected_native_imports_keep_exact_scoped_roots_and_refuse_sibling_only_values() {
        let mut session = PersistentSession::new(None, 1024 * 1024);
        let source = session.mint_notebook_scope_with_lease(None).unwrap();
        native_source_action(&mut session, &source, |session| {
            let public = session.mint_isolated_scope();
            let _public_source = session.adopt_initial_notebook_scope(public).unwrap();
            let first =
                super::super::prepared::tests::rooted_publication_fixture(session, "value", 460);
            let identity = first.value.identity.clone();
            let first_root = first.value.handle.raw().0;
            session.bind_in(public, first).unwrap();
            let private = session.mint_detached_scope(public).unwrap();
            let replacement =
                super::super::prepared::tests::rooted_publication_fixture(session, "value", 461);
            session.bind_in(public, replacement).unwrap();
            let sibling = session.mint_isolated_scope();
            let _sibling_source = session.adopt_initial_notebook_scope(sibling).unwrap();
            let mut foreign =
                super::super::prepared::tests::rooted_publication_fixture(session, "value", 462);
            foreign.module = tidepool_repr::SessionModule::val(Generation(460));
            let foreign_root = foreign.value.handle.raw().0;
            assert_ne!(first_root, foreign_root);
            session.bind_in(sibling, foreign).unwrap();
            let selected = session
                .capture_selected_native_imports(private, [(&identity, 460)])
                .unwrap();
            assert_eq!(selected.entries, [(identity.clone(), 460)]);
            assert_eq!(
                selected.owners.get(&(identity.clone(), 460)),
                Some(&first_root)
            );
            assert!(session
                .capture_selected_native_imports(public, [(&identity, 461)])
                .is_ok());
            assert!(matches!(
                session.capture_selected_native_imports(private, [(&identity, 461)]),
                Err(SessionError::StaleStagedDeclaration)
            ), "a later value that is live in the issuer scope cannot replace the captured generation");
            assert!(
                matches!(
                    session.capture_selected_native_imports(private, [(&identity, 9999)]),
                    Err(SessionError::StaleStagedDeclaration)
                ),
                "an absent runtime generation is refused before native execution"
            );
            let isolated = session.mint_isolated_scope();
            assert!(matches!(
                session.capture_selected_native_imports(isolated, [(&identity, 460)]),
                Err(SessionError::StaleStagedDeclaration)
            ));
            let empty = session
                .capture_selected_native_imports(private, [])
                .unwrap();
            assert!(empty.entries.is_empty());
            assert!(empty.owners.is_empty());
        });
    }

    fn bind_native_delta_fixture(
        session: &mut PersistentSession,
        name: &str,
        generation: u64,
    ) -> SymbolIdentity {
        let mut entry =
            crate::session::prepared::tests::rooted_publication_fixture(session, name, generation);
        entry.value.identity.module = entry.module.module_name();
        entry.value.identity.namespace = "value".into();
        entry.value.identity.occurrence = entry.name.0.clone();
        let identity = entry.value.identity.clone();
        session.bind_in(ScopeId::ROOT, entry).unwrap();
        identity
    }

    #[test]
    fn settled_native_delta_excludes_ambient_exports_and_reuses_only_owned_roots() {
        use tidepool_repr::execution_schema::testing;
        let mut session = PersistentSession::new(None, 1024 * 1024);
        let source = session.mint_notebook_scope_with_lease(None).unwrap();
        native_source_action(&mut session, &source, |session| {
            let baseline = Arc::new(
                session
                    .capture_admitted_native_imports(ScopeId::ROOT)
                    .unwrap(),
            );
            let mut index = CheckedNativeIndex::new(baseline.clone());
            let ambient_identity = testing::identity("Fixture", "entry");
            session
                .install_prepared(testing::prepare(testing::wire_program()).unwrap(), None)
                .unwrap();
            assert!(matches!(
                session.capture_selected_native_imports(ScopeId::ROOT, [(&ambient_identity, 0)]),
                Err(SessionError::StaleStagedDeclaration)
            ));
            let own_identity = bind_native_delta_fixture(session, "answer", 924);
            let delta = session
                .capture_checked_native_delta(ScopeId::ROOT, 924, [("answer", 924)].into_iter())
                .unwrap();
            assert_eq!(delta.imports.len(), 1);
            assert_eq!(delta.imports[0].identity, own_identity);
            let (ledger, added) = CheckedNativeImports::base(baseline.clone())
                .append(delta, &index)
                .unwrap();
            index.extend(added);
            assert!(!ledger
                .imports()
                .any(|(identity, _)| identity == &ambient_identity));
            assert!(ledger
                .imports()
                .any(|(identity, _)| identity == &own_identity));
            let absent = session
                .capture_checked_native_delta(ScopeId::ROOT, 924, [].into_iter())
                .unwrap();
            assert!(absent.imports.is_empty());
            assert!(absent.bindings.is_empty());
            let (unchanged, added) = ledger.append(absent, &index).unwrap();
            assert!(added.is_empty());
            assert!(Arc::ptr_eq(
                ledger.tail.as_ref().unwrap(),
                unchanged.tail.as_ref().unwrap()
            ));
            assert!(Arc::ptr_eq(&ledger.base, &baseline));
            assert!(matches!(
                session.capture_checked_native_delta(
                    ScopeId::ROOT,
                    925,
                    [("answer", 924)].into_iter()
                ),
                Err(SessionError::StaleStagedDeclaration)
            ));
        });
    }

    #[test]
    fn checked_native_index_shares_live_baseline_and_keeps_exact_delta_owners() {
        let mut session = PersistentSession::new(None, 1024 * 1024);
        let source = session.mint_notebook_scope_with_lease(None).unwrap();
        native_source_action(&mut session, &source, |session| {
            let value_identity = bind_native_delta_fixture(session, "answer", 924);
            let baseline = Arc::new(
                session
                    .capture_admitted_native_imports(ScopeId::ROOT)
                    .unwrap(),
            );
            let retained = Arc::downgrade(&baseline);
            let mut index = CheckedNativeIndex::new(baseline.clone());
            let ledger = CheckedNativeImports::base(baseline.clone());
            assert!(Arc::ptr_eq(&index.base, &ledger.base));
            assert!(index.added.is_empty());
            assert!(index.get(&(value_identity.clone(), 924)).is_some());
            assert!(index.get(&(value_identity.clone(), 925)).is_none());
            let mut foreign_value = value_identity.clone();
            foreign_value.unit.push_str("-foreign");
            assert!(index.get(&(foreign_value, 924)).is_none());
            let reused = session
                .capture_checked_native_delta(ScopeId::ROOT, 924, [("answer", 924)].into_iter())
                .unwrap();
            assert_eq!(reused.imports.len(), 1);
            let (unchanged, added) = ledger.append(reused, &index).unwrap();
            assert!(added.is_empty());
            assert!(unchanged.tail.as_ref().unwrap().imports.is_empty());
            assert_eq!(unchanged.tail.as_ref().unwrap().bindings.len(), 1);
            for ((identity, generation), root) in &baseline.owners {
                assert!(ledger
                    .append(
                        CapturedNativeDelta {
                            imports: vec![SettledNativeImport {
                                identity: identity.clone(),
                                generation: *generation,
                                root_id: root + 1,
                            }],
                            bindings: Vec::new(),
                        },
                        &index
                    )
                    .is_err());
            }
            let own_identity = bind_native_delta_fixture(session, "later", 925);
            let delta = session
                .capture_checked_native_delta(ScopeId::ROOT, 925, [("later", 925)].into_iter())
                .unwrap();
            assert_eq!(delta.imports.len(), 1);
            let root = delta.imports[0].root_id;
            let (settled, added) = ledger.append(delta, &index).unwrap();
            index.extend(added);
            assert_eq!(index.added.len(), 1);
            assert_eq!(index.get(&(own_identity.clone(), 925)), Some(&root));
            assert!(baseline.owners.get(&(own_identity.clone(), 925)).is_none());
            assert!(settled
                .append(
                    CapturedNativeDelta {
                        imports: vec![SettledNativeImport {
                            identity: own_identity,
                            generation: 925,
                            root_id: root + 1,
                        }],
                        bindings: Vec::new(),
                    },
                    &index
                )
                .is_err());
            drop(baseline);
            drop(ledger);
            drop(unchanged);
            drop(settled);
            assert!(retained.upgrade().is_some());
            drop(index);
            assert!(retained.upgrade().is_none());
        });
    }

    #[test]
    fn protected_injection_captures_private_setup_without_sibling_inventory() {
        let root = tempfile::tempdir().unwrap();
        let lib =
            SessionLib::open(SessionId(994), root.path(), ModuleEnv::standalone_default()).unwrap();
        let mut session = PersistentSession::new(Some(lib), 1024 * 1024);
        let public = session.mint_scope(ScopeId::ROOT).unwrap();
        let execution = Arc::new(session.begin_private_execution(public).unwrap());
        let sibling = session.mint_scope(ScopeId::ROOT).unwrap();
        let foreign = crate::session::prepared::tests::rooted_publication_fixture(
            &mut session,
            "foreign",
            918,
        );
        let foreign_module = foreign.module;
        session.bind_in(sibling, foreign).unwrap();
        let setup =
            crate::session::prepared::tests::rooted_publication_fixture(&mut session, "input", 919);
        let setup_module = setup.module;
        session.bind_in(execution.private_scope(), setup).unwrap();
        assert!(execution.view().unwrap().injected_values().is_empty());
        let ordinary = session.compile_view_in(execution.private_scope()).unwrap();
        assert!(ordinary.injected_values().contains(&foreign_module));
        let protected = session.compile_view_for_execution(&execution).unwrap();
        assert_eq!(protected.injected_values(), protected.reachable_values());
        assert!(protected.injected_values().contains(&setup_module));
        assert!(!protected.injected_values().contains(&foreign_module));
        for module in protected.reachable_values() {
            session.retain_fixture_value_interface(*module, Arc::from([1, 2, 3]));
        }
        assert!(matches!(session.admit_cell_for_execution(
            execution.clone(), 0, Arc::new(()), [7; 32], [8; 32], Vec::new(),
        ), Err(SessionError::MissingRetainedValueInterface(owner)) if owner == setup_module));
        let foreign_session = PersistentSession::new(None, 1024 * 1024);
        assert!(matches!(
            foreign_session.compile_view_for_execution(&execution),
            Err(SessionError::StaleStagedDeclaration)
        ));
    }

    #[test]
    fn native_admission_commits_only_exact_scoped_value_owners() {
        let root = tempfile::tempdir().unwrap();
        let lib =
            SessionLib::open(SessionId(991), root.path(), ModuleEnv::standalone_default()).unwrap();
        let mut session = PersistentSession::new(Some(lib), 1024 * 1024);
        let source = session.mint_notebook_scope_with_lease(None).unwrap();
        native_source_action(&mut session, &source, |session| {
            let unbound = crate::session::prepared::tests::rooted_publication_fixture(
                session,
                "nativeValue",
                914,
            );
            let key = (unbound.value.identity.clone(), unbound.module.gen.0);
            let handle = unbound.value.handle.raw().0;
            let scope = session.mint_scope(ScopeId::ROOT).unwrap();
            let sibling = session.mint_scope(ScopeId::ROOT).unwrap();
            let before = session.capture_admitted_native_imports(scope).unwrap();
            assert!(!before.entries.contains(&key));
            session.bind_in(scope, unbound).unwrap();
            let admitted = session.capture_admitted_native_imports(scope).unwrap();
            assert!(admitted.entries.contains(&key));
            assert_eq!(admitted.owners.get(&key), Some(&handle));
            assert_ne!(handle, 0);
            let unrelated = session.capture_admitted_native_imports(sibling).unwrap();
            assert!(!unrelated.entries.contains(&key));
            assert!(!unrelated.owners.contains_key(&key));
            assert_ne!(admitted.commitment, unrelated.commitment);
            assert_ne!(admitted.commitment, before.commitment);
        });
    }

    #[test]
    fn cell_admission_reserves_the_lazy_machine_identity_once() {
        let root = tempfile::tempdir().unwrap();
        let lib =
            SessionLib::open(SessionId(986), root.path(), ModuleEnv::standalone_default()).unwrap();
        let mut session = PersistentSession::new(Some(lib), 1024 * 1024);
        let public = session.mint_scope(ScopeId::ROOT).unwrap();
        let first = Arc::new(session.begin_private_execution(public).unwrap());
        let second = Arc::new(session.begin_private_execution(public).unwrap());
        assert_eq!(first.admitted_public().unwrap().machine_incarnation, None);
        let first_cell = session
            .admit_cell_for_execution(first.clone(), 0, Arc::new(()), [7; 32], [8; 32], Vec::new())
            .unwrap();
        let second_cell = session
            .admit_cell_for_execution(
                second.clone(),
                0,
                Arc::new(()),
                [7; 32],
                [8; 32],
                Vec::new(),
            )
            .unwrap();
        let incarnation = first_cell.visibility().unwrap().machine_incarnation;
        assert!(incarnation.is_some());
        assert_eq!(
            second_cell.visibility().unwrap().machine_incarnation,
            incarnation
        );
        assert!(session.prepared_mut().is_none());
        let _value = crate::session::prepared::tests::rooted_publication_fixture(
            &mut session,
            "bootstrapRoot",
            704,
        );
        assert_eq!(
            session
                .public_visibility_snapshot_in(first.private_scope())
                .unwrap(),
            *first_cell.visibility().unwrap()
        );
        assert_eq!(
            session
                .public_visibility_snapshot_in(second.private_scope())
                .unwrap(),
            *second_cell.visibility().unwrap()
        );
        drop(first);
        let scope = first_cell.private_execution().unwrap().private_scope();
        session.reap_admission_leases();
        assert!(session.scope_tree().is_live(scope));
        drop(first_cell);
        session.reap_admission_leases();
        assert!(!session.scope_tree().is_live(scope));
    }

    #[test]
    fn durable_owner_transfer_fences_offers_and_staged_tickets_without_retiring_scopes() {
        use crate::session::{
            ExecutionPublication, PublicManifestCommit, PublicationDecision, PublicationPhase,
            RecoveryPublicOwner,
        };
        let root = tempfile::tempdir().unwrap();
        let recovery = crate::session::recovery_test_fixture::RecoveryFixture::create(root.path());
        let mut lib = SessionLib::open(
            SessionId(987),
            root.path().join("compiler"),
            ModuleEnv::standalone_default(),
        )
        .unwrap();
        tidepool_testing::with_settlement(|settlement| recovery.attach_root(&mut lib, settlement))
            .unwrap();
        let mut session = PersistentSession::new(Some(lib), 1024 * 1024);
        let public = session.mint_scope(ScopeId::ROOT).unwrap();
        let owner = RecoveryPublicOwner::new(
            &tidepool_repr::ActorPath::parse("root/owner-transfer").unwrap(),
            1,
        )
        .unwrap();
        assert!(matches!(
            session
                .initialize_durable_public_scope(owner.clone(), public)
                .unwrap(),
            PublicManifestCommit::Durable
        ));
        let private = Arc::new(session.begin_private_execution(public).unwrap());
        let cell = session
            .admit_cell_for_execution(
                private.clone(),
                0,
                Arc::new(()),
                [7; 32],
                [8; 32],
                Vec::new(),
            )
            .unwrap();
        let intent = session
            .freeze_execution_intent(&private, vec![], vec![])
            .unwrap();
        let ExecutionPublication::Bindings(base) = session
            .restage_execution_publication(owner.clone(), intent.clone())
            .unwrap()
        else {
            unreachable!()
        };
        let ticket = base.stage().unwrap();
        let next = session.prepare_execution_admission_epoch_advance().unwrap();
        session.invalidate_execution_admissions_after_owner_transfer(next);
        assert!(!cell.belongs_to(&session));
        assert!(matches!(
            session.admit_cell_for_execution(
                private.clone(),
                0,
                Arc::new(()),
                [7; 32],
                [8; 32],
                Vec::new()
            ),
            Err(SessionError::StaleStagedDeclaration)
        ));
        assert!(matches!(
            session.freeze_execution_intent(&private, vec![], vec![]),
            Err(SessionError::StaleStagedDeclaration)
        ));
        assert!(matches!(
            session.restage_execution_publication(owner.clone(), intent),
            Err(SessionError::StaleStagedDeclaration)
        ));
        let decision = PublicationDecision::new();
        assert!(matches!(
            session.attempt_public_manifest(ticket, &decision).unwrap(),
            crate::session::PublicationAttempt::Settled(PublicManifestCommit::Refused { .. })
        ));
        assert_eq!(decision.phase(), PublicationPhase::Running);
        assert!(session.scope_tree().is_live(private.private_scope()));
        let fresh = Arc::new(session.begin_private_execution(public).unwrap());
        let current = session
            .admit_cell_for_execution(fresh, 0, Arc::new(()), [7; 32], [8; 32], Vec::new())
            .unwrap();
        assert!(current.belongs_to(&session));
        assert_ne!(cell.digest(), current.digest());
        let old_scope = private.private_scope();
        drop(cell);
        drop(private);
        session.reap_admission_leases();
        assert!(!session.scope_tree().is_live(old_scope));
    }

    #[test]
    fn interface_snapshots_share_bytes_and_hash_only_the_new_delta() {
        const BYTES: usize = 64 * 1024;
        for baseline_count in [0, 100] {
            for count in [1, 10, 100] {
                let baseline = (0..baseline_count)
                    .map(|index| ValueInterfaceSnapshot {
                        module: tidepool_repr::SessionModule::val(Generation(index + 1)),
                        bytes: Arc::from(vec![index as u8; BYTES]),
                    })
                    .collect::<Vec<_>>();
                let (mut snapshot, mut index) = CheckedInterfaces::base(&baseline);
                let original = snapshot.clone();
                let mut deltas = Vec::new();
                for offset in 0..count {
                    let bytes: Arc<[u8]> = Arc::from(vec![offset as u8; BYTES]);
                    let module =
                        tidepool_repr::SessionModule::val(Generation(baseline_count + offset + 1));
                    let digest = snapshot.interface_digest(&bytes);
                    assert!(index.insert(module.gen.0, digest).is_none());
                    snapshot = snapshot.append(
                        ValueInterfaceSnapshot {
                            module,
                            bytes: bytes.clone(),
                        },
                        digest,
                    );
                    deltas.push(bytes);
                }
                assert_eq!(original.iter().count(), baseline_count as usize);
                let actual = snapshot.iter().collect::<Vec<_>>();
                assert_eq!(actual.len(), (baseline_count + count) as usize);
                for (expected, current) in baseline.iter().zip(&actual) {
                    assert!(Arc::ptr_eq(&expected.bytes, &current.bytes));
                }
                for (expected, current) in deltas
                    .iter()
                    .zip(actual.iter().skip(baseline_count as usize))
                {
                    assert!(Arc::ptr_eq(expected, &current.bytes));
                }
                let hashed = snapshot
                    .bytes_hashed
                    .load(std::sync::atomic::Ordering::Relaxed);
                assert_eq!(hashed, (baseline_count + count) as usize * BYTES);
                assert_ne!(original.commitment, snapshot.commitment);
                eprintln!("prefix interfaces baseline={baseline_count} items={count} bytes_copied=0 bytes_hashed={hashed} delta_nodes={count}");
            }
        }
    }

    #[test]
    fn latest_snapshot_lease_keeps_shadowed_native_roots_without_accumulating_scopes() {
        for count in [1, 10, 100] {
            let root = tempfile::tempdir().unwrap();
            let lib =
                SessionLib::open(SessionId(989), root.path(), ModuleEnv::standalone_default())
                    .unwrap();
            let mut session = PersistentSession::new(Some(lib), 1024 * 1024);
            let source = session.mint_scope(ScopeId::ROOT).unwrap();
            let mut latest = None;
            let mut older_reader = None;
            let mut ids = Vec::new();
            for offset in 0..count {
                let value = crate::session::prepared::tests::rooted_publication_fixture(
                    &mut session,
                    "shadowed",
                    800 + offset,
                );
                ids.push(value.id);
                session.bind_in(source, value).unwrap();
                latest = Some(session.retain_lexical_scope(source).unwrap());
                if offset == 0 && count > 1 {
                    older_reader = latest.clone();
                }
                session.reap_admission_leases();
                assert!(session.scope_tree().len() <= 4);
                assert!(session.bindings().lease_count(ids[0]) <= 2);
            }
            // An outstanding old compilation/cancellation owner retains only
            // its exact snapshot. Dropping it releases that share normally.
            drop(older_reader);
            session.reap_admission_leases();
            assert_eq!(session.scope_tree().len(), 3);
            assert_eq!(session.bindings().lease_count(ids[0]), 1);
            session.retire_scope(source);
            for id in &ids {
                assert!(session.bindings().get(*id).is_some());
            }
            let latest = latest.unwrap();
            let child = session.mint_scope_from_lease(&latest).unwrap();
            drop(latest);
            session.reap_admission_leases();
            for id in &ids {
                assert!(session.bindings().get(*id).is_some());
            }
            session.retire_scope(child);
            assert!(session.bindings().is_empty());
            assert_eq!(session.scope_tree().len(), 1);
            eprintln!("prefix lexical owners items={count} steady_scopes=3 max_scopes=4 final_scopes=1 final_live_bindings=0");
        }
    }

    #[test]
    fn lexical_scope_lease_survives_owner_retirement_and_releases_after_child() {
        let root = tempfile::tempdir().unwrap();
        let lib =
            SessionLib::open(SessionId(988), root.path(), ModuleEnv::standalone_default()).unwrap();
        let mut session = PersistentSession::new(Some(lib), 1024 * 1024);
        let source = session.mint_scope(ScopeId::ROOT).unwrap();
        let value = crate::session::prepared::tests::rooted_publication_fixture(
            &mut session,
            "capturedRoot",
            702,
        );
        let id = value.id;
        session.bind_in(source, value).unwrap();
        let lease = session.retain_lexical_scope(source).unwrap();
        let admitted = lease.clone();
        session.retire_scope(source);
        drop(lease);
        session.reap_admission_leases();
        assert!(session.scope_tree().is_live(admitted.scope()));
        let child = session.mint_scope_from_lease(&admitted).unwrap();
        assert_eq!(session.resolve_in(child, "capturedRoot").unwrap().id, id);
        let retained = admitted.scope();
        drop(admitted);
        session.reap_admission_leases();
        assert!(!session.scope_tree().is_live(retained));
        assert_eq!(session.resolve_in(child, "capturedRoot").unwrap().id, id);
        session.retire_scope(child);
        assert!(session.bindings().get(id).is_none());
    }

    #[test]
    fn lexical_placement_validation_fences_foreign_scope_epoch_and_retirement() {
        let root = tempfile::tempdir().unwrap();
        let make_session = || {
            PersistentSession::new(
                Some(
                    SessionLib::open(SessionId(997), root.path(), ModuleEnv::standalone_default())
                        .unwrap(),
                ),
                1024 * 1024,
            )
        };
        let mut first = make_session();
        let mut second = make_session();
        let source = first.mint_scope(ScopeId::ROOT).unwrap();
        second.mint_scope(ScopeId::ROOT).unwrap();
        let lease = first.retain_lexical_scope(source).unwrap();
        let foreign = second.retain_lexical_scope(source).unwrap();
        assert_eq!(lease.scope(), foreign.scope());
        first
            .validate_lexical_scope_lease(lease.scope(), &lease)
            .unwrap();
        assert!(matches!(
            second.validate_lexical_scope_lease(lease.scope(), &lease),
            Err(SessionError::StaleStagedDeclaration)
        ));
        assert!(matches!(
            first.validate_lexical_scope_lease(source, &lease),
            Err(SessionError::StaleStagedDeclaration)
        ));
        let next = first.prepare_execution_admission_epoch_advance().unwrap();
        first.invalidate_execution_admissions_after_owner_transfer(next);
        assert!(matches!(
            first.validate_lexical_scope_lease(lease.scope(), &lease),
            Err(SessionError::StaleStagedDeclaration)
        ));
        let scopes_before = first.scope_tree().len();
        assert!(matches!(
            first.mint_scope_from_lease(&lease),
            Err(SessionError::StaleStagedDeclaration)
        ));
        assert_eq!(first.scope_tree().len(), scopes_before);
        // Custody survives invalidation; only the unconsumed placement grant
        // is stale. A newly captured exact lease can authorize this owner.
        assert!(first.scope_tree().is_live(lease.scope()));
        let fresh = first.retain_lexical_scope(lease.scope()).unwrap();
        first
            .validate_lexical_scope_lease(fresh.scope(), &fresh)
            .unwrap();
        first.retire_scope(fresh.scope());
        assert!(matches!(
            first.validate_lexical_scope_lease(fresh.scope(), &fresh),
            Err(SessionError::DeadScope(_))
        ));
    }

    #[test]
    fn admission_owner_fences_same_library_and_scope_counters() {
        let root = tempfile::tempdir().unwrap();
        let make_session = || {
            PersistentSession::new(
                Some(
                    SessionLib::open(SessionId(983), root.path(), ModuleEnv::standalone_default())
                        .unwrap(),
                ),
                1024 * 1024,
            )
        };
        let mut first = make_session();
        let mut second = make_session();
        let first_public = first.mint_scope(ScopeId::ROOT).unwrap();
        let second_public = second.mint_scope(ScopeId::ROOT).unwrap();
        let private = first.begin_private_execution(first_public).unwrap();
        let second_private = second.begin_private_execution(second_public).unwrap();
        assert_eq!(private.private_scope(), second_private.private_scope());
        assert_eq!(private.binding_tip(), second_private.binding_tip());
        assert_eq!(private.session(), second_private.session());
        assert_eq!(private.admitted_public().unwrap().machine_incarnation, None);
        assert_eq!(
            second_private
                .admitted_public()
                .unwrap()
                .machine_incarnation,
            None
        );
        assert!(matches!(
            second.freeze_execution_intent(&private, vec![], vec![]),
            Err(SessionError::StaleStagedDeclaration)
        ));
        let first_cell = first
            .admit_cell_in(
                private.private_scope(),
                0,
                Arc::new(()),
                [7; 32],
                [8; 32],
                Vec::new(),
            )
            .unwrap();
        let second_cell = second
            .admit_cell_in(
                second_private.private_scope(),
                0,
                Arc::new(()),
                [7; 32],
                [8; 32],
                Vec::new(),
            )
            .unwrap();
        assert_ne!(first_cell.digest(), second_cell.digest());
        assert!(first_cell.belongs_to(&first));
        assert!(!first_cell.belongs_to(&second));
    }
}
