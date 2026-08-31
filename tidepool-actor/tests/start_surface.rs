//! Public `startActor` proof through the shared resident runner. Its internal
//! sealing step derives an exact source facade from compiler provenance;
//! startup then runs one typed model/Haskell deliberation, publishes readiness,
//! and resumes the parent with the exact actor incarnation.

use std::sync::Arc;

use parking_lot::Mutex;
use tidepool_actor::{
    ActorDescriptor, ActorEvent, ActorExitKind, ActorLifecycle, ActorMachineRegistry,
    ActorPlacement, ActorRegistry, ActorTerminal, ActorTurnKind, ActorWorkbenchSource,
    OutboundSettlement, ResidentActorLifecycle, ResidentActorMailbox, ResidentActorRunner,
    ResidentActorStarter, ResidentCallPoll, ResidentCompletionExecutor, ResidentWaitPoll,
    StartInitiator,
};
use tidepool_codegen::scope::ScopeId;
use tidepool_codegen::suspension::RealmId;
use tidepool_effect::dispatch::{DispatchEffect, EffectContext};
use tidepool_effect::error::EffectError;
use tidepool_effect::{EffectRunPolicy, LivePayloadPolicy, Response};
use tidepool_eval::Value;
use tidepool_model::{ModelProvider, ProviderError, StreamSink, TurnRequest, TurnResponse, Usage};
use tidepool_repr::SessionId;
use tidepool_runtime::session::{
    resident_workbench_templates, run_turn, ModuleEnv, OutputSink, ResidentOutcome,
    ResidentSession, SessionLib, TurnRequest as HaskellTurnRequest, TurnResult,
};
use tidepool_runtime::DEFAULT_NURSERY_SIZE;
use tidepool_testing::eval_harness;

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

struct ApprovesStartup {
    requests: Mutex<Vec<TurnRequest>>,
}

impl ModelProvider for ApprovesStartup {
    async fn complete(
        &self,
        request: TurnRequest,
        _sink: Option<StreamSink>,
    ) -> Result<TurnResponse, ProviderError> {
        let mut requests = self.requests.lock();
        let ordinal = requests.len();
        requests.push(request);
        drop(requests);
        Ok(TurnResponse {
            text: match ordinal {
                0 => "```haskell\ncomplete True\n```".into(),
                1 => "```haskell\ncomplete (1 :: Int)\n```".into(),
                _ => panic!("startup requested an unexpected third model session"),
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

    let session_id = SessionId(93);
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
    preamble.push_str("type ActorEffects = '[Actor]\n");
    let templates = resident_workbench_templates(&preamble, "ActorEffects", "");
    let include_refs: Vec<_> = include.iter().map(std::path::PathBuf::as_path).collect();
    let root = tempfile::tempdir().expect("session root");
    let source = r#"
let idleDefinition :: ActorDefinition Int Maybe Int
    idleDefinition =
      ActorDefinition
        { label = "idle-worker"
        , effectProfile = ReadWrite
        , initialization = \seed -> pure seed
        , behavior = \_seed initial ->
            (pure initial :: Eff (ReadWriteEffects Maybe) Int)
        , visibleToChild = []
        , onShutdown = \reason -> case reason of
            ShutdownCompleted -> deliberate "completed shutdown must not deliberate" ()
            _ -> pure ()
        }

    workerDefinition :: ActorDefinition Int Maybe Int
    workerDefinition =
      ActorDefinition
        { label = "worker"
        , effectProfile = ReadOnly
        , initialization = \seed -> do
            approved <- deliberate "Approve the supplied seed." seed
            adjustment <- deliberate "Choose the adjustment." seed
            pure (approved, adjustment)
        , behavior = \seed (approved, adjustment) ->
            (pure (if approved then seed + adjustment else seed - adjustment)
              :: Eff (ReadOnlyEffects Maybe) Int)
        , visibleToChild = []
        , onShutdown = const (pure ())
        }

    serverDefinition :: ActorDefinition Int ((,) Int) Int
    serverDefinition =
      ActorDefinition
        { label = "server"
        , effectProfile = ReadOnly
        , initialization = \seed -> pure seed
        , behavior = \_seed initial ->
            (serve initial (\state (delta, result) -> pure (result, state + delta))
              :: Eff (ReadOnlyEffects ((,) Int)) Int)
        , visibleToChild = []
        , onShutdown = \reason -> case reason of
            ShutdownCancelled -> deliberate "shutdown must not deliberate" ()
            _ -> pure ()
        }

    jobDefinition :: ActorDefinition Int ((,) Int) Int
    jobDefinition =
      ActorDefinition
        { label = "job"
        , effectProfile = ReadOnly
        , initialization = \seed -> pure seed
        , behavior = \_seed initial ->
            (receive (\(delta, result) -> pure (result, initial + delta))
              :: Eff (ReadOnlyEffects ((,) Int)) Int)
        , visibleToChild = []
        , onShutdown = const (pure ())
        }

in do
    _ <- startActor idleDefinition 10
    _ <- startActor workerDefinition 41
    server <- startActor serverDefinition 0
    cast server (1, ())
    serverAnswer <- call server (2, 41)
    job <- startActor jobDefinition 10
    jobAnswer <- call job (5, 42)
    jobExit <- awaitExit job
    case jobExit of
      Completed value -> pure (serverAnswer, jobAnswer, value)
      _ -> pure (-1, -1, -1)
"#;
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
    let start_outcome = machine
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
    let parent_start = registry
        .begin_start(
            None,
            ActorDescriptor::new(
                "parent",
                ["Actor"],
                ActorPlacement {
                    session: session_id,
                    resource_scope: RealmId::fresh(),
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

    let start = runner
        .capture_start(parent_context, start_outcome)
        .await
        .expect("capture and seal child entry");
    let completions =
        ResidentCompletionExecutor::new(Arc::clone(&machines), workbench_source.clone());
    let starter = ResidentActorStarter::new(registry.clone(), runner, completions);
    let provider = ApprovesStartup {
        requests: Mutex::new(Vec::new()),
    };

    let (parent_turn, idle_child, parent_outcome) = starter
        .start(parent_turn, &provider, start, None)
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
    assert_eq!(provider.requests.lock().len(), 0);
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
    assert_eq!(provider.requests.lock().len(), 2);
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
    assert_eq!(provider.requests.lock().len(), 2);

    let mailbox_runner = ResidentActorRunner::new(Arc::clone(&machines), workbench_source.clone());
    let mailbox = ResidentActorMailbox::new(registry.clone(), mailbox_runner);
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
            assert_eq!(result.to_json(), serde_json::json!([41, 42, 15]));
        }
        ResidentOutcome::Suspended { .. } => panic!("parent should complete after typed job exit"),
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
            .contains("shutdown suspended on disallowed `Tidepool.Effects.Core.DeliberateWith`"),
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
