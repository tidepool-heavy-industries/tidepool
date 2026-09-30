//! Actual browser driver over the production embedded host and resident actor.

use super::browser_process::BrowserProcess;
use super::*;
use tokio::sync::oneshot;

const RAW_INPUT: &str = "Run one real Haskell cell and retain its answer.";
const CANCEL_INPUT: &str = "Start a cancellable resident cell.";
const CONTINUE_INPUT: &str = "Continue after the interrupted cell.";

struct Barrier {
    id: String,
    phase: &'static str,
    request: ResponsesRequest,
    release: oneshot::Sender<()>,
}

#[derive(Default)]
struct ScriptState {
    requests: usize,
    raw_issued: bool,
    raw_completed: bool,
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
    if item.0["type"] != "custom_tool_call_output" || item.0["call_id"] != "browser-real-cell" {
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
                    .is_some_and(|value| value.trim() == "42")
        })
}

#[async_trait]
impl ResponsesTransport for BrowserTransport {
    async fn create(&self, request: ResponsesRequest) -> Result<ResponsesTurn, TransportError> {
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
                        "call_id":"browser-real-cell", "input":"40 + 2 :: Int"
                    })),
                )
            } else if !state.raw_completed && result_present {
                state.raw_completed = true;
                ("raw_result", final_answer("The resident cell returned 42."))
            } else if state.raw_completed && !state.cancel_issued && contains_input(CANCEL_INPUT) {
                state.cancel_issued = true;
                (
                    "cancel_input",
                    harness::item::Item(json!({
                        "type":"custom_tool_call", "name":"haskell",
                        "call_id":"browser-cancellable-cell",
                        "input":"do { sleep (seconds 30); pure (99 :: Int) }"
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
            } else {
                return Err(TransportError::Stream(
                    "unexpected browser scenario request".into(),
                ));
            };
            (format!("browser-request-{}", state.requests), phase, item)
        };
        let (release, released) = oneshot::channel();
        self.barriers
            .send(Barrier {
                id: id.clone(),
                phase,
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

fn required_path(name: &str) -> Result<PathBuf, String> {
    std::env::var_os(name)
        .map(PathBuf::from)
        .ok_or_else(|| format!("browser gate requires declared {name}"))
}

async fn verify_cancelled_resident_call(fixture: &RunningBrowserHost) -> Result<(), String> {
    let target = browser_target(&fixture.campaign);
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
    if claim.operation.origin != expected_origin || claim.operation.request != claim.request {
        return Err("cancellation evidence belongs to another originating operation".into());
    }
    let scheduler = fixture.runtime.scheduler();
    if scheduler
        .output(&claim.operation)
        .await
        .map_err(|error| error.to_string())?
        != Some(harness::turn::JobOutput::Cancelled)
    {
        return Err("resident call did not retain confirmed cancellation".into());
    }
    if !matches!(
        scheduler
            .cancellation_acknowledgment(&claim.operation)
            .await
            .map_err(|error| error.to_string())?,
        Some(harness::provider::CancellationAcknowledgment::Stopped)
    ) {
        return Err("resident cancellation owner did not confirm cleanup".into());
    }
    if fixture.campaign.actor.hosted_cell_computing() {
        return Err("resident actor still owns the interrupted cell".into());
    }
    Ok(())
}

async fn drive_browser(
    fixture: &RunningBrowserHost,
    secret: &str,
    mut barriers: mpsc::UnboundedReceiver<Barrier>,
) -> Result<(), String> {
    let node = required_path("TIDEPOOL_BROWSER_NODE")?;
    let driver = required_path("TIDEPOOL_BROWSER_DRIVER")?;
    let browsers = required_path("PLAYWRIGHT_BROWSERS_PATH")?;
    let mut process = BrowserProcess::spawn(&node, &driver, &browsers)?;
    let identity = browser_target(&fixture.campaign);
    let ready = json!({
        "type":"ready", "version":1, "base_url":format!("http://{}", fixture.address),
        "session_secret":secret, "actor":{"name":identity.actor.0,"incarnation":identity.incarnation},
        "scenario":{"steps":[
            {"action":"input", "text":RAW_INPUT,"wait_for_receipt":false,"provider_barriers":[
                {"phase":"raw_input","expected_request_text":RAW_INPUT},
                {"until_phase":"raw_result","allowed_intermediate_phases":["raw_wait"],"expected_request_text":"42"}
            ], "wait_for_text":"The resident cell returned 42."},
            {"action":"reload"}, {"action":"retry"},
            {"action":"input","text":CANCEL_INPUT,"provider_barriers":[
                {"phase":"cancel_input","expected_request_text":CANCEL_INPUT},
                {"phase":"cancel_wait","expected_request_text":CANCEL_INPUT}
            ]},
            {"action":"interrupt"}, {"action":"wait_actor_state","state":"waiting"},
            {"action":"input","text":CONTINUE_INPUT,"provider_barriers":[
                {"phase":"continued","expected_request_text":CONTINUE_INPUT}
            ],"wait_for_text":"Continued after the interrupted cell."},
            {"action":"retire"}
        ]}
    });
    let outcome = tokio::time::timeout(Duration::from_secs(180), async {
        process.send_frame(&ready).await?;
        let mut started = false;
        let mut waiting: Option<Barrier> = None;
        let mut raw_completed = false;
        let mut cancel_wait = false;
        let mut cancel_pending_at = None;
        let mut continued = false;
        let readiness_deadline = tokio::time::Instant::now() + Duration::from_secs(30);
        loop {
            tokio::select! {
                _ = tokio::time::sleep_until(readiness_deadline), if !started => {
                    return Err("browser did not acknowledge readiness".to_owned());
                }
                barrier = barriers.recv(), if started && waiting.is_none() => {
                    let barrier = barrier.ok_or("scripted provider closed before browser completion")?;
                    if barrier.phase == "cancel_wait" {
                        tokio::time::timeout(Duration::from_secs(10), async {
                            while !fixture.campaign.actor.hosted_cell_computing() {
                                tokio::time::sleep(Duration::from_millis(10)).await;
                            }
                        }).await.map_err(|_| "cancellable cell never entered the real resident actor")?;
                        cancel_pending_at = Some(tokio::time::Instant::now());
                    }
                    if barrier.phase == "continued" {
                        verify_cancelled_resident_call(fixture).await?;
                        if !cancel_pending_at.is_some_and(|started| started.elapsed() < Duration::from_secs(8)) {
                            return Err("interrupt did not stop the pending 30-second cell promptly".into());
                        }
                    }
                    raw_completed |= barrier.phase == "raw_result";
                    cancel_wait |= barrier.phase == "cancel_wait";
                    continued |= barrier.phase == "continued";
                    process.send_frame(&json!({"type":"provider_barrier", "id":barrier.id,
                        "phase":barrier.phase, "request_summary":{"input":barrier.request.input}})).await?;
                    waiting = Some(barrier);
                }
                frame = process.next_frame() => {
                    let frame = frame?.ok_or("browser exited without a result")?;
                    match frame["type"].as_str() {
                        Some("driver_started") if !started => started = true,
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
                            if !started || waiting.is_some() || !raw_completed || !cancel_wait || !continued {
                                return Err("browser finished before the real provider scenario completed".into());
                            }
                            return Ok(());
                        }
                        _ => return Err("browser emitted an unexpected protocol frame".into()),
                    }
                }
            }
        }
    }).await.unwrap_or_else(|_| Err("browser journey exceeded its deadline".into()));
    process.finish(outcome, secret).await
}

pub(super) async fn production_browser_journey() {
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
        asset_root: assets,
        session_secret_file: secret_file,
        codex_auth_file: auth_file,
        context_capacity_tokens: 2_000_000,
        concurrent_jobs: 1,
    };
    let (barriers, barrier_rx) = mpsc::unbounded_channel();
    let transport = Arc::new(BrowserTransport {
        barriers,
        state: Mutex::new(ScriptState::default()),
    });
    let host_transport: Arc<dyn ResponsesTransport> = transport.clone();
    let fixture = RunningBrowserHost::start(&settings, &host_transport)
        .await
        .unwrap();
    let outcome = drive_browser(&fixture, secret, barrier_rx).await;
    let terminal = fixture.campaign.actor.terminal().get();
    let retirement_cleanup = fixture.campaign.actor.terminal().cleanup();
    let cleanup = fixture.stop().await;
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
    assert!(state.raw_completed && state.cancel_issued && state.continued);
}
