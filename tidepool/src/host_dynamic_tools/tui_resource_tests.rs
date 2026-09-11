//! Matched full-TUI acceptance with a local scripted provider; no paid model calls.
use super::*;
use crate::host_dynamic_tools::{HostDynamicToolService, MODEL_OUTPUT_LIMIT};
use axum::{extract::State, routing::post, Json, Router};
use serde_json::{json, Value};
use std::{collections::BTreeMap, path::PathBuf, sync::Mutex as StdMutex, time::Duration};
use tidepool_node::command_resources::{CommandResourcePolicy, CommandResources};
use tidepool_node::{
    ProcessInvocation, ProcessMountBoundary, ProcessSupervisorClient, ProcessSupervisorManifest,
    ProcessSupervisorObservation, ServiceEnvironment, TmuxLaunch, TmuxSession,
};
use tokio::net::UnixListener;

#[derive(Clone)]
struct Provider {
    requests: Arc<StdMutex<Vec<Value>>>,
    steps: Arc<Vec<Option<String>>>,
    work: PathBuf,
    shell: tidepool_agent::InteractiveShellTools,
}

async fn response(
    State(provider): State<Provider>,
    Json(body): Json<Value>,
) -> impl axum::response::IntoResponse {
    let title = body
        .pointer("/text/format/schema/properties/title")
        .is_some();
    let index = {
        let mut requests = provider.requests.lock().unwrap();
        let index = requests.len();
        if !title {
            requests.push(body);
        }
        index
    };
    if let Some(marker) = match index {
        3 => Some("holder-started"),
        10 => Some("cancel-started"),
        14 => Some("terminal-started"),
        24 => Some("structured-started"),
        28 => Some("structured-interrupt-ready"),
        _ => None,
    } {
        tokio::time::timeout(Duration::from_secs(120), async {
            while !provider.work.join(marker).exists() {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("prior native command must actually start");
    }
    let item = if title {
        json!({"type":"message", "role":"assistant", "id":"title",
            "content":[{"type":"output_text","text":"{\"title\":\"Exercise resource limits\"}"}]})
    } else if index == 0 {
        let args = match provider.shell {
            tidepool_agent::InteractiveShellTools::Native => {
                json!({"cmd":"python3 -c 'a=bytearray(512*1024*1024)'", "yield_time_ms":1000,"max_output_tokens":1000})
            }
            tidepool_agent::InteractiveShellTools::Hosted => {
                json!({"cmd":"python3 -c 'a=bytearray(512*1024*1024)'", "memory_mib":64,"yield_time_ms":30000})
            }
        };
        json!({"type":"function_call", "call_id":"initial-oom", "name":"exec_command", "arguments":args.to_string()})
    } else if index == 40 {
        json!({"type":"function_call", "call_id":"structured-40", "name":"exec_command",
            "arguments":json!({"cmd":"python3 -c 'import sys; sys.stdout.buffer.write(bytes([255])*9000)'", "memory_mib":64,"yield_time_ms":30000}).to_string()})
    } else if index == 41 || index == 42 {
        let requests = provider.requests.lock().unwrap();
        let output = |call: &str| {
            requests[index]["input"]
                .as_array()
                .unwrap()
                .iter()
                .find(|item| item["call_id"] == call && item["type"] == "function_call_output")
                .and_then(|item| item["output"].as_str())
                .unwrap()
        };
        let session = output("structured-40")
            .split("session_id: ")
            .nth(1)
            .unwrap()
            .split_whitespace()
            .next()
            .unwrap();
        let offset = if index == 41 {
            0
        } else {
            output("structured-41")
                .split("next_offset: ")
                .nth(1)
                .unwrap()
                .trim()
                .parse::<usize>()
                .unwrap()
        };
        json!({"type":"function_call", "call_id":format!("structured-{index}"), "name":"read_output",
            "arguments":json!({"session_id":session,"offset":offset,"max_output_bytes":if index == 41 {1024} else {8192}}).to_string()})
    } else if index == 30 || index == 34 {
        let arguments = if index == 30 {
            json!({"cmd":"wc -c", "stdin":true, "yield_time_ms":0})
        } else {
            json!({"cmd":"printf cancel-ready; exec sleep 30", "yield_time_ms":0})
        };
        json!({"type":"function_call", "call_id":format!("structured-{index}"),
            "name":"exec_command", "arguments":arguments.to_string()})
    } else if (31..=33).contains(&index) || (35..=38).contains(&index) {
        let origin = if index == 38 {
            27
        } else if index >= 35 {
            34
        } else {
            30
        };
        let requests = provider.requests.lock().unwrap();
        let receipt = requests[index]["input"]
            .as_array()
            .unwrap()
            .iter()
            .find(|item| {
                item["call_id"] == format!("structured-{origin}")
                    && item["type"] == "function_call_output"
            })
            .and_then(|item| item["output"].as_str())
            .expect("existing process receipt");
        let session = receipt
            .split("session_id: ")
            .nth(1)
            .unwrap()
            .split_whitespace()
            .next()
            .unwrap();
        let (name, arguments) = match index {
            31 => (
                "write_stdin",
                json!({"session_id":session,"chars":"abc\n","close_stdin":true,"yield_time_ms":30000}),
            ),
            32 | 38 => (
                "write_stdin",
                json!({"session_id":session,"close_stdin":true,"yield_time_ms":0}),
            ),
            37 => ("read_output", json!({"session_id":session})),
            _ => (
                "cancel_command",
                json!({"session_id":session,"yield_time_ms":30000}),
            ),
        };
        json!({"type":"function_call", "call_id":format!("structured-{index}"), "name":name,"arguments":arguments.to_string()})
    } else if index == 20 || index == 21 {
        let script = if index == 20 {
            "cat <<'EOF'\nraw λ $(literal) [bash|data|]\nEOF\nprintf 'raw-stderr\\n' >&2\n"
        } else {
            "printf 'once\\n' >> raw-start-count; printf 'RAW-BEGIN\\n'; head -c 96000 /dev/zero | tr '\\0' x; printf '\\nRAW-END\\n'; printf 'nonzero diagnostic\\n' >&2; exit 7"
        };
        json!({"type":"custom_tool_call", "call_id":format!("haskell-{index}"),
            "name":"bash", "input":script})
    } else if index == 22 {
        let requests = provider.requests.lock().unwrap();
        let receipt = requests[index]["input"]
            .as_array()
            .unwrap()
            .iter()
            .find(|item| {
                item["call_id"] == "haskell-21" && item["type"] == "custom_tool_call_output"
            })
            .and_then(|item| item["output"].as_str())
            .expect("raw tool output in provider history");
        let binding = receipt
            .lines()
            .find_map(|line| {
                line.strip_prefix("Optional Haskell binding: ")?
                    .strip_suffix(" :: Cmd.Job")
            })
            .expect("large raw command installs an actual job binding");
        json!({"type":"custom_tool_call", "call_id":"haskell-22",
            "name":"haskell",
            "input":format!("rawPage <- Cmd.output {binding}\n(T.length (Cmd.pageText rawPage), T.take 9 (Cmd.pageText rawPage))")})
    } else if index == 23 {
        json!({"type":"function_call", "call_id":"structured-23",
            "name":"exec_command", "arguments":json!({"cmd":"printf once >> structured-count; touch structured-started; read -r line; printf 'structured:%s\\n' \"$line\"; printf 'structured-error\\n' >&2; exit 7", "tty":true,"memory_mib":64,"yield_time_ms":0}).to_string()})
    } else if (24..=26).contains(&index) {
        let requests = provider.requests.lock().unwrap();
        let receipt = requests[index]["input"]
            .as_array()
            .unwrap()
            .iter()
            .find(|item| {
                item["call_id"] == "structured-23" && item["type"] == "function_call_output"
            })
            .and_then(|item| item["output"].as_str())
            .expect("structured command receipt");
        let session = receipt
            .split("session_id: ")
            .nth(1)
            .unwrap()
            .split_whitespace()
            .next()
            .unwrap();
        let (name, arguments) = if index == 24 {
            (
                "write_stdin",
                json!({"session_id":session,"chars":"hello\n","yield_time_ms":30000}),
            )
        } else {
            (
                "read_output",
                json!({"session_id":session,"stream":if index == 25 {"Stdout"} else {"Stderr"}}),
            )
        };
        json!({"type":"function_call","call_id":format!("structured-{index}"),
            "name":name,"arguments":arguments.to_string()})
    } else if index == 27 {
        json!({"type":"function_call", "call_id":"structured-27",
            "name":"exec_command", "arguments":json!({"cmd":"trap 'exit 42' INT; touch structured-interrupt-ready; while :; do sleep 1; done", "tty":true,"memory_mib":64,"yield_time_ms":0}).to_string()})
    } else if index == 28 {
        let requests = provider.requests.lock().unwrap();
        let receipt = requests[index]["input"]
            .as_array()
            .unwrap()
            .iter()
            .find(|item| {
                item["call_id"] == "structured-27" && item["type"] == "function_call_output"
            })
            .and_then(|item| item["output"].as_str())
            .unwrap();
        let session = receipt
            .split("session_id: ")
            .nth(1)
            .unwrap()
            .split_whitespace()
            .next()
            .unwrap();
        json!({"type":"function_call", "call_id":"structured-28", "name":"write_stdin",
            "arguments":json!({"session_id":session,"chars":"\u{3}","yield_time_ms":30000}).to_string()})
    } else if index == 18 {
        let requests = provider.requests.lock().unwrap();
        let receipt = requests[index]["input"]
            .as_array()
            .unwrap()
            .iter()
            .find(|item| {
                item["call_id"] == "haskell-17" && item["type"] == "custom_tool_call_output"
            })
            .and_then(|item| item["output"].as_str())
            .expect("foreground handoff in next provider request");
        let binding = receipt
            .lines()
            .find_map(|line| line.strip_suffix(" :: Cmd.Job"))
            .expect("installed recovery binding");
        assert!(binding.starts_with("job") && binding[3..].chars().all(|c| c.is_ascii_digit()));
        std::fs::write(provider.work.join("release-foreground"), "release").unwrap();
        json!({"type":"custom_tool_call", "call_id":"haskell-18",
            "name":"haskell",
            "input":format!("recovered <- Cmd.await {binding}\nCmd.stdout recovered")})
    } else if let Some(Some(source)) = provider.steps.get(index) {
        json!({"type":"custom_tool_call", "call_id":format!("haskell-{index}"),
            "name":"haskell", "input":source})
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
    run_shell_fixture(tidepool_agent::InteractiveShellTools::Hosted).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires delegated cgroups, SHOAL_RESOURCE_CODEX_BIN and SHOAL_RESOURCE_HOST_BIN"]
async fn full_tui_native_shell_oom_remains_isolated() {
    run_shell_fixture(tidepool_agent::InteractiveShellTools::Native).await;
}

async fn run_shell_fixture(shell: tidepool_agent::InteractiveShellTools) {
    use std::os::unix::fs::PermissionsExt;
    let native = PathBuf::from(
        std::env::var_os("SHOAL_RESOURCE_CODEX_BIN").expect("matched native executable"),
    );
    let host_binary = PathBuf::from(
        std::env::var_os("SHOAL_RESOURCE_HOST_BIN").expect("matched Shoal executable"),
    );
    assert!(native.is_absolute() && host_binary.is_absolute());
    let owner = CommandResources::delegated(CommandResourcePolicy {
        general_bytes: 512 * 1024 * 1024,
        protected_bytes: 256 * 1024 * 1024,
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
    assert!(std::process::Command::new("git")
        .args(["init", "--quiet"])
        .arg(&work)
        .status()
        .unwrap()
        .success());
    let skill = work.join(".shoal/skills/shoal-command");
    std::fs::create_dir_all(&skill).unwrap();
    std::fs::write(
        skill.join("SKILL.md"),
        include_str!("../../../examples/shoal-workspace/.shoal/skills/shoal-command/SKILL.md"),
    )
    .unwrap();
    std::fs::create_dir_all(work.join(".agents/skills")).unwrap();
    std::os::unix::fs::symlink(
        "../../.shoal/skills/shoal-command",
        work.join(".agents/skills/shoal-command"),
    )
    .unwrap();
    let plan_dir = work.join("plans/parallel-dogfood");
    std::fs::create_dir_all(plan_dir.join("next-wave")).unwrap();
    let resume = include_str!("../../../plans/parallel-dogfood/next-wave/resume.md");
    let readme = include_str!("../../../plans/parallel-dogfood/next-wave/README.md");
    let planner = include_str!("../../../plans/parallel-dogfood/planner.md");
    let skill_text = std::fs::read_to_string(skill.join("SKILL.md")).unwrap();
    let skill_description = skill_text
        .lines()
        .find_map(|line| line.strip_prefix("description: "))
        .unwrap();
    // Exercise the gap between the old native history allowance and hosted cap.
    let padding = 60_000usize
        .checked_sub(skill_text.len() + resume.len() + readme.len() + planner.len() + 100)
        .expect("four-file fixture must fit its presentation budget");
    let readme = format!(
        "{readme}\nREAD-BEGIN\n{}\nREAD-MIDDLE\n{}\nREAD-END\n",
        "a".repeat(padding / 2),
        "b".repeat(padding - padding / 2)
    );
    std::fs::write(plan_dir.join("next-wave/resume.md"), resume).unwrap();
    std::fs::write(plan_dir.join("next-wave/README.md"), &readme).unwrap();
    std::fs::write(plan_dir.join("planner.md"), planner).unwrap();
    let expected_read = format!("{skill_text}{resume}{readme}{planner}");
    let mut campaign = test_campaign::TestCampaign::start().await;
    let actor = campaign.actor.identity();
    let actor_key = format!("{}-{}", actor.id.0, actor.incarnation.0);
    let client = tidepool_node::command_resources::CommandResourceClient::local(owner.clone());
    let snippets: Vec<_> = include_str!("tui_commands.hs")
        .split("-- fixture-step\n")
        .map(str::to_owned)
        .collect();
    let mut steps = vec![None, None];
    steps.extend(snippets[..3].iter().cloned().map(Some));
    steps.push(None);
    steps.extend(snippets[3..].iter().cloned().map(Some));
    steps.push(None);
    steps.extend(
        include_str!("tui_foreground_commands.hs")
            .split("-- fixture-step\n")
            .map(|source| Some(source.to_owned())),
    );
    steps.extend([None, None]);
    let provider = Provider {
        requests: Arc::new(StdMutex::new(Vec::new())),
        steps: Arc::new(steps),
        work: work.clone(),
        shell,
    };
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
tool_output_token_limit = 16384
[features]
shell_tool = {}
[model_providers.fixture]
name = "Fixture"
base_url = "http://{address}/v1"
wire_api = "responses"
requires_openai_auth = false
supports_websockets = false
[projects.{}]
trust_level = "trusted"
"#,
            shell == tidepool_agent::InteractiveShellTools::Native,
            toml::Value::String(work.display().to_string())
        ),
    )
    .unwrap();
    let socket = temp.path().join("host.sock");
    let listener = UnixListener::bind(&socket).unwrap();
    let binding_path = temp.path().join("binding.json");
    let service = HostDynamicToolService::new(
        match shell {
            tidepool_agent::InteractiveShellTools::Hosted => {
                campaign.root_installation.policy.clone()
            }
            tidepool_agent::InteractiveShellTools::Native => {
                crate::host_dynamic_tools::tests::endpoint()
            }
        },
        binding_path.clone(),
        None,
    )
    .unwrap()
    .with_command_resources(Some((client.clone(), actor_key.clone())));
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
            owner
                .actor_directory(&actor_key)
                .unwrap()
                .display()
                .to_string(),
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
                "--disable".into(),
                "code_mode".into(),
                "--disable".into(),
                "code_mode_only".into(),
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
    let slice = tidepool_node::systemd_slice::SystemdSlice::default();
    slice.inspect().await.unwrap();
    slice
        .current_membership()
        .expect("test resource owner must share the swarm budget");
    let launch = slice.scope(slice.verified_command(
        &host_binary,
        ProcessInvocation {
            program: host_binary.display().to_string(),
            args: vec![
                "process-supervisor".into(),
                "--manifest".into(),
                manifest_path.display().to_string(),
            ],
        },
    ));
    let pane = tmux
        .create(&TmuxLaunch {
            window_name: "native".into(),
            cwd: work.clone(),
            program: launch.program,
            args: launch.args,
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
    let native_backend = tidepool_agent::native_interactive_backend(
        tidepool_agent::native_interactive_agent_from_parts(native, "command acceptance".into())
            .unwrap(),
    );
    let backend_task = tokio::spawn(async move {
        while let Some(deployment) = campaign.deployments.recv().await {
            if let LocalResidentDeployment::CommandBackend(request) = deployment {
                assert_eq!(request.owner, actor);
                let thread = tidepool_agent::read_interactive_binding(&binding_path)
                    .await
                    .unwrap();
                request.supply(Ok(Arc::new(commands::NativeCommandBackend::new(
                    native_backend.clone(),
                    thread,
                    client.clone(),
                    actor,
                ))));
            }
        }
    });
    let phases: &[usize] = match shell {
        tidepool_agent::InteractiveShellTools::Hosted => &[2, 6, 16, 20, 30, 40, 44],
        tidepool_agent::InteractiveShellTools::Native => &[2],
    };
    for &expected in phases {
        let reached = tokio::time::timeout(
            Duration::from_secs(if expected == 2 { 90 } else { 600 }),
            async {
                while provider.requests.lock().unwrap().len() < expected {
                    tokio::time::sleep(Duration::from_millis(50)).await;
                }
            },
        )
        .await;
        eprintln!(
            "TUI fixture: reached {} of {expected} provider requests",
            provider.requests.lock().unwrap().len()
        );
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
        let requests = provider.requests.lock().unwrap().clone();
        let output = |index: usize| {
            let call = if index == 1 {
                "initial-oom".to_owned()
            } else {
                format!("haskell-{}", index - 1)
            };
            requests[index]["input"]
                .as_array()
                .unwrap()
                .iter()
                .filter(|item| {
                    item["call_id"] == call
                        && matches!(
                            item["type"].as_str(),
                            Some("function_call_output" | "custom_tool_call_output")
                        )
                })
                .map(|item| {
                    item["output"]
                        .as_str()
                        .expect("textual command result")
                        .to_owned()
                })
                .collect::<Vec<_>>()
                .join("\n")
        };
        match expected {
            2 => {
                let oom = match shell {
                    tidepool_agent::InteractiveShellTools::Native => "resource limit",
                    tidepool_agent::InteractiveShellTools::Hosted => "CommandOutOfMemory",
                };
                assert!(output(1).contains(oom), "{}", output(1));
                assert!(
                    requests[0]["input"].to_string().contains(skill_description),
                    "command skill must appear in native skill discovery"
                );
                let tools = &requests[0]["input"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .find(|item| item["type"] == "additional_tools")
                    .expect("native provider request advertises tools")["tools"];
                let flat = tools
                    .as_array()
                    .expect("tool list")
                    .iter()
                    .find(|tool| tool["type"] == "namespace" && tool["name"] == "functions")
                    .expect("default tools share Codex's functions group")["tools"]
                    .as_array()
                    .expect("default tool list");
                for name in ["exec_command", "write_stdin", "haskell", "apply_patch"] {
                    assert_eq!(
                        flat.iter().filter(|tool| tool["name"] == name).count(),
                        1,
                        "{tools}"
                    );
                }
                if shell == tidepool_agent::InteractiveShellTools::Hosted {
                    for name in ["bash", "read_output", "cancel_command"] {
                        assert_eq!(
                            flat.iter().filter(|tool| tool["name"] == name).count(),
                            1,
                            "{tools}"
                        );
                    }
                    let exec = flat
                        .iter()
                        .find(|tool| tool["name"] == "exec_command")
                        .unwrap();
                    assert!(
                        exec.to_string().contains("memory_mib"),
                        "hosted schema required: {exec}"
                    );
                    assert!(
                        !exec.to_string().contains("sandbox_permissions"),
                        "native schema leaked: {exec}"
                    );
                }
                assert!(!tools.to_string().contains("tidepool_actor"), "{tools}");
                assert!(
                    !flat
                        .iter()
                        .any(|tool| tool["name"] == "exec" || tool["name"] == "shell_command"),
                    "{tools}"
                );
            }
            6 => {
                assert!(output(4).contains("CommandQueued"), "{}", output(4));
                assert!(output(5).contains("protected-slot-usable"), "{}", output(5));
                assert!(!work.join("release-holder").exists());
                std::fs::write(work.join("release-holder"), "release").unwrap();
            }
            16 => {
                let size = std::process::Command::new("tmux")
                    .args([
                        "display-message",
                        "-p",
                        "-t",
                        pane.as_str(),
                        "#{pane_height} #{pane_width}",
                    ])
                    .output()
                    .unwrap();
                assert!(size.status.success());
                assert_eq!(
                    std::fs::read_to_string(work.join("terminal-size"))
                        .unwrap()
                        .trim(),
                    String::from_utf8(size.stdout).unwrap().trim(),
                    "PTY inherits the owning TUI dimensions"
                );
                for (index, expected) in [
                    (7, "admitted-after-release"),
                    (8, "input-closed"),
                    (9, "CommandOutOfMemory"),
                    (11, "CommandCancelled"),
                    (12, "TAIL-MARKER"),
                    (13, "completion-once"),
                    (15, "terminal:hello"),
                ] {
                    assert!(
                        output(index).contains(expected),
                        "step {index}: {}",
                        output(index)
                    );
                }
                assert!(output(12).contains("page-contiguous"), "{}", output(12));
                assert!(!output(12).contains("retention loss"), "{}", output(12));
            }
            20 => {
                assert!(
                    output(17).contains(&expected_read),
                    "four-file read lost content in provider history"
                );
                let handoff = output(18);
                assert!(handoff.contains(" :: Cmd.Job"), "{handoff}");
                assert!(
                    handoff.contains("subsequent statements did not run"),
                    "{handoff}"
                );
                assert!(handoff.contains("foreground-started"), "{handoff}");
                assert!(!work.join("forbidden-suffix").exists());
                assert_eq!(
                    std::fs::read_to_string(work.join("foreground-start-count")).unwrap(),
                    "once\n"
                );
                assert!(output(19).contains("foreground-finished"), "{}", output(19));
                for index in [17, 18, 19] {
                    assert!(output(index).len() <= MODEL_OUTPUT_LIMIT);
                }
            }
            30 => {
                assert!(
                    output(21).contains("raw λ $(literal) [bash|data|]"),
                    "{}",
                    output(21)
                );
                assert!(output(21).contains("raw-stderr"), "{}", output(21));
                assert!(
                    !output(21).contains(" :: Cmd.Job"),
                    "short calls need no binding"
                );
                let large = output(22);
                assert!(large.len() <= 32 * 1024, "{}", large.len());
                for expected in [
                    "RAW-BEGIN",
                    "RAW-END",
                    "CommandExited 7",
                    "nonzero diagnostic",
                    " :: Cmd.Job",
                ] {
                    assert!(large.contains(expected), "missing {expected}: {large}");
                }
                assert!(output(23).contains("RAW-BEGIN"), "{}", output(23));
                let native_output = |index: usize| {
                    requests[index]["input"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .find(|item| {
                            item["call_id"] == format!("structured-{}", index - 1)
                                && item["type"] == "function_call_output"
                        })
                        .and_then(|item| item["output"].as_str())
                        .unwrap()
                        .to_owned()
                };
                assert!(
                    native_output(25).contains("CommandExited 7"),
                    "{}",
                    native_output(25)
                );
                assert!(
                    native_output(26).contains("structured:hello"),
                    "{}",
                    native_output(26)
                );
                assert!(
                    native_output(29).contains("CommandExited 42"),
                    "{}",
                    native_output(29)
                );
                assert_eq!(
                    std::fs::read_to_string(work.join("structured-count")).unwrap(),
                    "once"
                );
                assert_eq!(
                    std::fs::read_to_string(work.join("raw-start-count")).unwrap(),
                    "once\n"
                );
            }
            40 => {
                let receipt = |index: usize| {
                    requests[index]["input"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .find(|item| {
                            item["call_id"] == format!("structured-{}", index - 1)
                                && item["type"] == "function_call_output"
                        })
                        .and_then(|item| item["output"].as_str())
                        .unwrap()
                };
                assert!(receipt(32).contains("CommandExited 0"), "{}", receipt(32));
                assert!(receipt(32).contains("\n4"), "{}", receipt(32));
                assert!(receipt(33).contains("Stdin is closed"), "{}", receipt(33));
                assert!(
                    receipt(34).contains("CommandExited 0"),
                    "finished outcome changed: {}",
                    receipt(34)
                );
                for index in [36, 37] {
                    assert!(
                        receipt(index).contains("CommandCancelled"),
                        "{}",
                        receipt(index)
                    );
                }
                assert!(receipt(38).contains("bytes"), "{}", receipt(38));
                assert!(receipt(39).contains("PTY"), "{}", receipt(39));
                assert!(
                    receipt(39).contains("input not submitted"),
                    "{}",
                    receipt(39)
                );
                for index in [32, 34, 36, 37] {
                    assert!(
                        receipt(index).contains("cleanup: clean"),
                        "{}",
                        receipt(index)
                    );
                }
            }
            44 => {
                let receipt = |call: &str| {
                    requests[43]["input"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .find(|item| {
                            item["call_id"] == call && item["type"] == "function_call_output"
                        })
                        .and_then(|item| item["output"].as_str())
                        .unwrap()
                };
                let first = receipt("structured-41");
                let next = receipt("structured-42");
                assert!(first.len() <= 1024, "{} bytes: {first}", first.len());
                assert!(next.len() <= 8192, "{} bytes", next.len());
                let position = |text: &str| {
                    text.split("next_offset: ")
                        .nth(1)
                        .unwrap()
                        .trim()
                        .parse::<usize>()
                        .unwrap()
                };
                let end = position(first);
                assert_eq!(end, first.matches('�').count());
                assert!(next.contains(&format!("bytes {end}–")), "{next}");
                assert_eq!(position(next) - end, next.matches('�').count());
            }
            _ => unreachable!(),
        }
        assert!(!tmux.pane_status(&pane).await.unwrap().unwrap().dead);
        if Some(&expected) != phases.last() {
            assert!(std::process::Command::new("tmux")
                .args(["send-keys", "-t", pane.as_str(), "-l", "continue fixture"])
                .status()
                .unwrap()
                .success());
            tokio::time::sleep(Duration::from_millis(200)).await;
            assert!(std::process::Command::new("tmux")
                .args(["send-keys", "-t", pane.as_str(), "Enter"])
                .status()
                .unwrap()
                .success());
        }
    }
    campaign.forest.shutdown().await;
    campaign.hosted.await.unwrap();
    backend_task.abort();
    drop(fixture);
    control.drain();
    server.await.unwrap().unwrap();
    provider_task.abort();
}
