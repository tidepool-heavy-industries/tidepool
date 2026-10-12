//! Retaining an actor moves cleanup custody through the existing invocation
//! membership and kernel supervision owners; its creation authority is unchanged.
use super::*;

#[derive(Debug, Clone, Copy, PartialEq, Eq, tidepool_bridge_derive::ToHaskell)]
#[haskell(module = "Tidepool.Effects.Core")]
pub(crate) enum AgentRetentionError {
    AgentRetainUnavailable,
    AgentRetainUnauthorized,
    AgentRetainOwnerUnavailable,
    AgentRetainOwnerClosed,
}

/// Issued by the resident retention path; the run owner consumes this together
/// with its exact supervision reservation before releasing the parent's lease.
pub(crate) struct WorkerRetentionPublication {
    previous: Option<Arc<InvocationWork>>,
    destination: Option<Arc<InvocationWork>>,
    records: Arc<Mutex<std::collections::HashMap<ActorRef, ResidentActorRecord>>>,
    caller: ActorRef,
    child: LocalActorRef,
    destination_run: bool,
}

impl WorkerRetentionPublication {
    pub(super) fn new(
        previous: Option<Arc<InvocationWork>>,
        destination: Option<Arc<InvocationWork>>,
        records: Arc<Mutex<std::collections::HashMap<ActorRef, ResidentActorRecord>>>,
        caller: ActorRef,
        child: LocalActorRef,
        destination_run: bool,
    ) -> Self {
        Self {
            previous,
            destination,
            records,
            caller,
            child,
            destination_run,
        }
    }

    pub(crate) fn matches(&self, parent: ActorRef, child: ActorRef, destination_run: bool) -> bool {
        self.caller == parent
            && self.child.identity() == child
            && self.destination_run == destination_run
    }

    pub(crate) fn publish(self, supervision: impl FnOnce()) -> Result<(), AgentRetentionError> {
        InvocationWork::publish_worker_retention(
            self.previous.as_ref(),
            self.destination.as_ref(),
            self.caller,
            self.child.clone(),
            || {
                // Ordered scope admission/state locks precede actor-record access.
                // Record readers release this lock before acquiring scope custody.
                let mut records = self.records.lock();
                supervision();
                if let Some(record) = records.get_mut(&self.child.identity()) {
                    record.scheduler_root = self.destination_run;
                    record.descriptor = record
                        .descriptor
                        .clone()
                        .with_supervisor_parent((!self.destination_run).then_some(self.caller));
                }
            },
        )
    }
}

impl<H, O> ResidentKernelBehavior<H, O>
where
    H: DispatchEffect<O> + Send + 'static,
    O: OutputSink + Sync + 'static,
{
    pub(super) async fn retain_agent_resource(
        &self,
        kernel: &KernelContext,
        context: &ActorSessionContext,
        effect_owner: &CurrentEffectOwner<'_>,
        target: ActorRef,
        lifetime: crate::WorkerLifetime,
    ) -> Result<(), AgentRetentionError> {
        use AgentRetentionError::*;
        let child = kernel
            .resolve(target)
            .await
            .map_err(|_| AgentRetainUnavailable)?
            .filter(|child| child.terminal().get().is_none())
            .ok_or(AgentRetainUnavailable)?;
        if target == context.actor
            || !kernel
                .owns_child(target)
                .await
                .map_err(|_| AgentRetainUnavailable)?
        {
            return Err(AgentRetainUnauthorized);
        }
        let destination = match lifetime {
            crate::WorkerLifetime::InvocationOwned => effect_owner
                .invocation_work()
                .map(Some)
                .ok_or(AgentRetainOwnerUnavailable)?,
            crate::WorkerLifetime::InScope(
                tidepool_bridge_effects::ResourceScopeId::ScopeToken(token),
            ) => {
                fn retained(root: Arc<InvocationWork>, token: i64) -> Option<Arc<InvocationWork>> {
                    if root.scope_token() == Some(token) {
                        return Some(root);
                    }
                    root.scopes()
                        .into_iter()
                        .find_map(|scope| retained(scope, token))
                }
                let owner = self
                    .retained_scope_roots()
                    .into_iter()
                    .filter(|root| root.is_owned_by(context.actor))
                    .find_map(|root| retained(root, token))
                    .ok_or(AgentRetainOwnerUnavailable)?;
                owner
                    .with_admission(|| ())
                    .map_err(|_| AgentRetainOwnerClosed)?;
                Some(owner)
            }
            crate::WorkerLifetime::ActorOwned | crate::WorkerLifetime::RunOwned => None,
        };
        fn source(root: Arc<InvocationWork>, target: ActorRef) -> Option<Arc<InvocationWork>> {
            if root.owns_worker(target) {
                return Some(root);
            }
            root.scopes()
                .into_iter()
                .find_map(|scope| source(scope, target))
        }
        let previous = self
            .retained_scope_roots()
            .into_iter()
            .filter(|root| root.is_owned_by(context.actor))
            .find_map(|root| source(root, target));
        let destination_run = lifetime == crate::WorkerLifetime::RunOwned;
        let reservation = kernel
            .reserve_worker_lifetime_transfer(&child, destination_run)
            .await
            .map_err(|error| match error {
                crate::local_actor::LifetimeTransferError::Closed => AgentRetainOwnerClosed,
                crate::local_actor::LifetimeTransferError::Unauthorized => AgentRetainUnauthorized,
                crate::local_actor::LifetimeTransferError::Unavailable => AgentRetainUnavailable,
            })?;
        let publication = WorkerRetentionPublication::new(
            previous,
            destination,
            self.environment.actors.clone(),
            context.actor,
            child,
            destination_run,
        );
        reservation
            .publish(publication)
            .map_err(|_| AgentRetainOwnerUnavailable)?
            .await
            .map_err(|error| match error {
                crate::local_actor::run_control::WorkerTransferError::Publication(error) => error,
                _ => AgentRetainOwnerUnavailable,
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::local_actor::run_control::WorkerTransferError;
    use crate::resident_actor::invocation_work::tests::{Fixture, Owner};

    fn work(owner: ActorRef) -> Arc<InvocationWork> {
        InvocationWork::new(
            owner,
            RequestReservationOwner::Workbench {
                execution: WorkbenchExecutionId::from_digest([71; 16]),
                attempt: crate::request::WorkbenchReservationAttempt::fresh(),
            },
        )
    }

    async fn worker(fixture: &Fixture) -> (LocalActorRef, KernelContext) {
        let (send, receive) = tokio::sync::oneshot::channel();
        let child = fixture
            .kernel
            .spawn_child(None, Owner::new(send))
            .await
            .unwrap();
        let context = receive.await.unwrap();
        let descriptor = ActorDescriptor::new(
            "retention worker",
            crate::ActorPlacement {
                session: tidepool_repr::SessionId(71),
                resource_scope: RealmId::fresh(),
                lexical_scope: tidepool_codegen::scope::ScopeId::ROOT,
            },
        )
        .with_supervisor_parent(fixture.actor.identity());
        fixture.environment.actors.lock().insert(
            child.identity(),
            ResidentActorRecord {
                installed_tools: Arc::default(),
                lifecycle: context.lifecycle().clone(),
                fresh_context_seed: None,
                workbench_executions: Arc::default(),
                interactive_policy_installed: false,
                descriptor,
                bound_worktree: None,
                terminal: None,
                runtime_observation: Default::default(),
                scheduler_root: false,
                displays: Default::default(),
            },
        );
        (child, context)
    }

    fn publication(
        fixture: &Fixture,
        child: &LocalActorRef,
        previous: Option<Arc<InvocationWork>>,
        destination: Option<Arc<InvocationWork>>,
        destination_run: bool,
    ) -> WorkerRetentionPublication {
        WorkerRetentionPublication::new(
            previous,
            destination,
            fixture.environment.actors.clone(),
            fixture.actor.identity(),
            child.clone(),
            destination_run,
        )
    }

    fn assert_record(fixture: &Fixture, child: &LocalActorRef, independent: bool) {
        let records = fixture.environment.actors.lock();
        let record = &records[&child.identity()];
        assert_eq!(record.scheduler_root, independent);
        assert_eq!(
            record.descriptor.supervisor_parent(),
            (!independent).then_some(fixture.actor.identity())
        );
    }

    #[tokio::test]
    async fn dropped_retention_waiters_publish_both_lifetime_directions() {
        let fixture = Fixture::start().await;
        let (child, context) = worker(&fixture).await;
        let source = work(fixture.actor.identity());
        source.register_worker(child.clone()).unwrap();
        let reservation = fixture
            .kernel
            .reserve_worker_lifetime_transfer(&child, true)
            .await
            .unwrap();
        let waiter = reservation
            .publish(publication(
                &fixture,
                &child,
                Some(source.clone()),
                None,
                true,
            ))
            .unwrap();
        // This current-thread test does not yield between enqueue and receiver loss.
        assert!(
            source.owns_worker(child.identity()),
            "submission itself publishes nothing"
        );
        assert_record(&fixture, &child, false);
        drop(waiter);
        assert_eq!(
            fixture.kernel.lifecycle().observation().transactions,
            1,
            "queued publication retains parent admission despite waiter loss"
        );
        fixture.kernel.resolve(child.identity()).await.unwrap();
        assert!(!source.owns_worker(child.identity()));
        assert_eq!(context.supervisor_identity().await.unwrap(), None);
        assert_record(&fixture, &child, true);
        source.close();
        fixture.cleanup(&source).await;
        assert!(child.terminal().get().is_none());

        let destination = work(fixture.actor.identity());
        let reservation = fixture
            .kernel
            .reserve_worker_lifetime_transfer(&child, false)
            .await
            .unwrap();
        let waiter = reservation
            .publish(publication(
                &fixture,
                &child,
                None,
                Some(destination.clone()),
                false,
            ))
            .unwrap();
        assert!(!destination.owns_worker(child.identity()));
        drop(waiter);
        assert_eq!(
            fixture.kernel.lifecycle().observation().transactions,
            1,
            "queued publication retains parent admission despite waiter loss"
        );
        fixture.kernel.resolve(child.identity()).await.unwrap();
        assert!(destination.owns_worker(child.identity()));
        assert_eq!(
            context.supervisor_identity().await.unwrap(),
            Some(fixture.actor.identity())
        );
        assert_record(&fixture, &child, false);
        fixture.cleanup(&destination).await;
        assert!(child.terminal().get().is_some());
        fixture.finish().await;
    }

    #[tokio::test]
    async fn retention_destination_refusal_preserves_all_source_ownership() {
        let fixture = Fixture::start().await;
        let (child, context) = worker(&fixture).await;
        let source = work(fixture.actor.identity());
        let destination = source.new_scope().unwrap();
        source.register_worker(child.clone()).unwrap();
        let reservation = fixture
            .kernel
            .reserve_worker_lifetime_transfer(&child, false)
            .await
            .unwrap();
        let waiter = reservation
            .publish(publication(
                &fixture,
                &child,
                Some(source.clone()),
                Some(destination.clone()),
                false,
            ))
            .unwrap();
        destination.close();
        assert!(matches!(
            waiter.await,
            Err(WorkerTransferError::Publication(
                AgentRetentionError::AgentRetainOwnerClosed
            ))
        ));
        assert!(source.owns_worker(child.identity()));
        assert!(!destination.owns_worker(child.identity()));
        assert_eq!(
            context.supervisor_identity().await.unwrap(),
            Some(fixture.actor.identity())
        );
        assert_record(&fixture, &child, false);
        fixture.cleanup(&source).await;
        fixture.finish().await;
    }
    #[tokio::test]
    async fn retention_send_refusal_publishes_nothing() {
        let fixture = Fixture::start().await;
        let (child, _context) = worker(&fixture).await;
        let source = work(fixture.actor.identity());
        source.register_worker(child.clone()).unwrap();
        let reservation = fixture
            .kernel
            .reserve_worker_lifetime_transfer(&child, true)
            .await
            .unwrap();
        fixture.stop_directory().await;
        let result = reservation.publish(publication(
            &fixture,
            &child,
            Some(source.clone()),
            None,
            true,
        ));
        assert!(matches!(result, Err(WorkerTransferError::NotAdmitted)));
        assert!(source.owns_worker(child.identity()));
        assert_eq!(
            child
                .address()
                .get_cell()
                .try_get_supervisor()
                .unwrap()
                .get_id(),
            fixture.actor.address().get_id()
        );
        assert_record(&fixture, &child, false);
        fixture.finish_without_directory().await;
    }

    #[tokio::test]
    async fn admitted_retention_reply_loss_is_unconfirmed() {
        let fixture = Fixture::start().await;
        let (child, _context) = worker(&fixture).await;
        let source = work(fixture.actor.identity());
        source.register_worker(child.clone()).unwrap();
        let reservation = fixture
            .kernel
            .reserve_worker_lifetime_transfer(&child, true)
            .await
            .unwrap();
        let waiter = reservation
            .publish(publication(
                &fixture,
                &child,
                Some(source.clone()),
                None,
                true,
            ))
            .unwrap();
        // Ractor stop is a signal with priority over queued user messages. Either
        // commit won or the queued publication was lost; neither is no-admission.
        fixture.stop_directory().await;
        match waiter.await {
            Ok(()) => {
                assert!(!source.owns_worker(child.identity()));
                assert!(child.address().get_cell().try_get_supervisor().is_none());
                assert_record(&fixture, &child, true);
                child.address().stop(None);
            }
            Err(WorkerTransferError::SettlementUnconfirmed) => {
                assert!(source.owns_worker(child.identity()));
                assert_record(&fixture, &child, false);
            }
            other => panic!("admitted transfer misclassified: {other:?}"),
        }
        fixture.finish_without_directory().await;
    }
}
