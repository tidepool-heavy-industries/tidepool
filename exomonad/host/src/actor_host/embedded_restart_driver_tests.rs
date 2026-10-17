//! Durable restart uses the production host's journal, binding and actor owners.
use crate::actor_host::hosted_test_context::HostedTestRuntime;
use crate::actor_host::test_campaign::{
    configure_command_path, hosted_script_provider, hosted_test_settings, next_hosted_script_round,
};
use crate::actor_host::{embedded_harness::EmbeddedHarnessRuntime, ActorHostConfig};
use harness::{
    engine::ResponsesTransport,
    item::Item,
    model::{AgentPath, RequestId},
    transport::{PreparedProviderRequest, ResponsesRequest, ResponsesTurn, TransportError},
};
use parking_lot::Mutex;
use serde_json::json;
use std::{
    collections::VecDeque,
    path::Path,
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    },
    time::Duration,
};
use tokio::sync::{mpsc, oneshot};

async fn observe_provider_round<F: std::future::Future>(
    failure: oneshot::Receiver<String>,
    round: F,
) -> Result<F::Output, String> {
    tokio::select! {
        biased;
        failure = failure => Err(failure.unwrap_or_else(|_| "provider failure observer closed".into())),
        round = round => Ok(round),
    }
}

#[tokio::test]
async fn provider_fixture_failure_interrupts_pending_round_observation() {
    let (failure, observed) = oneshot::channel();
    let provider = InterruptedStream {
        runtime: std::sync::Weak::new(),
        inputs: Arc::new(Mutex::new(Vec::new())),
        calls: Arc::new(AtomicUsize::new(0)),
        interruption: InterruptedResponse::EmptyEof,
        effect_path: None,
        failure: Mutex::new(Some(failure)),
    };
    let error = provider.failed("original command refusal".into());
    assert!(
        matches!(error, TransportError::Stream(ref detail) if detail == "original command refusal")
    );
    let result = tokio::time::timeout(
        Duration::from_secs(1),
        observe_provider_round(observed, std::future::pending::<()>()),
    )
    .await
    .expect("provider failure must interrupt an indefinitely pending round");
    assert_eq!(result, Err("original command refusal".into()));
}

fn retained_config(config: &mut ActorHostConfig, workspace: &Path, run_root: &Path) {
    config.workspace = workspace.to_path_buf();
    config.run_directory =
        tidepool_atomic_write::DirectoryAnchor::open_existing(run_root.parent().unwrap())
            .unwrap()
            .child(run_root.file_name().unwrap())
            .unwrap();
    config.root_binding_path = run_root.join("root-binding.json");
}

async fn wait_head(host: &HostedTestRuntime, previous: Option<&RequestId>) -> RequestId {
    tokio::time::timeout(Duration::from_secs(300), async {
        loop {
            let conversation = host
                .context
                .binding(host.context.actor.identity())
                .unwrap()
                .conversation()
                .unwrap();
            let frontier = host
                .runtime
                .store()
                .embedded_round_frontier(conversation.identity())
                .unwrap();
            if frontier.pending_head.is_none() && conversation.active_round().is_none() {
                if let Some(head) = frontier.settled_head.filter(|head| Some(head) != previous) {
                    return head;
                }
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("production driver must settle its actual durable round")
}

#[tokio::test]
async fn driver_reopens_retained_final_without_provider_then_waits_for_explicit_input() {
    let files = tempfile::tempdir().unwrap();
    let settings = hosted_test_settings(&files, 1);
    let repository = exomonad_worktree::testing::TestRepo::init().unwrap();
    repository
        .writer()
        .commit_file("README.md", "source\n", "seed")
        .unwrap();
    let run_root = files.path().join("run");
    let (provider, mut requests) = hosted_script_provider();
    let host = HostedTestRuntime::start_configured(&settings, &provider, |config| {
        retained_config(config, repository.path(), &run_root)
    })
    .await
    .unwrap();
    host.input("Establish the retained final response.")
        .await
        .unwrap();
    let root = AgentPath("/root".into());
    let mut pending = VecDeque::new();
    next_hosted_script_round(&mut requests, &mut pending, &root)
        .await
        .finish();
    let first = wait_head(&host, None).await;
    let conversation = host
        .context
        .binding(host.context.actor.identity())
        .unwrap()
        .conversation()
        .unwrap();
    conversation
        .input("retained-input", "operator", "retained input")
        .await
        .unwrap();
    next_hosted_script_round(&mut requests, &mut pending, &root)
        .await
        .finish();
    let retained = wait_head(&host, Some(&first)).await;
    assert!(host.runtime.store().unread("/root").unwrap().is_empty());
    let predecessor = conversation.identity().clone();
    let retained_run_id = host.context.config.run_id.clone();
    drop(conversation);
    host.stop(&crate::actor_host::hosted_test_context::ScenarioOutcome::Passed)
        .await
        .unwrap();
    drop(provider);

    let (provider, mut requests) = hosted_script_provider();
    let host = HostedTestRuntime::start_existing_configured(&settings, &provider, |config| {
        config.run_id = retained_run_id;
        retained_config(config, repository.path(), &run_root)
    })
    .await
    .unwrap();
    let conversation = host
        .context
        .binding(host.context.actor.identity())
        .unwrap()
        .conversation()
        .unwrap();
    assert_ne!(conversation.identity().incarnation, predecessor.incarnation);
    assert_eq!(conversation.identity().run, predecessor.run);
    assert_eq!(wait_head(&host, None).await, retained);
    assert!(
        tokio::time::timeout(Duration::from_millis(100), requests.recv())
            .await
            .is_err(),
        "reopening settled history must not request the provider"
    );
    conversation
        .input("explicit-next", "operator", "new explicit input")
        .await
        .unwrap();
    let round = next_hosted_script_round(&mut requests, &mut pending, &root).await;
    for text in ["retained input", "new explicit input"] {
        assert_eq!(
            round
                .request
                .input
                .iter()
                .filter(|item| item.0["content"] == text)
                .count(),
            1
        );
    }
    round.finish();
    let successor = wait_head(&host, Some(&retained)).await;
    assert_eq!(
        host.runtime
            .store()
            .request(&successor)
            .unwrap()
            .unwrap()
            .parent,
        Some(retained)
    );
    drop(conversation);
    host.stop(&crate::actor_host::hosted_test_context::ScenarioOutcome::Passed)
        .await
        .unwrap();
}

#[derive(Clone, Copy)]
enum InterruptedResponse {
    EmptyEof,
    CompletedCallEof,
    Incomplete,
}

#[derive(Clone, Copy)]
enum ContinuationAction {
    Send,
    Retry,
}

struct InterruptedStream {
    runtime: std::sync::Weak<EmbeddedHarnessRuntime>,
    inputs: Arc<Mutex<Vec<(RequestId, ResponsesRequest)>>>,
    calls: Arc<AtomicUsize>,
    interruption: InterruptedResponse,
    effect_path: Option<std::path::PathBuf>,
    failure: Mutex<Option<oneshot::Sender<String>>>,
}

impl InterruptedStream {
    fn failed(&self, detail: String) -> TransportError {
        if let Some(failure) = self.failure.lock().take() {
            let _ = failure.send(detail.clone());
        }
        TransportError::Stream(detail)
    }
}

fn final_turn() -> ResponsesTurn {
    ResponsesTurn {
        response_id: uuid::Uuid::new_v4().simple().to_string(),
        items: vec![Item(
            json!({"type":"message", "role":"assistant", "phase":"final_answer", "content":[{"type":"output_text", "text":"done"}]}),
        )],
        usage: Default::default(),
    }
}

#[async_trait::async_trait]
impl ResponsesTransport for InterruptedStream {
    fn response_contract(
        &self,
    ) -> Result<
        (
            harness::transport::ResponsesRoute,
            harness::transport::ResponsesProtocol,
        ),
        TransportError,
    > {
        Ok((
            harness::transport::ResponsesRoute::ChatGptPlan,
            harness::transport::ResponsesProtocol::Standard,
        ))
    }

    async fn create(&self, _: PreparedProviderRequest) -> Result<ResponsesTurn, TransportError> {
        panic!("the production driver must use streamed dispatch");
    }

    async fn create_streaming(
        &self,
        request: PreparedProviderRequest,
        sink: mpsc::Sender<harness::transport::sse::StreamEvent>,
    ) -> Result<ResponsesTurn, TransportError> {
        self.inputs
            .lock()
            .push((request.request_id().clone(), request.request().clone()));
        let attempt = self.calls.fetch_add(1, Ordering::SeqCst);
        let failed_attempts = match self.interruption {
            InterruptedResponse::EmptyEof => 3,
            InterruptedResponse::CompletedCallEof | InterruptedResponse::Incomplete => 1,
        };
        if attempt >= failed_attempts {
            return Ok(final_turn());
        }
        let mut response = harness::transport::sse::ResponseAssembly::default();
        if matches!(self.interruption, InterruptedResponse::Incomplete) {
            return response
                .accept(
                    &json!({
                        "type": "response.incomplete",
                        "response": {
                            "id": "retire-incomplete",
                            "incomplete_details": {"reason": "content_filter"},
                        },
                    })
                    .to_string(),
                )
                .map(|_| unreachable!("response.incomplete must return its typed error"));
        }
        if matches!(self.interruption, InterruptedResponse::CompletedCallEof) {
            let event = response
                .accept(
                    &json!({"type":"response.output_item.done", "output_index":0, "item": {
                        "type":"custom_tool_call", "call_id":"effect-before-eof", "name":"haskell",
                        "input": tidepool_testing::fixture_source("exomonad/host/src/actor_host/restart_effect_once.hs"),
                    }})
                    .to_string(),
                )
                .map_err(|error| self.failed(format!("fixture event: {error}")))?
                .ok_or_else(|| self.failed("fixture event did not emit an item".into()))?;
            sink.send(event)
                .await
                .map_err(|error| self.failed(format!("fixture event delivery: {error}")))?;
            let runtime = self
                .runtime
                .upgrade()
                .ok_or_else(|| self.failed("provider runtime owner was released".into()))?;
            tokio::time::timeout(Duration::from_secs(300), async {
                loop {
                    let claims = runtime
                        .store()
                        .claims(&harness::model::CallId("effect-before-eof".into()))
                        .map_err(|error| format!("retained call claims: {error}"))?;
                    if let [claim] = claims.as_slice() {
                        if let Some(output) =
                            runtime.scheduler().output(&claim.operation).await
                                .map_err(|error| format!("operation settlement: {error}"))?
                        {
                            if !matches!(output, harness::turn::JobOutput::Completed(Ok(_))) {
                                return Err(format!("admitted notebook failed: {output:?}"));
                            }
                            if runtime
                                .store()
                                .replay_output_operation(&claim.operation)
                                .map_err(|error| format!("retained operation output: {error}"))?
                                .is_some()
                            {
                                let witness = std::fs::read(self.effect_path.as_ref().unwrap())
                                    .map_err(|error| format!("effect witness: {error}; retained notebook output: {output:?}"))?;
                                if witness != b"x" {
                                    return Err(format!("effect witness must be exactly one execution, got {witness:?}; retained notebook output: {output:?}"));
                                }
                                break;
                            }
                        }
                    }
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
                Ok::<(), String>(())
            })
            .await
            .unwrap_or_else(|_| Err("admitted effect did not settle before EOF".into()))
            .map_err(|detail| self.failed(detail))?;
        }
        response.finish()
    }
}

async fn browser_session(host: &HostedTestRuntime) -> String {
    let settings = host.context.config.embedded.as_ref().unwrap();
    let secret = std::fs::read_to_string(settings.session_secret_file.as_ref().unwrap()).unwrap();
    let login = reqwest::Client::builder()
        .timeout(Duration::from_secs(30))
        .build()
        .unwrap()
        .post(format!("http://{}/api/session", host.address))
        .header(reqwest::header::ORIGIN, format!("https://{}", host.address))
        .json(&json!({"secret": secret.trim()}))
        .send()
        .await
        .unwrap();
    assert_eq!(login.status(), reqwest::StatusCode::OK);
    login.headers()[reqwest::header::SET_COOKIE]
        .to_str()
        .unwrap()
        .split(';')
        .next()
        .unwrap()
        .to_owned()
}

async fn observe_second_automatic_retry(
    host: &HostedTestRuntime,
    identity: &harness::embedding::HostIdentity,
    cookie: &str,
) {
    host.while_host_running(host.context.while_root_live(
        "second automatic retry projection",
        tokio::time::timeout(Duration::from_secs(30), async {
            loop {
                let state = host
                    .runtime
                    .store()
                    .embedded_inference_state(identity)
                    .unwrap();
                if let harness::embedding::HostInferenceState::AutoRetry {
                    ref block,
                    attempt: 2,
                    maximum: 2,
                    delay_ms: 3000,
                } = state
                {
                    let request = host
                        .runtime
                        .store()
                        .request(&block.request)
                        .unwrap()
                        .unwrap();
                    let previous = request
                        .parent
                        .expect("second retry follows first automatic request");
                    assert!(host
                        .runtime
                        .store()
                        .events(Some(&previous))
                        .unwrap()
                        .iter()
                        .any(|event| event.kind == "model_interrupted"));
                    let (mut socket, snapshot) =
                        crate::actor_host::m1_host_tests::browser_snapshot(host.address, cookie)
                            .await;
                    let snapshot: harness::server::browser_contract::Snapshot =
                        serde_json::from_value(snapshot["snapshot"].clone()).unwrap();
                    let projected = snapshot
                        .actors
                        .iter()
                        .find(|actor| actor.identity == *identity)
                        .expect("actual root missing from browser snapshot");
                    let matches = projected.inference.as_ref() == Some(&(&state).into());
                    socket.close(None).await.unwrap();
                    if matches {
                        return;
                    }
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        }),
    ))
    .await
    .unwrap()
    .unwrap()
    .expect("second Store retry must reach the browser through normal host polling");
}

#[tokio::test]
async fn driver_retains_actor_after_eof_and_explicit_recovery_never_redispatches_admitted_call() {
    for (interruption, action) in [
        (InterruptedResponse::EmptyEof, ContinuationAction::Send),
        (
            InterruptedResponse::CompletedCallEof,
            ContinuationAction::Send,
        ),
        (
            InterruptedResponse::CompletedCallEof,
            ContinuationAction::Retry,
        ),
    ] {
        let emit_call = matches!(interruption, InterruptedResponse::CompletedCallEof);
        let files = tempfile::tempdir().unwrap();
        let settings = hosted_test_settings(&files, 1);
        let repository = exomonad_worktree::testing::TestRepo::init().unwrap();
        repository
            .writer()
            .commit_file("README.md", "source\n", "seed")
            .unwrap();
        let run_root = files.path().join("run");
        let inputs = Arc::new(Mutex::new(Vec::new()));
        let calls = Arc::new(AtomicUsize::new(0));
        let (failure, observed_failure) = oneshot::channel();
        let host = HostedTestRuntime::start_with_factory(
            &settings,
            None,
            |config| {
                retained_config(config, repository.path(), &run_root);
                configure_command_path(config);
                if emit_call {
                    let owner = exomonad_node::command_resources::CommandResources::delegated(
                        exomonad_node::command_resources::CommandResourcePolicy {
                            general_bytes: 2 * exomonad_node::command_resources::GIB,
                            protected_bytes: 0,
                            swap_max_bytes: 0,
                            ..Default::default()
                        },
                    )
                    .expect("the isolated test delegates production command resources");
                    config.command_resources = Some(
                        exomonad_node::command_resources::CommandResourceClient::local(
                            owner,
                            exomonad_node::command_resources::CommandRunIdentity::new(
                                "isolated-test".into(),
                                1,
                            )
                            .unwrap(),
                        )
                        .unwrap(),
                    );
                }
            },
            {
                let inputs = Arc::clone(&inputs);
                let calls = Arc::clone(&calls);
                let effect_path = repository.path().join("restart-effect-executions");
                move |runtime, _| {
                    Arc::new(InterruptedStream {
                        runtime: Arc::downgrade(runtime),
                        inputs,
                        calls,
                        interruption,
                        effect_path: Some(effect_path),
                        failure: Mutex::new(Some(failure)),
                    })
                }
            },
        )
        .await
        .unwrap();
        host.run_scenario(|host| {
            Box::pin(async move {
                let cookie = browser_session(host).await;
                host.http_input("first explicit input").await.unwrap();
                let conversation = host
                    .context
                    .binding(host.context.actor.identity())
                    .unwrap()
                    .conversation()
                    .unwrap();
                let interrupted = observe_provider_round(
                    observed_failure,
                    tokio::time::timeout(Duration::from_secs(300), async {
                        if matches!(interruption, InterruptedResponse::EmptyEof) {
                            observe_second_automatic_retry(host, conversation.identity(), &cookie).await;
                        }
                        loop {
                            let frontier = host
                                .runtime
                                .store()
                                .embedded_round_frontier(conversation.identity())
                                .unwrap();
                            if frontier.pending_interruption.is_some()
                                && conversation.active_round().is_none()
                                && matches!(host.runtime.store().embedded_inference_state(conversation.identity()).unwrap(), harness::embedding::HostInferenceState::Blocked { .. })
                            {
                                break frontier;
                            }
                            tokio::time::sleep(Duration::from_millis(10)).await;
                        }
                    }),
                )
                .await
                .unwrap_or_else(|failure| panic!("{failure}"))
                .expect("cleanup-confirmed EOF must leave the actual actor attached and idle");
                assert!(host.context.actor.terminal().get().is_none());
                assert!(interrupted.settled_head.is_none());
                let pending = interrupted.pending_head.unwrap();
                assert!(host
                    .runtime
                    .store()
                    .events(Some(&pending))
                    .unwrap()
                    .iter()
                    .any(|event| event.kind == "model_interrupted"));
                let failed_attempts = if emit_call { 1 } else { 3 };
                assert_eq!(calls.load(Ordering::SeqCst), failed_attempts);
                assert_eq!(
                    host.runtime.store().embedded_round_frontier(conversation.identity()).unwrap().pending_head,
                    Some(pending.clone())
                );
                assert!(host.context.actor.terminal().get().is_none());
                let harness::embedding::HostInferenceState::Blocked { block } = host
                    .runtime
                    .store()
                    .embedded_inference_state(conversation.identity())
                    .unwrap()
                else {
                    panic!("exhausted retries or completed-item interruption must remain blocked");
                };
                assert_eq!(block.request, pending);
                let operation_id = harness::embedding::ClientOperationId(uuid::Uuid::new_v4());
                let command = match action {
                    ContinuationAction::Send => harness::server::HostCommand::Input {
                        target: conversation.identity().clone(),
                        text: "continue explicitly".into(),
                    },
                    ContinuationAction::Retry => harness::server::HostCommand::Recover {
                        target: conversation.identity().clone(),
                        expected_block: block.clone(),
                        mode: harness::embedding::RecoveryMode::Exact,
                    },
                };
                host.http_command(operation_id, command.clone()).await.unwrap();
                let receipt = host.command_receipt(&conversation.identity().run, operation_id).await.unwrap();
                match (&action, &receipt) {
                    (ContinuationAction::Send, harness::server::CommandReceiptOutcome::Admitted {
                        continuation: Some(harness::embedding::InputContinuation::Granted { block: granted }),
                        wake_error: None,
                        ..
                    }) => assert_eq!(granted, &block),
                    (ContinuationAction::Retry, harness::server::CommandReceiptOutcome::RecoveryAuthorized {
                        block: granted,
                        mode: harness::embedding::RecoveryMode::Exact,
                        ..
                    }) => assert_eq!(granted, &block),
                    _ => panic!("unexpected continuation receipt: {receipt:?}"),
                }
                let successor = wait_head(&host, None).await;
                host.http_command(operation_id, command.clone()).await.unwrap();
                assert_eq!(host.command_receipt(&conversation.identity().run, operation_id).await.unwrap(), receipt);
                let stale_operation = harness::embedding::ClientOperationId(uuid::Uuid::new_v4());
                host.http_command(stale_operation, harness::server::HostCommand::Recover {
                    target: conversation.identity().clone(),
                    expected_block: block.clone(),
                    mode: harness::embedding::RecoveryMode::Exact,
                }).await.unwrap();
                assert!(matches!(
                    host.command_receipt(&conversation.identity().run, stale_operation).await.unwrap(),
                    harness::server::CommandReceiptOutcome::ContinuationRefused {
                        block: refused,
                        reason: harness::embedding::ContinuationRefusal::Stale,
                        ..
                    } if refused == block
                ));
                assert_eq!(calls.load(Ordering::SeqCst), failed_attempts + 1,
                    "duplicate and stale operations must not request the provider");
                assert_eq!(
                    host.runtime
                        .store()
                        .request(&successor)
                        .unwrap()
                        .unwrap()
                        .parent,
                    Some(pending)
                );
                let retained_inputs = inputs.lock();
                assert_eq!(retained_inputs.len(), failed_attempts + 1);
                let continued = &retained_inputs[failed_attempts].1;
                for (text, expected) in [
                    ("first explicit input", 1),
                    ("continue explicitly", usize::from(matches!(action, ContinuationAction::Send))),
                ] {
                    assert_eq!(continued.input.iter().filter(|item| item.0["content"] == text).count(), expected);
                }
                for pair in retained_inputs.windows(2) {
                    assert_eq!(host.runtime.store().request(&pair[1].0).unwrap().unwrap().parent, Some(pair[0].0.clone()));
                }
                assert_eq!(
                    host.runtime.store().claims(&harness::model::CallId("effect-before-eof".into())).unwrap().len(),
                    usize::from(emit_call),
                    "continuation must preserve the original invocation ownership"
                );
                if emit_call {
                    assert_eq!(
                        continued.input.iter().filter(|item| item.0["type"] == "custom_tool_call" && item.0["call_id"] == "effect-before-eof").count(),
                        1
                    );
                    assert_eq!(
                        continued
                            .input
                            .iter()
                            .filter(|item| item.0["type"] == "custom_tool_call_output"
                                && item.0["call_id"] == "effect-before-eof")
                            .count(),
                        1
                    );
                    assert_eq!(
                        std::fs::read(repository.path().join("restart-effect-executions")).unwrap(),
                        b"x",
                        "the actual retained command must never execute again"
                    );
                }
                drop(retained_inputs);
                drop(conversation);
            })
        })
        .await;
    }
}

#[tokio::test]
async fn blocked_actor_can_be_retired_through_its_browser_owner_without_inference() {
    let files = tempfile::tempdir().unwrap();
    let settings = hosted_test_settings(&files, 1);
    let calls = Arc::new(AtomicUsize::new(0));
    let provider_calls = Arc::clone(&calls);
    let host = HostedTestRuntime::start_with_factory(
        &settings,
        None,
        |_| {},
        move |runtime, _| {
            Arc::new(InterruptedStream {
                runtime: Arc::downgrade(runtime),
                inputs: Arc::new(Mutex::new(Vec::new())),
                calls: Arc::clone(&provider_calls),
                interruption: InterruptedResponse::Incomplete,
                effect_path: None,
                failure: Mutex::new(None),
            })
        },
    )
    .await
    .unwrap();
    host.run_scenario(|host| {
        Box::pin(async move {
            host.http_input("interrupt this model round").await.unwrap();
            let conversation = host
                .context
                .binding(host.context.actor.identity())
                .unwrap()
                .conversation()
                .unwrap();
            host.while_host_running(host.context.while_root_live(
                "inference block",
                tokio::time::timeout(Duration::from_secs(30), async {
                    loop {
                        if matches!(
                            host.runtime
                                .store()
                                .embedded_inference_state(conversation.identity())
                                .unwrap(),
                            harness::embedding::HostInferenceState::Blocked {
                                block: harness::embedding::InferenceBlock {
                                    reason: harness::embedding::InferenceBlockReason::ResponseIncomplete {
                                        reason: Some(harness::transport::IncompleteReason::ContentFilter),
                                        ..
                                    },
                                    ..
                                }
                            }
                        ) && conversation.active_round().is_none()
                        {
                            break;
                        }
                        tokio::time::sleep(Duration::from_millis(10)).await;
                    }
                }),
            ))
            .await
            .unwrap()
            .unwrap()
            .unwrap();
            let interrupted = harness::embedding::ClientOperationId(uuid::Uuid::new_v4());
            host.http_command(
                interrupted,
                harness::server::HostCommand::Interrupt {
                    target: conversation.identity().clone(),
                    expected_round: harness::embedding::EmbeddedRoundId(uuid::Uuid::new_v4()),
                },
            )
            .await
            .unwrap();
            assert!(matches!(
                host.command_receipt(&conversation.identity().run, interrupted)
                    .await
                    .unwrap(),
                harness::server::CommandReceiptOutcome::Refused { .. }
            ));
            let retire = harness::embedding::ClientOperationId(uuid::Uuid::new_v4());
            host.http_command(
                retire,
                harness::server::HostCommand::Retire {
                    target: conversation.identity().clone(),
                },
            )
            .await
            .unwrap();
            assert!(matches!(
                host.command_receipt(&conversation.identity().run, retire)
                    .await
                    .unwrap(),
                harness::server::CommandReceiptOutcome::ControlRequested {
                    control: harness::server::CommandControl::Retire,
                    ..
                }
            ));
            assert_eq!(
                host.context.actor.terminal().get().unwrap().kind,
                exomonad_actor::ActorExitKind::Cancelled
            );
            assert_eq!(calls.load(Ordering::SeqCst), 1);
        })
    })
    .await;
}
