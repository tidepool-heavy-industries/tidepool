use super::*;
use harness::transport::{ResponsesTurn, TransportError, Usage};
use serde_json::json;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};

struct RecoveryPersonaJev {
    root: PathBuf,
    phase: String,
    calls: AtomicUsize,
}

impl exomonad_actor::JevBackend for RecoveryPersonaJev {
    fn ask(
        &self,
        _packet: String,
    ) -> futures_util::future::BoxFuture<'_, Result<String, exomonad_actor::JevCallFailure>> {
        Box::pin(async move {
            let count = self.calls.fetch_add(1, Ordering::SeqCst) + 1;
            std::fs::write(
                self.root.join(format!("{}.jev-calls", self.phase)),
                count.to_string(),
            )
            .unwrap();
            Err(exomonad_actor::JevCallFailure::JevTransport(
                "arrival and refresh must not ask Jev".into(),
            ))
        })
    }
}

const PROCESS_TEST: &str = "actor_host::embedded_recovery_tests::production_recovery_process";
const SECRET: &str = "offline-production-recovery-secret-is-long-enough";

struct RecoveryTransport {
    root: PathBuf,
    phase: String,
    calls: AtomicUsize,
}
#[async_trait::async_trait]
impl harness::engine::ResponsesTransport for RecoveryTransport {
    fn response_contract(
        &self,
    ) -> Result<
        (
            harness::transport::ResponsesRoute,
            harness::transport::ResponsesProtocol,
        ),
        harness::transport::TransportError,
    > {
        Ok((
            harness::transport::ResponsesRoute::ChatGptPlan,
            harness::transport::ResponsesProtocol::Standard,
        ))
    }

    async fn create(
        &self,
        request: harness::transport::PreparedProviderRequest,
    ) -> Result<ResponsesTurn, TransportError> {
        let request = request.request().clone();
        let count = self.calls.fetch_add(1, Ordering::SeqCst) + 1;
        std::fs::write(
            self.root.join(format!("{}.calls", self.phase)),
            count.to_string(),
        )
        .unwrap();
        if matches!(
            self.phase.as_str(),
            "persona-original" | "persona-recovered"
        ) {
            std::fs::write(
                self.root.join(format!("{}.request.json", self.phase)),
                serde_json::to_vec(&request).unwrap(),
            )
            .unwrap();
        }
        if self.phase.starts_with("coherent-") {
            std::fs::write(
                self.root
                    .join(format!("{}.request-{count}.json", self.phase)),
                serde_json::to_vec(&request).unwrap(),
            )
            .unwrap();
            let sources: &[(&str, &str)] = if self.phase == "coherent-original" {
                &[
                    (
                        "publish-b",
                        include_str!("fixtures/coherent_recovery_publish_b.hs"),
                    ),
                    (
                        "bind-b",
                        include_str!("fixtures/coherent_recovery_bind_b.hs"),
                    ),
                ]
            } else {
                &[(
                    "observe-b",
                    include_str!("fixtures/coherent_recovery_observe_b.hs"),
                )]
            };
            assert!(count <= sources.len() + 1);
            if count > 1 {
                let previous = sources[count - 2].0;
                let item = request
                    .input
                    .iter()
                    .find(|item| {
                        item.0["type"] == "custom_tool_call_output" && item.0["call_id"] == previous
                    })
                    .unwrap();
                let output: serde_json::Value =
                    serde_json::from_str(item.0["output"].as_str().unwrap()).unwrap();
                assert_eq!(output["status"], "committed", "{output}");
                let ready: serde_json::Value = serde_json::from_slice(
                    &std::fs::read(self.root.join(format!("{}.ready", self.phase))).unwrap(),
                )
                .unwrap();
                let actor: ActorRef = serde_json::from_value(ready["actor"].clone()).unwrap();
                let run = self.root.join("run");
                let store = harness::store::Store::open(run.join("harness/store.sqlite")).unwrap();
                assert_eq!(
                    super::test_campaign::retained_committed_display_text(
                        &store,
                        &harness::store::actor_output::ActorOutputOrigin {
                            run: super::runtime_namespace(&run),
                            native_actor: actor.id.0,
                            incarnation: actor.incarnation.0,
                        },
                        &output,
                    ),
                    "True",
                    "{output}"
                );
                std::fs::write(
                    self.root.join(format!("{}.{previous}.json", self.phase)),
                    serde_json::to_vec(&output).unwrap(),
                )
                .unwrap();
            }
            let items = if count <= sources.len() {
                let (call, source) = sources[count - 1];
                vec![super::test_campaign::scripted_tool_call(
                    call,
                    "haskell",
                    harness::item::ToolInput::Custom(source.into()),
                    harness::item::ToolExecution::Synchronous,
                )]
            } else {
                std::fs::write(self.root.join(format!("{}.settled", self.phase)), "settled")
                    .unwrap();
                vec![harness::item::Item(
                    json!({"type":"message","role":"assistant","phase":"final_answer","content":[{"type":"output_text","text":"retained coherent history"}]}),
                )]
            };
            return Ok(ResponsesTurn {
                response_id: format!("{}-{count}", self.phase),
                items,
                usage: Usage::default(),
            });
        }
        if self.phase == "persona-original" {
            let source = match count {
                1 => Some((
                    "persona-refresh",
                    include_str!("fixtures/startup_persona_refresh.hs"),
                )),
                2 | 3 => {
                    let call = if count == 2 {
                        "persona-refresh"
                    } else {
                        "persona-native-refusal"
                    };
                    let item = request
                        .input
                        .iter()
                        .find(|item| {
                            item.0["type"] == "custom_tool_call_output" && item.0["call_id"] == call
                        })
                        .expect("real synchronous persona call settled before next inference");
                    let output: serde_json::Value =
                        serde_json::from_str(item.0["output"].as_str().unwrap()).unwrap();
                    assert!(
                        matches!(output["status"].as_str(), Some("committed" | "completed")),
                        "{output}"
                    );
                    assert!(
                        output["items"]
                            .as_array()
                            .unwrap()
                            .iter()
                            .all(|item| item["status"] == "committed"),
                        "{output}"
                    );
                    assert_eq!(
                        super::test_campaign::committed_display_text(&output),
                        "True"
                    );
                    std::fs::write(
                        self.root.join(format!("{}.{call}.json", self.phase)),
                        serde_json::to_vec(&output).unwrap(),
                    )
                    .unwrap();
                    if count == 2 {
                        Some((
                            "persona-native-refusal",
                            include_str!("fixtures/startup_persona_refuse_native.hs"),
                        ))
                    } else {
                        std::fs::write(
                            self.root.join(format!("{}.settled", self.phase)),
                            serde_json::to_vec(&output).unwrap(),
                        )
                        .unwrap();
                        None
                    }
                }
                _ => panic!("persona fixture exceeded its three-request history"),
            };
            let items = match source {
                Some((call, source)) => vec![super::test_campaign::scripted_tool_call(
                    call,
                    "haskell",
                    harness::item::ToolInput::Custom(source.into()),
                    harness::item::ToolExecution::Synchronous,
                )],
                None => vec![harness::item::Item(
                    json!({"type":"message","role":"assistant","phase":"final_answer","content":[{"type":"output_text","text":"retained persona history"}]}),
                )],
            };
            return Ok(ResponsesTurn {
                response_id: format!("{}-{count}", self.phase),
                items,
                usage: Usage::default(),
            });
        }
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
                    "display (case coldAnswer of RecoveryBox value -> value)".to_owned()
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
                            super::test_campaign::committed_display_text(&output).trim(),
                            "42",
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
            asset_root: super::test_campaign::hosted_browser_assets(),
            browser_auth: crate::exomonad::EmbeddedBrowserAuth::Secret,
            session_secret_file: Some(root.join("secret")),
            provider: crate::exomonad::EmbeddedModelProvider::ChatGptPlan,
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
        let mut config = configuration(&root);
        if matches!(phase.as_str(), "persona-original" | "persona-recovered") {
            if phase == "persona-original" {
                super::startup_persona_acceptance_tests::configure_persona(&mut config, "Persona.Spec.agentSpec", |_, _| {});
            } else {
                config.workspace_inputs = Some(crate::exomonad::workspace::FrozenWorkspace::load(&config.workspace, config.run_directory.path()).unwrap());
            }
            config.jev = Some(Arc::new(RecoveryPersonaJev { root: root.clone(), phase: phase.clone(), calls: AtomicUsize::new(0) }));
        }
        if phase.starts_with("coherent-") {
            if phase == "coherent-original" {
                super::startup_persona_acceptance_tests::configure_persona(&mut config, "Persona.Spec.agentSpec", |workspace, project| {
                    let authored = workspace.join(".exomonad");
                    std::fs::create_dir_all(authored.join("Project")).unwrap();
                    std::fs::create_dir_all(authored.join("Recovery")).unwrap();
                    std::fs::create_dir_all(workspace.join("reload-inputs")).unwrap();
                    for (path, body) in [
                        (".exomonad/Project/Work.hs", include_str!("fixtures/coherent_reload_work_a.hs")),
                        (".exomonad/Recovery/Initializer.hs", include_str!("fixtures/coherent_recovery_initializer.hs")),
                        ("reload-inputs/work-b.txt", include_str!("fixtures/coherent_reload_work_b.hs")),
                        ("reload-inputs/prompt-b.txt", include_str!("fixtures/coherent_reload_prompt_b.md")),
                        ("reload-inputs/extra-b.txt", include_str!("fixtures/coherent_recovery_extra_b.hs")),
                        ("initializer-count.txt", ""),
                    ] { std::fs::write(workspace.join(path), body).unwrap(); }
                    let spec = std::fs::read_to_string(authored.join("Persona/Spec.hs")).unwrap()
                        .replace("import qualified Persona.Orientation as Orientation", "import qualified Recovery.Initializer as Orientation");
                    std::fs::write(authored.join("Persona/Spec.hs"), spec).unwrap();
                    project.haskell.modules.extend(["Project.Work".into(), "Recovery.Initializer".into()]);
                });
            } else {
                config.workspace_inputs = Some(crate::exomonad::workspace::FrozenWorkspace::load(&config.workspace, config.run_directory.path()).unwrap());
            }
            config.jev = Some(Arc::new(RecoveryPersonaJev { root: root.clone(), phase: phase.clone(), calls: AtomicUsize::new(0) }));
        }
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

struct Process(Child, Option<ActorRef>);

fn observed_root(root: &Path) -> Option<exomonad_actor::DurableActorRecord> {
    let journal = root.join("run/actor-lifecycle.v2.jsonl");
    if !journal.try_exists().unwrap() {
        return None;
    }
    let records = exomonad_actor::ActorRecoveryJournal::read_observed(&journal).unwrap();
    latest_durable_root_application(&records).unwrap().cloned()
}

fn persisted_root_owner(
    records: &[exomonad_actor::DurableActorRecord],
    actor: ActorRef,
) -> tidepool_runtime::session::RecoveryPublicOwner {
    // This cold-process assertion has no live ResidentForest handle. Bind the
    // lookup to the exact canonical path retained in that actor's admission.
    let admission = records
        .iter()
        .find(|record| record.admission.actor == actor)
        .map(|record| &record.admission)
        .expect("exact root incarnation has retained admission evidence");
    assert!(admission.creator.is_none());
    assert!(admission.supervisor_parent.is_none());
    assert!(admission.context_parent.is_none());
    let rendered = admission
        .actor_path
        .as_deref()
        .expect("durable root admission retains its actor path");
    let path = tidepool_repr::ActorPath::parse(rendered)
        .expect("durable root admission path is canonical");
    assert_eq!(path.to_string(), rendered);
    tidepool_runtime::session::RecoveryPublicOwner::new(&path, actor.incarnation.0)
        .expect("durable root incarnation issues a recovery owner")
}

impl Drop for Process {
    fn drop(&mut self) {
        if self.0.try_wait().ok().flatten().is_none() {
            let _ = self.0.kill();
        }
        let _ = self.0.wait();
    }
}
fn start(root: &Path, phase: &str) -> Process {
    let previous = observed_root(root).map(|record| record.admission.actor);
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
        previous,
    )
}
async fn ready(root: &Path, phase: &str, process: &mut Process) -> serde_json::Value {
    tokio::time::timeout(Duration::from_secs(360), async {
        loop {
            if let Some(status) = process.0.try_wait().unwrap() {
                panic!(
                    "production {phase} exited {status}: {}",
                    std::fs::read_to_string(root.join(format!("{phase}.log"))).unwrap()
                );
            }
            if let Some(current) = observed_root(root) {
                if Some(current.admission.actor) != process.1 {
                    assert!(
                        current.terminal.is_none(),
                        "production {phase} actor {} retired before readiness: {:?}",
                        current.admission.actor,
                        current.terminal
                    );
                }
            }
            if let Ok(bytes) = std::fs::read(root.join(format!("{phase}.ready"))) {
                if let Ok(value) = serde_json::from_slice::<serde_json::Value>(&bytes) {
                    assert_eq!(value["pid"].as_u64(), Some(u64::from(process.0.id())));
                    let ready_actor: ActorRef =
                        serde_json::from_value(value["actor"].clone()).unwrap();
                    assert_ne!(
                        Some(ready_actor),
                        process.1,
                        "readiness cannot revive the predecessor"
                    );
                    let current =
                        observed_root(root).expect("readiness has its admitted journal owner");
                    assert_eq!(current.admission.actor, ready_actor);
                    assert!(
                        current.terminal.is_none(),
                        "production {phase} actor {ready_actor} retired before readiness: {:?}",
                        current.terminal
                    );
                    return value;
                }
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

async fn settled_frontier(root: &Path, identity: &harness::embedding::HostIdentity) {
    tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            let store = harness::store::Store::open(root.join("run/harness/store.sqlite")).unwrap();
            let frontier = store.embedded_round_frontier(identity).unwrap();
            if frontier.pending_head.is_none()
                && frontier.settled_head.is_some()
                && matches!(
                    store.embedded_inference_state(identity).unwrap(),
                    harness::embedding::HostInferenceState::Idle
                )
            {
                assert!(frontier.pending_interruption.is_none());
                return;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("provider continuation settles before the crash");
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
    let artifacts = std::env::var_os("TIDEPOOL_TEST_ARTIFACT_ROOT")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target/recovery-fixtures")
        });
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
    std::fs::write(root.join("secret"), SECRET).unwrap();
    std::fs::write(root.join("auth.json"), "{}").unwrap();
    retained_root
}

fn finish_fixture(root: PathBuf) {
    if std::env::var_os("TIDEPOOL_TEST_ARTIFACT_ROOT").is_some()
        || std::env::var("TIDEPOOL_KEEP_TEST_LOGS").ok().as_deref() == Some("1")
    {
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

#[tokio::test]
async fn production_persona_crash_preserves_edited_context_without_reinitialization() {
    use std::os::unix::process::ExitStatusExt;
    tidepool_testing::eval_harness::require_extract();
    let retained_root = fixture();
    let root = retained_root.as_path();
    let run = root.join("run");
    let mut original = start(root, "persona-original");
    let original_pid = original.0.id();
    let initial = ready(root, "persona-original", &mut original).await;
    assert_eq!(initial["pid"].as_u64(), Some(u64::from(original_pid)));
    let original_actor: ActorRef = serde_json::from_value(initial["actor"].clone()).unwrap();
    let original_identity = embedded_recovery::host_identity(&run, "/root", original_actor);
    assert!(!root.join("persona-original.calls").exists());
    assert!(!root.join("persona-original.jev-calls").exists());
    input(
        initial["address"].as_str().unwrap(),
        &original_identity,
        None,
    )
    .await;
    cell_settled(root, "persona-original", &mut original).await;
    wait_calls(root, "persona-original").await;
    settled_frontier(root, &original_identity).await;
    assert_eq!(
        std::fs::read_to_string(root.join("persona-original.calls")).unwrap(),
        "3"
    );
    assert!(!root.join("persona-original.jev-calls").exists());
    let before =
        exomonad_actor::ActorRecoveryJournal::read_observed(&run.join("actor-lifecycle.v2.jsonl"))
            .unwrap();
    let bound = latest_durable_root_application(&before).unwrap().unwrap();
    assert_eq!(bound.admission.actor, original_actor);
    assert!(bound.terminal.is_none());
    let startup = bound.initial_context.clone().unwrap();
    assert_eq!(
        startup.state,
        exomonad_actor::InitialContextState::Published
    );
    let source = active_source_identity(&run, true).unwrap().unwrap();
    assert_eq!(
        bound
            .application
            .as_ref()
            .unwrap()
            .accepted_source
            .as_deref(),
        Some(source.as_str())
    );
    assert!(!root.join("project/.exomonad/state/notes").exists());
    let edited = ["persona", "human", "curiosities"]
        .map(|name| {
            format!(
                "[Memory file: .exomonad/state/memory/{name}.md]\n{}",
                std::fs::read_to_string(
                    root.join(format!("project/.exomonad/state/memory/{name}.md"))
                )
                .unwrap()
            )
        })
        .join("\n\n");
    let edited = edited.as_str();
    assert_ne!(edited, startup.result.as_ref().unwrap().sections[0].body);
    assert!(edited.contains("persona-memory-updated"));
    let store = harness::store::Store::open(run.join("harness/store.sqlite")).unwrap();
    let head = store
        .agent(&harness::model::AgentPath("/root".into()))
        .unwrap()
        .unwrap()
        .head_request
        .unwrap();
    let reference = startup
        .publication
        .as_ref()
        .unwrap()
        .authored_reference
        .as_deref()
        .unwrap();
    assert!(store.read_context(&head).unwrap().blocks.iter().any(|block| matches!(block,
        harness::context::ContextBlock::Text { reference: Some(current), role: harness::context::ContextRole::User, text, .. }
            if current.as_str() == reference && text == edited)));
    drop(store);
    original.0.kill().unwrap();
    assert_eq!(original.0.wait().unwrap().signal(), Some(9));
    drop(
        host_incarnation::HostRunLock::existing(&run)
            .expect("crashed process releases its actual run lock"),
    );
    let after_crash =
        exomonad_actor::ActorRecoveryJournal::read_observed(&run.join("actor-lifecycle.v2.jsonl"))
            .unwrap();
    assert!(latest_durable_root_application(&after_crash)
        .unwrap()
        .unwrap()
        .terminal
        .is_none());
    std::fs::write(
        root.join("project/.exomonad/state/memory/persona.md"),
        "changed-after-crash",
    )
    .unwrap();
    assert_eq!(
        active_source_identity(&run, true).unwrap().as_deref(),
        Some(source.as_str())
    );

    let mut recovered = start(root, "persona-recovered");
    assert_ne!(recovered.0.id(), original_pid);
    let resumed = ready(root, "persona-recovered", &mut recovered).await;
    assert_eq!(resumed["pid"].as_u64(), Some(u64::from(recovered.0.id())));
    let current_actor: ActorRef = serde_json::from_value(resumed["actor"].clone()).unwrap();
    assert_eq!(current_actor.id, original_actor.id);
    assert_eq!(
        current_actor.incarnation.0,
        original_actor.incarnation.0 + 1
    );
    let current_identity = embedded_recovery::host_identity(&run, "/root", current_actor);
    let current_records =
        exomonad_actor::ActorRecoveryJournal::read_observed(&run.join("actor-lifecycle.v2.jsonl"))
            .unwrap();
    let current = latest_durable_root_application(&current_records)
        .unwrap()
        .unwrap();
    assert_eq!(
        current
            .application
            .as_ref()
            .unwrap()
            .accepted_source
            .as_deref(),
        Some(source.as_str())
    );
    assert_eq!(
        current.startup.as_ref().unwrap().predecessor,
        Some(original_actor)
    );
    let store = harness::store::Store::open(run.join("harness/store.sqlite")).unwrap();
    assert!(store.embedded_binding_matches(&current_identity).unwrap());
    assert!(!store.embedded_binding_matches(&original_identity).unwrap());
    assert_eq!(
        store
            .agent(&harness::model::AgentPath("/root".into()))
            .unwrap()
            .unwrap()
            .head_request
            .as_ref(),
        Some(&head)
    );
    assert!(!root.join("project/.exomonad/state/notes").exists());
    assert_eq!(
        std::fs::read_to_string(root.join("project/.exomonad/state/memory/persona.md")).unwrap(),
        "changed-after-crash"
    );
    assert!(!root.join("persona-recovered.jev-calls").exists());
    assert!(!root.join("persona-recovered.calls").exists());
    input(
        resumed["address"].as_str().unwrap(),
        &current_identity,
        None,
    )
    .await;
    wait_calls(root, "persona-recovered").await;
    settled_frontier(root, &current_identity).await;
    assert_eq!(
        std::fs::read_to_string(root.join("persona-recovered.calls")).unwrap(),
        "1"
    );
    let request: harness::transport::ResponsesRequest = serde_json::from_slice(
        &std::fs::read(root.join("persona-recovered.request.json")).unwrap(),
    )
    .unwrap();
    let user_texts = |items: &[harness::item::Item]| {
        items
            .iter()
            .filter(|item| item.0["type"] == "message" && item.0["role"] == "user")
            .map(|item| item.0["content"].as_str().unwrap().to_owned())
            .collect::<Vec<_>>()
    };
    let projected = store
        .embedded_conversation_history(&current_identity)
        .unwrap();
    assert_eq!(
        user_texts(&request.input),
        user_texts(
            &projected
                .history
                .iter()
                .map(|(_, _, item)| item.clone())
                .collect::<Vec<_>>()
        )
    );
    let actual_users = user_texts(&request.input);
    assert_eq!(actual_users.len(), 3);
    assert!(actual_users[0].ends_with(edited));
    assert!(actual_users[0].contains("persona-memory-updated"));
    assert!(!actual_users[0].contains("changed-after-crash"));
    assert_eq!(
        &actual_users[1..],
        &[
            "continue retained conversation",
            "continue retained conversation"
        ]
    );
    assert!(store.read_context(projected.head.as_ref().unwrap()).unwrap().blocks.iter().any(|block| matches!(block,
        harness::context::ContextBlock::Text { reference: Some(current), text, .. } if current.as_str() == reference && text == edited)));
    assert!(!root.join("persona-recovered.jev-calls").exists());
    drop(store);
    recovered.0.kill().unwrap();
    assert_eq!(recovered.0.wait().unwrap().signal(), Some(9));
    let anchor = tidepool_atomic_write::DirectoryAnchor::open_existing(&run).unwrap();
    let journal = exomonad_actor::ActorRecoveryJournal::open_existing(
        &anchor,
        "actor-lifecycle.v2.jsonl",
        Arc::new(anchor.clone()),
    )
    .await
    .unwrap();
    assert_eq!(
        journal.initial_context(current_actor).await.unwrap(),
        Some(startup)
    );
    let mut stopped = journal.owner_status();
    drop(journal);
    tokio::time::timeout(Duration::from_secs(5), async {
        while !matches!(
            *stopped.borrow(),
            exomonad_actor::JournalOwnerStatus::Stopped { .. }
        ) {
            stopped.changed().await.unwrap();
        }
    })
    .await
    .expect("offline journal owner stops before fixture deletion");
    assert_eq!(
        *stopped.borrow(),
        exomonad_actor::JournalOwnerStatus::Stopped {
            clean: true,
            fenced: false
        }
    );
    finish_fixture(retained_root);
}

#[tokio::test]
async fn production_coherent_reload_recovers_published_configuration_without_recapturing_draft() {
    use std::os::unix::process::ExitStatusExt;
    tidepool_testing::eval_harness::require_extract();
    let retained_root = fixture();
    let root = retained_root.as_path();
    let run = root.join("run");
    let mut original = start(root, "coherent-original");
    let first = ready(root, "coherent-original", &mut original).await;
    let actor_a: ActorRef = serde_json::from_value(first["actor"].clone()).unwrap();
    let identity_a = embedded_recovery::host_identity(&run, "/root", actor_a);
    input(first["address"].as_str().unwrap(), &identity_a, None).await;
    cell_settled(root, "coherent-original", &mut original).await;
    settled_frontier(root, &identity_a).await;
    let request_b: harness::transport::ResponsesRequest = serde_json::from_slice(
        &std::fs::read(root.join("coherent-original.request-3.json")).unwrap(),
    )
    .unwrap();
    assert!(request_b
        .instructions
        .contains(include_str!("fixtures/coherent_reload_prompt_b.md")));
    let before =
        exomonad_actor::ActorRecoveryJournal::read_observed(&run.join("actor-lifecycle.v2.jsonl"))
            .unwrap();
    let startup = latest_durable_root_application(&before)
        .unwrap()
        .unwrap()
        .initial_context
        .clone()
        .unwrap();
    assert_eq!(
        std::fs::read_to_string(root.join("project/initializer-count.txt")).unwrap(),
        "x"
    );
    assert!(!root.join("coherent-original.jev-calls").exists());
    original.0.kill().unwrap();
    assert_eq!(original.0.wait().unwrap().signal(), Some(9));
    for (path, body) in [
        (
            ".exomonad/Project/Work.hs",
            include_str!("fixtures/coherent_reload_work_invalid.hs"),
        ),
        (
            ".exomonad/Project/ExtraCheck.hs",
            "module Project.ExtraCheck where\nextraCheck = missingFromDraftC\n",
        ),
        (
            ".exomonad/prompts/agent.md",
            "prompt-C must not be selected by recovery",
        ),
        (".exomonad/state/memory/human.md", "memory-C"),
    ] {
        std::fs::write(root.join("project").join(path), body).unwrap();
    }
    let mut recovered = start(root, "coherent-recovered");
    let resumed = ready(root, "coherent-recovered", &mut recovered).await;
    let actor_b: ActorRef = serde_json::from_value(resumed["actor"].clone()).unwrap();
    assert_eq!(actor_b.id, actor_a.id);
    assert_eq!(actor_b.incarnation.0, actor_a.incarnation.0 + 1);
    let identity_b = embedded_recovery::host_identity(&run, "/root", actor_b);
    assert!(!root.join("coherent-recovered.calls").exists());
    assert_eq!(
        std::fs::read_to_string(root.join("project/initializer-count.txt")).unwrap(),
        "x"
    );
    input(resumed["address"].as_str().unwrap(), &identity_b, None).await;
    cell_settled(root, "coherent-recovered", &mut recovered).await;
    settled_frontier(root, &identity_b).await;
    let request_recovered: harness::transport::ResponsesRequest = serde_json::from_slice(
        &std::fs::read(root.join("coherent-recovered.request-1.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(request_recovered.instructions, request_b.instructions);
    let user_texts = |request: &harness::transport::ResponsesRequest| {
        request
            .input
            .iter()
            .filter(|item| item.0["type"] == "message" && item.0["role"] == "user")
            .map(|item| item.0["content"].clone())
            .collect::<Vec<_>>()
    };
    let retained_users = user_texts(&request_b);
    let resumed_users = user_texts(&request_recovered);
    assert_eq!(
        &resumed_users[..retained_users.len()],
        retained_users.as_slice()
    );
    assert_eq!(resumed_users.len(), retained_users.len() + 1);
    assert!(!serde_json::to_string(&resumed_users)
        .unwrap()
        .contains("memory-C"));
    let after =
        exomonad_actor::ActorRecoveryJournal::read_observed(&run.join("actor-lifecycle.v2.jsonl"))
            .unwrap();
    assert_eq!(
        latest_durable_root_application(&after)
            .unwrap()
            .unwrap()
            .initial_context
            .as_ref(),
        Some(&startup)
    );
    assert!(!root.join("coherent-recovered.jev-calls").exists());
    assert_eq!(
        std::fs::read_to_string(root.join("project/initializer-count.txt")).unwrap(),
        "x"
    );
    recovered.0.kill().unwrap();
    assert_eq!(recovered.0.wait().unwrap().signal(), Some(9));
    // The recovery owner selects the installer; the draft and derived manifests
    // never locate an artifact for this refusal control.
    let owner = persisted_root_owner(&after, actor_b);
    let selected = tidepool_runtime::session::RunRecoveryStore::read_notebook_generation_at(
        &run.join("recovery.sqlite"),
        tidepool_runtime::session::RecoveryDomainId::Root,
        &owner,
    )
    .unwrap()
    .expect("cold B has a durable selected generation");
    let installation = exomonad_actor::PersistedActorInstallation::decode(
        &selected.references.installation,
        &selected.references.source,
    )
    .unwrap();
    let (recipe, original) = installation
        .acquisition()
        .completed_entry_selection()
        .unwrap();
    let mut config = configuration(root);
    config.workspace_inputs = Some(
        crate::exomonad::workspace::FrozenWorkspace::load(
            &config.workspace,
            config.run_directory.path(),
        )
        .unwrap(),
    );
    let lease = Arc::new(HostIncarnationLease::claim(&config.run_directory).unwrap());
    let (worktrees, bindings, allocation) =
        actor_worktree_resources(&config.workspace, &config.run_directory, || {}).unwrap();
    let source_owner = source_service(
        &config,
        &run,
        worktrees,
        crate::exomonad::source::SourceRootOwner::Host(lease.clone()),
    )
    .unwrap()
    .unwrap();
    use exomonad_actor::ActorSourceLayers;
    let source = source_owner
        .restore_source_with_acquisition(&selected.references.source, installation.acquisition())
        .unwrap();
    let exomonad_actor::SourceEntryStorage::CompletedOriginal {
        directory,
        selections,
    } = source.prepared_entries().unwrap()
    else {
        panic!("selected installer must use completed source-owned storage");
    };
    assert_eq!(selections.get(recipe), Some(&original));
    let installer = directory.join(recipe).join(original.to_string());
    let output = installer.join("entry");
    let retained_output = installer.join("entry.retained-for-refusal");
    let wrapper = installer.join("Expr.hs");
    let original_wrapper = std::fs::read(&wrapper).unwrap();
    drop(source);
    drop(source_owner);
    drop(bindings);
    drop(allocation);
    drop(lease);
    let assert_configuration_preserved = || {
        let records = exomonad_actor::ActorRecoveryJournal::read_observed(
            &run.join("actor-lifecycle.v2.jsonl"),
        )
        .unwrap();
        let startup = latest_durable_root_application(&records).unwrap().unwrap();
        // Startup can commit an incarnation transfer before installer loading.
        // Observe the two exact owners named by its durable startup intent.
        let actors = std::iter::once(startup.admission.actor)
            .chain(startup.startup.as_ref().unwrap().manifest_predecessor);
        let heads = actors
            .filter_map(|actor| {
                let lookup = persisted_root_owner(&records, actor);
                tidepool_runtime::session::RunRecoveryStore::read_notebook_generation_at(
                    &run.join("recovery.sqlite"),
                    tidepool_runtime::session::RecoveryDomainId::Root,
                    &lookup,
                )
                .unwrap()
            })
            .collect::<Vec<_>>();
        assert_eq!(
            heads.len(),
            1,
            "startup retains one selected configuration owner"
        );
        assert_eq!(
            heads[0].references, selected.references,
            "installer refusal preserves exact B configuration through ownership transfer"
        );
    };
    std::fs::rename(&output, &retained_output).unwrap();
    let mut missing = start(root, "coherent-missing-installer");
    refused(
        root,
        "coherent-missing-installer",
        &mut missing,
        "selected completed installer is unavailable",
    )
    .await;
    assert_configuration_preserved();
    std::fs::rename(&retained_output, &output).unwrap();
    std::fs::write(
        &wrapper,
        b"module Expr where\ncorruptSelectedInstaller = ()\n",
    )
    .unwrap();
    let mut corrupt = start(root, "coherent-corrupt-installer");
    refused(
        root,
        "coherent-corrupt-installer",
        &mut corrupt,
        "recovered installer source closure differs from its owner",
    )
    .await;
    std::fs::write(&wrapper, original_wrapper).unwrap();
    assert_configuration_preserved();
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
    let previous = observed_root(root).map(|record| record.admission.actor);
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
        previous,
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
            initial_context: None,
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
async fn production_missing_recovery_store_refuses_bound_root_without_rewriting_journal() {
    tidepool_testing::eval_harness::require_extract();
    let retained_root = fixture();
    let root = retained_root.as_path();
    let mut first = start_crashing(root, "retained-bound-root", "bound");
    crashed(root, "retained-bound-root", &mut first).await;
    let journal = root.join("run/actor-lifecycle.v2.jsonl");
    let journal_bytes = std::fs::read(&journal).unwrap();
    let database = root.join("run/recovery.sqlite");
    assert!(tidepool_runtime::session::observe_declaration_domain(
        &database,
        tidepool_runtime::session::RecoveryDomainId::Root,
    )
    .unwrap()
    .is_some());
    let mut retained_files = Vec::new();
    for suffix in ["", "-wal", "-shm"] {
        let original = root.join(format!("run/recovery.sqlite{suffix}"));
        if original.try_exists().unwrap() {
            let retained = root.join(format!("run/retained-recovery.sqlite{suffix}"));
            let bytes = std::fs::read(&original).unwrap();
            std::fs::rename(&original, &retained).unwrap();
            retained_files.push((retained, bytes));
        }
    }
    let mut refused_root = start(root, "missing-recovery-store");
    refused(
        root,
        "missing-recovery-store",
        &mut refused_root,
        "root startup lost its retained public recovery domain",
    )
    .await;
    assert_eq!(std::fs::read(&journal).unwrap(), journal_bytes);
    for (retained, bytes) in retained_files {
        assert_eq!(std::fs::read(&retained).unwrap(), bytes);
    }
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
    use tidepool_runtime::session::{
        observe_declaration_domain, RecoveryArtifactClosure, RecoveryDomainId, RecoveryExportKind,
        RecoveryNodeKind,
    };
    let recovery_path = root.join("run/recovery.sqlite");
    let before = observe_declaration_domain(&recovery_path, RecoveryDomainId::Root)
        .unwrap()
        .unwrap()
        .high_water;
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
    let published = observe_declaration_domain(&recovery_path, RecoveryDomainId::Root)
        .unwrap()
        .unwrap();
    let authored = ((before.0 + 1)..=published.high_water.0)
        .filter_map(|id| {
            published
                .declaration(tidepool_repr::Generation(id))
                .unwrap()
        })
        .find(|node| {
            node.kind == RecoveryNodeKind::Authored
                && node.exports.iter().any(|export| {
                    export.kind == RecoveryExportKind::Value
                        && export.identity.occurrence == "coldAnswer"
                })
        })
        .expect("coldAnswer original authored declaration was not retained");
    let answer = authored
        .exports
        .iter()
        .find(|export| export.identity.occurrence == "coldAnswer")
        .unwrap();
    let unit = &answer.identity.unit;
    let module = &answer.identity.module;
    assert!(!unit.is_empty());
    assert_eq!(
        *module,
        tidepool_repr::SessionModule::lib(authored.generation).module_name(),
        "authored owner does not match its original declaration generation"
    );
    assert!(
        authored
            .exports
            .iter()
            .any(|export| export.kind == RecoveryExportKind::Type
                && export.identity.occurrence == "RecoveryBox"
                && export.identity.unit == *unit
                && export.identity.module == *module
                && export
                    .children
                    .iter()
                    .any(|child| child.occurrence == "RecoveryBox"
                        && child.unit == *unit
                        && child.module == *module)),
        "original RecoveryBox type and constructor identities were not retained"
    );
    for class in ["Eq", "Show"] {
        assert!(
            authored
                .instances
                .classes
                .iter()
                .any(|instance| instance.selected
                    && instance.class.occurrence == class
                    && instance.dfun.unit == *unit
                    && instance.dfun.module == *module),
            "original {class} instance was not retained"
        );
    }
    let original_reference = authored
        .artifact_refs
        .iter()
        .find_map(|id| match published.artifact(*id).unwrap() {
            Some(RecoveryArtifactClosure::Home(reference))
                if reference.unit == *unit && reference.module == *module =>
            {
                Some(reference)
            }
            _ => None,
        })
        .expect("authored original module product/interface was not retained");
    tidepool_toolchain::recovery_artifacts::verify_materialized_ref(
        &root.join("run/haskell-session"),
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
    let recovered_domain = observe_declaration_domain(&recovery_path, RecoveryDomainId::Root)
        .unwrap()
        .unwrap();
    let original_id =
        tidepool_toolchain::artifact_inventory::ArtifactDescriptor::from_recovery_product(
            &original_reference,
        )
        .id;
    assert_eq!(
        recovered_domain.artifact(original_id).unwrap(),
        Some(RecoveryArtifactClosure::Home(original_reference.clone())),
        "cold successor replaced the original module/product identity"
    );
    tidepool_toolchain::recovery_artifacts::verify_materialized_ref(
        &root.join("run/haskell-session"),
        &original_reference,
    )
    .expect("cold successor changed the original retained product/interface bytes");
    assert_eq!(
        std::fs::read_to_string(root.join("publish-original.calls")).unwrap(),
        original_calls
    );
    finish_fixture(retained_root);
}
