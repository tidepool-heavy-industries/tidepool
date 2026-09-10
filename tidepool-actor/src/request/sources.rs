use std::sync::Arc;

use super::{
    OwnerState, ProgressSnapshot, ReplyError, RequestId, RequestRecord, RequestRegistry,
    ResponseFailure, TargetState,
};
use crate::{ActorRef, LocalActorRef};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum RequestSourceKind {
    Progress,
    Settlement,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SourceTarget {
    Request(RequestId, RequestSourceKind),
    Lifecycle(ActorRef),
    Command(u128),
}

pub(crate) struct SourceBinding {
    pub target: SourceTarget,
    pub entry: Arc<tidepool_runtime::session::RootCustody>,
}

#[derive(Clone, Debug)]
pub(crate) enum SourceEvent {
    Progress(ProgressSnapshot),
    ProgressClosed,
    Settled(Result<(), ResponseFailure>),
    Lifecycle(crate::ActorLifecycle),
    Command(tidepool_bridge_effects::CommandResult),
}

/// A source publication owns its captured value until the receiving mailbox
/// handles or retires it. Later publications cannot replace that value.
#[derive(Debug, Clone)]
pub struct SourceDelivery {
    pub(crate) slot: usize,
    pub(crate) target: SourceTarget,
    pub(crate) event: SourceEvent,
}

/// One destination shared by a fixed set of source connections. The guard owns
/// the set independently of the actor identity currently receiving its events.
#[derive(Clone)]
struct SourceDestination(Arc<parking_lot::Mutex<LocalActorRef>>);

impl SourceDestination {
    fn identity(&self) -> ActorRef {
        self.0.lock().identity()
    }

    fn same_connections(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }

    fn send(&self, delivery: SourceDelivery) -> Result<(), crate::KernelCallFailure> {
        self.0.lock().source(delivery)
    }
}

pub(crate) struct RequestSourceConnection {
    recipient: SourceDestination,
    slot: usize,
    request: RequestId,
    kind: RequestSourceKind,
    closed: bool,
}

impl RequestSourceConnection {
    fn send(&mut self, event: SourceEvent) {
        if !self.closed
            && self
                .recipient
                .send(SourceDelivery {
                    slot: self.slot,
                    target: SourceTarget::Request(self.request, self.kind),
                    event,
                })
                .is_err()
        {
            self.closed = true;
        }
    }
}

pub(crate) struct ActorSourceConnections {
    registry: Arc<RequestRegistry>,
    lifecycle: Vec<crate::ActorLifecycleConnection>,
    commands: Vec<crate::command_jobs::CommandConnection>,
    recipient: SourceDestination,
}

impl ActorSourceConnections {
    pub(crate) fn attach_command(
        &mut self,
        slot: usize,
        key: u128,
        owner: ActorRef,
        jobs: &crate::command_jobs::CommandJobs,
    ) -> Result<(), tidepool_bridge_effects::CommandError> {
        let recipient = self.recipient.clone();
        let target = SourceTarget::Command(key);
        let observer = recipient.identity();
        self.commands.push(jobs.connect(
            owner,
            observer,
            &uuid::Uuid::from_u128(key).to_string(),
            move |event| {
                recipient
                    .send(SourceDelivery {
                        slot,
                        target,
                        event: SourceEvent::Command(event),
                    })
                    .is_ok()
            },
        )?);
        Ok(())
    }

    pub(crate) fn attach_lifecycle(&mut self, slot: usize, actor: &LocalActorRef) {
        let recipient = self.recipient.clone();
        let target = SourceTarget::Lifecycle(actor.identity());
        self.lifecycle
            .push(actor.terminal().connect_lifecycle(move |event| {
                recipient
                    .send(SourceDelivery {
                        slot,
                        target,
                        event: SourceEvent::Lifecycle(event),
                    })
                    .is_ok()
            }));
    }

    /// Fence the old mailbox before later publications can reach the successor.
    /// The fence must not acquire the request registry: publishers lock that
    /// registry before this destination. Failure leaves the destination intact.
    pub(crate) fn handoff<E>(
        &mut self,
        successor: LocalActorRef,
        fence: impl FnOnce(&LocalActorRef) -> Result<(), E>,
    ) -> Result<(), E> {
        let mut recipient = self.recipient.0.lock();
        fence(&recipient)?;
        for command in &mut self.commands {
            command.handoff(successor.identity());
        }
        *recipient = successor;
        Ok(())
    }
}

impl Drop for ActorSourceConnections {
    fn drop(&mut self) {
        let mut state = self.registry.state.lock();
        for record in state.requests.values_mut() {
            record
                .sources
                .retain(|source| !source.recipient.same_connections(&self.recipient));
        }
    }
}

impl RequestRegistry {
    /// Validation, retained-current capture, and connection installation share
    /// the publication lock. Ractor's mailbox owns all accepted deliveries.
    pub(crate) fn attach_sources(
        self: &Arc<Self>,
        owner: ActorRef,
        recipient: LocalActorRef,
        sources: &[(usize, RequestId, RequestSourceKind)],
    ) -> Result<ActorSourceConnections, ReplyError> {
        let mut state = self.state.lock();
        if state.cleaning.contains(&owner) || state.cleaning.contains(&recipient.identity()) {
            return Err(ReplyError::Stale);
        }
        if state.requests.values().any(|record| {
            record
                .sources
                .iter()
                .any(|source| source.recipient.identity() == recipient.identity())
        }) {
            return Err(ReplyError::Stale);
        }
        for (_, request, _) in sources {
            let record = state.requests.get(request).ok_or(ReplyError::Stale)?;
            super::authorize_owner(record, owner)?;
        }
        let recipient = SourceDestination(Arc::new(parking_lot::Mutex::new(recipient)));
        for (slot, request, kind) in sources {
            let Some(record) = state.requests.get_mut(request) else {
                unreachable!("validated source remains under the publication lock");
            };
            let mut connection = RequestSourceConnection {
                recipient: recipient.clone(),
                slot: *slot,
                request: *request,
                kind: *kind,
                closed: false,
            };
            if *kind == RequestSourceKind::Progress {
                if let Some(current) = &record.progress {
                    connection.send(SourceEvent::Progress(current.clone()));
                }
            }
            record.sources.push(connection);
            record.publish_source_closure();
        }
        Ok(ActorSourceConnections {
            registry: Arc::clone(self),
            lifecycle: Vec::new(),
            commands: Vec::new(),
            recipient,
        })
    }
}

impl RequestRecord {
    pub(super) fn publish_source_progress(&mut self) {
        let Some(snapshot) = &self.progress else {
            return;
        };
        for source in &mut self.sources {
            if source.kind == RequestSourceKind::Progress {
                source.send(SourceEvent::Progress(snapshot.clone()));
            }
        }
    }

    pub(super) fn publish_source_closure(&mut self) {
        let progress_closed = self.owner_state != OwnerState::Observing
            || matches!(
                self.target_state,
                TargetState::Closed
                    | TargetState::Settling
                    | TargetState::AcknowledgingCancellation(_)
            );
        let settlement = match &self.owner_state {
            OwnerState::Observing => None,
            OwnerState::Ready => Some(Ok(())),
            OwnerState::Unavailable(failure) => Some(Err(failure.clone())),
            OwnerState::Abandoned => Some(Err(ResponseFailure::Abandoned)),
        };
        for source in &mut self.sources {
            let event = match source.kind {
                RequestSourceKind::Progress if progress_closed => Some(SourceEvent::ProgressClosed),
                RequestSourceKind::Settlement => settlement.clone().map(SourceEvent::Settled),
                _ => None,
            };
            if let Some(event) = event {
                source.send(event);
                source.closed = true;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::KernelMessage;
    use ractor::{Actor, ActorProcessingErr};
    use tokio::sync::mpsc;

    struct Collector;

    impl Actor for Collector {
        type Msg = KernelMessage;
        type State = mpsc::UnboundedSender<SourceDelivery>;
        type Arguments = Self::State;

        async fn pre_start(
            &self,
            _: ractor::ActorRef<Self::Msg>,
            sender: Self::Arguments,
        ) -> Result<Self::State, ActorProcessingErr> {
            Ok(sender)
        }

        async fn handle(
            &self,
            _: ractor::ActorRef<Self::Msg>,
            message: Self::Msg,
            sender: &mut Self::State,
        ) -> Result<(), ActorProcessingErr> {
            if let KernelMessage::Source(delivery) = message {
                sender.send(delivery)?;
            }
            Ok(())
        }
    }

    fn actor(id: u64) -> ActorRef {
        ActorRef::first(crate::ActorId(id))
    }

    async fn receive(events: &mut mpsc::UnboundedReceiver<SourceDelivery>) -> SourceDelivery {
        tokio::time::timeout(std::time::Duration::from_secs(2), events.recv())
            .await
            .unwrap()
            .unwrap()
    }

    #[tokio::test]
    async fn lifecycle_handoff_preserves_connection_without_recapturing_current_state() {
        let registry = Arc::new(RequestRegistry::default());
        let (old_send, mut old_events) = mpsc::unbounded_channel();
        let (old_address, old_task) = Collector::spawn(None, Collector, old_send).await.unwrap();
        let old = LocalActorRef::new(old_address.clone(), crate::RetainedActorExit::new());
        let (new_send, mut new_events) = mpsc::unbounded_channel();
        let (new_address, new_task) = Collector::spawn(None, Collector, new_send).await.unwrap();
        let successor = LocalActorRef::new(new_address.clone(), crate::RetainedActorExit::new());
        let mut sources = registry
            .attach_sources(actor(100), old.clone(), &[])
            .unwrap();
        sources.attach_lifecycle(0, &old);
        assert!(matches!(
            receive(&mut old_events).await.event,
            SourceEvent::Lifecycle(crate::ActorLifecycle::Live)
        ));
        sources.handoff(successor, |_| Ok::<_, ()>(())).unwrap();
        old.terminal().publish_paused("failed".into());
        let paused = receive(&mut new_events).await;
        assert_eq!(paused.target, SourceTarget::Lifecycle(old.identity()));
        assert!(
            matches!(paused.event, SourceEvent::Lifecycle(crate::ActorLifecycle::Paused(detail)) if detail == "failed")
        );
        drop(sources);
        old.terminal()
            .publish(crate::ActorTerminal {
                kind: crate::ActorExitKind::Completed,
                summary: "done".into(),
            })
            .unwrap();
        old_address.stop(None);
        new_address.stop(None);
        old_task.await.unwrap();
        new_task.await.unwrap();
        assert!(old_events.try_recv().is_err());
        assert!(new_events.try_recv().is_err());
    }

    #[tokio::test]
    async fn sources_close_once_in_order_and_detach_without_discarding_sent_events() {
        let registry = Arc::new(RequestRegistry::default());
        let (send, mut events) = mpsc::unbounded_channel();
        let (address, task) = Collector::spawn(None, Collector, send).await.unwrap();
        let recipient = LocalActorRef::new(address.clone(), crate::RetainedActorExit::new());
        let request = registry.reserve(actor(100), actor(101));
        registry
            .mark_queued(actor(100), actor(101), request)
            .unwrap();
        registry.present(actor(101), request).unwrap();
        let sources = registry
            .attach_sources(
                actor(100),
                recipient,
                &[
                    (0, request, RequestSourceKind::Progress),
                    (1, request, RequestSourceKind::Settlement),
                ],
            )
            .unwrap();
        registry.begin_reply(actor(101), request).unwrap();
        registry.finish_reply(request);
        registry.finish_reply(request);
        drop(sources);
        let closed = receive(&mut events).await;
        let settled = receive(&mut events).await;
        assert_eq!((closed.slot, settled.slot), (0, 1));
        assert!(matches!(closed.event, SourceEvent::ProgressClosed));
        assert!(matches!(settled.event, SourceEvent::Settled(Ok(()))));
        address.stop(None);
        task.await.unwrap();
        assert!(events.try_recv().is_err());
        assert!(registry.state.lock().requests[&request].sources.is_empty());
    }

    #[tokio::test]
    async fn replacement_moves_request_and_watch_authority_without_changing_target() {
        let registry = RequestRegistry::default();
        let (send, _) = mpsc::unbounded_channel();
        let (address, task) = Collector::spawn(None, Collector, send).await.unwrap();
        let successor = LocalActorRef::new(address.clone(), crate::RetainedActorExit::new());
        let predecessor = actor(100);
        let target = actor(101);
        let request = registry.reserve(predecessor, target);
        registry.mark_queued(predecessor, target, request).unwrap();
        registry.present(target, request).unwrap();
        let (watch, _) = registry.register_watch(predecessor, vec![request]).unwrap();
        registry.transfer_owner(predecessor, &successor);
        assert_eq!(
            registry.observe_response(predecessor, request),
            Err(ReplyError::Unauthorized)
        );
        assert_eq!(
            registry.observe_watch(predecessor, watch),
            Err(ReplyError::Unauthorized)
        );
        assert_eq!(registry.state.lock().requests[&request].target, target);
        registry.begin_reply(target, request).unwrap();
        let notices = registry.finish_reply(request);
        assert!(notices
            .iter()
            .all(|notice| notice.owner == successor.identity()));
        assert_eq!(
            registry.observe_response(successor.identity(), request),
            Ok(super::super::ResponseObservation::Ready)
        );
        assert!(matches!(
            registry.observe_watch(successor.identity(), watch),
            Ok(super::super::WatchObservation::Ready(_))
        ));
        address.stop(None);
        task.await.unwrap();
    }

    #[tokio::test]
    async fn source_handoff_preserves_connections_and_never_recaptures_settlements() {
        let registry = Arc::new(RequestRegistry::default());
        let (old_send, mut old_events) = mpsc::unbounded_channel();
        let (old_address, old_task) = Collector::spawn(None, Collector, old_send).await.unwrap();
        let old = LocalActorRef::new(old_address.clone(), crate::RetainedActorExit::new());
        let (new_send, mut new_events) = mpsc::unbounded_channel();
        let (new_address, new_task) = Collector::spawn(None, Collector, new_send).await.unwrap();
        let successor = LocalActorRef::new(new_address.clone(), crate::RetainedActorExit::new());
        let requests = (0..3)
            .map(|_| {
                let request = registry.reserve(actor(100), actor(101));
                registry
                    .mark_queued(actor(100), actor(101), request)
                    .unwrap();
                registry.present(actor(101), request).unwrap();
                request
            })
            .collect::<Vec<_>>();
        let bindings = requests
            .iter()
            .enumerate()
            .map(|(slot, request)| (slot, *request, RequestSourceKind::Settlement))
            .collect::<Vec<_>>();
        let mut sources = registry
            .attach_sources(actor(100), old.clone(), &bindings)
            .unwrap();
        let settle = |request| {
            registry.begin_reply(actor(101), request).unwrap();
            registry.finish_reply(request);
        };
        settle(requests[0]);
        assert_eq!(receive(&mut old_events).await.slot, 0);
        assert_eq!(
            sources.handoff(successor.clone(), |_| Err("fence rejected")),
            Err("fence rejected")
        );
        settle(requests[1]);
        assert_eq!(receive(&mut old_events).await.slot, 1);
        let destination = sources.recipient.clone();
        sources
            .handoff(successor.clone(), |recipient| {
                assert_eq!(recipient.identity(), old.identity());
                assert!(destination.0.try_lock().is_none());
                Ok::<_, ()>(())
            })
            .unwrap();
        settle(requests[2]);
        assert_eq!(receive(&mut new_events).await.slot, 2);
        assert_eq!(old.identity().id.0, old_address.get_id().pid());
        {
            let state = registry.state.lock();
            for request in &requests {
                let connections = &state.requests[request].sources;
                assert_eq!(connections.len(), 1);
                assert_eq!(connections[0].recipient.identity(), successor.identity());
            }
        }
        drop(sources);
        assert!(registry
            .state
            .lock()
            .requests
            .values()
            .all(|record| record.sources.is_empty()));
        old_address.stop(None);
        new_address.stop(None);
        old_task.await.unwrap();
        new_task.await.unwrap();
        assert!(old_events.try_recv().is_err());
        assert!(new_events.try_recv().is_err());
    }

    #[tokio::test]
    async fn source_attachment_validates_the_whole_list_before_capturing_anything() {
        let registry = Arc::new(RequestRegistry::default());
        let (send, mut events) = mpsc::unbounded_channel();
        let (address, task) = Collector::spawn(None, Collector, send).await.unwrap();
        let recipient = LocalActorRef::new(address.clone(), crate::RetainedActorExit::new());
        let own = registry.reserve(actor(100), actor(101));
        registry.mark_queued(actor(100), actor(101), own).unwrap();
        registry.present(actor(101), own).unwrap();
        registry.begin_reply(actor(101), own).unwrap();
        registry.finish_reply(own);
        let foreign = registry.reserve(actor(102), actor(101));
        assert!(matches!(
            registry.attach_sources(
                actor(100),
                recipient.clone(),
                &[
                    (0, own, RequestSourceKind::Settlement),
                    (1, foreign, RequestSourceKind::Progress),
                ]
            ),
            Err(ReplyError::Unauthorized)
        ));
        assert!(registry
            .state
            .lock()
            .requests
            .values()
            .all(|record| record.sources.is_empty()));
        let sources = registry
            .attach_sources(
                actor(100),
                recipient,
                &[(0, own, RequestSourceKind::Settlement)],
            )
            .unwrap();
        let retained = receive(&mut events).await;
        assert!(matches!(retained.event, SourceEvent::Settled(Ok(()))));
        drop(sources);
        address.stop(None);
        task.await.unwrap();
        assert!(events.try_recv().is_err());
    }
}
