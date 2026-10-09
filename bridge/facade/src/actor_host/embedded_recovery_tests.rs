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
                    tidepool_testing::fixture_source(
                        "bridge/facade/src/actor_host/fixtures/embedded_cold_declaration.hs",
                    )
                } else {
                    "case coldAnswer of RecoveryBox value -> value".to_owned()
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
    let startup_driver = root.join("startup-driver");
    ActorHostConfig {
        systemd_slice: None,
        source_exclude: vec![],
        source_import: Default::default(),
        command_resources: None,
        exomonad_executable: std::env::current_exe().unwrap(),
        workspace_inputs: None,
        haskell_root: if startup_driver.exists() {
            startup_driver
        } else {
            crate::haskell_sources::ensure_exomonad_haskell().unwrap()
        },
        workspace: root.join("project"),
        run_directory: tidepool_atomic_write::DirectoryAnchor::open_existing(root)
            .unwrap()
            .child("run")
            .unwrap(),
        root_binding_path: root.join("root-binding.json"),

        embedded: Some(crate::exomonad::EmbeddedLaunchConfig {
            listen: "127.0.0.1:0".parse().unwrap(),
            public_origin_scheme: crate::exomonad::EmbeddedPublicOriginScheme::Http,
            public_origin: None,
            asset_root: root.join("assets"),
            browser_auth: crate::exomonad::EmbeddedBrowserAuth::Secret,
            session_secret_file: Some(root.join("secret")),
            provider: crate::exomonad::EmbeddedModelProvider::Codex,
            credential_file: root.join("auth.json"),
            context_capacity_tokens: 2_000_000,
            concurrent_jobs: 1,
        }),
        tmux_session: "unused-for-embedded-recovery".into(),
        model: "test-model".into(),
        effort: ForkEffort::Low,

        pane_environment: BTreeMap::new(),
        jev: Some(exomonad_actor::unconfigured_jev()),
    }
}

#[test]
#[ignore = "private subprocess entry for the owning production recovery test"]
fn production_recovery_process() {
    let root = PathBuf::from(
        std::env::var_os("TIDEPOOL_RECOVERY_TEST_ROOT")
            .expect("production recovery parent must supply TIDEPOOL_RECOVERY_TEST_ROOT"),
    );
    let phase = std::env::var("TIDEPOOL_RECOVERY_TEST_PHASE")
        .expect("production recovery parent must supply TIDEPOOL_RECOVERY_TEST_PHASE");
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .unwrap();
    runtime.block_on(async {
        let config = configuration(&root);
        let lease = HostIncarnationLease::claim(&config.run_directory).unwrap();
        let (ready, mut incoming) = mpsc::unbounded_channel();
        let transport: Arc<dyn harness::engine::ResponsesTransport> = Arc::new(RecoveryTransport {
            root:root.clone(), phase:phase.clone(), calls:AtomicUsize::new(0),
        });
        let observed = async {
            loop {
                match incoming.recv().await.unwrap() {
                    ActorHostReadiness::EmbeddedReady {root:actor, address} => {
                        std::fs::write(root.join(format!("{phase}.ready")), serde_json::to_vec(&json!({
                            "actor":actor, "address":address.to_string(), "pid":std::process::id()
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

#[test]
fn production_recovery_helper_refuses_missing_required_settings() {
    for missing in [
        "TIDEPOOL_RECOVERY_TEST_ROOT",
        "TIDEPOOL_RECOVERY_TEST_PHASE",
    ] {
        let root = tempfile::tempdir().unwrap();
        let output = Command::new(std::env::current_exe().unwrap())
            .args(["--exact", PROCESS_TEST, "--ignored", "--nocapture"])
            .env("TIDEPOOL_RECOVERY_TEST_ROOT", root.path())
            .env("TIDEPOOL_RECOVERY_TEST_PHASE", "missing-settings-control")
            .env_remove(missing)
            .output()
            .unwrap();
        assert!(
            !output.status.success(),
            "helper accepted missing {missing}"
        );
        assert!(
            String::from_utf8_lossy(&output.stderr)
                .contains(&format!("production recovery parent must supply {missing}")),
            "helper did not reach the missing-setting refusal: {output:?}"
        );
        assert_eq!(
            std::fs::read_dir(root.path()).unwrap().count(),
            0,
            "missing settings must refuse before host startup"
        );
    }
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

fn finish_fixture(root: PathBuf) {
    if std::env::var("TIDEPOOL_KEEP_TEST_LOGS").ok().as_deref() == Some("1") {
        eprintln!("retained production recovery evidence: {}", root.display());
    } else {
        std::fs::remove_dir_all(root).unwrap();
    }
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
    input(first_ready["address"].as_str().unwrap(), &old, None).await;
    wait_calls(root, "first").await;
    first.0.kill().unwrap();
    first.0.wait().unwrap();
    let old_calls = std::fs::read_to_string(root.join("first.calls")).unwrap();
    assert_eq!(
        old_calls, "1",
        "first input must run exactly one provider turn"
    );
    let retained_head = harness::store::Store::open(root.join("run/harness/store.sqlite"))
        .unwrap()
        .agent(&harness::model::AgentPath("/root".into()))
        .unwrap()
        .unwrap()
        .head_request
        .unwrap();
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
    assert_eq!(
        store
            .agent(&harness::model::AgentPath("/root".into()))
            .unwrap()
            .unwrap()
            .head_request,
        Some(retained_head),
        "cold binding transfer must preserve the exact conversation head"
    );
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(
        !root.join("second.calls").exists(),
        "cold startup replayed prior input"
    );
    assert_eq!(
        std::fs::read_to_string(root.join("first.calls")).unwrap(),
        old_calls
    );
    input(second_ready["address"].as_str().unwrap(), &new, None).await;
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
    finish_fixture(retained_root);
}

pub(super) fn startup_checkpoint(checkpoint: &str) {
    if std::env::var("TIDEPOOL_STARTUP_CRASH_AT").ok().as_deref() == Some(checkpoint) {
        let root = PathBuf::from(std::env::var_os("TIDEPOOL_RECOVERY_TEST_ROOT").unwrap());
        let phase = std::env::var("TIDEPOOL_RECOVERY_TEST_PHASE").unwrap();
        std::fs::write(root.join(format!("{phase}.checkpoint")), checkpoint).unwrap();
        std::process::exit(77);
    }
}

#[tokio::test]
async fn production_authored_root_failure_is_not_evaluated_before_durable_binding() {
    tidepool_testing::eval_harness::require_extract();
    let retained_root = fixture();
    let root = retained_root.as_path();
    fn copy_actors(source: &Path, target: &Path) {
        std::fs::create_dir_all(target).unwrap();
        for entry in std::fs::read_dir(source).unwrap() {
            let entry = entry.unwrap();
            let output = target.join(entry.file_name());
            if entry.file_type().unwrap().is_dir() {
                copy_actors(&entry.path(), &output);
            } else {
                std::fs::copy(entry.path(), output).unwrap();
            }
        }
    }
    copy_actors(
        &crate::haskell_sources::ensure_exomonad_haskell().unwrap(),
        &root.join("startup-driver"),
    );
    std::fs::write(
        root.join("startup-driver/Tidepool/Actors/Internal/ExomonadDriver.hs"),
        tidepool_testing::fixture_source(
            "bridge/facade/src/actor_host/fixtures/startup_authored_failure.hs",
        ),
    )
    .unwrap();
    let mut process = start_crashing(root, "authored-failure-before-bound", "bound");
    crashed(root, "authored-failure-before-bound", &mut process).await;
    let records = exomonad_actor::ActorRecoveryJournal::read_observed(
        &root.join("run/actor-lifecycle.v2.jsonl"),
    )
    .unwrap();
    let head = latest_durable_root_application(&records).unwrap().unwrap();
    assert!(head.application.as_ref().unwrap().conversation.is_some());
    assert!(
        !std::fs::read_to_string(root.join("authored-failure-before-bound.log"))
            .unwrap()
            .contains("startup authored failure sentinel")
    );
    finish_fixture(retained_root);
}

fn start_crashing(root: &Path, phase: &str, checkpoint: &str) -> Process {
    let output = std::fs::File::create(root.join(format!("{phase}.log"))).unwrap();
    Process(
        Command::new(std::env::current_exe().unwrap())
            .args(["--exact", PROCESS_TEST, "--ignored", "--nocapture"])
            .env("TIDEPOOL_RECOVERY_TEST_ROOT", root)
            .env("TIDEPOOL_RECOVERY_TEST_PHASE", phase)
            .env("TIDEPOOL_STARTUP_CRASH_AT", checkpoint)
            .stdout(Stdio::from(output.try_clone().unwrap()))
            .stderr(Stdio::from(output))
            .spawn()
            .unwrap(),
    )
}

async fn crashed(root: &Path, phase: &str, process: &mut Process) {
    tokio::time::timeout(Duration::from_secs(360), async {
        loop {
            if let Some(status) = process.0.try_wait().unwrap() {
                assert_eq!(
                    status.code(),
                    Some(77),
                    "wrong startup crash: {}",
                    std::fs::read_to_string(root.join(format!("{phase}.log"))).unwrap()
                );
                assert!(root.join(format!("{phase}.checkpoint")).exists());
                assert!(!root.join(format!("{phase}.ready")).exists());
                assert!(!root.join(format!("{phase}.calls")).exists());
                return;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("startup crash did not reach its transition");
}

#[test]
fn startup_head_follows_predecessors_across_fresh_ids_and_lower_incarnations() {
    fn record(
        actor: ActorRef,
        predecessor: Option<ActorRef>,
    ) -> exomonad_actor::DurableActorRecord {
        let conversation = embedded_recovery::host_identity(Path::new("run"), "/root", actor);
        let intent = exomonad_actor::RootStartupIntent {
            bootstrap_identity: "bootstrap".into(),
            predecessor,
            manifest_predecessor: None,
            manifest: None,
            store_predecessor: None,
            binding_path: "binding.json".into(),
            accepted_source: None,
            conversation: embedded_recovery::conversation(&conversation),
        };
        exomonad_actor::DurableActorRecord {
            admission: serde_json::from_value(json!({
                "actor": actor, "label": "root", "creator": null,
                "supervisor_parent": null, "context_parent": null,
                "actor_path": "root", "role": "root", "model": null,
                "effort": null, "instructions": null, "launch_worktrees": [], "source_layer": [],
            }))
            .unwrap(),
            startup: Some(intent.clone()),
            application: Some(exomonad_actor::DurableActorApplication {
                binding_path: intent.binding_path.clone(),
                accepted_source: None,
                intended_conversation: Some(intent.conversation),
                conversation: None,
            }),
            terminal: None,
        }
    }
    let old = ActorRef {
        id: exomonad_actor::ActorId(41),
        incarnation: exomonad_actor::Incarnation(17),
    };
    let head = ActorRef {
        id: exomonad_actor::ActorId(9),
        incarnation: exomonad_actor::Incarnation(3),
    };
    let mut records = vec![record(old, None), record(head, Some(old))];
    assert_eq!(
        latest_durable_root_application(&records)
            .unwrap()
            .unwrap()
            .admission
            .actor,
        head
    );
    assert_eq!(
        root_startup_chain(&records)
            .unwrap()
            .iter()
            .map(|record| record.admission.actor)
            .collect::<Vec<_>>(),
        vec![head, old]
    );
    records[1].application.as_mut().unwrap().conversation = records[1]
        .application
        .as_ref()
        .unwrap()
        .intended_conversation
        .clone();
    assert_eq!(root_startup_chain(&records).unwrap().len(), 1);
    records.push(record(ActorRef::first(exomonad_actor::ActorId(55)), None));
    assert!(latest_durable_root_application(&records).is_err());
}

pub(super) fn uncertain_store_reader() -> Option<rusqlite::Connection> {
    if std::env::var("TIDEPOOL_RECOVERY_TEST_PHASE")
        .ok()
        .as_deref()
        != Some("uncertain-store")
    {
        return None;
    }
    let root = PathBuf::from(std::env::var_os("TIDEPOOL_RECOVERY_TEST_ROOT").unwrap());
    let connection = rusqlite::Connection::open(root.join("run/harness/store.sqlite")).unwrap();
    connection
        .execute_batch("BEGIN; SELECT COUNT(*) FROM embedded_bindings;")
        .unwrap();
    Some(connection)
}

#[tokio::test]
async fn production_uncertain_store_write_never_activates_visible_binding() {
    tidepool_testing::eval_harness::require_extract();
    let retained_root = fixture();
    let root = retained_root.as_path();
    let mut first = start_crashing(root, "before-uncertainty", "store");
    crashed(root, "before-uncertainty", &mut first).await;
    let mut uncertain = start(root, "uncertain-store");
    refused(
        root,
        "uncertain-store",
        &mut uncertain,
        "binding committed but WAL durability confirmation is unavailable",
    )
    .await;
    let records = exomonad_actor::ActorRecoveryJournal::read_observed(
        &root.join("run/actor-lifecycle.v2.jsonl"),
    )
    .unwrap();
    let head = latest_durable_root_application(&records).unwrap().unwrap();
    assert!(head.application.as_ref().unwrap().conversation.is_none());
    let visible =
        embedded_recovery::host_identity(&root.join("run"), "/root", head.admission.actor);
    let store = harness::store::Store::open(root.join("run/harness/store.sqlite")).unwrap();
    assert!(
        store.embedded_binding_matches(&visible).unwrap(),
        "error after commit must leave visible successor evidence"
    );
    drop(store);
    let mut successor = start(root, "after-uncertainty");
    let observed = ready(root, "after-uncertainty", &mut successor).await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(!root.join("after-uncertainty.calls").exists());
    let actor: ActorRef = serde_json::from_value(observed["actor"].clone()).unwrap();
    let identity = embedded_recovery::host_identity(&root.join("run"), "/root", actor);
    input(observed["address"].as_str().unwrap(), &identity, None).await;
    wait_calls(root, "after-uncertainty").await;
    assert_eq!(
        std::fs::read_to_string(root.join("after-uncertainty.calls")).unwrap(),
        "1"
    );
    successor.0.kill().unwrap();
    successor.0.wait().unwrap();
    finish_fixture(retained_root);
}

#[tokio::test]
async fn production_old_startup_journal_is_refused_without_rewriting_evidence() {
    for version in [3, 4] {
        let retained_root = fixture();
        let root = retained_root.as_path();
        std::fs::create_dir_all(root.join("run")).unwrap();
        let journal = root.join("run/actor-lifecycle.v2.jsonl");
        let bytes = format!("{{\"version\":{version},\"sequence\":1,\"event\":\"created\"}}\n");
        std::fs::write(&journal, &bytes).unwrap();
        let mut process = start(root, "old-startup-journal");
        refused(
            root,
            "old-startup-journal",
            &mut process,
            "unsupported actor journal version",
        )
        .await;
        assert_eq!(std::fs::read(&journal).unwrap(), bytes.as_bytes());
        assert!(!root.join("run/root-declarations.json").exists());
        finish_fixture(retained_root);
    }
}

#[tokio::test]
async fn production_missing_manifest_refuses_bound_root_without_rewriting_journal() {
    tidepool_testing::eval_harness::require_extract();
    let retained_root = fixture();
    let root = retained_root.as_path();
    let mut first = start_crashing(root, "retained-bound-root", "bound");
    crashed(root, "retained-bound-root", &mut first).await;
    let journal = root.join("run/actor-lifecycle.v2.jsonl");
    let journal_bytes = std::fs::read(&journal).unwrap();
    let manifest = root.join("run/root-declarations.json");
    let retained_manifest = root.join("run/retained-root-declarations.json");
    let manifest_bytes = std::fs::read(&manifest).unwrap();
    std::fs::rename(&manifest, &retained_manifest).unwrap();
    let mut refused_root = start(root, "missing-manifest");
    refused(
        root,
        "missing-manifest",
        &mut refused_root,
        "root startup lost its retained public manifest",
    )
    .await;
    assert_eq!(std::fs::read(&journal).unwrap(), journal_bytes);
    assert_eq!(std::fs::read(&retained_manifest).unwrap(), manifest_bytes);
    assert!(!manifest.exists());
    finish_fixture(retained_root);
}

#[tokio::test]
async fn production_repeated_startup_crashes_roll_split_owners_forward_without_replay() {
    tidepool_testing::eval_harness::require_extract();
    let retained_root = fixture();
    let root = retained_root.as_path();
    for (index, checkpoint) in [
        "admitted", "manifest", "manifest", "store", "manifest", "manifest", "store", "bound",
        "released",
    ]
    .into_iter()
    .enumerate()
    {
        let phase = format!("crash-{index}-{checkpoint}");
        let mut process = start_crashing(root, &phase, checkpoint);
        crashed(root, &phase, &mut process).await;
        let records = exomonad_actor::ActorRecoveryJournal::read_observed(
            &root.join("run/actor-lifecycle.v2.jsonl"),
        )
        .unwrap();
        let head = latest_durable_root_application(&records).unwrap().unwrap();
        assert!(head.startup.is_some());
        assert_eq!(
            records
                .iter()
                .filter(|record| record.startup.is_some())
                .count(),
            index + 1
        );
        assert_eq!(
            head.application.as_ref().unwrap().conversation.is_some(),
            matches!(checkpoint, "bound" | "released")
        );
        if index == 5 {
            let intent = head.startup.as_ref().unwrap();
            assert_ne!(
                intent.store_predecessor,
                Some(embedded_recovery::conversation(
                    &embedded_recovery::host_identity(
                        &root.join("run"),
                        "/root",
                        intent.manifest_predecessor.unwrap()
                    )
                )),
                "split owners must be independently observed"
            );
        }
    }
    let mut final_process = start(root, "finally-ready");
    let final_ready = ready(root, "finally-ready", &mut final_process).await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(!root.join("finally-ready.calls").exists());
    let actor: ActorRef = serde_json::from_value(final_ready["actor"].clone()).unwrap();
    let identity = embedded_recovery::host_identity(&root.join("run"), "/root", actor);
    input(final_ready["address"].as_str().unwrap(), &identity, None).await;
    wait_calls(root, "finally-ready").await;
    assert_eq!(
        std::fs::read_to_string(root.join("finally-ready.calls")).unwrap(),
        "1"
    );
    final_process.0.kill().unwrap();
    final_process.0.wait().unwrap();
    finish_fixture(retained_root);
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
    input(
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
        None,
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
    finish_fixture(retained_root);
}
