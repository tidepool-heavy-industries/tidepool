//! Production Shoal/TUI/fork composition with a local scripted provider.
use super::*;
use serde_json::{json, Value};
use std::{collections::BTreeMap, path::Path, sync::Mutex as StdMutex, time::Duration};

#[derive(Clone, Default)]
struct Provider(Arc<StdMutex<BTreeMap<String, Vec<Value>>>>);

fn workspace_config() -> String {
    let resources =
        toml::to_string(&tidepool_node::command_resources::CommandResourcePolicy::default())
            .unwrap();
    format!("[defaults]\nmodel=\"gpt-5.6-sol\"\neffort=\"low\"\n[resources]\n{resources}")
}
fn oom_allocation_bytes() -> u64 {
    let policy = tidepool_node::command_resources::CommandResourcePolicy::default();
    policy.swap_max_bytes + tidepool_node::command_resources::GIB
}

#[test]
fn workspace_fixture_config_matches_shoal_schema() {
    toml::from_str::<crate::shoal::ShoalConfig>(&workspace_config()).unwrap();
    let policy = tidepool_node::command_resources::CommandResourcePolicy::default();
    assert!(
        oom_allocation_bytes()
            > tidepool_node::command_resources::NATIVE_COMMAND_BYTES + policy.swap_max_bytes
    );
}

fn shell(role: &str, command: &str) -> Value {
    let args = json!({"cmd":format!("set -eux; {command}; printf CHECKED-{role}"),"login":false,"yield_time_ms":10000,"max_output_tokens":3000});
    json!({"type":"function_call","name":"exec_command","arguments":args.to_string()})
}
fn haskell(code: &str) -> Value {
    json!({"type":"custom_tool_call","name":"haskell","input":code})
}
fn is_tool_output(item: &Value) -> bool {
    item["type"] == "custom_tool_call_output" || item["type"] == "function_call_output"
}
fn completed_command_output<'a>(item: &'a Value, marker: &str) -> Option<&'a str> {
    let output = (item["type"] == "function_call_output")
        .then(|| item["output"].as_str())
        .flatten()?;
    output
        .lines()
        .next()
        .is_some_and(|line| line.contains("CommandExited 0"))
        .then_some(output)
        .filter(|output| output.contains(marker))
}
fn running_command_output(item: &Value) -> bool {
    (item["type"] == "function_call_output")
        .then(|| item["output"].as_str())
        .flatten()
        .and_then(|output| output.lines().next())
        .is_some_and(|line| line.starts_with("Running · session_id: "))
}

#[test]
fn workspace_fixture_recognizes_completed_command_output() {
    let output = |text| json!({"type":"function_call_output","output":text});
    assert!(completed_command_output(
        &output("Finished · CommandExited 0 · cleanup: clean\nstdout:\nCHECKED-root\n"),
        "CHECKED-root"
    )
    .is_some());
    assert!(completed_command_output(
        &output("Finished · CommandExited 7 · cleanup: clean\nstdout:\nCHECKED-root\n"),
        "CHECKED-root"
    )
    .is_none());
    assert!(completed_command_output(
        &output("Script running with session ID 42\n"),
        "CHECKED-root"
    )
    .is_none());
    assert!(running_command_output(&output(
        "Running · session_id: b885ec4a-2f7c-4a36-b89b-3ed93be60572\nstderr:\n+ exec sleep infinity\n"
    )));
    assert!(!running_command_output(&output(
        "Running · stderr available, but no retained session identity\n"
    )));
    assert!(completed_command_output(
        &output("Finished · CommandExited 0 · cleanup: clean\n"),
        "CHECKED-root"
    )
    .is_none());
}
async fn scripted(
    State(provider): State<Provider>,
    Json(body): Json<Value>,
) -> impl axum::response::IntoResponse {
    let title = body
        .pointer("/text/format/schema/properties/title")
        .is_some();
    let mut role = "root";
    if let Some(input) = body["input"].as_array() {
        for item in input.iter().filter(|item| item["role"] == "user") {
            let text = item.to_string();
            for candidate in ["root", "child", "grandchild", "sibling", "later", "busy"] {
                if text.contains(&format!("fixture-{candidate}")) {
                    role = candidate;
                }
            }
        }
    }
    let mut requests = provider.0.lock().unwrap();
    let turns = requests.entry(role.into()).or_default();
    let index = turns.len();
    if !title {
        turns.push(body);
    }
    if role == "root" && index == 7 {
        assert!(turns.last().is_some_and(|request| {
            request["input"].as_array().is_some_and(|input| {
                input
                    .iter()
                    .any(|item| item["call_id"] == "root-6" && running_command_output(item))
            })
        }));
    }
    let inherited = "test -r .shoal/config.toml; test ! -w .shoal/config.toml; test \"$(cat untracked)\" = untracked; test \"$(stat -c %y tracked)\" = \"$(cat source-mtime)\"; CARGO_LOG=cargo::core::compiler::fingerprint=info cargo build --offline --message-format=json > build-result; rg '\"fresh\":true' build-result";
    let mut item = if title {
        json!({"type":"message","role":"assistant","id":"title","content":[{"type":"output_text","text":"{\"title\":\"Workspace acceptance\"}"}]})
    } else {
        match (role, index) {
            ("root", 0) => shell(role, "printf root-v1 > tracked; printf untracked > untracked; stat -c %y tracked > source-mtime; cargo build --offline"),
            ("root", 1) => haskell(include_str!("fixtures/workspace/root-fork.hs")),
            ("root", 2) => shell(role, "printf root-v2 > tracked; stat -c %y tracked > source-mtime"),
            ("root", 3) => haskell(include_str!("fixtures/workspace/root-later.hs")),
            ("root", 4) => shell(role, "test \"$(cat tracked)\" = root-v2"),
            ("root", 6) => shell(role, "echo $$ > writer-pid; exec sleep infinity"),
            ("root", 7) => haskell(include_str!("fixtures/workspace/root-busy.hs")),
            ("root", 8) => shell(role, "kill $(cat writer-pid)"),
            ("root", 9) => shell(
                role,
                &format!("python3 -c 'a=bytearray({})'", oom_allocation_bytes()),
            ),
            ("root", 11) => shell(role, "test \"$(cat tracked)\" = root-v2"),
            ("busy", 0) => shell(role, "test \"$(cat tracked)\" = base; test ! -e untracked; test -x \"$CARGO_TARGET_DIR/debug/workspace-acceptance\"; cargo build --offline"),
            ("child", 0) => shell(role, &format!("test \"$(cat tracked)\" = root-v1; {inherited}; printf child-v1 > tracked; stat -c %y tracked > source-mtime")),
            ("child", 1) => haskell(include_str!("fixtures/workspace/child-fork.hs")),
            ("child", 2) => shell(role, "printf child-v2 > tracked; stat -c %y tracked > source-mtime; git add tracked; git commit -m child-v2"),
            ("child", 3) => haskell(include_str!("fixtures/workspace/child-later.hs")),
            ("child", 4) => shell(role, "test \"$(cat tracked)\" = child-v2; test \"$(git log -1 --format=%s)\" = child-v2"),
            ("grandchild", 0) => shell(role, &format!("test \"$(cat tracked)\" = child-v1; {inherited}; printf private > grandchild-only")),
            ("later", 0) => shell(role, &format!("test \"$(cat tracked)\" = child-v2; test \"$(git log -1 --format=%s)\" = child-v2; test ! -e grandchild-only; {inherited}")),
            ("sibling", 0) => shell(role, &format!("test \"$(cat tracked)\" = root-v2; test ! -e grandchild-only; {inherited}")),
            ("root", _) => json!({"type":"message","role":"assistant","id":"done","content":[{"type":"output_text","text":"fixture-root-done"}]}),
            _ => json!({"type":"message","role":"assistant","id":"done","content":[{"type":"output_text","text":"fixture-complete"}]}),
        }
    };
    if item["type"] == "custom_tool_call" || item["type"] == "function_call" {
        item["call_id"] = format!("{role}-{index}").into();
    }
    let events = [
        json!({"type":"response.created","response":{"id":format!("{role}-{index}")}}),
        json!({"type":"response.output_item.done","item":item}),
        json!({"type":"response.completed","response":{"id":format!("{role}-{index}"),"usage":{"input_tokens":1,"output_tokens":1,"total_tokens":2}}}),
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

fn files(root: &Path, name: &str) -> Vec<PathBuf> {
    let mut found = Vec::new();
    if let Ok(entries) = std::fs::read_dir(root) {
        for entry in entries.flatten() {
            let path = entry.path();
            if entry.file_type().is_ok_and(|kind| kind.is_dir()) {
                found.extend(files(&path, name));
            } else if path.file_name().is_some_and(|value| value == name) {
                found.push(path);
            }
        }
    }
    found
}
struct Run {
    temp: Option<tempfile::TempDir>,
    session: String,
    scratch: PathBuf,
}
impl Drop for Run {
    fn drop(&mut self) {
        if std::thread::panicking() {
            if let Some(temp) = self.temp.take() {
                eprintln!(
                    "failed workspace fixture retained at {}",
                    temp.keep().display()
                );
            }
        }
        for path in files(&self.scratch, "manifest.json") {
            let Ok(bytes) = std::fs::read(&path) else {
                continue;
            };
            let Ok(value) = serde_json::from_slice::<Value>(&bytes) else {
                continue;
            };
            let (Some(launch), Some(secret)) = (
                value["launch_id"].as_str(),
                value["recovery_secret"].as_str(),
            ) else {
                continue;
            };
            if let Ok((mut owner, _)) = tidepool_node::ProcessSupervisorRecovery::recover(
                path.with_file_name("scope.sock"),
                launch.into(),
                secret.into(),
                Duration::from_secs(10),
            ) {
                let _ = owner.stop(Duration::from_secs(10));
                let _ = owner.finalize(Duration::from_secs(10));
            }
        }
        let _ = std::process::Command::new("tmux")
            .args(["kill-session", "-t", &self.session])
            .status();
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires packaged SHOAL_RESOURCE_HOST_BIN and matched native; local provider only"]
async fn production_tuis_fork_live_workspaces_recursively() {
    let host = std::env::var_os("SHOAL_RESOURCE_HOST_BIN").expect("packaged test runner");
    let temp = tempfile::tempdir().unwrap();
    let temp_root = temp.path().to_path_buf();
    eprintln!("workspace fixture: {}", temp_root.as_path().display());
    let root = temp_root.as_path().join("repo");
    let home = temp_root.as_path().join("codex-home");
    let scratch = temp_root.as_path().join("tmp");
    for dir in [
        root.join("src"),
        root.join(".shoal"),
        home.clone(),
        scratch.clone(),
    ] {
        std::fs::create_dir_all(dir).unwrap();
    }
    std::fs::write(
        root.join("Cargo.toml"),
        "[package]\nname=\"workspace-acceptance\"\nversion=\"0.1.0\"\nedition=\"2021\"\n",
    )
    .unwrap();
    std::fs::write(root.join("src/main.rs"), "fn main() {}\n").unwrap();
    std::fs::write(root.join("tracked"), "base").unwrap();
    std::fs::write(root.join(".gitignore"), ".shoal/\ntarget/\nbuild-result\n").unwrap();
    std::fs::write(root.join(".shoal/config.toml"), workspace_config()).unwrap();
    for args in [
        vec!["init"],
        vec!["config", "user.email", "fixture@example.invalid"],
        vec!["config", "user.name", "Fixture"],
        vec!["add", "."],
        vec!["commit", "-m", "fixture"],
    ] {
        assert!(std::process::Command::new("git")
            .arg("-C")
            .arg(&root)
            .args(args)
            .output()
            .unwrap()
            .status
            .success());
    }
    let provider = Provider::default();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let app = Router::new()
        .route("/v1/responses", post(scripted))
        .with_state(provider.clone());
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    std::fs::write(home.join("config.toml"),format!("model_provider=\"fixture\"\napproval_policy=\"never\"\nsandbox_mode=\"danger-full-access\"\n[model_providers.fixture]\nname=\"Fixture\"\nbase_url=\"http://{address}/v1\"\nwire_api=\"responses\"\nrequires_openai_auth=false\nsupports_websockets=false\n")).unwrap();
    let session = format!("shoal-workspace-test-{}", uuid::Uuid::new_v4().simple());
    let run = Run {
        temp: Some(temp),
        session: session.clone(),
        scratch: scratch.clone(),
    };
    let launch_log = temp_root.as_path().join("launch.log");
    let output = std::fs::File::create(&launch_log).unwrap();
    let result = tokio::process::Command::new(host)
        .args(["init", "--workspace"])
        .arg(&root)
        .args([
            "--session",
            &session,
            "--no-attach",
            "--model",
            "gpt-5.6-sol",
            "--effort",
            "low",
        ])
        .env("CODEX_HOME", &home)
        .env("TMPDIR", &scratch)
        .env_remove("OPENAI_API_KEY")
        .env_remove("ANTHROPIC_API_KEY")
        .env_remove("GEMINI_API_KEY")
        .stdout(output.try_clone().unwrap())
        .stderr(output)
        .status()
        .await
        .unwrap();
    assert!(
        result.success(),
        "{}",
        std::fs::read_to_string(&launch_log).unwrap()
    );
    let pane = tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            let output = tokio::process::Command::new("tmux")
                .args([
                    "list-panes",
                    "-s",
                    "-t",
                    &session,
                    "-F",
                    "#{pane_id} #{window_name}",
                ])
                .output()
                .await
                .unwrap();
            if let Some(pane) = String::from_utf8_lossy(&output.stdout)
                .lines()
                .find(|line| line.contains("shoal-root"))
                .and_then(|line| line.split_whitespace().next())
                .map(str::to_owned)
            {
                break pane;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    })
    .await
    .unwrap();
    tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            let output = tokio::process::Command::new("tmux")
                .args(["capture-pane", "-p", "-t", &pane])
                .output()
                .await
                .unwrap();
            if String::from_utf8_lossy(&output.stdout)
                .contains("gpt-5.6-sol low   /model to change")
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    })
    .await
    .unwrap();
    for args in [
        vec!["send-keys", "-t", &pane, "-l", "fixture-root"],
        vec!["send-keys", "-t", &pane, "Enter"],
    ] {
        tokio::time::sleep(Duration::from_millis(200)).await;
        assert!(tokio::process::Command::new("tmux")
            .args(args)
            .status()
            .await
            .unwrap()
            .success());
    }
    let mut fault_phase = false;
    let mut steered = false;
    let completed = tokio::time::timeout(Duration::from_secs(180), async {
        loop {
            let requests = provider.0.lock().unwrap().clone();
            for values in requests.values() {
                for request in values {
                    for item in request["input"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .filter(|item| item["type"] == "function_call_output")
                    {
                        match item["call_id"].as_str() {
                            Some("root-6") => assert!(running_command_output(item), "{item}"),
                            Some("root-9") => assert!(
                                item["output"]
                                    .as_str()
                                    .is_some_and(|output| output.contains("CommandOutOfMemory")),
                                "{item}"
                            ),
                            _ => assert!(
                                completed_command_output(item, "").is_some(),
                                "{}: {}",
                                item["call_id"],
                                item["output"]
                            ),
                        }
                    }
                    let text = request["input"].to_string();
                    assert!(
                        !text.contains("unfold admission failed"),
                        "{}",
                        request["input"]
                            .as_array()
                            .unwrap()
                            .iter()
                            .filter(|item| is_tool_output(item))
                            .last()
                            .unwrap()
                    );
                }
            }
            if !steered && requests.get("root").is_some_and(|turns| turns.len() >= 11) {
                let oom = requests["root"]
                    .iter()
                    .flat_map(|request| request["input"].as_array().unwrap())
                    .find(|item| is_tool_output(item) && item["call_id"] == "root-9")
                    .expect("OOM result");
                assert!(
                    oom["output"]
                        .as_str()
                        .is_some_and(|output| output.contains("CommandOutOfMemory")),
                    "{oom}"
                );
                assert!(tokio::process::Command::new("tmux")
                    .args(["send-keys", "-t", &pane, "-l", "fixture-resume"])
                    .status()
                    .await
                    .unwrap()
                    .success());
                tokio::time::sleep(Duration::from_millis(200)).await;
                assert!(tokio::process::Command::new("tmux")
                    .args(["send-keys", "-t", &pane, "Enter"])
                    .status()
                    .await
                    .unwrap()
                    .success());
                steered = true;
            }
            let checked = |role: &str, call: &str| {
                requests.get(role).is_some_and(|turns| {
                    turns.iter().any(|request| {
                        request["input"].as_array().unwrap().iter().any(|item| {
                            if !is_tool_output(item) || item["call_id"] != call {
                                return false;
                            }
                            completed_command_output(item, &format!("CHECKED-{role}")).is_some()
                        })
                    })
                })
            };
            if !fault_phase
                && [
                    ("root", "root-4"),
                    ("child", "child-4"),
                    ("grandchild", "grandchild-0"),
                    ("sibling", "sibling-0"),
                    ("later", "later-0"),
                ]
                .iter()
                .all(|(role, call)| checked(role, call))
            {
                assert!(tokio::process::Command::new("tmux")
                    .args(["send-keys", "-t", &pane, "-l", "fixture-phase2"])
                    .status()
                    .await
                    .unwrap()
                    .success());
                tokio::time::sleep(Duration::from_millis(200)).await;
                assert!(tokio::process::Command::new("tmux")
                    .args(["send-keys", "-t", &pane, "Enter"])
                    .status()
                    .await
                    .unwrap()
                    .success());
                fault_phase = true;
            }
            if [
                ("root", "root-11"),
                ("busy", "busy-0"),
                ("child", "child-4"),
                ("grandchild", "grandchild-0"),
                ("sibling", "sibling-0"),
                ("later", "later-0"),
            ]
            .iter()
            .all(|(role, call)| checked(role, call))
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    })
    .await;
    let captured = tokio::process::Command::new("tmux")
        .args(["capture-pane", "-p", "-t", &pane, "-S", "-100"])
        .output()
        .await
        .unwrap();
    assert!(
        completed.is_ok(),
        "root: {}\nrequests: {:?}",
        String::from_utf8_lossy(&captured.stdout),
        provider
            .0
            .lock()
            .unwrap()
            .iter()
            .map(|(role, turns)| (role, turns.len()))
            .collect::<Vec<_>>()
    );
    drop(run);
    server.abort();
}
