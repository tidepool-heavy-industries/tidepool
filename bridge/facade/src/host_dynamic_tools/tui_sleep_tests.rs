//! Matched native/Tidepool cancellation and real-wait acceptance.
//!
//! These ignored tests use the packaged native TUI under a real tmux PTY, a
//! scripted local provider, and the production actor-scoped host service.

use super::*;
use crate::host_dynamic_tools::HostDynamicToolService;
use axum::{extract::State, routing::post, Json, Router};
use serde_json::{json, Value};
use std::{
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex as StdMutex,
    },
    time::{Duration, Instant},
};
use tidepool_actor::{
    HostedWorkSeal, ResidentToolEndpoint, ResidentToolError, ResidentToolFuture,
    WorkbenchCancellationOutcome,
};
use tidepool_agent::{
    native_interactive_agent_from_parts, native_interactive_backend, read_interactive_binding,
    InputAdmission, InputOperationId, InputProducerId, InputPurpose, InteractiveInputEnvelope,
    InteractiveInputMode, InteractiveInputTarget,
};
use tidepool_tool::{HostedTool, ToolInvocation, ToolInvocationContext};
use tokio::{net::UnixListener, sync::Notify};

const NATIVE_ENV: &str = "TIDEPOOL_INTERACTIVE_CODEX_BIN";

#[derive(Clone, Copy, Debug)]
enum ShortIngress {
    Human,
    Actor,
    Progress,
}

impl ShortIngress {
    fn marker(self) -> &'static str {
        match self {
            Self::Human => "queued-human-after-sleep",
            Self::Actor => "queued-actor-after-sleep",
            Self::Progress => "queued-progress-after-sleep",
        }
    }
}

#[derive(Clone, Copy)]
enum Scenario {
    Short,
    RealWait,
    ComposedWait,
}

#[derive(Clone)]
struct Provider {
    requests: Arc<StdMutex<Vec<Value>>>,
    scenario: Scenario,
}

fn response_stream(item: Value, index: usize) -> String {
    [
        json!({"type":"response.created","response":{"id":format!("response-{index}")}}),
        json!({"type":"response.output_item.done","item":item}),
        json!({"type":"response.completed","response":{"id":format!("response-{index}"),
            "usage":{"input_tokens":1,"output_tokens":1,"total_tokens":2}}}),
    ]
    .iter()
    .map(|event| {
        format!(
            "event: {}\ndata: {event}\n\n",
            event["type"].as_str().unwrap()
        )
    })
    .collect()
}

async fn provider_response(
    State(provider): State<Provider>,
    Json(body): Json<Value>,
) -> impl axum::response::IntoResponse {
    let title = body
        .pointer("/text/format/schema/properties/title")
        .is_some();
    let index = if title {
        usize::MAX
    } else {
        let mut requests = provider.requests.lock().unwrap();
        let index = requests.len();
        requests.push(body);
        index
    };
    let item = if title {
        json!({"type":"message","role":"assistant","id":"title",
            "content":[{"type":"output_text","text":"{\"title\":\"Matched sleep\"}"}]})
    } else {
        match (provider.scenario, index) {
            (Scenario::Short, 0) => json!({
                "type":"custom_tool_call",
                "call_id":"sleep-short",
                "name":"haskell",
                "input":"sleep (minutes 15)\ninterruptedSuffix <- pure (99 :: Int)"
            }),
            (Scenario::Short, 1) => json!({
                "type":"message","role":"assistant","id":"interrupted-turn-settled",
                "content":[{"type":"output_text","text":"interrupted turn settled"}]
            }),
            (Scenario::Short, 2) => json!({
                "type":"custom_tool_call",
                "call_id":"reuse-after-interrupt",
                "name":"haskell",
                "input":"pure (11 :: Int)"
            }),
            (Scenario::Short, 3) => json!({
                "type":"custom_tool_call",
                "call_id":"suppressed-suffix",
                "name":"haskell",
                "input":"interruptedSuffix"
            }),
            (Scenario::Short, _) => json!({
                "type":"message","role":"assistant","id":format!("short-done-{index}"),
                "content":[{"type":"output_text","text":"short fixture done"}]
            }),
            (Scenario::RealWait, 0) => json!({
                "type":"custom_tool_call",
                "call_id":"sleep-real",
                "name":"haskell",
                "input":"sleep (minutes 15)\npure (7 :: Int)"
            }),
            (Scenario::ComposedWait, 0) => json!({
                "type":"custom_tool_call",
                "call_id":"sleep-composed",
                "name":"haskell",
                "input":"do print (\"before-sleep\" :: Text); sleep (seconds 1); print (\"after-sleep\" :: Text); pure (7 :: Int)"
            }),
            (Scenario::RealWait | Scenario::ComposedWait, _) => json!({
                "type":"message","role":"assistant","id":"real-wait-done",
                "content":[{"type":"output_text","text":"real wait fixture done"}]
            }),
        }
    };
    (
        [
            ("content-type", "text/event-stream"),
            ("connection", "close"),
        ],
        response_stream(item, index),
    )
}

struct ObservedEndpoint {
    inner: Arc<dyn ResidentToolEndpoint>,
    started: AtomicBool,
    changed: Notify,
    cancellations: Arc<StdMutex<Vec<String>>>,
}

impl ObservedEndpoint {
    fn new(inner: Arc<dyn ResidentToolEndpoint>) -> Arc<Self> {
        Arc::new(Self {
            inner,
            started: AtomicBool::new(false),
            changed: Notify::new(),
            cancellations: Arc::new(StdMutex::new(Vec::new())),
        })
    }

    async fn await_dispatch(&self) {
        tokio::time::timeout(Duration::from_secs(30), async {
            while !self.started.load(Ordering::Acquire) {
                self.changed.notified().await;
            }
        })
        .await
        .expect("native TUI did not dispatch the hosted Haskell call");
    }
}

impl ResidentToolEndpoint for ObservedEndpoint {
    fn seal_hosted_work_boxed(
        &self,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<HostedWorkSeal, ResidentToolError>> + Send>,
    > {
        self.inner.seal_hosted_work_boxed()
    }

    fn tools(&self) -> &[HostedTool] {
        self.inner.tools()
    }

    fn instructions(&self) -> Option<&str> {
        self.inner.instructions()
    }

    fn dispatch_boxed(&self, invocation: ToolInvocation) -> ResidentToolFuture {
        self.started.store(true, Ordering::Release);
        self.changed.notify_waiters();
        self.inner.dispatch_boxed(invocation)
    }

    fn cancel_workbench_boxed(
        &self,
        invocation: ToolInvocationContext,
    ) -> std::pin::Pin<
        Box<
            dyn std::future::Future<
                    Output = Result<WorkbenchCancellationOutcome, ResidentToolError>,
                > + Send,
        >,
    > {
        let cancellations = Arc::clone(&self.cancellations);
        cancellations
            .lock()
            .unwrap()
            .push(format!("request {invocation:?}"));
        let cancel = self.inner.cancel_workbench_boxed(invocation);
        Box::pin(async move {
            let result = cancel.await;
            cancellations
                .lock()
                .unwrap()
                .push(format!("outcome {result:?}"));
            result
        })
    }

    fn reattach_boxed(&self) -> ResidentToolFuture {
        self.inner.reattach_boxed()
    }

    fn reconcile_workbench_boxed(
        &self,
        boundary: tidepool_runtime::session::WorkbenchForkBoundary,
    ) -> std::pin::Pin<
        Box<
            dyn std::future::Future<
                    Output = Result<
                        tidepool_actor::WorkbenchBoundaryReconciliation,
                        ResidentToolError,
                    >,
                > + Send,
        >,
    > {
        self.inner.reconcile_workbench_boxed(boundary)
    }

    fn complete_boxed(
        &self,
        boundary: tidepool_runtime::session::WorkbenchForkBoundary,
    ) -> ResidentToolFuture {
        self.inner.complete_boxed(boundary)
    }
}

struct TmuxSession(String);

impl TmuxSession {
    fn launch(native: &Path, home: &Path, work: &Path, socket: &Path) -> Self {
        let session = format!("shoal-sleep-test-{}", uuid::Uuid::new_v4().simple());
        assert!(std::process::Command::new("tmux")
            .args(["new-session", "-d", "-s", &session, "-x", "100", "-y", "30"])
            .status()
            .unwrap()
            .success());
        assert!(std::process::Command::new("tmux")
            .args(["set-option", "-t", &session, "remain-on-exit", "on"])
            .status()
            .unwrap()
            .success());
        assert!(std::process::Command::new("tmux")
            .args(["respawn-pane", "-k", "-t", &session])
            .arg("env")
            .arg(format!("CODEX_HOME={}", home.display()))
            .arg("OPENAI_API_KEY=fixture")
            .arg("RUST_LOG=codex_tui=trace")
            .arg(native)
            .arg("--no-alt-screen")
            .arg("--host-dynamic-tools-socket")
            .arg(socket)
            .arg("-C")
            .arg(work)
            .arg("fixture bootstrap")
            .status()
            .unwrap()
            .success());
        Self(session)
    }

    fn human_input(&self, text: &str) {
        assert!(std::process::Command::new("tmux")
            .args(["send-keys", "-t", &self.0, "-l", text])
            .status()
            .unwrap()
            .success());
        // While a turn is running, Tab is the native TUI's explicit
        // queue/steer action. Enter only leaves text in the composer.
        assert!(std::process::Command::new("tmux")
            .args(["send-keys", "-t", &self.0, "Tab"])
            .status()
            .unwrap()
            .success());
    }

    fn capture(&self) -> String {
        let output = std::process::Command::new("tmux")
            .args(["capture-pane", "-p", "-t", &self.0, "-S", "-100"])
            .output()
            .unwrap();
        String::from_utf8_lossy(&output.stdout).into_owned()
    }
}

impl Drop for TmuxSession {
    fn drop(&mut self) {
        let _ = std::process::Command::new("tmux")
            .args(["kill-session", "-t", &self.0])
            .status();
    }
}

struct Fixture {
    _temp: tempfile::TempDir,
    campaign: test_campaign::TestCampaign,
    provider: Provider,
    provider_task: tokio::task::JoinHandle<Result<(), std::io::Error>>,
    host_task: tokio::task::JoinHandle<Result<(), std::io::Error>>,
    control: crate::host_dynamic_tools::HostToolControl,
    observed: Arc<ObservedEndpoint>,
    tui: TmuxSession,
    binding: PathBuf,
    home: PathBuf,
    native: PathBuf,
}

impl Fixture {
    async fn start(scenario: Scenario) -> Self {
        use std::os::unix::fs::PermissionsExt;
        let native =
            PathBuf::from(std::env::var_os(NATIVE_ENV).unwrap_or_else(|| {
                panic!("{NATIVE_ENV} must name exact matched native executable")
            }));
        assert!(native.is_absolute());
        let temp = tempfile::tempdir().unwrap();
        std::fs::set_permissions(temp.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        let root = temp.path().to_path_buf();
        let home = root.join("codex-home");
        let work = root.join("work");
        std::fs::create_dir(&home).unwrap();
        std::fs::create_dir(&work).unwrap();

        let provider = Provider {
            requests: Arc::new(StdMutex::new(Vec::new())),
            scenario,
        };
        let tcp = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = tcp.local_addr().unwrap();
        let app = Router::new()
            .route("/v1/responses", post(provider_response))
            .with_state(provider.clone());
        let provider_task = tokio::spawn(async move { axum::serve(tcp, app).await });
        std::fs::write(
            home.join("config.toml"),
            format!(
                r#"
model = "fixture-model"
model_provider = "fixture"
approval_policy = "never"
sandbox_mode = "danger-full-access"
[model_providers.fixture]
name = "Fixture"
base_url = "http://{address}/v1"
wire_api = "responses"
requires_openai_auth = false
supports_websockets = false
[projects.{}]
trust_level = "trusted"
"#,
                toml::Value::String(work.display().to_string())
            ),
        )
        .unwrap();

        let campaign = test_campaign::TestCampaign::start().await;
        let observed = ObservedEndpoint::new(campaign.root_installation.policy.clone());
        let socket = root.join("host.sock");
        let binding = root.join("binding.json");
        let listener = UnixListener::bind(&socket).unwrap();
        let service = HostDynamicToolService::new(observed.clone(), binding.clone(), None).unwrap();
        let control = service.control();
        let host_task = tokio::spawn(service.serve(listener));
        let tui = TmuxSession::launch(&native, &home, &work, &socket);
        Self {
            _temp: temp,
            campaign,
            provider,
            provider_task,
            host_task,
            control,
            observed,
            tui,
            binding,
            home,
            native,
        }
    }

    async fn thread(&self) -> tidepool_agent::QueueReadyThread {
        let end = Instant::now() + Duration::from_secs(30);
        while (!self.binding.exists() || self.control.challenged_binding().is_none())
            && Instant::now() < end
        {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        let challenged = self
            .control
            .challenged_binding()
            .expect("native session challenge did not settle");
        read_interactive_binding(&self.binding)
            .await
            .unwrap_or_else(|error| {
                panic!("native binding failed: {error}\n{}", self.tui.capture())
            })
            .with_challenged_session_binding(Some(challenged))
    }

    async fn await_requests(&self, count: usize, deadline: Duration) -> Vec<Value> {
        tokio::time::timeout(deadline, async {
            loop {
                let requests = self.provider.requests.lock().unwrap().clone();
                if requests.len() >= count {
                    return requests;
                }
                tokio::time::sleep(Duration::from_millis(25)).await;
            }
        })
        .await
        .unwrap_or_else(|_| {
            panic!(
                "provider request timeout\n{}\ncancellation trace: {:?}\nnative log:\n{}",
                self.tui.capture(),
                self.observed.cancellations.lock().unwrap(),
                std::fs::read_to_string(self.home.join("log/codex-tui.log"))
                    .unwrap_or_else(|error| format!("unavailable: {error}"))
            )
        })
    }

    async fn shutdown(self) {
        self.campaign.forest.shutdown().await;
        self.campaign.hosted.await.unwrap();
        self.control.drain();
        self.host_task.await.unwrap().unwrap();
        self.provider_task.abort();
        assert!(
            self.provider_task.await.unwrap_err().is_cancelled(),
            "scripted provider was not cancelled and reaped"
        );
    }
}

fn output_for(request: &Value, call_id: &str) -> String {
    request["input"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|item| item["call_id"] == call_id && item["type"] == "custom_tool_call_output")
        .map(|item| item["output"].as_str().unwrap_or_default())
        .collect::<Vec<_>>()
        .join("\n")
}

async fn submit_host_input(
    fixture: &Fixture,
    thread: &tidepool_agent::QueueReadyThread,
    ingress: ShortIngress,
) {
    if matches!(ingress, ShortIngress::Human) {
        fixture.tui.human_input(ingress.marker());
        return;
    }
    let purpose = match ingress {
        ShortIngress::Actor => InputPurpose::Notification,
        ShortIngress::Progress => InputPurpose::RequestUpdate,
        ShortIngress::Human => unreachable!(),
    };
    let backend = native_interactive_backend(
        native_interactive_agent_from_parts(fixture.native.clone(), "matched c76aa485".into())
            .unwrap(),
    );
    assert!(matches!(
        backend.bind_input(thread).await.unwrap(),
        InputAdmission::Admitted | InputAdmission::Presented
    ));
    let input = InteractiveInputEnvelope::new(
        InputOperationId {
            producer: InputProducerId::new(format!("matched/{ingress:?}")).unwrap(),
            sequence: std::num::NonZeroU64::new(1).unwrap(),
        },
        purpose,
        InteractiveInputMode::StartOrSteer,
        InteractiveInputTarget {
            conversation: thread.id().clone(),
            actor: "fixture-root".into(),
            correlation: Some(ingress.marker().into()),
        },
        ingress.marker().as_bytes().to_vec(),
    )
    .unwrap();
    let admission = backend
        .submit_input(thread, &input)
        .await
        .unwrap_or_else(|error| {
            panic!(
                "native input failed: {error}\ncancellation trace: {:?}\nnative log:\n{}",
                fixture.observed.cancellations.lock().unwrap(),
                std::fs::read_to_string(fixture.home.join("log/codex-tui.log"))
                    .unwrap_or_else(|read_error| format!("unavailable: {read_error}"))
            )
        });
    assert!(matches!(
        admission,
        InputAdmission::Admitted | InputAdmission::Dispatching | InputAdmission::Presented
    ));
}

async fn run_short_case(ingress: ShortIngress) {
    let fixture = Fixture::start(Scenario::Short).await;
    let thread = fixture.thread().await;
    fixture.observed.await_dispatch().await;
    assert_eq!(fixture.provider.requests.lock().unwrap().len(), 1);
    submit_host_input(&fixture, &thread, ingress).await;
    let requests = fixture.await_requests(5, Duration::from_secs(30)).await;
    let cancelled = output_for(&requests[1], "sleep-short");
    assert!(
        !cancelled.is_empty() && !cancelled.contains("interruptedSuffix"),
        "the first inference ran without original invocation settlement: {cancelled}"
    );
    assert!(
        !requests[1].to_string().contains(ingress.marker()),
        "{ingress:?} leaked into inference before the original invocation settled"
    );
    assert!(
        requests[2].to_string().contains(ingress.marker()),
        "{ingress:?} was not retained for inference after the interrupted turn settled"
    );
    assert!(
        output_for(&requests[3], "reuse-after-interrupt").contains("11"),
        "workbench did not reuse after {ingress:?}"
    );
    let suffix = output_for(&requests[4], "suppressed-suffix");
    let normalized_suffix = suffix.to_lowercase();
    assert!(
        normalized_suffix.contains("rejected") || normalized_suffix.contains("not in scope"),
        "interrupted suffix survived {ingress:?}: {suffix}"
    );
    fixture.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires exact packaged native and tmux"]
async fn full_tui_human_input_interrupts_sleep_before_inference() {
    run_short_case(ShortIngress::Human).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires exact packaged native and tmux"]
async fn full_tui_actor_input_interrupts_sleep_before_inference() {
    run_short_case(ShortIngress::Actor).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires exact packaged native and tmux"]
async fn full_tui_progress_input_interrupts_sleep_before_inference() {
    run_short_case(ShortIngress::Progress).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires exact matched native executable, tmux, and an actual 15-minute wait"]
async fn full_tui_real_fifteen_minute_sleep_has_no_intermediate_inference() {
    let fixture = Fixture::start(Scenario::RealWait).await;
    fixture.thread().await;
    fixture.observed.await_dispatch().await;
    assert_eq!(fixture.provider.requests.lock().unwrap().len(), 1);
    tokio::time::sleep(Duration::from_secs(14 * 60 + 50)).await;
    assert_eq!(
        fixture.provider.requests.lock().unwrap().len(),
        1,
        "provider inference occurred before the real monotonic deadline"
    );
    let requests = fixture.await_requests(2, Duration::from_secs(30)).await;
    let output = output_for(&requests[1], "sleep-real");
    assert!(
        output.contains('7'),
        "missing single Haskell suffix result: {output}"
    );
    assert_eq!(
        requests[1]["input"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|item| {
                item["call_id"] == "sleep-real" && item["type"] == "custom_tool_call_output"
            })
            .count(),
        1,
        "the real wait produced more than one Haskell completion"
    );
    fixture.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires exact matched native executable and tmux"]
async fn full_tui_sleep_preserves_console_output_order() {
    let fixture = Fixture::start(Scenario::ComposedWait).await;
    fixture.thread().await;
    let requests = fixture.await_requests(2, Duration::from_secs(90)).await;
    let output = output_for(&requests[1], "sleep-composed");
    let before = output.find("before-sleep").expect(&output);
    let after = output.find("after-sleep").expect(&output);
    assert!(before < after, "{output}");
    assert!(output.contains('7'), "{output}");
    assert_eq!(
        requests[1]["input"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|item| {
                item["call_id"] == "sleep-composed" && item["type"] == "custom_tool_call_output"
            })
            .count(),
        1
    );
    fixture.shutdown().await;
}
