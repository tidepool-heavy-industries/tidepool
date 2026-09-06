use crate as tidepool_actor;
use std::{sync::Arc, time::Duration};
use tidepool_actor::{
    ActorExitKind, ActorTerminal, ActorWorkbenchSource, EffectiveRole, Incarnation,
    LocalResidentDeployment, ResidentForest, ResidentToolEndpoint,
};
use tidepool_effect::{EffectRunPolicy, LivePayloadPolicy};
use tidepool_runtime::session::{
    insert_preamble_imports, resident_workbench_templates, run_turn, TurnRequest, TurnResult,
};
use tidepool_runtime::session::{ModuleEnv, ResidentSession, SessionLib};
use tidepool_testing::eval_harness;
#[derive(Clone, Default)]
struct TestSink;
impl tidepool_runtime::session::OutputSink for TestSink {
    fn drain(&self) -> Vec<String> {
        Vec::new()
    }
    fn snapshot(&self) -> Vec<String> {
        Vec::new()
    }
}
struct NoHandlers;
impl tidepool_effect::dispatch::DispatchEffect<TestSink> for NoHandlers {
    fn dispatch(
        &mut self,
        _: &tidepool_eval::Value,
        _: &tidepool_effect::dispatch::EffectContext<'_, TestSink>,
    ) -> Result<Option<tidepool_effect::Response>, tidepool_effect::error::EffectError> {
        Ok(None)
    }
}

#[tokio::test]
async fn authored_seal_survives_lost_waiter_and_rejects_late_work() {
    eval_harness::require_extract();
    let declarations = [tidepool_mcp::notifications_decl()];
    let effects = tidepool_mcp::ensure_effects_module(&declarations).unwrap();
    let mut include = effects.include_paths().to_vec();
    include.push(eval_harness::prelude_path());
    let preamble = format!(
        "{}\ntype ActorEffects = '[Notifications, Replies, Watches]\n",
        insert_preamble_imports(
            &tidepool_mcp::build_preamble(&declarations, false),
            "Tidepool.Effects.Core (Notifications(..))"
        )
    );
    let preamble = insert_preamble_imports(&preamble, "Tidepool.Agent.Reply (Replies)");
    let preamble = insert_preamble_imports(&preamble, "Tidepool.Agent.Watch (Watches)");
    let templates = resident_workbench_templates(&preamble, "ActorEffects", "");
    let include_refs: Vec<_> = include.iter().map(std::path::PathBuf::as_path).collect();
    let root = tempfile::tempdir().unwrap();
    let compile = |text: &str, gen| match run_turn(TurnRequest {
        turn_text: text,
        templates: &templates,
        include: &include_refs,
        session_root: root.path(),
        inject_modules: &[],
        gen,
        verdict: None,
        target: None,
    })
    .unwrap()
    {
        TurnResult::Expr { compiled, .. } => Arc::new(compiled),
        _ => panic!("expected compiled program"),
    };
    let boot = compile("pure (0 :: Int)", 1);
    let session = tidepool_repr::SessionId((u64::from(std::process::id()) << 32) | 205);
    let lib = SessionLib::open(session, root.path(), ModuleEnv::standalone_default())
        .unwrap()
        .with_validation_include(include.clone());
    let mut machine = ResidentSession::bootstrap(
        &boot.expr,
        boot.table.clone(),
        NoHandlers,
        TestSink,
        include.clone(),
        tidepool_runtime::DEFAULT_NURSERY_SIZE,
        Some(lib),
    )
    .unwrap();
    machine.set_effect_execution(
        EffectRunPolicy::SuspendAll,
        LivePayloadPolicy::HASKELL_EFFECT_VALUE,
    );
    let (forest, mut events) = ResidentForest::new(
        ActorWorkbenchSource::new(preamble.clone(), include.clone()),
        session,
        machine,
        None,
        Incarnation::FIRST,
    );

    let actor = forest
        .new_workbench("sealed-workbench".into(), EffectiveRole::coding())
        .await
        .unwrap();
    let policy: Arc<dyn ResidentToolEndpoint> =
        Arc::new(super::ResidentInteractivePolicy::local(actor.clone()));
    let sibling = forest
        .new_workbench("sibling".into(), EffectiveRole::coding())
        .await
        .unwrap();
    let sibling_policy: Arc<dyn ResidentToolEndpoint> =
        Arc::new(super::ResidentInteractivePolicy::local(sibling.clone()));
    let address = actor.identity();
    let source = format!(
        "send (NotifyWith ({}, {}) \"hold\")",
        address.id.0, address.incarnation.0
    );
    let mut first = tokio::spawn(policy.dispatch_boxed(invocation(&source, "first")));
    let held = tokio::time::timeout(Duration::from_secs(60), async {
        loop {
            tokio::select! {
                result = &mut first => panic!("authored invocation completed before controlled handoff: {result:?}"),
                event = events.recv() => if let LocalResidentDeployment::NotificationSend(command) = event.unwrap() { break command; }
            }
        }
    })
    .await
    .unwrap();
    first.abort();
    let _ = first.await;
    let mut seal = policy.seal_hosted_work_boxed();
    assert!(
        tokio::time::timeout(Duration::from_millis(25), &mut seal)
            .await
            .is_err(),
        "seal cannot pass active actor-owned work after HTTP waiter loss"
    );
    let late = tokio::spawn(policy.dispatch_boxed(invocation("pure (99 :: Int)", "late")));
    held.rejected(tidepool_actor::NotificationError::Unavailable);
    let proof = tokio::time::timeout(Duration::from_secs(60), seal)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(proof.actor(), actor.identity());
    assert!(late.await.unwrap().is_err());
    assert!(policy.reattach_boxed().await.is_err());
    assert_eq!(policy.seal_hosted_work_boxed().await.unwrap(), proof);
    policy
        .complete_boxed(tidepool_runtime::session::WorkbenchForkBoundary {
            thread_id: "seal-test".into(),
            call_id: "first".into(),
        })
        .await
        .unwrap();
    let sibling_proof = sibling_policy.seal_hosted_work_boxed().await.unwrap();
    assert_ne!(proof.actor(), sibling_proof.actor());
    let requested = ActorTerminal {
        kind: ActorExitKind::Cancelled,
        summary: "test shutdown".into(),
    };
    let stopped = actor
        .shutdown_with_cleanup(requested.clone())
        .await
        .unwrap();
    assert_eq!(stopped.cleanup.actor(), actor.identity());
    assert!(stopped.cleanup.is_confirmed(), "{stopped:?}");
    assert_eq!(
        actor
            .shutdown_with_cleanup(requested.clone())
            .await
            .unwrap(),
        stopped
    );
    assert!(policy.seal_hosted_work_boxed().await.is_err());
    assert!(sibling
        .shutdown_with_cleanup(requested)
        .await
        .unwrap()
        .cleanup
        .is_confirmed());
    forest.shutdown().await;
}

fn invocation(source: &str, id: &str) -> tidepool_tool::ToolInvocation {
    tidepool_tool::ToolInvocation {
        name: tidepool_actor::HASKELL_TOOL.into(),
        arguments: tidepool_tool::ToolArguments::Raw(source.into()),
        context: Some(tidepool_tool::ToolInvocationContext {
            context_call_id: Some(id.into()),
            thread_id: "seal-test".into(),
            turn_id: id.into(),
            call_id: id.into(),
            namespace: Some("haskell".into()),
        }),
    }
}
