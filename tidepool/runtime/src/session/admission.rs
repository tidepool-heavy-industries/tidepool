//! Runtime-owned immutable compilation and private execution admission.

use std::any::Any;
use std::path::PathBuf;
use std::sync::{Arc, OnceLock};

use tidepool_codegen::binding_table::{BindingTipId, SourceLeaseKey};
use tidepool_codegen::scope::ScopeId;
use tidepool_repr::Generation;

use super::{PersistentSession, PublicVisibilitySnapshot, SessionCompileView, SessionError};

/// A detached lexical owner captured in the same checkout as its public base.
/// Its scope retains exact binding and native shares through the existing
/// binding-store owner; completion must retire that scope once.
pub struct PrivateExecutionAdmission {
    pub(super) owner: Arc<RuntimeAdmissionOwner>,
    pub(super) scope_lease: Arc<RuntimeLexicalScopeLease>,
    admitted: PublicVisibilitySnapshot,
    private_scope: ScopeId,
    view: SessionCompileView,
    binding_tip: BindingTipId,
    pub(super) final_intent: OnceLock<Arc<super::FinalExecutionIntent>>,
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
}

/// Protected inputs for one whole-cell compiler offer. The opaque retained
/// specification is issued by the actor owner and holds its source/tool
/// leases; compiler evidence binds its digest without interpreting authority.
/// Original declaration identities are burned before checking any source.
pub struct RuntimeCellAdmission {
    owner: Arc<RuntimeAdmissionOwner>,
    private_execution: Option<Arc<PrivateExecutionAdmission>>,
    _retained_scope: Arc<RuntimeLexicalScopeLease>,
    prefix_started: std::sync::atomic::AtomicBool,
    view: SessionCompileView,
    visibility: PublicVisibilitySnapshot,
    reserved_generations: Vec<Generation>,
    initial_value_generation: Generation,
    native_shares: Vec<SourceLeaseKey>,
    interfaces: Vec<AdmittedValueInterface>,
    specification: Arc<dyn Any + Send + Sync>,
    specification_digest: [u8; 32],
    authority_digest: [u8; 32],
    digest: [u8; 32],
}

/// An admission lifetime exists before the lazy machine bootstrap and is
/// distinct from recoverable source-session IDs and per-store counters.
pub(super) struct RuntimeAdmissionOwner {
    identity: uuid::Uuid,
    retired: parking_lot::Mutex<Vec<ScopeId>>,
}

impl RuntimeAdmissionOwner {
    pub(super) fn new() -> Self {
        Self {
            identity: uuid::Uuid::new_v4(),
            retired: parking_lot::Mutex::new(Vec::new()),
        }
    }
}

/// Shared exact lexical lifetime, issued by its existing runtime owner.
/// The detached scope retains the binding and native shares captured at mint.
pub struct RuntimeLexicalScopeLease {
    owner: Arc<RuntimeAdmissionOwner>,
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
    module: tidepool_repr::SessionModule,
    bytes: Arc<[u8]>,
}

/// Only resident settlement can extend this same-cell execution prefix.
/// Snapshots remain immutable while compilation runs off checkout.
#[derive(Debug)]
pub struct RuntimeCheckedPrefix {
    admission: Arc<RuntimeCellAdmission>,
    first_item: tidepool_toolchain::checked_cell::ExactCheckedItem,
    state: parking_lot::Mutex<RuntimeCheckedState>,
}

#[derive(Debug)]
struct RuntimeCheckedState {
    snapshot: Arc<RuntimeCheckedPrefixSnapshot>,
    in_flight: Option<Arc<tidepool_toolchain::checked_cell::ExactCompiledItem>>,
    reservation: Option<CheckedItemReservation>,
    retained_scopes: Vec<Arc<RuntimeLexicalScopeLease>>,
}

#[derive(Debug)]
struct CheckedItemReservation {
    item: tidepool_toolchain::checked_cell::ExactCheckedItem,
    generation: Generation,
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

/// A pure display of an exact observation that this runtime already captured.
/// It retains that binding and its native dependency closure independently of
/// later private items and owns the display's fresh value identity.
#[derive(Debug)]
pub struct RuntimeCheckedDisplayAdmission {
    prefix: Arc<RuntimeCheckedPrefix>,
    snapshot: Arc<RuntimeCheckedPrefixSnapshot>,
    execution: Arc<tidepool_toolchain::checked_cell::ExactCompiledItem>,
    captured_binding: super::BoundBinder,
    generation: Generation,
    budget: usize,
    presented: Vec<String>,
    digest: [u8; 32],
    _retained_scope: Arc<RuntimeLexicalScopeLease>,
}

impl RuntimeCheckedDisplayAdmission {
    pub fn prefix(&self) -> &Arc<RuntimeCheckedPrefix> {
        &self.prefix
    }
    pub fn snapshot(&self) -> &Arc<RuntimeCheckedPrefixSnapshot> {
        &self.snapshot
    }
    pub fn execution(&self) -> &Arc<tidepool_toolchain::checked_cell::ExactCompiledItem> {
        &self.execution
    }
    pub fn captured_binding(&self) -> &super::BoundBinder {
        &self.captured_binding
    }
    pub fn generation(&self) -> Generation {
        self.generation
    }
    pub fn budget(&self) -> usize {
        self.budget
    }
    pub fn presented(&self) -> &[String] {
        &self.presented
    }
    pub fn digest(&self) -> [u8; 32] {
        self.digest
    }
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

#[derive(Debug)]
pub struct RuntimeCheckedPrefixSnapshot {
    view: SessionCompileView,
    visibility: PublicVisibilitySnapshot,
    interfaces: Vec<AdmittedValueInterface>,
    native_shares: Vec<SourceLeaseKey>,
    compiler_prefix: tidepool_toolchain::checked_cell::ExactCompiledPrefix,
    digest: [u8; 32],
}

impl RuntimeCheckedPrefixSnapshot {
    pub fn view(&self) -> &SessionCompileView {
        &self.view
    }
    pub fn interfaces(&self) -> &[AdmittedValueInterface] {
        &self.interfaces
    }
    pub fn native_shares(&self) -> &[SourceLeaseKey] {
        &self.native_shares
    }
    pub fn compiler_prefix(&self) -> &tidepool_toolchain::checked_cell::ExactCompiledPrefix {
        &self.compiler_prefix
    }
    pub fn digest(&self) -> [u8; 32] {
        self.digest
    }
}

impl RuntimeCheckedPrefix {
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
        if !self.admission.belongs_to(session)
            || self.admission.visibility.scope != scope
            || state.in_flight.is_some()
            || state.reservation.as_ref().is_none_or(|reservation| {
                reservation.item != *execution.item()
                    || reservation.generation.0 != execution.generation()
            })
            || execution.item().admission_digest() != self.admission.digest()
            || session.public_visibility_snapshot_in(scope).as_ref()
                != Some(&state.snapshot.visibility)
            || session.compile_view_in(scope).is_none_or(|view| {
                view.admission_digest() != state.snapshot.view.admission_digest()
            })
        {
            return Err(SessionError::StaleStagedDeclaration);
        }
        // Appending checks the compiler-owned same-cell identity and order.
        state.snapshot.compiler_prefix.append(execution.clone())?;
        state.in_flight = Some(execution.clone());
        Ok(Arc::new(CheckedTurnCompletion {
            prefix: self.clone(),
            execution,
            scope,
        }))
    }
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
    pub(crate) fn settle(&self, session: &mut PersistentSession) -> Result<(), SessionError> {
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
        let view = session
            .compile_view_in(self.scope)
            .ok_or(SessionError::DeadScope(self.scope))?;
        let visibility = session
            .public_visibility_snapshot_in(self.scope)
            .ok_or(SessionError::DeadScope(self.scope))?;
        let mut interfaces = self.prefix.admission.interfaces.clone();
        for (module, bytes) in compiler_prefix.completed_interfaces() {
            let module = checked_value_module(module)?;
            if interfaces
                .iter()
                .any(|existing| existing.module == module && existing.bytes.as_ref() != bytes)
            {
                return Err(SessionError::StaleStagedDeclaration);
            }
            if !interfaces.iter().any(|existing| existing.module == module) {
                interfaces.push(AdmittedValueInterface {
                    module,
                    bytes: Arc::from(bytes),
                });
            }
        }
        let snapshot = Arc::new(checked_snapshot(
            view,
            visibility,
            interfaces,
            compiler_prefix,
            self.prefix.admission.digest(),
        ));
        let retained = session.retain_lexical_scope(self.scope)?;
        state.retained_scopes.push(retained);
        state.snapshot = snapshot;
        state.in_flight = None;
        state.reservation = None;
        Ok(())
    }
}

fn checked_value_module(name: &str) -> Result<tidepool_repr::SessionModule, SessionError> {
    let generation = name
        .strip_prefix("Tidepool.Session.Val.G")
        .and_then(|suffix| suffix.parse::<u64>().ok())
        .ok_or_else(|| {
            SessionError::Compile(crate::CompileError::ExtractFailed(
                "checked value interface lacks its exact Val.G identity".into(),
            ))
        })?;
    Ok(tidepool_repr::SessionModule::val(Generation(generation)))
}

fn checked_snapshot(
    view: SessionCompileView,
    visibility: PublicVisibilitySnapshot,
    interfaces: Vec<AdmittedValueInterface>,
    compiler_prefix: tidepool_toolchain::checked_cell::ExactCompiledPrefix,
    admission: [u8; 32],
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
    frame(&view.admission_digest());
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
    for interface in &interfaces {
        frame(b"interface");
        frame(interface.module.module_name().as_bytes());
        frame(&interface.bytes);
    }
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
        visibility,
        interfaces,
        native_shares,
        compiler_prefix,
        digest: *digest.finalize().as_bytes(),
    }
}

impl AdmittedValueInterface {
    pub fn module(&self) -> tidepool_repr::SessionModule {
        self.module
    }
    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }
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
    pub fn private_execution(&self) -> Option<&Arc<PrivateExecutionAdmission>> {
        self.private_execution.as_ref()
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
    pub fn initial_value_generation(&self) -> Generation {
        self.initial_value_generation
    }
    pub fn native_shares(&self) -> &[SourceLeaseKey] {
        &self.native_shares
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
    pub(super) fn belongs_to(&self, session: &PersistentSession) -> bool {
        Arc::ptr_eq(&self.owner, session.admission_owner())
    }
    pub fn specification(&self) -> &Arc<dyn Any + Send + Sync> {
        &self.specification
    }
}

impl PersistentSession {
    pub fn admit_checked_display(
        &mut self,
        prefix: Arc<RuntimeCheckedPrefix>,
        execution: Arc<tidepool_toolchain::checked_cell::ExactCompiledItem>,
        captured_binding: &super::BoundBinder,
        budget: usize,
        presented: Vec<String>,
    ) -> Result<Arc<RuntimeCheckedDisplayAdmission>, SessionError> {
        self.reap_admission_leases();
        let state = prefix.state.lock();
        let scope = prefix.admission.visibility.scope;
        let id = tidepool_repr::SessionVarId::from_extract(captured_binding.var_id);
        if !prefix.admission.belongs_to(self)
            || prefix.admission.private_execution().is_none()
            || state.in_flight.is_some()
            || state.reservation.is_some()
            || state
                .snapshot
                .compiler_prefix
                .completed_item(execution.item().index())
                .is_none_or(|completed| !Arc::ptr_eq(completed, &execution))
            || execution.observation_name() != Some(captured_binding.name.as_str())
            || self.bindings().get(id).is_none_or(|entry| {
                entry.scope != scope
                    || entry.name.0 != captured_binding.name
                    || entry.module.module_name() != captured_binding.module
                    || entry.module
                        != tidepool_repr::SessionModule::val(Generation(execution.generation()))
            })
            || self.public_visibility_snapshot_in(scope).as_ref()
                != Some(&state.snapshot.visibility)
            || self.compile_view_in(scope).is_none_or(|view| {
                view.admission_digest() != state.snapshot.view.admission_digest()
            })
        {
            return Err(SessionError::StaleStagedDeclaration);
        }
        execution.validate_bound_binders(&[super::turn::encode_bound_binder_authority(
            captured_binding,
        )])?;
        let snapshot = state.snapshot.clone();
        drop(state);
        let generation = self
            .compile_view_in(scope)
            .ok_or(SessionError::DeadScope(scope))?
            .next_value_generation();
        self.set_val_gen(generation);
        let retained_scope = self.retain_lexical_scope(scope)?;
        let mut digest = blake3::Hasher::new();
        let mut frame = |bytes: &[u8]| {
            digest.update(&(bytes.len() as u64).to_le_bytes());
            digest.update(bytes);
        };
        frame(b"TidepoolRuntimeCheckedDisplay1");
        frame(&snapshot.digest());
        frame(&(execution.item().index() as u64).to_le_bytes());
        frame(&execution.generation().to_le_bytes());
        frame(&generation.0.to_le_bytes());
        frame(&captured_binding.var_id.to_le_bytes());
        frame(captured_binding.name.as_bytes());
        frame(captured_binding.module.as_bytes());
        frame(&(budget as u64).to_le_bytes());
        frame(&(presented.len() as u64).to_le_bytes());
        for key in &presented {
            frame(key.as_bytes());
        }
        Ok(Arc::new(RuntimeCheckedDisplayAdmission {
            prefix,
            snapshot,
            execution,
            captured_binding: captured_binding.clone(),
            generation,
            budget,
            presented,
            digest: *digest.finalize().as_bytes(),
            _retained_scope: retained_scope,
        }))
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
            || item.index() != state.snapshot.compiler_prefix.next_item()
            || state.in_flight.is_some()
            || state.reservation.is_some()
            || self.public_visibility_snapshot_in(scope).as_ref()
                != Some(&state.snapshot.visibility)
            || self.compile_view_in(scope).is_none_or(|view| {
                view.admission_digest() != state.snapshot.view.admission_digest()
            })
        {
            return Err(SessionError::StaleStagedDeclaration);
        }
        let generation = if item.index() == 0 {
            prefix.admission.initial_value_generation
        } else {
            let generation = self
                .compile_view_in(scope)
                .ok_or(SessionError::DeadScope(scope))?
                .next_value_generation();
            self.set_val_gen(generation);
            generation
        };
        let snapshot = state.snapshot.clone();
        let observation_name = (item.kind()
            == tidepool_toolchain::checked_cell::CheckedItemKind::Expression)
            .then(|| {
                let mut name = format!("observation{}", generation.0);
                let visible = self.bindings().iter_current_in(self.scope_tree(), scope);
                while visible.iter().any(|(existing, _)| existing.0 == name) {
                    name.push('_');
                }
                name
            });
        state.reservation = Some(CheckedItemReservation {
            item: item.clone(),
            generation,
        });
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
    pub fn retain_lexical_scope(
        &mut self,
        source: ScopeId,
    ) -> Result<Arc<RuntimeLexicalScopeLease>, SessionError> {
        self.reap_admission_leases();
        let scope = self
            .mint_detached_scope(source)
            .ok_or(SessionError::DeadScope(source))?;
        Ok(Arc::new(RuntimeLexicalScopeLease {
            owner: self.admission_owner().clone(),
            scope,
        }))
    }
    pub fn mint_scope_from_lease(
        &mut self,
        lease: &RuntimeLexicalScopeLease,
    ) -> Result<ScopeId, SessionError> {
        self.reap_admission_leases();
        if !Arc::ptr_eq(&lease.owner, self.admission_owner()) {
            return Err(SessionError::StaleStagedDeclaration);
        }
        self.mint_detached_scope(lease.scope)
            .ok_or(SessionError::DeadScope(lease.scope))
    }
    pub fn begin_checked_prefix(
        &self,
        admission: Arc<RuntimeCellAdmission>,
        first_item: tidepool_toolchain::checked_cell::ExactCheckedItem,
    ) -> Result<Arc<RuntimeCheckedPrefix>, SessionError> {
        if !admission.belongs_to(self)
            || first_item.admission_digest() != admission.digest()
            || self
                .public_visibility_snapshot_in(admission.visibility.scope)
                .as_ref()
                != Some(&admission.visibility)
            || self
                .compile_view_in(admission.visibility.scope)
                .is_none_or(|view| view.admission_digest() != admission.view.admission_digest())
        {
            return Err(SessionError::StaleStagedDeclaration);
        }
        let snapshot = Arc::new(checked_snapshot(
            admission.view.clone(),
            admission.visibility.clone(),
            admission.interfaces.clone(),
            first_item.initial_prefix()?,
            admission.digest(),
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
        Ok(Arc::new(RuntimeCheckedPrefix {
            admission,
            first_item,
            state: parking_lot::Mutex::new(RuntimeCheckedState {
                snapshot,
                in_flight: None,
                reservation: None,
                retained_scopes: Vec::new(),
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

    pub fn begin_private_execution(
        &mut self,
        public_scope: ScopeId,
    ) -> Result<PrivateExecutionAdmission, SessionError> {
        self.reap_admission_leases();
        let admitted = self
            .public_visibility_snapshot_in(public_scope)
            .ok_or(SessionError::DeadScope(public_scope))?;
        let private_scope = self
            .mint_detached_scope(public_scope)
            .ok_or(SessionError::DeadScope(public_scope))?;
        let view = self
            .compile_view_in(private_scope)
            .expect("fresh detached scope has a library");
        let binding_tip = self
            .binding_tip_id(private_scope)
            .expect("detached scope captures a binding tip");
        Ok(PrivateExecutionAdmission {
            owner: self.admission_owner().clone(),
            scope_lease: Arc::new(RuntimeLexicalScopeLease {
                owner: self.admission_owner().clone(),
                scope: private_scope,
            }),
            admitted,
            private_scope,
            view,
            binding_tip,
            final_intent: OnceLock::new(),
        })
    }

    pub fn admit_cell_in(
        &mut self,
        scope: ScopeId,
        declaration_count: usize,
        specification: Arc<dyn Any + Send + Sync>,
        specification_digest: [u8; 32],
        authority_digest: [u8; 32],
    ) -> Result<Arc<RuntimeCellAdmission>, SessionError> {
        self.reap_admission_leases();
        let view = self
            .compile_view_in(scope)
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
        let interfaces = view
            .reachable_values()
            .iter()
            .map(|module| {
                let path: PathBuf = view.session_root().join(module.relative_hi_path());
                Ok(AdmittedValueInterface {
                    module: *module,
                    bytes: std::fs::read(path)?.into(),
                })
            })
            .collect::<Result<Vec<_>, SessionError>>()?;
        let mut reserved_generations = Vec::with_capacity(declaration_count);
        for _ in 0..declaration_count {
            let generation = if self.lib().durable_graph.is_some() {
                self.lib_mut().reserve_declaration_generation_durable()?
            } else {
                self.lib_mut().log.reserve()
            };
            reserved_generations.push(generation);
        }
        let initial_value_generation = view.next_value_generation();
        self.set_val_gen(initial_value_generation);
        let mut digest = blake3::Hasher::new();
        let mut frame = |bytes: &[u8]| {
            digest.update(&(bytes.len() as u64).to_le_bytes());
            digest.update(bytes);
        };
        frame(b"TidepoolRuntimeCellAdmission1");
        frame(self.admission_owner().identity.as_bytes());
        frame(&specification_digest);
        frame(&authority_digest);
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
        frame(&view.admission_digest());
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
        for interface in &interfaces {
            frame(interface.module.module_name().as_bytes());
            frame(&interface.bytes);
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
        let digest = *digest.finalize().as_bytes();
        let retained_scope = self.retain_lexical_scope(scope)?;
        Ok(Arc::new(RuntimeCellAdmission {
            owner: self.admission_owner().clone(),
            private_execution: None,
            _retained_scope: retained_scope,
            prefix_started: std::sync::atomic::AtomicBool::new(false),
            view,
            visibility,
            reserved_generations,
            initial_value_generation,
            native_shares,
            interfaces,
            specification,
            specification_digest,
            authority_digest,
            digest,
        }))
    }

    /// Issue executable cell authority inside the exact private execution
    /// whose token this runtime minted. The token retains its lexical owner
    /// through off-checkout compilation and parked native execution.
    pub fn admit_cell_for_execution(
        &mut self,
        execution: Arc<PrivateExecutionAdmission>,
        declaration_count: usize,
        specification: Arc<dyn Any + Send + Sync>,
        specification_digest: [u8; 32],
        authority_digest: [u8; 32],
    ) -> Result<Arc<RuntimeCellAdmission>, SessionError> {
        if !Arc::ptr_eq(&execution.owner, self.admission_owner())
            || execution.view().session() != self.lib().session_id()
            || self.binding_tip_id(execution.private_scope()) != Some(execution.binding_tip())
        {
            return Err(SessionError::StaleStagedDeclaration);
        }
        let mut admission = self.admit_cell_in(
            execution.private_scope(),
            declaration_count,
            specification,
            specification_digest,
            authority_digest,
        )?;
        Arc::get_mut(&mut admission)
            .expect("fresh runtime admission has one owner")
            .private_execution = Some(execution);
        Ok(admission)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::{ModuleEnv, SessionId, SessionLib};

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
            .admit_cell_for_execution(first.clone(), 0, Arc::new(()), [7; 32], [8; 32])
            .unwrap();
        let second_cell = session
            .admit_cell_for_execution(second.clone(), 0, Arc::new(()), [7; 32], [8; 32])
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
            .admit_cell_in(private.private_scope(), 0, Arc::new(()), [7; 32], [8; 32])
            .unwrap();
        let second_cell = second
            .admit_cell_in(
                second_private.private_scope(),
                0,
                Arc::new(()),
                [7; 32],
                [8; 32],
            )
            .unwrap();
        assert_ne!(first_cell.digest(), second_cell.digest());
        assert!(first_cell.belongs_to(&first));
        assert!(!first_cell.belongs_to(&second));
    }
}
