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

    fn machine(
        &self,
        id: SessionId,
    ) -> (
        Session,
        crate::resident_workbench::generation_admission::SourceDependency,
    ) {
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
        let scope = session.mint_isolated_scope();
        let dependency = ActorDescriptor::new(
            "replacement fixture source",
            crate::ActorPlacement {
                session: id,
                resource_scope: RealmId::ROOT,
                lexical_scope: scope,
            },
        )
        .admit_independent_world()
        .source_dependency()
        .clone();
        dependency.adopt_final_scope(&mut session, scope).unwrap();
        let owner = crate::capabilities::CompilerOwnerScope::current()
            .unwrap()
            .with_source_dependency(&dependency);
        // Shared compiled fixture bindings keep their original ROOT domain;
        // the actual transaction retains this machine's canonical source.
        let bound = crate::termination::CompilerWorkTicket::run_source_transaction(
            &mut session,
            owner.admit_continuation().unwrap(),
            &dependency,
            |session, settlement| {
                session.run_projected_bind_with_sites(
                    "replacement-refusal-inputs",
                    self.compiled.code(),
                    &self.bound,
                    Generation(1),
                    settlement,
                )
            },
        )
        .unwrap()
        .unwrap();
        assert!(matches!(bound, ResidentOutcome::BindingsCommitted { .. }));
        self.roots.lock().push(root);
        (session, dependency)
    }

    async fn behavior(&self) -> (Behavior, super::super::invocation_work::tests::Fixture) {
        self.behavior_with_capabilities(crate::ActorCapabilities::default())
            .await
    }

    async fn behavior_with_capabilities(
        &self,
        capabilities: crate::ActorCapabilities,
    ) -> (Behavior, super::super::invocation_work::tests::Fixture) {
        let mut owner = super::super::invocation_work::tests::Fixture::start().await;
        let id = SessionId(0xE201);
        let (session, dependency) = owner
            .environment
            .compiler_lifecycle_scope(&owner.kernel)
            .scope(async { self.machine(id) })
            .await;
        let machines = Arc::new(ActorMachineRegistry::new());
        machines.insert_idle(id, Box::new(session));
        owner.environment.runner = ResidentActorRunner::new(
            machines,
            owner
                .environment
                .runner
                .preparation_source()
                .with_config(|_| {
                    ActorWorkbenchSource::new(
                        self.surface.preamble(),
                        self.surface.include_paths().to_vec(),
                    )
                }),
        );
        let descriptor = ActorDescriptor::new(
            "replacement-refusal-predecessor",
            crate::ActorPlacement {
                session: id,
                resource_scope: RealmId::fresh(),
                lexical_scope: dependency.source_lease().unwrap().scope(),
            },
        )
        .with_capabilities(capabilities)
        .with_source_dependency(dependency);
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
        let outcome = behavior
            .environment
            .compiler_lifecycle_scope(&owner.kernel)
            .scope(behavior.environment.runner.run_rooted_application(
                context.clone(),
                entry,
                initial,
                context.placement.resource_scope,
            ))
            .await
            .unwrap();
        behavior
            .environment
            .compiler_lifecycle_scope(&owner.kernel)
            .scope(behavior.stabilize_program(
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
        behavior
            .environment
            .compiler_lifecycle_scope(&owner.kernel)
            .scope(verify_retained_predecessor_custody(&behavior, actor))
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
                    requested_effects: behavior.descriptor.capabilities().effect_keys().to_vec(),
                    model_permissions: Vec::new(),
                    lifetime: crate::WorkerLifetime::ActorOwned,
                    descriptor: ActorDescriptor::new("replacement-refusal-candidate", placement)
                        .with_source_dependency(behavior.descriptor.source_dependency().clone()),
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
async fn replacement_preserves_nonempty_authority_across_grant_order_and_refuses_changes() {
    let fixture = Fixture::compile_mode(true);
    let low = crate::Reasoning::DeepSeekReasoning(crate::DeepSeekReasoning::DeepSeekLow);
    let high = crate::Reasoning::DeepSeekReasoning(crate::DeepSeekReasoning::DeepSeekHigh);
    let permission = crate::ModelPermission {
        operation: crate::ModelOperation::ModelCall,
        family: crate::ModelFamily::DeepSeek,
    };
    let identity = |key: &str, model| {
        crate::AdmittedModelIdentity::new("catalog".into(), key.into(), "provider".into(), model)
            .unwrap()
    };
    let grants = crate::ModelPermissions::new(vec![
        crate::ModelGrant::new(
            permission,
            identity("flash", crate::Model::DeepSeekFlash),
            vec![low, high],
        )
        .unwrap(),
        crate::ModelGrant::new(
            permission,
            identity("pro", crate::Model::DeepSeekPro),
            vec![low],
        )
        .unwrap(),
    ])
    .unwrap();
    let capabilities = crate::ActorCapabilities::default().with_model_permissions(grants);
    let (mut behavior, owner) = fixture
        .behavior_with_capabilities(capabilities.clone())
        .await;
    let actor = owner.actor.identity();
    let parent = behavior.descriptor.placement().session;
    let factory_calls = fixture.with_factory(&mut behavior, false);
    let requests = vec![
        tidepool_bridge_effects::ModelGrantRequest {
            operation: permission.operation,
            catalog_identity: "catalog".into(),
            key: "pro".into(),
            efforts: vec![low],
        },
        tidepool_bridge_effects::ModelGrantRequest {
            operation: permission.operation,
            catalog_identity: "catalog".into(),
            key: "flash".into(),
            efforts: vec![high, low],
        },
    ];
    let reordered = capabilities
        .model_permissions()
        .admit_child_requests(&requests)
        .unwrap();
    assert_ne!(
        &reordered,
        capabilities.model_permissions(),
        "the fixture must change grant and effort order"
    );
    assert!(reordered.is_subset_of(capabilities.model_permissions()));
    assert!(capabilities.model_permissions().is_subset_of(&reordered));
    let (mut definition, placement) = fixture.definition(&behavior, parent).await;
    assert!(Arc::ptr_eq(
        &definition
            .child
            .descriptor
            .source_dependency()
            .source_lease()
            .unwrap(),
        &behavior
            .descriptor
            .source_dependency()
            .source_lease()
            .unwrap(),
    ));
    definition.child.model_permissions = requests.clone();
    let staged = behavior
        .environment
        .compiler_lifecycle_scope(&owner.kernel)
        .scope(behavior.stage_replacement(actor, definition))
        .await
        .expect("equal authority in a different authored order reaches real replacement readiness");
    assert_eq!(staged.descriptor.capabilities(), &capabilities);
    let custody = staged.placement_custody.clone();
    drop(staged);
    custody
        .cleanup(&behavior.environment.runner, parent)
        .await
        .unwrap();
    assert_eq!(factory_calls.load(Ordering::Relaxed), 0);
    {
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
    for mutation in 0..4 {
        let (mut definition, _) = fixture.definition(&behavior, SessionId(0xE20F)).await;
        definition.child.model_permissions = requests.clone();
        match mutation {
            0 => definition.child.model_permissions[0].key = "ungranted-model".into(),
            1 => definition.child.model_permissions[1].efforts.push(
                crate::Reasoning::DeepSeekReasoning(crate::DeepSeekReasoning::DeepSeekMax),
            ),
            2 => {
                definition.child.model_permissions.remove(0);
            }
            _ => definition.child.requested_effects.reverse(),
        }
        let result = behavior
            .environment
            .compiler_lifecycle_scope(&owner.kernel)
            .scope(behavior.stage_replacement(actor, definition))
            .await;
        assert!(
            result.is_err(),
            "mutation {mutation} must not change immutable authority or effect order"
        );
        assert_eq!(
            factory_calls.load(Ordering::Relaxed),
            0,
            "mutation {mutation} refuses before provisioning a candidate"
        );
        assert_eq!(behavior.descriptor.capabilities(), &capabilities);
    }
    behavior
        .environment
        .compiler_lifecycle_scope(&owner.kernel)
        .scope(verify_retained_predecessor_custody(&behavior, actor))
        .await;
    drop(behavior);
    owner.finish().await;
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
        let mut collision_source = None;
        if mode == 2 {
            let (machine, dependency) = behavior
                .environment
                .compiler_lifecycle_scope(&owner.kernel)
                .scope(async { fixture.machine(candidate) })
                .await;
            collision_source = Some(dependency);
            behavior
                .environment
                .runner
                .machines_for_test()
                .insert_idle(candidate, Box::new(machine));
        }
        let (definition, placement) = fixture.definition(&behavior, session).await;
        let result = if mode == 0 {
            behavior.stage_replacement(actor, definition).await
        } else {
            behavior
                .environment
                .compiler_lifecycle_scope(&owner.kernel)
                .scope(behavior.stage_replacement(actor, definition))
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
        behavior
            .environment
            .compiler_lifecycle_scope(&owner.kernel)
            .scope(verify_retained_predecessor_custody(&behavior, actor))
            .await;
        drop(behavior);
        owner.finish().await;
        drop(collision_source);
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
            behavior
                .environment
                .compiler_lifecycle_scope(&owner.kernel)
                .scope(behavior.stage_replacement(actor, definition)),
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
        behavior
            .environment
            .compiler_lifecycle_scope(&owner.kernel)
            .scope(verify_retained_predecessor_custody(&behavior, actor))
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
            let staged = behavior
                .environment
                .compiler_lifecycle_scope(&owner.kernel)
                .scope(behavior.stage_replacement(actor, definition))
                .await
                .expect("real replacement reaches readiness, checkpoint and receive");
            let actual = staged.descriptor.placement();
            let custody = staged.placement_custody.clone();
            owner.seal_membership().await;
            let starting = behavior
                .environment
                .compiler_lifecycle_scope(&owner.kernel)
                .scope(behavior.admit_staged_successor(&owner.kernel, staged, reservation));
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
            behavior
                .environment
                .compiler_lifecycle_scope(&owner.kernel)
                .scope(verify_retained_predecessor_custody(&behavior, actor))
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
            let mut staged = behavior
                .environment
                .compiler_lifecycle_scope(&owner.kernel)
                .scope(behavior.stage_replacement(actor, definition))
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
            let result = behavior
                .environment
                .compiler_lifecycle_scope(&owner.kernel)
                .scope(behavior.admit_staged_successor(&owner.kernel, staged, reservation))
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
            behavior
                .environment
                .compiler_lifecycle_scope(&owner.kernel)
                .scope(verify_retained_predecessor_custody(&behavior, actor))
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
    let staged = behavior
        .environment
        .compiler_lifecycle_scope(&owner.kernel)
        .scope(behavior.stage_replacement(actor, definition))
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
    behavior.sources.bind_owner(actor);
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
        slot_delivery: None,
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
    let outcome = behavior
        .environment
        .compiler_lifecycle_scope(&owner.kernel)
        .scope(behavior.environment.runner.run_rooted_application(
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
    let staged = behavior
        .environment
        .compiler_lifecycle_scope(&owner.kernel)
        .scope(behavior.stage_replacement(target, definition))
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

#[tokio::test]
async fn source_registration_slots_preserve_independent_callbacks_and_deferred_input() {
    use crate::request::sources::tests::{receive, DirectoryFixture};
    use crate::request::sources::{CallbackExecution, SourceBinding, SourceEvent};
    use crate::resident_actor::source_slots::{
        SourceDeliveryAdmission, SourceRegistrationFailure, SourceRegistrationState, SourceSlots,
    };

    // Both executable mappings come from this one actual compiled fixture;
    // each installation retains its independently issued source scope.
    let fixture = Fixture::compile_mode(true);
    let (behavior, owner) = fixture.behavior().await;
    let recipient_fixture = DirectoryFixture::start().await;
    let (send, mut events) = tokio::sync::mpsc::unbounded_channel();
    let (recipient, address, task) = recipient_fixture.spawn(send, None).await;
    let actor = recipient.identity();
    let context_a = behavior.context(actor);
    let session_b = SessionId(0xE211);
    let (machine_b, source_b) = behavior
        .environment
        .compiler_lifecycle_scope(&owner.kernel)
        .scope(async { fixture.machine(session_b) })
        .await;
    let context_b = ActorDescriptor::new(
        "independent source callback",
        crate::ActorPlacement {
            session: session_b,
            resource_scope: RealmId::ROOT,
            lexical_scope: source_b.source_lease().unwrap().scope(),
        },
    )
    .with_source_dependency(source_b)
    .session_context(actor);
    behavior
        .environment
        .runner
        .machines_for_test()
        .insert_idle(session_b, Box::new(machine_b));
    let source_a = context_a.source_dependency().source_lease().unwrap();
    let source_b = context_b.source_dependency().source_lease().unwrap();
    let entry_a = Arc::new(
        retain(
            &behavior.environment.runner,
            context_a.placement.session,
            "replacementLifecycleMapper",
        )
        .await,
    );
    let entry_b = Arc::new(
        retain(
            &behavior.environment.runner,
            session_b,
            "replacementLifecycleMapper",
        )
        .await,
    );
    let weak_a = Arc::downgrade(&entry_a);
    let weak_b = Arc::downgrade(&entry_b);
    let target = crate::request::sources::SourceTarget::Lifecycle(actor);
    let slots = SourceSlots::default();
    slots.bind_owner(actor);
    slots.install_connections(
        behavior
            .environment
            .requests
            .attach_sources(actor, recipient.clone(), &[])
            .unwrap(),
    );
    for (entry, context) in [(entry_a, context_a.clone()), (entry_b, context_b.clone())] {
        slots
            .attach(
                SourceBinding {
                    target,
                    entry,
                    execution: CallbackExecution::from_context(context),
                },
                |connections, slot| {
                    connections.attach_lifecycle(slot, &recipient);
                    Ok(())
                },
            )
            .unwrap();
    }
    let initial_a = receive(&mut events).await;
    let initial_b = receive(&mut events).await;
    assert_eq!((initial_a.slot, initial_b.slot), (0, 1));
    // Another real recipient owns a registration of the same B executable.
    // Capturing its slot cannot keep it executable after its namespace drops.
    let (drop_send, mut drop_events) = tokio::sync::mpsc::unbounded_channel();
    let (drop_recipient, drop_address, drop_task) = recipient_fixture.spawn(drop_send, None).await;
    let drop_slots = SourceSlots::default();
    drop_slots.bind_owner(drop_recipient.identity());
    drop_slots.install_connections(
        behavior
            .environment
            .requests
            .attach_sources(drop_recipient.identity(), drop_recipient.clone(), &[])
            .unwrap(),
    );
    let attach_claim = context_b.source_dependency().claim().unwrap();
    drop_slots
        .attach(slots.binding(1).unwrap(), |connections, slot| {
            connections.attach_lifecycle(slot, &recipient);
            Ok(())
        })
        .unwrap();
    drop(attach_claim);
    let drop_initial = receive(&mut drop_events).await;
    assert!(matches!(
        drop_slots.admit_delivery(drop_initial),
        SourceDeliveryAdmission::Admitted(_)
    ));
    let dropped_capture = drop_slots.capture_source(source_b.source_admission());
    drop(drop_slots);
    assert!(matches!(
        dropped_capture[0].observation().state,
        SourceRegistrationState::Closed { rejected: 0 }
    ));
    let first = match slots.admit_delivery(initial_a.clone()) {
        SourceDeliveryAdmission::Admitted(delivery) => delivery,
        _ => panic!("A admits before fence"),
    };
    let fence = source_a.source_admission().fence().unwrap();
    assert_eq!(
        fence.pending_claims(),
        1,
        "the real callback admission keeps its source claim through the body"
    );
    let captured_a = slots.capture_source(source_a.source_admission());
    assert_eq!(captured_a.len(), 1);
    assert_eq!(captured_a[0].observation().identity.actor, actor);
    assert_eq!(captured_a[0].observation().identity.slot, 0);
    drop(first);
    assert_eq!(fence.pending_claims(), 0);
    let independent = match slots.admit_delivery(initial_b) {
        SourceDeliveryAdmission::Admitted(delivery) => delivery,
        _ => panic!("independent B stays admitted"),
    };
    drop(independent);
    assert!(matches!(
        slots.admit_delivery(initial_a.clone()),
        SourceDeliveryAdmission::Deferred(_)
    ));
    assert!(matches!(
        captured_a[0].resume_deferred().state,
        SourceRegistrationState::Open { deferred: 1, .. }
    ));
    assert!(
        events.try_recv().is_err(),
        "fenced input never reaches the mapper"
    );
    fence
        .reopen_with(source_a.original_source(), |reopen| {
            reopen.commit();
            Ok::<_, ()>(())
        })
        .unwrap()
        .unwrap();
    captured_a[0].resume_deferred();
    let marked_first = receive(&mut events).await;
    assert!(marked_first.slot_delivery.is_some());
    let mut wrong_slot = marked_first.clone();
    wrong_slot.slot = 1;
    assert!(
        matches!(
            slots.admit_delivery(wrong_slot),
            SourceDeliveryAdmission::Rejected(_)
        ),
        "the ordinal is authenticated to the actual issuing slot, not just a number"
    );
    // New mailbox input may arrive before the marked old head. It queues
    // behind the exact old event instead of overtaking it.
    let mut next = initial_a.clone();
    next.event = SourceEvent::Lifecycle(crate::ActorLifecycle::Paused("newer input".into()));
    assert!(matches!(
        slots.admit_delivery(next),
        SourceDeliveryAdmission::Deferred(_)
    ));
    captured_a[0].resume_deferred();
    assert!(
        events.try_recv().is_err(),
        "repeated resume does not resend the marked head"
    );
    let mut mapped = 0;
    for delivery in [Some(marked_first.clone()), None] {
        let delivery = match delivery {
            Some(delivery) => delivery,
            None => receive(&mut events).await,
        };
        let admitted = match slots.admit_delivery(delivery) {
            SourceDeliveryAdmission::Admitted(delivery) => delivery,
            _ => panic!("marked head admits once"),
        };
        behavior
            .environment
            .compiler_lifecycle_scope(&owner.kernel)
            .scope(behavior.environment.runner.map_source(
                admitted.binding.execution.activation_context(),
                admitted.binding.entry.clone(),
                admitted.delivery.event.clone(),
            ))
            .await
            .unwrap();
        mapped += 1;
        drop(admitted);
    }
    assert_eq!(mapped, 2, "each accepted original is mapped exactly once");
    assert!(matches!(
        slots.admit_delivery(marked_first),
        SourceDeliveryAdmission::Rejected(_)
    ));
    assert!(events.try_recv().is_err());
    assert!(matches!(
        captured_a[0].observation().state,
        SourceRegistrationState::Open {
            deferred: 0,
            resume_failure: None
        }
    ));
    let race_fence = source_a.source_admission().fence().unwrap();
    assert!(matches!(
        slots.admit_delivery_after_source_observation(initial_a.clone(), || {
            race_fence
                .reopen_with(source_a.original_source(), |reopen| {
                    reopen.commit();
                    Ok::<_, ()>(())
                })
                .unwrap()
                .unwrap();
        }),
        SourceDeliveryAdmission::Deferred(_)
    ));
    // No driver resume runs after queue insertion in this interleaving. The
    // owning post-store gate check must redispatch the same original itself.
    let race_marked = receive(&mut events).await;
    let admitted = match slots.admit_delivery(race_marked) {
        SourceDeliveryAdmission::Admitted(delivery) => delivery,
        _ => panic!("reopen race retains its original input"),
    };
    behavior
        .environment
        .compiler_lifecycle_scope(&owner.kernel)
        .scope(behavior.environment.runner.map_source(
            admitted.binding.execution.activation_context(),
            admitted.binding.entry.clone(),
            admitted.delivery.event.clone(),
        ))
        .await
        .unwrap();
    mapped += 1;
    drop(admitted);
    assert_eq!(mapped, 3);

    let fence = source_a.source_admission().fence().unwrap();
    assert!(matches!(
        slots.admit_delivery(initial_a.clone()),
        SourceDeliveryAdmission::Deferred(_)
    ));
    fence
        .reopen_with(source_a.original_source(), |reopen| {
            reopen.commit();
            Ok::<_, ()>(())
        })
        .unwrap()
        .unwrap();
    captured_a[0].resume_deferred();
    let closing_marked = receive(&mut events).await;
    let fence = source_a.source_admission().fence().unwrap();
    let closing = slots.capture_source(source_a.source_admission());
    assert!(matches!(
        closing[0].close().state,
        SourceRegistrationState::Closed { rejected: 1 }
    ));
    assert!(matches!(
        slots.admit_delivery(closing_marked),
        SourceDeliveryAdmission::Closed(_)
    ));
    assert!(
        matches!(
            closing[0].observation().state,
            SourceRegistrationState::Closed { rejected: 1 }
        ),
        "late marked input was already settled by close and is not counted twice"
    );
    assert!(
        weak_a.upgrade().is_none(),
        "closing the actual slot releases A executable custody"
    );
    assert!(
        weak_b.upgrade().is_some(),
        "B executable custody remains with B's slot"
    );
    assert!(matches!(
        slots.admit_delivery(initial_a),
        SourceDeliveryAdmission::Closed(_)
    ));
    assert!(matches!(
        captured_a[0].observation().state,
        SourceRegistrationState::Closed { rejected: 2 }
    ));
    assert_eq!(slots.len(), 2, "closed ordinal remains a tombstone");
    assert!(slots.capture_source(source_a.source_admission()).is_empty());
    assert_eq!(slots.capture_source(source_b.source_admission()).len(), 1);
    recipient
        .terminal()
        .publish_paused("independent callback still connected".into());
    let independent = receive(&mut events).await;
    assert_eq!(
        independent.slot, 1,
        "only A's native subscription was detached"
    );
    assert!(events.try_recv().is_err());
    assert!(recipient.terminal().requested_shutdown().is_none());
    // Retirement is a permanent producer refusal, distinct from temporary
    // fencing. The same real owner retains the original without retrying it.
    source_b
        .source_admission()
        .fence()
        .unwrap()
        .retire(tidepool_runtime::session::NotebookGenerationId::INITIAL)
        .unwrap();
    let refused_b = slots.capture_source(source_b.source_admission());
    assert!(matches!(
        slots.admit_delivery(independent.clone()),
        SourceDeliveryAdmission::Refused(
            _,
            crate::resident_workbench::generation_admission::GenerationAdmissionError::Closed {
                phase: tidepool_runtime::session::SourceAdmissionPhase::Retired { .. },
                ..
            }
        )
    ));
    for _ in 0..2 {
        assert!(matches!(refused_b[0].resume_deferred().state,
            SourceRegistrationState::Open { deferred: 1, resume_failure: Some(SourceRegistrationFailure::Admission(
                crate::resident_workbench::generation_admission::GenerationAdmissionError::Closed {
                    phase: tidepool_runtime::session::SourceAdmissionPhase::Retired { .. }, ..
                }
            )) }));
        assert!(
            events.try_recv().is_err(),
            "retired source input is not automatically retried"
        );
    }
    assert!(matches!(
        slots.admit_delivery(independent),
        SourceDeliveryAdmission::Refused(_, _)
    ));
    assert!(matches!(
        refused_b[0].observation().state,
        SourceRegistrationState::Open { deferred: 2, .. }
    ));
    slots.clear();
    assert!(matches!(
        refused_b[0].observation().state,
        SourceRegistrationState::Closed { rejected: 2 }
    ));
    assert!(
        weak_b.upgrade().is_none(),
        "terminal clear releases B even while its capture is retained"
    );
    assert!(recipient.terminal().requested_shutdown().is_none());
    fence
        .reopen_with(source_a.original_source(), |reopen| {
            reopen.commit();
            Ok::<_, ()>(())
        })
        .unwrap()
        .unwrap();
    drop(slots);
    drop(captured_a);
    drop(behavior);
    address.stop(None);
    drop_address.stop(None);
    task.await.unwrap();
    drop_task.await.unwrap();
    owner.finish().await;
}
