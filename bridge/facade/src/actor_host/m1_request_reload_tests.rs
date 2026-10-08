//! Request-owned installed handlers across actor reload and pending compaction.

use super::*;
use exomonad_actor::ResidentToolEndpoint;
use harness::{
    item::Item,
    model::{CallId, ConversationIdentity, OperationId, RequestId},
    store::ClaimState,
    transport::sse::StreamEvent,
    turn::JobOutput,
};
use tokio::sync::oneshot;

const OLD_CALL: &str = "reload-old-issued-call";
const NEW_CALL: &str = "reload-new-issued-call";
fn request_reload_spec() -> String {
    tidepool_testing::fixture_source(
        "bridge/facade/src/actor_host/fixtures/request_reload_agent_spec.hs",
    )
}

struct IssuedRequest {
    id: Option<RequestId>,
    request: ResponsesRequest,
    reply: oneshot::Sender<ResponsesTurn>,
}

struct ReloadTransport(mpsc::UnboundedSender<IssuedRequest>);

impl ReloadTransport {
    async fn offer(
        &self,
        id: Option<RequestId>,
        request: ResponsesRequest,
    ) -> Result<ResponsesTurn, TransportError> {
        let (reply, response) = oneshot::channel();
        self.0
            .send(IssuedRequest { id, request, reply })
            .map_err(|_| TransportError::Stream("reload test stopped receiving requests".into()))?;
        response
            .await
            .map_err(|_| TransportError::Stream("reload test abandoned the issued request".into()))
    }
}

#[async_trait]
impl ResponsesTransport for ReloadTransport {
    async fn create(&self, request: ResponsesRequest) -> Result<ResponsesTurn, TransportError> {
        self.offer(None, request).await
    }

    async fn create_streaming_for_request(
        &self,
        id: &RequestId,
        request: ResponsesRequest,
        sink: mpsc::Sender<StreamEvent>,
    ) -> Result<ResponsesTurn, TransportError> {
        let turn = self.offer(Some(id.clone()), request).await?;
        for item in &turn.items {
            let _ = sink.send(StreamEvent::ItemDone(item.clone())).await;
        }
        Ok(turn)
    }
}

async fn issued(receiver: &mut mpsc::UnboundedReceiver<IssuedRequest>) -> IssuedRequest {
    tokio::time::timeout(Duration::from_secs(90), receiver.recv())
        .await
        .expect("production Engine did not issue the next request")
        .expect("production Engine dropped the transport")
}

#[derive(serde::Serialize)]
#[serde(rename_all = "snake_case")]
enum MessagePhase {
    Commentary,
    FinalAnswer,
}

fn message_turn(id: &str, tokens: u64, phase: MessagePhase) -> ResponsesTurn {
    ResponsesTurn {
        response_id: id.into(),
        items: vec![Item(json!({
            "type":"message", "role":"assistant", "phase":phase,
            "content":[{"type":"output_text","text":"The typed operation remains retained."}]
        }))],
        usage: Usage {
            input_tokens: tokens,
            ..Usage::default()
        },
    }
}

fn bounded_receipt(value: &Value) -> Value {
    json!({
        "status": value["status"],
        "total": value["total"],
        "nextIndex": value["nextIndex"],
        "items": value["items"].as_array().into_iter().flatten().take(4).map(|item| {
            json!({
                "status":item["status"],
                "failureLayer":item["failureLayer"],
                "output":item["output"].as_str().map(|text| text.chars().take(512).collect::<String>()),
            })
        }).collect::<Vec<_>>(),
    })
}

async fn assert_pending(host: &HostedTestRuntime, operation: &OperationId, phase: &str) {
    let output = host.runtime.scheduler().output(operation).await.unwrap();
    if let Some(output) = output {
        let terminal = match &output {
            JobOutput::Completed(Ok(value)) => {
                json!({"kind":"completed", "receipt":bounded_receipt(value)})
            }
            JobOutput::Completed(Err(error)) => json!({
                "kind":"failed", "message":error.message().chars().take(512).collect::<String>(),
                "metadata":error.metadata(),
            }),
            JobOutput::Cancelled => json!({"kind":"cancelled"}),
            JobOutput::CancelledWithReceipt(receipt) => {
                json!({"kind":"cancelled", "receipt":receipt})
            }
            JobOutput::Interrupted => json!({"kind":"interrupted"}),
            JobOutput::CancellationUnconfirmed(error) => json!({
                "kind":"cancellation_unconfirmed", "message":error.chars().take(512).collect::<String>(),
            }),
        };
        let store = host.runtime.store();
        let retained = store
            .replay_output_operation(operation)
            .unwrap()
            .and_then(|item| {
                item.0["output"]
                    .as_str()
                    .and_then(|text| serde_json::from_str::<Value>(text).ok())
            });
        let claims = store.claims_for_operation(operation).unwrap();
        let evidence = json!({
            "phase":phase, "operation":operation, "scheduler":terminal,
            "retained":retained.as_ref().map(bounded_receipt),
            "claims":claims.iter().take(4).map(|claim| json!({
                "request":claim.request, "outputHash":claim.output,
                "state":match claim.state {
                    ClaimState::Pending => "pending",
                    ClaimState::Settled => "settled",
                    ClaimState::Interrupted => "interrupted",
                },
            })).collect::<Vec<_>>(),
        });
        if let Some(root) = std::env::var_os("TIDEPOOL_TEST_ARTIFACT_ROOT") {
            let root = std::path::PathBuf::from(root);
            std::fs::create_dir_all(&root).unwrap();
            std::fs::write(
                root.join(format!("{phase}-terminal.json")),
                serde_json::to_vec_pretty(&evidence).unwrap(),
            )
            .unwrap();
        }
        panic!("typed operation unexpectedly terminal at pending boundary: {evidence}");
    }
}

fn probe_turn(id: &str, call: &str, pause: u32) -> ResponsesTurn {
    ResponsesTurn {
        response_id: id.into(),
        items: vec![Item(json!({
            "type":"function_call", "name":"probe", "call_id":call,
            "arguments":serde_json::to_string(&json!({"number":40,"delay":pause})).unwrap()
        }))],
        usage: Usage::default(),
    }
}

fn outputs<'a>(request: &'a ResponsesRequest, call: &'a str) -> impl Iterator<Item = &'a Item> {
    request
        .input
        .iter()
        .filter(move |item| item.0["type"] == "function_call_output" && item.0["call_id"] == call)
}

fn assert_value(item: &Item, call: &str, expected: &str) {
    assert_eq!(item.0["type"], "function_call_output");
    assert_eq!(item.0["call_id"], call);
    let response: Value = serde_json::from_str(item.0["output"].as_str().unwrap()).unwrap();
    use tidepool_runtime::session::{WorkbenchItemStatus, WorkbenchRunStatus};
    assert!(
        [WorkbenchRunStatus::Committed, WorkbenchRunStatus::Completed]
            .into_iter()
            .any(|status| response["status"] == serde_json::to_value(status).unwrap()),
        "typed tool failed: {response}",
    );
    assert_eq!(response["total"], 1);
    assert_eq!(response["nextIndex"], 1);
    let [receipt] = response["items"].as_array().unwrap().as_slice() else {
        panic!("typed call must retain one receipt: {response}");
    };
    assert_eq!(
        receipt["status"],
        serde_json::to_value(WorkbenchItemStatus::Committed).unwrap()
    );
    assert_eq!(receipt["output"].as_str().unwrap().trim(), expected);
}

fn operation(host: &HostedTestRuntime, request: &RequestId, call: &str) -> OperationId {
    host.runtime
        .store()
        .recorded_operation_for_request(request, &CallId(call.into()))
        .unwrap()
        .expect("issued typed operation must have its original durable claim")
}

async fn completed(host: &HostedTestRuntime, operation: &OperationId, expected: &str) {
    let result = tokio::time::timeout(Duration::from_secs(90), async {
        loop {
            match host.runtime.scheduler().output(operation).await.unwrap() {
                None => {}
                Some(JobOutput::Completed(Ok(_))) => {
                    let store = host.runtime.store();
                    if let Some(retained) = store.replay_output_operation(operation).unwrap() {
                        assert_value(&retained, &operation.call.0, expected);
                        let claims = store.claims_for_operation(operation).unwrap();
                        assert!(!claims.is_empty());
                        assert!(claims.iter().all(|claim| {
                            claim.operation == *operation && claim.state == ClaimState::Settled
                        }));
                        return;
                    }
                }
                Some(other) => panic!("typed operation failed: {other:?}"),
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await;
    assert!(
        result.is_ok(),
        "{}",
        host.cell_settlement_diagnostic(&operation.call.0).await
    );
}

#[tokio::test]
async fn real_host_pins_typed_handler_across_reload_and_pending_compaction() {
    let files = tempfile::tempdir().unwrap();
    let assets = files.path().join("assets");
    std::fs::create_dir(&assets).unwrap();
    std::fs::write(assets.join("index.html"), "<!doctype html>").unwrap();
    let secret = "request-reload-secret-is-long-enough";
    let secret_file = files.path().join("secret");
    std::fs::write(&secret_file, secret).unwrap();
    let auth_file = files.path().join("unused-auth.json");
    std::fs::write(&auth_file, "{}").unwrap();
    let settings = EmbeddedLaunchConfig {
        listen: "127.0.0.1:0".parse().unwrap(),
        public_origin_scheme: crate::exomonad::EmbeddedPublicOriginScheme::Https,
        public_origin: None,
        asset_root: assets,
        browser_auth: crate::exomonad::EmbeddedBrowserAuth::Secret,
        session_secret_file: Some(secret_file),
        provider: crate::exomonad::EmbeddedModelProvider::Codex,
        credential_file: auth_file,
        context_capacity_tokens: 200_000,
        concurrent_jobs: 2,
    };
    let (requests, mut receiver) = mpsc::unbounded_channel();
    let transport: Arc<dyn ResponsesTransport> = Arc::new(ReloadTransport(requests));
    let host = HostedTestRuntime::start_configured(&settings, &transport, |config| {
        let authored = config.workspace.join(".exomonad");
        std::fs::create_dir_all(&authored).unwrap();
        std::fs::write(authored.join("AgentSpec.hs"), request_reload_spec()).unwrap();
        crate::exomonad::write_fixture_project_config(&authored, "test-model", |project| {
            project.haskell.source_roots = vec![".".into()];
            project.haskell.spec = Some("AgentSpec.agentSpec".into());
        });
        config.workspace_inputs = Some(
            crate::exomonad::workspace::FrozenWorkspace::load(
                &config.workspace,
                &config.run_directory.path(),
            )
            .unwrap(),
        );
    })
    .await
    .unwrap();
    let client = reqwest::Client::new();
    let origin = format!("https://{}", host.address);
    let login = client
        .post(format!("http://{}/api/session", host.address))
        .header("Origin", &origin)
        .json(&json!({"secret":secret}))
        .send()
        .await
        .unwrap();
    assert_eq!(login.status(), reqwest::StatusCode::OK);
    let cookie = login.headers()[reqwest::header::SET_COOKIE]
        .to_str()
        .unwrap()
        .split(';')
        .next()
        .unwrap()
        .to_owned();
    let target = browser_target(&host.context);
    real_host_late_output_tests::submit_host_input(
        &client,
        host.address,
        &origin,
        &cookie,
        &target,
        "Run the old issued typed request.",
    )
    .await;

    let first = issued(&mut receiver).await;
    let original_request = first.id.clone().expect("normal Engine request identity");
    assert!(first
        .request
        .tools
        .iter()
        .any(|tool| tool["name"] == "probe"));
    let declared = first.request.tools.clone();
    // The transport already owns the request. Reload before its call is emitted.
    std::fs::write(
        host.context.config.workspace.join(".exomonad/AgentSpec.hs"),
        request_reload_spec().replace("offset = 2", "offset = 3"),
    )
    .unwrap();
    // The issued provider request stays held while its exact actor reloads.
    // This production projection only routes to the already-owned actor; it
    // creates neither an installation nor additional admission authority.
    let reloaded = exomonad_actor::ResidentInteractivePolicy::local(host.context.actor.clone())
        .dispatch_boxed(exomonad_tool::ToolInvocation {
            context: None,
            name: "reload_agent_spec".into(),
            arguments: exomonad_tool::ToolArguments::Structured(json!({})),
        })
        .await
        .expect("actual actor-local reload authority must answer")
        .into_json()
        .expect("reload receipt must encode");
    assert!(reloaded.to_string().contains("swapped"), "{reloaded}");
    let compiled_after_reload = tidepool_extract_cmd::extract_spawn_count();
    first
        .reply
        .send(probe_turn("old-issued-handler", OLD_CALL, 60))
        .unwrap();

    let before_compaction = issued(&mut receiver).await;
    let old = operation(&host, &original_request, OLD_CALL);
    assert_eq!(old.request, original_request);
    assert_eq!(
        old.origin,
        ConversationIdentity::Embedded {
            run: target.run.clone(),
            actor: target.actor.clone(),
            incarnation: target.incarnation.clone()
        }
    );
    let context = exomonad_tool::ToolInvocationContext {
        origin: exomonad_tool::ToolInvocationOrigin::Model(
            embedded_harness::original_operation(&target, &old).unwrap(),
        ),
        call_id: OLD_CALL.into(),
        namespace: None,
    };
    tokio::time::timeout(Duration::from_secs(90), async {
        while host
            .context
            .actor
            .hosted_workbench_waiting(&context)
            .is_none()
        {
            assert_pending(&host, &old, "before-sleep-armed").await;
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("old installed handler must park in actual resident Sleep");
    assert_eq!(before_compaction.request.tools, declared);
    assert_eq!(outputs(&before_compaction.request, OLD_CALL).count(), 0);
    let mut trigger = message_turn(
        "compact-pending-typed-call",
        100_001,
        MessagePhase::Commentary,
    );
    trigger.items[0].0["content"][0]["text"] = json!(
        real_host_late_output_tests::compressible_history(&before_compaction.request)
    );
    before_compaction.reply.send(trigger).unwrap();

    let compact = issued(&mut receiver).await;
    assert!(compact
        .request
        .tools_allowed
        .as_ref()
        .is_some_and(Vec::is_empty));
    assert_pending(&host, &old, "compaction-issued").await;
    compact
        .reply
        .send(message_turn(
            "pending-typed-summary",
            0,
            MessagePhase::FinalAnswer,
        ))
        .unwrap();
    let successor = issued(&mut receiver).await;
    let successor_request = successor.id.clone().expect("compacted successor identity");
    assert_ne!(successor_request, original_request);
    assert_eq!(successor.request.tools, declared);
    assert!(successor
        .request
        .input
        .iter()
        .any(|item| { item.0["type"] == "function_call" && item.0["call_id"] == OLD_CALL }));
    assert_eq!(outputs(&successor.request, OLD_CALL).count(), 0);
    assert_eq!(
        real_host_late_output_tests::assert_applied_compaction(&host, &old),
        successor_request,
    );
    let inherited = host.runtime.store().claims_for_operation(&old).unwrap();
    assert_eq!(inherited.len(), 2, "original and compacted claimant only");
    assert!(inherited
        .iter()
        .any(|claim| claim.request == successor_request));
    assert!(inherited
        .iter()
        .all(|claim| claim.operation == old && claim.state == ClaimState::Pending));
    successor
        .reply
        .send(probe_turn("new-installed-handler", NEW_CALL, 0))
        .unwrap();

    let after_new_call = issued(&mut receiver).await;
    let new = operation(&host, &successor_request, NEW_CALL);
    assert_eq!(new.request, successor_request);
    assert_eq!(new.origin, old.origin);
    completed(&host, &new, "Number 43").await;
    assert_eq!(outputs(&after_new_call.request, OLD_CALL).count(), 0);
    // Retain the already-issued model request while the original Sleep settles.
    // The final answer then sees both jobs ready; their outputs enter later input.
    completed(&host, &old, "Number 42").await;
    after_new_call
        .reply
        .send(message_turn(
            "new-result-is-retained",
            0,
            MessagePhase::FinalAnswer,
        ))
        .unwrap();
    let (mut socket, snapshot) = browser_snapshot_until(host.address, &cookie, "waiting").await;
    assert_eq!(snapshot["snapshot"]["conversations"][0]["state"], "idle");
    socket.close(None).await.unwrap();
    assert_eq!(
        tidepool_extract_cmd::extract_spawn_count(),
        compiled_after_reload,
        "old and new installed typed handlers must issue no compiler requests"
    );

    real_host_late_output_tests::submit_host_input(
        &client,
        host.address,
        &origin,
        &cookie,
        &target,
        "Read both retained typed results.",
    )
    .await;
    let final_request = issued(&mut receiver).await;
    assert_eq!(final_request.request.tools, declared);
    for (call, expected) in [(OLD_CALL, "Number 42"), (NEW_CALL, "Number 43")] {
        let retained = outputs(&final_request.request, call).collect::<Vec<_>>();
        assert_eq!(
            retained.len(),
            1,
            "each original typed result must appear exactly once"
        );
        assert_value(retained[0], call, expected);
    }
    assert_eq!(
        host.runtime
            .store()
            .claims(&CallId(OLD_CALL.into()))
            .unwrap()
            .len(),
        2
    );
    assert_eq!(
        host.runtime
            .store()
            .claims(&CallId(NEW_CALL.into()))
            .unwrap()
            .len(),
        1
    );
    final_request
        .reply
        .send(message_turn(
            "both-original-results-once",
            0,
            MessagePhase::FinalAnswer,
        ))
        .unwrap();
    let (mut socket, _) = browser_snapshot_until(host.address, &cookie, "waiting").await;
    socket.close(None).await.unwrap();
    assert!(
        receiver.try_recv().is_err(),
        "completed operations must not replay"
    );
    host.stop().await.unwrap();
}
