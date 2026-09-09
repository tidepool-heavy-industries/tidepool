//! Matched full-TUI acceptance with a local scripted provider; no paid model calls.
use super::*;
use serde_json::{json, Value};
use std::{collections::BTreeMap, path::PathBuf, sync::Mutex as StdMutex, time::Duration};
use tidepool_node::command_resources::{CommandResourcePolicy, CommandResources};
use tidepool_node::{
    ProcessInvocation, ProcessMountBoundary, ProcessSupervisorClient, ProcessSupervisorManifest,
    ProcessSupervisorObservation, ServiceEnvironment, TmuxLaunch, TmuxSession,
};

#[derive(Clone, Default)]
struct Provider(Arc<StdMutex<Vec<Value>>>);

async fn response(
    State(provider): State<Provider>,
    Json(body): Json<Value>,
) -> impl axum::response::IntoResponse {
    let mut requests = provider.0.lock().unwrap();
    let index = requests.len();
    let title = body
        .pointer("/text/format/schema/properties/title")
        .is_some();
    if !title {
        requests.push(body);
    }
    let item = if title {
        json!({"type":"message", "role":"assistant", "id":"title",
            "content":[{"type":"output_text","text":"{\"title\":\"Exercise resource limits\"}"}]})
    } else if index == 0 || index == 2 {
        let command = if index == 0 {
            "python3 -c 'a=bytearray(128*1024*1024)'"
        } else {
            "printf native-still-usable"
        };
        let args = json!({"cmd":command,"yield_time_ms":1000,"max_output_tokens":1000});
        json!({"type":"custom_tool_call", "call_id":format!("command-{index}"),
            "name":"exec", "input":format!("text(await tools.exec_command({args}));")})
    } else {
        json!({"type":"message", "role":"assistant", "id":format!("message-{index}"),
            "content":[{"type":"output_text","text":format!("fixture-turn-{index}-done")}]})
    };
    let events = [
        json!({"type":"response.created","response":{"id":format!("response-{index}")}}),
        json!({"type":"response.output_item.done","item":item}),
        json!({"type":"response.completed","response":{"id":format!("response-{index}"),
            "usage":{"input_tokens":1,"output_tokens":1,"total_tokens":2}}}),
    ];
    let stream = events
        .iter()
        .map(|event| {
            format!(
                "event: {}\ndata: {event}\n\n",
                event["type"].as_str().unwrap()
            )
        })
        .collect::<String>();
    (
        [
            ("content-type", "text/event-stream"),
            ("connection", "close"),
        ],
        stream,
    )
}

struct NativeFixture {
    session: String,
    process: Option<ProcessSupervisorClient>,
}
impl Drop for NativeFixture {
    fn drop(&mut self) {
        if let Some(mut process) = self.process.take() {
            let _ = process.stop(Duration::from_secs(10));
            let _ = process.finalize(Duration::from_secs(10));
        }
        let _ = std::process::Command::new("tmux")
            .args(["kill-session", "-t", &self.session])
            .status();
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires delegated cgroups, SHOAL_RESOURCE_CODEX_BIN and SHOAL_RESOURCE_HOST_BIN"]
async fn full_tui_survives_command_oom_and_accepts_steering() {
    use std::os::unix::fs::PermissionsExt;
    let native = PathBuf::from(
        std::env::var_os("SHOAL_RESOURCE_CODEX_BIN").expect("matched native executable"),
    );
    let host_binary = PathBuf::from(
        std::env::var_os("SHOAL_RESOURCE_HOST_BIN").expect("matched Shoal executable"),
    );
    assert!(native.is_absolute() && host_binary.is_absolute());
    let code_mode_host = native.with_file_name("codex-code-mode-host");
    assert!(
        code_mode_host.is_file(),
        "build the matched codex-code-mode-host beside {} before running full-TUI acceptance",
        native.display()
    );
    let owner = CommandResources::delegated(CommandResourcePolicy {
        memory_high_bytes: None,
        memory_max_bytes: 64 * 1024 * 1024,
        swap_max_bytes: 0,
        ..Default::default()
    })
    .unwrap();
    let temp = tempfile::tempdir().unwrap();
    std::fs::set_permissions(temp.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    let home = temp.path().join("codex-home");
    let work = temp.path().join("work");
    let private = temp.path().join("supervisor");
    for path in [&home, &work, &private] {
        std::fs::create_dir(path).unwrap();
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700)).unwrap();
    }
    let provider = Provider::default();
    let tcp = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = tcp.local_addr().unwrap();
    let app = Router::new()
        .route("/v1/responses", post(response))
        .with_state(provider.clone());
    let provider_task = tokio::spawn(async move { axum::serve(tcp, app).await.unwrap() });
    std::fs::write(
        home.join("config.toml"),
        format!(
            r#"
model = "gpt-5.6-sol"
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
    let socket = temp.path().join("host.sock");
    let listener = UnixListener::bind(&socket).unwrap();
    let service =
        HostDynamicToolService::new(test_endpoint(), temp.path().join("binding.json"), None)
            .unwrap()
            .with_command_resources(Some((owner.clone(), "tui".into())));
    let control = service.control();
    let server = tokio::spawn(service.serve(listener));
    let environment = BTreeMap::from([
        ("CODEX_HOME".into(), home.display().to_string()),
        ("CODEX_WORKSPACE_SNAPSHOTS".into(), "1".into()),
        (
            "CODEX_COMMAND_RESOURCE_SOCKET".into(),
            socket.display().to_string(),
        ),
        (
            "CODEX_COMMAND_WRITER_CGROUP".into(),
            owner.actor_directory("tui").unwrap().display().to_string(),
        ),
        ("TERM".into(), "xterm-256color".into()),
    ]);
    let bubblewrap = std::process::Command::new("which")
        .arg("bwrap")
        .output()
        .unwrap();
    assert!(bubblewrap.status.success());
    let manifest = ProcessSupervisorManifest::new(
        "tui-test".into(),
        "p".repeat(64),
        "r".repeat(64),
        private,
        PathBuf::from(String::from_utf8(bubblewrap.stdout).unwrap().trim()),
        ProcessMountBoundary::new(&work, [work.clone()], [work.clone()]).unwrap(),
        ProcessInvocation {
            program: native.display().to_string(),
            args: vec![
                "--no-alt-screen".into(),
                "--host-dynamic-tools-socket".into(),
                socket.display().to_string(),
                "-C".into(),
                work.display().to_string(),
                "fixture bootstrap".into(),
            ],
        },
        ServiceEnvironment {
            set: environment.clone(),
            unset: Default::default(),
        },
    )
    .unwrap();
    let supervisor_socket = manifest.socket_path();
    let manifest_path = manifest.write_new().unwrap();
    let session = format!("shoal-resource-test-{}", uuid::Uuid::new_v4().simple());
    let tmux = TmuxSession::new(&session).unwrap();
    let mut fixture = NativeFixture {
        session,
        process: None,
    };
    let pane = tmux
        .create(&TmuxLaunch {
            window_name: "native".into(),
            cwd: work,
            program: host_binary.display().to_string(),
            args: vec![
                "process-supervisor".into(),
                "--manifest".into(),
                manifest_path.display().to_string(),
            ],
            environment,
            unset_environment: Default::default(),
        })
        .await
        .unwrap();
    tmux.retain_pane_on_exit(&pane).await.unwrap();
    tokio::time::timeout(Duration::from_secs(10), async {
        while !supervisor_socket.exists() {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    let (process, _) = ProcessSupervisorClient::pair(
        supervisor_socket,
        "tui-test".into(),
        "p".repeat(64),
        Duration::from_secs(10),
    )
    .unwrap();
    fixture.process = Some(process);
    let process = fixture.process.as_mut().unwrap();
    assert_eq!(
        process.prepare(Duration::from_secs(10)).unwrap(),
        ProcessSupervisorObservation::Blocked
    );
    assert_eq!(
        process.pin(Duration::from_secs(10)).unwrap(),
        ProcessSupervisorObservation::Pinned
    );
    assert_eq!(
        process.release(Duration::from_secs(10)).unwrap(),
        ProcessSupervisorObservation::Released
    );
    for expected in [2, 4] {
        let reached = tokio::time::timeout(Duration::from_secs(60), async {
            while provider.0.lock().unwrap().len() < expected {
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        })
        .await;
        if reached.is_err() {
            let captured = std::process::Command::new("tmux")
                .args(["capture-pane", "-p", "-t", pane.as_str(), "-S", "-100"])
                .output()
                .unwrap();
            panic!(
                "native fixture timed out: {}",
                String::from_utf8_lossy(&captured.stdout)
            );
        }
        let text = {
            let requests = provider.0.lock().unwrap();
            requests[expected - 1]["input"]
                .as_array()
                .unwrap()
                .iter()
                .filter(|item| {
                    matches!(
                        item["type"].as_str(),
                        Some("function_call_output" | "custom_tool_call_output")
                    )
                })
                .map(Value::to_string)
                .collect::<Vec<_>>()
                .join("\n")
        };
        assert!(
            text.contains(if expected == 2 {
                "resource limit"
            } else {
                "native-still-usable"
            }),
            "{text}"
        );
        assert!(!tmux.pane_status(&pane).await.unwrap().unwrap().dead);
        if expected == 2 {
            assert!(std::process::Command::new("tmux")
                .args(["send-keys", "-t", pane.as_str(), "-l", "continue fixture"])
                .status()
                .unwrap()
                .success());
            // The TUI distinguishes pasted text from a subsequent submit key.
            tokio::time::sleep(Duration::from_millis(200)).await;
            assert!(std::process::Command::new("tmux")
                .args(["send-keys", "-t", pane.as_str(), "Enter"])
                .status()
                .unwrap()
                .success());
        }
    }
    drop(fixture);
    control.drain();
    server.await.unwrap().unwrap();
    provider_task.abort();
}
