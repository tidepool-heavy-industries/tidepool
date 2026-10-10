//! Runtime-owned immutable compilation and private execution admission.

use std::any::Any;
use std::path::PathBuf;
use std::sync::{Arc, OnceLock};

use tidepool_codegen::binding_table::{BindingTipId, SourceLeaseKey};
use tidepool_codegen::scope::ScopeId;
use tidepool_repr::execution_schema::{ImportOwner, SymbolIdentity};
use tidepool_repr::Generation;

use super::{PersistentSession, PublicVisibilitySnapshot, SessionCompileView, SessionError};

/// Read-only native readiness for one originally admitted durable public owner.
/// The graph owns the confirmation state; losing that graph revokes readiness.
pub struct RuntimeDurablePublicReadiness {
    owner: Arc<RuntimeAdmissionOwner>,
    owner_epoch: u64,
    session: super::SessionId,
    durable: super::RecoveryPublicOwner,
    scope: ScopeId,
    confirmation:
        std::sync::Weak<parking_lot::Mutex<Option<tidepool_atomic_write::PublishedWrite>>>,
}

impl RuntimeDurablePublicReadiness {
    /// Confirmation can temporarily block an otherwise current owner. Currency
    /// alone permits retaining its original placement, never executing work.
    pub fn is_current(&self) -> bool {
        self.owner.epoch() == self.owner_epoch && self.confirmation.upgrade().is_some()
    }

    pub fn is_ready(&self) -> bool {
        self.owner.epoch() == self.owner_epoch
            && self
                .confirmation
                .upgrade()
                .is_some_and(|state| state.lock().is_none())
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
    pub(super) owner: Arc<RuntimeAdmissionOwner>,
    pub(super) owner_epoch: u64,
    pub(super) durable_owner: Option<super::RecoveryPublicOwner>,
    pub(super) scope_lease: Arc<RuntimeLexicalScopeLease>,
    admitted: PublicVisibilitySnapshot,
    private_scope: ScopeId,
    view: SessionCompileView,
    binding_tip: BindingTipId,
    pub(super) completed_values: parking_lot::Mutex<
        std::collections::HashMap<tidepool_repr::SessionVarId, CertifiedPrivateValueWrite>,
    >,
    pub(super) final_intent: OnceLock<Arc<super::FinalExecutionIntent>>,
}

pub(super) struct CertifiedPrivateValueWrite {
    name: String,
    identity: SymbolIdentity,
    generation: Generation,
    root_id: u64,
    pub(super) proof: CertifiedPrivateValueProof,
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

/// One fresh thin-interface slot shared by original inputs and host builders.
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
    pub(super) fn admit_binding_interface(
        &mut self,
        scope: ScopeId,
        binding: String,
        prototype: Arc<tidepool_toolchain::checked_cell::ExactHostBindingPrototype>,
        authority_digest: [u8; 32],
    ) -> Result<Arc<RuntimeBindingInterfaceReservation>, SessionError> {
        if !self.scope_tree().is_live(scope) {
            return Err(SessionError::DeadScope(scope));
        }
        let generation = self.val_gen().next();
        self.set_val_gen(generation);
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
        }))
    }

    pub(super) fn admit_host_binding_interface(
        &mut self,
        scope: ScopeId,
        binding: String,
        prototype: Arc<super::resident::HostBindingPrototype>,
    ) -> Result<Arc<RuntimeHostBindingAdmission>, SessionError> {
        let reservation = self.admit_binding_interface(
            scope,
            binding,
            prototype.compiler().clone(),
            prototype.authority_digest(),
        )?;
        Ok(Arc::new(RuntimeHostBindingAdmission {
            reservation,
            prototype,
        }))
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
        Ok(())
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
        self.consume_binding_interface(&admission.reservation, interface)
    }
}
impl CertifiedPrivateValueWrite {
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
    owner: Arc<RuntimeAdmissionOwner>,
    owner_epoch: u64,
    scope: ScopeId,
    id: tidepool_repr::SessionVarId,
    write: CertifiedPrivateValueWrite,
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
            },
        })
    }

    /// Transfer an accepted effect's exact write to its existing private owner.
    /// Final publication revalidates the native identity under machine checkout.
    pub fn accept(self, admission: &PrivateExecutionAdmission) -> Result<(), SessionError> {
        if !Arc::ptr_eq(&self.owner, &admission.owner)
            || self.owner_epoch != admission.owner_epoch
            || self.owner_epoch != self.owner.epoch()
            || self.scope != admission.private_scope
        {
            return Err(SessionError::StaleStagedDeclaration);
        }
        let mut completed = admission.completed_values.lock();
        if admission.final_intent.get().is_some() {
            return Err(SessionError::StaleStagedDeclaration);
        }
        if let Some(prior) = completed.get(&self.id) {
            if prior.name != self.write.name
                || prior.identity != self.write.identity
                || prior.generation != self.write.generation
                || prior.root_id != self.write.root_id
                || !prior.proof.same_original(&self.write.proof)
            {
                return Err(SessionError::StaleStagedDeclaration);
            }
        } else {
            completed.insert(self.id, self.write);
        }
        Ok(())
    }
}

impl PrivateExecutionAdmission {
    pub fn admitted_public(&self) -> &PublicVisibilitySnapshot {
        &self.admitted
    }
    pub fn private_scope(&self) -> ScopeId {
        self.private_scope
    }
    pub fn view(&self) -> &SessionCompileView {
        &self.view
    }
    pub fn binding_tip(&self) -> BindingTipId {
        self.binding_tip
    }
    pub fn durable_owner(&self) -> Option<&super::RecoveryPublicOwner> {
        self.durable_owner.as_ref()
    }
}

/// Protected inputs for one whole-cell compiler offer. The opaque retained
/// specification is issued by the actor owner and holds its source/tool
/// leases; compiler evidence binds its digest without interpreting authority.
/// Original declaration identities are burned before checking any source.
pub struct RuntimeCellAdmission {
    catalog_selection: tidepool_toolchain::toolchain::CatalogSelection,
    owner: Arc<RuntimeAdmissionOwner>,
    owner_epoch: u64,
    purpose: RuntimeCellPurpose,
    _retained_scope: Arc<RuntimeLexicalScopeLease>,
    prefix_started: std::sync::atomic::AtomicBool,
    declaration_baseline: Option<super::lexical_projection::DeclarationProjectionBaseline>,
    prepared_declarations: std::sync::OnceLock<
        Vec<(
            tidepool_toolchain::checked_cell::ExactCheckedItem,
            Arc<super::lexical_projection::PreparedAuthoredDeclaration>,
        )>,
    >,
    compile_inputs: super::prepared::RuntimeCompileInputs,
    view: SessionCompileView,
    view_digest: [u8; 32],
    visibility: PublicVisibilitySnapshot,
    reserved_generations: Vec<Generation>,
    planned: Option<Arc<super::RuntimeCellPlanReservation>>,
    initial_value_generation: Generation,
    native_shares: Vec<SourceLeaseKey>,
    native_imports: Arc<AdmittedNativeImports>,
    interfaces: Vec<AdmittedValueInterface>,
    specification: Arc<dyn Any + Send + Sync>,
    specification_digest: [u8; 32],
    authority_digest: [u8; 32],
    include_paths: Vec<PathBuf>,
    digest: [u8; 32],
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
    },
}

impl RuntimeCellPurpose {
    fn frame_authorization(&self, frame: &mut impl FnMut(&[u8])) {
        match self {
            #[cfg(test)]
            Self::Observation => {}
            Self::PrivateExecution(_) => frame(b"TidepoolPrivateExecutionAdmission1"),
            Self::NativeSetup => frame(b"TidepoolNativeSetupAdmission1"),
            Self::HostCarrier { binding, expected } => {
                frame(b"TidepoolHostCarrierAdmission1");
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
    retired: parking_lot::Mutex<Vec<ScopeId>>,
}

impl RuntimeAdmissionOwner {
    pub(super) fn epoch(&self) -> u64 {
        self.epoch.load(std::sync::atomic::Ordering::Acquire)
    }
    pub(super) fn new() -> Self {
        Self {
            identity: uuid::Uuid::new_v4(),
            epoch: std::sync::atomic::AtomicU64::new(0),
            retired: parking_lot::Mutex::new(Vec::new()),
        }
    }
}

/// Shared exact lexical lifetime, issued by its existing runtime owner.
/// The detached scope retains the binding and native shares captured at mint.
pub struct RuntimeLexicalScopeLease {
    owner: Arc<RuntimeAdmissionOwner>,
    owner_epoch: u64,
    scope: ScopeId,
}
impl RuntimeLexicalScopeLease {
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
impl Drop for RuntimeLexicalScopeLease {
    fn drop(&mut self) {
        self.owner.retired.lock().push(self.scope);
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
    admission: Arc<RuntimeCellAdmission>,
    first_item: tidepool_toolchain::checked_cell::ExactCheckedItem,
    program: Arc<tidepool_toolchain::checked_cell::CellProgram>,
    state: parking_lot::Mutex<RuntimeCheckedState>,
}

#[derive(Debug)]
struct RuntimeCheckedState {
    snapshot: Arc<RuntimeCheckedPrefixSnapshot>,
    in_flight: Option<Arc<tidepool_toolchain::checked_cell::ExactCompiledItem>>,
    reservation: Option<CheckedItemReservation>,
    interface_index: std::collections::BTreeMap<u64, [u8; 32]>,
    native_index: CheckedNativeIndex,
    compile_inputs: super::prepared::RuntimeCompileInputs,
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
    view: SessionCompileView,
    view_digest: [u8; 32],
    visibility: PublicVisibilitySnapshot,
    interfaces: CheckedInterfaces,
    _retained_scope: Arc<RuntimeLexicalScopeLease>,
    native_shares: Vec<SourceLeaseKey>,
    native_imports: CheckedNativeImports,
    compiler_prefix: tidepool_toolchain::checked_cell::ExactCompiledPrefix,
    digest: [u8; 32],
}

impl RuntimeCheckedPrefixSnapshot {
    pub fn view(&self) -> &SessionCompileView {
        &self.view
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
        let mut state = self.state.lock();
        if self.admission.host_carrier().is_some()
            || !self.admission.belongs_to(session)
            || self.admission.visibility.scope != scope
            || state.in_flight.is_some()
            || state.reservation.as_ref().is_none_or(|reservation| {
                reservation.item != *execution.item()
                    || reservation.generation.0 != execution.generation()
            })
            || {
                self.program
                    .items()
                    .get(execution.item().index())
                    .and_then(|item| item.native())
                    .is_none_or(|native| !Arc::ptr_eq(native, &execution))
            }
            || execution.item().admission_digest() != self.admission.digest()
            || session.public_visibility_snapshot_in(scope).as_ref()
                != Some(&state.snapshot.visibility)
            || session.compile_view_digest_in(scope) != Some(state.snapshot.view_digest)
        {
            return Err(SessionError::StaleStagedDeclaration);
        }
        let reservation = state
            .reservation
            .as_ref()
            .expect("checked reservation preflighted");
        execution.validate_runtime_admission(reservation.digest, self.admission.digest())?;
        execution.validate_required_native_imports(state.snapshot.actual_retained_imports())?;
        execution.validate_settled_native_bindings(state.snapshot.settled_native_bindings())?;
        // Appending checks the compiler-owned same-cell identity and order.
        validate_private_value_overlay(
            self,
            session,
            &state.snapshot,
            execution.private_value_overlay_binders(),
        )?;
        state.snapshot.compiler_prefix.append(execution.clone())?;
        state.in_flight = Some(execution.clone());
        Ok(Arc::new(CheckedTurnCompletion {
            prefix: self.clone(),
            execution,
            scope,
        }))
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

    pub(crate) fn settle(
        &self,
        session: &mut PersistentSession,
        program: tidepool_codegen::prepared_program::ProgramId,
    ) -> Result<(), SessionError> {
        let mut state = self.prefix.state.lock();
        if !self.prefix.admission.belongs_to(session)
            || state
                .in_flight
                .as_ref()
                .is_none_or(|execution| !Arc::ptr_eq(execution, &self.execution))
        {
            return Err(SessionError::StaleStagedDeclaration);
        }
        let compiler_prefix = state
            .snapshot
            .compiler_prefix
            .append(self.execution.clone())?;
        let native = session.capture_checked_native_delta(
            self.scope,
            self.execution.generation(),
            Some(program),
            &state.native_index,
            self.execution.target_definition_identities(),
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
                    },
                ));
            }
            Some((private, values))
        } else {
            None
        };
        settle_checked_snapshot(
            session,
            &mut state,
            self.scope,
            compiler_prefix,
            self.prefix.admission.digest(),
            self.execution.value_interface_certificate(),
            Some(native),
        )?;
        if let Some((private, values)) = completed_values {
            private.completed_values.lock().extend(values);
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
    admission: [u8; 32],
    interface: Option<Arc<tidepool_toolchain::checked_cell::CheckedValueArtifact>>,
    native_delta: Option<CapturedNativeDelta>,
) -> Result<(), SessionError> {
    let view = session
        .compile_view_in(scope)
        .ok_or(SessionError::DeadScope(scope))?
        .with_scoped_injection()
        .with_compile_inputs(&state.compile_inputs)
        .map_err(SessionError::Compile)?;
    let view_digest = session
        .compile_view_digest_in(scope)
        .ok_or(SessionError::DeadScope(scope))?;
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
    let snapshot = Arc::new(checked_snapshot(
        view,
        view_digest,
        visibility,
        interfaces,
        compiler_prefix,
        admission,
        retained,
        native_imports,
    ));
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
    let native_shares = visibility.source_instances.clone();
    let mut digest = blake3::Hasher::new();
    let mut frame = |bytes: &[u8]| {
        digest.update(&(bytes.len() as u64).to_le_bytes());
        digest.update(bytes);
    };
    frame(b"TidepoolRuntimeCheckedPrefix1");
    frame(&admission);
    frame(&(compiler_prefix.next_item() as u64).to_le_bytes());
    frame(&view_digest);
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
        frame(&key.binder.version.0);
        frame(key.binder.binder.unit.as_bytes());
        frame(key.binder.binder.module.as_bytes());
        frame(key.binder.binder.namespace.as_bytes());
        frame(key.binder.binder.occurrence.as_bytes());
        match &key.binder.binder.record_parent {
            Some(parent) => {
                frame(&[1]);
                frame(parent.as_bytes());
            }
            None => frame(&[0]),
        }
    }
    RuntimeCheckedPrefixSnapshot {
        view,
        view_digest,
        visibility,
        interfaces,
        _retained_scope: retained_scope,
        native_shares,
        native_imports,
        compiler_prefix,
        digest: *digest.finalize().as_bytes(),
    }
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
        f.debug_struct("RuntimeCellAdmission")
            .field("view", &self.view)
            .field("visibility", &self.visibility)
            .field("reserved_generations", &self.reserved_generations)
            .field("native_shares", &self.native_shares)
            .field("digest", &self.digest)
            .finish_non_exhaustive()
    }
}

impl RuntimeCellAdmission {
    pub fn catalog_selection(&self) -> &tidepool_toolchain::toolchain::CatalogSelection {
        &self.catalog_selection
    }

    /// Issue cumulative lexical projections while the machine and prefix
    /// mutex are stowed. Only this admission's immutable checked items enter
    /// the prepared sequence consumed by declaration adoption.
    pub(super) fn prepare_declaration_projections(
        &self,
        cell: &Arc<tidepool_toolchain::checked_cell::ExactCheckedCell>,
        settlement: &mut dyn FnMut(crate::CompilerTransactionClose),
    ) -> Result<(), SessionError> {
        let mut baseline = self.declaration_baseline.clone();
        let mut prepared = Vec::new();
        for index in 0..cell.item_count() {
            let item = cell.item(index)?;
            if item.admission_digest() != self.digest() {
                return Err(SessionError::StaleStagedDeclaration);
            }
            let Some(evidence) = item.planned_declaration() else {
                continue;
            };
            let generation = match &self.planned {
                Some(plan) => plan
                    .items()
                    .get(index)
                    .and_then(|row| row.declaration_generation()),
                None if index == 0 => self.reserved_generations.first().copied(),
                None => None,
            }
            .ok_or(SessionError::StaleStagedDeclaration)?;
            let projection = super::lexical_projection::prepare_authored_projection(
                baseline.as_ref(),
                generation,
                evidence.clone(),
                &[],
                &self.include_paths,
                self.view.session_root(),
                settlement,
            )?;
            baseline = Some(projection.next_baseline());
            prepared.push((item, projection));
        }
        self.prepared_declarations
            .set(prepared)
            .map_err(|_| SessionError::StaleStagedDeclaration)
    }

    pub(super) fn retained_declaration_projections(
        &self,
    ) -> Vec<Arc<tidepool_toolchain::declaration_join::AcceptedJoin>> {
        self.compile_inputs
            .projections()
            .iter()
            .map(|projection| projection.receipt().clone())
            .collect()
    }

    pub(super) fn prepared_declaration(
        &self,
        item: &tidepool_toolchain::checked_cell::ExactCheckedItem,
    ) -> Result<Arc<super::lexical_projection::PreparedAuthoredDeclaration>, SessionError> {
        self.prepared_declarations
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
            RuntimeCellPurpose::HostCarrier { binding, expected } => Some((binding, *expected)),
            _ => None,
        }
    }
    pub fn view(&self) -> &SessionCompileView {
        &self.view
    }
    pub fn visibility(&self) -> &PublicVisibilitySnapshot {
        &self.visibility
    }
    pub fn reserved_generations(&self) -> &[Generation] {
        &self.reserved_generations
    }
    pub fn plan_reservation(&self) -> Option<&Arc<super::RuntimeCellPlanReservation>> {
        self.planned.as_ref()
    }
    pub fn initial_value_generation(&self) -> Generation {
        self.initial_value_generation
    }
    pub fn native_shares(&self) -> &[SourceLeaseKey] {
        &self.native_shares
    }
    pub fn admitted_retained_imports(&self) -> &[(SymbolIdentity, u64)] {
        &self.native_imports.entries
    }
    pub fn interfaces(&self) -> &[AdmittedValueInterface] {
        &self.interfaces
    }
    pub fn digest(&self) -> [u8; 32] {
        self.digest
    }
    pub fn specification_digest(&self) -> [u8; 32] {
        self.specification_digest
    }
    pub fn authority_digest(&self) -> [u8; 32] {
        self.authority_digest
    }
    /// Ordered search inputs selected by the original specification owner.
    /// The source/tool capsule retains their lifetime; compiler evidence
    /// independently validates every consumed byte and resolution witness.
    pub fn include_paths(&self) -> &[PathBuf] {
        &self.include_paths
    }
    pub(super) fn belongs_to(&self, session: &PersistentSession) -> bool {
        Arc::ptr_eq(&self.owner, session.admission_owner())
            && self.owner_epoch == session.admission_owner().epoch()
    }
    pub fn specification(&self) -> &Arc<dyn Any + Send + Sync> {
        &self.specification
    }
}

impl PersistentSession {
    pub(super) fn capture_value_interfaces(
        &self,
        view: &super::SessionCompileView,
    ) -> Result<Vec<AdmittedValueInterface>, SessionError> {
        view.reachable_values()
            .iter()
            .map(|module| {
                let artifact = self
                    .retained_checked_value_artifact(*module)
                    .ok_or(SessionError::MissingRetainedValueInterface(*module))?;
                Ok(AdmittedValueInterface::from_checked(artifact.clone()))
            })
            .collect()
    }
    pub(super) fn consume_host_carrier_reservation(
        &self,
        admission: &Arc<RuntimeCheckedItemAdmission>,
        execution: &Arc<tidepool_toolchain::checked_cell::ExactCompiledItem>,
        binding: &str,
        expected: super::resident::HostBindingType,
    ) -> Result<(), SessionError> {
        let prefix = admission.prefix();
        let mut state = prefix.state.lock();
        let scope = prefix.admission.visibility.scope;
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
            || self.public_visibility_snapshot_in(scope).as_ref()
                != Some(&state.snapshot.visibility)
            || self.compile_view_digest_in(scope) != Some(state.snapshot.view_digest)
        {
            return Err(SessionError::StaleStagedDeclaration);
        }
        execution.validate_runtime_admission(admission.digest(), prefix.admission.digest())?;
        execution.validate_required_native_imports(state.snapshot.actual_retained_imports())?;
        execution.validate_settled_native_bindings(state.snapshot.settled_native_bindings())?;
        // Mounting a host payload never executes or completes the placeholder.
        state.reservation = None;
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
    pub(crate) fn invalidate_execution_admissions_after_owner_transfer(&mut self, next: u64) {
        self.admission_owner()
            .epoch
            .store(next, std::sync::atomic::Ordering::Release);
    }

    pub fn admit_checked_item(
        &mut self,
        prefix: Arc<RuntimeCheckedPrefix>,
        item: tidepool_toolchain::checked_cell::ExactCheckedItem,
    ) -> Result<Arc<RuntimeCheckedItemAdmission>, SessionError> {
        let mut state = prefix.state.lock();
        let scope = prefix.admission.visibility.scope;
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
            || self.public_visibility_snapshot_in(scope).as_ref()
                != Some(&state.snapshot.visibility)
            || self.compile_view_digest_in(scope) != Some(state.snapshot.view_digest)
        {
            return Err(SessionError::StaleStagedDeclaration);
        }
        let snapshot = state.snapshot.clone();
        let planned_row = prefix
            .admission
            .plan_reservation()
            .ok_or(SessionError::StaleStagedDeclaration)?
            .items()
            .get(item.index())
            .ok_or(SessionError::StaleStagedDeclaration)?;
        use super::RuntimePlannedCellItemKind as Kind;
        use tidepool_toolchain::checked_cell::CheckedItemKind as CheckedKind;
        if !matches!(
            (planned_row.kind(), item.kind()),
            (Kind::Prologue | Kind::Declaration, CheckedKind::Declaration)
                | (Kind::Bind, CheckedKind::Bind)
                | (Kind::Expression, CheckedKind::Expression)
        ) {
            return Err(SessionError::StaleStagedDeclaration);
        }
        let generation = planned_row
            .value_generation()
            .or_else(|| planned_row.declaration_generation())
            .ok_or(SessionError::StaleStagedDeclaration)?;
        let observation_name = planned_row.observation_name().map(str::to_owned);
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
        state.reservation = Some(CheckedItemReservation {
            item: item.clone(),
            generation,
            digest,
        });
        drop(state);
        Ok(Arc::new(RuntimeCheckedItemAdmission {
            prefix,
            snapshot,
            item,
            generation,
            observation_name,
            digest,
        }))
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
        let mut state = prefix.state.lock();
        let scope = prefix.admission.visibility.scope;
        let item = &admission.item;
        if !prefix.admission.belongs_to(self)
            || !Arc::ptr_eq(&state.snapshot, &admission.snapshot)
            || state.in_flight.is_some()
            || state.reservation.as_ref().is_none_or(|reserved| {
                reserved.item != *item || reserved.generation != admission.generation
            })
            || self.public_visibility_snapshot_in(scope).as_ref()
                != Some(&state.snapshot.visibility)
            || self.compile_view_digest_in(scope) != Some(state.snapshot.view_digest)
        {
            return Err(SessionError::StaleStagedDeclaration);
        }
        let certificate = item
            .planned_declaration()
            .ok_or(SessionError::StaleStagedDeclaration)?;
        let original_source = item
            .planned_declaration_source()
            .ok_or(SessionError::StaleStagedDeclaration)?;
        let generation = prefix
            .admission
            .plan_reservation()
            .ok_or(SessionError::StaleStagedDeclaration)?
            .items()
            .get(item.index())
            .and_then(|row| row.declaration_generation())
            .ok_or(SessionError::StaleStagedDeclaration)?;
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
        let external = state.snapshot.view.persistent_imports().clone();
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
            .append_declaration_with_projection(
                item.clone(),
                prepared.projection.receipt().clone(),
            )?;
        let committed = self.adopt_staged_declaration_in(staged);
        if committed.is_ok()
            || committed
                .as_ref()
                .is_err_and(|error| error.published_declaration_commit().is_some())
        {
            settle_checked_snapshot(
                self,
                &mut state,
                scope,
                compiler_prefix,
                prefix.admission.digest(),
                None,
                None,
            )?;
            state.reservation = None;
        }
        committed
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
        Ok(Arc::new(RuntimeLexicalScopeLease {
            owner: self.admission_owner().clone(),
            owner_epoch: self.admission_owner().epoch(),
            scope,
        }))
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
        admission: Arc<RuntimeCellAdmission>,
        program: Arc<tidepool_toolchain::checked_cell::CellProgram>,
    ) -> Result<Option<Arc<RuntimeCheckedPrefix>>, SessionError> {
        if !admission.has_execution_purpose() {
            return Err(SessionError::StaleStagedDeclaration);
        }
        let planned = admission
            .plan_reservation()
            .ok_or(SessionError::StaleStagedDeclaration)?;
        program
            .validate_typed_entries()
            .map_err(SessionError::Compile)?;
        if program.admission_digest() != admission.digest()
            || !Arc::ptr_eq(program.parsed_plan(), planned.plan())
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
                || self
                    .public_visibility_snapshot_in(admission.visibility.scope)
                    .as_ref()
                    != Some(&admission.visibility)
                || self.compile_view_digest_in(admission.visibility.scope)
                    != Some(admission.view_digest)
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
        self.begin_cell_program_prefix(admission, first.checked_item().clone(), program)
            .map(Some)
    }

    fn begin_cell_program_prefix(
        &self,
        admission: Arc<RuntimeCellAdmission>,
        first_item: tidepool_toolchain::checked_cell::ExactCheckedItem,
        program: Arc<tidepool_toolchain::checked_cell::CellProgram>,
    ) -> Result<Arc<RuntimeCheckedPrefix>, SessionError> {
        if !admission.belongs_to(self)
            || first_item.admission_digest() != admission.digest()
            || first_item.specification_digest() != admission.specification_digest()
            || first_item.include_paths().len() != admission.include_paths().len()
            || !first_item
                .include_paths()
                .iter()
                .zip(admission.include_paths())
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
            || first_item.injected_modules().len() != admission.view.injected_values.len()
            || !admission
                .view
                .injected_values
                .iter()
                .zip(first_item.injected_modules())
                .all(|(module, supplied)| module.module_name().eq(supplied))
            || !initial_interfaces_match(
                &admission.interfaces,
                first_item.baseline_value_interfaces(),
            )
            || self
                .public_visibility_snapshot_in(admission.visibility.scope)
                .as_ref()
                != Some(&admission.visibility)
            || self.compile_view_digest_in(admission.visibility.scope)
                != Some(admission.view_digest)
        {
            return Err(SessionError::StaleStagedDeclaration);
        }
        let (interfaces, interface_index) = CheckedInterfaces::base(&admission.interfaces);
        let snapshot = Arc::new(checked_snapshot(
            admission.view.clone(),
            admission.view_digest,
            admission.visibility.clone(),
            interfaces,
            first_item.initial_prefix()?,
            admission.digest(),
            admission._retained_scope.clone(),
            CheckedNativeImports::base(admission.native_imports.clone()),
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
        let native_index = CheckedNativeIndex::new(admission.native_imports.clone());
        let compile_inputs = admission.compile_inputs.clone();
        Ok(Arc::new(RuntimeCheckedPrefix {
            admission,
            first_item,
            program,
            state: parking_lot::Mutex::new(RuntimeCheckedState {
                snapshot,
                in_flight: None,
                reservation: None,
                interface_index,
                native_index,
                compile_inputs,
            }),
        }))
    }
    /// Capsule drops happen outside checkout. The next owner entry retires
    /// their detached lease scopes through the same binding/native owner.
    pub(crate) fn reap_admission_leases(&mut self) {
        let retired = std::mem::take(&mut *self.admission_owner().retired.lock());
        for scope in retired {
            self.retire_scope(scope);
        }
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
        let state = self
            .lib()
            .durable_graph
            .as_ref()
            .expect("validated durable graph");
        Ok(Arc::new(RuntimeDurablePublicReadiness {
            owner: self.admission_owner().clone(),
            owner_epoch: self.admission_owner().epoch(),
            session: self.lib().session_id(),
            durable: owner.clone(),
            scope: public_scope,
            confirmation: Arc::downgrade(&state.unconfirmed.0),
        }))
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
        let surface = state
            .graph
            .public_surfaces()
            .find(|surface| &surface.owner == owner)
            .ok_or_else(|| fail(Failure::MissingSurface))?;
        if state.unconfirmed.is_some() {
            return Err(fail(Failure::Unconfirmed));
        }
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
        if let Some(live) = state.retained.lock().clone() {
            if live
                .observe(
                    &state.graph,
                    &state.path,
                    state.path.parent().expect("canonical manifest parent"),
                )
                .map_err(|error| SessionError::RecoveryManifest {
                    path: state.path.clone(),
                    detail: error.to_string(),
                })?
            {
                retained.validate_owner()?;
                return Ok(snapshot);
            }
        }
        let bytes = std::fs::read(&state.path).map_err(|error| SessionError::RecoveryManifest {
            path: state.path.clone(),
            detail: error.to_string(),
        })?;
        let read = super::recovery::read_v2_bytes(
            &state.path,
            state.path.parent().expect("canonical manifest parent"),
            &bytes,
            super::recovery::RecoveryReadPurpose::Metadata,
        )
        .map_err(|error| SessionError::RecoveryManifest {
            path: state.path.clone(),
            detail: error.to_string(),
        })?
        .ok_or_else(|| fail(Failure::UnrecognizedManifest))?;
        if !read.artifact_losses.is_empty() {
            return Err(fail(Failure::ArtifactLoss {
                count: read.artifact_losses.len(),
            }));
        }
        if read.graph.checksum() != state.graph.checksum() {
            return Err(fail(Failure::ManifestChecksum {
                published: read.graph.checksum().to_owned(),
                current: state.graph.checksum().to_owned(),
            }));
        }
        *state.retained.lock() = read.retain(&bytes);
        retained.validate_owner()?;
        Ok(snapshot)
    }

    pub fn begin_private_execution(
        &mut self,
        public_scope: ScopeId,
    ) -> Result<PrivateExecutionAdmission, SessionError> {
        self.reap_admission_leases();
        let durable_owner = self
            .lib()
            .durable_public_scopes
            .iter()
            .find_map(|(owner, scope)| (*scope == public_scope).then(|| owner.clone()));
        let admitted = self
            .public_visibility_snapshot_in(public_scope)
            .ok_or(SessionError::DeadScope(public_scope))?;
        self.lib_mut()
            .seed_scope(public_scope, admitted.declaration_tip);
        let private_scope = self
            .mint_detached_scope(public_scope)
            .ok_or(SessionError::DeadScope(public_scope))?;
        let view = self
            .compile_view_in(private_scope)
            .expect("fresh detached scope has a library")
            .with_scoped_injection();
        let binding_tip = self
            .binding_tip_id(private_scope)
            .expect("detached scope captures a binding tip");
        Ok(PrivateExecutionAdmission {
            owner: self.admission_owner().clone(),
            owner_epoch: self.admission_owner().epoch(),
            durable_owner,
            scope_lease: Arc::new(RuntimeLexicalScopeLease {
                owner: self.admission_owner().clone(),
                owner_epoch: self.admission_owner().epoch(),
                scope: private_scope,
            }),
            admitted,
            private_scope,
            view,
            binding_tip,
            completed_values: parking_lot::Mutex::new(std::collections::HashMap::new()),
            final_intent: OnceLock::new(),
        })
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
            RuntimeCellPurpose::HostCarrier { binding, expected },
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
        self.ensure_machine_incarnation();
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
        let last_value_generation = Generation(
            self.val_gen()
                .0
                .checked_add(native_count)
                .ok_or(SessionError::StaleStagedDeclaration)?,
        );
        last_value_generation
            .0
            .checked_add(1)
            .ok_or(SessionError::StaleStagedDeclaration)?;
        let count_u64 =
            u64::try_from(declaration_count).map_err(|_| SessionError::StaleStagedDeclaration)?;
        self.lib()
            .log
            .generation()
            .0
            .checked_add(count_u64)
            .ok_or(SessionError::StaleStagedDeclaration)?;
        self.lib()
            .log
            .publication_revision()
            .and_then(|revision| revision.checked_add(count_u64))
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
        let retained_scope = self.retain_lexical_scope(scope)?;
        if self.lib().durable_graph.is_some() {
            reserved_generations = self
                .lib_mut()
                .reserve_declaration_generations_durable(declaration_count)?;
        } else {
            for _ in 0..declaration_count {
                reserved_generations.push(self.lib_mut().log.reserve());
            }
        }
        let initial_value_generation = view.next_value_generation();
        self.set_val_gen(last_value_generation);
        let planned = plan.map(|plan| {
            use super::planned_cell::{
                RuntimeCellPlanReservation, RuntimePlannedCellItem, RuntimePlannedCellSlot as Slot,
            };
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
            Arc::new(RuntimeCellPlanReservation {
                plan,
                items: planned_items,
                digest: [0; 32],
            })
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
        if let Some(planned) = &planned {
            frame(b"TidepoolRuntimeOrderedCell1");
            frame(&planned.plan_digest());
            frame(&planned.producer_sha256());
            for item in planned.items() {
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
            frame(&key.binder.version.0);
            frame(key.binder.binder.unit.as_bytes());
            frame(key.binder.binder.module.as_bytes());
            frame(key.binder.binder.namespace.as_bytes());
            frame(key.binder.binder.occurrence.as_bytes());
            if let Some(parent) = &key.binder.binder.record_parent {
                frame(parent.as_bytes());
            }
        }
        purpose.frame_authorization(&mut frame);
        compile_inputs.frame_authorization(&mut frame);
        let digest = *digest.finalize().as_bytes();
        let planned = planned.map(|mut planned| {
            Arc::get_mut(&mut planned)
                .expect("fresh ordered reservation has one owner")
                .digest = digest;
            planned
        });
        Ok(Arc::new(RuntimeCellAdmission {
            catalog_selection: self.catalog_selection().clone(),
            owner: self.admission_owner().clone(),
            owner_epoch: self.admission_owner().epoch(),
            purpose,
            _retained_scope: retained_scope,
            prefix_started: std::sync::atomic::AtomicBool::new(false),
            declaration_baseline,
            prepared_declarations: std::sync::OnceLock::new(),
            compile_inputs,
            view,
            view_digest,
            visibility,
            reserved_generations,
            planned,
            initial_value_generation,
            native_shares,
            native_imports,
            interfaces,
            specification,
            specification_digest,
            authority_digest,
            include_paths,
            digest,
        }))
    }

    fn capture_admitted_native_imports(
        &self,
        scope: ScopeId,
    ) -> Result<AdmittedNativeImports, SessionError> {
        if !self.scope_tree().is_live(scope) {
            return Err(SessionError::DeadScope(scope));
        }
        let mut entries = std::collections::BTreeSet::new();
        let mut bound = std::collections::BTreeSet::new();
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
            bound.insert(entry.value.identity.clone());
        }
        if let Some(engine) = self.prepared() {
            for (identity, generation) in engine.protected_code_export_retentions() {
                if bound.contains(&identity) {
                    continue;
                }
                let Some(ImportOwner::CodeExport { root_id, .. }) =
                    engine.retained_code_export_owner(&identity, generation)
                else {
                    return Err(SessionError::StaleStagedDeclaration);
                };
                hash.update(b"code-export");
                hash.update(&generation.to_le_bytes());
                hash.update(&root_id.to_le_bytes());
                frame_native_identity(&mut hash, &identity);
                owners.insert((identity.clone(), generation), root_id);
                entries.insert((identity, generation));
            }
        }
        Ok(AdmittedNativeImports {
            entries: entries.into_iter().collect(),
            owners,
            commitment: *hash.finalize().as_bytes(),
        })
    }

    fn capture_checked_native_delta<'a>(
        &self,
        scope: ScopeId,
        generation: u64,
        program: Option<tidepool_codegen::prepared_program::ProgramId>,
        known_owners: &CheckedNativeIndex,
        definitions: impl Iterator<Item = &'a SymbolIdentity>,
        bound_rows: impl Iterator<Item = (&'a str, u64)>,
    ) -> Result<CapturedNativeDelta, SessionError> {
        if !self.scope_tree().is_live(scope) {
            return Err(SessionError::DeadScope(scope));
        }
        let engine = self
            .prepared()
            .ok_or(SessionError::StaleStagedDeclaration)?;
        let mut imports = std::collections::BTreeMap::new();
        // Only exact definitions of this sealed target may join the ledger.
        // Optional native exports can be absent from a successful installation.
        for identity in definitions {
            if let Some(ImportOwner::CodeExport { root_id, .. }) =
                engine.retained_code_export_owner(identity, 0)
            {
                let from_this_install = program.is_some_and(|program| {
                    engine
                        .retained_code_export_owner_installed_by(identity, program)
                        .is_some()
                });
                if from_this_install || known_owners.get(&(identity.clone(), 0)) == Some(&root_id) {
                    imports.insert((identity.clone(), 0), root_id);
                }
            }
        }
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

    /// Capture the current private environment before freezing its recipe.
    /// Trusted setup mounts may extend this scope before whole-cell admission.
    pub fn compile_view_for_execution(
        &self,
        execution: &PrivateExecutionAdmission,
    ) -> Result<SessionCompileView, SessionError> {
        if !Arc::ptr_eq(&execution.owner, self.admission_owner())
            || execution.owner_epoch != self.admission_owner().epoch()
            || execution.view().session() != self.lib().session_id()
            || self.binding_tip_id(execution.private_scope()) != Some(execution.binding_tip())
        {
            return Err(SessionError::StaleStagedDeclaration);
        }
        self.compile_view_in(execution.private_scope())
            .map(SessionCompileView::with_scoped_injection)
            .ok_or(SessionError::DeadScope(execution.private_scope()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::{ModuleEnv, SessionId, SessionLib};

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
        let public = session.mint_scope(ScopeId::ROOT).unwrap();
        let sibling = session.mint_scope(ScopeId::ROOT).unwrap();
        let mut original =
            crate::session::prepared::tests::rooted_publication_fixture(&mut session, "x", 910);
        original.value.identity.module = original.module.module_name();
        let original_identity = original.value.identity.clone();
        let original_id = original.id;
        session.bind_in(public, original).unwrap();
        let mut historical =
            crate::session::prepared::tests::rooted_publication_fixture(&mut session, "x", 911);
        historical.value.identity.module = historical.module.module_name();
        let historical_identity = historical.value.identity.clone();
        session.bind_in(public, historical).unwrap();

        // A sibling sharing a module is still a different exact binding owner.
        let mut foreign = crate::session::prepared::tests::rooted_publication_fixture(
            &mut session,
            "foreign",
            912,
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
            crate::session::prepared::tests::rooted_publication_fixture(&mut session, "x", 913);
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
        session.publish_alias_in(public, alias, id).unwrap();
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
        assert!(admitted.interfaces().is_empty());
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
    fn settled_native_delta_excludes_ambient_exports_and_reuses_only_owned_roots() {
        use tidepool_repr::execution_schema::{testing, Group};
        let mut session = PersistentSession::new(None, 1024 * 1024);
        let baseline = Arc::new(
            session
                .capture_admitted_native_imports(ScopeId::ROOT)
                .unwrap(),
        );
        let mut index = CheckedNativeIndex::new(baseline.clone());
        let ambient_identity = testing::identity("Fixture", "entry");
        let ambient = session
            .install_prepared(testing::prepare(testing::wire_program()).unwrap())
            .unwrap();
        let mut own_wire = testing::wire_program();
        let own_identity = testing::identity("Own", "entry");
        let Group::NonRecursive(top) = &mut own_wire.bindings[0] else {
            unreachable!()
        };
        top.identity = own_identity.clone();
        let own = session
            .install_prepared(testing::prepare(own_wire).unwrap())
            .unwrap();
        let delta = session
            .capture_checked_native_delta(
                ScopeId::ROOT,
                0,
                Some(own),
                &index,
                [&ambient_identity, &own_identity].into_iter(),
                [].into_iter(),
            )
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

        // A target spelling an ambient definition does not acquire its CAF.
        let repeated = session
            .install_prepared(testing::prepare(testing::wire_program()).unwrap())
            .unwrap();
        assert_ne!(ambient, repeated);
        let absent = session
            .capture_checked_native_delta(
                ScopeId::ROOT,
                0,
                Some(repeated),
                &index,
                [&ambient_identity].into_iter(),
                [].into_iter(),
            )
            .unwrap();
        assert!(absent.imports.is_empty());
        let next = session
            .capture_checked_native_delta(
                ScopeId::ROOT,
                0,
                Some(repeated),
                &index,
                [&own_identity].into_iter(),
                [].into_iter(),
            )
            .unwrap();
        let (unchanged, added) = ledger.append(next, &index).unwrap();
        assert!(added.is_empty());
        assert!(Arc::ptr_eq(
            ledger.tail.as_ref().unwrap(),
            unchanged.tail.as_ref().unwrap()
        ));
        assert!(Arc::ptr_eq(&ledger.base, &baseline));
    }

    #[test]
    fn checked_native_index_shares_live_baseline_and_keeps_exact_delta_owners() {
        use tidepool_repr::execution_schema::{testing, Group};
        let mut session = PersistentSession::new(None, 1024 * 1024);
        let package_identity = testing::identity("Fixture", "entry");
        session
            .install_prepared(testing::prepare(testing::wire_program()).unwrap())
            .unwrap();
        let mut value = crate::session::prepared::tests::rooted_publication_fixture(
            &mut session,
            "answer",
            924,
        );
        value.value.identity.module = value.module.module_name();
        value.value.identity.namespace = "value".into();
        value.value.identity.occurrence = value.name.0.clone();
        let value_identity = value.value.identity.clone();
        session.bind_in(ScopeId::ROOT, value).unwrap();
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
        assert!(index.get(&(package_identity.clone(), 0)).is_some());
        assert!(index.get(&(package_identity.clone(), 1)).is_none());
        assert!(index.get(&(value_identity.clone(), 924)).is_some());
        assert!(index.get(&(value_identity.clone(), 925)).is_none());
        let mut foreign_package = package_identity.clone();
        foreign_package.unit.push_str("-foreign");
        assert!(index.get(&(foreign_package.clone(), 0)).is_none());

        // Runtime capture may reuse an exact baseline package export; neither
        // a different package nor a wrong value generation acquires its root.
        let reused = session
            .capture_checked_native_delta(
                ScopeId::ROOT,
                924,
                None,
                &index,
                [&package_identity, &foreign_package].into_iter(),
                [("answer", 924)].into_iter(),
            )
            .unwrap();
        assert_eq!(reused.imports.len(), 2);
        let (unchanged, added) = ledger.append(reused, &index).unwrap();
        assert!(added.is_empty());
        assert!(unchanged.tail.as_ref().unwrap().imports.is_empty());
        assert_eq!(unchanged.tail.as_ref().unwrap().bindings.len(), 1);
        assert!(session
            .capture_checked_native_delta(
                ScopeId::ROOT,
                925,
                None,
                &index,
                [].into_iter(),
                [("answer", 924)].into_iter(),
            )
            .is_err());
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
                    &index,
                )
                .is_err());
        }

        // Only the later real installation enters the mutable suffix.
        let mut wire = testing::wire_program();
        let own_identity = testing::identity("Later", "entry");
        let Group::NonRecursive(top) = &mut wire.bindings[0] else {
            unreachable!()
        };
        top.identity = own_identity.clone();
        let program = session
            .install_prepared(testing::prepare(wire).unwrap())
            .unwrap();
        let delta = session
            .capture_checked_native_delta(
                ScopeId::ROOT,
                0,
                Some(program),
                &index,
                [&own_identity].into_iter(),
                [].into_iter(),
            )
            .unwrap();
        assert_eq!(delta.imports.len(), 1);
        let root = delta.imports[0].root_id;
        let (settled, added) = ledger.append(delta, &index).unwrap();
        index.extend(added);
        assert_eq!(index.added.len(), 1);
        assert_eq!(index.get(&(own_identity.clone(), 0)), Some(&root));
        assert!(baseline.owners.get(&(own_identity.clone(), 0)).is_none());
        assert!(settled
            .append(
                CapturedNativeDelta {
                    imports: vec![SettledNativeImport {
                        identity: own_identity,
                        generation: 0,
                        root_id: root + 1,
                    }],
                    bindings: Vec::new(),
                },
                &index,
            )
            .is_err());
        drop(baseline);
        drop(ledger);
        drop(unchanged);
        drop(settled);
        assert!(retained.upgrade().is_some());
        drop(index);
        assert!(retained.upgrade().is_none());
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
        assert!(execution.view().injected_values().is_empty());
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
    fn native_admission_commits_the_live_machine_export_owner() {
        let root = tempfile::tempdir().unwrap();
        let lib =
            SessionLib::open(SessionId(991), root.path(), ModuleEnv::standalone_default()).unwrap();
        let mut session = PersistentSession::new(Some(lib), 1024 * 1024);
        let unbound = crate::session::prepared::tests::rooted_publication_fixture(
            &mut session,
            "nativeExport",
            914,
        );
        let identity = unbound.value.identity;
        let scope = session.mint_scope(ScopeId::ROOT).unwrap();
        let admitted = session.capture_admitted_native_imports(scope).unwrap();
        assert!(admitted.entries.contains(&(identity.clone(), 0)));
        let Some(ImportOwner::CodeExport { root_id, .. }) = session
            .prepared()
            .unwrap()
            .retained_code_export_owner(&identity, 0)
        else {
            panic!("real installed export must have its native owner")
        };
        assert_ne!(root_id, 0);
        let empty = PersistentSession::new(None, 1024 * 1024)
            .capture_admitted_native_imports(ScopeId::ROOT)
            .unwrap();
        assert!(empty.entries.is_empty());
        assert_ne!(admitted.commitment, empty.commitment);
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
        assert_eq!(first.admitted_public().machine_incarnation, None);
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
        let incarnation = first_cell.visibility().machine_incarnation;
        assert!(incarnation.is_some());
        assert_eq!(second_cell.visibility().machine_incarnation, incarnation);
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
            *first_cell.visibility()
        );
        assert_eq!(
            session
                .public_visibility_snapshot_in(second.private_scope())
                .unwrap(),
            *second_cell.visibility()
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
        let mut lib =
            SessionLib::open(SessionId(987), root.path(), ModuleEnv::standalone_default()).unwrap();
        tidepool_testing::with_settlement(|settlement| {
            lib.attach_recovery_graph_v2(root.path().join("declarations.json"), settlement)
        })
        .unwrap();
        let mut session = PersistentSession::new(Some(lib), 1024 * 1024);
        let public = session.mint_scope(ScopeId::ROOT).unwrap();
        let owner = RecoveryPublicOwner::new(
            &tidepool_repr::ActorPath::parse("root/owner-transfer").unwrap(),
            1,
        )
        .unwrap();
        session
            .bind_durable_public_scope(owner.clone(), public)
            .unwrap();
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
        assert_eq!(
            session
                .publish_staged_public_manifest(ticket, &decision)
                .unwrap(),
            PublicManifestCommit::Stale
        );
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
        assert_eq!(private.view().session(), second_private.view().session());
        assert_eq!(private.admitted_public().machine_incarnation, None);
        assert_eq!(second_private.admitted_public().machine_incarnation, None);
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
