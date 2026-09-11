//! Full-TUI contract fixture for the interactive application boundary.
//!
//! This is deliberately an integration test: the native input listener is owned
//! by the packaged Codex TUI, while Tidepool owns the binding and delivery seam.

#![cfg(unix)]

use axum::{
    extract::State,
    http::StatusCode,
    routing::{get, post},
    Json, Router,
};
use serde_json::{json, Value};
use std::{
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};
use tidepool_agent::{
    accept_interactive_session_binding, native_interactive_backend, read_interactive_binding,
    resolve_native_interactive_agent, BackendThreadId, InputAdmission, InputOperationId,
    InputProducerControlOutcome, InputProducerId, InputPurpose, InteractiveInputEnvelope,
    InteractiveInputMode, InteractiveInputTarget, InteractiveSessionBinding,
    HOST_DYNAMIC_TOOLS_PROTOCOL_VERSION,
};
use tokio::net::{TcpListener, UnixListener};

#[derive(Clone, Default)]
struct ProviderState(Arc<Mutex<Vec<Value>>>);

#[derive(Clone)]
struct HostState {
    binding: PathBuf,
    input_socket: PathBuf,
    sessions: Arc<Mutex<Vec<String>>>,
    challenged_binding: Arc<Mutex<Option<InteractiveSessionBinding>>>,
}

const LAUNCH_ID: &str = "tidepool-interactive-applications";
const INPUT_CONTROL_NONCE: &str = "tidepool-interactive-applications-nonce";

async fn response(
    State(state): State<ProviderState>,
    Json(body): Json<Value>,
) -> impl axum::response::IntoResponse {
    let generates_title = body
        .pointer("/text/format/schema/properties/title")
        .is_some();
    if !generates_title {
        state.0.lock().unwrap().push(body);
    }
    ([
        ("content-type", "text/event-stream"),
        ("connection", "close"),
    ], "event: response.created\ndata: {\"type\":\"response.created\",\"response\":{\"id\":\"fixture-response\"}}\n\nevent: response.completed\ndata: {\"type\":\"response.completed\",\"response\":{\"id\":\"fixture-response\",\"usage\":{\"input_tokens\":1,\"output_tokens\":1,\"total_tokens\":2}}}\n\n")
}

async fn registration(State(state): State<HostState>) -> Json<Value> {
    Json(json!({
        "protocolVersion": HOST_DYNAMIC_TOOLS_PROTOCOL_VERSION,
        "dynamicTools": [{
            "type": "namespace", "modelOnly": true, "name": "fixture",
            "description": "Fixture host namespace",
            "tools": [{"type": "function", "name": "probe", "description": "Fixture probe", "inputSchema": {"type": "object"}}]
        }],
        "scope": "primaryThread",
        "inputControlSocket": state.input_socket,
        "launchId": LAUNCH_ID,
        "inputControlNonce": INPUT_CONTROL_NONCE,
    }))
}

async fn attach(State(state): State<HostState>, Json(body): Json<Value>) -> StatusCode {
    let Some(thread) = body.get("threadId").and_then(Value::as_str) else {
        return StatusCode::BAD_REQUEST;
    };
    if body.get("protocolVersion").and_then(Value::as_u64)
        != Some(HOST_DYNAMIC_TOOLS_PROTOCOL_VERSION.into())
        || body.get("inputControlSocket").and_then(Value::as_str) != state.input_socket.to_str()
        || body.get("launchId").and_then(Value::as_str) != Some(LAUNCH_ID)
        || body.get("inputControlNonce").and_then(Value::as_str) != Some(INPUT_CONTROL_NONCE)
    {
        return StatusCode::BAD_REQUEST;
    }
    let Some(instance_id) = body
        .get("applicationInstanceId")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
    else {
        return StatusCode::BAD_REQUEST;
    };
    let Some(generation) = body
        .get("sessionGeneration")
        .and_then(Value::as_u64)
        .and_then(std::num::NonZeroU64::new)
    else {
        return StatusCode::BAD_REQUEST;
    };
    if accept_interactive_session_binding(
        &state.binding,
        HOST_DYNAMIC_TOOLS_PROTOCOL_VERSION,
        BackendThreadId(thread.to_owned()),
        Some(state.input_socket.clone()),
    )
    .await
    .is_err()
    {
        return StatusCode::INTERNAL_SERVER_ERROR;
    }
    *state.challenged_binding.lock().unwrap() = Some(InteractiveSessionBinding {
        launch_id: LAUNCH_ID.into(),
        instance_id: instance_id.into(),
        generation,
        nonce: INPUT_CONTROL_NONCE.into(),
    });
    state.sessions.lock().unwrap().push(thread.to_owned());
    StatusCode::NO_CONTENT
}

fn command_exists(name: &str) -> bool {
    std::process::Command::new(name).arg("-V").output().is_ok()
}

fn live_fixture_prerequisites(
    configured: Option<std::ffi::OsString>,
    tmux_available: bool,
) -> Result<PathBuf, &'static str> {
    let executable = configured
        .ok_or("TIDEPOOL_INTERACTIVE_CODEX_BIN must explicitly select the pinned executable")?;
    let executable = PathBuf::from(executable);
    if !executable.is_absolute() {
        return Err("TIDEPOOL_INTERACTIVE_CODEX_BIN must be absolute");
    }
    if !tmux_available {
        return Err("tmux is required to exercise the full TUI under a real PTY");
    }
    Ok(executable)
}

#[test]
fn live_fixture_refuses_unpinned_or_ambiguous_launch() {
    assert_eq!(
        live_fixture_prerequisites(None, true),
        Err("TIDEPOOL_INTERACTIVE_CODEX_BIN must explicitly select the pinned executable")
    );
    assert_eq!(
        live_fixture_prerequisites(Some("codex".into()), true),
        Err("TIDEPOOL_INTERACTIVE_CODEX_BIN must be absolute")
    );
    assert_eq!(
        live_fixture_prerequisites(Some("/nix/store/pinned/bin/codex".into()), false),
        Err("tmux is required to exercise the full TUI under a real PTY")
    );
}

async fn wait_for(path: &Path, deadline: Duration) {
    let end = Instant::now() + deadline;
    while Instant::now() < end {
        if path.exists() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!("timed out waiting for {}", path.display());
}

fn pane_pid(session: &str) -> u32 {
    let output = std::process::Command::new("tmux")
        .args(["display-message", "-p", "-t", session, "#{pane_pid}"])
        .output()
        .expect("read fixture pane pid");
    assert!(output.status.success(), "tmux did not report its pane pid");
    String::from_utf8(output.stdout)
        .expect("pane pid is utf-8")
        .trim()
        .parse()
        .expect("pane pid is numeric")
}

fn process_exists(pid: u32) -> bool {
    std::process::Command::new("kill")
        .args(["-0", &pid.to_string()])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok_and(|status| status.success())
}

async fn wait_for_process_exit(pid: u32, deadline: Duration) {
    let end = Instant::now() + deadline;
    while process_exists(pid) && Instant::now() < end {
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    assert!(
        !process_exists(pid),
        "fixture pane process {pid} survived cleanup"
    );
}

struct TmuxSession(Option<String>);

impl TmuxSession {
    fn terminate(&mut self) {
        let session = self.0.take().expect("tmux session already terminated");
        let status = std::process::Command::new("tmux")
            .args(["kill-session", "-t", &session])
            .status()
            .expect("run tmux kill-session");
        assert!(status.success(), "tmux failed to terminate fixture session");
    }
}

impl Drop for TmuxSession {
    fn drop(&mut self) {
        if let Some(session) = self.0.take() {
            let _ = std::process::Command::new("tmux")
                .args(["kill-session", "-t", &session])
                .status();
        }
    }
}

struct EnvironmentVariable {
    name: &'static str,
    previous: Option<std::ffi::OsString>,
}

impl EnvironmentVariable {
    fn set(name: &'static str, value: &Path) -> Self {
        let previous = std::env::var_os(name);
        // This dedicated integration target contains one test and performs the
        // mutation before starting its worker tasks.
        unsafe { std::env::set_var(name, value) };
        Self { name, previous }
    }
}

impl Drop for EnvironmentVariable {
    fn drop(&mut self) {
        unsafe {
            match &self.previous {
                Some(value) => std::env::set_var(self.name, value),
                None => std::env::remove_var(self.name),
            }
        }
    }
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn pinned_full_tui_binds_and_accepts_exactly_one_owned_input() {
    let selected = live_fixture_prerequisites(
        std::env::var_os("TIDEPOOL_INTERACTIVE_CODEX_BIN"),
        command_exists("tmux"),
    )
    .unwrap_or_else(|error| panic!("real PTY fixture preflight failed: {error}"));
    let installation = resolve_native_interactive_agent().await.unwrap();
    assert_eq!(installation.executable(), selected);
    assert_eq!(installation.executable_sha256().len(), 64);
    assert!(installation
        .executable_sha256()
        .bytes()
        .all(|byte| byte.is_ascii_hexdigit()));
    let lock_path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../flake.lock");
    let lock: Value = serde_json::from_slice(&std::fs::read(lock_path).unwrap()).unwrap();
    let source = lock
        .pointer("/nodes/codex/locked/rev")
        .and_then(Value::as_str)
        .unwrap();
    let package = installation
        .package_root()
        .expect("pinned executable must belong to a package");
    let selection_path = package.join("selection.json");
    let configured_closure = std::env::var_os("TIDEPOOL_SHOAL_CODEX_CLOSURE").map(PathBuf::from);
    if selection_path.exists() {
        let selection: Value = serde_json::from_slice(
            &std::fs::read(&selection_path)
                .unwrap_or_else(|error| panic!("read {}: {error}", selection_path.display())),
        )
        .unwrap_or_else(|error| panic!("parse {}: {error}", selection_path.display()));
        assert_eq!(
            selection.pointer("/codex").and_then(Value::as_str),
            Some(source)
        );
        let selected_digest = installation.executable_sha256();
        assert!(
            selection
                .pointer("/binaries")
                .and_then(Value::as_object)
                .is_some_and(|binaries| binaries.values().any(|digest| {
                    digest
                        .as_str()
                        .is_some_and(|digest| digest == selected_digest)
                })),
            "selected executable digest is absent from build selection"
        );
    } else {
        assert_eq!(
            configured_closure.as_deref(),
            Some(package),
            "pinned package needs either build-selection evidence or the flake closure"
        );
    }
    let unmatched_closure = configured_closure.filter(|closure| closure != package);
    eprintln!(
        "interactive fixture provenance: executable={} version={} sha256={} package={} flake_source={source} unmatched_closure={}",
        installation.executable().display(),
        installation.version(),
        installation.executable_sha256(),
        package.display(),
        unmatched_closure
            .as_deref()
            .map_or_else(|| "none".into(), |path| path.display().to_string())
    );

    let temp = tempfile::tempdir().unwrap();
    use std::os::unix::fs::{FileTypeExt, PermissionsExt};
    std::fs::set_permissions(temp.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    let host_socket = temp.path().join("host.sock");
    let input_socket = temp.path().join("input.sock");
    let binding = temp.path().join("binding.json");
    let codex_home = temp.path().join("codex-home");
    let work = temp.path().join("work");
    std::fs::create_dir_all(&codex_home).unwrap();
    std::fs::create_dir_all(&work).unwrap();
    let _codex_home = EnvironmentVariable::set("CODEX_HOME", &codex_home);

    let provider_state = ProviderState::default();
    let tcp = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let provider_addr = tcp.local_addr().unwrap();
    let provider_app = Router::new()
        .route("/v1/responses", post(response))
        .with_state(provider_state.clone());
    let provider = tokio::spawn(async move { axum::serve(tcp, provider_app).await });

    let sessions = Arc::new(Mutex::new(Vec::new()));
    let challenged_binding = Arc::new(Mutex::new(None));
    let host_state = HostState {
        binding: binding.clone(),
        input_socket: input_socket.clone(),
        sessions: sessions.clone(),
        challenged_binding: challenged_binding.clone(),
    };
    let uds = UnixListener::bind(&host_socket).unwrap();
    let host_app = Router::new()
        .route("/v1/dynamic-tools/registration", get(registration))
        .route("/v1/dynamic-tools/session", post(attach))
        .with_state(host_state);
    let host = tokio::spawn(async move { axum::serve(uds, host_app).await });

    std::fs::write(
        codex_home.join("config.toml"),
        format!(
            r#"
model = "fixture-model"
model_provider = "fixture"
approval_policy = "never"
sandbox_mode = "danger-full-access"
[model_providers.fixture]
name = "Fixture"
base_url = "http://{provider_addr}/v1"
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

    // tmux allocates the actual controlling PTY; the test process intentionally has none.
    let session = format!("tidepool-a0-{}", std::process::id());
    let status = std::process::Command::new("tmux")
        .args(["new-session", "-d", "-s", &session, "-x", "100", "-y", "30"])
        .status()
        .unwrap();
    assert!(status.success(), "tmux failed to allocate the fixture PTY");
    let mut tmux = TmuxSession(Some(session.clone()));
    let pane_tty = std::process::Command::new("tmux")
        .args(["display-message", "-p", "-t", &session, "#{pane_tty}"])
        .output()
        .unwrap();
    assert!(
        pane_tty.status.success(),
        "tmux did not report its pane PTY"
    );
    let pane_tty = PathBuf::from(String::from_utf8(pane_tty.stdout).unwrap().trim());
    assert!(
        std::fs::metadata(&pane_tty)
            .unwrap()
            .file_type()
            .is_char_device(),
        "tmux pane is not backed by a terminal device: {}",
        pane_tty.display()
    );
    assert!(std::process::Command::new("tmux")
        .args(["set-option", "-t", &session, "remain-on-exit", "on"])
        .status()
        .unwrap()
        .success());
    let status = std::process::Command::new("tmux")
        .args(["respawn-pane", "-k", "-t", &session])
        .arg("env")
        .arg(format!("CODEX_HOME={}", codex_home.display()))
        .arg("OPENAI_API_KEY=fixture")
        .arg(installation.executable())
        .arg("--no-alt-screen")
        .arg("--host-dynamic-tools-socket")
        .arg(&host_socket)
        .arg("-C")
        .arg(&work)
        .arg("fixture bootstrap")
        .status()
        .unwrap();
    assert!(status.success(), "tmux failed to launch full Codex TUI");
    let end = Instant::now() + Duration::from_secs(30);
    while !binding.exists() && Instant::now() < end {
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    if !binding.exists() {
        let output = std::process::Command::new("tmux")
            .args(["capture-pane", "-p", "-t", &session, "-S", "-100"])
            .output()
            .unwrap();
        panic!(
            "TUI did not attach its host session:\n{}",
            String::from_utf8_lossy(&output.stdout)
        );
    }
    let end = Instant::now() + Duration::from_secs(2);
    while sessions.lock().unwrap().is_empty() && Instant::now() < end {
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    let thread = read_interactive_binding(&binding)
        .await
        .unwrap()
        .with_challenged_session_binding(challenged_binding.lock().unwrap().clone());
    assert_eq!(
        sessions.lock().unwrap().as_slice(),
        &[thread.id().0.clone()]
    );
    wait_for(&input_socket, Duration::from_secs(10)).await;
    let end = Instant::now() + Duration::from_secs(10);
    while provider_state.0.lock().unwrap().is_empty() && Instant::now() < end {
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert!(
        !provider_state.0.lock().unwrap().is_empty(),
        "fixture bootstrap must complete before hosted input"
    );
    tokio::time::sleep(Duration::from_millis(100)).await;
    let bootstrap_requests = provider_state.0.lock().unwrap().len();

    let client = reqwest::Client::builder()
        .unix_socket(input_socket.clone())
        .build()
        .unwrap();
    let wrong = client
        .post("http://localhost/v1/input")
        .json(&json!({
            "threadId": "00000000-0000-0000-0000-000000000001",
            "clientUserMessageId": "wrong-owner", "message": "must not deliver"
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(wrong.status(), StatusCode::CONFLICT);

    let backend = native_interactive_backend(installation);
    assert_eq!(
        backend.bind_input(&thread).await.unwrap(),
        InputAdmission::Admitted
    );
    let operation = InputOperationId {
        producer: InputProducerId::new("fixture/interactive-applications".into()).unwrap(),
        sequence: std::num::NonZeroU64::new(1).unwrap(),
    };
    let input = InteractiveInputEnvelope::new(
        operation.clone(),
        InputPurpose::OperatorInput,
        InteractiveInputMode::StartOrSteer,
        InteractiveInputTarget {
            conversation: thread.id().clone(),
            actor: "fixture-root".into(),
            correlation: Some("host-input-1".into()),
        },
        b"one owned host input".to_vec(),
    )
    .unwrap();
    assert!(matches!(
        backend.submit_input(&thread, &input).await.unwrap(),
        InputAdmission::Admitted | InputAdmission::Dispatching | InputAdmission::Presented
    ));
    let end = Instant::now() + Duration::from_secs(20);
    while provider_state.0.lock().unwrap().len() <= bootstrap_requests && Instant::now() < end {
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    let requests = provider_state.0.lock().unwrap();
    let delivered = requests
        .iter()
        .filter(|body| body.to_string().contains("one owned host input"))
        .count();
    assert_eq!(
        delivered, 1,
        "owned input must reach the provider exactly once: {requests:?}"
    );
    assert!(
        !requests
            .iter()
            .any(|body| body.to_string().contains("must not deliver")),
        "wrong owner reached provider"
    );
    drop(requests);
    let observed = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let outcome = backend.query_input(&thread, &operation).await.unwrap();
            if outcome != InputAdmission::Dispatching {
                break outcome;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("native acceptance must persist presentation before acknowledgement");
    assert_eq!(observed, InputAdmission::Presented);
    assert_eq!(
        backend
            .acknowledge_input_prefix(&thread, &operation.producer, operation.sequence)
            .await
            .unwrap(),
        InputProducerControlOutcome::Applied
    );
    assert_eq!(
        backend.submit_input(&thread, &input).await.unwrap(),
        InputAdmission::Compacted,
        "acknowledged input must remain fenced against replay"
    );

    let codex_pid = pane_pid(&session);
    tmux.terminate();
    wait_for_process_exit(codex_pid, Duration::from_secs(10)).await;
    provider.abort();
    assert!(
        provider.await.unwrap_err().is_cancelled(),
        "provider task was not cancelled and reaped"
    );
    host.abort();
    assert!(
        host.await.unwrap_err().is_cancelled(),
        "host task was not cancelled and reaped"
    );
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
#[test]
fn real_tui_fixture_reports_unsupported_platform() {
    panic!(
        "real PTY fixture unavailable: owning-TUI Unix sockets are supported only on Linux/macOS"
    );
}
