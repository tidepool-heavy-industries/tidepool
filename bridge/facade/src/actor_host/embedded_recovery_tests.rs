use super::*;
use harness::transport::{ResponsesRequest, ResponsesTurn, TransportError, Usage};
use serde_json::json;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};

const PROCESS_TEST: &str = "actor_host::embedded_recovery_tests::production_recovery_process";
const SECRET: &str = "offline-production-recovery-secret-is-long-enough";

struct RecoveryTransport {
    root: PathBuf,
    phase: String,
    calls: AtomicUsize,
}
#[async_trait::async_trait]
impl harness::engine::ResponsesTransport for RecoveryTransport {
    async fn create(&self, request: ResponsesRequest) -> Result<ResponsesTurn, TransportError> {
        let count = self.calls.fetch_add(1, Ordering::SeqCst) + 1;
        std::fs::write(
            self.root.join(format!("{}.calls", self.phase)),
            count.to_string(),
        )
        .unwrap();
        if matches!(self.phase.as_str(), "publish-original" | "execute-original") {
            assert!(
                count <= 16,
                "resident fixture exceeded provider request budget"
            );
            let call = format!("{}-cell", self.phase);
            let items = if count == 1 {
                let source = if self.phase == "publish-original" {
                    include_str!("fixtures/embedded_cold_declaration.hs")
                } else {
                    "case coldAnswer of RecoveryBox value -> value"
                };
                vec![harness::item::Item(json!({
                    "type":"custom_tool_call", "call_id":call,
                    "name":"haskell", "input":source,
                }))]
            } else {
                if let Some(item) = request.input.iter().find(|item| {
                    item.0["type"] == "custom_tool_call_output" && item.0["call_id"] == call
                }) {
                    let output: serde_json::Value =
                        serde_json::from_str(item.0["output"].as_str().unwrap()).unwrap();
                    assert!(
                        matches!(output["status"].as_str(), Some("committed" | "completed")),
                        "real resident cell did not commit: {output}"
                    );
                    let receipts = output["items"].as_array().unwrap();
                    assert!(
                        receipts.iter().all(|item| item["status"] == "committed"),
                        "resident item failed: {output}"
                    );
                    if self.phase == "publish-original" {
                        assert!(
                            receipts.iter().any(|item| item["installedBindings"]
                                .as_array()
                                .is_some_and(|names| names
                                    .iter()
                                    .any(|name| name == "coldAnswer"))),
                            "original declaration did not install its certified value: {output}"
                        );
                    } else {
                        assert_eq!(output["total"], 1, "{output}");
                        assert_eq!(output["nextIndex"], 1, "{output}");
                        assert_eq!(receipts.len(), 1, "{output}");
                        assert_eq!(
                            receipts[0]["output"].as_str().map(str::trim),
                            Some("42"),
                            "cold original constructor/value returned a different result: {output}"
                        );
                    }
                    std::fs::write(
                        self.root.join(format!("{}.settled", self.phase)),
                        serde_json::to_vec(&output).unwrap(),
                    )
                    .unwrap();
                }
                vec![harness::item::Item(
                    json!({"type":"message","role":"assistant",
                    "phase":"final_answer","content":[{"type":"output_text","text":"Waiting for the resident cell."}]}),
                )]
            };
            return Ok(ResponsesTurn {
                response_id: format!("{}-{count}", self.phase),
                items,
                usage: Usage::default(),
            });
        }
        Ok(ResponsesTurn {
            response_id: format!("{}-{count}", self.phase),
            items: vec![harness::item::Item(
                json!({"type":"message","role":"assistant",
                "phase":"final_answer","content":[{"type":"output_text","text":"retained conversation"}]}),
            )],
            usage: Usage::default(),
        })
    }
}

fn configuration(root: &Path) -> ActorHostConfig {
    ActorHostConfig {
        systemd_slice: None,
        source_exclude: vec![],
        source_import: Default::default(),
        command_resources: None,
        exomonad_executable: std::env::current_exe().unwrap(),
        workspace_inputs: None,
        haskell_root: crate::haskell_sources::ensure_exomonad_haskell().unwrap(),
        workspace: root.join("project"),
        run_root: root.join("run"),
        root_binding_path: root.join("root-binding.json"),
        backend: crate::exomonad::HostBackendOptions::Embedded,
        embedded: Some(crate::exomonad::EmbeddedLaunchConfig {
            listen: "127.0.0.1:0".parse().unwrap(),
            public_origin_scheme: crate::exomonad::EmbeddedPublicOriginScheme::Http,
            asset_root: root.join("assets"),
            session_secret_file: root.join("secret"),
            codex_auth_file: root.join("auth.json"),
            context_capacity_tokens: 2_000_000,
            concurrent_jobs: 1,
        }),
        tmux_session: "unused-for-embedded-recovery".into(),
        model: "test-model".into(),
        effort: ReasoningEffort::Low,
        research_policy: exomonad_actor::ResearchPolicy::default(),
        root_launch_mode: InteractiveLaunchMode::Fresh,
        pane_environment: BTreeMap::new(),
        jev: Some(exomonad_actor::unconfigured_jev()),
    }
}

#[test]
#[ignore = "private subprocess entry for the owning production recovery test"]
fn production_recovery_process() {
    let Some(root) = std::env::var_os("TIDEPOOL_RECOVERY_TEST_ROOT") else {
        return;
    };
    let root = PathBuf::from(root);
    let phase = std::env::var("TIDEPOOL_RECOVERY_TEST_PHASE").unwrap();
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .unwrap();
    runtime.block_on(async {
        let config = configuration(&root);
        let lease = HostIncarnationLease::claim(&config.run_root).unwrap();
        let (ready, mut incoming) = mpsc::unbounded_channel();
        let transport: Arc<dyn harness::engine::ResponsesTransport> = Arc::new(RecoveryTransport {
            root:root.clone(), phase:phase.clone(), calls:AtomicUsize::new(0),
        });
        let observed = async {
            loop {
                match incoming.recv().await.unwrap() {
                    ActorHostReadiness::EmbeddedReady {root:actor, address} => {
                        std::fs::write(root.join(format!("{phase}.ready")), serde_json::to_vec(&json!({
                            "actor":actor, "address":address.to_string()
                        })).unwrap()).unwrap();
                    }
                    ActorHostReadiness::CoordinationFailed {error,..} => panic!("production startup failed: {error}"),
                    _ => {},
                }
            }
        };
        tokio::select! {
            result = run_with_test_transport(config, ready, lease, transport) => panic!("production host exited: {result:?}"),
            _ = observed => unreachable!(),
        }
    });
}

struct Process(Child);
impl Drop for Process {
    fn drop(&mut self) {
        if self.0.try_wait().ok().flatten().is_none() {
            let _ = self.0.kill();
        }
        let _ = self.0.wait();
    }
}
fn start(root: &Path, phase: &str) -> Process {
    let output = std::fs::File::create(root.join(format!("{phase}.log"))).unwrap();
    Process(
        Command::new(std::env::current_exe().unwrap())
            .args(["--exact", PROCESS_TEST, "--ignored", "--nocapture"])
            .env("TIDEPOOL_RECOVERY_TEST_ROOT", root)
            .env("TIDEPOOL_RECOVERY_TEST_PHASE", phase)
            .stdout(Stdio::from(output.try_clone().unwrap()))
            .stderr(Stdio::from(output))
            .spawn()
            .unwrap(),
    )
}
async fn ready(root: &Path, phase: &str, process: &mut Process) -> serde_json::Value {
    tokio::time::timeout(Duration::from_secs(360), async {
        loop {
            if let Ok(bytes) = std::fs::read(root.join(format!("{phase}.ready"))) {
                if let Ok(value) = serde_json::from_slice(&bytes) {
                    return value;
                }
            }
            if let Some(status) = process.0.try_wait().unwrap() {
                panic!(
                    "production {phase} exited {status}: {}",
                    std::fs::read_to_string(root.join(format!("{phase}.log"))).unwrap()
                );
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap_or_else(|_| {
        panic!(
            "production {phase} startup timed out: {}",
            std::fs::read_to_string(root.join(format!("{phase}.log"))).unwrap()
        )
    })
}
async fn input(
    address: &str,
    target: &harness::embedding::HostIdentity,
    cookie: Option<&str>,
) -> String {
    let client = reqwest::Client::new();
    let api = format!("http://{address}/api");
    let origin = format!("http://{address}");
    let cookie = match cookie {
        Some(cookie) => cookie.to_owned(),
        None => {
            let response = client
                .post(format!("{api}/session"))
                .header("Origin", &origin)
                .json(&json!({"secret":SECRET}))
                .send()
                .await
                .unwrap();
            assert_eq!(response.status(), reqwest::StatusCode::OK);
            response.headers()[reqwest::header::SET_COOKIE]
                .to_str()
                .unwrap()
                .split(';')
                .next()
                .unwrap()
                .to_owned()
        }
    };
    let command = harness::server::ClientCommand::Host {
        operation_id: harness::embedding::ClientOperationId(uuid::Uuid::new_v4()),
        command: harness::server::HostCommand::Input {
            target: target.clone(),
            text: "continue retained conversation".into(),
        },
    };
    let response = client
        .post(format!("{api}/commands"))
        .header("Origin", origin)
        .header(reqwest::header::COOKIE, &cookie)
        .json(&command)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), reqwest::StatusCode::ACCEPTED);
    cookie
}
async fn wait_calls(root: &Path, phase: &str) {
    tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            if std::fs::read_to_string(root.join(format!("{phase}.calls"))).is_ok() {
                let store =
                    harness::store::Store::open(root.join("run/harness/store.sqlite")).unwrap();
                if let Some(head) = store
                    .agent(&harness::model::AgentPath("/root".into()))
                    .unwrap()
                    .and_then(|agent| agent.head_request)
                {
                    if store.items(&head).unwrap().iter().any(|item| {
                        item.0["role"] == "assistant" && item.0["phase"] == "final_answer"
                    }) {
                        return;
                    }
                }
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
}

fn snapshot_store(root: &Path) -> Vec<(&'static str, Vec<u8>)> {
    ["store.sqlite", "store.sqlite-wal", "store.sqlite-shm"]
        .into_iter()
        .filter_map(|name| {
            std::fs::read(root.join("run/harness").join(name))
                .ok()
                .map(|bytes| (name, bytes))
        })
        .collect()
}

fn restore_store(root: &Path, snapshot: &[(&str, Vec<u8>)]) {
    let directory = root.join("run/harness");
    for name in ["store.sqlite", "store.sqlite-wal", "store.sqlite-shm"] {
        match std::fs::remove_file(directory.join(name)) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => panic!("cannot restore owned Store fixture: {error}"),
        }
    }
    for (name, bytes) in snapshot {
        std::fs::write(directory.join(name), bytes).unwrap();
    }
}

async fn refused(root: &Path, phase: &str, process: &mut Process, expected: &str) {
    tokio::time::timeout(Duration::from_secs(360), async {
        loop {
            if let Some(status) = process.0.try_wait().unwrap() {
                let log = std::fs::read_to_string(root.join(format!("{phase}.log"))).unwrap();
                assert!(
                    !status.success(),
                    "invalid recovery unexpectedly exited successfully: {log}"
                );
                assert!(log.contains(expected), "wrong recovery refusal: {log}");
                assert!(!root.join(format!("{phase}.ready")).exists());
                assert!(!root.join(format!("{phase}.calls")).exists());
                return;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("invalid recovery did not refuse before provider readiness");
}

fn fixture() -> PathBuf {
    let artifacts =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target/recovery-fixtures");
    std::fs::create_dir_all(&artifacts).unwrap();
    let files = tempfile::Builder::new()
        .prefix("production-recovery-")
        .tempdir_in(artifacts)
        .unwrap();
    let retained_root = files.keep().canonicalize().unwrap();
    let root = retained_root.as_path();
    eprintln!("production recovery artifacts: {}", root.display());
    let repository = exomonad_worktree::testing::TestRepo::init().unwrap();
    repository
        .writer()
        .commit_file("README.md", "source\n", "seed")
        .unwrap();
    exomonad_worktree::GitCli::new()
        .try_run(
            repository.path(),
            &[
                "clone",
                "--quiet",
                repository.path().to_str().unwrap(),
                root.join("project").to_str().unwrap(),
            ],
        )
        .unwrap();
    std::fs::create_dir(root.join("assets")).unwrap();
    std::fs::write(root.join("assets/index.html"), "<!doctype html>").unwrap();
    std::fs::write(root.join("secret"), SECRET).unwrap();
    std::fs::write(root.join("auth.json"), "{}").unwrap();
    retained_root
}

#[tokio::test]
async fn production_startup_and_cold_successor_preserve_bound_conversation_without_replay() {
    tidepool_testing::eval_harness::require_extract();
    let retained_root = fixture();
    let root = retained_root.as_path();
    let mut first = start(root, "first");
    let first_pid = first.0.id();
    let first_ready = ready(root, "first", &mut first).await;
    let old_actor: ActorRef = serde_json::from_value(first_ready["actor"].clone()).unwrap();
    let old = embedded_recovery::host_identity(&root.join("run"), "/root", old_actor);
    let cookie = input(first_ready["address"].as_str().unwrap(), &old, None).await;
    wait_calls(root, "first").await;
    first.0.kill().unwrap();
    first.0.wait().unwrap();
    let old_calls = std::fs::read_to_string(root.join("first.calls")).unwrap();
    assert_eq!(
        old_calls, "1",
        "first input must run exactly one provider turn"
    );
    let predecessor_store = snapshot_store(root);
    let records = exomonad_actor::ActorRecoveryJournal::read_observed(
        &root.join("run/actor-lifecycle.v2.jsonl"),
    )
    .unwrap();
    assert_eq!(
        latest_durable_root_application(&records)
            .unwrap()
            .unwrap()
            .application
            .as_ref()
            .unwrap()
            .conversation,
        Some(embedded_recovery::conversation(&old))
    );
    let mut second = start(root, "second");
    assert_ne!(
        first_pid,
        second.0.id(),
        "cold recovery requires a new process"
    );
    let second_ready = ready(root, "second", &mut second).await;
    let new_actor: ActorRef = serde_json::from_value(second_ready["actor"].clone()).unwrap();
    assert_eq!(old_actor.id, new_actor.id);
    assert_eq!(new_actor.incarnation.0, old_actor.incarnation.0 + 1);
    let new = embedded_recovery::host_identity(&root.join("run"), "/root", new_actor);
    let store = harness::store::Store::open(root.join("run/harness/store.sqlite")).unwrap();
    assert!(store.embedded_binding_matches(&new).unwrap());
    assert!(!store.embedded_binding_matches(&old).unwrap());
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(
        !root.join("second.calls").exists(),
        "cold startup replayed prior input"
    );
    assert_eq!(
        std::fs::read_to_string(root.join("first.calls")).unwrap(),
        old_calls
    );
    input(
        second_ready["address"].as_str().unwrap(),
        &new,
        Some(&cookie),
    )
    .await;
    wait_calls(root, "second").await;
    assert_eq!(
        std::fs::read_to_string(root.join("second.calls")).unwrap(),
        "1",
        "successor input must run exactly one provider turn",
    );
    second.0.kill().unwrap();
    second.0.wait().unwrap();
    let records = exomonad_actor::ActorRecoveryJournal::read_observed(
        &root.join("run/actor-lifecycle.v2.jsonl"),
    )
    .unwrap();
    assert_eq!(
        latest_durable_root_application(&records)
            .unwrap()
            .unwrap()
            .application
            .as_ref()
            .unwrap()
            .conversation,
        Some(embedded_recovery::conversation(&new))
    );
    drop(store);
    let successor_store = snapshot_store(root);
    let journal_path = root.join("run/actor-lifecycle.v2.jsonl");
    let journal = std::fs::read_to_string(&journal_path).unwrap();
    let lines: Vec<_> = journal.split_inclusive('\n').collect();
    let bound = lines
        .iter()
        .position(|line| {
            let row: serde_json::Value = serde_json::from_str(line).unwrap();
            row["event"] == "application_bound"
                && row["actor"] == serde_json::to_value(new_actor).unwrap()
        })
        .unwrap();
    let prepared_prefix = lines[..bound].concat();
    assert!(prepared_prefix.contains("application_prepared"));
    // These are actual journal prefixes and actual Store snapshots from the
    // stopped production processes. No descriptor or authority is reconstructed.
    std::fs::write(&journal_path, &prepared_prefix).unwrap();
    restore_store(root, &predecessor_store);
    let manifest = std::fs::read(root.join("run/root-declarations.json")).unwrap();
    let mut conflicting = start(root, "manifest-store-gap");
    refused(
        root,
        "manifest-store-gap",
        &mut conflicting,
        "latest prepared root has no exact successor Store binding",
    )
    .await;
    assert_eq!(
        std::fs::read_to_string(&journal_path).unwrap(),
        prepared_prefix
    );
    assert_eq!(
        std::fs::read(root.join("run/root-declarations.json")).unwrap(),
        manifest
    );
    drop(conflicting);
    // Store CAS was durable, but ApplicationBound had not yet been appended.
    // Startup may finish only that row, then admit its own fresh successor.
    restore_store(root, &successor_store);
    let mut reconciled = start(root, "bound-gap");
    assert_ne!(reconciled.0.id(), first_pid);
    assert_ne!(reconciled.0.id(), second.0.id());
    let recovered_ready = ready(root, "bound-gap", &mut reconciled).await;
    let recovered_actor: ActorRef =
        serde_json::from_value(recovered_ready["actor"].clone()).unwrap();
    assert_eq!(recovered_actor.id, new_actor.id);
    assert_eq!(recovered_actor.incarnation.0, new_actor.incarnation.0 + 1);
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(
        !root.join("bound-gap.calls").exists(),
        "reconciliation replayed a prior input"
    );
    let records = exomonad_actor::ActorRecoveryJournal::read_observed(&journal_path).unwrap();
    assert_eq!(
        records
            .iter()
            .find(|record| record.admission.actor == new_actor)
            .unwrap()
            .application
            .as_ref()
            .unwrap()
            .conversation,
        Some(embedded_recovery::conversation(&new))
    );
    let recovered_identity =
        embedded_recovery::host_identity(&root.join("run"), "/root", recovered_actor);
    assert_eq!(
        latest_durable_root_application(&records)
            .unwrap()
            .unwrap()
            .application
            .as_ref()
            .unwrap()
            .conversation,
        Some(embedded_recovery::conversation(&recovered_identity))
    );
    let store = harness::store::Store::open(root.join("run/harness/store.sqlite")).unwrap();
    assert!(store.embedded_binding_matches(&recovered_identity).unwrap());
    assert!(!store.embedded_binding_matches(&new).unwrap());
    reconciled.0.kill().unwrap();
    reconciled.0.wait().unwrap();
    drop(store);
    std::fs::remove_dir_all(retained_root).unwrap();
}

async fn cell_settled(root: &Path, phase: &str, process: &mut Process) {
    tokio::time::timeout(Duration::from_secs(360), async {
        loop {
            if root.join(format!("{phase}.settled")).exists() {
                return;
            }
            if let Some(status) = process.0.try_wait().unwrap() {
                panic!(
                    "production cell process {status}: {}",
                    std::fs::read_to_string(root.join(format!("{phase}.log"))).unwrap()
                );
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("resident declaration/expression did not settle");
}

#[tokio::test]
async fn production_cold_successor_executes_retained_original_declaration_in_fresh_heap() {
    tidepool_testing::eval_harness::require_extract();
    let retained_root = fixture();
    let root = retained_root.as_path();
    let mut original = start(root, "publish-original");
    let original_pid = original.0.id();
    let initial = ready(root, "publish-original", &mut original).await;
    let initial_actor: ActorRef = serde_json::from_value(initial["actor"].clone()).unwrap();
    let initial_identity =
        embedded_recovery::host_identity(&root.join("run"), "/root", initial_actor);
    let cookie = input(
        initial["address"].as_str().unwrap(),
        &initial_identity,
        None,
    )
    .await;
    cell_settled(root, "publish-original", &mut original).await;
    wait_calls(root, "publish-original").await;
    original.0.kill().unwrap();
    original.0.wait().unwrap();
    let original_calls = std::fs::read_to_string(root.join("publish-original.calls")).unwrap();
    let manifest: serde_json::Value =
        serde_json::from_slice(&std::fs::read(root.join("run/root-declarations.json")).unwrap())
            .unwrap();
    let authored = manifest["nodes"]
        .as_array()
        .unwrap()
        .iter()
        .find(|node| {
            node["kind"] == "authored"
                && node["exports"].as_array().is_some_and(|exports| {
                    exports.iter().any(|export| {
                        export["kind"] == "value"
                            && export["identity"]["occurrence"] == "coldAnswer"
                    })
                })
        })
        .expect("coldAnswer original authored declaration was not retained");
    let exports = authored["exports"].as_array().unwrap();
    let answer = exports
        .iter()
        .find(|export| export["identity"]["occurrence"] == "coldAnswer")
        .unwrap();
    let unit = answer["identity"]["unit"].as_str().unwrap();
    let module = answer["identity"]["module"].as_str().unwrap();
    assert!(!unit.is_empty());
    let authored_generation = tidepool_repr::Generation(authored["id"].as_u64().unwrap());
    assert_eq!(
        module,
        tidepool_repr::SessionModule::lib(authored_generation).module_name(),
        "authored owner does not match its original declaration generation"
    );
    assert!(
        exports.iter().any(|export| {
            export["kind"] == "type"
                && export["identity"]["occurrence"] == "RecoveryBox"
                && export["identity"]["unit"] == unit
                && export["identity"]["module"] == module
                && export["children"].as_array().is_some_and(|children| {
                    children.iter().any(|child| {
                        child["occurrence"] == "RecoveryBox"
                            && child["unit"] == unit
                            && child["module"] == module
                    })
                })
        }),
        "original RecoveryBox type and constructor identities were not retained"
    );
    for class in ["Eq", "Show"] {
        assert!(
            authored["instances"]["classes"]
                .as_array()
                .unwrap()
                .iter()
                .any(|instance| {
                    instance["selected"] == true
                        && instance["class"]["occurrence"] == class
                        && instance["dfun"]["unit"] == unit
                        && instance["dfun"]["module"] == module
                }),
            "original {class} instance was not retained"
        );
    }
    let original_artifact = manifest["artifacts"]
        .as_array()
        .unwrap()
        .iter()
        .find(|artifact| {
            artifact["kind"] == "home"
                && artifact["reference"]["unit"] == unit
                && artifact["reference"]["module"] == module
        })
        .expect("authored original module product/interface was not retained");
    let original_reference: tidepool_toolchain::recovery_artifacts::RecoveryArtifactRef =
        serde_json::from_value(original_artifact["reference"].clone()).unwrap();
    tidepool_toolchain::recovery_artifacts::verify_materialized_ref(
        &root.join("run"),
        &original_reference,
    )
    .expect("original interface/product bytes do not match their certified identity");
    let mut recovered = start(root, "execute-original");
    assert_ne!(recovered.0.id(), original_pid);
    let current = ready(root, "execute-original", &mut recovered).await;
    let current_actor: ActorRef = serde_json::from_value(current["actor"].clone()).unwrap();
    assert_eq!(current_actor.id, initial_actor.id);
    assert_eq!(current_actor.incarnation.0, initial_actor.incarnation.0 + 1);
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(
        !root.join("execute-original.calls").exists(),
        "recovery replayed old source/effects"
    );
    let current_identity =
        embedded_recovery::host_identity(&root.join("run"), "/root", current_actor);
    input(
        current["address"].as_str().unwrap(),
        &current_identity,
        Some(&cookie),
    )
    .await;
    cell_settled(root, "execute-original", &mut recovered).await;
    wait_calls(root, "execute-original").await;
    recovered.0.kill().unwrap();
    recovered.0.wait().unwrap();
    let recovered_manifest: serde_json::Value =
        serde_json::from_slice(&std::fs::read(root.join("run/root-declarations.json")).unwrap())
            .unwrap();
    assert!(
        recovered_manifest["artifacts"]
            .as_array()
            .unwrap()
            .contains(original_artifact),
        "cold successor replaced the original module/product identity"
    );
    tidepool_toolchain::recovery_artifacts::verify_materialized_ref(
        &root.join("run"),
        &original_reference,
    )
    .expect("cold successor changed the original retained product/interface bytes");
    assert_eq!(
        std::fs::read_to_string(root.join("publish-original.calls")).unwrap(),
        original_calls
    );
    std::fs::remove_dir_all(retained_root).unwrap();
}
