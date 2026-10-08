use super::*;
use std::sync::atomic::{AtomicUsize, Ordering};

#[derive(Default)]
struct ModelOwner {
    prepared: Mutex<Vec<(&'static str, tidepool_repr::PrincipalId)>>,
    started: Arc<AtomicUsize>,
    cancelled: AtomicUsize,
    settling: AtomicUsize,
    settled: AtomicUsize,
    settlement: Mutex<
        Option<tokio::sync::oneshot::Receiver<Result<(), tidepool_effect::error::EffectError>>>,
    >,
}

impl crate::CellModelBinding for ModelOwner {
    fn prepare(
        &self,
        request: crate::ModelReq,
        principal: tidepool_repr::PrincipalId,
        table: tidepool_repr::DataConTable,
    ) -> tidepool_effect::DeferredEffect {
        assert_eq!(
            table.field_labels_of(tidepool_repr::DataConId(73)),
            Some(["original request table".to_owned()].as_slice())
        );
        let kind = match request {
            crate::ModelReq::ModelStartWith(_) => "start",
            crate::ModelReq::ModelResumeWith(..) => "resume",
            crate::ModelReq::ModelAnnotateWith(..) => "annotate",
            crate::ModelReq::ModelCloseWith(_) => "close",
        };
        self.prepared.lock().push((kind, principal));
        let started = self.started.clone();
        tidepool_effect::DeferredEffect::blocking(move || {
            started.fetch_add(1, Ordering::SeqCst);
            Ok(tidepool_effect::Response::new(()))
        })
    }

    fn cancel(&self) {
        self.cancelled.fetch_add(1, Ordering::SeqCst);
    }

    fn settle(
        &self,
    ) -> futures_util::future::BoxFuture<'_, Result<(), tidepool_effect::error::EffectError>> {
        Box::pin(async move {
            self.settling.fetch_add(1, Ordering::SeqCst);
            let settlement = self.settlement.lock().take();
            if let Some(settlement) = settlement {
                settlement.await.map_err(|_| {
                    tidepool_effect::error::EffectError::Handler(
                        "controlled settlement owner disappeared".into(),
                    )
                })??;
            }
            self.settled.fetch_add(1, Ordering::SeqCst);
            Ok(())
        })
    }
}

fn execution(model: Arc<ModelOwner>) -> OwnedExecution<frunk::HNil, tidepool_mcp::CapturedOutput> {
    let descriptor = crate::ActorDescriptor::new(
        "model ownership",
        crate::ActorPlacement {
            session: tidepool_repr::SessionId(17),
            resource_scope: RealmId::ROOT,
            lexical_scope: tidepool_codegen::scope::ScopeId::ROOT,
        },
    );
    let context = descriptor.session_context(crate::ActorRef::first(crate::ActorId(9)));
    let public = WorkbenchPublicOwner::issue(&context, &descriptor, None).unwrap();
    let resources = ExecutionResourceOwners::for_inspection(public);
    let workbench = crate::ResidentActorWorkbench::new(
        Arc::new(crate::ActorMachineRegistry::new()),
        crate::ActorWorkbenchSource::new("", Vec::new()),
        None,
    );
    let cleanup = workbench.continuation_cleanup_owner(
        context.clone(),
        "model test execution abandoned".into(),
        resources.clone(),
    );
    let reservation_owner = RequestReservationOwner::Workbench {
        execution: WorkbenchExecutionId::from_digest([17; 16]),
        attempt: crate::request::WorkbenchReservationAttempt::fresh(),
    };
    let invocation_work = InvocationWork::new(context.actor, reservation_owner.clone());
    OwnedExecution {
        state: Box::new(WorkbenchExecutionState {
            effects: WorkbenchEffectState {
                display_receipt_owner: None,
                park_effects: true,
                context,
                public_visibility: None,
                control: Some(crate::WorkbenchExecutionControl::untracked()),
                model: Some(model),
                context_binding: None,
                installed_tools: None,
                admitted_source: Default::default(),
                reservation_owner,
                invocation_work,
                publication: CheckpointPublication::Resident,
                after_tool_active: false,
                terminal_transfer: None,
            },
            request: WorkbenchRequest::from_cell_input("pure ()"),
            replay_request: None,
            invocation: None,
            cursor: WorkbenchCursor::default(),
        }),
        workbench: Some(workbench),
        private: None,
        timing: Some(crate::call_timing::CallScope::new("model test", 9, 1)),
        cleanup,
        resources,
        observation: Default::default(),
        retirement: crate::RetainedActorExit::new(),
    }
}

#[tokio::test]
async fn execution_disposal_cancels_model_with_parked_binding_references() {
    let model = Arc::new(ModelOwner::default());
    let execution = execution(model.clone());
    let parked = execution.state.effects.model.clone().unwrap();
    assert_eq!(model.cancelled.load(Ordering::SeqCst), 0);
    drop(execution);
    assert_eq!(model.cancelled.load(Ordering::SeqCst), 1);
    assert!(
        Arc::strong_count(&parked) > 1,
        "service disposal is not the signal"
    );
    assert_eq!(model.started.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn terminal_fence_awaits_model_with_retained_binding_after_native_acknowledgement() {
    for native_cancelled in [false, true] {
        let model = Arc::new(ModelOwner::default());
        let mut owned = execution(model.clone());
        let retained = owned.state.effects.model.clone().unwrap();
        let control = owned.state.effects.control.clone().unwrap();
        if native_cancelled {
            assert!(control.request_cancellation());
            control.acknowledge_cancellation();
        }
        let (release, settlement) = tokio::sync::oneshot::channel();
        *model.settlement.lock() = Some(settlement);
        {
            let settling = settle_execution_owners(&mut owned);
            tokio::pin!(settling);
            assert!(futures_util::poll!(&mut settling).is_pending());
            assert_eq!(model.settling.load(Ordering::SeqCst), 1);
            assert_eq!(model.settled.load(Ordering::SeqCst), 0);
            assert!(Arc::strong_count(&retained) > 1);
            release.send(Ok(())).unwrap();
            settling.await.unwrap();
        }
        assert_eq!(model.settled.load(Ordering::SeqCst), 1);
        drop(owned);
        assert_eq!(model.settled.load(Ordering::SeqCst), 1);
        assert!(Arc::strong_count(&retained) > 1);
    }
}

#[tokio::test]
async fn native_cleanup_failure_still_awaits_model_and_retains_both_failures() {
    let model = Arc::new(ModelOwner::default());
    // The fixture has no mounted machine, so native cleanup really refuses.
    let mut owned = execution(model.clone());
    let control = owned.state.effects.control.clone().unwrap();
    assert!(control.request_cancellation());
    let (release, settlement) = tokio::sync::oneshot::channel();
    *model.settlement.lock() = Some(settlement);
    let settling = settle_execution_owners(&mut owned);
    tokio::pin!(settling);
    assert!(futures_util::poll!(&mut settling).is_pending());
    assert_eq!(model.settling.load(Ordering::SeqCst), 1);
    release
        .send(Err(tidepool_effect::error::EffectError::Handler(
            "model terminal receipt unavailable".into(),
        )))
        .unwrap();
    let KernelInvocationFailure::CleanupUnconfirmed { detail, .. } = settling.await.unwrap_err()
    else {
        panic!("cleanup uncertainty must refuse successful cell settlement")
    };
    assert!(detail.contains("native cleanup unconfirmed"), "{detail}");
    assert!(detail.contains("model cleanup unconfirmed"), "{detail}");
    assert_eq!(model.settled.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn model_terminal_failure_is_unconfirmed_after_native_cancellation_was_acknowledged() {
    let model = Arc::new(ModelOwner::default());
    let mut owned = execution(model.clone());
    let control = owned.state.effects.control.clone().unwrap();
    assert!(control.request_cancellation());
    control.acknowledge_cancellation();
    let (release, settlement) = tokio::sync::oneshot::channel();
    *model.settlement.lock() = Some(settlement);
    release
        .send(Err(tidepool_effect::error::EffectError::Handler(
            "terminal model receipt missing".into(),
        )))
        .unwrap();
    let result = settle_execution_owners(&mut owned).await;
    assert!(matches!(
        &result,
        Err(KernelInvocationFailure::CleanupUnconfirmed { .. })
    ));
    control.settle(result.map(|_| unreachable!("cleanup failed")));
    assert!(matches!(
        control.cancellation_outcome(
            WorkbenchExecutionId::from_digest([17; 16]),
            control.terminal_reply().unwrap(),
        ),
        crate::WorkbenchCancellationOutcome::Unconfirmed { .. }
    ));
}

#[tokio::test]
async fn terminal_finalization_releases_invocation_reservations_after_model_cleanup_failure() {
    let fixture = crate::resident_actor::invocation_work::tests::Fixture::start().await;
    let model = Arc::new(ModelOwner::default());
    let mut owned = execution(model.clone());
    let context = owned.state.effects.context.clone();
    let reservation_owner = owned.state.effects.reservation_owner.clone();
    let invocation = owned.state.effects.invocation_work.clone();
    let request = fixture.environment.requests.reserve_for_operation(
        context.actor,
        fixture.actor.identity(),
        "unsubmitted callback request".into(),
        false,
        Some(reservation_owner.clone()),
    );
    let (release, settlement) = tokio::sync::oneshot::channel();
    *model.settlement.lock() = Some(settlement);
    release
        .send(Err(tidepool_effect::error::EffectError::Handler(
            "model terminal receipt unavailable".into(),
        )))
        .unwrap();
    let finalization = WorkbenchFinalization {
        context: context.clone(),
        reservation_owner,
        invocation_work: invocation.clone(),
        kernel: fixture.kernel.clone(),
        result: Err(workbench_failure(
            &[],
            0,
            1,
            ResidentActorWorkbenchError::ActorProtocol("authored callback failed".into()),
        )),
        rejected: true,
        retire_scopes: None,
        context_boundary: None,
        control: None,
    };
    invocation.close();
    assert!(invocation.cleanup_observation().is_none());
    assert!(fixture
        .environment
        .requests
        .observe_response(context.actor, request)
        .is_ok());
    let result =
        settle_execution_finalization(&mut owned, fixture.environment.clone(), finalization).await;
    let Err(KernelInvocationFailure::CleanupUnconfirmed { detail, .. }) = result.result else {
        panic!("model cleanup uncertainty must survive terminal finalization")
    };
    assert!(
        detail.contains("model terminal receipt unavailable"),
        "{detail}"
    );
    assert!(detail.contains("authored callback failed"), "{detail}");
    assert_eq!(
        invocation.cleanup_observation().unwrap().uncertainty(),
        None
    );
    assert_eq!(
        fixture
            .environment
            .requests
            .observe_response(context.actor, request),
        Err(crate::ReplyError::Stale),
        "the actual finalizer must release the unpublished reservation"
    );
    fixture.finish().await;
}

#[tokio::test]
async fn items_callbacks_and_after_tool_prepare_on_the_original_model_binding() {
    let model = Arc::new(ModelOwner::default());
    let mut execution = execution(model.clone());
    let principal = tidepool_repr::PrincipalId::from(execution.state.effects.context.actor);
    let value = || tidepool_bridge::HaskellValue::Con(tidepool_repr::DataConId(73), Vec::new());
    let requests = [
        crate::ModelReq::ModelStartWith(value()),
        crate::ModelReq::ModelResumeWith("invocation".into(), "call".into(), value()),
        crate::ModelReq::ModelAnnotateWith("invocation".into(), "operation".into(), value()),
        crate::ModelReq::ModelCloseWith("invocation".into()),
    ];
    let mut work = Vec::new();
    for (index, request) in requests.into_iter().enumerate() {
        execution.state.cursor.index = index;
        execution.state.effects.after_tool_active = index == 2;
        let mut table = tidepool_repr::DataConTable::new();
        table.set_field_labels(
            tidepool_repr::DataConId(73),
            vec!["original request table".into()],
        );
        let boundary = prepare_execution_effect(
            &execution.state.effects.context,
            &CurrentEffectOwner::Workbench(&execution.state.effects),
            ResidentActorBoundary::Model {
                continuation: ResidentHole::plain(format!("model-{index}")),
                request,
                table,
            },
        );
        let ResidentActorBoundary::External {
            continuation,
            work: deferred,
        } = boundary
        else {
            panic!("model work uses the existing external continuation route")
        };
        assert_eq!(continuation.cont_id(), format!("model-{index}"));
        work.push(deferred);
    }
    assert_eq!(
        *model.prepared.lock(),
        vec![
            ("start", principal),
            ("resume", principal),
            ("annotate", principal),
            ("close", principal)
        ]
    );
    assert_eq!(
        model.started.load(Ordering::SeqCst),
        0,
        "preparation starts no provider work"
    );
    for deferred in work {
        let tidepool_effect::DeferredEffect::Blocking(work) = deferred else {
            panic!("controlled work")
        };
        work.into_inner().unwrap()().unwrap();
    }
    assert_eq!(model.started.load(Ordering::SeqCst), 4);
    drop(execution);
    assert_eq!(model.cancelled.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn cancellation_closes_model_while_non_model_callback_wait_remains_parked() {
    for retiring in [false, true] {
        let model = Arc::new(ModelOwner::default());
        let execution = execution(model.clone());
        let control = execution.state.effects.control.clone().unwrap();
        let retirement = execution.retirement.clone();
        let binding = execution.state.effects.model.clone().unwrap();
        let mut table = tidepool_repr::DataConTable::new();
        table.set_field_labels(
            tidepool_repr::DataConId(73),
            vec!["original request table".into()],
        );
        let tidepool_effect::DeferredEffect::Blocking(start) = binding.prepare(
            crate::ModelReq::ModelStartWith(tidepool_bridge::HaskellValue::Con(
                tidepool_repr::DataConId(73),
                Vec::new(),
            )),
            tidepool_repr::PrincipalId::from(execution.state.effects.context.actor),
            table,
        ) else {
            panic!("controlled model start")
        };
        start.into_inner().unwrap()().unwrap();
        assert_eq!(model.started.load(Ordering::SeqCst), 1);

        // A callback's Sleep/Request wait has its own settlement. Model
        // cancellation must close the parent while this wait is still parked.
        let (release, parked) = tokio::sync::oneshot::channel();
        let waiting = join_execution_step(
            async {
                parked.await.unwrap();
                42
            },
            control.clone(),
            retirement.clone(),
            Some(binding),
        );
        tokio::pin!(waiting);
        assert!(futures_util::poll!(&mut waiting).is_pending());
        if retiring {
            retirement.request_shutdown(crate::ActorTerminal {
                kind: crate::ActorExitKind::Cancelled,
                summary: "model test retirement".into(),
                diagnostic: None,
            });
        } else {
            control.request_cancellation();
        }
        assert!(
            futures_util::poll!(&mut waiting).is_pending(),
            "cancellation still joins the callback's real wait"
        );
        assert_eq!(
            model.cancelled.load(Ordering::SeqCst),
            1,
            "model closes before parked wait or execution disposal"
        );
        assert!(control.cancellation_requested());
        release.send(()).unwrap();
        assert_eq!(waiting.await, 42);
        assert_eq!(model.cancelled.load(Ordering::SeqCst), 1);
        drop(execution);
    }
}

#[derive(Default)]
struct ContextOwner {
    prepared: AtomicUsize,
    cancelled: AtomicUsize,
}

impl crate::HostedContextBinding for ContextOwner {
    fn admit(
        &self,
        _: &WorkbenchExecutionId,
        _: &exomonad_tool::ToolInvocationContext,
        _: tidepool_repr::PrincipalId,
    ) -> Result<(), tidepool_effect::error::EffectError> {
        Ok(())
    }
    fn prepare(
        &self,
        _: crate::ContextReq,
        _: tidepool_repr::PrincipalId,
        _: tidepool_repr::DataConTable,
    ) -> tidepool_effect::DeferredEffect {
        self.prepared.fetch_add(1, Ordering::SeqCst);
        tidepool_effect::DeferredEffect::blocking(|| Ok(tidepool_effect::Response::new(())))
    }
    fn cancel(&self) {
        self.cancelled.fetch_add(1, Ordering::SeqCst);
    }
    fn finish(&self, _: crate::CellExit) {}
}

#[tokio::test]
async fn context_authority_cannot_escape_to_after_tool_or_actor_callbacks() {
    let binding = Arc::new(ContextOwner::default());
    let mut execution = execution(Arc::new(ModelOwner::default()));
    execution.state.effects.context_binding = Some(binding.clone());
    let boundary = || ResidentActorBoundary::Context {
        continuation: ResidentHole::plain("context-owned"),
        request: crate::ContextReq::GetContextWith,
        table: tidepool_repr::DataConTable::new(),
    };
    let routed = prepare_execution_effect(
        &execution.state.effects.context,
        &CurrentEffectOwner::Workbench(&execution.state.effects),
        boundary(),
    );
    assert!(matches!(routed, ResidentActorBoundary::External { .. }));
    assert_eq!(binding.prepared.load(Ordering::SeqCst), 1);
    execution.state.effects.after_tool_active = true;
    let owners = [
        CurrentEffectOwner::Workbench(&execution.state.effects),
        CurrentEffectOwner::Actor {
            ephemeral_work: execution.state.effects.invocation_work.clone(),
            publication: CheckpointPublication::Resident,
            reservation_owner: None,
            control: None,
        },
    ];
    for owner in owners {
        let ResidentActorBoundary::External { work, .. } =
            prepare_execution_effect(&execution.state.effects.context, &owner, boundary())
        else {
            panic!("context authority refusal is resumed through the normal effect route")
        };
        let tidepool_effect::DeferredEffect::Blocking(start) = work else {
            panic!("context authority denial must not start asynchronous work")
        };
        assert!(start.into_inner().unwrap()().is_err());
    }
    assert_eq!(binding.prepared.load(Ordering::SeqCst), 1);
    drop(execution);
    assert_eq!(binding.cancelled.load(Ordering::SeqCst), 1);
}
