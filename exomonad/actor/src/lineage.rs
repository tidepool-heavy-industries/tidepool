//! Checkpoint custody and independent child admission under one atomic owner.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use parking_lot::Mutex;
use tidepool_codegen::scope::ScopeId;
use tidepool_repr::SessionId;

use tidepool_runtime::session::ContextCheckpointBoundary;

use crate::ActorRef;
use crate::HostedCheckpointAttachment;

mod spawn_admission;
pub use spawn_admission::{SpawnAdmission, SpawnAdmissionOutcome, SpawnCleanupOutcome};

#[derive(Default)]
struct ActorAdmissionsState {
    spawns: HashMap<uuid::Uuid, spawn_admission::SpawnAdmissionRecord>,
    parents: HashMap<ActorRef, ActorRef>,
    descendant_limits: HashMap<ActorRef, usize>,
    active: HashSet<ActorRef>,
    checkpoints: HashMap<String, CheckpointLease>,
    // Revocation fences admission immediately; retirement remains retryable.
    released_checkpoints: HashMap<String, ReleasedCheckpoint>,
}

struct ReleasedCheckpoint {
    session: SessionId,
    scope: Option<ScopeId>,
    cleanup_pending: bool,
    retained_scope: Option<std::sync::Weak<tidepool_runtime::session::RuntimeLexicalScopeLease>>,
}

/// One atomic owner for checkpoint leases and independent spawn reservations.
#[derive(Clone)]
pub struct ActorAdmissionRegistry {
    state: Arc<Mutex<ActorAdmissionsState>>,
    checkpoint_namespace: uuid::Uuid,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, tidepool_bridge_derive::ToHaskell)]
pub enum CheckpointRefusal {
    #[haskell(module = "Tidepool.Effects.Core")]
    NoHostedBoundary,
    #[haskell(module = "Tidepool.Effects.Core")]
    WrongSession,
    #[haskell(module = "Tidepool.Effects.Core")]
    UnavailableCheckpoint,
    #[haskell(module = "Tidepool.Effects.Core")]
    ReleasedCheckpoint,
    #[haskell(module = "Tidepool.Effects.Core")]
    CaptureFailed,
    #[haskell(module = "Tidepool.Effects.Core")]
    ProcessRestartUnsupported,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CheckpointPhase {
    Pending,
    Published,
    Failed,
    Released,
    // A branch admitted before release may still finish provider binding.
    ReleasedAfterPublication,
}

/// Checkpoint metadata used to validate a proposed launch, not child admission.
pub(crate) struct CheckpointPreview(CheckpointLease);

impl CheckpointPreview {
    pub(crate) fn lease(&self) -> &CheckpointLease {
        &self.0
    }
}

#[derive(Clone)]
pub struct CheckpointLease {
    pub name: String,
    pub issuer: ActorRef,
    budget_sponsors: Vec<ActorRef>,
    pub issuer_capabilities: crate::ActorCapabilities,
    pub(crate) issuer_persistence_policy: crate::ActorPersistencePolicy,
    pub issuer_model: Option<crate::Model>,
    pub issuer_effort: Option<crate::ForkEffort>,
    pub issuer_source_layer: crate::CheckpointSourceLayer,
    pub session: SessionId,
    pub scope: ScopeId,
    retained_scope: Option<Arc<tidepool_runtime::session::RuntimeLexicalScopeLease>>,
    pub boundary: ContextCheckpointBoundary,
    host_attachment: Arc<Mutex<Option<HostedCheckpointAttachment>>>,
    phase: tokio::sync::watch::Sender<CheckpointPhase>,
}

impl CheckpointLease {
    pub(crate) fn retained_scope(
        &self,
    ) -> Result<&Arc<tidepool_runtime::session::RuntimeLexicalScopeLease>, CheckpointRefusal> {
        self.retained_scope
            .as_ref()
            .ok_or(CheckpointRefusal::UnavailableCheckpoint)
    }

    #[must_use]
    pub fn host_attachment<T: std::any::Any + Send + Sync>(&self) -> Option<Arc<T>> {
        if matches!(
            *self.phase.borrow(),
            CheckpointPhase::Pending | CheckpointPhase::Failed
        ) {
            return None;
        }
        self.host_attachment.lock().as_ref()?.downcast()
    }
}

impl CheckpointLease {
    pub async fn wait_published(&self) -> Result<(), CheckpointRefusal> {
        let mut phase = self.phase.subscribe();
        loop {
            match *phase.borrow_and_update() {
                CheckpointPhase::Published => return Ok(()),
                CheckpointPhase::ReleasedAfterPublication => return Ok(()),
                CheckpointPhase::Failed => return Err(CheckpointRefusal::CaptureFailed),
                CheckpointPhase::Released => return Err(CheckpointRefusal::ReleasedCheckpoint),
                CheckpointPhase::Pending => {}
            }
            if phase.changed().await.is_err() {
                return Err(CheckpointRefusal::CaptureFailed);
            }
        }
    }
}

impl Default for ActorAdmissionRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl ActorAdmissionRegistry {
    #[must_use]
    pub fn new() -> Self {
        Self {
            state: Arc::new(Mutex::new(ActorAdmissionsState::default())),
            checkpoint_namespace: uuid::Uuid::new_v4(),
        }
    }

    /// Keep the captured scope under the admission ledger until
    /// explicit release or failure. The opaque token cannot move it to another machine.
    pub fn capture_checkpoint(
        &self,
        name: String,
        issuer: ActorRef,
        issuer_capabilities: crate::ActorCapabilities,
        issuer_model: Option<crate::Model>,
        issuer_effort: Option<crate::ForkEffort>,
        issuer_source_layer: crate::CheckpointSourceLayer,
        session: SessionId,
        scope: ScopeId,
        boundary: ContextCheckpointBoundary,
    ) -> String {
        self.capture_checkpoint_with_host_attachment(
            name,
            issuer,
            issuer_capabilities,
            issuer_model,
            issuer_effort,
            issuer_source_layer,
            session,
            scope,
            boundary,
            None,
        )
    }

    pub fn capture_checkpoint_with_host_attachment(
        &self,
        name: String,
        issuer: ActorRef,
        issuer_capabilities: crate::ActorCapabilities,
        issuer_model: Option<crate::Model>,
        issuer_effort: Option<crate::ForkEffort>,
        issuer_source_layer: crate::CheckpointSourceLayer,
        session: SessionId,
        scope: ScopeId,
        boundary: ContextCheckpointBoundary,
        host_attachment: Option<HostedCheckpointAttachment>,
    ) -> String {
        self.capture_checkpoint_inner(
            name,
            issuer,
            issuer_capabilities,
            issuer_model,
            issuer_effort,
            issuer_source_layer,
            session,
            scope,
            boundary,
            host_attachment,
            None,
            crate::ActorPersistencePolicy::Ephemeral,
        )
    }

    /// Runtime captures retain a detached lexical share independently of the
    /// token's original scope and every later parent settlement.
    pub fn capture_checkpoint_with_retained_scope(
        &self,
        name: String,
        issuer: ActorRef,
        issuer_capabilities: crate::ActorCapabilities,
        issuer_model: Option<crate::Model>,
        issuer_effort: Option<crate::ForkEffort>,
        issuer_source_layer: crate::CheckpointSourceLayer,
        session: SessionId,
        scope: ScopeId,
        boundary: ContextCheckpointBoundary,
        host_attachment: Option<HostedCheckpointAttachment>,
        retained_scope: Arc<tidepool_runtime::session::RuntimeLexicalScopeLease>,
        issuer_persistence_policy: crate::ActorPersistencePolicy,
    ) -> String {
        self.capture_checkpoint_inner(
            name,
            issuer,
            issuer_capabilities,
            issuer_model,
            issuer_effort,
            issuer_source_layer,
            session,
            scope,
            boundary,
            host_attachment,
            Some(retained_scope),
            issuer_persistence_policy,
        )
    }

    fn capture_checkpoint_inner(
        &self,
        name: String,
        issuer: ActorRef,
        issuer_capabilities: crate::ActorCapabilities,
        issuer_model: Option<crate::Model>,
        issuer_effort: Option<crate::ForkEffort>,
        issuer_source_layer: crate::CheckpointSourceLayer,
        session: SessionId,
        scope: ScopeId,
        boundary: ContextCheckpointBoundary,
        host_attachment: Option<HostedCheckpointAttachment>,
        retained_scope: Option<Arc<tidepool_runtime::session::RuntimeLexicalScopeLease>>,
        issuer_persistence_policy: crate::ActorPersistencePolicy,
    ) -> String {
        let token = format!("{}:{}", self.checkpoint_namespace, uuid::Uuid::new_v4());
        let (phase, _) = tokio::sync::watch::channel(CheckpointPhase::Pending);
        let mut state = self.state.lock();
        let budget_sponsors = spawn_admission::actor_budget_sponsors(&state, issuer);
        if let Some(limit) = issuer_capabilities.descendants().maximum_active_children {
            state
                .descendant_limits
                .entry(issuer)
                .and_modify(|old| *old = (*old).min(usize::from(limit)))
                .or_insert(usize::from(limit));
        }
        state.checkpoints.insert(
            token.clone(),
            CheckpointLease {
                name,
                issuer,
                budget_sponsors,
                issuer_capabilities,
                issuer_persistence_policy,
                issuer_model,
                issuer_effort,
                issuer_source_layer,
                session,
                scope,
                retained_scope,
                boundary,
                host_attachment: Arc::new(Mutex::new(host_attachment)),
                phase,
            },
        );
        token
    }

    pub fn checkpoint(
        &self,
        token: &str,
        session: SessionId,
    ) -> Result<CheckpointLease, CheckpointRefusal> {
        let (namespace, _) = token
            .split_once(':')
            .ok_or(CheckpointRefusal::UnavailableCheckpoint)?;
        if namespace != self.checkpoint_namespace.to_string() {
            return Err(CheckpointRefusal::ProcessRestartUnsupported);
        }
        let state = self.state.lock();
        let lease = match state.checkpoints.get(token) {
            Some(lease) => lease.clone(),
            None => {
                return match state.released_checkpoints.get(token) {
                    Some(released) if released.session == session => {
                        Err(CheckpointRefusal::ReleasedCheckpoint)
                    }
                    Some(_) => Err(CheckpointRefusal::WrongSession),
                    None => Err(CheckpointRefusal::UnavailableCheckpoint),
                };
            }
        };
        if lease.session != session {
            return Err(CheckpointRefusal::WrongSession);
        }
        match *lease.phase.borrow() {
            CheckpointPhase::Failed => return Err(CheckpointRefusal::CaptureFailed),
            CheckpointPhase::Released | CheckpointPhase::ReleasedAfterPublication => {
                return Err(CheckpointRefusal::ReleasedCheckpoint)
            }
            CheckpointPhase::Pending | CheckpointPhase::Published => {}
        }
        Ok(lease)
    }

    /// Preview metadata for launch validation. Release may still win before
    /// claim_spawn atomically admits the child and checkpoint installation share.
    pub(crate) fn preview_checkpoint(
        &self,
        token: &str,
        session: SessionId,
    ) -> Result<CheckpointPreview, CheckpointRefusal> {
        self.checkpoint_admission_locked(&self.state.lock(), token, session)
            .map(|(lease, _)| CheckpointPreview(lease))
    }

    fn checkpoint_admission_locked(
        &self,
        state: &ActorAdmissionsState,
        token: &str,
        session: SessionId,
    ) -> Result<(CheckpointLease, Option<HostedCheckpointAttachment>), CheckpointRefusal> {
        let (namespace, _) = token
            .split_once(':')
            .ok_or(CheckpointRefusal::UnavailableCheckpoint)?;
        if namespace != self.checkpoint_namespace.to_string() {
            return Err(CheckpointRefusal::ProcessRestartUnsupported);
        }
        let lease = match state.checkpoints.get(token) {
            Some(lease) => lease,
            None => {
                return match state.released_checkpoints.get(token) {
                    Some(released) if released.session == session => {
                        Err(CheckpointRefusal::ReleasedCheckpoint)
                    }
                    Some(_) => Err(CheckpointRefusal::WrongSession),
                    None => Err(CheckpointRefusal::UnavailableCheckpoint),
                };
            }
        };
        if lease.session != session {
            return Err(CheckpointRefusal::WrongSession);
        }
        match *lease.phase.borrow() {
            CheckpointPhase::Pending | CheckpointPhase::Published => {}
            CheckpointPhase::Failed => return Err(CheckpointRefusal::CaptureFailed),
            CheckpointPhase::Released | CheckpointPhase::ReleasedAfterPublication => {
                return Err(CheckpointRefusal::ReleasedCheckpoint);
            }
        }
        let attachment = lease.host_attachment.lock().clone();
        Ok((lease.clone(), attachment))
    }

    /// Revoke future admissions and return the captured root for retirement.
    /// A retry returns the same scope until retirement is acknowledged.
    /// Admitted children own independent detached scopes.
    pub fn release_checkpoint(
        &self,
        token: &str,
        session: SessionId,
    ) -> Result<Option<ScopeId>, CheckpointRefusal> {
        let (namespace, _) = token
            .split_once(':')
            .ok_or(CheckpointRefusal::UnavailableCheckpoint)?;
        if namespace != self.checkpoint_namespace.to_string() {
            return Err(CheckpointRefusal::ProcessRestartUnsupported);
        }
        let mut state = self.state.lock();
        if let Some(released) = state.released_checkpoints.get(token) {
            return if released.session == session {
                Ok(released.cleanup_pending.then_some(released.scope).flatten())
            } else {
                Err(CheckpointRefusal::WrongSession)
            };
        }
        let lease = state
            .checkpoints
            .get(token)
            .ok_or(CheckpointRefusal::UnavailableCheckpoint)?;
        if lease.session != session {
            return Err(CheckpointRefusal::WrongSession);
        }
        let lease = state.checkpoints.remove(token).expect("checked checkpoint");
        let retained_scope = lease.retained_scope.as_ref().map(Arc::downgrade);
        let phase = *lease.phase.borrow();
        let scope = match phase {
            CheckpointPhase::Pending => {
                lease.phase.send_replace(CheckpointPhase::Released);
                Some(lease.scope)
            }
            CheckpointPhase::Published => {
                lease
                    .phase
                    .send_replace(CheckpointPhase::ReleasedAfterPublication);
                Some(lease.scope)
            }
            // Failed settlement returns the original captured scope for
            // immediate cleanup, but that cleanup can fail. Transfer it to
            // the same retryable release record used by published captures.
            CheckpointPhase::Failed => Some(lease.scope),
            CheckpointPhase::Released | CheckpointPhase::ReleasedAfterPublication => None,
        };
        state.released_checkpoints.insert(
            token.to_owned(),
            ReleasedCheckpoint {
                session,
                scope,
                cleanup_pending: scope.is_some(),
                retained_scope,
            },
        );
        Ok(scope)
    }

    pub fn confirm_checkpoint_release(
        &self,
        token: &str,
        session: SessionId,
        scope: ScopeId,
    ) -> Result<(), CheckpointRefusal> {
        let mut state = self.state.lock();
        let released = state
            .released_checkpoints
            .get_mut(token)
            .ok_or(CheckpointRefusal::UnavailableCheckpoint)?;
        if released.session != session {
            return Err(CheckpointRefusal::WrongSession);
        }
        if released.scope != Some(scope) {
            return Err(CheckpointRefusal::CaptureFailed);
        }
        released.cleanup_pending = false;
        Ok(())
    }

    pub fn pending_release_scopes(&self, session: SessionId) -> Vec<(String, ScopeId)> {
        self.state
            .lock()
            .released_checkpoints
            .iter()
            .filter_map(|(token, released)| {
                (released.session == session && released.cleanup_pending)
                    .then(|| released.scope.map(|scope| (token.clone(), scope)))
                    .flatten()
            })
            .collect()
    }

    pub fn settle_checkpoints(
        &self,
        issuer: ActorRef,
        boundary: &ContextCheckpointBoundary,
        success: bool,
    ) -> Vec<(SessionId, ScopeId)> {
        let mut state = self.state.lock();
        let mut retired = Vec::new();
        let mut attachments = Vec::new();
        let mut scopes = Vec::new();
        for lease in state.checkpoints.values_mut() {
            if lease.issuer == issuer
                && &lease.boundary == boundary
                && *lease.phase.borrow() == CheckpointPhase::Pending
            {
                lease.phase.send_replace(if success {
                    CheckpointPhase::Published
                } else {
                    CheckpointPhase::Failed
                });
                if !success {
                    retired.push((lease.session, lease.scope));
                    attachments.extend(lease.host_attachment.lock().take());
                    scopes.extend(lease.retained_scope.take());
                }
            }
        }
        drop(state);
        drop(attachments);
        drop(scopes);
        retired
    }

    /// Settle the one checkpoint whose token was delivered at an effect
    /// boundary. A later failure of the enclosing workbench only fails tokens
    /// still Pending; it cannot revoke this independently completed capture.
    pub fn settle_checkpoint(
        &self,
        token: &str,
        session: SessionId,
        delivered: bool,
    ) -> Result<Option<ScopeId>, CheckpointRefusal> {
        let mut state = self.state.lock();
        let lease = state
            .checkpoints
            .get_mut(token)
            .ok_or(CheckpointRefusal::UnavailableCheckpoint)?;
        if lease.session != session {
            return Err(CheckpointRefusal::WrongSession);
        }
        let phase = *lease.phase.borrow();
        let mut attachment = None;
        let mut retained_scope = None;
        let result = match phase {
            CheckpointPhase::Pending => {
                lease.phase.send_replace(if delivered {
                    CheckpointPhase::Published
                } else {
                    CheckpointPhase::Failed
                });
                if !delivered {
                    attachment = lease.host_attachment.lock().take();
                    retained_scope = lease.retained_scope.take();
                }
                Ok((!delivered).then_some(lease.scope))
            }
            CheckpointPhase::Published if delivered => Ok(None),
            CheckpointPhase::Failed if !delivered => Ok(None),
            CheckpointPhase::Released | CheckpointPhase::ReleasedAfterPublication => {
                Err(CheckpointRefusal::ReleasedCheckpoint)
            }
            _ => Err(CheckpointRefusal::CaptureFailed),
        };
        drop(state);
        drop(attachment);
        drop(retained_scope);
        result
    }

    pub fn fail_issuer_checkpoints(&self, issuer: ActorRef) {
        let mut state = self.state.lock();
        let mut attachments = Vec::new();
        let mut scopes = Vec::new();
        for lease in state.checkpoints.values_mut() {
            if lease.issuer == issuer && *lease.phase.borrow() == CheckpointPhase::Pending {
                lease.phase.send_replace(CheckpointPhase::Failed);
                attachments.extend(lease.host_attachment.lock().take());
                scopes.extend(lease.retained_scope.take());
            }
        }
        drop(state);
        drop(attachments);
        drop(scopes);
    }

    pub fn failed_checkpoint_scopes(&self, issuer: ActorRef) -> Vec<(SessionId, ScopeId)> {
        self.state
            .lock()
            .checkpoints
            .values()
            .filter(|lease| {
                lease.issuer == issuer && *lease.phase.borrow() == CheckpointPhase::Failed
            })
            .map(|lease| (lease.session, lease.scope))
            .collect()
    }

    /// A published lease keeps its issuing machine resident after the actor
    /// and its ordinary supervision scope retire.
    pub fn retains_session(&self, session: SessionId) -> bool {
        let state = self.state.lock();
        state.checkpoints.values().any(|lease| {
            lease.session == session && *lease.phase.borrow() == CheckpointPhase::Published
        }) || state.released_checkpoints.values().any(|released| {
            released.session == session
                && (released.cleanup_pending
                    || released
                        .retained_scope
                        .as_ref()
                        .is_some_and(|scope| scope.strong_count() != 0))
        })
    }

    pub fn retire_actor(&self, actor: ActorRef) {
        self.state.lock().active.remove(&actor);
    }
}

#[cfg(test)]
#[path = "lineage/capture_lifetime_tests.rs"]
mod capture_lifetime_tests;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ActorId, ActorRef};
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct AttachmentDrop(Arc<AtomicUsize>);

    impl Drop for AttachmentDrop {
        fn drop(&mut self) {
            self.0.fetch_add(1, Ordering::SeqCst);
        }
    }

    fn checkpoint_with_attachment(
        registry: &ActorAdmissionRegistry,
        issuer: ActorRef,
        boundary: ContextCheckpointBoundary,
        scope: ScopeId,
    ) -> (String, Arc<AtomicUsize>) {
        let drops = Arc::new(AtomicUsize::new(0));
        let attachment =
            HostedCheckpointAttachment::new(Arc::new(AttachmentDrop(Arc::clone(&drops))));
        let caller_share = attachment.clone();
        let token = registry.capture_checkpoint_with_host_attachment(
            "captured".into(),
            issuer,
            crate::ActorCapabilities::default(),
            None,
            None,
            crate::CheckpointSourceLayer::default(),
            SessionId(7),
            scope,
            boundary,
            Some(attachment),
        );
        drop(caller_share);
        assert_eq!(drops.load(Ordering::SeqCst), 0);
        (token, drops)
    }

    #[test]
    fn undelivered_checkpoint_drops_opaque_host_attachment() {
        let registry = ActorAdmissionRegistry::new();
        let issuer = ActorRef::first(ActorId(1));
        let (token, drops) = checkpoint_with_attachment(
            &registry,
            issuer,
            ContextCheckpointBoundary::external("thread".into(), "call".into(), "call".into()),
            ScopeId(3),
        );
        let retained_waiter = registry.checkpoint(&token, SessionId(7)).unwrap();
        assert_eq!(
            registry.settle_checkpoint(&token, SessionId(7), false),
            Ok(Some(ScopeId(3)))
        );
        assert!(retained_waiter
            .host_attachment::<AttachmentDrop>()
            .is_none());
        assert_eq!(drops.load(Ordering::SeqCst), 1);
        assert_eq!(
            registry.settle_checkpoint(&token, SessionId(7), false),
            Ok(None)
        );
        assert_eq!(drops.load(Ordering::SeqCst), 1);
        assert!(matches!(
            registry.checkpoint(&token, SessionId(7)),
            Err(CheckpointRefusal::CaptureFailed)
        ));
    }

    #[test]
    fn failed_workbench_boundary_drops_only_its_pending_host_attachments() {
        let registry = ActorAdmissionRegistry::new();
        let issuer = ActorRef::first(ActorId(1));
        let failed_boundary =
            ContextCheckpointBoundary::external("thread".into(), "failed".into(), "failed".into());
        let live_boundary =
            ContextCheckpointBoundary::external("thread".into(), "live".into(), "live".into());
        let (_, failed_drops) =
            checkpoint_with_attachment(&registry, issuer, failed_boundary.clone(), ScopeId(3));
        let (live, live_drops) =
            checkpoint_with_attachment(&registry, issuer, live_boundary.clone(), ScopeId(4));
        assert_eq!(
            registry.settle_checkpoints(issuer, &failed_boundary, false),
            vec![(SessionId(7), ScopeId(3))]
        );
        assert_eq!(failed_drops.load(Ordering::SeqCst), 1);
        assert_eq!(live_drops.load(Ordering::SeqCst), 0);
        registry.settle_checkpoints(issuer, &live_boundary, true);
        assert_eq!(live_drops.load(Ordering::SeqCst), 0);
        assert_eq!(
            registry.release_checkpoint(&live, SessionId(7)),
            Ok(Some(ScopeId(4)))
        );
        assert_eq!(live_drops.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn issuer_failure_drops_only_pending_host_attachments() {
        let registry = ActorAdmissionRegistry::new();
        let issuer = ActorRef::first(ActorId(1));
        let other = ActorRef::first(ActorId(2));
        let boundary =
            ContextCheckpointBoundary::external("thread".into(), "call".into(), "call".into());
        let (_, failed_drops) =
            checkpoint_with_attachment(&registry, issuer, boundary.clone(), ScopeId(3));
        let (published, published_drops) = checkpoint_with_attachment(
            &registry,
            issuer,
            ContextCheckpointBoundary::external("thread".into(), "call".into(), "published".into()),
            ScopeId(4),
        );
        let (unrelated, unrelated_drops) =
            checkpoint_with_attachment(&registry, other, boundary, ScopeId(5));
        assert_eq!(
            registry.settle_checkpoint(&published, SessionId(7), true),
            Ok(None)
        );
        registry.fail_issuer_checkpoints(issuer);
        registry.fail_issuer_checkpoints(issuer);
        assert_eq!(failed_drops.load(Ordering::SeqCst), 1);
        assert_eq!(published_drops.load(Ordering::SeqCst), 0);
        assert_eq!(unrelated_drops.load(Ordering::SeqCst), 0);
        assert_eq!(
            registry.release_checkpoint(&published, SessionId(7)),
            Ok(Some(ScopeId(4)))
        );
        assert_eq!(
            registry.release_checkpoint(&unrelated, SessionId(7)),
            Ok(Some(ScopeId(5)))
        );
        assert_eq!(published_drops.load(Ordering::SeqCst), 1);
        assert_eq!(unrelated_drops.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn checkpoint_waits_for_exact_boundary_and_survives_issuer_retirement() {
        let registry = ActorAdmissionRegistry::new();
        let issuer = ActorRef::first(ActorId(1));
        let boundary =
            ContextCheckpointBoundary::external("thread".into(), "call".into(), "call".into());
        let token = registry.capture_checkpoint(
            "capture".into(),
            issuer,
            crate::ActorCapabilities::default(),
            None,
            None,
            crate::CheckpointSourceLayer::default(),
            SessionId(7),
            ScopeId(3),
            boundary.clone(),
        );
        let lease = registry.checkpoint(&token, SessionId(7)).unwrap();
        assert!(matches!(
            registry.checkpoint(&token, SessionId(8)),
            Err(CheckpointRefusal::WrongSession)
        ));
        assert_eq!(*lease.phase.borrow(), CheckpointPhase::Pending);
        assert!(!registry.retains_session(SessionId(7)));
        assert!(registry
            .settle_checkpoints(
                issuer,
                &ContextCheckpointBoundary::external(
                    "thread".into(),
                    "other".into(),
                    "other".into(),
                ),
                true
            )
            .is_empty());
        assert_eq!(*lease.phase.borrow(), CheckpointPhase::Pending);
        registry.settle_checkpoints(issuer, &boundary, true);
        registry.retire_actor(issuer);
        assert!(registry.retains_session(SessionId(7)));
        lease.wait_published().await.unwrap();
        assert_eq!(
            registry.checkpoint(&token, SessionId(7)).unwrap().scope,
            ScopeId(3)
        );
    }

    #[tokio::test]
    async fn failed_checkpoint_refuses_delegation_and_restart_namespace() {
        let registry = ActorAdmissionRegistry::new();
        let issuer = ActorRef::first(ActorId(1));
        let boundary =
            ContextCheckpointBoundary::external("thread".into(), "call".into(), "call".into());
        let token = registry.capture_checkpoint(
            "capture".into(),
            issuer,
            crate::ActorCapabilities::default(),
            None,
            None,
            crate::CheckpointSourceLayer::default(),
            SessionId(7),
            ScopeId(3),
            boundary.clone(),
        );
        let lease = registry.checkpoint(&token, SessionId(7)).unwrap();
        assert_eq!(
            registry.settle_checkpoints(issuer, &boundary, false),
            vec![(SessionId(7), ScopeId(3))]
        );
        assert_eq!(
            lease.wait_published().await,
            Err(CheckpointRefusal::CaptureFailed)
        );
        assert!(matches!(
            registry.checkpoint(&token, SessionId(7)),
            Err(CheckpointRefusal::CaptureFailed)
        ));
        let restarted = ActorAdmissionRegistry::new();
        assert!(matches!(
            restarted.checkpoint(&token, SessionId(7)),
            Err(CheckpointRefusal::ProcessRestartUnsupported)
        ));
    }

    #[tokio::test]
    async fn delivered_checkpoint_survives_later_failure_of_its_workbench_boundary() {
        let registry = ActorAdmissionRegistry::new();
        let issuer = ActorRef::first(ActorId(1));
        let boundary =
            ContextCheckpointBoundary::external("thread".into(), "call".into(), "call".into());
        let capture = |name: &str, scope: ScopeId| {
            registry.capture_checkpoint(
                name.into(),
                issuer,
                crate::ActorCapabilities::default(),
                None,
                None,
                crate::CheckpointSourceLayer::default(),
                SessionId(7),
                scope,
                boundary.clone(),
            )
        };
        let delivered = capture("completed item", ScopeId(3));
        let incomplete = capture("failed item", ScopeId(4));
        let lease = registry.checkpoint(&delivered, SessionId(7)).unwrap();
        assert_eq!(
            registry.settle_checkpoint(&delivered, SessionId(7), true),
            Ok(None)
        );
        lease.wait_published().await.unwrap();
        assert_eq!(
            registry.settle_checkpoints(issuer, &boundary, false),
            vec![(SessionId(7), ScopeId(4))]
        );
        assert!(registry.checkpoint(&delivered, SessionId(7)).is_ok());
        assert!(registry.retains_session(SessionId(7)));
        assert_eq!(
            registry.checkpoint(&incomplete, SessionId(7)).err(),
            Some(CheckpointRefusal::CaptureFailed)
        );
        assert_eq!(
            registry.release_checkpoint(&delivered, SessionId(7)),
            Ok(Some(ScopeId(3)))
        );
        assert!(registry.retains_session(SessionId(7)));
        registry
            .confirm_checkpoint_release(&delivered, SessionId(7), ScopeId(3))
            .unwrap();
        assert!(!registry.retains_session(SessionId(7)));
    }

    #[tokio::test]
    async fn release_is_idempotent_and_revokes_pending_or_published_lease() {
        let registry = ActorAdmissionRegistry::new();
        let issuer = ActorRef::first(ActorId(1));
        let boundary =
            ContextCheckpointBoundary::external("thread".into(), "call".into(), "call".into());
        let capture = || {
            registry.capture_checkpoint(
                "seed".into(),
                issuer,
                crate::ActorCapabilities::default(),
                None,
                None,
                crate::CheckpointSourceLayer::default(),
                SessionId(7),
                ScopeId(3),
                boundary.clone(),
            )
        };
        let pending = capture();
        let pending_lease = registry.checkpoint(&pending, SessionId(7)).unwrap();
        assert_eq!(
            registry.release_checkpoint(&pending, SessionId(8)),
            Err(CheckpointRefusal::WrongSession)
        );
        assert_eq!(
            registry.release_checkpoint(&pending, SessionId(7)),
            Ok(Some(ScopeId(3)))
        );
        assert_eq!(
            registry.release_checkpoint(&pending, SessionId(7)),
            Ok(Some(ScopeId(3)))
        );
        registry
            .confirm_checkpoint_release(&pending, SessionId(7), ScopeId(3))
            .unwrap();
        assert_eq!(
            registry.release_checkpoint(&pending, SessionId(7)),
            Ok(None)
        );
        registry.settle_checkpoints(issuer, &boundary, true);
        assert_eq!(
            pending_lease.wait_published().await,
            Err(CheckpointRefusal::ReleasedCheckpoint)
        );
        assert!(matches!(
            registry.checkpoint(&pending, SessionId(7)),
            Err(CheckpointRefusal::ReleasedCheckpoint)
        ));
        assert_eq!(
            registry.release_checkpoint(&pending, SessionId(8)),
            Err(CheckpointRefusal::WrongSession)
        );
        let published = capture();
        let published_lease = registry.checkpoint(&published, SessionId(7)).unwrap();
        registry.settle_checkpoints(issuer, &boundary, true);
        assert!(registry.retains_session(SessionId(7)));
        assert_eq!(
            registry.release_checkpoint(&published, SessionId(7)),
            Ok(Some(ScopeId(3)))
        );
        assert!(registry.retains_session(SessionId(7)));
        registry
            .confirm_checkpoint_release(&published, SessionId(7), ScopeId(3))
            .unwrap();
        assert_eq!(
            registry.confirm_checkpoint_release(&published, SessionId(7), ScopeId(3)),
            Ok(())
        );
        assert_eq!(
            registry.confirm_checkpoint_release(&published, SessionId(7), ScopeId(4)),
            Err(CheckpointRefusal::CaptureFailed)
        );
        assert!(!registry.retains_session(SessionId(7)));
        published_lease.wait_published().await.unwrap();
        assert_eq!(
            registry.release_checkpoint(&published, SessionId(7)),
            Ok(None)
        );
        let restarted = ActorAdmissionRegistry::new();
        assert_eq!(
            restarted.release_checkpoint(&published, SessionId(7)),
            Err(CheckpointRefusal::ProcessRestartUnsupported)
        );
    }

    #[test]
    fn failed_checkpoint_release_retains_scope_until_retry_is_confirmed() {
        let registry = ActorAdmissionRegistry::new();
        let issuer = ActorRef::first(ActorId(1));
        let boundary =
            ContextCheckpointBoundary::external("thread".into(), "call".into(), "call".into());
        let token = registry.capture_checkpoint(
            "failed capture".into(),
            issuer,
            crate::ActorCapabilities::default(),
            None,
            None,
            crate::CheckpointSourceLayer::default(),
            SessionId(7),
            ScopeId(3),
            boundary,
        );

        // Failed settlement starts retirement of the original captured scope.
        assert_eq!(
            registry.settle_checkpoint(&token, SessionId(7), false),
            Ok(Some(ScopeId(3)))
        );
        assert_eq!(
            registry.failed_checkpoint_scopes(issuer),
            vec![(SessionId(7), ScopeId(3))]
        );

        // Model a failed first retirement by withholding confirmation.
        // Releasing the token transfers the same obligation into the retryable
        // released-checkpoint record.
        assert_eq!(
            registry.release_checkpoint(&token, SessionId(7)),
            Ok(Some(ScopeId(3)))
        );
        assert!(registry.failed_checkpoint_scopes(issuer).is_empty());
        assert_eq!(
            registry.pending_release_scopes(SessionId(7)),
            vec![(token.clone(), ScopeId(3))]
        );
        assert!(registry.retains_session(SessionId(7)));

        // An unconfirmed retry returns the original scope again. Confirmation
        // is the only operation that consumes the pending retirement.
        assert_eq!(
            registry.release_checkpoint(&token, SessionId(7)),
            Ok(Some(ScopeId(3)))
        );
        registry
            .confirm_checkpoint_release(&token, SessionId(7), ScopeId(3))
            .unwrap();
        assert!(registry.pending_release_scopes(SessionId(7)).is_empty());
        assert!(!registry.retains_session(SessionId(7)));
        assert_eq!(registry.release_checkpoint(&token, SessionId(7)), Ok(None));
    }
}
