//! Actual browser driver over the production embedded host and resident actor.

use super::browser_process::BrowserProcess;
use super::*;
use tokio::sync::oneshot;

const RAW_INPUT: &str = "Run one real Haskell cell and retain its answer.";
const TYPED_INPUT: &str = "Call the installed probe twice with different numbers.";
const CANCEL_INPUT: &str = "Start a cancellable resident cell.";
const CONTINUE_INPUT: &str = "Continue after the interrupted cell.";

struct Barrier {
    id: String,
    phase: &'static str,
    request_id: Option<harness::model::RequestId>,
    request: ResponsesRequest,
    release: oneshot::Sender<()>,
}

#[derive(Default)]
struct ScriptState {
    requests: usize,
    raw_issued: bool,
    raw_completed: bool,
    typed_issued: usize,
    typed_completed: bool,
    cancel_issued: bool,
    continued: bool,
}

struct BrowserTransport {
    barriers: mpsc::UnboundedSender<Barrier>,
    state: Mutex<ScriptState>,
}

fn final_answer(text: &str) -> harness::item::Item {
    harness::item::Item(json!({
        "type":"message", "role":"assistant", "phase":"final_answer",
        "content":[{"type":"output_text", "text":text}]
    }))
}

fn real_cell_returned_42(item: &harness::item::Item) -> bool {
    cell_output_matches(item, "browser-real-cell", "42")
}

fn typed_result_matches(item: &harness::item::Item, call_id: &str, expected: &str) -> bool {
    if item.0["type"] != "function_call_output" || item.0["call_id"] != call_id {
        return false;
    }
    let Some(output) = item.0["output"].as_str() else {
        return false;
    };
    let Ok(response) = serde_json::from_str::<Value>(output) else {
        return false;
    };
    matches!(response["status"].as_str(), Some("completed" | "committed"))
        && response["total"] == 1
        && response["nextIndex"] == 1
        && response["items"].as_array().is_some_and(|items| {
            items.len() == 1
                && items[0]["status"] == "committed"
                && items[0]["output"]
                    .as_str()
                    .is_some_and(|output| output.trim() == expected)
        })
}

fn validate_completed_replies(input: &[harness::item::Item]) -> Result<(), String> {
    for item in input {
        let Some(call_id) = item.0["call_id"].as_str() else {
            continue;
        };
        let matches = match (item.0["type"].as_str(), call_id) {
            (Some("custom_tool_call_output"), "browser-real-cell") => real_cell_returned_42(item),
            (Some("function_call_output"), "browser-typed-first") => {
                typed_result_matches(item, call_id, "42")
            }
            (Some("function_call_output"), "browser-typed-second") => {
                typed_result_matches(item, call_id, "43")
            }
            _ => continue,
        };
        // A delivered tool result is terminal. Waiting for another result on
        // this call hides a settled failure behind the browser's phase timeout.
        if !matches {
            let output = item.0["output"].as_str().unwrap_or("<missing text output>");
            return Err(format!(
                "browser call {call_id} settled without its expected result: {}",
                output.chars().take(1024).collect::<String>(),
            ));
        }
    }
    Ok(())
}

fn probe_call(call_id: &str, number: i64) -> harness::item::Item {
    harness::item::Item(json!({
        "type":"function_call", "name":"probe", "call_id":call_id,
        "arguments":serde_json::to_string(&json!({"number":number})).unwrap(),
    }))
}

fn typed_reply_signature(item: &harness::item::Item) -> Value {
    let output = item.0["output"]
        .as_str()
        .and_then(|output| serde_json::from_str::<Value>(output).ok());
    json!({
        "type": item.0["type"],
        "callId": item.0["call_id"],
        "response": output.as_ref().map(|response| json!({
            "keys": response.as_object().map(|object| object.keys().collect::<Vec<_>>()),
            "status": response["status"],
            "nextIndex": response["nextIndex"],
            "total": response["total"],
            "items": response["items"].as_array().map(|items| items.iter().take(4).map(|item| {
                json!({
                    "status": item["status"],
                    "outputPreview": item["output"].as_str().map(|value| value.chars().take(64).collect::<String>()),
                    "outputIs42": item["output"].as_str().is_some_and(|value| value.trim() == "42"),
                    "outputIs43": item["output"].as_str().is_some_and(|value| value.trim() == "43"),
                })
            }).collect::<Vec<_>>()),
        })),
    })
}

impl BrowserTransport {
    async fn create_for_request(
        &self,
        request: ResponsesRequest,
        request_id: Option<&harness::model::RequestId>,
    ) -> Result<ResponsesTurn, TransportError> {
        for item in request.input.iter().filter(|item| {
            matches!(
                item.0["call_id"].as_str(),
                Some("browser-typed-first" | "browser-typed-second")
            ) && item.0["type"] == "function_call_output"
        }) {
            eprintln!(
                "browser gate typed reply signature={}",
                typed_reply_signature(item)
            );
        }
        for item in request.input.iter().filter(|item| {
            item.0["type"] == "custom_tool_call_output" && item.0["call_id"] == "browser-real-cell"
        }) {
            let response = item.0["output"]
                .as_str()
                .and_then(|output| serde_json::from_str::<Value>(output).ok());
            let signature = response.as_ref().map(|response| {
                json!({
                    "keys": response.as_object().map(|object| object.keys().collect::<Vec<_>>()),
                    "status": response["status"],
                    "nextIndex": response["nextIndex"],
                    "total": response["total"],
                    "items": response["items"].as_array().map(|items| items.iter().map(|item| {
                        json!({"status":item["status"], "outputIs42":item["output"].as_str().is_some_and(|value| value.trim() == "42")})
                    }).collect::<Vec<_>>()),
                })
            });
            eprintln!("browser gate raw-cell reply signature={signature:?}");
        }
        let (id, phase, item) = {
            let mut state = self.state.lock();
            state.requests += 1;
            if state.requests > 16 {
                return Err(TransportError::Stream(
                    "browser scenario exceeded request budget".into(),
                ));
            }
            let contains_input = |text| request.input.iter().any(|item| item.0["content"] == text);
            let result_present = request.input.iter().any(real_cell_returned_42);
            let (phase, item) = if !state.raw_issued && contains_input(RAW_INPUT) {
                state.raw_issued = true;
                (
                    "raw_input",
                    harness::item::Item(json!({
                        "type":"custom_tool_call", "name":"haskell",
                        "call_id":"browser-real-cell", "input":"_ <- display (40 + 2 :: Int)"
                    })),
                )
            } else if !state.raw_completed && result_present {
                state.raw_completed = true;
                ("raw_result", final_answer("The resident cell returned 42."))
            } else if state.raw_completed && state.typed_issued == 0 && contains_input(TYPED_INPUT)
            {
                state.typed_issued = 1;
                ("typed_first", probe_call("browser-typed-first", 40))
            } else if state.typed_issued == 1
                && request
                    .input
                    .iter()
                    .any(|item| typed_result_matches(item, "browser-typed-first", "42"))
            {
                state.typed_issued = 2;
                ("typed_second", probe_call("browser-typed-second", 41))
            } else if state.typed_issued == 2
                && !state.typed_completed
                && request
                    .input
                    .iter()
                    .any(|item| typed_result_matches(item, "browser-typed-second", "43"))
            {
                state.typed_completed = true;
                (
                    "typed_result",
                    final_answer("Installed typed tools returned 42 and 43."),
                )
            } else if state.typed_completed && !state.cancel_issued && contains_input(CANCEL_INPUT)
            {
                state.cancel_issued = true;
                (
                    "cancel_input",
                    harness::item::Item(json!({
                        "type":"custom_tool_call", "name":"haskell",
                        "call_id":"browser-cancellable-cell",
                        "async":true,
                        "input":"do { sleep (seconds 30); _ <- display (99 :: Int); pure () }"
                    })),
                )
            } else if state.cancel_issued && !state.continued && contains_input(CONTINUE_INPUT) {
                state.continued = true;
                (
                    "continued",
                    final_answer("Continued after the interrupted cell."),
                )
            } else if state.cancel_issued && !state.continued {
                (
                    "cancel_wait",
                    final_answer("Waiting for the cancellable resident cell."),
                )
            } else if state.raw_issued && !state.raw_completed {
                ("raw_wait", final_answer("Waiting for the resident cell."))
            } else if state.typed_issued > 0 && !state.typed_completed {
                (
                    "typed_wait",
                    final_answer("Waiting for the installed tool result."),
                )
            } else {
                return Err(TransportError::Stream(
                    "unexpected browser scenario request".into(),
                ));
            };
            (format!("browser-request-{}", state.requests), phase, item)
        };
        let required_kind = match item.0["type"].as_str() {
            Some("custom_tool_call") => Some("custom"),
            Some("function_call") => Some("function"),
            _ => None,
        };
        if let Some(kind) = required_kind {
            if !request.tools.iter().any(|tool| {
                tool["type"] == kind
                    && tool["name"] == item.0["name"]
                    && (kind != "function" || tool["strict"] == true)
                    && (item.0["async"] != true || tool["async"] == true)
            }) {
                return Err(TransportError::Stream(format!(
                    "browser scripted {phase} call requires installed {kind} tool {} with its selected execution mode in its issuing request",
                    item.0["name"],
                )));
            }
        }
        let (release, released) = oneshot::channel();
        self.barriers
            .send(Barrier {
                id: id.clone(),
                phase,
                request_id: request_id.cloned(),
                request,
                release,
            })
            .map_err(|_| {
                TransportError::Stream("browser host closed its barrier receiver".into())
            })?;
        released.await.map_err(|_| {
            TransportError::Stream("browser host abandoned a provider barrier".into())
        })?;
        Ok(ResponsesTurn {
            response_id: id,
            items: vec![item],
            usage: Usage::default(),
        })
    }
}

#[async_trait]
impl ResponsesTransport for BrowserTransport {
    async fn create(&self, request: ResponsesRequest) -> Result<ResponsesTurn, TransportError> {
        self.create_for_request(request, None).await
    }

    async fn create_streaming_for_request(
        &self,
        request_id: &harness::model::RequestId,
        request: ResponsesRequest,
        sink: mpsc::Sender<harness::transport::sse::StreamEvent>,
    ) -> Result<ResponsesTurn, TransportError> {
        let turn = self.create_for_request(request, Some(request_id)).await?;
        for item in &turn.items {
            let _ = sink
                .send(harness::transport::sse::StreamEvent::ItemDone(item.clone()))
                .await;
        }
        Ok(turn)
    }
}

fn required_path(name: &str) -> Result<PathBuf, String> {
    std::env::var_os(name)
        .map(PathBuf::from)
        .ok_or_else(|| format!("browser gate requires declared {name}"))
}

async fn verify_cancelled_resident_call(
    fixture: &HostedTestRuntime,
    expected_operation: &harness::model::OperationId,
) -> Result<(), String> {
    let target = browser_target(&fixture.context);
    let call = harness::model::CallId("browser-cancellable-cell".into());
    let claims = fixture
        .runtime
        .store()
        .claims(&call)
        .map_err(|error| error.to_string())?;
    let [claim] = claims.as_slice() else {
        return Err("cancellable resident call must have one retained originating claim".into());
    };
    let expected_origin = harness::model::ConversationIdentity::Embedded {
        run: target.run,
        actor: target.actor,
        incarnation: target.incarnation,
    };
    if claim.operation != *expected_operation
        || claim.operation.origin != expected_origin
        || claim.operation.request != claim.request
    {
        return Err("cancellation evidence belongs to another originating operation".into());
    }
    let scheduler = fixture.runtime.scheduler();
    if !matches!(
        scheduler
            .output(&claim.operation)
            .await
            .map_err(|error| error.to_string())?,
        Some(harness::turn::JobOutput::CancelledWithReceipt(_))
    ) {
        return Err("resident call did not retain confirmed cancellation".into());
    }
    if !matches!(
        scheduler
            .cancellation_acknowledgment(&claim.operation)
            .await
            .map_err(|error| error.to_string())?,
        Some(harness::provider::CancellationAcknowledgment::StoppedWithReceipt(_))
    ) {
        return Err("resident cancellation owner did not confirm cleanup".into());
    }
    Ok(())
}

async fn pending_sleep_operation(
    fixture: &HostedTestRuntime,
) -> Result<Option<harness::model::OperationId>, String> {
    let call = harness::model::CallId("browser-cancellable-cell".into());
    let claims = fixture
        .runtime
        .store()
        .claims(&call)
        .map_err(|error| error.to_string())?;
    let [claim] = claims.as_slice() else {
        return if claims.is_empty() {
            Ok(None)
        } else {
            Err("cancellable resident call has multiple originating operations".into())
        };
    };
    let target = browser_target(&fixture.context);
    let context = exomonad_tool::ToolInvocationContext {
        origin: exomonad_tool::ToolInvocationOrigin::Model(
            embedded_harness::original_operation(&target, &claim.operation)
                .map_err(|error| error.to_string())?,
        ),
        call_id: claim.operation.call.0.clone(),
        namespace: None,
    };
    let expected_origin = harness::model::ConversationIdentity::Embedded {
        run: target.run,
        actor: target.actor,
        incarnation: target.incarnation,
    };
    if claim.call_id != call
        || claim.operation.call != call
        || claim.operation.origin != expected_origin
        || claim.operation.request != claim.request
    {
        return Err("pending sleep evidence belongs to another embedded operation".into());
    }
    if claim.state != harness::store::ClaimState::Pending
        || fixture
            .runtime
            .scheduler()
            .output(&claim.operation)
            .await
            .map_err(|error| error.to_string())?
            .is_some()
    {
        return Err("cancellable resident operation settled before interruption".into());
    }
    if fixture
        .context
        .actor
        .hosted_workbench_waiting(&context)
        .is_none()
    {
        return Ok(None);
    }

    let actor = fixture.context.actor.identity();
    let graph = fixture.context.forest.inspect_host_graph();
    let Some(node) = graph.into_iter().find(|node| node.actor == actor) else {
        return Ok(None);
    };
    match node.workbench {
        exomonad_actor::ActorWorkbenchPosture::AwaitingEffect {
            input_unit_index: 0,
            total: 1,
            effect,
        } if effect == "sleep" => Ok(Some(claim.operation.clone())),
        _ => Ok(None),
    }
}

async fn drive_browser(
    fixture: &HostedTestRuntime,
    secret: &str,
    mut barriers: mpsc::UnboundedReceiver<Barrier>,
) -> Result<(), String> {
    let node = required_path("TIDEPOOL_BROWSER_NODE")?;
    let driver = required_path("TIDEPOOL_BROWSER_DRIVER")?;
    let browsers = required_path("PLAYWRIGHT_BROWSERS_PATH")?;
    let mut process = BrowserProcess::spawn(&node, &driver, &browsers)?;
    let identity = browser_target(&fixture.context);
    let ready = json!({
        "type":"ready", "version":1, "base_url":format!("http://{}", fixture.address),
        "session_secret":secret, "actor":{"run":identity.run,"name":identity.actor.0,"incarnation":identity.incarnation},
        "scenario":{"steps":[
            {"action":"input", "text":RAW_INPUT,"retry_unresolved":true,"wait_for_receipt":false,"provider_barriers":[
                {"phase":"raw_input","expected_request_text":RAW_INPUT},
                {"until_phase":"raw_result","timeout_ms":300000,"allowed_intermediate_phases":["raw_wait"],"expected_request_text":"42"}
            ], "wait_for_text":"The resident cell returned 42."},
            {"action":"reload"}, {"action":"assert_settled_retry"},
            {"action":"input", "text":TYPED_INPUT, "wait_for_receipt":false, "provider_barriers":[
                {"phase":"typed_first", "expected_request_text":TYPED_INPUT},
                {"until_phase":"typed_second", "allowed_intermediate_phases":["typed_wait"], "expected_request_text":"42"},
                {"until_phase":"typed_result", "allowed_intermediate_phases":["typed_wait"], "expected_request_text":"43"}
            ], "wait_for_text":"Installed typed tools returned 42 and 43."},
            {"action":"reload"}, {"action":"assert_settled_retry"},
            {"action":"input","text":CANCEL_INPUT,"provider_barriers":[
                {"phase":"cancel_input","expected_request_text":CANCEL_INPUT},
                {"phase":"cancel_wait","timeout_ms":300000,"expected_request_text":CANCEL_INPUT}
            ]},
            {"action":"interrupt"}, {"action":"wait_actor_state","state":"waiting"},
            {"action":"input","text":CONTINUE_INPUT,"provider_barriers":[
                {"phase":"continued","expected_request_text":CONTINUE_INPUT}
            ],"wait_for_text":"Continued after the interrupted cell."},
            {"action":"retire"}
        ]}
    });
    // Includes compiler preparation for both real cells; control deadlines
    // below independently bound interruption and browser readiness.
    let journey_started = tokio::time::Instant::now();
    let mut last_phase = "driver_startup";
    let outcome = tokio::time::timeout(Duration::from_secs(420), async {
        process.send_frame(&ready).await?;
        let mut started = false;
        let mut waiting: Option<Barrier> = None;
        let mut raw_completed = false;
        let mut typed_completed = false;
        let mut typed_compile_count = None;
        let mut cancel_wait = false;
        let mut cancel_pending_at = None;
        let mut cancel_operation = None;
        let mut continued = false;
        let mut root_retired = false;
        let readiness_deadline = tokio::time::Instant::now() + Duration::from_secs(30);
        loop {
            tokio::select! {
                biased;
                terminal = fixture.context.actor.terminal().wait(), if !root_retired => {
                    if !continued || last_phase != "continued" || waiting.is_some()
                        || terminal.kind != exomonad_actor::ActorExitKind::Cancelled {
                        return Err(format!(
                            "browser root exited at phase={last_phase} ({:?}): {}",
                            terminal.kind, terminal.summary,
                        ));
                    }
                    // The final browser retire command is verified by its real
                    // control receipt and the retained cleanup below.
                    root_retired = true;
                }
                _ = tokio::time::sleep_until(readiness_deadline), if !started => {
                    return Err("browser did not acknowledge readiness".to_owned());
                }
                barrier = barriers.recv(), if started && waiting.is_none() => {
                    let barrier = barrier.ok_or("scripted provider closed before browser completion")?;
                    validate_completed_replies(&barrier.request.input)?;
                    last_phase = barrier.phase;
                    eprintln!("browser gate phase={last_phase} elapsed={:?}", journey_started.elapsed());
                    if barrier.phase == "typed_first" {
                        typed_compile_count = Some(tidepool_extract_cmd::extract_spawn_count());
                    }
                    if barrier.phase == "typed_result" {
                        if typed_compile_count != Some(tidepool_extract_cmd::extract_spawn_count()) {
                            return Err("first and repeated installed tool calls submitted compiler work".into());
                        }
                        typed_completed = true;
                    }
                    if barrier.phase == "cancel_wait" {
                        cancel_operation = Some(fixture.context.while_root_live(
                            "browser pending native sleep effect",
                            tokio::time::timeout(Duration::from_secs(300), async {
                                loop {
                                    if let Some(operation) = pending_sleep_operation(fixture).await? {
                                        return Ok::<_, String>(operation);
                                    }
                                    tokio::time::sleep(Duration::from_millis(10)).await;
                                }
                            }),
                        ).await.map_err(|error| error.to_string())?
                            .map_err(|_| "cancellable cell exceeded its compilation/effect-admission budget before reaching the captured native sleep effect")??);
                        cancel_pending_at = Some(tokio::time::Instant::now());
                    }
                    if barrier.phase == "continued" {
                        let operation = cancel_operation.as_ref().ok_or(
                            "cancellation completed without an exact pending operation witness",
                        )?;
                        verify_cancelled_resident_call(fixture, operation).await?;
                        if !cancel_pending_at.is_some_and(|started| started.elapsed() < Duration::from_secs(8)) {
                            return Err("interrupt did not stop the pending 30-second cell promptly".into());
                        }
                    }
                    raw_completed |= barrier.phase == "raw_result";
                    cancel_wait |= barrier.phase == "cancel_wait";
                    continued |= barrier.phase == "continued";
                    process.send_frame(&json!({"type":"provider_barrier", "id":barrier.id,
                        "phase":barrier.phase, "request_id":barrier.request_id, "request_summary":{"input":barrier.request.input}})).await?;
                    waiting = Some(barrier);
                }
                frame = process.next_frame() => {
                    let frame = frame?.ok_or("browser exited without a result")?;
                    match frame["type"].as_str() {
                        Some("driver_started") if !started => {
                            started = true;
                            last_phase = "driver_started";
                            eprintln!("browser gate phase={last_phase} elapsed={:?}", journey_started.elapsed());
                        }
                        Some("provider_release") => {
                            let barrier = waiting.take().ok_or("browser released an absent barrier")?;
                            if frame["id"] != barrier.id { return Err("browser released another barrier identity".into()); }
                            barrier.release.send(()).map_err(|_| "provider abandoned its barrier")?;
                        }
                        Some("driver_result") => {
                            if frame["ok"] != true {
                                let detail = frame["detail"].as_str().unwrap_or("driver rejected the journey");
                                return Err(format!("browser journey failed: {}", detail.replace(secret, "[test-secret]")));
                            }
                            if !started || waiting.is_some() || !raw_completed || !typed_completed || !cancel_wait || !continued {
                                return Err("browser finished before the real provider scenario completed".into());
                            }
                            return Ok(());
                        }
                        _ => return Err("browser emitted an unexpected protocol frame".into()),
                    }
                }
            }
        }
    }).await.unwrap_or_else(|_| Err(format!(
        "browser journey exceeded its deadline at phase={last_phase}, elapsed={:?}",
        journey_started.elapsed(),
    )));
    process.finish(outcome, secret).await
}

pub(super) async fn production_browser_journey() {
    let test_started = tokio::time::Instant::now();
    let assets = required_path("EXOMONAD_EMBEDDED_ASSET_ROOT").unwrap();
    assert!(
        assets.join("index.html").is_file(),
        "matched browser assets must exist"
    );
    let files = tempfile::tempdir().unwrap();
    let secret = "private-scripted-browser-test-secret-with-no-live-credentials";
    let secret_file = files.path().join("session-secret");
    let auth_file = files.path().join("unused-offline-auth.json");
    std::fs::write(&secret_file, secret).unwrap();
    std::fs::write(&auth_file, "{}").unwrap();
    let settings = EmbeddedLaunchConfig {
        listen: "127.0.0.1:0".parse().unwrap(),
        public_origin_scheme: crate::exomonad::EmbeddedPublicOriginScheme::Http,
        public_origin: None,
        asset_root: assets,
        browser_auth: crate::exomonad::EmbeddedBrowserAuth::Secret,
        session_secret_file: Some(secret_file),
        provider: crate::exomonad::EmbeddedModelProvider::Codex,
        credential_file: auth_file,
        context_capacity_tokens: 2_000_000,
        concurrent_jobs: 1,
    };
    let (barriers, barrier_rx) = mpsc::unbounded_channel();
    let transport = Arc::new(BrowserTransport {
        barriers,
        state: Mutex::new(ScriptState::default()),
    });
    let host_transport: Arc<dyn ResponsesTransport> = transport.clone();
    let host_started = tokio::time::Instant::now();
    let fixture = HostedTestRuntime::start_configured(&settings, &host_transport, |config| {
        let authored = config.workspace.join(".exomonad");
        std::fs::create_dir_all(&authored).unwrap();
        std::fs::write(
            authored.join("AgentSpec.hs"),
            tidepool_testing::fixture_source(
                "bridge/facade/src/actor_host/fixtures/browser_agent_spec.hs",
            ),
        )
        .unwrap();
        crate::exomonad::write_fixture_project_config(&authored, "test-model", |project| {
            project.haskell.source_roots = vec![".".into()];
            project.haskell.spec = Some("AgentSpec.agentSpec".into());
        });
        test_campaign::commit_workspace(&config.workspace);
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
    eprintln!(
        "browser gate host_startup elapsed={:?}",
        host_started.elapsed()
    );
    let outcome = drive_browser(&fixture, secret, barrier_rx).await;
    if outcome.is_err() {
        let store = fixture.runtime.store();
        for call_id in [
            "browser-real-cell",
            "browser-typed-first",
            "browser-typed-second",
            "browser-cancellable-cell",
        ] {
            eprintln!(
                "browser gate {}",
                fixture.cell_settlement_diagnostic(call_id).await
            );
            for claim in store
                .claims(&harness::model::CallId(call_id.into()))
                .unwrap_or_default()
                .iter()
                .take(4)
            {
                let retained = store.replay_output_operation(&claim.operation);
                let retained_signature = retained
                    .as_ref()
                    .ok()
                    .and_then(|item| item.as_ref())
                    .map(typed_reply_signature);
                let scheduler = fixture.runtime.scheduler().output(&claim.operation).await;
                let scheduler_signature = match &scheduler {
                    Ok(Some(harness::turn::JobOutput::Completed(Ok(value)))) => {
                        Some(typed_reply_signature(&harness::item::Item(json!({
                            "type":"function_call_output", "call_id":call_id,
                            "output":serde_json::to_string(value).unwrap(),
                        }))))
                    }
                    _ => None,
                };
                eprintln!(
                    "browser gate call={call_id} retained_signature={retained_signature:?} scheduler_signature={scheduler_signature:?}"
                );
            }
        }
        if let Ok(agents) = store.list_agents() {
            for agent in agents.iter().take(4) {
                eprintln!(
                    "browser gate durable agent={} state={:?} head={:?}",
                    agent.path.0, agent.state, agent.head_request
                );
            }
        }
        if let Ok(events) = store.events(None) {
            let kinds = events
                .iter()
                .rev()
                .take(16)
                .map(|event| (&event.kind, &event.request))
                .collect::<Vec<_>>();
            eprintln!("browser gate final durable event kinds={kinds:?}");
        }
    }
    let terminal = fixture.context.actor.terminal().get();
    let retirement_cleanup = fixture.context.actor.terminal().cleanup();
    let cleanup_started = tokio::time::Instant::now();
    let cleanup = fixture.stop().await;
    eprintln!(
        "browser gate cleanup elapsed={:?} test_total={:?}",
        cleanup_started.elapsed(),
        test_started.elapsed()
    );
    assert!(outcome.is_ok(), "{}", outcome.unwrap_err());
    assert!(cleanup.is_ok(), "{}", cleanup.unwrap_err());
    assert!(
        terminal.is_some_and(|terminal| terminal.kind == exomonad_actor::ActorExitKind::Cancelled),
        "browser retirement must terminate the real resident actor"
    );
    assert!(
        retirement_cleanup.is_some_and(|cleanup| cleanup.is_confirmed()),
        "browser retirement must retain confirmed actor cleanup"
    );
    let state = transport.state.lock();
    assert!(state.raw_completed && state.typed_completed && state.cancel_issued && state.continued);
}

#[test]
fn completed_browser_replies_report_failures_and_wrong_results() {
    for (kind, call_id) in [
        ("custom_tool_call_output", "browser-real-cell"),
        ("function_call_output", "browser-typed-first"),
        ("function_call_output", "browser-typed-second"),
    ] {
        for output in [
            json!({"error":"native custody requires identical certified inputs", "failure":{"phase":"compile"}}).to_string(),
            json!({"status":"completed", "total":1, "nextIndex":1, "items":[{"status":"committed", "output":"41"}]}).to_string(),
            "not a JSON response".to_owned(),
        ] {
            let input = [harness::item::Item(json!({
                "type":kind, "call_id":call_id, "output":output,
            }))];
            let failure = validate_completed_replies(&input).unwrap_err();
            assert!(failure.contains(call_id));
            assert!(failure.contains(&output));
        }
    }
}

#[test]
fn browser_reply_observation_allows_pending_and_successful_calls() {
    let mut input = vec![
        final_answer("Waiting for the resident cell."),
        harness::item::Item(json!({
            "type":"custom_tool_call", "call_id":"browser-real-cell", "input":"pending",
        })),
        harness::item::Item(json!({
            "type":"custom_tool_call_output", "call_id":"browser-cancellable-cell", "output":"cancelled",
        })),
    ];
    assert!(validate_completed_replies(&input).is_ok());
    for (kind, call_id, expected) in [
        ("custom_tool_call_output", "browser-real-cell", "42"),
        ("function_call_output", "browser-typed-first", "42"),
        ("function_call_output", "browser-typed-second", "43"),
    ] {
        input.push(harness::item::Item(json!({
            "type":kind, "call_id":call_id,
            "output":json!({"status":"completed", "total":1, "nextIndex":1,
                "items":[{"status":"committed", "output":expected}]}).to_string(),
        })));
        assert!(validate_completed_replies(&input).is_ok());
    }
}
