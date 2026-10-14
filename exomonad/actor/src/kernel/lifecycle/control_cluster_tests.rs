use super::*;

/// A retained source capture with a real native value, issued through the same
/// source boundary as a bootstrap installation's compilation authority.
struct NativeSourceCapture {
    _value: Arc<crate::owned_result::OwnedResultSnapshot>,
}

impl crate::RetainedSourceLayer for NativeSourceCapture {
    fn identities(&self) -> &[String] {
        &[]
    }

    fn include_paths(&self) -> &[std::path::PathBuf] {
        &[]
    }

    fn catalog_selection(&self) -> tidepool_toolchain::toolchain::CatalogSelection {
        tidepool_toolchain::toolchain::CatalogSelection::Acquired(None)
    }
}

#[tokio::test]
async fn confirmed_resident_retirement_releases_current_source_while_observers_survive() {
    resident_retirement_source_lifetime(RetirementSourceCase::Observation).await;
}

#[tokio::test]
async fn issued_request_keeps_native_source_after_confirmed_resident_retirement() {
    resident_retirement_source_lifetime(RetirementSourceCase::IssuedRequest).await;
}

#[tokio::test]
async fn failed_resident_retirement_preserves_current_source_and_failure_evidence() {
    resident_retirement_source_lifetime(RetirementSourceCase::Failure).await;
}

enum RetirementSourceCase {
    Observation,
    IssuedRequest,
    Failure,
}

async fn resident_retirement_source_lifetime(case: RetirementSourceCase) {
    use crate::request::{test_support, RequestRegistry};

    let baseline = test_support::outstanding_custody();
    let registry = RequestRegistry::default();
    let issuer = ActorRef::first(crate::ActorId(9200));
    let target = ActorRef::first(crate::ActorId(9201));
    let request = registry.reserve_native(issuer, target);
    registry.mark_queued(issuer, target, request).unwrap();
    registry.present(target, request).unwrap();
    let claim = registry.begin_reply(target, request).unwrap();
    let value = test_support::snapshot(&claim);
    let native_value = Arc::downgrade(&value);
    let session = value.session();
    drop(claim);
    drop(registry);
    let source =
        crate::SourceLayerIssuer::default().issue(Arc::new(NativeSourceCapture { _value: value }));

    let (owner, executor) = opened().await;
    let placement = crate::ActorPlacement {
        session,
        resource_scope: tidepool_codegen::suspension::RealmId::ROOT,
        lexical_scope: tidepool_codegen::scope::ScopeId::ROOT,
    };
    let descriptor = crate::ActorDescriptor::new("retained installation", placement);
    let context = descriptor.session_context(owner.identity());
    let runner = crate::ResidentActorRunner::<frunk::HNil, tidepool_mcp::CapturedOutput>::new(
        Arc::new(crate::ActorMachineRegistry::new()),
        crate::ActorWorkbenchSource::new("", Vec::new()),
    );
    owner
        .configure_resident(
            context,
            descriptor,
            None,
            Arc::new(runner),
            None,
            Some("retained caller seed".into()),
        )
        .await
        .unwrap();
    let installed = Arc::new(crate::resident_workbench::InstalledToolsState::default());
    let lease = crate::InstalledToolLease::new(owner.identity(), source, None);
    installed.publish(lease.clone());
    owner.bootstrap_policy_installed(Some(lease));
    let barrier = owner.transaction().await.unwrap();
    drop(barrier);
    let mut observation = owner.observations_for_test();
    assert!(observation
        .borrow()
        .resident
        .as_ref()
        .unwrap()
        .installed_policy
        .is_some());
    let endpoint = crate::ResidentInteractivePolicy::local_with_installation(
        // The fixture executor deliberately handles no tool operations. Its
        // native source witness is forced through the existing native owner.
        crate::LocalActorRef::with_identity_admission(
            executor.clone(),
            owner.retained_terminal().clone(),
            owner.identity(),
            owner.clone(),
        ),
        Vec::new(),
        installed.clone(),
    );
    let issued = matches!(case, RetirementSourceCase::IssuedRequest).then(|| {
        use crate::ResidentToolEndpoint;
        endpoint.snapshot_for_request().unwrap()
    });
    drop(endpoint);
    // Production stopped() clears this shared selection before sending the
    // lifecycle retirement event; only issued requests may retain the lease.
    installed.clear();
    drop(installed);
    if matches!(case, RetirementSourceCase::Failure) {
        owner
            .fault_next_resident_worker_for_test(resident::ResidentWorkerFault::Retirement)
            .await
            .unwrap();
    }
    owner.close().await.unwrap();
    executor.stop(None);
    executor
        .get_cell()
        .wait(Some(std::time::Duration::from_secs(5)))
        .await
        .unwrap();
    owner.retire_resident(crate::ActorTerminal {
        kind: crate::ActorExitKind::Completed,
        summary: "confirmed selected-installation retirement".into(),
        diagnostic: None,
    });
    owner.executor_retired();
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            let settled = {
                let observed = observation.borrow_and_update();
                match case {
                    RetirementSourceCase::Failure => matches!(
                        observed.resident.as_ref().unwrap().settlement,
                        resident::ResidentSettlement::NeedsAttention(_)
                    ),
                    _ => observed.phase == LifecyclePhase::Retired,
                }
            };
            if settled {
                break;
            }
            observation.changed().await.unwrap();
        }
    })
    .await
    .unwrap();
    {
        let observed = observation.borrow();
        let resident = observed.resident.as_ref().unwrap();
        assert_eq!(
            resident.installed_policy.is_some(),
            matches!(case, RetirementSourceCase::Failure)
        );
        assert_eq!(resident.descriptor.display_label(), "retained installation");
        assert_eq!(
            resident.fresh_context_seed.as_deref(),
            Some("retained caller seed")
        );
    }
    if let Some(issued) = issued {
        let retained = native_value
            .upgrade()
            .expect("issued request retains its exact source");
        assert_eq!(test_support::force(&retained), 41);
        drop(retained);
        drop(issued);
    }
    if matches!(case, RetirementSourceCase::Failure) {
        let retained = native_value
            .upgrade()
            .expect("unconfirmed retirement keeps current source");
        assert_eq!(test_support::force(&retained), 41);
        drop(retained);
        assert!(matches!(
            observation.borrow().resident.as_ref().unwrap().settlement,
            resident::ResidentSettlement::NeedsAttention(_)
        ));
        owner.address.stop(None);
        owner
            .address
            .get_cell()
            .wait(Some(std::time::Duration::from_secs(5)))
            .await
            .unwrap();
    }
    assert!(
        native_value.upgrade().is_none(),
        "observation retains no executable source"
    );
    assert_eq!(test_support::outstanding_custody(), baseline);
    owner.address.stop(None);
}

struct Executor;
impl Actor for Executor {
    type Msg = KernelMessage;
    type State = ();
    type Arguments = ();

    async fn pre_start(&self, _: RactorRef<Self::Msg>, (): ()) -> Result<(), ActorProcessingErr> {
        Ok(())
    }

    async fn handle(
        &self,
        _: RactorRef<Self::Msg>,
        _: Self::Msg,
        _: &mut (),
    ) -> Result<(), ActorProcessingErr> {
        Ok(())
    }
}

async fn opened() -> (LifecycleHandle, RactorRef<KernelMessage>) {
    let identity = ActorRef::first(crate::ActorId(9101));
    let owner = LifecycleHandle::start(identity, RetainedActorExit::new())
        .await
        .unwrap();
    let (executor, _) = Actor::spawn(None, Executor, ()).await.unwrap();
    owner.attach_executor(executor.clone()).await.unwrap();
    owner.executor_started().await.unwrap();
    (owner, executor)
}

#[tokio::test]
async fn close_preserves_accepted_lease_and_refuses_new_admission() {
    let (owner, executor) = opened().await;
    let admitted = owner.transaction().await.unwrap();
    owner.close().await.unwrap();
    assert_eq!(owner.observation().phase, LifecyclePhase::Closing);
    assert_eq!(owner.observation().transactions, 1);
    assert!(matches!(
        owner.transaction().await,
        Err(KernelCallFailure::MailboxClosed(_))
    ));
    let mut drained = Box::pin(owner.wait_transactions());
    assert!(futures_util::poll!(drained.as_mut()).is_pending());
    drop(admitted);
    tokio::time::timeout(std::time::Duration::from_secs(5), drained)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(owner.observation().phase, LifecyclePhase::Closing);
    executor.stop(None);
    owner.address.stop(None);
}

#[tokio::test]
async fn lost_admission_observer_releases_its_exact_lease_without_reopening() {
    let (owner, executor) = opened().await;
    let (reply, observer) = tokio::sync::oneshot::channel();
    owner
        .address
        .cast(LifecycleCommand::Transaction(reply.into()))
        .unwrap();
    drop(observer);
    owner.close().await.unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(5), owner.wait_transactions())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(owner.observation().transactions, 0);
    assert_eq!(owner.observation().phase, LifecyclePhase::Closing);
    assert!(matches!(
        owner.transaction().await,
        Err(KernelCallFailure::MailboxClosed(_))
    ));
    executor.stop(None);
    owner.address.stop(None);
}

proptest::proptest! {
    #![proptest_config(proptest::test_runner::Config::with_cases(64))]
    #[test]
    fn admission_histories_obey_close_and_exact_lease_release(
        steps in proptest::collection::vec((0u8..4, 0usize..4), 1..48)
    ) {
        let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        runtime.block_on(async {
            let (owner, executor) = opened().await;
            let initial = owner.transaction().await.unwrap();
            let mut settled_sequence = initial.sequence;
            drop(initial);
            let mut open = true;
            let mut held: [Option<ActorAdmissionLease>; 4] = std::array::from_fn(|_| None);
            for (operation, slot) in steps {
                match operation {
                    0 => {
                        let result = owner.transaction().await;
                        assert_eq!(result.is_ok(), open);
                        if let Ok(lease) = result { held[slot] = Some(lease); }
                    }
                    1 => { held[slot].take(); }
                    2 => { owner.close().await.unwrap(); open = false; }
                    _ => {
                        // Replay a previously issued, completed operation.
                        // It cannot reopen or release a live operation.
                        owner.address.cast(LifecycleCommand::NativeCompleted {
                            sequence: settled_sequence, outcome: Ok(()),
                        }).unwrap();
                    }
                }
                // A same-mailbox round trip orders prior lease drops and stale
                // completions without using a timing assumption.
                let barrier = owner.transaction().await;
                assert_eq!(barrier.is_ok(), open);
                let snapshot = owner.observation();
                assert_eq!(snapshot.phase, if open { LifecyclePhase::Open } else { LifecyclePhase::Closing });
                assert_eq!(snapshot.transactions, held.iter().filter(|lease| lease.is_some()).count() + usize::from(open));
                if let Ok(lease) = &barrier { settled_sequence = lease.sequence; }
                drop(barrier);
            }
            drop(held);
            owner.close().await.unwrap();
            tokio::time::timeout(std::time::Duration::from_secs(5), owner.wait_transactions()).await.unwrap().unwrap();
            executor.stop(None);
            owner.address.stop(None);
        });
    }
}

struct StartupExecutorState {
    lifecycle: LifecycleHandle,
    events: tokio::sync::mpsc::UnboundedSender<crate::SourceDelivery>,
    attached: Option<tokio::sync::mpsc::UnboundedSender<RactorRef<KernelMessage>>>,
    ready: Option<tokio::sync::oneshot::Receiver<bool>>,
}

struct StartupExecutor;
impl Actor for StartupExecutor {
    type Msg = KernelMessage;
    type State = StartupExecutorState;
    type Arguments = Self::State;

    async fn pre_start(
        &self,
        myself: RactorRef<Self::Msg>,
        mut state: Self::Arguments,
    ) -> Result<Self::State, ActorProcessingErr> {
        state.lifecycle.attach_executor(myself.clone()).await?;
        state.attached.take().unwrap().send(myself)?;
        if !state.ready.take().unwrap().await? {
            return Err(std::io::Error::other("startup refused before readiness").into());
        }
        state.lifecycle.executor_started().await?;
        Ok(state)
    }

    async fn handle(
        &self,
        _: RactorRef<Self::Msg>,
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
        _: RactorRef<Self::Msg>,
        state: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        state.lifecycle.close().await?;
        state.lifecycle.executor_retired();
        Ok(())
    }
}
async fn source_receipt(
    owner: &LifecycleHandle,
    delivery: crate::SourceDelivery,
) -> Result<(), KernelCallFailure> {
    let (send, receive) = tokio::sync::oneshot::channel();
    owner
        .address
        .cast(LifecycleCommand::Source {
            delivery,
            reply: Some(send.into()),
        })
        .unwrap();
    receive.await.unwrap()
}

#[tokio::test]
async fn startup_source_snapshot_keeps_fifo_and_releases_on_failed_readiness() {
    use crate::request::sources::{RequestSourceKind, SourceEvent, SourceTarget};
    use crate::SourceDelivery;
    use tokio::sync::mpsc;
    let actor = |id| ActorRef::first(crate::ActorId(id));
    let registry = crate::request::RequestRegistry::default();
    let request = registry.reserve_native(actor(100), actor(101));
    registry
        .mark_queued(actor(100), actor(101), request)
        .unwrap();
    registry.present(actor(101), request).unwrap();
    let claim = registry.begin_reply(actor(101), request).unwrap();
    for success in [false, true] {
        let snapshot = crate::request::test_support::snapshot(&claim);
        let payload = Arc::downgrade(&snapshot);
        let lifecycle =
            crate::kernel::LifecycleHandle::start(actor(120), crate::RetainedActorExit::new())
                .await
                .unwrap();
        let closed = || SourceDelivery {
            slot: 9,
            target: SourceTarget::Request(request, RequestSourceKind::Progress),
            event: SourceEvent::ProgressClosed,
        };
        assert!(
            matches!(
                source_receipt(&lifecycle, closed()).await,
                Err(KernelCallFailure::TargetUnavailable(_))
            ),
            "a source cannot enter startup before executor attachment"
        );
        let (send, mut events) = mpsc::unbounded_channel();
        let (attached, mut address) = mpsc::unbounded_channel();
        let (ready, wait_ready) = tokio::sync::oneshot::channel();
        let spawn_lifecycle = lifecycle.clone();
        let started = tokio::spawn(async move {
            StartupExecutor::spawn(
                None,
                StartupExecutor,
                StartupExecutorState {
                    lifecycle: spawn_lifecycle,
                    events: send,
                    attached: Some(attached),
                    ready: Some(wait_ready),
                },
            )
            .await
        });
        let address = address.recv().await.unwrap();
        assert_eq!(lifecycle.observation().phase, LifecyclePhase::Starting);
        assert!(
            matches!(
                lifecycle.enqueue(KernelMessage::DrainMailbox).await,
                Err(crate::KernelCallFailure::TargetUnavailable(_))
            ),
            "ordinary ingress remains closed before readiness"
        );
        assert!(
            source_receipt(
                &lifecycle,
                SourceDelivery {
                    slot: 0,
                    target: SourceTarget::Request(request, RequestSourceKind::Settlement),
                    event: SourceEvent::Settled(Ok(snapshot)),
                }
            )
            .await
            .is_ok(),
            "an installed source snapshot must queue during startup"
        );
        assert!(
            payload.upgrade().is_some(),
            "the queued mailbox owns the live publication"
        );
        assert!(
            events.try_recv().is_err(),
            "Ractor must not run a queued source before startup completes"
        );
        if !success {
            lifecycle.close().await.unwrap();
            assert!(matches!(
                source_receipt(&lifecycle, closed()).await,
                Err(KernelCallFailure::MailboxClosed(_))
            ));
            ready.send(false).unwrap();
            assert!(started.await.unwrap().is_err());
            assert!(events.try_recv().is_err());
            assert!(
                payload.upgrade().is_none(),
                "failed startup releases the publication owner"
            );
            assert!(matches!(
                lifecycle.enqueue(KernelMessage::DrainMailbox).await,
                Err(crate::KernelCallFailure::MailboxClosed(_))
            ));
        } else {
            ready.send(true).unwrap();
            let (_, task) = started.await.unwrap().unwrap();
            let first = tokio::time::timeout(std::time::Duration::from_secs(2), events.recv())
                .await
                .unwrap()
                .unwrap();
            assert_eq!(first.slot, 0);
            let SourceEvent::Settled(Ok(snapshot)) = first.event else {
                panic!("current source snapshot changed across startup");
            };
            assert!(std::sync::Weak::ptr_eq(
                &Arc::downgrade(&snapshot),
                &payload
            ));
            assert_eq!(crate::request::test_support::force(&snapshot), 41);
            for (slot, event) in [
                (
                    1,
                    SourceEvent::Lifecycle(crate::ActorLifecycle::Paused("update".into())),
                ),
                (2, SourceEvent::ProgressClosed),
                (3, SourceEvent::Settled(Ok(snapshot.clone()))),
            ] {
                lifecycle
                    .dispatch_source(SourceDelivery {
                        slot,
                        target: match slot {
                            1 => SourceTarget::Lifecycle(actor(100)),
                            2 => SourceTarget::Request(request, RequestSourceKind::Progress),
                            _ => SourceTarget::Request(request, RequestSourceKind::Settlement),
                        },
                        event,
                    })
                    .unwrap();
            }
            for slot in 1..=3 {
                let observed =
                    tokio::time::timeout(std::time::Duration::from_secs(2), events.recv())
                        .await
                        .unwrap()
                        .unwrap();
                assert_eq!(
                    observed.slot, slot,
                    "source update, closure and result keep the existing ingress FIFO"
                );
                match (slot, observed.event) {
                    (1, SourceEvent::Lifecycle(crate::ActorLifecycle::Paused(detail))) => {
                        assert_eq!(detail, "update")
                    }
                    (2, SourceEvent::ProgressClosed) => {}
                    (3, SourceEvent::Settled(Ok(value))) => assert!(Arc::ptr_eq(&value, &snapshot)),
                    _ => panic!("source event changed across startup"),
                }
            }
            lifecycle.close().await.unwrap();
            assert!(matches!(
                source_receipt(&lifecycle, closed()).await,
                Err(KernelCallFailure::MailboxClosed(_))
            ));
            assert!(
                events.try_recv().is_err(),
                "each accepted source is delivered exactly once"
            );
            drop(snapshot);
            address.stop(None);
            task.await.unwrap();
            assert!(
                payload.upgrade().is_none(),
                "retirement releases the last publication owner"
            );
        }
        lifecycle.address.stop(None);
    }
}
