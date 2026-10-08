//! Durable restart uses the production host's journal, binding and actor owners.
use crate::actor_host::hosted_test_context::HostedTestRuntime;
use crate::actor_host::test_campaign::{
    hosted_script_provider, hosted_test_settings, next_hosted_script_round,
};
use crate::actor_host::{embedded_harness::EmbeddedHarnessRuntime, ActorHostConfig};
use harness::{
    engine::ResponsesTransport,
    item::Item,
    model::{AgentPath, RequestId},
    transport::{ResponsesRequest, ResponsesTurn, TransportError},
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
use tokio::sync::mpsc;

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
    async fn create(&self, _: ResponsesRequest) -> Result<ResponsesTurn, TransportError> {
        panic!("the production driver must use streamed dispatch");
    }

    async fn create_streaming(
        &self,
        request: ResponsesRequest,
        sink: mpsc::Sender<harness::transport::sse::StreamEvent>,
    ) -> Result<ResponsesTurn, TransportError> {
        self.inputs.lock().push(request);
        let attempt = self.calls.fetch_add(1, Ordering::SeqCst);
        if attempt > 0 {
            return Ok(final_turn());
        }
        let mut response = harness::transport::sse::ResponseAssembly::default();
        if self.emit_call {
            let event = response
                .accept(
                    &json!({"type":"response.output_item.done", "item": {
                        "type":"custom_tool_call", "call_id":"effect-before-eof", "name":"haskell",
                        "input": tidepool_testing::fixture_source("bridge/facade/src/actor_host/restart_effect_once.hs"),
                    }})
                    .to_string(),
                )
                .unwrap()
                .unwrap();
            sink.send(event).await.unwrap();
            let runtime = self
                .runtime
                .upgrade()
                .expect("the production runtime owns this provider request");
            tokio::time::timeout(Duration::from_secs(300), async {
                loop {
                    let claims = runtime
                        .store()
                        .claims(&harness::model::CallId("effect-before-eof".into()))
                        .unwrap();
                    if let [claim] = claims.as_slice() {
                        if let Some(output) =
                            runtime.scheduler().output(&claim.operation).await.unwrap()
                        {
                            assert!(
                                matches!(output, harness::turn::JobOutput::Completed(Ok(_))),
                                "{output:?}"
                            );
                            if runtime
                                .store()
                                .replay_output_operation(&claim.operation)
                                .unwrap()
                                .is_some()
                            {
                                break;
                            }
                        }
                    }
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
            })
            .await
            .expect("actual admitted effect must complete and persist before the streamed EOF");
        }
        response.finish()
    }
}

#[tokio::test]
async fn driver_retains_actor_after_eof_and_explicit_input_never_redispatches_admitted_call() {
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
        let host = HostedTestRuntime::start_with_factory(
            &settings,
            |config| retained_config(config, repository.path(), &run_root),
            {
                let inputs = Arc::clone(&inputs);
                let calls = Arc::clone(&calls);
                move |runtime, _| {
                    Arc::new(InterruptedStream {
                        runtime: Arc::downgrade(runtime),
                        inputs,
                        calls,
                        emit_call,
                    })
                }
            },
        )
        .await
        .unwrap();
        host.input("first explicit input").await.unwrap();
        let conversation = host
            .context
            .binding(host.context.actor.identity())
            .unwrap()
            .conversation()
            .unwrap();
        let interrupted = tokio::time::timeout(Duration::from_secs(300), async {
            loop {
                let frontier = host
                    .runtime
                    .store()
                    .embedded_round_frontier(conversation.identity())
                    .unwrap();
                if frontier.pending_interruption.is_some() && conversation.active_round().is_none()
                {
                    break frontier;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
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
        let predecessor = conversation.identity().clone();
        drop(conversation);
        host.stop().await.unwrap();

        let host = HostedTestRuntime::start_with_factory(
            &settings,
            |config| retained_config(config, repository.path(), &run_root),
            {
                let inputs = Arc::clone(&inputs);
                let calls = Arc::clone(&calls);
                move |runtime, _| {
                    Arc::new(InterruptedStream {
                        runtime: Arc::downgrade(runtime),
                        inputs,
                        calls,
                        emit_call,
                    })
                }
            },
        )
        .await
        .unwrap();
        let conversation = host
            .context
            .binding(host.context.actor.identity())
            .unwrap()
            .conversation()
            .unwrap();
        assert_ne!(conversation.identity().incarnation, predecessor.incarnation);
        assert!(host.context.actor.terminal().get().is_none());
        assert_eq!(
            host.runtime
                .store()
                .embedded_round_frontier(conversation.identity())
                .unwrap()
                .pending_head,
            Some(pending.clone())
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert_eq!(
            calls.load(Ordering::SeqCst),
            1,
            "a reopened interruption must not retry itself"
        );
        conversation
            .input("explicit-next", "operator", "continue explicitly")
            .await
            .unwrap();
        let successor = wait_head(&host, None).await;
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
        host.stop().await.unwrap();
    }
}
