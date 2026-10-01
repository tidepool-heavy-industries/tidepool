use super::*;
use async_trait::async_trait;
use harness::{
    engine::ResponsesTransport,
    item::{Item, ToolKind},
    model::{AgentPath, CallId, ConversationIdentity, OperationId},
    store::ClaimState,
    transport::{ResponsesRequest, ResponsesTurn, TransportError, Usage},
    turn::JobOutput,
};
use serde_json::json;
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Mutex as StdMutex,
};
use tokio::sync::{mpsc, oneshot, Mutex as AsyncMutex};

const CELL_CALL_ID: &str = "m1-late-output-cell";
const FIRST_INPUT: &str = "Start the resident Haskell operation.";
const CONTINUE_INPUT: &str = "Continue after the resident operation settles.";

struct PendingCellTransport {
    normal_rounds: AtomicUsize,
    declared_tools: StdMutex<Option<Vec<serde_json::Value>>>,
    compaction_seen: mpsc::UnboundedSender<()>,
    compaction_release: AsyncMutex<Option<oneshot::Receiver<()>>>,
    before_compaction: mpsc::UnboundedSender<(ResponsesRequest, oneshot::Sender<ResponsesTurn>)>,
    successor: mpsc::UnboundedSender<(ResponsesRequest, oneshot::Sender<ResponsesTurn>)>,
    late_output: mpsc::UnboundedSender<ResponsesRequest>,
}

fn final_turn(response_id: &str, text: &str) -> ResponsesTurn {
    ResponsesTurn {
        response_id: response_id.into(),
        items: vec![harness::item::Item(json!({
            "type":"message",
            "role":"assistant",
            "phase":"final_answer",
            "content":[{"type":"output_text","text":text}]
        }))],
        usage: Usage::default(),
    }
}

fn usage_turn(response_id: &str, text: &str, input_tokens: u64) -> ResponsesTurn {
    let mut turn = final_turn(response_id, text);
    // A final answer waits for pending jobs before another Engine iteration.
    // Commentary triggers compaction while the actual resident call is parked.
    turn.items[0].0["phase"] = json!("commentary");
    turn.usage.input_tokens = input_tokens;
    turn
}

pub(super) fn compressible_history(request: &ResponsesRequest) -> String {
    let input_bytes = serde_json::to_vec(&request.input)
        .expect("serialize issued input")
        .len();
    let bytes = input_bytes.checked_mul(2).unwrap().max(4096);
    assert!(bytes <= 1024 * 1024, "fixture history exceeds its bound");
    "x".repeat(bytes)
}

pub(super) fn assert_applied_compaction(
    host: &RunningBrowserHost,
    operation: &OperationId,
) -> harness::model::RequestId {
    let store = host.runtime.store();
    let events = store.events(None).expect("read compaction evidence");
    let evidence = events
        .iter()
        .filter(|event| matches!(event.kind.as_str(), "compaction" | "compaction_attempt"))
        .map(|event| {
            json!({"request":event.request.as_ref().map(|id| &id.0),
                "kind":event.kind,"payload":serde_json::from_str::<serde_json::Value>(&event.payload).unwrap()})
        })
        .collect::<Vec<_>>();
    eprintln!("m1-compaction-evidence {}", json!({"events":evidence}));
    assert!(
        events.iter().any(|event| {
            event.kind == "compaction_attempt"
                && serde_json::from_str::<serde_json::Value>(&event.payload).unwrap()["outcome"]
                    == "applied"
        }),
        "fixture must apply compaction rather than merely request a summary"
    );
    let claims = store.claims_for_operation(operation).unwrap();
    assert_eq!(
        claims.len(),
        2,
        "original and applied-compaction claimant only"
    );
    let inherited = claims
        .iter()
        .find(|claim| claim.request != operation.request)
        .expect("compaction inherits the qualified original operation");
    assert!(
        events.iter().any(|event| {
            event.kind == "compaction" && event.request.as_ref() == Some(&inherited.request)
        }),
        "inherited claimant must belong to the applied compaction boundary"
    );
    inherited.request.clone()
}

async fn wait_for_armed_cell(
    host: &RunningBrowserHost,
    target: &harness::embedding::HostIdentity,
    operation: &OperationId,
) {
    let context = exomonad_tool::ToolInvocationContext {
        origin: exomonad_tool::ToolInvocationOrigin::Model(
            embedded_harness::original_operation(target, operation).unwrap(),
        ),
        call_id: operation.call.0.clone(),
        namespace: None,
    };
    tokio::time::timeout(Duration::from_secs(180), async {
        loop {
            wait_for_cell_state(host, operation, CellState::Pending, Duration::from_secs(1)).await;
            if host
                .campaign
                .actor
                .hosted_workbench_waiting(&context)
                .is_some()
            {
                return;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("raw Haskell must finish preparation and arm its actual resident Sleep");
}

fn output_for_call(request: &ResponsesRequest) -> impl Iterator<Item = &harness::item::Item> {
    request.input.iter().filter(|item| {
        item.0["type"] == "custom_tool_call_output" && item.0["call_id"] == CELL_CALL_ID
    })
}

fn retains_declared_tools(transport: &PendingCellTransport, request: &ResponsesRequest) -> bool {
    let current = request.tools.iter().cloned().collect::<Vec<_>>();
    transport.declared_tools.lock().unwrap().as_ref() == Some(&current)
}

#[async_trait]
impl ResponsesTransport for PendingCellTransport {
    async fn create(&self, request: ResponsesRequest) -> Result<ResponsesTurn, TransportError> {
        if request.tools_allowed.as_ref().is_some_and(Vec::is_empty) {
            self.compaction_seen.send(()).map_err(|_| {
                TransportError::Stream("test closed before compaction began".into())
            })?;
            let release = self.compaction_release.lock().await.take().ok_or_else(|| {
                TransportError::Stream("compaction was requested more than once".into())
            })?;
            release.await.map_err(|_| {
                TransportError::Stream("test dropped the compaction release".into())
            })?;
            return Ok(final_turn(
                "m1-late-cell-compaction",
                "The resident operation is still pending.",
            ));
        }

        match self.normal_rounds.fetch_add(1, Ordering::SeqCst) + 1 {
            1 => {
                if !request.tools.iter().any(|tool| tool["name"] == "haskell") {
                    return Err(TransportError::Stream(
                        "the real root declaration did not expose the Haskell tool".into(),
                    ));
                }
                *self.declared_tools.lock().unwrap() = Some(request.tools.to_vec());
                Ok(ResponsesTurn {
                    response_id: "m1-late-cell-start".into(),
                    items: vec![harness::item::Item(json!({
                        "type":"custom_tool_call",
                        "call_id":CELL_CALL_ID,
                        "name":"haskell",
                        "input":"do { sleep (seconds 30); pure (40 + 2 :: Int) }"
                    }))],
                    usage: Usage::default(),
                })
            }
            2 => {
                if !retains_declared_tools(self, &request) {
                    return Err(TransportError::Stream(
                        "pre-compaction tool declarations changed during the resident call".into(),
                    ));
                }
                if !request
                    .input
                    .iter()
                    .any(|item| item.0["call_id"] == CELL_CALL_ID)
                {
                    return Err(TransportError::Stream(
                        "post-compaction request lost the pending Haskell call".into(),
                    ));
                }
                if output_for_call(&request).next().is_some() {
                    return Err(TransportError::Stream(
                        "pending Haskell call already had an output".into(),
                    ));
                }
                let (reply, response) = oneshot::channel();
                self.before_compaction
                    .send((request, reply))
                    .map_err(|_| TransportError::Stream("test dropped pending request".into()))?;
                response
                    .await
                    .map_err(|_| TransportError::Stream("test dropped pending response".into()))
            }
            3 => {
                if !retains_declared_tools(self, &request) {
                    return Err(TransportError::Stream(
                        "post-compaction tool declarations changed during the resident call".into(),
                    ));
                }
                if !request
                    .input
                    .iter()
                    .any(|item| item.0["call_id"] == CELL_CALL_ID)
                    || output_for_call(&request).next().is_some()
                {
                    return Err(TransportError::Stream(
                        "post-compaction request did not preserve exactly the pending call".into(),
                    ));
                }
                let (reply, response) = oneshot::channel();
                self.successor
                    .send((request, reply))
                    .map_err(|_| TransportError::Stream("test dropped successor request".into()))?;
                response
                    .await
                    .map_err(|_| TransportError::Stream("test dropped successor response".into()))
            }
            4 => {
                if !request
                    .input
                    .iter()
                    .any(|item| item.0["content"] == CONTINUE_INPUT)
                {
                    return Err(TransportError::Stream(
                        "late-output request did not include the explicit continuation input"
                            .into(),
                    ));
                }
                if !retains_declared_tools(self, &request) {
                    return Err(TransportError::Stream(
                        "late-output tool declarations changed during the resident call".into(),
                    ));
                }
                let outputs = output_for_call(&request).count();
                if outputs != 1 {
                    return Err(TransportError::Stream(format!(
                        "late Haskell output must be delivered exactly once, got {outputs}"
                    )));
                }
                let output = output_for_call(&request).next().expect("count checked");
                if !cell_output_matches(output, CELL_CALL_ID, "42") {
                    return Err(TransportError::Stream(format!(
                        "late Haskell output did not retain the exact committed result: {output:?}"
                    )));
                }
                self.late_output.send(request).map_err(|_| {
                    TransportError::Stream("test dropped late-output request".into())
                })?;
                Ok(final_turn(
                    "m1-late-cell-finished",
                    "The late resident result was delivered once.",
                ))
            }
            other => Err(TransportError::Stream(format!(
                "unexpected normal provider round {other}"
            ))),
        }
    }
}

#[derive(Clone, Copy, Debug)]
enum CellState {
    Pending,
    Completed,
}

fn resident_cell_operation(
    host: &RunningBrowserHost,
    target: &harness::embedding::HostIdentity,
) -> OperationId {
    let claims = host
        .runtime
        .store()
        .claims(&CallId(CELL_CALL_ID.into()))
        .expect("read admitted resident tool claim");
    assert_eq!(
        claims.len(),
        1,
        "one original resident operation was admitted"
    );
    let operation = claims[0].operation.clone();
    assert_eq!(
        operation.origin,
        ConversationIdentity::Embedded {
            run: target.run.clone(),
            actor: target.actor.clone(),
            incarnation: target.incarnation.clone(),
        },
        "resident operation must belong to the actual embedded host",
    );
    operation
}

async fn wait_for_cell_state(
    host: &RunningBrowserHost,
    operation: &OperationId,
    expected: CellState,
    timeout: Duration,
) {
    let result = tokio::time::timeout(timeout, async {
        loop {
            let store = host.runtime.store();
            let claims = store
                .claims_for_operation(operation)
                .expect("read original resident tool claim");
            let claim = claims
                .iter()
                .find(|claim| claim.request == operation.request)
                .expect("resident operation must retain its original claimant");
            assert!(claims.iter().all(|claim| &claim.operation == operation),
                "compacted claimants must retain the qualified original operation");
            let persisted = store
                .replay_output_operation(operation)
                .expect("read original resident output");
            let scheduler = host.runtime.scheduler();
            let output = scheduler
                .output(operation)
                .await
                .expect("original resident operation must remain in the scheduler");
            match output {
                None => {
                    assert!(claims.iter().all(|claim| claim.state == ClaimState::Pending));
                    assert!(persisted.is_none(), "pending operation already has output");
                    if matches!(expected, CellState::Pending) {
                        assert!(
                            scheduler
                                .provider_completion(operation)
                                .await
                                .expect("read original provider completion")
                                .is_none(),
                            "pending operation's provider already completed",
                        );
                        return;
                    }
                }
                Some(output @ JobOutput::Completed(Ok(_))) => {
                    assert!(
                        matches!(expected, CellState::Completed),
                        "resident operation completed before the pending barrier",
                    );
                    assert!(
                        cell_output_matches(
                            &Item::tool_output(&operation.call, ToolKind::Custom, &output),
                            CELL_CALL_ID,
                            "42",
                        ),
                        "scheduler did not retain the exact committed resident result: {output:?}",
                    );
                    if claim.state == ClaimState::Settled {
                        assert!(claims.iter().all(|claim| claim.state == ClaimState::Settled));
                        let persisted = persisted.expect("settled claim must retain its output");
                        assert!(
                            cell_output_matches(&persisted, CELL_CALL_ID, "42"),
                            "persisted original output must match the completed result: {persisted:?}",
                        );
                        return;
                    }
                    assert_eq!(claim.state, ClaimState::Pending);
                }
                Some(output) => panic!("resident operation did not complete successfully: {output:?}"),
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await;
    if result.is_err() {
        panic!(
            "resident operation did not reach {expected:?}: {}",
            host.cell_settlement_diagnostic(CELL_CALL_ID).await,
        );
    }
}

pub(super) async fn submit_host_input(
    client: &reqwest::Client,
    address: std::net::SocketAddr,
    origin: &str,
    cookie: &str,
    target: &harness::embedding::HostIdentity,
    text: &str,
) {
    let response = client
        .post(format!("http://{address}/api/commands"))
        .header("Origin", origin)
        .header(reqwest::header::COOKIE, cookie)
        .json(&browser_input(target, text))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), reqwest::StatusCode::ACCEPTED);
}

#[tokio::test]
async fn real_host_retains_one_late_haskell_output_across_compaction() {
    let files = tempfile::tempdir().unwrap();
    let assets = files.path().join("assets");
    std::fs::create_dir_all(&assets).unwrap();
    std::fs::write(assets.join("index.html"), "<!doctype html>").unwrap();
    let secret = "m1-late-output-secret-is-long-enough";
    let secret_file = files.path().join("browser-secret");
    std::fs::write(&secret_file, secret).unwrap();
    let auth_file = files.path().join("unused-auth.json");
    std::fs::write(&auth_file, "{}").unwrap();
    let settings = crate::exomonad::EmbeddedLaunchConfig {
        listen: "127.0.0.1:0".parse().unwrap(),
        public_origin_scheme: crate::exomonad::EmbeddedPublicOriginScheme::Https,
        public_origin: None,
        asset_root: assets,
        browser_auth: crate::exomonad::EmbeddedBrowserAuth::Secret,
        session_secret_file: Some(secret_file),
        codex_auth_file: auth_file,
        context_capacity_tokens: 200_000,
        concurrent_jobs: 1,
    };

    let (compaction_tx, mut compaction_rx) = mpsc::unbounded_channel();
    let (compaction_release_tx, compaction_release_rx) = oneshot::channel();
    let (before_compaction_tx, mut before_compaction_rx) = mpsc::unbounded_channel();
    let (successor_tx, mut successor_rx) = mpsc::unbounded_channel();
    let (late_output_tx, mut late_output_rx) = mpsc::unbounded_channel();
    let transport: Arc<dyn ResponsesTransport> = Arc::new(PendingCellTransport {
        normal_rounds: AtomicUsize::new(0),
        declared_tools: StdMutex::new(None),
        compaction_seen: compaction_tx,
        compaction_release: AsyncMutex::new(Some(compaction_release_rx)),
        before_compaction: before_compaction_tx,
        successor: successor_tx,
        late_output: late_output_tx,
    });
    let host = RunningBrowserHost::start(&settings, &transport)
        .await
        .expect("production embedded host should start");
    let client = reqwest::Client::new();
    let origin = format!("https://{}", host.address);
    let login = client
        .post(format!("http://{}/api/session", host.address))
        .header("Origin", &origin)
        .json(&json!({ "secret": secret }))
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
    let target = browser_target(&host.campaign);

    submit_host_input(
        &client,
        host.address,
        &origin,
        &cookie,
        &target,
        FIRST_INPUT,
    )
    .await;
    let (pending_request, pending_reply) =
        tokio::time::timeout(Duration::from_secs(60), before_compaction_rx.recv())
            .await
            .expect("provider did not receive the pending-call turn")
            .expect("provider dropped the pending-call turn");
    assert!(pending_request
        .input
        .iter()
        .any(|item| item.0["call_id"] == CELL_CALL_ID));
    assert_eq!(output_for_call(&pending_request).count(), 0);
    let operation = resident_cell_operation(&host, &target);
    wait_for_armed_cell(&host, &target, &operation).await;
    pending_reply
        .send(usage_turn(
            "m1-late-cell-trigger-compaction",
            &compressible_history(&pending_request),
            100_001,
        ))
        .expect("Engine stopped before compaction threshold was applied");
    tokio::time::timeout(Duration::from_secs(30), compaction_rx.recv())
        .await
        .expect("provider did not enter compaction")
        .expect("provider dropped compaction signal");
    wait_for_cell_state(
        &host,
        &operation,
        CellState::Pending,
        Duration::from_secs(30),
    )
    .await;
    compaction_release_tx
        .send(())
        .expect("compaction request was no longer waiting");

    let (successor_request, successor_reply) =
        tokio::time::timeout(Duration::from_secs(30), successor_rx.recv())
            .await
            .expect("provider did not receive the post-compaction request")
            .expect("provider dropped post-compaction request");
    assert!(successor_request
        .input
        .iter()
        .any(|item| item.0["call_id"] == CELL_CALL_ID));
    assert_eq!(output_for_call(&successor_request).count(), 0);
    assert_applied_compaction(&host, &operation);
    wait_for_cell_state(
        &host,
        &operation,
        CellState::Pending,
        Duration::from_secs(30),
    )
    .await;
    // Hold the issued response until completion so the final answer can stop
    // this turn without automatically requesting a pending job's late output.
    wait_for_cell_state(
        &host,
        &operation,
        CellState::Completed,
        Duration::from_secs(60),
    )
    .await;
    successor_reply
        .send(final_turn(
            "m1-late-cell-waiting",
            "The resident Haskell result is retained.",
        ))
        .expect("Engine stopped before retained output could settle");
    let (mut socket, snapshot) = browser_snapshot_until(host.address, &cookie, "waiting").await;
    assert_eq!(snapshot["snapshot"]["conversations"][0]["state"], "idle");
    socket.close(None).await.unwrap();
    submit_host_input(
        &client,
        host.address,
        &origin,
        &cookie,
        &target,
        CONTINUE_INPUT,
    )
    .await;
    let late_request = tokio::time::timeout(Duration::from_secs(30), late_output_rx.recv())
        .await
        .expect("late output was not sent to the model")
        .expect("provider dropped late-output request");
    let outputs = output_for_call(&late_request).collect::<Vec<_>>();
    assert_eq!(outputs.len(), 1, "late output must be retained once");
    assert!(
        cell_output_matches(outputs[0], CELL_CALL_ID, "42"),
        "late output must retain the completed cell's exact committed value: {:?}",
        outputs[0],
    );
    let claims = host
        .runtime
        .store()
        .claims(&CallId(CELL_CALL_ID.into()))
        .expect("read resident tool claim");
    assert_eq!(claims.len(), 2, "one original and one compacted claimant");
    assert!(claims
        .iter()
        .all(|claim| claim.operation == operation && claim.state == ClaimState::Settled));
    assert_eq!(target.actor, AgentPath("/root".into()));

    host.stop()
        .await
        .expect("production host cleanup should succeed");
}
