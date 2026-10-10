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
        let declarations = if ready {
            vec![
                tidepool_mcp::actor_kernel_decl(),
                tidepool_mcp::actor_local_decl(),
            ]
        } else {
            Vec::new()
        };
        let surface = if ready {
            TestEffectSurface::with_options(
                &declarations,
                tidepool_testing::effect_surface::TestEffectSurfaceOptions {
                    row_args: tidepool_mcp::RowArgs::default()
                        .importing(["Tidepool.Effects.Core (ActorKernel(..), ActorLocal(..))"]),
                    ..Default::default()
                },
            )
            .unwrap()
        } else {
            TestEffectSurface::minimal(&declarations).unwrap()
        };
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
                    turn_text: if ready {
                        include_str!("../replacement_staging_inputs.hs")
                    } else {
                        include_str!("../../resident_workbench/replacement_inputs.hs")
                    },
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
        let checkpoint =
            Arc::new(retain(&owner.environment.runner, id, "replacementCheckpoint").await);
        let mut behavior = Behavior::with_boot(
            descriptor,
            owner.environment.clone(),
            ResidentBoot::Workbench,
            Vec::new(),
        );
        behavior.standing = ResidentStanding::Paused(PausedHandler {
            checkpoint: StateCheckpoint {
                site: 0,
                value: checkpoint.clone(),
            },
            input: RetainedActorInput::Mailbox(checkpoint),
            detail: "retained predecessor".into(),
        });
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

async fn verify_predecessor(behavior: &Behavior, actor: ActorRef) {
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
    let mut checkout = behavior
        .environment
        .runner
        .machines_for_test()
        .checkout_run(context.placement.session)
        .unwrap();
    assert_eq!(
        checkout.machine().outstanding_custody(),
        1,
        "only original predecessor checkpoint remains"
    );
    checkout.restore_suspended(Vec::new());
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
            .compiler_lifecycle_scope(verify_predecessor(&behavior, actor))
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
            .compiler_lifecycle_scope(verify_predecessor(&behavior, actor))
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
            let staged = owner
                .kernel
                .compiler_lifecycle_scope(behavior.stage_replacement(actor, definition))
                .await
                .expect("real replacement reaches readiness, checkpoint and receive");
            let actual = staged.descriptor.placement();
            let custody = staged.placement_custody.clone();
            let mut admission = behavior
                .environment
                .root_admission_closed
                .clone()
                .write_owned()
                .await;
            let mut starting =
                Box::pin(owner.kernel.compiler_lifecycle_scope(
                    behavior.admit_staged_successor(&owner.kernel, staged),
                ));
            assert!(futures_util::poll!(&mut starting).is_pending());
            if cancelled {
                drop(starting);
                drop(admission);
                custody
                    .cleanup(&behavior.environment.runner, parent)
                    .await
                    .unwrap();
            } else {
                *admission = true;
                drop(admission);
                match starting.await {
                    Err(error) => {
                        assert!(error.to_string().contains("swarm admission is closed"))
                    }
                    Ok(_) => panic!("closed admission must refuse the candidate"),
                }
            }
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
                .compiler_lifecycle_scope(verify_predecessor(&behavior, actor))
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
            let mut staged = owner
                .kernel
                .compiler_lifecycle_scope(behavior.stage_replacement(actor, definition))
                .await
                .unwrap();
            let actual = staged.descriptor.placement();
            let custody = staged.placement_custody.clone();
            if before_start {
                owner.seal_membership();
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
                .compiler_lifecycle_scope(behavior.admit_staged_successor(&owner.kernel, staged))
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
                .compiler_lifecycle_scope(verify_predecessor(&behavior, actor))
                .await;
            drop(behavior);
            owner.finish().await;
        }
    }
}
