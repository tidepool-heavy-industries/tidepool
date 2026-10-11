use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use tidepool_bridge::{BridgeError, FromHaskell, HaskellValue};
use tidepool_codegen::scope::ScopeId;
use tidepool_codegen::suspension::RealmId;
use tidepool_effect::dispatch::{DispatchEffect, EffectContext, EffectDispatch, Response};
use tidepool_effect::error::EffectError;
use tidepool_effect::{EffectRunPolicy, LivePayloadPolicy};
use tidepool_mcp::CapturedOutput;
use tidepool_runtime::session::{
    fresh_session_id, ModuleEnv, ParsedBlock, PublicManifestCommit, PublicationDecision,
    PublicationPhase, RecoveryPublicOwner, ResidentSession, SessionLib,
};
use tidepool_testing::effect_surface::TestEffectSurface;

use crate::resident_workbench::{
    ActorWorkbenchSource, ExecutionPrivateScope, PreparedCell, PrivateExecutionPublication,
    ResidentActorWorkbench, ResidentWorkbenchStep,
};
use crate::{ActorSessionContext, ResidentActorRunner};

use super::{ActorMachineRegistry, WorkbenchCompilationAuthority, WorkbenchPublicOwner};

#[derive(Default)]
struct PublicationGate {
    attempts: AtomicUsize,
    reached: tokio::sync::Notify,
    release: tokio::sync::Notify,
}

tokio::task_local! {
    static GATE: Arc<PublicationGate>;
}

pub(in crate::resident_actor) async fn before_publication_attempt() {
    let Ok(gate) = GATE.try_with(Arc::clone) else {
        return;
    };
    if gate.attempts.fetch_add(1, Ordering::SeqCst) < 2 {
        gate.reached.notify_one();
        gate.release.notified().await;
    }
}

struct CountingConsole(Arc<AtomicUsize>);

impl DispatchEffect<CapturedOutput> for CountingConsole {
    fn dispatch(
        &mut self,
        request: &HaskellValue,
        cx: &EffectContext<'_, CapturedOutput>,
    ) -> Result<Option<Response>, EffectError> {
        match crate::generated::console::ConsoleReq::from_value(request, cx.table()) {
            Ok(crate::generated::console::ConsoleReq::Print(text)) => {
                self.0.fetch_add(1, Ordering::SeqCst);
                cx.user().push(text);
                cx.respond(()).map(Some)
            }
            Ok(_) | Err(BridgeError::UnknownDataCon(_)) => Ok(None),
            Err(error) => Err(EffectError::Bridge(error)),
        }
    }

    fn prepare_dispatch(
        &mut self,
        request: &HaskellValue,
        cx: &EffectContext<'_, CapturedOutput>,
    ) -> Result<EffectDispatch, EffectError> {
        self.dispatch(request, cx).map(|response| match response {
            Some(response) => EffectDispatch::Immediate(response),
            None => EffectDispatch::Unhandled,
        })
    }
}

async fn admit(
    runner: &ResidentActorRunner<CountingConsole, CapturedOutput>,
    public: ActorSessionContext,
    owner: Arc<WorkbenchPublicOwner>,
) -> (
    ResidentActorWorkbench<CountingConsole, CapturedOutput>,
    ActorSessionContext,
    Arc<ExecutionPrivateScope>,
) {
    let (mut context, authority) =
        WorkbenchCompilationAuthority::admit(public, Default::default(), None, None)
            .unwrap_or_else(|error| panic!("owner admits compilation: {error:?}"));
    let execution = Arc::new(
        runner
            .begin_private_execution(context.clone(), owner, PublicationDecision::new())
            .await
            .unwrap(),
    );
    context.placement.lexical_scope = execution.private_scope;
    let workbench = runner
        .application_workbench()
        .with_compilation_authority(authority)
        .with_private_execution(Arc::clone(&execution));
    (workbench, context, execution)
}

async fn execute(
    workbench: &ResidentActorWorkbench<CountingConsole, CapturedOutput>,
    context: ActorSessionContext,
    source: &str,
) -> Vec<String> {
    let (checked, prepared) = workbench
        .prepare_cell(context.clone(), source.into())
        .await
        .unwrap();
    let PreparedCell {
        items,
        dependencies,
    } = prepared;
    assert!(!items.is_empty());
    assert_eq!(checked.items.len(), items.len());
    let mut outputs = Vec::new();
    for (index, (observed, item)) in checked.items.iter().zip(items).enumerate() {
        let step = workbench
            .begin_prepared_cell_item(
                context.clone(),
                ParsedBlock {
                    ordinal: index + 1,
                    total: checked.items.len(),
                    source: observed.source.clone(),
                },
                item,
            )
            .await
            .unwrap();
        let settled = match step {
            ResidentWorkbenchStep::Running { fragment, outcome } => workbench
                .settle_item(context.clone(), *fragment, (*outcome).into())
                .await
                .unwrap(),
            other => other,
        };
        let ResidentWorkbenchStep::Committed { output, .. } = settled else {
            panic!("checked item must commit");
        };
        outputs.push(output);
    }
    drop(dependencies);
    outputs
}

#[tokio::test]
async fn semantic_and_metadata_publication_retries_do_not_repeat_executed_effect() {
    let mut compiler = crate::CompilerPreparationOwner::new();
    let cleanup = compiler.cleanup();
    let result = futures_util::FutureExt::catch_unwind(std::panic::AssertUnwindSafe(
        compiler.scope(counted_publication_history()),
    ))
    .await;
    assert!(cleanup
        .wait_for_settlement(Duration::from_secs(5))
        .await
        .is_confirmed());
    if let Err(panic) = result {
        std::panic::resume_unwind(panic);
    }
}

async fn counted_publication_history() {
    tidepool_testing::eval_harness::require_extract();
    let surface = TestEffectSurface::minimal(&[tidepool_mcp::console_decl()]).unwrap();
    let source = ActorWorkbenchSource::new(surface.preamble(), surface.include_paths().to_vec());
    let root = tempfile::tempdir().unwrap();
    let recovery = crate::recovery::recovery_test_fixture::RecoveryFixture::create(root.path());
    let session_id = fresh_session_id();
    let mut lib = SessionLib::open(
        session_id,
        root.path().join("machine"),
        ModuleEnv::standalone_default(),
    )
    .unwrap()
    .with_validation_include(surface.include_paths().to_vec());
    tidepool_testing::with_settlement(|settlement| recovery.attach_root(&mut lib, settlement));
    let count = Arc::new(AtomicUsize::new(0));
    let mut session = ResidentSession::unbootstrapped(
        CountingConsole(Arc::clone(&count)),
        CapturedOutput::new(),
        tidepool_runtime::DEFAULT_NURSERY_SIZE,
        Some(lib),
    );
    let mut contexts = Vec::new();
    let mut owners = Vec::new();
    let mut durable_owners = Vec::new();
    for (id, name) in [(1, "counted"), (2, "unrelated")] {
        let context = ActorSessionContext {
            actor: crate::ActorRef::first(crate::ActorId(id)),
            placement: crate::ActorPlacement {
                session: session_id,
                lexical_scope: session.mint_scope(ScopeId::ROOT).unwrap(),
                resource_scope: RealmId::fresh(),
            },
            effect_policy: EffectRunPolicy::HandleOrSuspend,
            live_payload: LivePayloadPolicy::HASKELL_EFFECT_VALUE,
            source_imports: Default::default(),
            haskell_effects_alias: surface.row().into(),
            source_layer: Arc::from([]),
        };
        let path = tidepool_repr::ActorPath::parse(&format!("root/{name}")).unwrap();
        let durable = RecoveryPublicOwner::new(&path, context.actor.incarnation.0).unwrap();
        session
            .initialize_durable_public_scope(durable.clone(), context.placement.lexical_scope)
            .unwrap();
        let descriptor = crate::ActorDescriptor::new(name, context.placement)
            .with_actor_path(path)
            .with_persistence_policy(crate::ActorPersistencePolicy::Durable);
        let readiness = session
            .durable_public_readiness(&durable, context.placement.lexical_scope)
            .unwrap();
        owners.push(WorkbenchPublicOwner::issue(&context, &descriptor, Some(readiness)).unwrap());
        contexts.push(context);
        durable_owners.push(durable);
    }
    let machines = Arc::new(ActorMachineRegistry::new());
    machines.insert_idle(session_id, Box::new(session));
    let runner = ResidentActorRunner::new(Arc::clone(&machines), source);
    let (counted, private, execution) =
        admit(&runner, contexts[0].clone(), Arc::clone(&owners[0])).await;
    let outputs = execute(
        &counted,
        private.clone(),
        "data PublishedTag = PublishedTag\ncountedValue <- send (Print \"counted once\") >> pure (41 :: Int)",
    )
    .await;
    assert_eq!(count.load(Ordering::SeqCst), 1);
    assert!(outputs.iter().any(|output| output.contains("counted once")));
    let counted_identity = {
        let mut checkout = machines.checkout_run(session_id).unwrap();
        let identity = checkout
            .machine()
            .current_binding_in(private.placement.lexical_scope, "countedValue")
            .unwrap()
            .0;
        checkout.restore_suspended(Vec::new());
        identity
    };

    let gate = Arc::new(PublicationGate::default());
    let timing = crate::call_timing::CallScope::new("counted-publication", 1, 1);
    let publishing = GATE.scope(
        Arc::clone(&gate),
        timing.run(runner.publish_private_execution(private, Arc::clone(&execution))),
    );
    tokio::pin!(publishing);
    let mut unrelated_confirmed = None;
    for round in 0..2 {
        tokio::select! {
            _ = &mut publishing => panic!("publication skipped retry barrier"),
            _ = gate.reached.notified() => {}
        }
        assert_eq!(count.load(Ordering::SeqCst), 1);
        assert_eq!(timing.compile_count(), round + 1);
        let index = round as usize;
        let (competitor, context, competing_execution) =
            admit(&runner, contexts[index].clone(), Arc::clone(&owners[index])).await;
        let cell = if round == 0 {
            "data CompetitorTag = CompetitorTag\ncompetitorValue <- pure (42 :: Int)"
        } else {
            "unrelatedValue <- pure (43 :: Int)"
        };
        let _ = execute(&competitor, context.clone(), cell).await;
        assert!(matches!(
            runner
                .publish_private_execution(context, competing_execution)
                .await
                .unwrap(),
            PrivateExecutionPublication::Manifest {
                commit: PublicManifestCommit::Durable,
                ..
            }
        ));
        if round == 1 {
            unrelated_confirmed = recovery.public_owner(&durable_owners[1]);
        }
        gate.release.notify_one();
    }
    let result = tokio::time::timeout(Duration::from_secs(60), &mut publishing)
        .await
        .expect("publication drains when competing writes stop")
        .unwrap();
    assert!(matches!(
        result,
        PrivateExecutionPublication::Manifest {
            commit: PublicManifestCommit::Durable,
            ref native_bindings,
        } if native_bindings == &["countedValue"]
    ));
    assert_eq!(gate.attempts.load(Ordering::SeqCst), 3);
    assert_eq!(
        timing.compile_count(),
        2,
        "metadata retry retains certification"
    );
    assert_eq!(execution.decision.phase(), PublicationPhase::Published);
    assert_eq!(count.load(Ordering::SeqCst), 1);

    let mut checkout = machines.checkout_run(session_id).unwrap();
    let session = checkout.machine();
    let public = contexts[0].placement.lexical_scope;
    assert_eq!(
        session
            .current_binding_in(public, "countedValue")
            .unwrap()
            .0,
        counted_identity,
        "publication installs the original executed binding"
    );
    assert!(session
        .current_binding_in(public, "competitorValue")
        .is_some());
    let declarations = session.current_decl_heads_in(public);
    for name in ["PublishedTag", "CompetitorTag"] {
        assert!(declarations.iter().any(|(found, _)| found == name));
    }
    let unrelated = contexts[1].placement.lexical_scope;
    assert!(session
        .current_binding_in(unrelated, "unrelatedValue")
        .is_some());
    assert!(session
        .current_binding_in(public, "unrelatedValue")
        .is_none());
    for durable in &durable_owners {
        assert!(recovery.public_owner(durable).is_some());
    }
    assert_eq!(
        recovery.public_owner(&durable_owners[1]),
        unrelated_confirmed
    );
    checkout.restore_suspended(Vec::new());
}
