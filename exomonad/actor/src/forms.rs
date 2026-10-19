//! Human form transport. Native frames own mounts; the host owns durable cards.
use std::sync::Arc;
use tidepool_bridge_effects::{FormAttempt, FormAttemptId, FormCause, FormLease, FormTransition};

/// The provenance supplied by the admitted native effect, never by form JSON.
#[derive(Clone, Debug)]
pub struct FormPublication {
    pub actor: crate::ActorRef,
    pub operation: Option<tidepool_runtime::session::WorkbenchOperationId>,
}

/// Installed before actor admission. Successful commit means the answer is durable.
pub trait FormHost: Send + Sync {
    /// Subscribe synchronously before reading durable attempt state. Wakes
    /// are hints; the subsequent Store read remains authoritative.
    fn changed(&self) -> futures_util::future::BoxFuture<'static, Result<(), FormCause>>;
    fn open(
        &self,
        publication: &FormPublication,
        mount: &str,
        descriptor: &serde_json::Value,
    ) -> Result<(), FormCause>;
    fn attempt(
        &self,
        actor: crate::ActorRef,
        mount: &str,
    ) -> Result<Option<FormAttempt>, FormCause>;
    fn reject(
        &self,
        actor: crate::ActorRef,
        mount: &str,
        attempt: &str,
        errors: &serde_json::Value,
    ) -> Result<FormTransition, FormCause>;
    fn commit(
        &self,
        actor: crate::ActorRef,
        mount: &str,
        attempt: &str,
        presentation: &serde_json::Value,
    ) -> Result<FormTransition, FormCause>;
    fn close(&self, actor: crate::ActorRef, mount: &str) -> Result<(), FormCause>;
    fn display(
        &self,
        publication: &FormPublication,
        display_slot: u64,
        view: &serde_json::Value,
    ) -> Result<(), FormCause>;
}

#[derive(Debug)]
pub(crate) enum FormAdmissionError {
    Source(crate::resident_workbench::generation_admission::GenerationAdmissionError),
    Form(FormCause),
}
impl From<FormCause> for FormAdmissionError {
    fn from(cause: FormCause) -> Self {
        Self::Form(cause)
    }
}

#[derive(Debug)]
enum FormPhase {
    Unregistered,
    ReservedNotStarted,
    Attempted {
        open: Option<Result<(), FormCause>>,
        close: Option<Result<(), FormCause>>,
    },
    Closed(FormCloseProof),
}

#[derive(Debug)]
enum FormCloseProof {
    NotStarted,
    HostClosed,
    Committed,
}

/// Retained by invocation cleanup independently of the frame's ownership lease.
pub(crate) struct FormCleanup {
    pub actor: crate::ActorRef,
    pub mount: String,
    pub host: Arc<dyn FormHost>,
    source: crate::resident_workbench::generation_admission::SourceDependency,
    // Retain the exact source-scope lease while the owning session is live,
    // including when world publication becomes unavailable.
    _source_scope: Arc<tidepool_runtime::session::RuntimeLexicalScopeLease>,
    phase: parking_lot::Mutex<FormPhase>,
}
impl FormCleanup {
    pub(crate) fn new(
        actor: crate::ActorRef,
        mount: String,
        host: Arc<dyn FormHost>,
        source: crate::resident_workbench::generation_admission::SourceDependency,
    ) -> Result<Arc<Self>, crate::resident_workbench::generation_admission::GenerationAdmissionError>
    {
        let scope = source.source_lease()?;
        scope.validate_source_issuer()?;
        Ok(Arc::new(Self {
            actor,
            mount,
            host,
            source,
            _source_scope: scope,
            phase: parking_lot::Mutex::new(FormPhase::Unregistered),
        }))
    }
    pub(crate) fn source_dependency(
        &self,
    ) -> &crate::resident_workbench::generation_admission::SourceDependency {
        &self.source
    }
    /// Called only while the real invocation inserts this exact form under its
    /// source/cancellation admission. No foreign host code runs here.
    pub(crate) fn reserve(&self) -> Result<(), FormCause> {
        let mut phase = self.phase.lock();
        if !matches!(&*phase, FormPhase::Unregistered) {
            return Err(FormCause::FormClosed);
        }
        *phase = FormPhase::ReservedNotStarted;
        Ok(())
    }
    /// The actual interaction cutoff encloses this attempt. Cleanup serializes
    /// on the same phase and can never close-before-open and then resurrect it.
    pub(crate) fn open(
        &self,
        publication: &FormPublication,
        descriptor: &serde_json::Value,
    ) -> Result<(), FormCause> {
        let mut phase = self.phase.lock();
        if !matches!(&*phase, FormPhase::ReservedNotStarted) {
            return Err(FormCause::FormClosed);
        }
        *phase = FormPhase::Attempted {
            open: None,
            close: None,
        };
        let result = self.host.open(publication, &self.mount, descriptor);
        if let FormPhase::Attempted { open, .. } = &mut *phase {
            *open = Some(result.clone());
        }
        result
    }
    pub(crate) fn close(&self) -> Result<(), FormCause> {
        let mut phase = self.phase.lock();
        match &*phase {
            FormPhase::Closed(_) => return Ok(()),
            FormPhase::Unregistered | FormPhase::ReservedNotStarted => {
                *phase = FormPhase::Closed(FormCloseProof::NotStarted);
                return Ok(());
            }
            FormPhase::Attempted { .. } => {}
        }
        let closed = self.host.close(self.actor, &self.mount);
        match &closed {
            Ok(()) => *phase = FormPhase::Closed(FormCloseProof::HostClosed),
            Err(_) => {
                if let FormPhase::Attempted { close, .. } = &mut *phase {
                    *close = Some(closed.clone());
                }
            }
        }
        closed
    }
    pub(crate) fn settled(&self) {
        *self.phase.lock() = FormPhase::Closed(FormCloseProof::Committed);
    }
    pub(crate) fn is_closed(&self) -> bool {
        matches!(
            &*self.phase.lock(),
            FormPhase::Closed(
                FormCloseProof::NotStarted | FormCloseProof::HostClosed | FormCloseProof::Committed
            )
        )
    }
}

/// Only native parked frames retain this guard. A sibling cannot hold it alive.
pub(crate) struct MountedForm {
    pub session: tidepool_repr::SessionId,
    pub realm: tidepool_codegen::suspension::RealmId,
    pub cleanup: Arc<FormCleanup>,
}
impl Drop for MountedForm {
    fn drop(&mut self) {
        if let Err(cause) = self.cleanup.close() {
            tracing::error!(actor = ?self.cleanup.actor, mount = %self.cleanup.mount, ?cause, "form cleanup retained for invocation settlement");
        }
    }
}

pub(crate) fn lease_id(lease: &FormLease) -> &str {
    let FormLease::FormLeaseToken(id) = lease;
    id
}
pub(crate) fn attempt_id(attempt: &FormAttemptId) -> &str {
    let FormAttemptId::FormAttemptToken(id) = attempt;
    id
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[derive(Default)]
    struct Host {
        closes: AtomicUsize,
        fail_closes: AtomicUsize,
    }
    impl FormHost for Host {
        fn changed(&self) -> futures_util::future::BoxFuture<'static, Result<(), FormCause>> {
            Box::pin(std::future::pending())
        }
        fn open(
            &self,
            _: &FormPublication,
            _: &str,
            _: &serde_json::Value,
        ) -> Result<(), FormCause> {
            Ok(())
        }
        fn attempt(&self, _: crate::ActorRef, _: &str) -> Result<Option<FormAttempt>, FormCause> {
            Ok(None)
        }
        fn reject(
            &self,
            _: crate::ActorRef,
            _: &str,
            _: &str,
            _: &serde_json::Value,
        ) -> Result<FormTransition, FormCause> {
            Ok(FormTransition::FormStale)
        }
        fn commit(
            &self,
            _: crate::ActorRef,
            _: &str,
            _: &str,
            _: &serde_json::Value,
        ) -> Result<FormTransition, FormCause> {
            Ok(FormTransition::FormApplied)
        }
        fn close(&self, _: crate::ActorRef, _: &str) -> Result<(), FormCause> {
            self.closes.fetch_add(1, Ordering::SeqCst);
            if self
                .fail_closes
                .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |remaining| {
                    remaining.checked_sub(1)
                })
                .is_ok()
            {
                return Err(FormCause::FormCleanupUnconfirmed(
                    "durability failed".into(),
                ));
            }
            Ok(())
        }
        fn display(
            &self,
            _: &FormPublication,
            _: u64,
            _: &serde_json::Value,
        ) -> Result<(), FormCause> {
            Ok(())
        }
    }
    fn actor() -> crate::ActorRef {
        crate::ActorRef::first(crate::ActorId(1))
    }
    fn source() -> (
        tidepool_runtime::session::PersistentSession,
        crate::resident_workbench::generation_admission::SourceDependency,
    ) {
        use crate::resident_workbench::generation_admission::{BootstrapWorld, SourceDependency};
        let mut session = tidepool_runtime::session::PersistentSession::new(None, 1024);
        let scope = session.mint_notebook_scope_with_lease(None).unwrap();
        let world = BootstrapWorld::new(None);
        world.adopt_scope(scope).unwrap();
        (session, SourceDependency::World(world))
    }
    fn lease(
        host: Arc<Host>,
        mount: &str,
        source: &crate::resident_workbench::generation_admission::SourceDependency,
    ) -> Arc<MountedForm> {
        let cleanup = FormCleanup::new(actor(), mount.into(), host, source.clone()).unwrap();
        source
            .with_open_admission(|| cleanup.reserve())
            .unwrap()
            .unwrap();
        cleanup
            .open(
                &FormPublication {
                    actor: actor(),
                    operation: None,
                },
                &serde_json::json!({}),
            )
            .unwrap();
        Arc::new(MountedForm {
            cleanup,
            session: tidepool_repr::SessionId(1),
            realm: tidepool_codegen::suspension::RealmId(1),
        })
    }

    #[test]
    fn dropping_one_continuation_form_closes_once_and_preserves_sibling() {
        let (_session, source) = source();
        let host = Arc::new(Host::default());
        let one = lease(host.clone(), "one", &source);
        let sibling = lease(host.clone(), "sibling", &source);
        let cleanup = one.cleanup.clone(); // Cleanup evidence does not retain ownership.
        let weak = Arc::downgrade(&one);
        drop(one);
        assert!(weak.upgrade().is_none());
        assert!(cleanup.is_closed());
        assert!(!sibling.cleanup.is_closed());
        assert_eq!(host.closes.load(Ordering::SeqCst), 1);
        cleanup.close().unwrap();
        assert_eq!(host.closes.load(Ordering::SeqCst), 1);
        drop(sibling);
        assert_eq!(host.closes.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn failed_durable_close_remains_observable_and_retries_at_cleanup() {
        let (_session, source) = source();
        let host = Arc::new(Host::default());
        host.fail_closes.store(1, Ordering::SeqCst);
        let mounted = lease(host.clone(), "one", &source);
        let cleanup = mounted.cleanup.clone();
        drop(mounted);
        assert!(!cleanup.is_closed());
        assert!(matches!(
            &*cleanup.phase.lock(),
            FormPhase::Attempted {
                close: Some(Err(FormCause::FormCleanupUnconfirmed(_))),
                ..
            }
        ));
        cleanup.close().unwrap();
        assert!(cleanup.is_closed());
        assert_eq!(host.closes.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn pending_close_prevents_late_open_without_host_io() {
        let (_session, source) = source();
        let host = Arc::new(Host::default());
        let cleanup =
            FormCleanup::new(actor(), "pending".into(), host.clone(), source.clone()).unwrap();
        source
            .with_open_admission(|| cleanup.reserve())
            .unwrap()
            .unwrap();
        cleanup.close().unwrap();
        assert!(matches!(
            &*cleanup.phase.lock(),
            FormPhase::Closed(FormCloseProof::NotStarted)
        ));
        assert_eq!(
            cleanup.open(
                &FormPublication {
                    actor: actor(),
                    operation: None
                },
                &serde_json::json!({})
            ),
            Err(FormCause::FormClosed)
        );
        assert_eq!(host.closes.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn durable_commit_releases_live_lease_without_rewriting_answer() {
        let (_session, source) = source();
        let host = Arc::new(Host::default());
        let mounted = lease(host.clone(), "one", &source);
        let scope = source.source_lease().unwrap();
        let _fence = scope.source_admission().fence().unwrap();
        assert_eq!(
            host.commit(actor(), "one", "attempt", &serde_json::json!({})),
            Ok(FormTransition::FormApplied)
        );
        mounted.cleanup.settled();
        let cleanup = mounted.cleanup.clone();
        drop(mounted);
        cleanup.close().unwrap();
        assert_eq!(host.closes.load(Ordering::SeqCst), 0);
    }
}
