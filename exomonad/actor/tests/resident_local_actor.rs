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
    ResidentSession, SessionLib, TurnRequest as HaskellTurnRequest, TurnResult, WorkbenchRequest,
};
use tidepool_runtime::DEFAULT_NURSERY_SIZE;
use tidepool_testing::eval_harness;

use super::support;

trait ResidentToolEndpointTestProjection {
    fn dispatch_json_boxed(&self, invocation: ToolInvocation)
        -> exomonad_actor::ResidentToolFuture;
}

impl<T: exomonad_actor::ResidentToolEndpoint + ?Sized> ResidentToolEndpointTestProjection for T {
    fn dispatch_json_boxed(
        &self,
        invocation: ToolInvocation,
    ) -> exomonad_actor::ResidentToolFuture {
        let response = exomonad_actor::ResidentToolEndpoint::dispatch_boxed(self, invocation);
        Box::pin(async move {
            response
                .await?
                .into_json()
                .map_err(exomonad_actor::ResidentToolError::Encoding)
        })
    }
}

#[path = "resident_local_actor/completion_progress.rs"]
mod completion_progress;
#[path = "resident_local_actor/reload_progress.rs"]
mod reload_progress;
#[path = "resident_local_actor/reload_uncertainty.rs"]
mod reload_uncertainty;

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
    manual_release: bool,
    release: tokio::sync::Notify,
    completion_count: std::sync::atomic::AtomicUsize,
    cancellation_count: std::sync::atomic::AtomicUsize,
    output: std::sync::Mutex<std::collections::HashMap<String, String>>,
    keyed: Option<KeyedCommandGates>,
}

struct KeyedCommandGates {
    started: tokio::sync::mpsc::UnboundedSender<(String, String)>,
    release: std::collections::HashMap<String, tokio::sync::Semaphore>,
    completed: std::sync::Mutex<Vec<(String, String)>>,
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
                if let Some(keyed) = &self.keyed {
                    let marker = spec.argv.last().expect("keyed command marker");
                    let gate = keyed.release.get(marker).expect("known command marker");
                    keyed
                        .started
                        .send((marker.clone(), id.to_owned()))
                        .expect("test retains command admission receiver");
                    gate.acquire()
                        .await
                        .expect("command gate remains live")
                        .forget();
                    keyed
                        .completed
                        .lock()
                        .expect("keyed completion lock")
                        .push((marker.clone(), id.to_owned()));
                } else if self.manual_release {
                    self.release.notified().await;
                } else {
                    tokio::time::sleep(std::time::Duration::from_secs(seconds)).await;
                }
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
                Some(LocalResidentDeployment::SettlementChanged { notification }) => {
                    assert_command_settlement(&notification, owner, &backend);
                }
                Some(other) => panic!("unexpected deployment while awaiting command start: {}", other.kind()),
                None => panic!("resident forest closed before starting the command"),
            }
        }
    }
}

fn assert_command_settlement(
    notification: &exomonad_actor::SettlementNotification,
    owner: exomonad_actor::ActorRef,
    backend: &DelayedCommandBackend,
) {
    assert_eq!(notification.owner, owner);
    assert_eq!(
        notification.transition,
        exomonad_actor::SettlementTransition::Ready
    );
    let job = notification
        .command_job
        .as_ref()
        .expect("fixture command settlement");
    assert!(
        backend
            .output
            .lock()
            .expect("backend output lock")
            .contains_key(job),
        "settlement belongs to an actually completed fixture command: {notification:?}"
    );
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
async fn resident_command_retain_binding_returns_reference_without_command_presentation() {
    resident_await_watch_case(WatchCase::PrimaryCommandRetainBinding).await;
}

#[tokio::test]
async fn resident_failed_cell_discards_command_binding_and_retains_job_output() {
    resident_await_watch_case(WatchCase::PrimaryCommandRetainBindingFailure).await;
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

#[tokio::test]
async fn resident_parked_cell_publishes_into_latest_environment() {
    resident_await_watch_case(WatchCase::PrimaryInterleavedPublication).await;
}

type ResidentCellCall =
    tokio::task::JoinHandle<Result<serde_json::Value, exomonad_actor::ResidentToolError>>;

async fn dispatch_unbound_workbench_cell(
    actor: &exomonad_actor::LocalActorRef,
    source: &str,
) -> Result<serde_json::Value, exomonad_actor::ResidentToolError> {
    let (reply, receive) = tokio::sync::oneshot::channel();
    actor
        .address()
        .send_message(exomonad_actor::KernelMessage::Workbench {
            invocation: exomonad_actor::ActorWorkbenchInvocation::unbound(
                WorkbenchRequest::from_cell_input(source),
            ),
            control: None,
            reply: reply.into(),
        })
        .expect("queue direct owned workbench cell");
    match receive.await.map_err(|_| {
        exomonad_actor::ResidentToolError::Unavailable(
            "actor stopped before workbench cell completion".into(),
        )
    })? {
        Ok(response) => {
            serde_json::to_value(response).map_err(exomonad_actor::ResidentToolError::Encoding)
        }
        Err(error) => Err(exomonad_actor::ResidentToolError::Invocation(error)),
    }
}

async fn wait_for_armed_call<T: std::fmt::Debug>(
    actor: &exomonad_actor::LocalActorRef,
    context: &ToolInvocationContext,
    call: &mut tokio::task::JoinHandle<Result<T, exomonad_actor::ResidentToolError>>,
) -> tidepool_runtime::session::WorkbenchExecutionId {
    tokio::time::timeout(std::time::Duration::from_secs(180), async {
        tokio::select! {
            execution = async {
                loop {
                    if let Some(execution) = actor.hosted_workbench_waiting(context) {
                        break execution;
                    }
                    tokio::task::yield_now().await;
                }
            } => execution,
            reply = &mut *call => panic!("call settled before arming its exact workbench wait: {context:?}: {reply:?}"),
        }
    })
    .await
    .expect("original workbench wait arms within the preparation bound")
}

struct ConcurrentResident {
    forest: ResidentForest<NoHandlers, TestSink>,
    actor: exomonad_actor::LocalActorRef,
    policy: std::sync::Arc<dyn exomonad_actor::ResidentToolEndpoint>,
    backend: std::sync::Arc<DelayedCommandBackend>,
    started: tokio::sync::mpsc::UnboundedReceiver<(String, String)>,
    deployment_task: tokio::task::JoinHandle<()>,
    _root: tempfile::TempDir,
}

impl ConcurrentResident {
    async fn new(bucket: u32, markers: &[&str]) -> Self {
        Self::new_with_source(bucket, markers, None).await
    }

    async fn new_with_source(
        bucket: u32,
        markers: &[&str],
        layers: Option<exomonad_actor::ActorSourceLayerResolver>,
    ) -> Self {
        eval_harness::require_extract();
        let declarations = [
            tidepool_mcp::agent_tools_decl(),
            tidepool_mcp::actor_decl(),
            tidepool_mcp::actor_kernel_decl(),
            tidepool_mcp::actor_local_decl(),
            tidepool_mcp::commands_decl(),
            tidepool_mcp::fs_read_decl(),
            tidepool_mcp::sleep_decl(),
        ];
        let effects = tidepool_mcp::ensure_effects_module(&declarations).expect("actor effects");
        let mut include = effects.include_paths().to_vec();
        include.push(eval_harness::prelude_path());
        let preamble = insert_preamble_imports(
            &tidepool_mcp::build_preamble(&declarations, false),
            "Tidepool.Agent.Contract",
        );
        let preamble =
            insert_preamble_imports(&preamble, "qualified Tidepool.Agent.Watch as Watch");
        let preamble = insert_preamble_imports(&preamble, "Tidepool.Agent.Watch (Watches)");
        let preamble = format!(
            "{preamble}\ntype ActorEffects = '[AgentTools, Actor, Commands, Watch.Watches, Sleep]\n"
        );
        let session = support::process_unique_session(bucket);
        let root = tempfile::tempdir().expect("concurrent actor session root");
        let lib = SessionLib::open(session, root.path(), ModuleEnv::standalone_default())
            .expect("declaration plane")
            .with_validation_include(include.clone());
        let machine =
            ResidentSession::unbootstrapped(NoHandlers, TestSink, DEFAULT_NURSERY_SIZE, Some(lib));
        let (mut forest, mut deployments) = ResidentForest::new(
            ActorWorkbenchSource::new(preamble, include),
            session,
            machine,
            None,
            exomonad_actor::Incarnation::FIRST,
        );
        if let Some(layers) = layers {
            forest.set_source_layers(layers);
        }
        let actor = forest
            .new_workbench(
                "concurrent-publication".into(),
                exomonad_actor::ActorCapabilities::default().with_effect_keys(vec![
                    exomonad_actor::ActorEffectKey::Commands,
                    exomonad_actor::ActorEffectKey::Watches,
                ]),
            )
            .await
            .expect("concurrent workbench");
        let policy = std::sync::Arc::new(exomonad_actor::ResidentInteractivePolicy::local(
            actor.clone(),
        ));
        let (entered, started) = tokio::sync::mpsc::unbounded_channel();
        let backend = std::sync::Arc::new(DelayedCommandBackend {
            keyed: Some(KeyedCommandGates {
                started: entered,
                release: markers
                    .iter()
                    .map(|marker| ((*marker).to_owned(), tokio::sync::Semaphore::new(0)))
                    .collect(),
                completed: Default::default(),
            }),
            ..Default::default()
        });
        let owner = actor.identity();
        let supplied = backend.clone();
        let deployment_task = tokio::spawn(async move {
            while let Some(deployment) = deployments.recv().await {
                match deployment {
                    LocalResidentDeployment::CommandBackend(request) => {
                        assert_eq!(request.owner, owner);
                        let backend: std::sync::Arc<
                            dyn exomonad_actor::command_jobs::CommandBackend,
                        > = supplied.clone();
                        request.supply(Ok(backend));
                    }
                    LocalResidentDeployment::WatchChanged { notification } => {
                        assert_eq!(notification.owner, owner);
                    }
                    LocalResidentDeployment::SettlementChanged { notification } => {
                        assert_command_settlement(&notification, owner, &supplied);
                    }
                    LocalResidentDeployment::Retired { actor: retired, .. } => {
                        assert_eq!(retired, owner);
                        break;
                    }
                    other => panic!("unexpected concurrency deployment: {}", other.kind()),
                }
            }
        });
        Self {
            forest,
            actor,
            policy,
            backend,
            started,
            deployment_task,
            _root: root,
        }
    }

    fn spawn_cell(&self, key: &str, source: String) -> ResidentCellCall {
        let policy = self.policy.clone();
        let context = Self::cell_context(key);
        tokio::spawn(async move {
            policy
                .dispatch_json_boxed(ToolInvocation {
                    context: Some(context),
                    name: exomonad_actor::HASKELL_TOOL.into(),
                    arguments: ToolArguments::Raw(source),
                })
                .await
        })
    }

    fn cell_context(key: &str) -> ToolInvocationContext {
        ToolInvocationContext::external(
            "concurrent-publication".into(),
            key.into(),
            key.into(),
            Some(key.into()),
            None,
        )
    }

    async fn wait_started(&mut self, marker: &str, call: &mut ResidentCellCall) -> String {
        let (actual, job) =
            tokio::time::timeout(std::time::Duration::from_secs(180), async {
                tokio::select! {
                    started = self.started.recv() => started,
                    reply = &mut *call => panic!("{marker} settled before command admission: {reply:?}"),
                }
            })
                .await
                .expect("real cell reaches its command")
                .expect("command owner remains live");
        assert_eq!(actual, marker);
        assert!(!job.is_empty());
        let context = Self::cell_context(marker);
        let execution = wait_for_armed_call(&self.actor, &context, call).await;
        assert!(
            !call.is_finished(),
            "{marker} retains its independent continuation"
        );
        execution.as_str().into()
    }

    fn release(&self, marker: &str) {
        self.backend
            .keyed
            .as_ref()
            .expect("keyed backend")
            .release
            .get(marker)
            .expect("known marker")
            .add_permits(1);
    }

    async fn settle(
        call: ResidentCellCall,
    ) -> Result<serde_json::Value, exomonad_actor::ResidentToolError> {
        tokio::time::timeout(std::time::Duration::from_secs(600), call)
            .await
            .expect("real cell settles after gate release")
            .expect("cell dispatch task")
    }

    async fn read(&self, key: &str, source: &str) -> serde_json::Value {
        let reply = Self::settle(self.spawn_cell(key, source.into()))
            .await
            .expect("public read");
        assert_eq!(reply["status"], "committed", "{reply:?}");
        reply
    }

    async fn shutdown(self, markers: &[&str]) {
        self.forest.shutdown().await;
        assert_eq!(
            self.actor.terminal().wait().await.kind,
            exomonad_actor::ActorExitKind::Cancelled,
        );
        let cleanup = self.actor.terminal().cleanup().expect("retained cleanup");
        assert!(cleanup.is_confirmed(), "{cleanup:?}");
        {
            let completed = self
                .backend
                .keyed
                .as_ref()
                .expect("keyed backend")
                .completed
                .lock()
                .expect("keyed completion lock");
            assert_eq!(
                completed.len(),
                markers.len(),
                "each command executes once: {completed:?}"
            );
            for marker in markers {
                assert_eq!(
                    completed
                        .iter()
                        .filter(|(actual, _)| actual == marker)
                        .count(),
                    1
                );
            }
            assert_eq!(
                completed
                    .iter()
                    .map(|(_, job)| job)
                    .collect::<std::collections::HashSet<_>>()
                    .len(),
                markers.len()
            );
        }
        assert_eq!(
            self.backend
                .cancellation_count
                .load(std::sync::atomic::Ordering::Acquire),
            0
        );
        tokio::time::timeout(std::time::Duration::from_secs(5), self.deployment_task)
            .await
            .expect("retirement reaches the deployment owner")
            .expect("deployment owner succeeds");
    }
}

fn committed_execution(reply: &serde_json::Value) -> String {
    assert_eq!(reply["status"], "committed", "{reply:?}");
    let operations: Vec<_> = reply["items"]
        .as_array()
        .expect("item receipts")
        .iter()
        .filter_map(|item| item["operations"].as_array())
        .flatten()
        .collect();
    assert!(
        !operations.is_empty(),
        "real command/watch effects have receipts"
    );
    let execution = operations[0]["id"]["execution"]
        .as_str()
        .expect("execution identity");
    let mut addresses = std::collections::HashSet::new();
    for operation in &operations {
        assert_eq!(operation["disposition"], "committed");
        assert_eq!(operation["id"]["execution"], execution);
        assert!(
            addresses.insert((
                operation["id"]["inputUnitIndex"].as_u64().unwrap(),
                operation["id"]["effectOrdinal"].as_u64().unwrap()
            )),
            "no duplicated operation receipt"
        );
    }
    execution.into()
}

#[tokio::test]
async fn resident_same_name_shadowing_follows_completion_order() {
    let markers = ["shadow-0-A", "shadow-0-B", "shadow-1-A", "shadow-1-B"];
    let mut fixture = ConcurrentResident::new(185, &markers).await;
    for round in 0..2 {
        fixture
            .read(&format!("seed-{round}"), "x <- pure (0 :: Int)")
            .await;
        let source = include_str!("resident_local_actor/shadow_cell.hs");
        let mut a = fixture.spawn_cell(
            markers[round * 2],
            source
                .replace("OLD_BINDING", "oldA")
                .replace("MARKER", markers[round * 2])
                .replace("RESULT_VALUE", "11"),
        );
        let a_execution = fixture.wait_started(markers[round * 2], &mut a).await;
        let mut b = fixture.spawn_cell(
            markers[round * 2 + 1],
            source
                .replace("OLD_BINDING", "oldB")
                .replace("MARKER", markers[round * 2 + 1])
                .replace("RESULT_VALUE", "22"),
        );
        let b_execution = fixture.wait_started(markers[round * 2 + 1], &mut b).await;
        assert!(!a.is_finished() && !b.is_finished());
        let seed = fixture.read(&format!("parked-read-{round}"), "x").await;
        assert_eq!(seed["items"][0]["output"], "0");
        let (
            first,
            last,
            first_index,
            last_index,
            first_value,
            last_value,
            first_execution,
            last_execution,
        ) = if round == 0 {
            (
                a,
                b,
                round * 2,
                round * 2 + 1,
                "11",
                "22",
                a_execution,
                b_execution,
            )
        } else {
            (
                b,
                a,
                round * 2 + 1,
                round * 2,
                "22",
                "11",
                b_execution,
                a_execution,
            )
        };
        fixture.release(markers[first_index]);
        let first = ConcurrentResident::settle(first)
            .await
            .expect("first publication");
        assert_eq!(committed_execution(&first), first_execution);
        assert!(!last.is_finished(), "other cell remains parked");
        let current = fixture.read(&format!("first-read-{round}"), "x").await;
        assert_eq!(current["items"][0]["output"], first_value);
        fixture.release(markers[last_index]);
        let last = ConcurrentResident::settle(last)
            .await
            .expect("last publication");
        assert_eq!(committed_execution(&last), last_execution);
        assert_ne!(committed_execution(&first), committed_execution(&last));
        let current = fixture.read(&format!("last-read-{round}"), "x").await;
        assert_eq!(current["items"][0]["output"], last_value);
        let captures = fixture
            .read(&format!("captured-read-{round}"), "oldA == 0 && oldB == 0")
            .await;
        assert_eq!(
            captures["items"][0]["output"], "True",
            "both admitted values retain their old meaning"
        );
    }
    fixture.shutdown(&markers).await;
}

#[tokio::test]
async fn resident_invalid_concurrent_declaration_join_publishes_nothing_from_loser() {
    let markers = ["join-A", "join-B"];
    let mut fixture = ConcurrentResident::new(186, &markers).await;
    fixture
        .read(
            "join-base",
            include_str!("resident_local_actor/join_base.hs"),
        )
        .await;
    let source = include_str!("resident_local_actor/conflicting_join_cell.hs");
    let mut a = fixture.spawn_cell(
        markers[0],
        source
            .replace("SIDE", "A")
            .replace("MARKER", markers[0])
            .replace("RESULT_VALUE", "11"),
    );
    let a_execution = fixture.wait_started(markers[0], &mut a).await;
    let mut b = fixture.spawn_cell(
        markers[1],
        source
            .replace("SIDE", "B")
            .replace("MARKER", markers[1])
            .replace("RESULT_VALUE", "22"),
    );
    let b_execution = fixture.wait_started(markers[1], &mut b).await;
    assert!(
        !a.is_finished() && !b.is_finished(),
        "both complete cells admitted before publication"
    );
    fixture.release(markers[1]);
    let winner = ConcurrentResident::settle(b)
        .await
        .expect("B publishes its valid instance");
    let winner_execution = committed_execution(&winner);
    assert_eq!(winner_execution, b_execution);
    fixture.release(markers[0]);
    let failure = ConcurrentResident::settle(a)
        .await
        .expect_err("A's concurrent instance conflicts");
    let exomonad_actor::ResidentToolError::Invocation(
        exomonad_actor::KernelInvocationFailure::Workbench(failure),
    ) = failure
    else {
        panic!("expected publication workbench failure: {failure:?}");
    };
    assert!(
        failure.detail.contains("private publication rejected"),
        "{failure:?}"
    );
    assert!(
        failure.detail.contains("ClassInstanceConflict"),
        "the real compiler join refuses the concurrent instances: {failure:?}"
    );
    let operations: Vec<_> = failure
        .receipts
        .iter()
        .flat_map(|receipt| &receipt.operations)
        .collect();
    assert!(
        !operations.is_empty(),
        "completed effects survive publication rejection"
    );
    let loser_execution = &operations[0].id.execution;
    assert_eq!(loser_execution.as_str(), a_execution);
    assert_ne!(loser_execution.as_str(), winner_execution);
    for operation in &operations {
        assert_eq!(&operation.id.execution, loser_execution);
        assert_eq!(
            operation.disposition,
            tidepool_runtime::session::WorkbenchOperationDisposition::Committed
        );
    }
    let public = fixture
        .read("join-winner-read", "joinValue False + onlyB + joinedB")
        .await;
    assert_eq!(public["items"][0]["output"], "66");
    let constructor = fixture
        .read(
            "join-winner-constructor",
            "case OnlyB of OnlyB -> (22 :: Int)",
        )
        .await;
    assert_eq!(constructor["items"][0]["output"], "22");
    for (key, source) in [
        ("join-no-loser-early-native", "onlyA"),
        ("join-no-loser-late-native", "joinedA"),
        (
            "join-no-loser-declaration",
            "case OnlyA of OnlyA -> (11 :: Int)",
        ),
    ] {
        let refused = ConcurrentResident::settle(fixture.spawn_cell(key, source.into()))
            .await
            .expect("invalid fresh source has a structured rejection");
        assert_eq!(
            refused["status"], "rejected",
            "loser publishes nothing: {refused:?}"
        );
    }
    fixture.shutdown(&markers).await;
}

enum WatchCase {
    PrimaryRoundTrip,
    PrimaryInterleavedPublication,
    StructuredRoundTrip,
    StructuredCommandPresentation,
    PrimaryCommandRetainBinding,
    PrimaryCommandRetainBindingFailure,
    PrimaryCancellation,
    PrimarySleepCancellation,
    PrimaryCommandAwaitCancellation,
    PrimaryCommandForegroundCancellation,
    PrimaryCommandNotifyCancellation,
}

async fn resident_await_watch_case(case: WatchCase) {
    let interleaved = matches!(case, WatchCase::PrimaryInterleavedPublication);
    let structured_command = matches!(case, WatchCase::StructuredCommandPresentation);
    let binding_failure = matches!(case, WatchCase::PrimaryCommandRetainBindingFailure);
    let retain_command_binding = matches!(case, WatchCase::PrimaryCommandRetainBinding);
    let direct_binding_cell = binding_failure || retain_command_binding;
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

    let session = support::process_unique_session(if retain_command_binding {
        185
    } else if binding_failure {
        186
    } else if command_observation.is_some() {
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
    let declarations = if direct_binding_cell {
        vec![tidepool_mcp::commands_decl()]
    } else {
        vec![
            tidepool_mcp::agent_tools_decl(),
            tidepool_mcp::actor_decl(),
            tidepool_mcp::actor_kernel_decl(),
            tidepool_mcp::actor_local_decl(),
            tidepool_mcp::commands_decl(),
            tidepool_mcp::fs_read_decl(),
            tidepool_mcp::sleep_decl(),
        ]
    };
    let effects = tidepool_mcp::ensure_effects_module(&declarations).expect("actor effects");
    let mut include = effects.include_paths().to_vec();
    include.push(eval_harness::prelude_path());
    let preamble = tidepool_mcp::build_preamble(&declarations, false);
    let preamble = if direct_binding_cell {
        format!("{preamble}type ActorEffects = '[Commands]\n")
    } else {
        let preamble = insert_preamble_imports(&preamble, "Tidepool.Agent.Contract");
        let preamble =
            insert_preamble_imports(&preamble, "qualified Tidepool.Agent.Watch as Watch");
        let preamble = insert_preamble_imports(&preamble, "Tidepool.Agent.Watch (Watches)");
        format!(
            "{preamble}\
             type ActorEffects = '[AgentTools, Actor, Commands, Watch.Watches, Sleep]\n\
             data WaitInput = WaitInput {{ delay :: Int }} deriving (Generic, FromJSON, JsonSchema)\n\
             data WaitOutput = WaitOutput {{ settled :: Bool }} deriving (Generic, ToJSON, JsonSchema)\n\
             data ResidentTools mode = ResidentTools {{ waitForCommand :: mode :- Call WaitInput WaitOutput }} deriving (Generic)\n"
        )
    };
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
        let compiled = match tidepool_testing::with_settlement(|settlement| {
            run_turn(
                HaskellTurnRequest {
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
                },
                settlement,
            )
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
                exomonad_actor::ActorCapabilities::default().with_effect_keys(
                    if direct_binding_cell {
                        vec![exomonad_actor::ActorEffectKey::Commands]
                    } else {
                        vec![
                            exomonad_actor::ActorEffectKey::Commands,
                            exomonad_actor::ActorEffectKey::Watches,
                            exomonad_actor::ActorEffectKey::Sleep,
                        ]
                    },
                ),
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
    let command_backend = std::sync::Arc::new(DelayedCommandBackend {
        manual_release: interleaved,
        ..DelayedCommandBackend::default()
    });
    let settled = if cancel_first {
        None
    } else {
        let settled_context = ToolInvocationContext::external(
            "await-watch-test".into(),
            "turn-settled".into(),
            "call-settled".into(),
            Some("call-settled".into()),
            None,
        );
        let mut settled_call = {
            let actor = actor.clone();
            let policy = policy.clone();
            let context = settled_context.clone();
            tokio::spawn(async move {
                let arguments = if primary {
                    ToolArguments::Raw(if retain_command_binding {
                        include_str!("resident_local_actor/command_retain_binding_cell.hs").into()
                    } else if binding_failure {
                        include_str!("resident_local_actor/command_retain_binding_failure_cell.hs")
                            .into()
                    } else if interleaved {
                        include_str!("resident_local_actor/interleaved_bind_cell.hs").into()
                    } else {
                        include_str!("resident_local_actor/await_watch_cell.hs")
                            .replace("DELAY", "1")
                    })
                } else {
                    ToolArguments::Structured(serde_json::json!({"delay": 1}))
                };
                if direct_binding_cell {
                    let ToolArguments::Raw(source) = arguments else {
                        unreachable!("binding tests always dispatch source cells")
                    };
                    dispatch_unbound_workbench_cell(&actor, &source).await
                } else {
                    policy
                        .dispatch_boxed(ToolInvocation {
                            context: Some(context),
                            name: if primary {
                                exomonad_actor::HASKELL_TOOL
                            } else {
                                "wait_for_command"
                            }
                            .into(),
                            arguments,
                        })
                        .await
                        .map(|response| {
                            response
                                .into_json()
                                .expect("serialize typed resident tool response")
                        })
                }
            })
        };
        tokio::time::timeout(std::time::Duration::from_secs(180), async {
        tokio::select! {
            () = supply_command_until_started(&mut deployments, command_backend.clone(), actor.identity()) => {},
            reply = &mut settled_call => panic!("watch cell settled before starting its command: {reply:?}"),
        }
    }).await.expect("command start is bounded");
        if primary && !direct_binding_cell {
            wait_for_armed_call(&actor, &settled_context, &mut settled_call).await;
        }
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(100), &mut settled_call)
                .await
                .is_err(),
            "awaitWatch must keep the hosted workbench parked while its exact command is live"
        );
        if interleaved {
            let second_context = ToolInvocationContext::external(
                "await-watch-test".into(),
                "turn-settled".into(),
                "publish-B".into(),
                Some("publish-B".into()),
                None,
            );
            let second = tokio::time::timeout(
                std::time::Duration::from_secs(180),
                policy.dispatch_boxed(ToolInvocation {
                    context: Some(second_context),
                    name: exomonad_actor::HASKELL_TOOL.into(),
                    arguments: ToolArguments::Raw("b :: Int\nb = 40".into()),
                }),
            )
            .await
            .expect("B publishes while A remains parked")
            .expect("B publication")
            .into_json()
            .expect("serialize B publication");
            assert_eq!(second["status"], "committed", "{second:?}");
            assert!(
                !settled_call.is_finished(),
                "A still owns its parked continuation"
            );
            command_backend.release.notify_one();
        }
        if primary && !direct_binding_cell {
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
        .expect("watch call task");
        if binding_failure {
            let Err(error) = settled else {
                panic!("the deliberately failing post-effect cell must fail: {settled:?}");
            };
            let exomonad_actor::ResidentToolError::Invocation(
                exomonad_actor::KernelInvocationFailure::Workbench(failure),
            ) = error
            else {
                panic!("post-effect failure should retain a workbench receipt: {error:?}");
            };
            let receipt = failure
                .receipts
                .last()
                .expect("failed unit receipt is retained");
            assert_eq!(receipt.installed_bindings.len(), 1, "{receipt:?}");
            let binding = &receipt.installed_bindings[0];
            assert!(
                matches!(
                    failure.publication.as_ref(),
                    Some(
                        tidepool_runtime::session::WorkbenchPublicationOutcome::NotPublished {
                            reason: tidepool_runtime::session::WorkbenchNotPublishedReason::Failed
                        }
                    )
                ),
                "a failed cell publishes no names: {:?}",
                failure.publication
            );
            assert!(!binding.contains("session_id:"), "{receipt:?}");
            assert!(
                receipt.output.contains(binding),
                "the receipt retains private progress: {receipt:?}"
            );
            assert_eq!(
                receipt.operations.len(),
                3,
                "start, retain binding, and await committed before the later cell failure: {receipt:?}"
            );
            assert!(
                receipt
                    .operations
                    .iter()
                    .all(|operation| operation.effect == "command job"),
                "all three operations belong to the Commands effect: {receipt:?}"
            );
            for (ordinal, operation) in receipt.operations.iter().enumerate() {
                assert_eq!(operation.id.effect_ordinal, ordinal);
                assert_eq!(
                    operation.disposition,
                    tidepool_runtime::session::WorkbenchOperationDisposition::Committed
                );
            }
            assert!(
                receipt
                    .operations
                    .iter()
                    .all(|operation| !operation.effect.contains("Present")),
                "retaining a binding does not implicitly present command output: {receipt:?}"
            );
            assert!(
                receipt
                    .operations
                    .iter()
                    .all(|operation| operation.display.is_none()),
                "retaining the command job produces no display output: {receipt:?}"
            );
            let binding_read =
                dispatch_unbound_workbench_cell(&actor, &format!("Cmd.await {binding} >> pure ()"))
                    .await
                    .expect("discarded name produces a structured rejection");
            assert_eq!(binding_read["status"], "rejected", "{binding_read:?}");
            let job = command_backend
                .output
                .lock()
                .expect("backend output lock")
                .iter()
                .find_map(|(job, text)| text.is_empty().then(|| job.clone()))
                .expect("the completed command retains its output by session id");
            let recovered = exomonad_actor::command_jobs::CommandBackend::output(
                command_backend.as_ref(),
                &job,
                1024,
            )
            .await
            .expect("same session id recovers output after the cell fails");
            assert!(recovered.stdout.finished);
            assert_eq!(
                command_backend
                    .completion_count
                    .load(std::sync::atomic::Ordering::SeqCst),
                1,
                "recovering by session id observes the original command without replay"
            );
            forest.shutdown().await;
            return;
        }
        let settled = settled.expect("watch tool call");
        if primary {
            assert_eq!(settled["status"], "committed", "{settled:?}");
            if interleaved {
                let read_context = ToolInvocationContext::external(
                    "await-watch-test".into(),
                    "turn-settled".into(),
                    "read-joined".into(),
                    Some("read-joined".into()),
                    None,
                );
                let joined = policy
                    .dispatch_boxed(ToolInvocation {
                        context: Some(read_context),
                        name: exomonad_actor::HASKELL_TOOL.into(),
                        arguments: ToolArguments::Raw("a + b".into()),
                    })
                    .await
                    .expect("read joined values")
                    .into_json()
                    .expect("serialize joined values");
                assert_eq!(joined["status"], "committed", "{joined:?}");
                assert_eq!(joined["items"][0]["output"], "42", "{joined:?}");
                forest.shutdown().await;
                return;
            }
            if retain_command_binding {
                let installed_bindings = settled["items"][0]["installedBindings"]
                    .as_array()
                    .expect("host binding references");
                let observation_binding = installed_bindings
                    .first()
                    .and_then(serde_json::Value::as_str)
                    .expect("ordinary result observation is listed first");
                assert!(
                    observation_binding.starts_with("observation"),
                    "the automatic result binder is distinct from the retained job: {settled:?}"
                );
                let binding = installed_bindings
                    .last()
                    .and_then(serde_json::Value::as_str)
                    .expect("the command retain effect returns the retained binding reference");
                assert!(!binding.contains("session_id:"), "{settled:?}");
                assert_eq!(installed_bindings.len(), 2, "{settled:?}");
                assert_ne!(binding, observation_binding, "{settled:?}");
                assert_eq!(
                    settled["items"][0]["output"],
                    format!("[bound {observation_binding}]"),
                    "only the cell's automatic observation is reported"
                );
                let operations = settled["items"][0]["operations"]
                    .as_array()
                    .expect("retained binding effect receipts");
                assert_eq!(operations.len(), 3, "{settled:?}");
                assert!(
                    operations
                        .iter()
                        .all(|operation| operation["effect"] == "command job"),
                    "retaining and awaiting the job does not emit a presentation operation: {settled:?}"
                );
                assert!(
                    operations
                        .iter()
                        .all(|operation| operation["display"].is_null()),
                    "the effect receipt carries no display output: {settled:?}"
                );
                let binding_read = dispatch_unbound_workbench_cell(
                    &actor,
                    &format!("Cmd.await {binding} >> pure ()"),
                )
                .await
                .expect("the retained binding can be used by a later cell");
                assert_eq!(binding_read["status"], "committed", "{binding_read:?}");
                assert_eq!(
                    binding_read["items"][0]["operations"][0]["effect"], "command job",
                    "{binding_read:?}"
                );
                forest.shutdown().await;
                return;
            }
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
        Some("call-cancelled".into()),
        None,
    );
    let mut cancelled_call = {
        let policy = policy.clone();
        let context = cancelled_context.clone();
        tokio::spawn(async move {
            policy
                .dispatch_json_boxed(ToolInvocation {
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
    } else {
        let cancellation_start_bound = 180;
        tokio::time::timeout(std::time::Duration::from_secs(cancellation_start_bound), async {
        tokio::select! {
            () = supply_command_until_started(&mut deployments, command_backend.clone(), actor.identity()) => {},
            reply = &mut cancelled_call => panic!("cancellable watch cell settled before starting its command: {reply:?}"),
        }
    }).await.expect("cancellable command start is bounded");
    }
    let armed_execution =
        wait_for_armed_call(&actor, &cancelled_context, &mut cancelled_call).await;
    if cancel_sleep {
        if let Ok(reply) =
            tokio::time::timeout(std::time::Duration::from_millis(100), &mut cancelled_call).await
        {
            panic!("captured sleep settled before cancellation: {reply:?}");
        }
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
        exomonad_actor::WorkbenchCancellationOutcome::Cancelled { execution, reply } => {
            assert_eq!(execution, armed_execution);
            reply
        }
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
            LocalResidentDeployment::SettlementChanged { notification } => {
                assert_command_settlement(&notification, actor.identity(), &command_backend);
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
    let compiled = match tidepool_testing::with_settlement(|settlement| {
        run_turn(
            HaskellTurnRequest {
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
            },
            settlement,
        )
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
        .dispatch_json_boxed(ToolInvocation {
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
            .dispatch_json_boxed(ToolInvocation {
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
        .dispatch_json_boxed(ToolInvocation {
            context: None,
            name: "current_value".into(),
            arguments: ToolArguments::Structured(serde_json::json!({})),
        })
        .await
        .expect("state remains available after refused transitions");
    assert_eq!(unchanged, serde_json::json!({"current": 0}));

    let doubled = policy
        .dispatch_json_boxed(ToolInvocation {
            context: None,
            name: "double_value".into(),
            arguments: ToolArguments::Structured(serde_json::json!({"value": 6})),
        })
        .await
        .expect("double value");
    assert_eq!(doubled, serde_json::json!({"doubled": 12}));

    let spawned = policy
        .dispatch_json_boxed(ToolInvocation {
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
        .dispatch_json_boxed(ToolInvocation {
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
        .dispatch_json_boxed(ToolInvocation {
            context: None,
            name: "current_value".into(),
            arguments: ToolArguments::Structured(serde_json::json!({})),
        })
        .await
        .expect("sibling survives root retirement");
    assert_eq!(retained, serde_json::json!({"current": 73}));
    sibling_installation
        .policy
        .dispatch_json_boxed(ToolInvocation {
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
