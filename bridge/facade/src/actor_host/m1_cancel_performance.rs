//! Production interrupt-to-owner-acknowledgment and subsequent cleanup samples.

use super::warm_cell_performance::{require_owned_daemon, ClientRequests, DaemonTrace};
use super::*;
use harness::model::{CallId, ConversationIdentity, OperationId, RequestId};
use harness::provider::CancellationAcknowledgment;
use harness::turn::JobOutput;
use std::collections::HashSet;
use std::time::Instant;
use tracing_subscriber::prelude::*;

const SAMPLES: usize = 50;
const WARMUP_INPUT: &str = "Warm the real cancellation measurement host.";
const WARMUP_CALL: &str = "cancel-performance-warmup";
fn sleep_source() -> String {
    tidepool_testing::fixture_source("bridge/facade/src/actor_host/m1_cancel_cell.hs")
}

struct IssuedCell {
    index: usize,
    operation: OperationId,
}

struct CancelTransport {
    clients: ClientRequests,
    issued: Mutex<HashSet<usize>>,
    warmup_issued: AtomicUsize,
    warmup_displayed: tokio::sync::Notify,
    calls: mpsc::UnboundedSender<IssuedCell>,
}

fn input_text(index: usize) -> String {
    format!("Start actual cancellation measurement cell {index}.")
}

impl CancelTransport {
    fn turn(&self, request_id: &RequestId, request: &ResponsesRequest) -> ResponsesTurn {
        if request
            .input
            .iter()
            .any(|item| cell_output_matches(item, WARMUP_CALL, "42"))
        {
            self.warmup_displayed.notify_one();
        }
        let index = (0..SAMPLES).rev().find(|index| {
            request
                .input
                .iter()
                .any(|item| item.0["content"] == input_text(*index))
                && !self.issued.lock().contains(index)
        });
        let item = if let Some(index) = index {
            assert!(
                self.issued.lock().insert(index),
                "one authored Sleep per input"
            );
            let (prefix, incarnation) = request.session_id.rsplit_once(':').unwrap();
            let (run, actor) = prefix.rsplit_once(':').unwrap();
            let call_id = format!("cancel-performance-cell-{index}");
            let operation = OperationId {
                origin: ConversationIdentity::Embedded {
                    run: run.into(),
                    actor: AgentPath(actor.into()),
                    incarnation: incarnation.into(),
                },
                request: request_id.clone(),
                call: CallId(call_id.clone()),
            };
            self.clients.issue(&operation);
            self.calls.send(IssuedCell { index, operation }).unwrap();
            harness::item::Item(
                json!({"type":"custom_tool_call", "call_id":call_id, "name":"haskell", "input":sleep_source()}),
            )
        } else if request
            .input
            .iter()
            .any(|item| item.0["content"] == WARMUP_INPUT)
            && self.warmup_issued.fetch_add(1, Ordering::SeqCst) == 0
        {
            harness::item::Item(
                json!({"type":"custom_tool_call", "call_id":WARMUP_CALL, "name":"haskell", "input":"_ <- display (40 + 2 :: Int)"}),
            )
        } else {
            harness::item::Item(json!({
                "type":"message", "role":"assistant", "phase":"final_answer",
                "content":[{"type":"output_text", "text":"Waiting for the next actual cancellation input."}]
            }))
        };
        ResponsesTurn {
            response_id: format!("cancel-measurement-response-{}", uuid::Uuid::new_v4()),
            items: vec![item],
            usage: Usage::default(),
        }
    }
}

#[async_trait]
impl ResponsesTransport for CancelTransport {
    async fn create(&self, _: ResponsesRequest) -> Result<ResponsesTurn, TransportError> {
        panic!("cancellation samples require exact durable Engine request identities")
    }

    async fn create_streaming_for_request(
        &self,
        request_id: &RequestId,
        request: ResponsesRequest,
        sink: mpsc::Sender<harness::transport::sse::StreamEvent>,
    ) -> Result<ResponsesTurn, TransportError> {
        let turn = self.turn(request_id, &request);
        for item in &turn.items {
            let _ = sink
                .send(harness::transport::sse::StreamEvent::ItemDone(item.clone()))
                .await;
        }
        Ok(turn)
    }
}

fn invocation_context(
    target: &harness::embedding::HostIdentity,
    operation: &OperationId,
) -> exomonad_tool::ToolInvocationContext {
    exomonad_tool::ToolInvocationContext {
        origin: exomonad_tool::ToolInvocationOrigin::Model(
            embedded_harness::original_operation(target, operation).unwrap(),
        ),
        call_id: operation.call.0.clone(),
        namespace: None,
    }
}

async fn submit_input(
    client: &reqwest::Client,
    api: &str,
    origin: &str,
    cookie: &str,
    target: &harness::embedding::HostIdentity,
    text: &str,
) {
    let response = client
        .post(format!("{api}/commands"))
        .header("Origin", origin)
        .header(reqwest::header::COOKIE, cookie)
        .json(&browser_input(target, text))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), reqwest::StatusCode::ACCEPTED);
}

async fn idle_projection(fixture: &HostedTestRuntime, cookie: &str) -> Value {
    tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            let (mut socket, snapshot) = browser_snapshot(fixture.address, cookie).await;
            socket.close(None).await.unwrap();
            let root = &snapshot["snapshot"]["actors"][0];
            if root["lifecycle"] == "waiting" && root["activeRound"].is_null() {
                return snapshot;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("the actual Engine round must clear before the next input")
}

async fn active_sleep(
    fixture: &HostedTestRuntime,
    operation: &OperationId,
    context: &exomonad_tool::ToolInvocationContext,
    cookie: &str,
) -> (
    harness::embedding::EmbeddedRoundId,
    tidepool_runtime::session::WorkbenchExecutionId,
) {
    tokio::time::timeout(Duration::from_secs(300), async {
        loop {
            let store = fixture.runtime.store();
            let claims = store.claims_for_operation(operation).unwrap();
            if let Some(claim) = claims.first() {
                assert_eq!(claims.len(), 1);
                assert_eq!(&claim.operation, operation);
                assert_eq!(claim.request, operation.request);
                assert_eq!(
                    claim.state,
                    harness::store::ClaimState::Pending,
                    "authored Sleep must remain pending before interruption"
                );
                assert!(fixture
                    .runtime
                    .scheduler()
                    .output(operation)
                    .await
                    .unwrap()
                    .is_none());
                if let Some(execution) = fixture.context.actor.hosted_workbench_waiting(context) {
                    let (mut socket, snapshot) = browser_snapshot(fixture.address, cookie).await;
                    socket.close(None).await.unwrap();
                    let root = &snapshot["snapshot"]["actors"][0];
                    if root["lifecycle"] == "running"
                        && !root["activeRound"].is_null()
                        && fixture
                            .context
                            .actor
                            .hosted_workbench_waiting(context)
                            .as_ref()
                            == Some(&execution)
                    {
                        let round = serde_json::from_value(root["activeRound"].clone()).unwrap();
                        return (round, execution);
                    }
                }
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("real Sleep did not reach its exact armed owner: {operation:?}"))
}

#[tokio::test]
#[ignore = "requires exclusive owned matched daemon and retained TIDEPOOL_PERFORMANCE_COMPILER_TRACE"]
async fn production_engine_store_active_cancellation_50() {
    let monotonic_origin = Instant::now();
    let trace_path = std::path::PathBuf::from(
        std::env::var_os("TIDEPOOL_PERFORMANCE_COMPILER_TRACE")
            .expect("retained actual daemon trace"),
    );
    assert!(trace_path.is_absolute());
    let socket = std::path::PathBuf::from(
        std::env::var_os(tidepool_extract_cmd::DAEMON_SOCKET_ENV)
            .expect("owned production compiler daemon"),
    );
    let endpoint = tidepool_extract_cmd::preflight_compiler_daemon(&socket).unwrap();
    let mut trace = DaemonTrace::open(&trace_path);
    let boots = trace
        .read()
        .into_iter()
        .filter(|row| row["message"] == "compiler daemon ready")
        .collect::<Vec<_>>();
    assert_eq!(boots.len(), 1, "one actual owned daemon boot");
    assert_eq!(boots[0]["producer"], endpoint.producer_hex());
    require_owned_daemon(&boots[0]);
    let epoch = boots[0]["daemon_epoch"].as_str().unwrap().to_owned();
    let clients = ClientRequests::default();
    tracing_subscriber::registry()
        .with(clients.clone())
        .try_init()
        .expect("isolated fixture owns tracing subscriber");
    let (calls, mut issued) = mpsc::unbounded_channel();
    let transport = Arc::new(CancelTransport {
        clients: clients.clone(),
        issued: Mutex::new(HashSet::new()),
        warmup_issued: AtomicUsize::new(0),
        warmup_displayed: tokio::sync::Notify::new(),
        calls,
    });
    let files = tempfile::tempdir().unwrap();
    let assets = files.path().join("assets");
    std::fs::create_dir_all(&assets).unwrap();
    std::fs::write(assets.join("index.html"), "<!doctype html>").unwrap();
    let secret_file = files.path().join("browser-secret");
    let secret = "offline-actual-cancellation-performance-secret";
    std::fs::write(&secret_file, secret).unwrap();
    let auth_file = files.path().join("codex-auth.json");
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
        context_capacity_tokens: 2_000_000,
        concurrent_jobs: 1,
    };
    let provider: Arc<dyn ResponsesTransport> = transport.clone();
    let fixture = HostedTestRuntime::start(&settings, &provider)
        .await
        .unwrap();
    let target = browser_target(&fixture.context);
    let api = format!("http://{}/api", fixture.address);
    let origin = format!("https://{}", fixture.address);
    let client = reqwest::Client::new();
    let login = client
        .post(format!("{api}/session"))
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
    submit_input(&client, &api, &origin, &cookie, &target, WARMUP_INPUT).await;
    tokio::time::timeout(
        Duration::from_secs(300),
        transport.warmup_displayed.notified(),
    )
    .await
    .expect("production Engine must return the real warm-up display 42");
    idle_projection(&fixture, &cookie).await;
    let mut operations = HashSet::new();
    for index in 0..SAMPLES {
        submit_input(&client, &api, &origin, &cookie, &target, &input_text(index)).await;
        let cell = tokio::time::timeout(Duration::from_secs(30), issued.recv())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(cell.index, index);
        assert!(
            operations.insert(cell.operation.clone()),
            "fifty distinct original operations"
        );
        let context = invocation_context(&target, &cell.operation);
        let (round, execution) = active_sleep(&fixture, &cell.operation, &context, &cookie).await;
        let store = fixture.runtime.store();
        let turns = store.replay_turns(&cell.operation.request).unwrap();
        assert!(
            turns
                .iter()
                .flat_map(|turn| &turn.model_response.items)
                .any(|item| {
                    item.0["type"] == "custom_tool_call"
                        && item.0["call_id"] == cell.operation.call.0
                        && item.0["input"] == sleep_source()
                }),
            "the exact armed call must retain its real single-Sleep authored source"
        );
        assert_eq!(
            tidepool_extract_cmd::preflight_compiler_daemon(&socket).unwrap(),
            endpoint
        );
        for row in trace.read() {
            if row["message"] == "compiler daemon ready" {
                panic!("daemon reboot invalidates the cancellation campaign: {row}");
            }
        }
        let interrupt_id = harness::embedding::ClientOperationId(uuid::Uuid::new_v4());
        let command = harness::server::ClientCommand::Host {
            operation_id: interrupt_id,
            command: harness::server::HostCommand::Interrupt {
                target: target.clone(),
                expected_round: round,
            },
        };
        let request = client
            .post(format!("{api}/commands"))
            .header("Origin", &origin)
            .header(reqwest::header::COOKIE, &cookie)
            .json(&command);
        // This observation is momentary. Only the real owner acknowledgment
        // below can establish that this interrupt actually stopped this call.
        assert_eq!(
            fixture
                .context
                .actor
                .hosted_workbench_waiting(&context)
                .as_ref(),
            Some(&execution)
        );
        let effect_active_ns = monotonic_origin.elapsed().as_nanos();
        let compiler_requests = tidepool_extract_cmd::extract_spawn_count();
        let started = Instant::now();
        let started_ns = started.duration_since(monotonic_origin).as_nanos();
        let scheduler = fixture.runtime.scheduler();
        let acknowledged = async {
            assert!(matches!(
                scheduler.wait(&cell.operation).await.unwrap(),
                JobOutput::CancelledWithReceipt(_)
            ));
            assert!(
                matches!(
                    scheduler
                        .cancellation_acknowledgment(&cell.operation)
                        .await
                        .unwrap(),
                    Some(CancellationAcknowledgment::StoppedWithReceipt(_))
                ),
                "only the retained native cancellation owner can acknowledge stop"
            );
            Instant::now()
        };
        let (acknowledged_at, response) = tokio::time::timeout(Duration::from_secs(8), async {
            tokio::join!(acknowledged, request.send())
        })
        .await
        .expect("actual active interrupt and acknowledgment must settle");
        assert_eq!(response.unwrap().status(), reqwest::StatusCode::ACCEPTED);
        let settled_ns = acknowledged_at.duration_since(monotonic_origin).as_nanos();
        assert_eq!(
            tidepool_extract_cmd::extract_spawn_count(),
            compiler_requests,
            "compilation must finish before the measured interrupt interval"
        );
        let compiler_requests = clients.requests(&cell.operation);
        assert!(
            !compiler_requests.is_empty(),
            "actual preparation must identify exact daemon invocations"
        );
        let operation_id = serde_json::to_string(&cell.operation).unwrap();
        eprintln!(
            "resident-performance {}",
            json!({
                "schema":1, "runner_id":"cancel_ack", "composition":"engine-store", "kind":"cancel_ack", "index":index,
                "started_ns":started_ns, "settled_ns":settled_ns, "elapsed_ns":settled_ns-started_ns,
                "completed":true, "daemon_epoch":epoch, "operation_id":operation_id,
                "original_operation":cell.operation, "compiler_requests":compiler_requests, "effect_active_ns":effect_active_ns,
                "acknowledged":true, "acknowledgment":"stopped", "workbench_execution_id":execution,
                "interrupt_operation_id":interrupt_id, "interrupted_round":round,
            })
        );
        tokio::time::timeout(Duration::from_secs(30), async {
            loop {
                let claims = store.claims_for_operation(&cell.operation).unwrap();
                assert_eq!(claims.len(), 1);
                assert_eq!(claims[0].operation, cell.operation);
                if claims[0].state == harness::store::ClaimState::Settled
                    && store
                        .replay_output_operation(&cell.operation)
                        .unwrap()
                        .is_some()
                {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(2)).await;
            }
        })
        .await
        .expect("exact cancelled output and claim must become durable");
        let cancelled_output = store
            .replay_output_operation(&cell.operation)
            .unwrap()
            .unwrap();
        assert_eq!(
            serde_json::from_str::<Value>(cancelled_output.0["output"].as_str().unwrap()).unwrap(),
            json!({"error":"job cancelled"}),
            "durable output must retain cancellation, never the authored post-Sleep success"
        );
        idle_projection(&fixture, &cookie).await;
        assert!(fixture
            .context
            .actor
            .hosted_workbench_waiting(&context)
            .is_none());
        assert!(
            fixture.context.actor.terminal().get().is_none(),
            "interrupt preserves the issuer actor"
        );
        let control = store
            .embedded_command(&target.run, interrupt_id)
            .unwrap()
            .unwrap()
            .receipt
            .unwrap();
        assert!(
            matches!(control.outcome, harness::server::CommandReceiptOutcome::ControlRequested {
            target: ref actual, control: harness::server::CommandControl::Interrupt,
        } if actual == &target),
            "real production interrupt receipt: {control:?}"
        );
        let cleaned_ns = monotonic_origin.elapsed().as_nanos();
        eprintln!(
            "resident-performance {}",
            json!({
                "schema":1, "runner_id":"cancel_ack", "composition":"engine-store", "kind":"cancel_cleanup", "index":index,
                "started_ns":settled_ns, "settled_ns":cleaned_ns, "elapsed_ns":cleaned_ns-settled_ns,
                "completed":true, "daemon_epoch":epoch, "operation_id":operation_id,
                "boundary":"owner-acknowledgment-to-durable-output-and-round-idle", "native_abort_confirmed":true,
            })
        );
    }
    assert_eq!(operations.len(), SAMPLES);
    assert_eq!(transport.issued.lock().len(), SAMPLES);
    fixture
        .stop()
        .await
        .expect("production host and resident forest acknowledge final cleanup");
}
