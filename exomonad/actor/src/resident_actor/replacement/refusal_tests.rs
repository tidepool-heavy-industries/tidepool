//! Refusal and cancellation preserve issued candidate custody.

use super::*;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use tidepool_codegen::scope::ScopeId;
use tidepool_repr::{Generation, SessionId};
use tidepool_runtime::session::turn::{run_turn, BoundBinder, TurnRequest, TurnResult};
use tidepool_runtime::session::{
    resident_workbench_templates, CompiledTurn, ModuleEnv, SessionLib,
};
use tidepool_testing::effect_surface::TestEffectSurface;

type Behavior = ResidentKernelBehavior<frunk::HNil, tidepool_mcp::CapturedOutput>;
type Session = ResidentSession<frunk::HNil, tidepool_mcp::CapturedOutput>;

struct Fixture {
    _root: tempfile::TempDir,
    surface: TestEffectSurface,
    compiled: Arc<CompiledTurn>,
    bound: Vec<BoundBinder>,
    roots: Arc<Mutex<Vec<tempfile::TempDir>>>,
    ready: bool,
}

impl Fixture {
    fn compile() -> Self {
        Self::compile_mode(false)
    }

    fn compile_mode(ready: bool) -> Self {
        tidepool_testing::eval_harness::require_extract();
        let declarations = [
            tidepool_mcp::actor_kernel_decl(),
            tidepool_mcp::actor_local_decl(),
        ];
        let surface = TestEffectSurface::with_options(
            &declarations,
            tidepool_testing::effect_surface::TestEffectSurfaceOptions {
                row_args: tidepool_mcp::RowArgs::default()
                    .importing(["Tidepool.Effects.Core (ActorKernel(..), ActorLocal(..))"]),
                ..Default::default()
            },
        )
        .unwrap();
        let root = tempfile::tempdir().unwrap();
        let templates = resident_workbench_templates(surface.preamble(), surface.row(), "");
        let include = surface
            .include_paths()
            .iter()
            .map(PathBuf::as_path)
            .collect::<Vec<_>>();
        let TurnResult::Bind {
            compiled, bound, ..
        } = tidepool_testing::with_settlement(|settlement| {
            run_turn(
                TurnRequest {
                    exact_context: None,
                    session_id: None,
                    turn_text: include_str!("../replacement_staging_inputs.hs"),
                    templates: &templates,
                    include: &include,
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
        .unwrap()
        else {
            panic!("native replacement inputs bind");
        };
        Self {
            _root: root,
            surface,
            compiled: Arc::new(compiled),
            bound,
            roots: Default::default(),
            ready,
        }
    }

    fn machine(&self, id: SessionId, bind: bool) -> Session {
        let root = tempfile::tempdir().unwrap();
        let library = SessionLib::open(id, root.path(), ModuleEnv::standalone_default())
            .unwrap()
            .with_validation_include(self.surface.include_paths().to_vec());
        let mut session = Session::unbootstrapped(
            frunk::HNil,
            tidepool_mcp::CapturedOutput::new(),
            tidepool_runtime::DEFAULT_NURSERY_SIZE,
            Some(library),
        );
        if bind {
            assert!(matches!(
                tidepool_testing::with_settlement(|settlement| session
                    .run_projected_bind_with_sites(
                        "replacement-refusal-inputs",
                        self.compiled.code(),
                        &self.bound,
                        Generation(1),
                        settlement
                    ))
                .unwrap(),
                ResidentOutcome::BindingsCommitted { .. }
            ));
        }
        self.roots.lock().push(root);
        session
    }

    async fn behavior(&self) -> (Behavior, super::super::invocation_work::tests::Fixture) {
        let mut owner = super::super::invocation_work::tests::Fixture::start().await;
        let id = SessionId(0xE201);
        let machines = Arc::new(ActorMachineRegistry::new());
        machines.insert_idle(id, Box::new(self.machine(id, true)));
        owner.environment.runner = ResidentActorRunner::new(
            machines,
            ActorWorkbenchSource::new(
                self.surface.preamble(),
                self.surface.include_paths().to_vec(),
            ),
        );
        let descriptor = ActorDescriptor::new(
            "replacement-refusal-predecessor",
            crate::ActorPlacement {
                session: id,
                resource_scope: RealmId::fresh(),
                lexical_scope: ScopeId::ROOT,
            },
        );
        let mut behavior = Behavior::with_boot(
            descriptor,
            owner.environment.clone(),
            ResidentBoot::Workbench,
            Vec::new(),
        );
        let actor = owner.actor.identity();
        let context = behavior.context(actor);
        let entry = retain(&behavior.environment.runner, id, "predecessorEntry").await;
        let initial =
            Arc::new(retain(&behavior.environment.runner, id, "replacementCheckpoint").await);
        let outcome = owner
            .kernel
            .compiler_lifecycle_scope(behavior.environment.runner.run_rooted_application(
                context.clone(),
                entry,
                initial,
                context.placement.resource_scope,
            ))
            .await
            .unwrap();
        owner
            .kernel
            .compiler_lifecycle_scope(behavior.stabilize_program(
                &owner.kernel,
                &context,
                &crate::CallAncestry::begin(actor),
                outcome,
                None,
            ))
            .await
            .unwrap();
        assert!(matches!(behavior.standing, ResidentStanding::Receiving(_)));
        let input = retain(&behavior.environment.runner, id, "predecessorInput").await;
        let error = crate::KernelBehavior::cast(
            &mut behavior,
            &owner.kernel,
            actor,
            MailboxValue::new(id, input),
        )
        .await
        .expect_err("native predecessor handler deliberately fails");
        assert!(
            error
                .detail
                .contains("intentional predecessor handler failure"),
            "{error}"
        );
        assert!(
            crate::KernelBehavior::pause_failed_handler(
                &mut behavior,
                &owner.kernel,
                &error.detail
            )
            .await
        );
        assert!(matches!(
            crate::KernelBehavior::resume(&mut behavior, &owner.kernel).await,
            Ok(KernelStep::Continue(()))
        ));
        owner
            .kernel
            .compiler_lifecycle_scope(verify_retained_predecessor_custody(&behavior, actor))
            .await;
        (behavior, owner)
    }

    fn with_factory(&self, behavior: &mut Behavior, refuse: bool) -> Arc<AtomicUsize> {
        let roots = self.roots.clone();
        let include = self.surface.include_paths().to_vec();
        let calls = Arc::new(AtomicUsize::new(0));
        let observed = calls.clone();
        behavior.environment.runner = behavior
            .environment
            .runner
            .clone()
            .with_child_bootstrap_program(self.compiled.clone())
            .with_child_session_factory(Arc::new(move |id, _, _settlement| {
                observed.fetch_add(1, Ordering::Relaxed);
                if refuse {
                    return Err("controlled replacement provisioning refusal".into());
                }
                let root = tempfile::tempdir().unwrap();
                let library = SessionLib::open(id, root.path(), ModuleEnv::standalone_default())
                    .unwrap()
                    .with_validation_include(include.clone());
                roots.lock().push(root);
                Ok(Box::new(Session::unbootstrapped(
                    frunk::HNil,
                    tidepool_mcp::CapturedOutput::new(),
                    tidepool_runtime::DEFAULT_NURSERY_SIZE,
                    Some(library),
                )))
            }));
        calls
    }

    async fn definition(
        &self,
        behavior: &Behavior,
        session: SessionId,
    ) -> (crate::ActorReplacementDefinition, crate::ActorPlacement) {
        let parent = behavior.descriptor.placement().session;
        let lexical_scope = if session == parent {
            let mut checkout = behavior
                .environment
                .runner
                .machines_for_test()
                .checkout_run(parent)
                .unwrap();
            let scope = checkout.machine().mint_isolated_scope();
            checkout.restore_suspended(Vec::new());
            scope
        } else {
            ScopeId::ROOT
        };
        let placement = crate::ActorPlacement {
            session,
            resource_scope: RealmId::fresh(),
            lexical_scope,
        };
        (
            crate::ActorReplacementDefinition {
                child: crate::start::CapturedChildLaunch {
                    lifetime: crate::WorkerLifetime::ActorOwned,
                    descriptor: ActorDescriptor::new("replacement-refusal-candidate", placement),
                    spawn: None,
                    entry: MailboxValue::new(
                        parent,
                        retain(
                            &behavior.environment.runner,
                            parent,
                            if self.ready {
                                "replacementReadyEntry"
                            } else {
                                "replacementEntry"
                            },
                        )
                        .await,
                    ),
                    launch_worktrees: Vec::new(),
                    record_workspace: None,
                    seed: None,
                    exit_destination: None,
                },
            },
            placement,
        )
    }
}

async fn retain(
    runner: &ResidentActorRunner<frunk::HNil, tidepool_mcp::CapturedOutput>,
    id: SessionId,
    name: &str,
) -> RootCustody {
    let mut checkout = runner.machines_for_test().checkout_run(id).unwrap();
    let mut context = checkout.machine().run_context();
    context.lexical_scope = ScopeId::ROOT;
    checkout.machine().set_run_context(context).unwrap();
    let value = checkout
        .machine()
        .retain_binding_custody(name)
        .unwrap()
        .unwrap();
    checkout.restore_suspended(Vec::new());
    value
}

async fn verify_retained_predecessor_custody(behavior: &Behavior, actor: ActorRef) {
    let ResidentStanding::Paused(paused) = &behavior.standing else {
        panic!("predecessor state is retained");
    };
    let context = behavior.context(actor);
    let entry = retain(
        &behavior.environment.runner,
        context.placement.session,
        "replacementEntry",
    )
    .await;
    let outcome = behavior
        .environment
        .runner
        .run_rooted_application(
            context.clone(),
            entry,
            paused.checkpoint.value.clone(),
            context.placement.resource_scope,
        )
        .await
        .unwrap();
    let ResidentOutcome::Completed { result, .. } = outcome else {
        panic!("checkpoint verifier completes");
    };
    assert_eq!(result.to_json(), serde_json::json!(42));
    let RetainedActorInput::Mailbox(input) = &paused.input else {
        panic!("failed mailbox input remains retained");
    };
    let entry = retain(
        &behavior.environment.runner,
        context.placement.session,
        "predecessorInputValue",
    )
    .await;
    let outcome = behavior
        .environment
        .runner
        .run_rooted_application(
            context.clone(),
            entry,
            input.clone(),
            context.placement.resource_scope,
        )
        .await
        .unwrap();
    let ResidentOutcome::Completed { result, .. } = outcome else {
        panic!("failed input verifier completes");
    };
    assert_eq!(result.to_json(), serde_json::json!(73));
}

fn failure(
    result: Result<StagedHandler, ResidentActorWorkbenchError>,
) -> ResidentActorWorkbenchError {
    match result {
        Err(error) => error,
        Ok(_) => panic!("refusal fixture must not stage a successor"),
    }
}

#[tokio::test]
async fn replacement_refusal_preserves_reserved_collision_and_materialized_placements() {
    let fixture = Fixture::compile();
    let candidate = SessionId(0xE202);
    for mode in 0..5 {
        let (mut behavior, owner) = fixture.behavior().await;
        let actor = owner.actor.identity();
        let calls = fixture.with_factory(&mut behavior, mode == 1);
        let session = if mode == 4 {
            behavior.descriptor.placement().session
        } else {
            candidate
        };
        if mode == 2 {
            behavior
                .environment
                .runner
                .machines_for_test()
                .insert_idle(candidate, Box::new(fixture.machine(candidate, true)));
        }
        let (definition, placement) = fixture.definition(&behavior, session).await;
        let result = if mode == 0 {
            behavior.stage_replacement(actor, definition).await
        } else {
            owner
                .kernel
                .compiler_lifecycle_scope(behavior.stage_replacement(actor, definition))
                .await
        };
        let error = failure(result);
        let expected = match mode {
            0 => ResidentActorWorkbenchError::CompilerCleanupOwnerUnavailable.to_string(),
            1 => "controlled replacement provisioning refusal".into(),
            2 => "already had a live entry; refusing to overwrite it".into(),
            _ => "actor startup completed without reaching readiness".into(),
        };
        assert!(
            error.to_string().contains(&expected),
            "mode {mode}: {error}"
        );
        assert!(
            !error.to_string().contains("candidate cleanup failed"),
            "mode {mode}: {error}"
        );
        assert_eq!(
            calls.load(Ordering::Relaxed),
            usize::from(mode != 0 && mode != 4)
        );
        if mode == 2 {
            let mut checkout = behavior
                .environment
                .runner
                .machines_for_test()
                .checkout_run(candidate)
                .expect("failed collision never acquired existing machine cleanup authority");
            assert!(checkout
                .machine()
                .retain_binding_custody("replacementCheckpoint")
                .unwrap()
                .is_some());
            assert!(checkout.machine().compile_view_in(ScopeId::ROOT).is_some());
            checkout.restore_suspended(Vec::new());
        } else if mode == 4 {
            let mut checkout = behavior
                .environment
                .runner
                .machines_for_test()
                .checkout_run(session)
                .unwrap();
            assert!(checkout
                .machine()
                .compile_view_in(placement.lexical_scope)
                .is_none());
            assert!(checkout.machine().compile_view_in(ScopeId::ROOT).is_some());
            checkout.restore_suspended(Vec::new());
        } else {
            assert!(behavior
                .environment
                .runner
                .machines_for_test()
                .kind(candidate)
                .is_none());
        }
        owner
            .kernel
            .compiler_lifecycle_scope(verify_retained_predecessor_custody(&behavior, actor))
            .await;
        drop(behavior);
        owner.finish().await;
    }
}

#[tokio::test]
async fn cancelled_replacement_stage_retains_exact_shared_and_dedicated_cleanup() {
    let fixture = Fixture::compile();
    for dedicated in [false, true] {
        let (mut behavior, owner) = fixture.behavior().await;
        let actor = owner.actor.identity();
        let parent = behavior.descriptor.placement().session;
        let candidate = if dedicated { SessionId(0xE203) } else { parent };
        fixture.with_factory(&mut behavior, false);
        let (definition, placement) = fixture.definition(&behavior, candidate).await;
        let checkout = behavior
            .environment
            .runner
            .machines_for_test()
            .checkout_run(parent)
            .unwrap();
        let mut staging = Box::pin(
            owner
                .kernel
                .compiler_lifecycle_scope(behavior.stage_replacement(actor, definition)),
        );
        std::future::poll_fn(|cx| {
            use std::future::Future;
            assert!(
                staging.as_mut().poll(cx).is_pending(),
                "staging must wait on the predecessor"
            );
            if !dedicated
                || behavior
                    .environment
                    .runner
                    .machines_for_test()
                    .kind(candidate)
                    .is_some()
            {
                std::task::Poll::Ready(())
            } else {
                std::task::Poll::Pending
            }
        })
        .await;
        drop(staging);
        drop(checkout);
        if dedicated {
            assert!(
                behavior
                    .environment
                    .runner
                    .machines_for_test()
                    .kind(candidate)
                    .is_none(),
                "owned startup lease discards cancelled dedicated candidate"
            );
        }
        let work = behavior.workbench_executions.lock().actor_scope_root(actor);
        let cleanup = work.cleanup(&behavior.environment, &owner.kernel).await;
        assert_eq!(
            cleanup.uncertainty(),
            None,
            "abandoned placement stays with exact existing owner"
        );
        if !dedicated {
            let mut checkout = behavior
                .environment
                .runner
                .machines_for_test()
                .checkout_run(parent)
                .unwrap();
            assert!(checkout
                .machine()
                .compile_view_in(placement.lexical_scope)
                .is_none());
            checkout.restore_suspended(Vec::new());
        }
        owner
            .kernel
            .compiler_lifecycle_scope(verify_retained_predecessor_custody(&behavior, actor))
            .await;
        drop(behavior);
        owner.finish().await;
    }
}

#[tokio::test]
async fn staged_successor_admission_refusal_and_cancellation_release_actual_placement() {
    let fixture = Fixture::compile_mode(true);
    for dedicated in [false, true] {
        for cancelled in [false, true] {
            let (mut behavior, owner) = fixture.behavior().await;
            let actor = owner.actor.identity();
            let parent = behavior.descriptor.placement().session;
            let candidate = if dedicated { SessionId(0xE204) } else { parent };
            fixture.with_factory(&mut behavior, false);
            let (definition, _) = fixture.definition(&behavior, candidate).await;
            let reservation = owner
                .kernel
                .reserve_successor_creation()
                .await
                .expect("actual staged replacement reservation before preparation");
            let staged = owner
                .kernel
                .compiler_lifecycle_scope(behavior.stage_replacement(actor, definition))
                .await
                .expect("real replacement reaches readiness, checkpoint and receive");
            let actual = staged.descriptor.placement();
            let custody = staged.placement_custody.clone();
            owner.seal_membership().await;
            let starting = owner
                .kernel
                .compiler_lifecycle_scope(behavior.admit_staged_successor(
                    &owner.kernel,
                    staged,
                    reservation,
                ));
            if cancelled {
                // Cancellation before producer submission drops the actual
                // affine reservation; native placement custody still settles.
                drop(starting);
            } else {
                assert!(
                    starting.await.is_err(),
                    "closed owner must refuse successor publication"
                );
            }
            custody
                .cleanup(&behavior.environment.runner, parent)
                .await
                .unwrap();
            if dedicated {
                assert!(
                    behavior
                        .environment
                        .runner
                        .machines_for_test()
                        .kind(candidate)
                        .is_none(),
                    "refusal returns only after the sole startup lease removes the machine"
                );
            } else {
                let mut checkout = behavior
                    .environment
                    .runner
                    .machines_for_test()
                    .checkout_run(parent)
                    .unwrap();
                assert!(checkout
                    .machine()
                    .compile_view_in(actual.lexical_scope)
                    .is_none());
                assert!(checkout.machine().compile_view_in(ScopeId::ROOT).is_some());
                checkout.restore_suspended(Vec::new());
            }
            let work = behavior.workbench_executions.lock().actor_scope_root(actor);
            let cleanup = work.cleanup(&behavior.environment, &owner.kernel).await;
            assert_eq!(cleanup.uncertainty(), None);
            owner
                .kernel
                .compiler_lifecycle_scope(verify_retained_predecessor_custody(&behavior, actor))
                .await;
            drop(behavior);
            owner.finish().await;
        }
    }
}

#[tokio::test(start_paused = true)]
async fn startup_refusal_preserves_typed_cleanup_before_and_after_actor_transfer() {
    let fixture = Fixture::compile_mode(true);
    for before_start in [false, true] {
        for unavailable_checkout in [false, true] {
            let (mut behavior, owner) = fixture.behavior().await;
            let actor = owner.actor.identity();
            let parent = behavior.descriptor.placement().session;
            let (definition, _) = fixture.definition(&behavior, parent).await;
            let reservation = owner
                .kernel
                .reserve_successor_creation()
                .await
                .expect("actual staged reservation before preparation");
            let mut staged = owner
                .kernel
                .compiler_lifecycle_scope(behavior.stage_replacement(actor, definition))
                .await
                .unwrap();
            let actual = staged.descriptor.placement();
            let custody = staged.placement_custody.clone();
            if before_start {
                owner.seal_membership().await;
            } else {
                staged.descriptor = staged
                    .descriptor
                    .with_persistence_policy(crate::ActorPersistencePolicy::Durable);
            }
            let machines = behavior.environment.runner.machines_for_test().clone();
            let held = unavailable_checkout.then(|| machines.checkout_run(parent).unwrap());
            let work = behavior.workbench_executions.lock().actor_scope_root(actor);
            let result = owner
                .kernel
                .compiler_lifecycle_scope(behavior.admit_staged_successor(
                    &owner.kernel,
                    staged,
                    reservation,
                ))
                .await;
            let error = match result {
                Err(error) => error,
                Ok(_) => panic!("real membership or durable-path refusal must stop startup"),
            };
            drop(held);
            if unavailable_checkout {
                assert!(error.to_string().contains("cleanup unconfirmed"), "{error}");
                for _ in 0..2 {
                    assert!(custody
                        .cleanup(&behavior.environment.runner, parent)
                        .await
                        .is_err());
                    assert!(
                        work.cleanup(&behavior.environment, &owner.kernel)
                            .await
                            .uncertainty()
                            .is_some(),
                        "retained startup cleanup cannot be reclassified by a later retirement"
                    );
                }
                behavior
                    .environment
                    .runner
                    .retire_root_placement(actual)
                    .await
                    .unwrap();
            } else {
                for _ in 0..2 {
                    custody
                        .cleanup(&behavior.environment.runner, parent)
                        .await
                        .unwrap();
                    assert_eq!(
                        work.cleanup(&behavior.environment, &owner.kernel)
                            .await
                            .uncertainty(),
                        None
                    );
                }
            }
            let mut checkout = behavior
                .environment
                .runner
                .machines_for_test()
                .checkout_run(parent)
                .unwrap();
            assert!(checkout
                .machine()
                .compile_view_in(actual.lexical_scope)
                .is_none());
            assert!(checkout.machine().compile_view_in(ScopeId::ROOT).is_some());
            checkout.restore_suspended(Vec::new());
            owner
                .kernel
                .compiler_lifecycle_scope(verify_retained_predecessor_custody(&behavior, actor))
                .await;
            drop(behavior);
            owner.finish().await;
        }
    }
}

#[tokio::test]
async fn source_handler_failure_retains_original_checkpoint_and_source_after_staging() {
    use crate::request::sources::{CallbackExecution, SourceBinding, SourceEvent, SourceTarget};

    let fixture = Fixture::compile_mode(true);
    let (mut behavior, owner) = fixture.behavior().await;
    let actor = owner.actor.identity();
    let parent = behavior.descriptor.placement().session;
    fixture.with_factory(&mut behavior, false);
    let (mut definition, _) = fixture.definition(&behavior, parent).await;
    definition.child.entry = MailboxValue::new(
        parent,
        retain(
            &behavior.environment.runner,
            parent,
            "replacementSourceFailureEntry",
        )
        .await,
    );
    let staged = owner
        .kernel
        .compiler_lifecycle_scope(behavior.stage_replacement(actor, definition))
        .await
        .unwrap();
    staged
        .placement_custody
        .transfer_to_actor(actor, staged.descriptor.placement())
        .unwrap();
    let original_checkpoint = Arc::clone(&staged.checkpoint.value);
    behavior.descriptor = staged.descriptor;
    behavior.standing = ResidentStanding::Receiving(staged.receiver);
    behavior.committed = Some(CommittedActorState {
        checkpoint: staged.checkpoint,
        input: None,
    });
    let target = SourceTarget::Lifecycle(actor);
    let execution = CallbackExecution::from_context(behavior.descriptor.session_context(actor));
    behavior.sources.push(SourceBinding {
        target,
        execution,
        entry: Arc::new(
            retain(
                &behavior.environment.runner,
                parent,
                "replacementLifecycleMapper",
            )
            .await,
        ),
    });
    let delivery = crate::SourceDelivery {
        slot: 0,
        target,
        event: SourceEvent::Lifecycle(crate::ActorLifecycle::Paused(
            "original source event".into(),
        )),
    };
    let error = match behavior.source(&owner.kernel, delivery).await {
        Err(error) => error,
        Ok(_) => panic!("source handler must fail after its next checkpoint is staged"),
    };
    assert!(
        error
            .detail
            .contains("controlled failure after staged source checkpoint"),
        "{error}"
    );
    assert!(
        behavior.pending_checkpoint.is_some(),
        "the real continuation reached its new checkpoint"
    );
    assert!(
        behavior
            .pause_failed_handler(&owner.kernel, &error.detail)
            .await
    );
    assert!(behavior.pending_checkpoint.is_none());
    let ResidentStanding::Paused(paused) = &behavior.standing else {
        panic!("failed source handler pauses for replacement");
    };
    assert!(Arc::ptr_eq(&paused.checkpoint.value, &original_checkpoint));
    let RetainedActorInput::Source(source) = &paused.input else {
        panic!("mapped mailbox custody must not replace the original source delivery");
    };
    assert_eq!(source.slot, 0);
    assert_eq!(source.target, target);
    assert!(matches!(
        &source.event,
        SourceEvent::Lifecycle(crate::ActorLifecycle::Paused(detail))
            if detail == "original source event"
    ));
    let context = behavior.context(actor);
    let entry = retain(&behavior.environment.runner, parent, "replacementEntry").await;
    let outcome = owner
        .kernel
        .compiler_lifecycle_scope(behavior.environment.runner.run_rooted_application(
            context.clone(),
            entry,
            Arc::clone(&paused.checkpoint.value),
            context.placement.resource_scope,
        ))
        .await
        .unwrap();
    let ResidentOutcome::Completed { result, .. } = outcome else {
        panic!("retained predecessor checkpoint remains executable");
    };
    assert_eq!(result.to_json(), serde_json::json!(42));
    let work = behavior.workbench_executions.lock().actor_scope_root(actor);
    assert_eq!(
        work.cleanup(&behavior.environment, &owner.kernel)
            .await
            .uncertainty(),
        None
    );
    drop(behavior);
    owner.finish().await;
}

#[tokio::test]
async fn cancellation_cleanup_refusal_rolls_back_claim_and_retirement_keeps_uncertainty() {
    use crate::request::{RequestCleanupState, ResponseFailure, ResponseObservation};
    let fixture = Fixture::compile_mode(true);
    let (mut behavior, owner) = fixture.behavior().await;
    let target = owner.actor.identity();
    let parent = behavior.descriptor.placement().session;
    fixture.with_factory(&mut behavior, false);
    let (definition, _) = fixture.definition(&behavior, parent).await;
    let staged = owner
        .kernel
        .compiler_lifecycle_scope(behavior.stage_replacement(target, definition))
        .await
        .unwrap();
    staged
        .placement_custody
        .transfer_to_actor(target, staged.descriptor.placement())
        .unwrap();
    behavior.descriptor = staged.descriptor;
    behavior.boot = None;
    behavior.standing = ResidentStanding::Boot;
    let requests = Arc::clone(&behavior.environment.requests);
    let requester = ActorRef::first(crate::ActorId(target.id.0 + 100));
    let request = requests.reserve(requester, target);
    requests.mark_queued(requester, target, request).unwrap();
    requests.present(target, request).unwrap();
    requests
        .cancel_request(
            requester,
            request,
            crate::CancellationReason::RequesterCancelled,
        )
        .unwrap();
    let claim = requests
        .begin_cancellation_acknowledgement(target, request)
        .unwrap();
    behavior.accepted_settlement = Some(AcceptedRequestSettlement {
        transfer: None,
        kind: AcceptedRequestKind::Cancellation(claim),
    });
    // Cleanup may not retire the receiver's own realm as a message realm.
    behavior.pending_program = Some(PendingActorProgram {
        transfer: None,
        resumption: ActorResumption::ReceiveAgain(SuspendedCast {
            receiver: staged.receiver,
            handler_realm: behavior.descriptor.placement().resource_scope,
            cleanup: None,
        }),
    });
    let error = behavior.resume(&owner.kernel).await.unwrap_err();
    assert!(error
        .detail
        .contains("cancelled message lost its actor-owned receiver"));
    assert!(
        behavior.suspended_cast.is_some(),
        "refused cleanup remains owned for shutdown"
    );
    assert_eq!(
        requests.request_cleanup_state(requester, request),
        Ok(RequestCleanupState::CancellationRequested {
            reason: crate::CancellationReason::RequesterCancelled,
            presented: true,
        })
    );
    let machines = Arc::clone(behavior.environment.runner.machines_for_test());
    let checkout = machines
        .checkout_run(behavior.descriptor.placement().session)
        .unwrap();
    let terminal = ActorTerminal::new(ActorExitKind::Failed, "cancellation cleanup refused");
    let (_, realm) = behavior
        .shutdown_components(
            &owner.kernel,
            &terminal,
            tokio::time::Instant::now() + std::time::Duration::from_millis(1),
        )
        .await;
    assert!(matches!(
        realm,
        crate::CleanupComponentOutcome::Unconfirmed(_)
    ));
    assert_eq!(
        requests.observe_response(requester, request),
        Ok(ResponseObservation::Unavailable(
            ResponseFailure::TargetFailed(terminal.summary)
        ))
    );
    drop(checkout);
    drop(behavior);
    owner.finish().await;
}
