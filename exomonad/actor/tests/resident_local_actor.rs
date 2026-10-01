//! The real resident MCP policy driven directly by the canonical local actor.

use exomonad_actor::{
    ActorDescriptor, ActorPlacement, ActorWorkbenchSource, LocalResidentDeployment, ResidentForest,
};
use exomonad_tool::{ToolArguments, ToolInvocation, ToolInvocationContext};
use tidepool_bridge::HaskellValue;
use tidepool_codegen::scope::ScopeId;
use tidepool_codegen::suspension::RealmId;
use tidepool_effect::dispatch::{DispatchEffect, EffectContext};
use tidepool_effect::error::EffectError;
use tidepool_effect::{EffectRunPolicy, LivePayloadPolicy, Response};
use tidepool_runtime::session::{
    insert_preamble_imports, resident_workbench_templates, run_turn, ModuleEnv, OutputSink,
    ResidentSession, SessionLib, TurnRequest as HaskellTurnRequest, TurnResult,
};
use tidepool_runtime::DEFAULT_NURSERY_SIZE;
use tidepool_testing::eval_harness;

use super::support;

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
        _request: &HaskellValue,
        _context: &EffectContext<'_, TestSink>,
    ) -> Result<Option<Response>, EffectError> {
        Ok(None)
    }

    fn prepare_dispatch(
        &mut self,
        _: &HaskellValue,
        _: &EffectContext<'_, TestSink>,
    ) -> Result<tidepool_effect::dispatch::EffectDispatch, EffectError> {
        Ok(tidepool_effect::dispatch::EffectDispatch::Unhandled)
    }
}

#[derive(Default)]
struct DelayedCommandBackend {
    started: tokio::sync::Notify,
    completed: tokio::sync::Notify,
    completion_count: std::sync::atomic::AtomicUsize,
    cancellation_count: std::sync::atomic::AtomicUsize,
    output: std::sync::Mutex<std::collections::HashMap<String, String>>,
}

impl DelayedCommandBackend {
    fn page(text: String) -> tidepool_bridge_effects::CommandPage {
        let end = i64::try_from(text.len()).expect("fixture output length");
        tidepool_bridge_effects::CommandPage {
            text,
            start: 0,
            end,
            available_end: end,
            retained_start: 0,
            lost_bytes: 0,
            finished: true,
            lossy: false,
            leading_fragment: false,
            trailing_fragment: false,
        }
    }
}

impl exomonad_actor::command_jobs::CommandBackend for DelayedCommandBackend {
    fn execute<'a>(
        &'a self,
        id: &'a str,
        spec: tidepool_bridge_effects::CommandSpec,
        phase: tokio::sync::watch::Sender<tidepool_bridge_effects::CommandStatus>,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = tidepool_bridge_effects::CommandResult> + Send + 'a>,
    > {
        Box::pin(async move {
            // The discard hold can prefix the probe script. Its explicit shell
            // argument identifies this fixture request without parsing script bytes.
            let is_probe = spec.argv.last().is_some_and(|arg| arg == "exomonad-source");
            let seconds = if is_probe {
                0
            } else {
                spec.argv
                    .last()
                    .and_then(|value| value.parse::<u64>().ok())
                    .unwrap_or(1)
            };
            phase.send_replace(tidepool_bridge_effects::CommandStatus::CommandRunning);
            if is_probe {
                self.output
                    .lock()
                    .expect("backend output lock")
                    .insert(id.to_owned(), "/test-workspace\n".into());
            } else {
                self.started.notify_one();
                tokio::time::sleep(std::time::Duration::from_secs(seconds)).await;
                self.output
                    .lock()
                    .expect("backend output lock")
                    .insert(id.to_owned(), String::new());
                self.completion_count
                    .fetch_add(1, std::sync::atomic::Ordering::AcqRel);
                self.completed.notify_one();
            }
            tidepool_bridge_effects::CommandResult {
                outcome: tidepool_bridge_effects::CommandOutcome::CommandExited(0),
                cleanup: tidepool_bridge_effects::CommandCleanup::CommandClean,
            }
        })
    }

    fn control<'a>(
        &'a self,
        _id: &'a str,
        operation: exomonad_actor::command_jobs::CommandControl,
    ) -> std::pin::Pin<
        Box<
            dyn std::future::Future<Output = Result<(), tidepool_bridge_effects::CommandError>>
                + Send
                + 'a,
        >,
    > {
        Box::pin(async move {
            if matches!(
                operation,
                exomonad_actor::command_jobs::CommandControl::Cancel
            ) {
                self.cancellation_count
                    .fetch_add(1, std::sync::atomic::Ordering::AcqRel);
            }
            Ok(())
        })
    }

    fn output<'a>(
        &'a self,
        id: &'a str,
        _bytes: usize,
    ) -> std::pin::Pin<
        Box<
            dyn std::future::Future<
                    Output = Result<
                        tidepool_bridge_effects::CommandOutput,
                        tidepool_bridge_effects::CommandError,
                    >,
                > + Send
                + 'a,
        >,
    > {
        Box::pin(async move {
            let text = self
                .output
                .lock()
                .expect("backend output lock")
                .get(id)
                .cloned()
                .unwrap_or_default();
            Ok(tidepool_bridge_effects::CommandOutput {
                stdout: Self::page(text),
                stderr: Self::page(String::new()),
            })
        })
    }

    fn read<'a>(
        &'a self,
        id: &'a str,
        stream: tidepool_bridge_effects::CommandStream,
        _position: tidepool_bridge_effects::CommandPosition,
    ) -> std::pin::Pin<
        Box<
            dyn std::future::Future<
                    Output = Result<
                        tidepool_bridge_effects::CommandPage,
                        tidepool_bridge_effects::CommandError,
                    >,
                > + Send
                + 'a,
        >,
    > {
        Box::pin(async move {
            let text = if stream == tidepool_bridge_effects::CommandStream::Stdout {
                self.output
                    .lock()
                    .expect("backend output lock")
                    .get(id)
                    .cloned()
                    .unwrap_or_default()
            } else {
                String::new()
            };
            Ok(Self::page(text))
        })
    }

    fn cleanup<'a>(
        &'a self,
        _id: &'a str,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = tidepool_bridge_effects::CommandCleanup> + Send + 'a>,
    > {
        Box::pin(async { tidepool_bridge_effects::CommandCleanup::CommandClean })
    }
}

async fn supply_command_until_started(
    deployments: &mut tokio::sync::mpsc::Receiver<LocalResidentDeployment>,
    backend: std::sync::Arc<DelayedCommandBackend>,
    owner: exomonad_actor::ActorRef,
) {
    let started = backend.started.notified();
    tokio::pin!(started);
    loop {
        tokio::select! {
            () = &mut started => return,
            deployment = deployments.recv() => match deployment {
                Some(LocalResidentDeployment::CommandBackend(request)) => {
                    assert_eq!(request.owner, owner);
                    let backend: std::sync::Arc<dyn exomonad_actor::command_jobs::CommandBackend> =
                        backend.clone();
                    request.supply(Ok(backend));
                }
                Some(LocalResidentDeployment::WatchChanged { notification }) => {
                    assert_eq!(notification.owner, owner);
                    assert_eq!(notification.current, exomonad_actor::WatchStateProjection::Ready);
                }
                Some(other) => panic!("unexpected deployment while awaiting command start: {}", other.kind()),
                None => panic!("resident forest closed before starting the command"),
            }
        }
    }
}

async fn wait_for_command_completions(backend: &DelayedCommandBackend, count: usize) {
    let result = tokio::time::timeout(std::time::Duration::from_secs(15), async {
        loop {
            if backend
                .completion_count
                .load(std::sync::atomic::Ordering::Acquire)
                >= count
            {
                return;
            }
            backend.completed.notified().await;
        }
    })
    .await;
    assert!(
        result.is_ok(),
        "expected {count} command completions, observed {} ({} cancellations)",
        backend
            .completion_count
            .load(std::sync::atomic::Ordering::Acquire),
        backend
            .cancellation_count
            .load(std::sync::atomic::Ordering::Acquire),
    );
}

#[tokio::test]
async fn local_actor_owns_resident_policy_children_and_terminal_reply() {
    resident_cleanup_case(false).await;
}

#[tokio::test]
async fn authored_failed_shutdown_hook_remains_unconfirmed_in_parent_cleanup() {
    resident_cleanup_case(true).await;
}

#[tokio::test]
async fn resident_local_actor_await_watch_parks_resumes_and_cancels() {
    resident_await_watch_case(WatchCase::PrimaryRoundTrip).await;
}

#[tokio::test]
async fn resident_structured_tool_await_watch_resumes() {
    resident_await_watch_case(WatchCase::StructuredRoundTrip).await;
}

#[tokio::test]
async fn resident_structured_tool_command_presentation_retains_job_and_resumes() {
    resident_await_watch_case(WatchCase::StructuredCommandPresentation).await;
}

#[tokio::test]
async fn resident_primary_await_watch_cancels_an_unpublished_cell() {
    resident_await_watch_case(WatchCase::PrimaryCancellation).await;
}

#[tokio::test]
async fn resident_primary_sleep_parks_and_cancels_the_same_owned_step() {
    resident_await_watch_case(WatchCase::PrimarySleepCancellation).await;
}

#[tokio::test]
async fn resident_primary_unbounded_command_observation_cancels_without_cancelling_job() {
    resident_await_watch_case(WatchCase::PrimaryCommandAwaitCancellation).await;
}

#[tokio::test]
async fn resident_primary_foreground_command_observation_cancels_without_cancelling_job() {
    resident_await_watch_case(WatchCase::PrimaryCommandForegroundCancellation).await;
}

#[tokio::test]
async fn resident_primary_command_notice_wait_cancels_before_notice_handoff() {
    resident_await_watch_case(WatchCase::PrimaryCommandNotifyCancellation).await;
}

enum WatchCase {
    PrimaryRoundTrip,
    StructuredRoundTrip,
    StructuredCommandPresentation,
    PrimaryCancellation,
    PrimarySleepCancellation,
    PrimaryCommandAwaitCancellation,
    PrimaryCommandForegroundCancellation,
    PrimaryCommandNotifyCancellation,
}

async fn resident_await_watch_case(case: WatchCase) {
    let structured_command = matches!(case, WatchCase::StructuredCommandPresentation);
    let primary = !matches!(
        case,
        WatchCase::StructuredRoundTrip | WatchCase::StructuredCommandPresentation
    );
    let command_observation = match case {
        WatchCase::PrimaryCommandAwaitCancellation => {
            Some("Cmd.observe (Cmd.Observation (-1) 0) job")
        }
        WatchCase::PrimaryCommandForegroundCancellation => Some("Cmd.await job"),
        WatchCase::PrimaryCommandNotifyCancellation => {
            Some("Cmd.observeCompletion (Cmd.Observation (-1) 0) job")
        }
        _ => None,
    };
    let cancel_first = command_observation.is_some()
        || matches!(
            case,
            WatchCase::PrimaryCancellation | WatchCase::PrimarySleepCancellation
        );
    let cancel_sleep = matches!(case, WatchCase::PrimarySleepCancellation);
    if std::env::var_os("TIDEPOOL_ACTOR_TEST_TRACE").is_some() {
        tracing_subscriber::fmt()
            .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
            .with_writer(std::io::stderr)
            .try_init()
            .expect("one test tracing subscriber");
    }
    eval_harness::require_extract();

    let session = support::process_unique_session(if command_observation.is_some() {
        184
    } else if cancel_sleep {
        183
    } else if cancel_first {
        182
    } else if primary {
        180
    } else {
        181
    });
    let declarations = [
        tidepool_mcp::agent_tools_decl(),
        tidepool_mcp::actor_decl(),
        tidepool_mcp::actor_kernel_decl(),
        tidepool_mcp::actor_local_decl(),
        tidepool_mcp::commands_decl(),
        tidepool_mcp::fs_read_decl(),
    ];
    let effects = tidepool_mcp::ensure_effects_module(&declarations).expect("actor effects");
    let mut include = effects.include_paths().to_vec();
    include.push(eval_harness::prelude_path());
    let preamble = insert_preamble_imports(
        &tidepool_mcp::build_preamble(&declarations, false),
        "Tidepool.Agent.Contract",
    );
    let preamble = insert_preamble_imports(&preamble, "qualified Tidepool.Agent.Watch as Watch");
    let preamble = insert_preamble_imports(&preamble, "Tidepool.Agent.Watch (Watches)");
    let preamble = format!(
        "{preamble}\
         type ActorEffects = '[AgentTools, Actor, Commands, Watch.Watches]\n\
         data WaitInput = WaitInput {{ delay :: Int }} deriving (Generic, FromJSON, JsonSchema)\n\
         data WaitOutput = WaitOutput {{ settled :: Bool }} deriving (Generic, ToJSON, JsonSchema)\n\
         data ResidentTools mode = ResidentTools {{ waitForCommand :: mode :- Call WaitInput WaitOutput }} deriving (Generic)\n"
    );
    let templates = resident_workbench_templates(&preamble, "ActorEffects", "");
    let include_refs: Vec<_> = include.iter().map(std::path::PathBuf::as_path).collect();
    let session_root = tempfile::tempdir().expect("session root");
    let lib = SessionLib::open(
        session,
        session_root.path(),
        ModuleEnv::standalone_default(),
    )
    .expect("declaration plane")
    .with_validation_include(include.clone());
    let mut machine =
        ResidentSession::unbootstrapped(NoHandlers, TestSink, DEFAULT_NURSERY_SIZE, Some(lib));
    let outcome = if primary {
        None
    } else {
        let retained = machine.prepared_retained();
        let compiled = match run_turn(HaskellTurnRequest {
            exact_context: None,
            session_id: None,
            turn_text: if structured_command {
                include_str!("resident_local_actor/await_command_policy.hs")
            } else {
                include_str!("resident_local_actor/await_watch_policy.hs")
            },
            templates: &templates,
            include: &include_refs,
            session_root: session_root.path(),
            inject_modules: &[],
            gen: 1,
            verdict: None,
            target: None,
            retained_imports: &retained,
        })
        .expect("compile awaitWatch policy")
        {
            TurnResult::Expr { compiled, .. } => compiled,
            other => panic!("policy should be an expression, got {other:?}"),
        };
        machine.set_effect_execution(
            EffectRunPolicy::SuspendAll,
            LivePayloadPolicy::HASKELL_EFFECT_VALUE,
        );
        Some(
            machine
                .run_with_sites("resident_await_watch_policy", compiled.code())
                .expect("first policy boundary"),
        )
    };
    let descriptor = ActorDescriptor::new(
        "resident-await-watch",
        ActorPlacement {
            session,
            resource_scope: RealmId::fresh(),
            lexical_scope: ScopeId::ROOT,
        },
    );
    let (forest, mut deployments) = ResidentForest::new(
        ActorWorkbenchSource::new(preamble, include),
        session,
        machine,
        None,
        exomonad_actor::Incarnation::FIRST,
    );
    let (actor, task, policy): (
        _,
        _,
        std::sync::Arc<dyn exomonad_actor::ResidentToolEndpoint>,
    ) = if primary {
        let actor = forest
            .new_workbench(
                "resident-await-watch".into(),
                exomonad_actor::EffectiveRole::root().with_effect_keys(vec![
                    exomonad_actor::ActorEffectKey::Commands,
                    exomonad_actor::ActorEffectKey::Watches,
                ]),
            )
            .await
            .expect("spawn primary workbench");
        let policy = std::sync::Arc::new(exomonad_actor::ResidentInteractivePolicy::local(
            actor.clone(),
        ));
        (actor, None, policy)
    } else {
        let (actor, task) = forest
            .admit_root(descriptor, outcome.expect("structured policy boundary"))
            .await
            .expect("spawn local resident root");
        let LocalResidentDeployment::PolicyInstalled(installation) =
            deployments.recv().await.expect("policy installation")
        else {
            panic!("root retired before installing policy");
        };
        assert_eq!(installation.actor.identity(), actor.identity());
        (actor, Some(task), installation.policy)
    };
    let command_backend = std::sync::Arc::new(DelayedCommandBackend::default());
    let settled = if cancel_first {
        None
    } else {
        let settled_context = ToolInvocationContext::external(
            "await-watch-test".into(),
            "turn-settled".into(),
            "call-settled".into(),
            Some("await-watch-settled".into()),
            None,
        );
        let mut settled_call = {
            let policy = policy.clone();
            let context = settled_context.clone();
            tokio::spawn(async move {
                policy
                    .dispatch_boxed(ToolInvocation {
                        context: Some(context),
                        name: if primary {
                            exomonad_actor::HASKELL_TOOL
                        } else {
                            "wait_for_command"
                        }
                        .into(),
                        arguments: if primary {
                            ToolArguments::Raw(
                                include_str!("resident_local_actor/await_watch_cell.hs")
                                    .replace("DELAY", "1"),
                            )
                        } else {
                            ToolArguments::Structured(serde_json::json!({"delay": 1}))
                        },
                    })
                    .await
            })
        };
        tokio::time::timeout(std::time::Duration::from_secs(180), async {
        tokio::select! {
            () = supply_command_until_started(&mut deployments, command_backend.clone(), actor.identity()) => {},
            reply = &mut settled_call => panic!("watch cell settled before starting its command: {reply:?}"),
        }
    }).await.expect("command start is bounded");
        if primary {
            tokio::time::timeout(std::time::Duration::from_secs(2), async {
                while actor.hosted_cell_computing() {
                    assert!(
                        !settled_call.is_finished(),
                        "the cell must reach its captured watch"
                    );
                    tokio::task::yield_now().await;
                }
            })
            .await
            .expect("the owned watch parks promptly after command admission");
        }
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(100), &mut settled_call)
                .await
                .is_err(),
            "awaitWatch must keep the hosted workbench parked while its exact command is live"
        );
        if primary {
            tokio::time::timeout(std::time::Duration::from_secs(2), async {
                while !actor.hosted_cell_computing() && !settled_call.is_finished() {
                    tokio::task::yield_now().await;
                }
            })
            .await
            .expect("ready watch resumes promptly into the same cell");
        }
        // A primary cell renders its deferred observation through GHC after the
        // watch resumes; the structured tool already owns its JSON result.
        let settled = tokio::time::timeout(
            std::time::Duration::from_secs(if primary { 600 } else { 5 }),
            settled_call,
        )
        .await
        .expect("watch settles")
        .expect("watch call task")
        .expect("watch tool call");
        if primary {
            assert_eq!(settled["status"], "committed", "{settled:?}");
            let operations = settled["items"][0]["operations"]
                .as_array()
                .expect("operation receipts");
            assert_eq!(operations.len(), 4, "{settled:?}");
            for (ordinal, operation) in operations.iter().enumerate() {
                assert_eq!(operation["id"]["effectOrdinal"], ordinal, "{settled:?}");
                assert_eq!(operation["id"]["inputUnitIndex"], 0, "{settled:?}");
                assert_eq!(operation["disposition"], "committed", "{settled:?}");
            }
            assert_eq!(operations[2]["effect"], "awaitWatch", "{settled:?}");
        } else {
            assert_eq!(settled, serde_json::json!({"settled": true}));
            forest.shutdown().await;
            task.expect("structured actor task")
                .await
                .expect("actor task");
            return;
        }
        Some(settled)
    };

    let cancelled_context = ToolInvocationContext::external(
        "await-watch-test".into(),
        "turn-cancelled".into(),
        "call-cancelled".into(),
        Some("await-watch-cancelled".into()),
        None,
    );
    let mut cancelled_call = {
        let policy = policy.clone();
        let context = cancelled_context.clone();
        tokio::spawn(async move {
            policy
                .dispatch_boxed(ToolInvocation {
                    context: Some(context),
                    name: exomonad_actor::HASKELL_TOOL.into(),
                    arguments: ToolArguments::Raw(if cancel_sleep {
                        include_str!("resident_local_actor/await_sleep_cell.hs").into()
                    } else if let Some(observation) = command_observation {
                        include_str!("resident_local_actor/await_command_cell.hs")
                            .replace("OBSERVATION", observation)
                    } else {
                        include_str!("resident_local_actor/await_watch_cell.hs")
                            .replace("DELAY", "3")
                    }),
                })
                .await
        })
    };
    if cancel_sleep {
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            while !actor.hosted_cell_computing() {
                assert!(
                    !cancelled_call.is_finished(),
                    "sleep must admit its hosted control"
                );
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("hosted sleep admission is prompt");
        tokio::time::timeout(std::time::Duration::from_secs(180), async {
            while actor.hosted_cell_computing() {
                assert!(
                    !cancelled_call.is_finished(),
                    "sleep must reach its captured wait"
                );
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("captured owned sleep parks after compiler preparation");
        if let Ok(reply) =
            tokio::time::timeout(std::time::Duration::from_millis(100), &mut cancelled_call).await
        {
            panic!("captured sleep settled before cancellation: {reply:?}");
        }
    } else {
        let cancellation_start_bound = 180;
        tokio::time::timeout(std::time::Duration::from_secs(cancellation_start_bound), async {
        tokio::select! {
            () = supply_command_until_started(&mut deployments, command_backend.clone(), actor.identity()) => {},
            reply = &mut cancelled_call => panic!("cancellable watch cell settled before starting its command: {reply:?}"),
        }
    }).await.expect("cancellable command start is bounded");
    }
    let cancellation = tokio::time::timeout(std::time::Duration::from_secs(2), async {
        loop {
            match policy
                .cancel_workbench_boxed(cancelled_context.clone())
                .await
                .expect("cancel awaitWatch")
            {
                exomonad_actor::WorkbenchCancellationOutcome::NotSleeping { .. } => {
                    tokio::task::yield_now().await;
                }
                outcome => break outcome,
            }
        }
    })
    .await
    .expect("awaitWatch cancellation is prompt");
    let retained_reply = match cancellation {
        exomonad_actor::WorkbenchCancellationOutcome::Cancelled { reply, .. } => reply,
        other => panic!("exact native continuation was not cancelled: {other:?}"),
    };
    let returned_reply =
        tokio::time::timeout(std::time::Duration::from_secs(2), &mut cancelled_call)
            .await
            .expect("cancelled workbench settles")
            .expect("cancelled workbench task");
    match (retained_reply, returned_reply) {
        (Ok(retained), Ok(returned)) => {
            assert_eq!(serde_json::to_value(retained).unwrap(), returned)
        }
        (Err(retained), Err(exomonad_actor::ResidentToolError::Invocation(returned))) => {
            assert_eq!(retained, returned)
        }
        other => panic!("cancellation and original terminal reply differ: {other:?}"),
    }
    if !cancel_sleep {
        if command_observation.is_some() {
            assert_eq!(
                command_backend
                    .completion_count
                    .load(std::sync::atomic::Ordering::Acquire),
                0,
                "the observation cancels before the independent job finishes"
            );
        }
        wait_for_command_completions(&command_backend, if cancel_first { 1 } else { 2 }).await;
        if command_observation.is_some() {
            assert_eq!(
                command_backend
                    .cancellation_count
                    .load(std::sync::atomic::Ordering::Acquire),
                0,
                "cancelling the cell's observation does not cancel the command job"
            );
        }
    }

    forest.shutdown().await;
    assert_eq!(
        actor.terminal().wait().await.kind,
        exomonad_actor::ActorExitKind::Cancelled
    );
    loop {
        match deployments
            .recv()
            .await
            .expect("actor retirement is published")
        {
            LocalResidentDeployment::WatchChanged { notification } => {
                assert_eq!(notification.owner, actor.identity());
                assert_eq!(
                    notification.current,
                    exomonad_actor::WatchStateProjection::Ready
                );
            }
            LocalResidentDeployment::Retired { actor: retired, .. } => {
                assert_eq!(retired, actor.identity());
                break;
            }
            other => panic!("unexpected final deployment: {}", other.kind()),
        }
    }
    if let Some(settled) = settled {
        assert_eq!(settled["items"][0]["output"], "True", "{settled:?}");
    }
}

async fn resident_cleanup_case(fail_hook: bool) {
    eval_harness::require_extract();

    let session = support::process_unique_session(if fail_hook { 178 } else { 177 });
    let declarations = [
        tidepool_mcp::agent_tools_decl(),
        tidepool_mcp::actor_decl(),
        tidepool_mcp::actor_kernel_decl(),
        tidepool_mcp::actor_local_decl(),
        tidepool_mcp::fs_read_decl(),
    ];
    let effects = tidepool_mcp::ensure_effects_module(&declarations).expect("actor effects");
    let mut include = effects.include_paths().to_vec();
    include.push(eval_harness::prelude_path());
    let preamble = insert_preamble_imports(
        &tidepool_mcp::build_preamble(&declarations, false),
        "Tidepool.Agent.Contract",
    );
    let preamble = format!(
        "{preamble}\
         type ActorEffects = '[AgentTools, Actor]\n\
         data EchoInput = EchoInput {{ value :: Int }} deriving (Generic, FromJSON, JsonSchema)\n\
         data EchoOutput = EchoOutput {{ doubled :: Int }} deriving (Generic, ToJSON, JsonSchema)\n\
         data SpawnInput = SpawnInput {{ seed :: Int }} deriving (Generic, FromJSON, JsonSchema)\n\
         data SpawnOutput = SpawnOutput {{ started :: Bool }} deriving (Generic, ToJSON, JsonSchema)\n\
         data StateInput = StateInput {{ next :: Int }} deriving (Generic, FromJSON, JsonSchema)\n\
         data StateQuery = StateQuery deriving (Generic, FromJSON, JsonSchema)\n\
         data FinishInput = FinishInput {{ confirm :: Bool }} deriving (Generic, FromJSON, JsonSchema)\n\
         data StateOutput = StateOutput {{ current :: Int }} deriving (Generic, ToJSON, JsonSchema)\n\
         data ResidentTools mode = ResidentTools {{ doubleValue :: mode :- Call EchoInput EchoOutput, spawnChild :: mode :- Call SpawnInput SpawnOutput, currentValue :: mode :- Call StateQuery StateOutput, setValue :: mode :- Update StateInput StateOutput, finishValue :: mode :- Finish FinishInput StateOutput }} deriving (Generic)\n"
    );
    let templates = resident_workbench_templates(&preamble, "ActorEffects", "");
    let include_refs: Vec<_> = include.iter().map(std::path::PathBuf::as_path).collect();
    let session_root = tempfile::tempdir().expect("session root");
    let lib = SessionLib::open(
        session,
        session_root.path(),
        ModuleEnv::standalone_default(),
    )
    .expect("declaration plane")
    .with_validation_include(include.clone());
    let mut machine =
        ResidentSession::unbootstrapped(NoHandlers, TestSink, DEFAULT_NURSERY_SIZE, Some(lib));
    let retained = machine.prepared_retained();
    let compiled = match run_turn(HaskellTurnRequest {
        exact_context: None,
        session_id: None,
        turn_text: if fail_hook {
            include_str!("resident_local_actor/policy_failed_hook.hs")
        } else {
            include_str!("resident_local_actor/policy.hs")
        },
        templates: &templates,
        include: &include_refs,
        session_root: session_root.path(),
        inject_modules: &[],
        gen: 1,
        verdict: None,
        target: None,
        retained_imports: &retained,
    })
    .expect("compile resident policy")
    {
        TurnResult::Expr { compiled, .. } => compiled,
        other => panic!("policy should be an expression, got {other:?}"),
    };
    machine.set_effect_execution(
        EffectRunPolicy::SuspendAll,
        LivePayloadPolicy::HASKELL_EFFECT_VALUE,
    );
    let outcome = machine
        .run_with_sites("resident_local_policy", compiled.code())
        .expect("first policy boundary");
    let descriptor = ActorDescriptor::new(
        "resident-local-policy",
        ActorPlacement {
            session,
            resource_scope: RealmId::fresh(),
            lexical_scope: ScopeId::ROOT,
        },
    );
    let sibling_scope = machine.mint_isolated_scope();
    let sibling_realm = RealmId::fresh();
    machine
        .set_actor_execution(
            tidepool_runtime::session::SessionRunContext {
                resource_scope: sibling_realm,
                lexical_scope: sibling_scope,
                ..tidepool_runtime::session::SessionRunContext::ROOT
            },
            EffectRunPolicy::SuspendAll,
            LivePayloadPolicy::HASKELL_EFFECT_VALUE,
        )
        .expect("independent root scope");
    let sibling_outcome = machine
        .run_with_sites("resident_sibling_policy", compiled.code())
        .expect("sibling policy boundary");
    let sibling_descriptor = ActorDescriptor::new(
        "sibling",
        ActorPlacement {
            session,
            resource_scope: sibling_realm,
            lexical_scope: sibling_scope,
        },
    );
    let (forest, mut deployments) = ResidentForest::new(
        ActorWorkbenchSource::new(preamble, include),
        session,
        machine,
        None,
        exomonad_actor::Incarnation::FIRST,
    );
    for invalid in [
        ActorDescriptor::new(
            "foreign",
            ActorPlacement {
                session: support::process_unique_session(179),
                ..descriptor.placement()
            },
        ),
        descriptor
            .clone()
            .with_supervisor_parent(exomonad_actor::ActorRef {
                id: exomonad_actor::ActorId(1),
                incarnation: exomonad_actor::Incarnation::FIRST,
            }),
        descriptor
            .clone()
            .with_context_parent(exomonad_actor::ActorRef {
                id: exomonad_actor::ActorId(1),
                incarnation: exomonad_actor::Incarnation::FIRST,
            }),
    ] {
        assert!(forest
            .admit_root(
                invalid,
                tidepool_runtime::session::ResidentOutcome::BindingsCommitted {
                    output: Vec::new()
                },
            )
            .await
            .is_err());
    }
    let (actor, task) = forest
        .admit_root(descriptor, outcome)
        .await
        .expect("spawn local resident root");

    let LocalResidentDeployment::PolicyInstalled(installation) =
        deployments.recv().await.expect("policy installation")
    else {
        panic!("root retired before installing policy");
    };
    assert_eq!(installation.actor.identity(), actor.identity());
    let policy = installation.policy;
    let (sibling, sibling_task) = forest
        .admit_root(sibling_descriptor, sibling_outcome)
        .await
        .expect("admit independent sibling");
    let LocalResidentDeployment::PolicyInstalled(sibling_installation) = deployments
        .recv()
        .await
        .expect("sibling policy installation")
    else {
        panic!("sibling retired before installation")
    };
    assert_eq!(sibling_installation.actor.identity(), sibling.identity());
    assert_eq!(sibling_installation.supervisor_parent, None);
    assert_eq!(sibling_installation.context_parent, None);

    let changed = sibling_installation
        .policy
        .dispatch_boxed(ToolInvocation {
            context: None,
            name: "set_value".into(),
            arguments: ToolArguments::Structured(serde_json::json!({"next": 73})),
        })
        .await
        .expect("change sibling state");
    assert_eq!(changed, serde_json::json!({"current": 73}));

    // Malformed state transitions and completion calls must refuse without
    // changing state or terminating the actor. The next valid call still runs.
    for (name, arguments) in [
        ("double_value", serde_json::json!({"value": "bad"})),
        ("set_value", serde_json::json!({"next": "bad"})),
        ("finish_value", serde_json::json!({"confirm": "bad"})),
    ] {
        let error = policy
            .dispatch_boxed(ToolInvocation {
                context: None,
                name: name.into(),
                arguments: ToolArguments::Structured(arguments),
            })
            .await
            .expect_err("malformed tool input is a refusal");
        assert!(
            matches!(
                error,
                exomonad_actor::ResidentToolError::Invocation(
                    exomonad_actor::KernelInvocationFailure::Rejected { .. }
                )
            ),
            "{error}"
        );
    }
    let unchanged = policy
        .dispatch_boxed(ToolInvocation {
            context: None,
            name: "current_value".into(),
            arguments: ToolArguments::Structured(serde_json::json!({})),
        })
        .await
        .expect("state remains available after refused transitions");
    assert_eq!(unchanged, serde_json::json!({"current": 0}));

    let doubled = policy
        .dispatch_boxed(ToolInvocation {
            context: None,
            name: "double_value".into(),
            arguments: ToolArguments::Structured(serde_json::json!({"value": 6})),
        })
        .await
        .expect("double value");
    assert_eq!(doubled, serde_json::json!({"doubled": 12}));

    let spawned = policy
        .dispatch_boxed(ToolInvocation {
            context: None,
            name: "spawn_child".into(),
            arguments: ToolArguments::Structured(serde_json::json!({"seed": 19})),
        })
        .await
        .expect("spawn and await child");
    // Q2-B (aad9f184b): a failed shutdown hook never rewrites the exit kind
    // the actor itself requested by completing normally. The child's own
    // program ran to completion regardless of `fail_hook`, so `awaitExit`
    // sees `Completed` both times and the hook's failure surfaces only as
    // this actor's own (parent's) cleanup confirmation below, never as the
    // child's reported exit kind.
    assert_eq!(spawned, serde_json::json!({"started": true}));
    let child_retired = deployments.recv().await.expect("child retirement");
    assert!(matches!(
        child_retired,
        LocalResidentDeployment::Retired { ref terminal, .. }
            if terminal.kind == exomonad_actor::ActorExitKind::Completed
    ));
    let finished = policy
        .dispatch_boxed(ToolInvocation {
            context: None,
            name: "finish_value".into(),
            arguments: ToolArguments::Structured(serde_json::json!({"confirm": true})),
        })
        .await
        .expect("finish value");
    assert_eq!(finished, serde_json::json!({"current": 0}));
    task.await.expect("root actor task");
    let cleanup = actor.terminal().cleanup().expect("retained cleanup");
    assert_eq!(cleanup.actor(), actor.identity());
    assert_eq!(cleanup.is_confirmed(), !fail_hook, "{cleanup:?}");
    assert!(matches!(
        cleanup.realm(),
        exomonad_actor::CleanupComponentOutcome::Confirmed
    ));
    if fail_hook {
        assert!(matches!(
            cleanup.children(),
            exomonad_actor::CleanupComponentOutcome::Unconfirmed(_)
        ));
    }

    assert_eq!(
        actor.terminal().wait().await.kind,
        exomonad_actor::ActorExitKind::Completed
    );
    assert!(matches!(
        deployments.recv().await,
        Some(LocalResidentDeployment::Retired { actor: retired, .. })
            if retired == actor.identity()
    ));
    // External host composition can construct the canonical projection, but a
    // terminal actor cannot silently retarget its independently live sibling.
    let canonical = exomonad_actor::ResidentInteractivePolicy::local(actor.clone());
    let error = exomonad_actor::ResidentToolEndpoint::seal_hosted_work_boxed(&canonical)
        .await
        .expect_err("terminal canonical projection remains terminal");
    assert!(matches!(error,
        exomonad_actor::ResidentToolError::Invocation(
            exomonad_actor::KernelInvocationFailure::ActorExited(identity))
                if identity == actor.identity()));

    // The first tree's retirement must preserve the sibling's live closures.
    let retained = sibling_installation
        .policy
        .dispatch_boxed(ToolInvocation {
            context: None,
            name: "current_value".into(),
            arguments: ToolArguments::Structured(serde_json::json!({})),
        })
        .await
        .expect("sibling survives root retirement");
    assert_eq!(retained, serde_json::json!({"current": 73}));
    sibling_installation
        .policy
        .dispatch_boxed(ToolInvocation {
            context: None,
            name: "finish_value".into(),
            arguments: ToolArguments::Structured(serde_json::json!({"confirm": true})),
        })
        .await
        .expect("retire sibling");
    sibling_task.await.expect("sibling task");
    assert_eq!(
        sibling.terminal().wait().await.kind,
        exomonad_actor::ActorExitKind::Completed
    );
}
