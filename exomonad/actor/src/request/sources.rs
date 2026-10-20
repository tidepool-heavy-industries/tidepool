use std::sync::Arc;

use super::{
    OwnerState, ProgressSnapshot, ReplyError, RequestId, RequestRecord, RequestRegistry,
    RequestSuccess, ResponseFailure, TargetState,
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

#[derive(Clone)]
pub(crate) struct CallbackExecution {
    pub context: crate::ActorSessionContext,
    pub snapshot: Option<crate::resident_workbench::ActorRequestSnapshot>,
    pub scope: Option<Arc<tidepool_runtime::session::RuntimeLexicalScopeLease>>,
}

impl CallbackExecution {
    pub(crate) fn source_dependency(
        &self,
    ) -> &crate::resident_workbench::generation_admission::SourceDependency {
        self.snapshot.as_ref().map_or_else(
            || self.context.source_dependency(),
            |snapshot| snapshot.source_dependency(),
        )
    }

    pub(crate) fn admit_activation(
        &self,
    ) -> Result<(), crate::resident_workbench::generation_admission::GenerationAdmissionError> {
        self.with_activation_admission(|| ())
    }

    /// The synchronous callback may only reserve the actual activation. It must
    /// not reenter the source owner or retain its guards across an await.
    pub(crate) fn with_activation_admission<T>(
        &self,
        activate: impl FnOnce() -> T,
    ) -> Result<T, crate::resident_workbench::generation_admission::GenerationAdmissionError> {
        // Captured code requires its final canonical source lease. The
        // pre-placement bootstrap path used by child reservation cannot grant
        // callback execution authority.
        let _source = self.source_dependency().source_lease()?;
        self.source_dependency().with_open_admission(activate)
    }

    /// Move recipient identity and placement while preserving captured source.
    /// Physical custody must be moved separately before executing the callback.
    pub(crate) fn relocated(&self, recipient: crate::ActorSessionContext) -> Self {
        let mut context = self.activation_context();
        context.actor = recipient.actor;
        context.placement = recipient.placement;
        let snapshot = self.snapshot.as_ref().map(|snapshot| {
            crate::resident_workbench::ActorRequestSnapshot::SourceCallback(
                crate::resident_workbench::InstalledToolLease::source_callback_custody(
                    context.actor,
                    snapshot.installation().source().clone(),
                    self.source_dependency().clone(),
                ),
            )
        });
        Self {
            context,
            snapshot,
            scope: self.scope.clone().or_else(|| {
                self.snapshot
                    .as_ref()
                    .and_then(|snapshot| snapshot.generation())
                    .map(|generation| generation.scope().clone())
            }),
        }
    }

    pub(crate) fn activation_context(&self) -> crate::ActorSessionContext {
        let mut context = self.context.clone();
        context.source_dependency = self.source_dependency().clone();
        context
    }
    pub(crate) fn from_context(context: crate::ActorSessionContext) -> Self {
        Self {
            context,
            snapshot: None,
            scope: None,
        }
    }

    /// Replacement transfers the captured code and source, never the original
    /// actor's installation or notebook publication authority.
    pub(crate) fn transferred(
        &self,
        context: crate::ActorSessionContext,
    ) -> Result<Self, crate::mount::ActorCompileViewError> {
        self.admit_activation()?;
        let snapshot = self.snapshot.as_ref().map(|snapshot| {
            crate::resident_workbench::ActorRequestSnapshot::SourceCallback(
                crate::resident_workbench::InstalledToolLease::source_callback_custody(
                    context.actor,
                    snapshot.installation().source().clone(),
                    snapshot.source_dependency().clone(),
                ),
            )
        });
        let mut context = match &snapshot {
            Some(snapshot) => context.with_issued_source(snapshot.installation().source())?,
            None => context,
        };
        context.source_dependency = self.source_dependency().clone();
        let scope = self.scope.clone().or_else(|| {
            self.snapshot
                .as_ref()
                .and_then(|snapshot| snapshot.generation())
                .map(|generation| generation.scope().clone())
        });
        Ok(Self {
            context,
            snapshot,
            scope,
        })
    }
}

#[derive(Clone)]
pub(crate) struct SourceBinding {
    pub target: SourceTarget,
    pub entry: Arc<tidepool_runtime::session::RootCustody>,
    pub execution: CallbackExecution,
}

#[derive(Clone, Debug)]
pub(crate) enum SourceEvent {
    Progress(ProgressSnapshot),
    ProgressClosed,
    ProgressRejected(ReplyError),
    Settled(Result<Arc<crate::owned_result::OwnedResultSnapshot>, ResponseFailure>),
    Lifecycle(crate::ActorLifecycle),
    Command(tidepool_bridge_effects::CommandResult),
}

/// A source publication owns its captured value until the receiving mailbox
/// handles or retires it. Later publications cannot replace that value.
#[derive(Debug, Clone)]
pub struct SourceDelivery {
    pub(crate) slot_delivery: Option<u64>,
    pub(crate) slot: usize,
    pub(crate) target: SourceTarget,
    pub(crate) event: SourceEvent,
}

/// One destination shared by an actor's source connections. The guard owns
/// the set independently of the actor identity currently receiving its events.
#[derive(Clone)]
struct SourceDestination(Arc<crate::kernel::LifecycleHandle>);

impl SourceDestination {
    fn identity(&self) -> ActorRef {
        self.0.source_recipient()
    }

    fn same_connections(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }

    fn send(&self, delivery: SourceDelivery) -> Result<(), crate::KernelCallFailure> {
        self.0.dispatch_source(delivery)
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
                    slot_delivery: None,
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
    lifecycle: Vec<(usize, crate::ActorLifecycleConnection)>,
    commands: Vec<(usize, crate::command_jobs::CommandConnection)>,
    recipient: SourceDestination,
    request_slots: std::collections::HashSet<usize>,
}

impl ActorSourceConnections {
    pub(crate) fn attach_request(
        &mut self,
        slot: usize,
        request: RequestId,
        kind: RequestSourceKind,
        owner: ActorRef,
    ) -> Result<(), ReplyError> {
        let mut state = self.registry.state.lock();
        if state.cleaning.contains(&owner) || state.cleaning.contains(&self.recipient.identity()) {
            return Err(ReplyError::Stale);
        }
        let target = state
            .requests
            .get(&request)
            .ok_or(ReplyError::Stale)?
            .target;
        if state.cleaning.contains(&target) {
            return Err(ReplyError::CancellationRequested);
        }
        let record = state.requests.get_mut(&request).ok_or(ReplyError::Stale)?;
        if record.owner != owner {
            return Err(ReplyError::Stale);
        }
        let mut connection = RequestSourceConnection {
            recipient: self.recipient.clone(),
            slot,
            request,
            kind,
            closed: false,
        };
        if kind == RequestSourceKind::Progress {
            if let Some(current) = record.progress() {
                connection.send(SourceEvent::Progress(current.clone()));
            }
        }
        record.sources.push(connection);
        record.publish_source_closure();
        self.request_slots.insert(slot);
        Ok(())
    }

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
        self.commands.push((
            slot,
            jobs.connect(
                owner,
                observer,
                &uuid::Uuid::from_u128(key).to_string(),
                move |event| {
                    recipient
                        .send(SourceDelivery {
                            slot_delivery: None,
                            slot,
                            target,
                            event: SourceEvent::Command(event),
                        })
                        .is_ok()
                },
            )?,
        ));
        Ok(())
    }

    pub(crate) fn attach_lifecycle(&mut self, slot: usize, actor: &LocalActorRef) {
        let recipient = self.recipient.clone();
        let target = SourceTarget::Lifecycle(actor.identity());
        self.lifecycle.push((
            slot,
            actor.terminal().connect_lifecycle(move |event| {
                recipient
                    .send(SourceDelivery {
                        slot_delivery: None,
                        slot,
                        target,
                        event: SourceEvent::Lifecycle(event),
                    })
                    .is_ok()
            }),
        ));
    }

    /// Move one subscription into its existing stable source slot. The shared
    /// destination still forwards every slot through the original lifecycle.
    pub(crate) fn take_slot(&mut self, slot: usize) -> Self {
        let lifecycle = self
            .lifecycle
            .iter()
            .position(|(id, _)| *id == slot)
            .map(|index| self.lifecycle.remove(index))
            .into_iter()
            .collect();
        let commands = self
            .commands
            .iter()
            .position(|(id, _)| *id == slot)
            .map(|index| self.commands.remove(index))
            .into_iter()
            .collect();
        let request_slots = self
            .request_slots
            .remove(&slot)
            .then_some(slot)
            .into_iter()
            .collect();
        Self {
            registry: self.registry.clone(),
            recipient: self.recipient.clone(),
            lifecycle,
            commands,
            request_slots,
        }
    }

    pub(crate) fn dispatch(
        &self,
        delivery: SourceDelivery,
    ) -> Result<(), crate::KernelCallFailure> {
        self.recipient.send(delivery)
    }

    pub(crate) fn destination(&self) -> crate::LifecycleHandle {
        (*self.recipient.0).clone()
    }

    pub(crate) fn handoff_observers(&mut self, successor: ActorRef) {
        for (_, command) in &mut self.commands {
            command.handoff(successor);
        }
    }

    /// The predecessor selects forwarding before subscriptions change owner.
    /// Publications follow the same ordered path across every handoff.
    pub(crate) async fn handoff(
        &mut self,
        grant: crate::local_actor::run_control::ReplacementIngressGrant,
    ) -> Result<(), crate::KernelCallFailure> {
        self.recipient.0.retarget_sources(grant.clone()).await?;
        for (_, command) in &mut self.commands {
            command.handoff(grant.successor());
        }
        Ok(())
    }
}

impl Drop for ActorSourceConnections {
    fn drop(&mut self) {
        let mut state = self.registry.state.lock();
        for record in state.requests.values_mut() {
            record.sources.retain(|source| {
                !(source.recipient.same_connections(&self.recipient)
                    && self.request_slots.contains(&source.slot))
            });
        }
    }
}

impl RequestRegistry {
    /// A queued delivery becomes an accepted read only here, when the source
    /// actor takes it for mapping. Publication alone does not keep a released
    /// request available. The captured value survives release after this lock.
    pub(crate) fn accept_source_delivery(&self, mut delivery: SourceDelivery) -> SourceDelivery {
        let SourceTarget::Request(request, kind) = delivery.target else {
            return delivery;
        };
        let state = self.state.lock();
        if !state.requests.contains_key(&request) {
            delivery.event = match kind {
                RequestSourceKind::Progress => SourceEvent::ProgressRejected(ReplyError::Stale),
                RequestSourceKind::Settlement => {
                    SourceEvent::Settled(Err(ResponseFailure::Released))
                }
            };
        }
        delivery
    }

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
            if state.cleaning.contains(&record.target) {
                return Err(ReplyError::CancellationRequested);
            }
        }
        let recipient = SourceDestination(Arc::new(recipient.lifecycle().clone()));
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
                if let Some(current) = record.progress() {
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
            request_slots: sources.iter().map(|(slot, _, _)| *slot).collect(),
        })
    }
}

impl RequestRecord {
    pub(super) fn publish_source_release(&mut self) {
        for source in &mut self.sources {
            if source.closed {
                continue;
            }
            let event = match source.kind {
                RequestSourceKind::Progress => SourceEvent::ProgressRejected(ReplyError::Stale),
                RequestSourceKind::Settlement => {
                    SourceEvent::Settled(Err(ResponseFailure::Released))
                }
            };
            source.send(event);
            source.closed = true;
        }
    }

    pub(super) fn publish_source_progress(&mut self) {
        let super::RequestKind::Operation(operation) = &self.kind else {
            return;
        };
        let Some(snapshot) = &operation.progress else {
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
                self.target_state(),
                TargetState::Closed
                    | TargetState::Settling { .. }
                    | TargetState::AcknowledgingCancellation { .. }
            );
        let settlement = match &self.owner_state {
            OwnerState::Observing => None,
            OwnerState::Ready(RequestSuccess::Typed(snapshot)) => Some(Ok(Arc::clone(snapshot))),
            OwnerState::Ready(RequestSuccess::Command(_)) => {
                Some(Err(ResponseFailure::SettlementFailed(
                    "command completion has no typed request result".into(),
                )))
            }
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

    struct CollectorState {
        lifecycle: crate::kernel::LifecycleHandle,
        events: mpsc::UnboundedSender<SourceDelivery>,
    }

    struct Collector;
    impl Actor for Collector {
        type Msg = KernelMessage;
        type State = CollectorState;
        type Arguments = Self::State;
        async fn pre_start(
            &self,
            _: ractor::ActorRef<Self::Msg>,
            state: Self::Arguments,
        ) -> Result<Self::State, ActorProcessingErr> {
            Ok(state)
        }
        async fn handle(
            &self,
            _: ractor::ActorRef<Self::Msg>,
            message: Self::Msg,
            state: &mut Self::State,
        ) -> Result<(), ActorProcessingErr> {
            let (message, lease) = match message {
                KernelMessage::Admitted(envelope) => {
                    let (message, lease) = envelope.into_parts(state.lifecycle.identity())?;
                    (message, Some(lease))
                }
                message => (message, None),
            };
            if let KernelMessage::Source(delivery) = message {
                state.events.send(delivery)?;
            }
            drop(lease);
            Ok(())
        }
        async fn post_stop(
            &self,
            _: ractor::ActorRef<Self::Msg>,
            state: &mut Self::State,
        ) -> Result<(), ActorProcessingErr> {
            state.lifecycle.close().await?;
            state.lifecycle.executor_retired();
            Ok(())
        }
    }

    struct DirectoryFixture {
        directory: crate::LocalActorDirectory,
        task: ractor::concurrency::JoinHandle<()>,
    }
    impl DirectoryFixture {
        async fn start() -> Self {
            let (directory, task) = crate::LocalActorDirectory::start().await.unwrap();
            Self { directory, task }
        }
        async fn spawn(
            &self,
            events: mpsc::UnboundedSender<SourceDelivery>,
            predecessor: Option<ActorRef>,
        ) -> (
            LocalActorRef,
            ractor::ActorRef<KernelMessage>,
            ractor::concurrency::JoinHandle<()>,
        ) {
            let reservation = match predecessor {
                Some(predecessor) => self
                    .directory
                    .reserve_successor(predecessor, crate::Incarnation::FIRST)
                    .await
                    .unwrap(),
                None => self
                    .directory
                    .reserve(crate::Incarnation::FIRST)
                    .await
                    .unwrap(),
            };
            let identity = reservation.identity();
            let lifecycle = reservation.lifecycle().clone();
            let terminal = reservation.terminal().clone();
            let (address, task) = Collector::spawn(
                None,
                Collector,
                CollectorState {
                    lifecycle: lifecycle.clone(),
                    events,
                },
            )
            .await
            .unwrap();
            let actor = LocalActorRef::with_identity_admission(
                address.clone(),
                terminal,
                identity,
                lifecycle.clone(),
            );
            lifecycle.attach_executor(address.clone()).await.unwrap();
            self.directory
                .register_executor(
                    reservation,
                    actor.clone(),
                    std::sync::Weak::new(),
                    crate::local_actor::SpawnOwnership::Independent,
                )
                .await
                .unwrap();
            self.directory.observe_executor_linked(&actor);
            lifecycle.executor_started().await.unwrap();
            (actor, address, task)
        }
    }
    impl Drop for DirectoryFixture {
        fn drop(&mut self) {
            self.task.abort();
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
    async fn route_registration_and_transfer_keep_original_activation_authority() {
        crate::resident_workbench::with_test_compiler_owner(|preparation| async move {
            use crate::request::routes::{RouteRegistrationError, RouteState, WatchRoute};
            use crate::resident_workbench::{ActorRequestSnapshot, InstalledToolLease};
            use tidepool_codegen::{scope::ScopeId, suspension::RealmId};
            use tidepool_runtime::session::SourceAdmissionError;

            let fixture = DirectoryFixture::start().await;
            let (events, _) = mpsc::unbounded_channel();
            let (old, old_address, old_task) = fixture.spawn(events, None).await;
            let (events, _) = mpsc::unbounded_channel();
            let (successor, new_address, new_task) =
                fixture.spawn(events, Some(old.identity())).await;
            let (runner, context, _root, mut callbacks) =
                captured_routes(preparation, old.identity(), 3).await;
            let original = context.source_dependency().source_lease().unwrap();
            let provisional = crate::ActorDescriptor::new("not placed", context.placement)
                .admit_independent_world()
                .session_context(old.identity());
            assert!(matches!(
                CallbackExecution::from_context(provisional).admit_activation(),
                Err(SourceAdmissionError::Unqualified)
            ));
            let registry = RequestRegistry::default();
            let mut register = || {
                let (registration, entry, mut execution) = callbacks.pop().unwrap();
                // This is the same source-only snapshot emitted by callback
                // transfer: no selected generation metadata can recover A.
                execution.snapshot = Some(ActorRequestSnapshot::SourceCallback(
                    InstalledToolLease::source_callback_custody(
                        context.actor,
                        crate::CheckpointSourceLayer::default(),
                        context.source_dependency().clone(),
                    ),
                ));
                execution.scope = Some(original.clone());
                assert!(execution.snapshot.as_ref().unwrap().generation().is_none());
                let plan = registration
                    .dependencies
                    .try_map(|_| Err(ReplyError::Stale))
                    .unwrap();
                (
                    registration.label,
                    plan,
                    WatchRoute::new(old.clone(), entry, execution),
                )
            };
            let (label, plan, route) = register();
            let (running, _) = registry
                .register_watch_route(old.identity(), label, plan, route)
                .unwrap();
            let (label, plan, route) = register();
            let (queued, _) = registry
                .register_watch_route(old.identity(), label, plan, route)
                .unwrap();
            let (label, plan, late) = register();
            drop(register);
            assert_eq!(
                registry.observe_route(old.identity(), queued),
                Ok(RouteState::Running)
            );

            // One callback was admitted before fencing. Its duplicate notice
            // and final settlement must remain neutral afterwards.
            let started = registry
                .take_route(old.identity(), running)
                .unwrap()
                .unwrap();
            assert!(registry
                .take_route(old.identity(), running)
                .unwrap()
                .is_none());
            registry.transfer_owner(old.identity(), &successor);
            assert!(registry
                .take_route(old.identity(), queued)
                .unwrap()
                .is_none());
            let fence = original.source_admission().fence().unwrap();
            assert!(matches!(
                registry.register_watch_route(old.identity(), label, plan, late),
                Err(RouteRegistrationError::Source(
                    SourceAdmissionError::Closed { .. }
                ))
            ));
            assert_eq!(
                registry.list_routes(successor.identity()),
                vec![running, queued]
            );
            assert!(matches!(
                registry.take_route(successor.identity(), queued),
                Err(SourceAdmissionError::Closed { .. })
            ));
            assert!(registry
                .take_route(successor.identity(), queued)
                .unwrap()
                .is_none());
            let _ =
                registry.finish_route(successor.identity(), queued, Err("source closed".into()));
            assert!(matches!(
                registry.observe_route(successor.identity(), queued),
                Ok(RouteState::Failed(_))
            ));
            assert!(registry
                .take_route(successor.identity(), running)
                .unwrap()
                .is_none());
            {
                let state = registry.state.lock();
                for watch in [running, queued] {
                    let execution = state.watches[&watch].route.as_ref().unwrap().execution();
                    assert!(Arc::ptr_eq(
                        &execution.source_dependency().source_lease().unwrap(),
                        &original
                    ));
                }
            }
            // Placement is recipient-owned; the original source/scope survives.
            let recipient = crate::ActorDescriptor::new(
                "recipient",
                crate::ActorPlacement {
                    session: tidepool_repr::SessionId(0xCA08),
                    resource_scope: RealmId::fresh(),
                    lexical_scope: ScopeId::ROOT,
                },
            )
            .admit_independent_world()
            .session_context(successor.identity());
            let relocated = started.execution().relocated(recipient.clone());
            assert_eq!(relocated.context.actor, successor.identity());
            assert_eq!(relocated.context.placement, recipient.placement);
            assert_eq!(relocated.context.source_imports, context.source_imports);
            assert_eq!(relocated.context.source_layer, context.source_layer);
            assert!(relocated.snapshot.as_ref().unwrap().generation().is_none());
            assert!(Arc::ptr_eq(
                &relocated.source_dependency().source_lease().unwrap(),
                &original
            ));
            assert!(Arc::ptr_eq(relocated.scope.as_ref().unwrap(), &original));
            assert!(relocated.admit_activation().is_err());
            assert_eq!(fence.pending_claims(), 0);
            let mut execution_recipient = context.clone();
            execution_recipient.actor = successor.identity();
            let admitted_context = started
                .execution()
                .relocated(execution_recipient)
                .activation_context();
            let mut outcome = runner
                .run_route_entry(admitted_context.clone(), started, running)
                .await
                .unwrap();
            assert!(Arc::ptr_eq(
                &admitted_context.source_dependency().source_lease().unwrap(),
                &original
            ));
            let mut counts = [0usize; 4];
            let mut completed = false;
            // Public pollWatch first owns a transient read, then observes,
            // projects its original decision, and releases that read. None of
            // these operations registers another callable or compiles source.
            for _ in 0..6 {
                use crate::request_effect::{ReplyResult, WatchSubject};
                use crate::resident_workbench::ResidentActorBoundary;
                let boundary = runner
                    .capture_boundary(admitted_context.clone(), outcome, RealmId::ROOT)
                    .await
                    .unwrap();
                eprintln!("admitted fenced route boundary: {}", boundary.operation());
                assert_eq!(fence.pending_claims(), 0);
                outcome = match boundary {
                    ResidentActorBoundary::WatchRegistration(registration)
                        if registration.transient =>
                    {
                        counts[0] += 1;
                        let plan = registration
                            .dependencies
                            .try_map(|(subject, _)| match subject {
                                WatchSubject::Watch(watch) if watch == running => {
                                    Ok(super::super::ReadinessDependency::Watch(watch))
                                }
                                WatchSubject::Watch(other) => panic!(
                                    "route read selected watch {other:?}, expected {running:?}"
                                ),
                                WatchSubject::Request(request) => {
                                    panic!("route read unexpectedly selected request {request:?}")
                                }
                                WatchSubject::Command(job) => {
                                    panic!("route read unexpectedly selected command {job}")
                                }
                            })
                            .unwrap();
                        let transient = registry
                            .register_transient_watch(successor.identity(), plan)
                            .unwrap();
                        runner
                            .resume_value(
                                admitted_context.clone(),
                                registration.continuation,
                                ReplyResult(Ok::<i64, ReplyError>(transient.0.try_into().unwrap())),
                            )
                            .await
                            .unwrap()
                    }
                    ResidentActorBoundary::WatchPoll(poll) => {
                        counts[1] += 1;
                        runner
                            .resume_watch_observation(
                                admitted_context.clone(),
                                poll.continuation,
                                registry.observe_watch(successor.identity(), poll.watch),
                            )
                            .await
                            .unwrap()
                    }
                    ResidentActorBoundary::WatchDecisionPoll {
                        continuation,
                        watch,
                        path,
                    } => {
                        counts[2] += 1;
                        let decision = registry
                            .observe_watch_snapshot_decision(watch, &path)
                            .unwrap();
                        runner
                            .resume_value(admitted_context.clone(), continuation, decision)
                            .await
                            .unwrap()
                    }
                    ResidentActorBoundary::WatchRelease(release) => {
                        counts[3] += 1;
                        registry
                            .release_transient_watch(successor.identity(), release.watch)
                            .unwrap();
                        runner
                            .resume_value(
                                admitted_context.clone(),
                                release.continuation,
                                ReplyResult(Ok::<(), ReplyError>(())),
                            )
                            .await
                            .unwrap()
                    }
                    ResidentActorBoundary::Completed => {
                        assert_eq!(
                            counts,
                            [1, 1, 1, 1],
                            "public route performs one complete read protocol"
                        );
                        completed = true;
                        break;
                    }
                    other => panic!(
                        "unexpected admitted route boundary under the original source fence: {}",
                        other.operation()
                    ),
                };
            }
            assert!(
                completed,
                "admitted route must finish its bounded public read protocol"
            );
            assert!(registry
                .finish_route(successor.identity(), running, Ok(()))
                .is_none());
            assert_eq!(
                registry.observe_route(successor.identity(), running),
                Ok(RouteState::Completed)
            );
            assert_eq!(fence.pending_claims(), 0);
            drop(runner);
            old_address.stop(None);
            new_address.stop(None);
            old_task.await.unwrap();
            new_task.await.unwrap();
        })
        .await;
    }

    async fn captured_routes(
        preparation: crate::CompilerPreparationClient,
        actor: ActorRef,
        count: usize,
    ) -> (
        crate::resident_workbench::ResidentActorRunner<frunk::HNil, tidepool_mcp::CapturedOutput>,
        crate::ActorSessionContext,
        tempfile::TempDir,
        Vec<(
            crate::request_effect::WatchRegistration,
            tidepool_runtime::session::RootCustody,
            CallbackExecution,
        )>,
    ) {
        use crate::resident_workbench::{
            ActorWorkbenchSource, ResidentActorBoundary, ResidentActorRunner,
        };
        use tidepool_codegen::suspension::RealmId;
        use tidepool_runtime::session::turn::{run_turn, TurnRequest, TurnResult};
        use tidepool_runtime::session::{
            insert_preamble_imports, resident_workbench_templates, ModuleEnv, ResidentSession,
            SessionLib,
        };
        tidepool_testing::eval_harness::require_extract();
        let root = tempfile::tempdir().unwrap();
        let surface = tidepool_testing::effect_surface::TestEffectSurface::minimal(&[]).unwrap();
        let preamble = insert_preamble_imports(
            surface.preamble(),
            "qualified Tidepool.Agent.Watch as RouteW",
        );
        let templates = resident_workbench_templates(
            &preamble,
            "'[RouteW.Watches]",
            "qualified Tidepool.Agent.Watch as RouteW",
        );
        let mut include = surface.include_paths().to_vec();
        include.push(std::path::PathBuf::from(
            std::env::var_os("TIDEPOOL_HASKELL_ACTORS_DIR")
                .expect("declared actor sources are required"),
        ));
        let compiled = tidepool_testing::with_settlement(|settlement| {
            run_turn(
                TurnRequest {
                    exact_context: None,
                    session_id: None,
                    turn_text: include_str!("fixtures/watch-route.hs"),
                    templates: &templates,
                    include: &include
                        .iter()
                        .map(|path| path.as_path())
                        .collect::<Vec<_>>(),
                    session_root: root.path(),
                    inject_modules: &[],
                    gen: 1,
                    verdict: None,
                    target: None,
                    retained_imports: &[],
                },
                settlement,
            )
        })
        .unwrap();
        let compiled = match compiled {
            TurnResult::Bind { compiled, .. } => compiled,
            _ => panic!("route fixture must compile a binding"),
        };
        let session_id = tidepool_repr::SessionId(0xCA07);
        let library = SessionLib::open(session_id, root.path(), ModuleEnv::standalone_default())
            .unwrap()
            .with_validation_include(include.clone());
        let mut session = ResidentSession::unbootstrapped(
            frunk::HNil,
            tidepool_mcp::CapturedOutput::new(),
            tidepool_runtime::DEFAULT_NURSERY_SIZE,
            Some(library),
        );
        let scope = session.mint_isolated_scope();
        let mut context = crate::ActorDescriptor::new(
            "route-source",
            crate::ActorPlacement {
                session: session_id,
                resource_scope: RealmId::ROOT,
                lexical_scope: scope,
            },
        )
        .admit_independent_world()
        .session_context(actor);
        context.effect_policy = tidepool_effect::EffectRunPolicy::SuspendAll;
        context.haskell_effects_alias = "'[RouteW.Watches]".into();
        context
            .source_dependency()
            .adopt_final_scope(&mut session, scope)
            .unwrap();
        session
            .set_actor_execution(
                context.run_context(),
                context.effect_policy,
                context.live_payload,
            )
            .unwrap();
        let owner = crate::capabilities::CompilerOwnerScope::current().unwrap();
        let mut outcomes = Vec::new();
        for _ in 0..count {
            let outcome = crate::termination::CompilerWorkTicket::run_source_transaction(
                &mut session,
                owner.admit_continuation().unwrap(),
                context.source_dependency(),
                |session, settlement| {
                    session.run_with_sites("route-capture", compiled.code(), settlement)
                },
            )
            .unwrap()
            .unwrap();
            outcomes.push(outcome);
        }
        let machines = Arc::new(crate::ActorMachineRegistry::new());
        machines.insert_idle(session_id, Box::new(session));
        let runner = ResidentActorRunner::new(
            machines,
            ActorWorkbenchSource::new(preamble, include).bind_preparation(preparation),
        );
        let mut callbacks = Vec::new();
        for outcome in outcomes {
            let ResidentActorBoundary::RouteRegistration {
                registration,
                entry,
                execution,
            } = runner
                .capture_boundary(context.clone(), outcome, RealmId::ROOT)
                .await
                .unwrap()
            else {
                panic!("actual route effect must capture its root and execution together");
            };
            callbacks.push((registration, entry, execution));
        }
        (runner, context, root, callbacks)
    }

    #[tokio::test]
    async fn lifecycle_handoff_preserves_connection_without_recapturing_current_state() {
        let fixture = DirectoryFixture::start().await;
        let registry = Arc::new(RequestRegistry::default());
        let (old_send, mut old_events) = mpsc::unbounded_channel();
        let (old, old_address, old_task) = fixture.spawn(old_send, None).await;
        let (new_send, mut new_events) = mpsc::unbounded_channel();
        let (successor, new_address, new_task) =
            fixture.spawn(new_send, Some(old.identity())).await;
        let mut sources = registry
            .attach_sources(actor(100), old.clone(), &[])
            .unwrap();
        sources.attach_lifecycle(0, &old);
        assert!(matches!(
            receive(&mut old_events).await.event,
            SourceEvent::Lifecycle(crate::ActorLifecycle::Live)
        ));
        let grant = fixture
            .directory
            .replacement_ingress_grant(old.identity(), successor.identity())
            .await
            .unwrap();
        old.lifecycle()
            .begin_replacement(grant.clone())
            .await
            .unwrap();
        sources.handoff(grant).await.unwrap();
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
                diagnostic: None,
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
        let fixture = DirectoryFixture::start().await;
        let registry = Arc::new(RequestRegistry::default());
        let (send, mut events) = mpsc::unbounded_channel();
        let (recipient, address, task) = fixture.spawn(send, None).await;
        let request = registry.reserve_native(actor(100), actor(101));
        registry
            .mark_queued(actor(100), actor(101), request)
            .unwrap();
        registry.present(actor(101), request).unwrap();
        let sources = registry
            .attach_sources(
                actor(100),
                recipient.clone(),
                &[
                    (0, request, RequestSourceKind::Progress),
                    (1, request, RequestSourceKind::Settlement),
                ],
            )
            .unwrap();
        let mut reply_claim_request = Some(registry.begin_reply(actor(101), request).unwrap());
        crate::request::test_support::complete_optional_reply(
            &registry,
            &mut reply_claim_request,
            None,
        );
        crate::request::test_support::complete_optional_reply(
            &registry,
            &mut reply_claim_request,
            None,
        );
        drop(sources);
        let closed = receive(&mut events).await;
        let settled = receive(&mut events).await;
        assert_eq!((closed.slot, settled.slot), (0, 1));
        assert!(matches!(closed.event, SourceEvent::ProgressClosed));
        assert!(
            matches!(&settled.event, SourceEvent::Settled(Ok(root)) if crate::request::test_support::force(root) == 41)
        );
        address.stop(None);
        task.await.unwrap();
        assert!(events.try_recv().is_err());
        assert!(registry.state.lock().requests[&request].sources.is_empty());
    }

    #[tokio::test]
    async fn queued_source_read_after_release_reports_unavailable() {
        let fixture = DirectoryFixture::start().await;
        let registry = Arc::new(RequestRegistry::default());
        let (send, mut events) = mpsc::unbounded_channel();
        let (recipient, address, task) = fixture.spawn(send, None).await;
        let owner = actor(100);
        let target = actor(101);
        let request = registry.reserve_native(owner, target);
        registry.mark_queued(owner, target, request).unwrap();
        registry.present(target, request).unwrap();
        let _sources = registry
            .attach_sources(
                owner,
                recipient.clone(),
                &[
                    (0, request, RequestSourceKind::Progress),
                    (1, request, RequestSourceKind::Settlement),
                ],
            )
            .unwrap();
        let mut reply_claim_request = Some(registry.begin_reply(target, request).unwrap());
        crate::request::test_support::complete_optional_reply(
            &registry,
            &mut reply_claim_request,
            None,
        );
        let queued_progress = receive(&mut events).await;
        let queued_settlement = receive(&mut events).await;
        assert!(matches!(queued_progress.event, SourceEvent::ProgressClosed));
        assert!(
            matches!(&queued_settlement.event, SourceEvent::Settled(Ok(root)) if crate::request::test_support::force(root) == 41)
        );
        assert!(matches!(&registry
                .accept_source_delivery(queued_settlement.clone())
                .event, SourceEvent::Settled(Ok(root)) if crate::request::test_support::force(root) == 41));

        registry.forget_response(owner, request).unwrap();
        assert!(matches!(
            registry.accept_source_delivery(queued_progress).event,
            SourceEvent::ProgressRejected(ReplyError::Stale)
        ));
        assert!(matches!(
            registry.accept_source_delivery(queued_settlement).event,
            SourceEvent::Settled(Err(ResponseFailure::Released))
        ));
        address.stop(None);
        task.await.unwrap();
    }

    #[tokio::test]
    async fn abort_unsubmitted_wakes_open_sources_without_replaying_closed_settlements() {
        let fixture = DirectoryFixture::start().await;
        let registry = Arc::new(RequestRegistry::default());
        let (send, mut events) = mpsc::unbounded_channel();
        let (recipient, address, task) = fixture.spawn(send, None).await;
        let owner = actor(100);
        let target = actor(101);
        let operation = crate::request::RequestReservationOwner::Route(crate::WatchId(1));
        let abandoned = registry.reserve_for_operation(
            owner,
            target,
            "request".into(),
            true,
            Some(operation.clone()),
        );
        let _sources = registry
            .attach_sources(
                owner,
                recipient.clone(),
                &[
                    (0, abandoned, RequestSourceKind::Progress),
                    (1, abandoned, RequestSourceKind::Settlement),
                ],
            )
            .unwrap();
        assert_eq!(
            registry.abort_unsubmitted(owner, &operation).0,
            vec![abandoned]
        );
        let progress = receive(&mut events).await;
        let settlement = receive(&mut events).await;
        assert_eq!((progress.slot, settlement.slot), (0, 1));
        assert!(matches!(
            registry.accept_source_delivery(progress).event,
            SourceEvent::ProgressRejected(ReplyError::Stale)
        ));
        assert!(matches!(
            registry.accept_source_delivery(settlement).event,
            SourceEvent::Settled(Err(ResponseFailure::Released))
        ));

        let completed = registry.reserve_native(owner, target);
        registry.mark_queued(owner, target, completed).unwrap();
        registry.present(target, completed).unwrap();
        let _sources = registry
            .attach_sources(
                owner,
                recipient.clone(),
                &[(2, completed, RequestSourceKind::Settlement)],
            )
            .unwrap();
        let mut reply_claim_completed = Some(registry.begin_reply(target, completed).unwrap());
        crate::request::test_support::complete_optional_reply(
            &registry,
            &mut reply_claim_completed,
            None,
        );
        assert!(
            matches!(&receive(&mut events).await.event, SourceEvent::Settled(Ok(root)) if crate::request::test_support::force(root) == 41)
        );
        registry.forget_response(owner, completed).unwrap();
        address.stop(None);
        task.await.unwrap();
        assert!(events.try_recv().is_err());
    }

    #[tokio::test]
    async fn replacement_moves_request_and_watch_authority_without_changing_target() {
        let fixture = DirectoryFixture::start().await;
        let registry = RequestRegistry::default();
        let (send, _) = mpsc::unbounded_channel();
        let (successor, address, task) = fixture.spawn(send, None).await;
        let predecessor = actor(100);
        let target = actor(101);
        let request = registry.reserve_native(predecessor, target);
        registry.mark_queued(predecessor, target, request).unwrap();
        registry.present(target, request).unwrap();
        let (watch, _) = registry.register_watch(predecessor, vec![request]).unwrap();
        registry.transfer_owner(predecessor, &successor);
        assert!(matches!(
            registry.observe_response(predecessor, request),
            Ok(super::super::ResponseObservation::Pending(_))
        ));
        assert!(matches!(
            registry.observe_watch(predecessor, watch),
            Ok(super::super::WatchObservation::Pending(_))
        ));
        assert_eq!(registry.state.lock().requests[&request].target, target);
        let mut reply_claim_request = Some(registry.begin_reply(target, request).unwrap());
        let notices = crate::request::test_support::complete_optional_reply(
            &registry,
            &mut reply_claim_request,
            None,
        );
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
        let fixture = DirectoryFixture::start().await;
        let registry = Arc::new(RequestRegistry::default());
        let (old_send, mut old_events) = mpsc::unbounded_channel();
        let (old, old_address, old_task) = fixture.spawn(old_send, None).await;
        let (new_send, mut new_events) = mpsc::unbounded_channel();
        let (successor, new_address, new_task) =
            fixture.spawn(new_send, Some(old.identity())).await;
        let requests = (0..3)
            .map(|_| {
                let request = registry.reserve_native(actor(100), actor(101));
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
            let mut reply_claim_request = Some(registry.begin_reply(actor(101), request).unwrap());
            crate::request::test_support::complete_optional_reply(
                &registry,
                &mut reply_claim_request,
                None,
            );
        };
        settle(requests[0]);
        assert_eq!(receive(&mut old_events).await.slot, 0);
        let grant = fixture
            .directory
            .replacement_ingress_grant(old.identity(), successor.identity())
            .await
            .unwrap();
        assert!(
            matches!(sources.handoff(grant.clone()).await, Err(crate::KernelCallFailure::TargetUnavailable(actor)) if actor == old.identity())
        );
        settle(requests[1]);
        assert_eq!(receive(&mut old_events).await.slot, 1);
        old.lifecycle()
            .begin_replacement(grant.clone())
            .await
            .unwrap();
        sources.handoff(grant).await.unwrap();
        settle(requests[2]);
        assert_eq!(receive(&mut new_events).await.slot, 2);
        assert_ne!(old.identity(), successor.identity());
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
    async fn repeated_handoff_keeps_original_source_path_and_ordered_settlements() {
        eprintln!("source-order stage: start directory");
        let fixture = DirectoryFixture::start().await;
        eprintln!("source-order stage: directory started");
        let registry = Arc::new(RequestRegistry::default());
        let (first_send, mut first_events) = mpsc::unbounded_channel();
        let (first, first_address, first_task) = fixture.spawn(first_send, None).await;
        eprintln!("source-order stage: A registered {:?}", first.identity());
        let (second_send, mut second_events) = mpsc::unbounded_channel();
        let (second, second_address, second_task) =
            fixture.spawn(second_send, Some(first.identity())).await;
        eprintln!("source-order stage: B registered {:?}", second.identity());
        let (third_send, mut third_events) = mpsc::unbounded_channel();
        let (third, third_address, third_task) =
            fixture.spawn(third_send, Some(second.identity())).await;
        eprintln!("source-order stage: C registered {:?}", third.identity());
        let fixture_started = std::time::Instant::now();
        eprintln!("source-order stage: compile real request wrappers");
        let requests = (0..4)
            .map(|_| {
                let request = registry.reserve_native(actor(100), actor(101));
                registry
                    .mark_queued(actor(100), actor(101), request)
                    .unwrap();
                registry.present(actor(101), request).unwrap();
                request
            })
            .collect::<Vec<_>>();
        eprintln!(
            "source-order stage: request wrappers prepared; setup_elapsed={:?}",
            fixture_started.elapsed()
        );
        let protocol_started = std::time::Instant::now();
        let bindings = requests
            .iter()
            .enumerate()
            .map(|(slot, request)| (slot, *request, RequestSourceKind::Settlement))
            .collect::<Vec<_>>();
        let mut sources = registry
            .attach_sources(actor(100), first.clone(), &bindings)
            .unwrap();
        let original_destination = sources.recipient.clone();
        let settle = |request| {
            let mut claim = Some(registry.begin_reply(actor(101), request).unwrap());
            crate::request::test_support::complete_optional_reply(&registry, &mut claim, None);
        };
        eprintln!("source-order stage: settle first publication");
        settle(requests[0]);
        eprintln!("source-order stage: first publication queued");
        assert_eq!(receive(&mut first_events).await.slot, 0);
        for (predecessor, successor) in [(&first, &second), (&second, &third)] {
            eprintln!(
                "source-order stage: acquire {:?}->{:?} grant",
                predecessor.identity(),
                successor.identity()
            );
            let grant = fixture
                .directory
                .replacement_ingress_grant(predecessor.identity(), successor.identity())
                .await
                .unwrap();
            eprintln!("source-order stage: grant issued");
            predecessor
                .lifecycle()
                .begin_replacement(grant.clone())
                .await
                .unwrap();
            eprintln!("source-order stage: predecessor closed");
            sources.handoff(grant).await.unwrap();
            eprintln!("source-order stage: source handoff acknowledged");
        }
        assert!(sources.recipient.same_connections(&original_destination));
        assert_eq!(sources.recipient.0.identity(), first.identity());
        assert_eq!(sources.recipient.identity(), third.identity());
        for request in &requests[1..] {
            settle(*request);
        }
        eprintln!("source-order stage: three publications queued");
        for slot in 1..4 {
            let delivery = receive(&mut third_events).await;
            assert_eq!(delivery.slot, slot);
            assert!(
                matches!(&delivery.event, SourceEvent::Settled(Ok(root)) if crate::request::test_support::force(root) == 41)
            );
        }
        assert!(first_events.try_recv().is_err());
        assert!(second_events.try_recv().is_err());
        let dropped = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let request = crate::MailboxValue::probe(tidepool_repr::SessionId(7), Arc::clone(&dropped));
        assert!(matches!(
            first.cast(actor(100), request).await,
            Err(crate::KernelCallFailure::MailboxClosed(identity)) if identity == first.identity()
        ));
        assert_eq!(dropped.load(std::sync::atomic::Ordering::SeqCst), 1);
        drop(sources);
        first_address.stop(None);
        second_address.stop(None);
        third_address.stop(None);
        eprintln!("source-order stage: joining collectors");
        first_task.await.unwrap();
        second_task.await.unwrap();
        third_task.await.unwrap();
        eprintln!(
            "source-order stage: collectors joined; protocol_elapsed={:?}",
            protocol_started.elapsed()
        );
    }

    #[tokio::test]
    async fn source_attachment_validates_the_whole_list_before_capturing_anything() {
        let fixture = DirectoryFixture::start().await;
        let registry = Arc::new(RequestRegistry::default());
        let (send, mut events) = mpsc::unbounded_channel();
        let (recipient, address, task) = fixture.spawn(send, None).await;
        let own = registry.reserve_native(actor(100), actor(101));
        registry.mark_queued(actor(100), actor(101), own).unwrap();
        registry.present(actor(101), own).unwrap();
        let mut reply_claim_own = Some(registry.begin_reply(actor(101), own).unwrap());
        crate::request::test_support::complete_optional_reply(
            &registry,
            &mut reply_claim_own,
            None,
        );
        let missing = RequestId(u64::MAX);
        assert!(matches!(
            registry.attach_sources(
                actor(100),
                recipient.clone(),
                &[
                    (0, own, RequestSourceKind::Settlement),
                    (1, missing, RequestSourceKind::Progress),
                ]
            ),
            Err(ReplyError::Stale)
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
                recipient.clone(),
                &[(0, own, RequestSourceKind::Settlement)],
            )
            .unwrap();
        let retained = receive(&mut events).await;
        assert!(
            matches!(&retained.event, SourceEvent::Settled(Ok(root)) if crate::request::test_support::force(root) == 41)
        );
        drop(sources);
        address.stop(None);
        task.await.unwrap();
        assert!(events.try_recv().is_err());
    }

    #[tokio::test]
    async fn dynamic_attachment_reports_partial_admission_and_keeps_retained_event_once() {
        let fixture = DirectoryFixture::start().await;
        let registry = Arc::new(RequestRegistry::default());
        let (send, mut events) = mpsc::unbounded_channel();
        let (recipient, address, task) = fixture.spawn(send, None).await;
        let owner = actor(100);
        let target = actor(101);
        let settled = registry.reserve_native(owner, target);
        registry.mark_queued(owner, target, settled).unwrap();
        registry.present(target, settled).unwrap();
        let mut reply_claim_settled = Some(registry.begin_reply(target, settled).unwrap());
        crate::request::test_support::complete_optional_reply(
            &registry,
            &mut reply_claim_settled,
            None,
        );
        let mut sources = registry
            .attach_sources(owner, recipient.clone(), &[])
            .unwrap();
        sources
            .attach_request(0, settled, RequestSourceKind::Settlement, owner)
            .unwrap();
        assert!(matches!(
            sources.attach_request(1, RequestId(u64::MAX), RequestSourceKind::Progress, owner),
            Err(ReplyError::Stale)
        ));
        let first = receive(&mut events).await;
        assert_eq!(first.slot, 0);
        assert!(
            matches!(&first.event, SourceEvent::Settled(Ok(root)) if crate::request::test_support::force(root) == 41)
        );
        assert!(events.try_recv().is_err());
        assert_eq!(registry.state.lock().requests[&settled].sources.len(), 1);
        drop(sources);
        assert!(registry.state.lock().requests[&settled].sources.is_empty());
        address.stop(None);
        task.await.unwrap();
    }

    #[tokio::test]
    async fn dynamic_request_source_refuses_a_different_owner_without_delivery() {
        let fixture = DirectoryFixture::start().await;
        let registry = Arc::new(RequestRegistry::default());
        let (send, mut events) = mpsc::unbounded_channel();
        let (recipient, address, task) = fixture.spawn(send, None).await;
        let owner = actor(100);
        let impostor = actor(102);
        let request = registry.reserve(owner, actor(101));
        let mut sources = registry.attach_sources(impostor, recipient, &[]).unwrap();
        assert!(matches!(
            sources.attach_request(0, request, RequestSourceKind::Settlement, impostor),
            Err(ReplyError::Stale)
        ));
        assert!(events.try_recv().is_err());
        assert!(registry.state.lock().requests[&request].sources.is_empty());
        drop(sources);
        address.stop(None);
        task.await.unwrap();
    }

    #[tokio::test]
    async fn dynamic_request_connection_survives_pause_and_detaches_on_drop() {
        let fixture = DirectoryFixture::start().await;
        let registry = Arc::new(RequestRegistry::default());
        let (send, mut events) = mpsc::unbounded_channel();
        let (recipient, address, task) = fixture.spawn(send, None).await;
        let owner = actor(100);
        let target = actor(101);
        let request = registry.reserve_native(owner, target);
        registry.mark_queued(owner, target, request).unwrap();
        registry.present(target, request).unwrap();
        let mut sources = registry
            .attach_sources(owner, recipient.clone(), &[])
            .unwrap();

        // Attachment before publication receives the future settlement. A
        // paused owner retains its connection until explicit retirement.
        sources
            .attach_request(0, request, RequestSourceKind::Settlement, owner)
            .unwrap();
        recipient.terminal().publish_paused("handler failed".into());
        assert_eq!(registry.state.lock().requests[&request].sources.len(), 1);
        let mut reply_claim_request = Some(registry.begin_reply(target, request).unwrap());
        crate::request::test_support::complete_optional_reply(
            &registry,
            &mut reply_claim_request,
            None,
        );
        let first = receive(&mut events).await;
        assert_eq!(first.slot, 0);
        assert!(
            matches!(&first.event, SourceEvent::Settled(Ok(root)) if crate::request::test_support::force(root) == 41)
        );

        // Attachment after publication captures the retained terminal event
        // once under the same request lock.
        sources
            .attach_request(1, request, RequestSourceKind::Settlement, owner)
            .unwrap();
        let retained = receive(&mut events).await;
        assert_eq!(retained.slot, 1);
        assert!(
            matches!(&retained.event, SourceEvent::Settled(Ok(root)) if crate::request::test_support::force(root) == 41)
        );
        assert!(events.try_recv().is_err());
        drop(sources);
        assert!(registry.state.lock().requests[&request].sources.is_empty());
        address.stop(None);
        task.await.unwrap();
    }

    #[tokio::test]
    async fn dynamic_connection_handoff_keeps_queued_event_without_replay() {
        let fixture = DirectoryFixture::start().await;
        let registry = Arc::new(RequestRegistry::default());
        let (old_send, mut old_events) = mpsc::unbounded_channel();
        let (old, old_address, old_task) = fixture.spawn(old_send, None).await;
        let (new_send, mut new_events) = mpsc::unbounded_channel();
        let (successor, new_address, new_task) =
            fixture.spawn(new_send, Some(old.identity())).await;
        let owner = actor(100);
        let target = actor(101);
        let before = registry.reserve_native(owner, target);
        let after = registry.reserve_native(owner, target);
        for request in [before, after] {
            registry.mark_queued(owner, target, request).unwrap();
            registry.present(target, request).unwrap();
        }
        let mut sources = registry.attach_sources(owner, old.clone(), &[]).unwrap();
        sources
            .attach_request(0, before, RequestSourceKind::Settlement, owner)
            .unwrap();
        sources
            .attach_request(1, after, RequestSourceKind::Settlement, owner)
            .unwrap();
        let mut reply_claim_before = Some(registry.begin_reply(target, before).unwrap());
        crate::request::test_support::complete_optional_reply(
            &registry,
            &mut reply_claim_before,
            None,
        );
        let grant = fixture
            .directory
            .replacement_ingress_grant(old.identity(), successor.identity())
            .await
            .unwrap();
        old.lifecycle()
            .begin_replacement(grant.clone())
            .await
            .unwrap();
        sources.handoff(grant).await.unwrap();
        let queued = receive(&mut old_events).await;
        assert_eq!(queued.slot, 0);
        assert!(
            matches!(&queued.event, SourceEvent::Settled(Ok(root)) if crate::request::test_support::force(root) == 41)
        );
        let mut reply_claim_after = Some(registry.begin_reply(target, after).unwrap());
        crate::request::test_support::complete_optional_reply(
            &registry,
            &mut reply_claim_after,
            None,
        );
        let subsequent = receive(&mut new_events).await;
        assert_eq!(subsequent.slot, 1);
        assert!(
            matches!(&subsequent.event, SourceEvent::Settled(Ok(root)) if crate::request::test_support::force(root) == 41)
        );
        assert!(old_events.try_recv().is_err());
        assert!(new_events.try_recv().is_err());
        drop(sources);
        old_address.stop(None);
        new_address.stop(None);
        old_task.await.unwrap();
        new_task.await.unwrap();
    }
    #[tokio::test]
    async fn closed_predecessor_refuses_source_handoff_to_issued_successor() {
        let fixture = DirectoryFixture::start().await;
        let registry = Arc::new(RequestRegistry::default());
        let (old_send, _) = mpsc::unbounded_channel();
        let (old, old_address, old_task) = fixture.spawn(old_send, None).await;
        let (new_send, _) = mpsc::unbounded_channel();
        let (successor, new_address, new_task) =
            fixture.spawn(new_send, Some(old.identity())).await;
        let grant = fixture
            .directory
            .replacement_ingress_grant(old.identity(), successor.identity())
            .await
            .unwrap();
        let mut sources = registry
            .attach_sources(actor(100), old.clone(), &[])
            .unwrap();
        old.lifecycle().close().await.unwrap();
        assert!(
            matches!(old.lifecycle().begin_replacement(grant.clone()).await,
            Err(crate::KernelCallFailure::MailboxClosed(actor)) if actor == old.identity())
        );
        assert!(matches!(sources.handoff(grant).await,
            Err(crate::KernelCallFailure::TargetUnavailable(actor)) if actor == old.identity()));
        assert_eq!(sources.recipient.identity(), old.identity());
        old_address.stop(None);
        new_address.stop(None);
        old_task.await.unwrap();
        new_task.await.unwrap();
    }
}
