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

pub(crate) struct SourceBinding {
    pub request: RequestId,
    pub kind: RequestSourceKind,
    pub entry: Arc<tidepool_runtime::session::RootCustody>,
}

#[derive(Clone, Debug)]
pub(crate) enum SourceEvent {
    Progress(ProgressSnapshot),
    ProgressClosed,
    Settled(Result<(), ResponseFailure>),
}

/// A source publication owns its captured value until the receiving mailbox
/// handles or retires it. Later publications cannot replace that value.
#[derive(Debug, Clone)]
pub struct SourceDelivery {
    pub(crate) slot: usize,
    pub(crate) request: RequestId,
    pub(crate) event: SourceEvent,
}

pub(crate) struct RequestSourceConnection {
    recipient: LocalActorRef,
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
                .source(SourceDelivery {
                    slot: self.slot,
                    request: self.request,
                    event,
                })
                .is_err()
        {
            self.closed = true;
        }
    }
}

pub(crate) struct RequestSources {
    registry: Arc<RequestRegistry>,
    recipient: ActorRef,
}

impl Drop for RequestSources {
    fn drop(&mut self) {
        let mut state = self.registry.state.lock();
        for record in state.requests.values_mut() {
            record
                .sources
                .retain(|source| source.recipient.identity() != self.recipient);
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
    ) -> Result<RequestSources, ReplyError> {
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
        Ok(RequestSources {
            registry: Arc::clone(self),
            recipient: recipient.identity(),
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
