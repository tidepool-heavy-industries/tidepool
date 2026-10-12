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
        emit_call: false,
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

async fn command_receipt(
    host: &HostedTestRuntime,
    run: &str,
    operation: harness::embedding::ClientOperationId,
) -> harness::server::CommandReceiptOutcome {
    host.while_host_running(tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            if let Some(receipt) = host
                .runtime
                .store()
                .embedded_command(run, operation)
                .unwrap()
                .and_then(|record| record.receipt)
            {
                return receipt.outcome;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }))
    .await
    .expect("host must remain available until its exact command settles")
    .expect("the browser command must settle its durable receipt")
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
    drop(conversation);
    host.stop().await.unwrap();
    drop(provider);

    let (provider, mut requests) = hosted_script_provider();
    let host = HostedTestRuntime::start_configured(&settings, &provider, |config| {
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
    host.stop().await.unwrap();
}

struct InterruptedStream {
    runtime: std::sync::Weak<EmbeddedHarnessRuntime>,
    inputs: Arc<Mutex<Vec<ResponsesRequest>>>,
    calls: Arc<AtomicUsize>,
    emit_call: bool,
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
            harness::transport::ResponsesRoute::Codex,
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
        self.inputs.lock().push(request.request().clone());
        let attempt = self.calls.fetch_add(1, Ordering::SeqCst);
        if attempt > 0 {
            return Ok(final_turn());
        }
        let mut response = harness::transport::sse::ResponseAssembly::default();
        if self.emit_call {
            let event = response
                .accept(
                    &json!({"type":"response.output_item.done", "output_index":0, "item": {
                        "type":"custom_tool_call", "call_id":"effect-before-eof", "name":"haskell",
                        "input": tidepool_testing::fixture_source("bridge/facade/src/actor_host/restart_effect_once.hs"),
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

#[tokio::test]
async fn driver_retains_actor_after_eof_and_explicit_recovery_never_redispatches_admitted_call() {
    for emit_call in [false, true] {
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
                    config.command_resources =
                        Some(exomonad_node::command_resources::CommandResourceClient::local(owner));
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
                        emit_call,
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
                host.input("first explicit input").await.unwrap();
                let conversation = host
                    .context
                    .binding(host.context.actor.identity())
                    .unwrap()
                    .conversation()
                    .unwrap();
                let interrupted = observe_provider_round(
                    observed_failure,
                    tokio::time::timeout(Duration::from_secs(300), async {
                        loop {
                            let frontier = host
                                .runtime
                                .store()
                                .embedded_round_frontier(conversation.identity())
                                .unwrap();
                            if frontier.pending_interruption.is_some()
                                && conversation.active_round().is_none()
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
                assert_eq!(calls.load(Ordering::SeqCst), 1);
                assert_eq!(
                    host.runtime
                        .store()
                        .embedded_round_frontier(conversation.identity())
                        .unwrap()
                        .pending_head,
                    Some(pending.clone())
                );
                assert!(host.context.actor.terminal().get().is_none());
                tokio::time::sleep(Duration::from_millis(100)).await;
                assert_eq!(calls.load(Ordering::SeqCst), 1);
                conversation
                    .input("explicit-next", "operator", "continue explicitly")
                    .await
                    .unwrap();
                tokio::time::sleep(Duration::from_millis(100)).await;
                assert_eq!(
                    calls.load(Ordering::SeqCst),
                    1,
                    "ordinary input must not release the durable inference block"
                );
                let harness::embedding::HostInferenceState::Blocked { block } = host
                    .runtime
                    .store()
                    .embedded_inference_state(conversation.identity())
                    .unwrap()
                else {
                    panic!("the actual interruption must remain blocked");
                };
                let operation_id = harness::embedding::ClientOperationId(uuid::Uuid::new_v4());
                let command = harness::server::HostCommand::Recover {
                    target: conversation.identity().clone(),
                    expected_block: block,
                    mode: harness::embedding::RecoveryMode::Exact,
                };
                host.http_command(operation_id, command.clone())
                    .await
                    .unwrap();
                assert!(matches!(
                    command_receipt(&host, &conversation.identity().run, operation_id).await,
                    harness::server::CommandReceiptOutcome::RecoveryAuthorized { .. }
                ));
                let successor = wait_head(&host, None).await;
                host.http_command(operation_id, command.clone())
                    .await
                    .unwrap();
                let stale_operation = harness::embedding::ClientOperationId(uuid::Uuid::new_v4());
                host.http_command(stale_operation, command).await.unwrap();
                assert!(matches!(
                    command_receipt(&host, &conversation.identity().run, stale_operation).await,
                    harness::server::CommandReceiptOutcome::Refused { .. }
                ));
                assert_eq!(
                    calls.load(Ordering::SeqCst),
                    2,
                    "retrying recovery must only return its retained receipt"
                );
                assert_eq!(
                    host.runtime
                        .store()
                        .request(&successor)
                        .unwrap()
                        .unwrap()
                        .parent,
                    Some(pending)
                );
                assert_eq!(calls.load(Ordering::SeqCst), 2);
                let retained_inputs = inputs.lock();
                let continued = &retained_inputs[1];
                for text in ["first explicit input", "continue explicitly"] {
                    assert_eq!(
                        continued
                            .input
                            .iter()
                            .filter(|item| item.0["content"] == text)
                            .count(),
                        1
                    );
                }
                if emit_call {
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
        |_| {},
        move |runtime, _| {
            Arc::new(InterruptedStream {
                runtime: Arc::downgrade(runtime),
                inputs: Arc::new(Mutex::new(Vec::new())),
                calls: Arc::clone(&provider_calls),
                emit_call: false,
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
                            harness::embedding::HostInferenceState::Blocked { .. }
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
            host.http_input("this remains queued while inference is blocked")
                .await
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
                command_receipt(host, &conversation.identity().run, interrupted).await,
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
                command_receipt(host, &conversation.identity().run, retire).await,
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
