use crate as tidepool_actor;
use std::{sync::Arc, time::Duration};
use tidepool_actor::{
    ActorExitKind, ActorTerminal, ActorWorkbenchSource, EffectiveRole, Incarnation,
    LocalResidentDeployment, ResidentForest, ResidentToolEndpoint, WorkbenchCancellationOutcome,
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
    let retiring = forest
        .new_workbench("lost-retirement-waiter".into(), EffectiveRole::coding())
        .await
        .unwrap();
    let retiring_policy = super::ResidentInteractivePolicy::local(retiring.clone());
    let source = format!(
        "send (NotifyWith ({}, {}) \"retire-hold\")",
        retiring.identity().id.0,
        retiring.identity().incarnation.0
    );
    let running = tokio::spawn(retiring_policy.dispatch_boxed(invocation(&source, "retire-first")));
    let held = tokio::time::timeout(Duration::from_secs(60), async {
        loop {
            if let LocalResidentDeployment::NotificationSend(command) = events.recv().await.unwrap()
            {
                break command;
            }
        }
    })
    .await
    .unwrap();
    let owner = retiring.clone();
    let mut waiter = tokio::spawn(async move {
        owner
            .shutdown_with_cleanup(ActorTerminal {
                kind: ActorExitKind::Cancelled,
                summary: "lost waiter".into(),
            })
            .await
    });
    assert!(tokio::time::timeout(Duration::from_millis(25), &mut waiter)
        .await
        .is_err());
    waiter.abort();
    let _ = waiter.await;
    held.rejected(tidepool_actor::NotificationError::Unavailable);
    running.await.unwrap().unwrap();
    let terminal = tokio::time::timeout(Duration::from_secs(60), retiring.terminal().wait())
        .await
        .unwrap();
    let retained = retiring
        .terminal()
        .cleanup()
        .expect("cleanup survives lost retirement waiter");
    assert!(retained.is_confirmed(), "{retained:?}");
    let observed = retiring
        .shutdown_with_cleanup(terminal.clone())
        .await
        .unwrap();
    assert_eq!(observed.terminal, terminal);
    assert_eq!(observed.cleanup, retained);
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

#[tokio::test]
async fn resident_sleep_waits_fifteen_minutes_without_blocking_a_sibling() {
    eval_harness::require_extract();
    let declarations = [tidepool_mcp::sleep_decl()];
    let effects = tidepool_mcp::ensure_effects_module(&declarations).unwrap();
    let mut include = effects.include_paths().to_vec();
    include.push(eval_harness::prelude_path());
    let preamble = format!(
        "{}\ntype ActorEffects = '[Sleep, Replies, Watches]\n",
        tidepool_mcp::build_preamble(&declarations, false)
    );
    let preamble = insert_preamble_imports(&preamble, "Tidepool.Agent.Reply (Replies)");
    let preamble = insert_preamble_imports(&preamble, "Tidepool.Agent.Watch (Watches)");
    let templates = resident_workbench_templates(&preamble, "ActorEffects", "");
    let include_refs: Vec<_> = include.iter().map(std::path::PathBuf::as_path).collect();
    let root = tempfile::tempdir().unwrap();
    let boot = match run_turn(TurnRequest {
        turn_text: "pure (0 :: Int)",
        templates: &templates,
        include: &include_refs,
        session_root: root.path(),
        inject_modules: &[],
        gen: 1,
        verdict: None,
        target: None,
    })
    .unwrap()
    {
        TurnResult::Expr { compiled, .. } => Arc::new(compiled),
        _ => panic!("expected compiled program"),
    };
    let session = tidepool_repr::SessionId((u64::from(std::process::id()) << 32) | 206);
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
    let (forest, _events) = ResidentForest::new(
        ActorWorkbenchSource::new(preamble, include),
        session,
        machine,
        None,
        Incarnation::FIRST,
    );
    let sleeper = forest
        .new_workbench("sleeper".into(), EffectiveRole::coding())
        .await
        .unwrap();
    let sibling = forest
        .new_workbench("sibling".into(), EffectiveRole::coding())
        .await
        .unwrap();
    let sleeper_policy: Arc<dyn ResidentToolEndpoint> =
        Arc::new(super::ResidentInteractivePolicy::local(sleeper));
    let sibling_policy: Arc<dyn ResidentToolEndpoint> =
        Arc::new(super::ResidentInteractivePolicy::local(sibling));

    tokio::time::pause();
    let sleeping = tokio::spawn(sleeper_policy.dispatch_boxed(invocation(
        "sleep (minutes 15)\npure (7 :: Int)",
        "fifteen-minute-sleep",
    )));
    tokio::task::yield_now().await;

    let sibling_result = sibling_policy
        .dispatch_boxed(invocation("pure (3 :: Int)", "sibling-progress"))
        .await
        .unwrap();
    assert_eq!(
        sibling_result["status"], "committed",
        "sibling result: {sibling_result}"
    );
    assert!(!sleeping.is_finished());

    tokio::time::advance(Duration::from_secs(899)).await;
    tokio::task::yield_now().await;
    assert!(
        !sleeping.is_finished(),
        "sleep completed before its monotonic deadline"
    );
    tokio::time::advance(Duration::from_secs(1)).await;
    let sleep_result = sleeping.await.unwrap().unwrap();
    assert_eq!(sleep_result["status"], "committed");
    assert_eq!(sleep_result["items"].as_array().unwrap().len(), 2);

    let cancel_invocation = invocation(
        "sleep (minutes 15)\nafterCancelled <- pure (99 :: Int)",
        "cancelled-sleep",
    );
    let cancel_context = cancel_invocation.context.clone().unwrap();
    let cancelled = {
        let sleeper_policy = Arc::clone(&sleeper_policy);
        tokio::spawn(async move { sleeper_policy.dispatch_boxed(cancel_invocation).await })
    };
    let cancellation = loop {
        tokio::task::yield_now().await;
        match sleeper_policy
            .cancel_workbench_boxed(cancel_context.clone())
            .await
            .unwrap()
        {
            WorkbenchCancellationOutcome::UnknownEvaluation { .. }
            | WorkbenchCancellationOutcome::NotSleeping { .. } => continue,
            outcome => break outcome,
        }
    };
    assert!(
        matches!(
            cancellation,
            WorkbenchCancellationOutcome::Cancelled { .. }
        ),
        "cancellation outcome: {cancellation:?}"
    );
    let cancelled_result = cancelled.await.unwrap();
    assert!(
        cancelled_result.is_err(),
        "an interrupted evaluation must not report committed"
    );

    let next = sleeper_policy
        .dispatch_boxed(invocation("pure (11 :: Int)", "after-cancellation"))
        .await
        .unwrap();
    assert_eq!(next["status"], "committed");
    let suffix = sleeper_policy
        .dispatch_boxed(invocation("afterCancelled", "cancelled-suffix"))
        .await;
    match suffix {
        Ok(response) => assert_eq!(
            response["status"], "rejected",
            "the interrupted suffix must not install bindings: {response}"
        ),
        Err(_) => {}
    }
    forest.shutdown().await;
}

struct UnsupportedEndpoint;
impl ResidentToolEndpoint for UnsupportedEndpoint {
    fn tools(&self) -> &[tidepool_tool::HostedTool] {
        &[]
    }
    fn instructions(&self) -> Option<&str> {
        None
    }
    fn dispatch_boxed(&self, _: tidepool_tool::ToolInvocation) -> crate::ResidentToolFuture {
        Box::pin(async {
            Err(crate::ResidentToolError::Unavailable(
                "unsupported test endpoint".into(),
            ))
        })
    }
}
#[tokio::test]
async fn unsupported_endpoint_does_not_fabricate_seal() {
    assert!(matches!(
        UnsupportedEndpoint.seal_hosted_work_boxed().await,
        Err(crate::ResidentToolError::Unavailable(_))
    ));
}
