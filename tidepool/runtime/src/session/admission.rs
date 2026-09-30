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
    retained_scope: ScopeId,
    view: SessionCompileView,
    visibility: PublicVisibilitySnapshot,
    reserved_generations: Vec<Generation>,
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

impl Drop for RuntimeCellAdmission {
    fn drop(&mut self) {
        self.owner.retired.lock().push(self.retained_scope);
    }
}

/// Exact injected interface bytes captured by the runtime owner. These
/// snapshots travel with an admission rather than being re-read from a mutable
/// session include tree when the compiler offer runs off checkout.
#[derive(Debug)]
pub struct AdmittedValueInterface {
    module: tidepool_repr::SessionModule,
    bytes: Arc<[u8]>,
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
    pub fn view(&self) -> &SessionCompileView {
        &self.view
    }
    pub fn visibility(&self) -> &PublicVisibilitySnapshot {
        &self.visibility
    }
    pub fn reserved_generations(&self) -> &[Generation] {
        &self.reserved_generations
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
        let retained_scope = self
            .mint_detached_scope(scope)
            .ok_or(SessionError::DeadScope(scope))?;
        Ok(Arc::new(RuntimeCellAdmission {
            owner: self.admission_owner().clone(),
            retained_scope,
            view,
            visibility,
            reserved_generations,
            native_shares,
            interfaces,
            specification,
            specification_digest,
            authority_digest,
            digest,
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::{ModuleEnv, SessionId, SessionLib};

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
