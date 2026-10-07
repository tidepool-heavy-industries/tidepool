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

/// Retained by invocation cleanup independently of the frame's ownership lease.
pub(crate) struct FormCleanup {
    pub actor: crate::ActorRef,
    pub mount: String,
    pub host: Arc<dyn FormHost>,
    result: parking_lot::Mutex<Option<Result<(), FormCause>>>,
}
impl FormCleanup {
    pub(crate) fn new(actor: crate::ActorRef, mount: String, host: Arc<dyn FormHost>) -> Arc<Self> {
        Arc::new(Self {
            actor,
            mount,
            host,
            result: parking_lot::Mutex::new(None),
        })
    }
    pub(crate) fn close(&self) -> Result<(), FormCause> {
        let mut result = self.result.lock();
        if matches!(&*result, Some(Ok(()))) {
            return Ok(());
        }
        let closed = self.host.close(self.actor, &self.mount);
        *result = Some(closed.clone());
        closed
    }
    pub(crate) fn settled(&self) {
        *self.result.lock() = Some(Ok(()));
    }
    pub(crate) fn is_closed(&self) -> bool {
        matches!(&*self.result.lock(), Some(Ok(())))
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
    fn lease(host: Arc<Host>, mount: &str) -> Arc<MountedForm> {
        Arc::new(MountedForm {
            cleanup: FormCleanup::new(actor(), mount.into(), host),
            session: tidepool_repr::SessionId(1),
            realm: tidepool_codegen::suspension::RealmId(1),
        })
    }

    #[test]
    fn dropping_one_continuation_form_closes_once_and_preserves_sibling() {
        let host = Arc::new(Host::default());
        let one = lease(host.clone(), "one");
        let sibling = lease(host.clone(), "sibling");
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
        let host = Arc::new(Host::default());
        host.fail_closes.store(1, Ordering::SeqCst);
        let mounted = lease(host.clone(), "one");
        let cleanup = mounted.cleanup.clone();
        drop(mounted);
        assert!(!cleanup.is_closed());
        assert!(matches!(
            &*cleanup.result.lock(),
            Some(Err(FormCause::FormCleanupUnconfirmed(_)))
        ));
        cleanup.close().unwrap();
        assert!(cleanup.is_closed());
        assert_eq!(host.closes.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn durable_commit_releases_live_lease_without_rewriting_answer() {
        let host = Arc::new(Host::default());
        let mounted = lease(host.clone(), "one");
        mounted.cleanup.settled();
        let cleanup = mounted.cleanup.clone();
        drop(mounted);
        cleanup.close().unwrap();
        assert_eq!(host.closes.load(Ordering::SeqCst), 0);
    }
}
