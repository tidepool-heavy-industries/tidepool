//! Public `startActor` proof through the shared resident runner. Its internal
//! sealing step derives an exact source facade from compiler provenance;
//! startup then runs one typed model/Haskell deliberation, publishes readiness,
//! and resumes the parent with the exact actor incarnation.

use std::sync::Arc;

use parking_lot::Mutex;
use tidepool_actor::{
    ActorAgentSession, ActorDescriptor, ActorEvent, ActorExitKind, ActorLifecycle,
    ActorMachineRegistry, ActorPlacement, ActorRegistry, ActorTerminal, ActorTurnKind,
    ActorWorkbenchSource, OutboundSettlement, ResidentActorLifecycle, ResidentActorMailbox,
    ResidentActorRunner, ResidentActorStarter, ResidentCallPoll, ResidentCompletionExecutor,
    ResidentWaitPoll, StartInitiator,
};
use tidepool_codegen::scope::ScopeId;
use tidepool_codegen::suspension::RealmId;
use tidepool_effect::dispatch::{DispatchEffect, EffectContext};
use tidepool_effect::error::EffectError;
use tidepool_effect::{EffectRunPolicy, LivePayloadPolicy, Response};
use tidepool_eval::Value;
use tidepool_model::{ModelProvider, ProviderError, StreamSink, TurnRequest, TurnResponse, Usage};
use tidepool_runtime::session::{
    resident_workbench_templates, run_turn, ModuleEnv, OutputSink, ResidentOutcome,
    ResidentSession, SessionLib, TurnRequest as HaskellTurnRequest, TurnResult,
};
use tidepool_runtime::DEFAULT_NURSERY_SIZE;
use tidepool_testing::eval_harness;

mod support;

#[derive(Clone, Default)]
struct TestSink;

impl OutputSink for TestSink {
    fn drain(&self) -> Vec<String> {
        Vec::new()
    }

    fn snapshot(&self) -> Vec<String> {
        Vec::new()
    }
}

struct NoHandlers;

impl DispatchEffect<TestSink> for NoHandlers {
    fn dispatch(
        &mut self,
        request: &Value,
        context: &EffectContext<'_, TestSink>,
    ) -> Result<Option<Response>, EffectError> {
        let _ = (request, context);
        Ok(None)
    }
}

struct AuthorsActorAndApprovesStartup {
    requests: Mutex<Vec<TurnRequest>>,
}

impl ModelProvider for AuthorsActorAndApprovesStartup {
    async fn complete(
        &self,
        request: TurnRequest,
        _sink: Option<StreamSink>,
    ) -> Result<TurnResponse, ProviderError> {
        let mut requests = self.requests.lock();
        let ordinal = requests.len();
        requests.push(request.clone());
        drop(requests);
        Ok(TurnResponse {
            text: match ordinal {
                0 => include_str!("start_surface/child_startup_response.hs")
                    .trim_end()
                    .into(),
                1 => "```haskell\ncomplete (DynamicPolicy 40)\n```".into(),
                2 => "```haskell\ncomplete True\n```".into(),
                3 => "```haskell\ncomplete (1 :: Int)\n```".into(),
                _ => panic!(
                    "startup requested an unexpected fifth model session: {:#?}",
                    request.messages
                ),
            },
            usage: Usage::default(),
            reasoning: None,
            reasoning_items: Vec::new(),
        })
    }
}

#[tokio::test]
async fn public_start_uses_one_exact_resident_path() {
    eval_harness::require_extract();

    let session_id = support::process_unique_session(93);
    let decls = [
        tidepool_mcp::actor_decl(),
        tidepool_mcp::actor_kernel_decl(),
        tidepool_mcp::actor_local_decl(),
        tidepool_mcp::deliberate_decl(),
    ];
    let effects = tidepool_mcp::ensure_effects_module(&decls).expect("materialize effects");
    let mut include = effects.include_paths().to_vec();
    include.push(eval_harness::prelude_path());
    let mut preamble = tidepool_mcp::build_preamble(&decls, false);
    preamble.push_str("type ActorEffects = '[Actor, Deliberate]\n");
    let templates = resident_workbench_templates(&preamble, "ActorEffects", "");
    let include_refs: Vec<_> = include.iter().map(std::path::PathBuf::as_path).collect();
    let root = tempfile::tempdir().expect("session root");
    let source = include_str!("start_surface/parent_program.hs");
    let compiled = match run_turn(HaskellTurnRequest {
        turn_text: source,
        templates: &templates,
        include: &include_refs,
        session_root: root.path(),
        inject_modules: &[],
        gen: 1,
        verdict: None,
        target: None,
    })
    .expect("compile public actor program")
    {
        TurnResult::Expr { compiled, .. } => compiled,
        other => panic!("actor program should compile as an expression, got {other:?}"),
    };

    let lib = SessionLib::open(session_id, root.path(), ModuleEnv::standalone_default())
        .expect("open declaration plane")
        .with_validation_include(include.clone());
    let mut machine = ResidentSession::bootstrap(
        &compiled.expr,
        compiled.table.clone(),
        NoHandlers,
        TestSink,
        include.clone(),
        DEFAULT_NURSERY_SIZE,
        Some(lib),
    )
    .expect("bootstrap resident machine");
    machine.set_effect_execution(
        EffectRunPolicy::SuspendAll,
        LivePayloadPolicy::HASKELL_EFFECT_VALUE,
    );
    let parent_outcome = machine
        .run_with_sites(
            "start_parent",
            &compiled.expr,
            &compiled.table,
            &compiled.asks,
        )
        .expect("run parent to start suspension");

    let machines = Arc::new(ActorMachineRegistry::new());
    assert!(machines.insert_idle(session_id, machine).is_none());
    let registry = ActorRegistry::new();
    let parent_realm = RealmId::fresh();
    let parent_start = registry
        .begin_start(
            None,
            ActorDescriptor::new(
                "parent",
                ["Actor", "Deliberate"],
                ActorPlacement {
                    session: session_id,
                    resource_scope: parent_realm,
                    lexical_scope: ScopeId::ROOT,
                },
            ),
            StartInitiator::Runtime,
        )
        .expect("begin parent");
    let parent = registry
        .publish_ready(parent_start)
        .expect("publish parent");
    let parent_turn = registry
        .begin_turn(parent, ActorTurnKind::Haskell)
        .expect("admit parent Haskell turn");
    let parent_context = parent_turn.session_context();
    let workbench_source = ActorWorkbenchSource::new(preamble, include);
    let runner = ResidentActorRunner::new(Arc::clone(&machines), workbench_source.clone());
    let provider = AuthorsActorAndApprovesStartup {
        requests: Mutex::new(Vec::new()),
    };
    let completion = runner
        .capture_completion(parent_context, parent_outcome, parent_realm)
        .await
        .expect("capture model-authored definition obligation");
    let agent = ActorAgentSession::attach(registry.clone(), parent).expect("attach parent model");
    let completions =
        ResidentCompletionExecutor::new(Arc::clone(&machines), workbench_source.clone());
    let (parent_turn, parent_outcome) = completions
        .resolve(&agent, parent_turn, &provider, completion, None)
        .await
        .expect("author a live typed actor definition");
    let dynamic_start = runner
        .capture_start(parent_turn.session_context(), parent_outcome)
        .await
        .expect("seal the model-authored definition");
    let starter = ResidentActorStarter::new(
        registry.clone(),
        runner,
        ResidentCompletionExecutor::new(Arc::clone(&machines), workbench_source.clone()),
    );

    let (parent_turn, dynamic_child, parent_outcome) = starter
        .start(parent_turn, &provider, dynamic_start, None)
        .await
        .expect("start model-authored actor");
    assert_eq!(
        registry
            .descriptor(dynamic_child)
            .expect("dynamic child descriptor")
            .label(),
        "model-authored"
    );
    assert_eq!(registry.lifecycle(dynamic_child), Ok(ActorLifecycle::Ready));
    assert_eq!(provider.requests.lock().len(), 2);

    let mailbox = ResidentActorMailbox::new(
        registry.clone(),
        ResidentActorRunner::new(Arc::clone(&machines), workbench_source.clone()),
    );
    let pending = match mailbox
        .submit_outbound(parent_turn, parent_outcome)
        .await
        .expect("submit model-authored GADT call")
    {
        OutboundSettlement::Pending(call) => call,
        OutboundSettlement::Continued { .. } => panic!("dynamic call must park for its reply"),
    };
    assert!(mailbox
        .dispatch_one(dynamic_child)
        .await
        .expect("dispatch model-authored GADT call"));
    assert_eq!(
        registry.lifecycle(dynamic_child),
        Ok(ActorLifecycle::Exited)
    );
    let (parent_turn, parent_outcome) = match mailbox
        .poll_call(pending)
        .await
        .expect("resume dynamic reply")
    {
        ResidentCallPoll::Continued { turn, outcome } => (turn, outcome),
        ResidentCallPoll::Pending(_) => panic!("dynamic call must be settled"),
    };
    let wait = mailbox
        .submit_wait(parent_turn, parent_outcome)
        .await
        .expect("submit model-authored child wait");
    let (parent_turn, parent_outcome) = match mailbox
        .poll_wait(wait)
        .await
        .expect("resume model-authored child exit")
    {
        ResidentWaitPoll::Continued { turn, outcome } => (turn, outcome),
        ResidentWaitPoll::Pending(_) => {
            panic!("completed model-authored child wait must settle immediately")
        }
    };

    let idle_start = ResidentActorRunner::new(Arc::clone(&machines), workbench_source.clone())
        .capture_start(parent_turn.session_context(), parent_outcome)
        .await
        .expect("capture checked-in idle child entry");

    let (parent_turn, idle_child, parent_outcome) = starter
        .start(parent_turn, &provider, idle_start, None)
        .await
        .expect("start sealed actor");
    assert_eq!(parent_turn.kind(), ActorTurnKind::Haskell);
    assert_eq!(
        registry
            .descriptor(idle_child)
            .expect("child descriptor")
            .label(),
        "idle-worker"
    );
    assert_eq!(
        registry
            .descriptor(idle_child)
            .expect("child descriptor")
            .profile(),
        tidepool_actor::ActorEffectProfile::ReadWrite
    );
    assert_eq!(
        registry
            .descriptor(idle_child)
            .expect("child descriptor")
            .effect_names(),
        [
            "ActorKernel",
            "FsWrite",
            "ActorLocal",
            "Actor",
            "Deliberate",
            "FsRead"
        ]
    );
    assert_eq!(provider.requests.lock().len(), 2);
    assert_eq!(registry.lifecycle(idle_child), Ok(ActorLifecycle::Exited));

    let capture_runner = ResidentActorRunner::new(Arc::clone(&machines), workbench_source.clone());
    let next_start = capture_runner
        .capture_start(parent_turn.session_context(), parent_outcome)
        .await
        .expect("capture second child entry");
    let (parent_turn, prompted_child, parent_outcome) = starter
        .start(parent_turn, &provider, next_start, None)
        .await
        .expect("start actor with two prompted sessions");
    assert_eq!(
        registry
            .descriptor(prompted_child)
            .expect("prompted child descriptor")
            .label(),
        "worker"
    );
    assert_eq!(provider.requests.lock().len(), 4);
    assert_eq!(
        registry.lifecycle(prompted_child),
        Ok(ActorLifecycle::Exited)
    );

    let capture_runner = ResidentActorRunner::new(Arc::clone(&machines), workbench_source.clone());
    let server_start = capture_runner
        .capture_start(parent_turn.session_context(), parent_outcome)
        .await
        .expect("capture server entry");
    let (parent_turn, server, parent_outcome) = starter
        .start(parent_turn, &provider, server_start, None)
        .await
        .expect("start mailbox server");
    assert_eq!(
        registry
            .descriptor(server)
            .expect("server descriptor")
            .label(),
        "server"
    );
    assert_eq!(registry.lifecycle(server), Ok(ActorLifecycle::Ready));
    assert_eq!(provider.requests.lock().len(), 4);

    let (parent_turn, parent_outcome) = match mailbox
        .submit_outbound(parent_turn, parent_outcome)
        .await
        .expect("submit cast")
    {
        OutboundSettlement::Continued { turn, outcome } => (turn, outcome),
        OutboundSettlement::Pending(_) => panic!("cast must continue after acceptance"),
    };
    assert!(mailbox.dispatch_one(server).await.expect("dispatch cast"));
    let pending = match mailbox
        .submit_outbound(parent_turn, parent_outcome)
        .await
        .expect("submit call")
    {
        OutboundSettlement::Pending(call) => call,
        OutboundSettlement::Continued { .. } => panic!("call must park for its reply"),
    };
    assert!(mailbox.dispatch_one(server).await.expect("dispatch call"));
    let (parent_turn, parent_outcome) = match mailbox
        .poll_call(pending)
        .await
        .expect("resume replied call")
    {
        ResidentCallPoll::Continued { turn, outcome } => (turn, outcome),
        ResidentCallPoll::Pending(_) => panic!("dispatched call must be settled"),
    };

    let job_start = capture_runner
        .capture_start(parent_turn.session_context(), parent_outcome)
        .await
        .expect("capture one-shot job entry");
    let (parent_turn, job, parent_outcome) = starter
        .start(parent_turn, &provider, job_start, None)
        .await
        .expect("start one-shot mailbox job");
    let pending = match mailbox
        .submit_outbound(parent_turn, parent_outcome)
        .await
        .expect("submit job call")
    {
        OutboundSettlement::Pending(call) => call,
        OutboundSettlement::Continued { .. } => panic!("job call must park for its reply"),
    };
    assert!(mailbox.dispatch_one(job).await.expect("dispatch job call"));
    assert_eq!(registry.lifecycle(job), Ok(ActorLifecycle::Exited));
    let (parent_turn, parent_outcome) =
        match mailbox.poll_call(pending).await.expect("resume job reply") {
            ResidentCallPoll::Continued { turn, outcome } => (turn, outcome),
            ResidentCallPoll::Pending(_) => panic!("job call must be settled"),
        };
    let wait = mailbox
        .submit_wait(parent_turn, parent_outcome)
        .await
        .expect("submit typed job wait");
    let (parent_turn, parent_outcome) = match mailbox
        .poll_wait(wait)
        .await
        .expect("resume typed job exit")
    {
        ResidentWaitPoll::Continued { turn, outcome } => (turn, outcome),
        ResidentWaitPoll::Pending(_) => panic!("completed job wait must settle immediately"),
    };
    drop(parent_turn);
    match parent_outcome {
        ResidentOutcome::Completed { result, .. } => {
            assert_eq!(result.to_json(), serde_json::json!([83, 41, 42, 15]));
        }
        ResidentOutcome::Suspended { .. } => {
            panic!("parent should complete after the typed job exit")
        }
    }

    assert_eq!(
        machines.peek(session_id, |machine| machine.parked_holes().len()),
        Some(1),
        "the live server owns the sole remaining parked continuation"
    );
    let lifecycle = ResidentActorLifecycle::new(
        registry.clone(),
        ResidentActorRunner::new(Arc::clone(&machines), workbench_source),
    );
    let shutdown_error = lifecycle
        .force_terminate(
            parent,
            ActorTerminal {
                kind: ActorExitKind::Completed,
                summary: "parent completed".into(),
            },
        )
        .await
        .expect_err("disallowed shutdown deliberation must be reported");
    assert!(
        shutdown_error
            .to_string()
            .contains("shutdown suspended on disallowed `DeliberateWith`"),
        "unexpected shutdown error: {shutdown_error}"
    );
    assert_eq!(
        machines.peek(session_id, |machine| machine.parked_holes().len()),
        Some(0),
        "subtree cleanup closes the server's resident realm"
    );
    assert_eq!(
        registry
            .events()
            .into_iter()
            .filter(|record| matches!(record.event, ActorEvent::ShutdownHookFailed { .. }))
            .count(),
        2,
        "both forbidden shutdown sessions remain visible in the neutral event stream"
    );
}
