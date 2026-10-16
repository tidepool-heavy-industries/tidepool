//! Real outer host admission, resources, provider driving and physical release.
use super::*;
use crate::actor_host::HostTestHooks;
use harness::{
    engine::ResponsesTransport,
    item::Item,
    transport::{PreparedProviderRequest, ResponsesRequest, ResponsesTurn, TransportError},
};
use serde_json::{json, Value};
use std::{
    os::unix::{fs::OpenOptionsExt, process::ExitStatusExt},
    process::{Child, Command, Stdio},
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    },
};

const HELPER: &str = "exomonad::production_restart_acceptance_tests::outer_host_process";
const ROOT: &str = "TIDEPOOL_OUTER_RESTART_ROOT";
const PHASE: &str = "TIDEPOOL_OUTER_RESTART_PHASE";
const SLICE: &str = "tidepool-completion-build.slice";
const DEADLINE: Duration = Duration::from_secs(300);

fn fixture_code(name: &str) -> String {
    match name {
        "spec" => tidepool_testing::fixture_source(
            "exomonad/host/src/exomonad/fixtures/restart_agent_spec.hs",
        ),
        "effect" => tidepool_testing::fixture_source(
            "exomonad/host/src/exomonad/fixtures/restart_effect_once.hs",
        ),
        "crash" => tidepool_testing::fixture_source(
            "exomonad/host/src/exomonad/fixtures/restart_crash_effect_once.hs",
        ),
        "pending" => tidepool_testing::fixture_source(
            "exomonad/host/src/exomonad/fixtures/restart_pending_effect_once.hs",
        ),
        "observe" => tidepool_testing::fixture_source(
            "exomonad/host/src/exomonad/fixtures/restart_observe_b.hs",
        ),
        "publish" => tidepool_testing::fixture_source(
            "exomonad/host/src/actor_host/fixtures/coherent_recovery_publish_b.hs",
        ),
        "noop" => tidepool_testing::fixture_source(
            "exomonad/host/src/exomonad/fixtures/restart_reload_b_again.hs",
        ),
        "bind" => tidepool_testing::fixture_source(
            "exomonad/host/src/actor_host/fixtures/coherent_recovery_bind_b.hs",
        ),
        _ => panic!("unknown authored fixture {name}"),
    }
}

fn run_root(root: &Path) -> PathBuf {
    root.join(std::fs::read_to_string(root.join("run-id")).unwrap())
}

fn evidence(root: &Path, phase: &str, suffix: &str) -> PathBuf {
    root.join(format!("{phase}.{suffix}"))
}
fn write_json(path: &Path, value: &impl serde::Serialize) {
    tidepool_atomic_write::write_durable(path, &serde_json::to_vec_pretty(value).unwrap()).unwrap();
}

struct Provider {
    root: PathBuf,
    phase: String,
    calls: AtomicUsize,
}
impl Provider {
    fn failure(&self, message: String) -> TransportError {
        write_json(
            &evidence(&self.root, &self.phase, "failure.json"),
            &json!({"error":message}),
        );
        TransportError::Stream(message)
    }
}
#[async_trait::async_trait]
impl ResponsesTransport for Provider {
    fn response_contract(
        &self,
    ) -> Result<
        (
            harness::transport::ResponsesRoute,
            harness::transport::ResponsesProtocol,
        ),
        TransportError,
    > {
        Ok((
            harness::transport::ResponsesRoute::ChatGptPlan,
            harness::transport::ResponsesProtocol::Standard,
        ))
    }
    async fn create(&self, _: PreparedProviderRequest) -> Result<ResponsesTurn, TransportError> {
        panic!("real Engine must use streamed dispatch")
    }
    async fn create_streaming(
        &self,
        prepared: PreparedProviderRequest,
        sink: mpsc::Sender<harness::transport::sse::StreamEvent>,
    ) -> Result<ResponsesTurn, TransportError> {
        let index = self.calls.fetch_add(1, Ordering::SeqCst);
        let request = prepared.request();
        write_json(
            &evidence(&self.root, &self.phase, &format!("request-{index}.json")),
            request,
        );
        let crash_output_observed = self.phase == "crash"
            && index > 0
            && request.input.iter().any(|item| {
                item.0["call_id"] == "crash-0" && item.0["type"] == "custom_tool_call_output"
            });
        if (index > 0 && self.phase != "crash") || crash_output_observed {
            let previous = if self.phase == "crash" {
                "crash-0".to_owned()
            } else {
                format!("{}-{}", self.phase, index - 1)
            };
            let output = request
                .input
                .iter()
                .find(|item| {
                    item.0["call_id"] == previous && item.0["type"] == "custom_tool_call_output"
                })
                .ok_or_else(|| {
                    self.failure(format!("successor omitted exact completed call {previous}"))
                })?;
            let result: Value = serde_json::from_str(output.0["output"].as_str().unwrap())
                .map_err(|error| self.failure(error.to_string()))?;
            if !matches!(result["status"].as_str(), Some("committed" | "completed"))
                || !result["items"]
                    .as_array()
                    .is_some_and(|items| items.iter().all(|item| item["status"] == "committed"))
            {
                return Err(
                    self.failure(format!("original invocation {previous} refused: {result}"))
                );
            }
        }
        if crash_output_observed {
            write_json(
                &evidence(&self.root, &self.phase, "inflight.json"),
                &json!({"request":prepared.request_id(), "index":index}),
            );
            std::future::pending::<()>().await;
        }
        let code = match (self.phase.as_str(), index) {
            ("fresh", 0) => Some(fixture_code("effect")),
            ("fresh", 1) => Some(fixture_code("publish")),
            ("fresh", 2) => Some(fixture_code("noop")),
            ("fresh", 3) => Some(fixture_code("bind")),
            ("crash", 0) => Some(fixture_code("crash")),
            ("uncertain", 0) => Some(fixture_code("pending")),
            ("recovered", 0) => Some(fixture_code("observe")),
            _ => None,
        };
        let items = match code {
            Some(input) => vec![Item(
                json!({"type":"custom_tool_call", "call_id":format!("{}-{index}", self.phase), "name":if matches!(self.phase.as_str(), "crash" | "uncertain") { "haskell" } else { "haskell_sync" }, "async":matches!(self.phase.as_str(), "crash" | "uncertain"), "input":input}),
            )],
            None => {
                write_json(
                    &evidence(&self.root, &self.phase, "final.json"),
                    &json!({"request":prepared.request_id()}),
                );
                vec![Item(
                    json!({"type":"message","role":"assistant","phase":"final_answer","content":[{"type":"output_text","text":"retained final"}]}),
                )]
            }
        };
        let turn = ResponsesTurn {
            response_id: format!("{}-{index}", self.phase),
            items,
            usage: Default::default(),
        };
        let mut assembly = harness::transport::sse::ResponseAssembly::default();
        for (index, item) in turn.items.iter().enumerate() {
            let event = assembly
                .accept(
                    &json!({"type":"response.output_item.done","output_index":index,"item":item})
                        .to_string(),
                )?
                .unwrap();
            sink.send(event)
                .await
                .map_err(|error| TransportError::Stream(error.to_string()))?;
        }
        if self.phase == "uncertain" && index == 0 {
            // The real tool runs before the envelope completes. Parent kills the
            // host only after its external effect is independently observable.
            std::future::pending::<()>().await;
        }
        Ok(turn)
    }
}

#[test]
#[ignore = "subprocess entry; parent owns its inputs, lifetime and observations"]
fn outer_host_process() {
    let root = PathBuf::from(std::env::var_os(ROOT).expect("parent supplies exact fixture root"));
    let phase = std::env::var(PHASE).expect("parent supplies exact fixture phase");
    tokio::runtime::Builder::new_multi_thread().worker_threads(2).enable_all().build().unwrap().block_on(async {
        if phase == "resources" {
            resources::serve(root.join("resources/resources.sock"), root.join("resources/policy.toml")).await.unwrap();
            return;
        }
        let selected_root = run_root(&root);
        let run_id = selected_root.file_name().unwrap().to_str().unwrap().to_owned();
        let provider = Arc::new(Provider { root: root.clone(), phase: phase.clone(), calls: AtomicUsize::new(0) });
        let (hooks, _stop, assembly) = HostTestHooks::external_provider(provider);
        let host = host_with_test_hooks(HostOptions { run_id, run_root: selected_root }, hooks, root.join("resources/resources.sock"));
        tokio::pin!(host);
        tokio::select! {
            result = &mut host => {
                if phase.starts_with("refusal-") {
                    let error = result.expect_err("missing evidence must refuse startup");
                    let permanent = error.downcast_ref::<HostBootRefusal>().is_some();
                    write_json(&evidence(&root, &phase, "refusal.json"), &json!({"permanent":permanent,"error":error.to_string()}));
                    assert!(permanent, "startup refusal lost its permanent typed classification: {error}");
                    return;
                }
                panic!("outer host exited before assembly: {result:?}");
            },
            context = assembly => {
                assert!(!phase.starts_with("refusal-"), "host assembled despite missing admitted evidence");
                let context = context.expect("production assembly must issue observations");
                let identity = loop {
                    if let Ok(identity) = context.conversation_identity() {
                        break identity;
                    }
                    tokio::select! {
                        result = &mut host => panic!("production host exited before conversation binding: {result:?}"),
                        terminal = context.actor.terminal().wait() => panic!("original actor terminated before conversation binding: {terminal:?}"),
                        _ = tokio::time::sleep(Duration::from_millis(10)) => {},
                    }
                };
                write_json(&evidence(&root, &phase, "assembled.json"), &json!({
                    "identity":identity, "actor":context.actor.identity(), "address":context.service_address,
                    "selected_account":context.store().run_account(&identity.run).unwrap(),
                }));
                // Assembly handles carry authority; release them before the
                // production host's physical lease acknowledgement can complete.
                drop(context);
            }
        }
        host.await.unwrap();
        write_json(&evidence(&root, &phase, "released.json"), &json!({"host_returned":true}));
    });
}

struct Process(Child);
impl Process {
    fn start(root: &Path, phase: &str, checkpoint: Option<&str>) -> Self {
        let log = std::fs::File::create(evidence(root, phase, "log")).unwrap();
        let mut command = Command::new(std::env::current_exe().unwrap());
        command
            .args(["--exact", HELPER, "--ignored", "--nocapture"])
            .env(ROOT, root)
            .env(PHASE, phase)
            .env("TIDEPOOL_RECOVERY_TEST_ROOT", root)
            .env("TIDEPOOL_RECOVERY_TEST_PHASE", phase)
            .stdout(Stdio::from(log.try_clone().unwrap()))
            .stderr(Stdio::from(log));
        match checkpoint {
            Some(point) => {
                command.env("TIDEPOOL_STARTUP_CRASH_AT", point);
            }
            None => {
                command.env_remove("TIDEPOOL_STARTUP_CRASH_AT");
            }
        }
        Self(command.spawn().unwrap())
    }
    fn assert_live(&mut self) {
        assert!(
            self.0.try_wait().unwrap().is_none(),
            "host exited before its requested observation"
        );
    }
    async fn wait(&mut self) -> std::process::ExitStatus {
        tokio::time::timeout(DEADLINE, async {
            loop {
                if let Some(status) = self.0.try_wait().unwrap() {
                    return status;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("owned subprocess did not settle")
    }
    async fn stop(&mut self) {
        assert!(Command::new("kill")
            .args(["-TERM", &self.0.id().to_string()])
            .status()
            .unwrap()
            .success());
        assert!(self.wait().await.success());
    }
    async fn crash(&mut self) {
        self.0.kill().unwrap();
        assert_eq!(self.wait().await.signal(), Some(9));
    }
}
impl Drop for Process {
    fn drop(&mut self) {
        if self.0.try_wait().ok().flatten().is_none() {
            let _ = self.0.kill();
        }
        let _ = self.0.wait();
    }
}

async fn observed(root: &Path, phase: &str, suffix: &str, process: &mut Process) -> Value {
    let started = std::time::Instant::now();
    let result = tokio::time::timeout(DEADLINE, async {
        loop {
            process.assert_live();
            if let Ok(error) = std::fs::read_to_string(evidence(root, phase, "failure.json")) {
                panic!("provider observed original failure: {error}");
            }
            if let Ok(bytes) = std::fs::read(evidence(root, phase, suffix)) {
                return serde_json::from_slice(&bytes).unwrap();
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await;
    eprintln!(
        "outer-restart-observation {}",
        json!({
            "phase":phase,
            "observation":suffix,
            "budget_ms":DEADLINE.as_millis(),
            "elapsed_ms":started.elapsed().as_millis(),
            "observed":result.is_ok(),
        }),
    );
    result.unwrap_or_else(|_| {
        panic!(
            "missing {phase}.{suffix} after {:?} (budget {DEADLINE:?}); inspect owned subprocess log",
            started.elapsed(),
        )
    })
}

struct ResourceService {
    unit: String,
}
impl ResourceService {
    async fn start(root: &Path) -> Self {
        let unit = format!("restart-resource-{}.service", uuid::Uuid::new_v4());
        let mut command = Command::new("systemd-run");
        command
            .args([
                "--user",
                "--quiet",
                "--service-type=exec",
                "--property=Delegate=yes",
                "--property=KillMode=control-group",
            ])
            .arg(format!("--unit={unit}"))
            .arg(format!("--slice={SLICE}"))
            .arg(format!("--setenv={ROOT}={}", root.display()))
            .arg(format!("--setenv={PHASE}=resources"));
        if let Some(path) = std::env::var_os("LD_LIBRARY_PATH") {
            command.arg(format!(
                "--setenv=LD_LIBRARY_PATH={}",
                path.to_str().unwrap()
            ));
        }
        let output = command
            .arg(std::env::current_exe().unwrap())
            .args(["--exact", HELPER, "--ignored", "--nocapture"])
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let service = Self { unit };
        service.ready(root).await;
        service.record(root);
        service
    }
    async fn restart(&self, root: &Path) {
        let status = Command::new("systemctl")
            .args(["--user", "restart", &self.unit])
            .status()
            .unwrap();
        assert!(
            status.success(),
            "same-unit private resource restart failed"
        );
        self.ready(root).await;
        self.record(root);
    }
    async fn ready(&self, root: &Path) {
        tokio::time::timeout(Duration::from_secs(30), async {
            loop {
                if tokio::net::UnixStream::connect(root.join("resources/resources.sock"))
                    .await
                    .is_ok()
                {
                    break;
                }
                let output = Command::new("systemctl")
                    .args(["--user", "is-active", "--quiet", &self.unit])
                    .output()
                    .unwrap();
                assert!(output.status.success(), "isolated resource service exited");
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("isolated resource service must become ready");
    }
    fn record(&self, root: &Path) {
        let output = Command::new("systemctl")
            .args([
                "--user",
                "show",
                &self.unit,
                "--property=MainPID,ControlGroup",
            ])
            .output()
            .unwrap();
        assert!(output.status.success());
        let properties = String::from_utf8(output.stdout).unwrap();
        let pid = properties
            .lines()
            .find_map(|line| line.strip_prefix("MainPID="))
            .unwrap();
        write_json(
            &root.join(format!("resources/{}-{pid}.json", self.unit)),
            &json!({"unit":self.unit,"manager_properties":properties,"process_cgroup":std::fs::read_to_string(format!("/proc/{pid}/cgroup")).unwrap()}),
        );
    }
}
impl Drop for ResourceService {
    fn drop(&mut self) {
        let status = Command::new("systemctl")
            .args(["--user", "stop", &self.unit])
            .status()
            .unwrap();
        assert!(status.success());
    }
}

struct Fixture {
    files: tempfile::TempDir,
    repository: exomonad_worktree::testing::TestRepo,
    run: PathBuf,
}
impl Fixture {
    async fn new(admit: bool) -> Self {
        Self::with_initializer_pause(admit, false).await
    }
    async fn with_initializer_pause(admit: bool, pause: bool) -> Self {
        exomonad_node::systemd_slice::SystemdSlice::try_from(SLICE.to_owned())
            .unwrap()
            .current_membership()
            .expect("run the declared facade_outer_restart_test in its delegated user slice");
        tidepool_testing::eval_harness::require_extract();
        let retained = std::env::var_os("TIDEPOOL_TEST_ARTIFACT_ROOT");
        // Run-owned Unix sockets must fit sockaddr_un independently of the
        // evidence directory depth. Retain the issuer's exact selected path.
        let mut files = tempfile::Builder::new()
            .prefix("tpr-")
            .tempdir_in("/tmp")
            .unwrap();
        files.disable_cleanup(retained.is_some());
        if let Some(root) = &retained {
            write_json(
                &PathBuf::from(root).join("outer-fixture.json"),
                &json!({"root":files.path()}),
            );
        }
        let mut repository = exomonad_worktree::testing::TestRepo::init_in(files.path()).unwrap();
        repository.disable_cleanup(retained.is_some());
        let project = repository.path();
        let authored = project.join(".exomonad");
        std::fs::create_dir_all(authored.join("Project")).unwrap();
        std::fs::create_dir_all(project.join("reload-inputs")).unwrap();
        for (path, body) in [
            (".exomonad/AgentSpec.hs", fixture_code("spec")),
            (
                ".exomonad/Project/Work.hs",
                tidepool_testing::fixture_source(
                    "exomonad/host/src/actor_host/fixtures/coherent_reload_work_a.hs",
                ),
            ),
            (
                "reload-inputs/work-b.txt",
                tidepool_testing::fixture_source(
                    "exomonad/host/src/actor_host/fixtures/coherent_reload_work_b.hs",
                ),
            ),
            (
                "reload-inputs/extra-b.txt",
                tidepool_testing::fixture_source(
                    "exomonad/host/src/actor_host/fixtures/coherent_recovery_extra_b.hs",
                ),
            ),
            ("reload-inputs/prompt-b.txt", "accepted prompt B".to_owned()),
            ("initializer-count.txt", String::new()),
            (
                "pause-initializer.txt",
                if pause { "pause" } else { "" }.to_owned(),
            ),
            ("effect-count.txt", String::new()),
            ("expected-effects.txt", "x".to_owned()),
        ] {
            std::fs::write(project.join(path), body).unwrap();
        }
        let settings = crate::actor_host::test_campaign::hosted_test_settings(&files, 1);
        let account = include_str!("../actor_host/fixtures/saved-account-test.json").to_owned();
        for (path, text) in [
            (settings.credential_file.clone(), account.clone()),
            (
                settings.credential_file.with_file_name("second.json"),
                account
                    .replace("test-account-a", "test-account-b")
                    .replace("first@example.test", "second@example.test"),
            ),
        ] {
            use std::io::Write;
            let mut file = std::fs::OpenOptions::new()
                .create(true)
                .truncate(true)
                .write(true)
                .mode(0o600)
                .open(path)
                .unwrap();
            file.write_all(text.as_bytes()).unwrap();
        }
        write_fixture_project_config(&authored, "test-model", |project| {
            project.launch.embedded = Some(settings);
            project.launch.systemd_slice = SLICE.to_owned().try_into().unwrap();
            project.haskell.source_roots = vec![PathBuf::from(".")];
            project.haskell.modules = vec!["Project.Work".into()];
            project.haskell.spec = Some("AgentSpec.agentSpec".into());
            project.prompts.agent = Some(PathBuf::from("prompts/agent.md"));
        });
        std::fs::create_dir_all(authored.join("prompts")).unwrap();
        std::fs::write(authored.join("prompts/agent.md"), "initial prompt A").unwrap();
        let git = exomonad_worktree::GitCli::new();
        git.try_run(project, &["add", "--all", "--", "."]).unwrap();
        git.try_run(
            project,
            &[
                "-c",
                "commit.gpgsign=false",
                "commit",
                "--quiet",
                "-m",
                "authored restart workspace",
            ],
        )
        .unwrap();
        let run_id = uuid::Uuid::new_v4().to_string();
        std::fs::write(files.path().join("run-id"), &run_id).unwrap();
        let run = tidepool_atomic_write::DirectoryAnchor::open_existing(files.path())
            .unwrap()
            .child(&run_id)
            .unwrap();
        ensure_private_run_root(&run).unwrap();
        let selected = std::env::current_exe().unwrap();
        retain_run_executable(run.path(), "exomonad", &selected).unwrap();
        retain_run_executable(
            run.path(),
            "exomonad-view-helper",
            &exomonad_node::view_command::helper_path().unwrap(),
        )
        .unwrap();
        let tidepool_toolchain::toolchain::CompilerDeploymentConfiguration::Configured(authority) =
            tidepool_toolchain::toolchain::CompilerDeploymentConfiguration::from_env().unwrap()
        else {
            panic!("declared native compiler authority is required")
        };
        retain_run_executable(run.path(), "tidepool-extract", &authority.frontend_path).unwrap();
        retain_run_executable(
            run.path(),
            "tidepool-extract-worker",
            &authority.worker_path,
        )
        .unwrap();
        write_json(&run.path().join("compiler-deployment.json"), &authority);
        let preparation = files.path().join("prepared");
        prepare(PrepareOptions {
            workspace: Some(project.to_path_buf()),
            directory: preparation.clone(),
        })
        .await
        .unwrap();
        if admit {
            admit_run_launch(
                run.path(),
                &RunLaunchCoordinates {
                    version: 1,
                    run_id,
                    workspace: project.to_path_buf(),
                    session: "outer-restart-acceptance".into(),
                    agent: ExomonadAgentDefaults {
                        model: "test-model".into(),
                        effort: ExomonadEffort::Low,
                    },
                },
                Some(&preparation),
            )
            .await
            .unwrap();
        }
        std::fs::create_dir(files.path().join("resources")).unwrap();
        std::fs::write(
            files.path().join("resources/policy.toml"),
            toml::to_string(&exomonad_node::command_resources::CommandResourcePolicy::default())
                .unwrap(),
        )
        .unwrap();
        Self {
            files,
            repository,
            run: run.path().to_path_buf(),
        }
    }
    fn root(&self) -> &Path {
        self.files.path()
    }
    fn workspace(&self) -> &Path {
        self.repository.path()
    }
    async fn ready(&self, phase: &str, process: &mut Process) -> Value {
        let assembled = observed(self.root(), phase, "assembled.json", process).await;
        tokio::time::timeout(DEADLINE, async {
            loop {
                process.assert_live();
                if let Ok(bytes) = std::fs::read(self.run.join("status.json")) {
                    let status = decode_run_status(&bytes).unwrap();
                    match status.phase {
                        RunPhase::EmbeddedReady { .. } => return,
                        RunPhase::Failed { error } => {
                            panic!("actual host failed before readiness: {error}")
                        }
                        RunPhase::Exited => panic!("actual host exited before readiness"),
                        _ => {}
                    }
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("actual outer host must publish embedded readiness");
        assembled
    }
    async fn command(
        &self,
        ready: &Value,
        command: harness::server::HostCommand,
    ) -> harness::embedding::ClientOperationId {
        let client = reqwest::Client::builder()
            .timeout(DEADLINE)
            .build()
            .unwrap();
        let api = format!("http://{}/api", ready["address"].as_str().unwrap());
        let origin = format!("https://{}", ready["address"].as_str().unwrap());
        let login = client
            .post(format!("{api}/session"))
            .header(reqwest::header::ORIGIN, &origin)
            .json(&json!({"secret":"hosted-script-test-secret-32-bytes"}))
            .send()
            .await
            .unwrap();
        assert_eq!(login.status(), reqwest::StatusCode::OK);
        let cookie = login.headers()[reqwest::header::SET_COOKIE]
            .to_str()
            .unwrap()
            .split(';')
            .next()
            .unwrap();
        let operation = harness::embedding::ClientOperationId(uuid::Uuid::new_v4());
        let wire = harness::server::browser_contract::ClientCommand::Host {
            operation_id: operation,
            command: (&command).into(),
        };
        let accepted = client
            .post(format!("{api}/commands"))
            .header(reqwest::header::ORIGIN, origin)
            .header(reqwest::header::COOKIE, cookie)
            .json(&wire)
            .send()
            .await
            .unwrap();
        assert_eq!(accepted.status(), reqwest::StatusCode::ACCEPTED);
        operation
    }
    async fn command_receipt(
        &self,
        identity: &harness::embedding::HostIdentity,
        operation: harness::embedding::ClientOperationId,
        process: &mut Process,
    ) -> harness::server::CommandReceipt {
        tokio::time::timeout(DEADLINE, async {
            loop {
                process.assert_live();
                if let Some(receipt) = self
                    .store()
                    .embedded_command(&identity.run, operation)
                    .unwrap()
                    .and_then(|entry| entry.receipt)
                {
                    return receipt;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("the exact admitted command must retain its receipt")
    }
    async fn input(&self, ready: &Value, text: &str) -> harness::embedding::ClientOperationId {
        let identity = serde_json::from_value(ready["identity"].clone()).unwrap();
        self.command(
            ready,
            harness::server::HostCommand::Input {
                target: identity,
                text: text.into(),
            },
        )
        .await
    }
    async fn settled(
        &self,
        identity: &harness::embedding::HostIdentity,
        phase: &str,
        process: &mut Process,
    ) -> harness::model::RequestId {
        tokio::time::timeout(DEADLINE, async {
            loop {
                process.assert_live();
                if let Ok(error) =
                    std::fs::read_to_string(evidence(self.root(), phase, "failure.json"))
                {
                    panic!("original operation failed: {error}");
                }
                let frontier = self.store().embedded_round_frontier(identity).unwrap();
                if frontier.pending_head.is_none() {
                    if let Some(head) = frontier.settled_head {
                        return head;
                    }
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("exact durable model round must settle")
    }
    fn store(&self) -> harness::store::Store {
        harness::store::Store::open(self.run.join("harness/store.sqlite")).unwrap()
    }
    fn draft_c(&self) {
        std::fs::write(
            self.workspace().join(".exomonad/Project/Work.hs"),
            "module Project.Work where\nthisDraftDoesNotCompile = unknownDraftC\n",
        )
        .unwrap();
        std::fs::write(
            self.workspace().join(".exomonad/prompts/agent.md"),
            "unaccepted draft C",
        )
        .unwrap();
    }
}

fn projected_operation_output(
    store: &harness::store::Store,
    identity: &harness::embedding::HostIdentity,
    operation: &harness::model::OperationId,
) -> Item {
    store
        .inspect_embedded_result(identity, operation)
        .unwrap()
        .expect("the actual binding retains the exact original result");
    let history = store.embedded_conversation_history(identity).unwrap();
    let calls = history
        .history
        .iter()
        .filter(|(_, _, item)| {
            item.0["type"] == "custom_tool_call" && item.0["call_id"] == operation.call.0
        })
        .collect::<Vec<_>>();
    let [(request, _, _)] = calls.as_slice() else {
        panic!("the exact original call must occur once in the binding's qualified history");
    };
    assert_eq!(
        store
            .recorded_operation_for_request(request, &operation.call)
            .unwrap()
            .as_ref(),
        Some(operation),
    );
    let outputs = history
        .history
        .into_iter()
        .filter_map(|(_, _, item)| {
            (item.0["type"] == "custom_tool_call_output" && item.0["call_id"] == operation.call.0)
                .then_some(item)
        })
        .collect::<Vec<_>>();
    let [output] = outputs.as_slice() else {
        panic!("the exact original output must occur once in the binding's qualified history");
    };
    output.clone()
}

#[tokio::test]
#[ignore = "requires the declared delegated user slice and isolated systemd service"]
async fn outer_host_stop_and_crash_restore_accepted_source_and_selected_account_without_replay() {
    use harness::transport::auth::chatgpt::SavedAccounts;
    let fixture = Fixture::new(true).await;
    let resources = ResourceService::start(fixture.root()).await;
    let mut fresh = Process::start(fixture.root(), "fresh", None);
    let first = fixture.ready("fresh", &mut fresh).await;
    fixture
        .input(&first, "Establish accepted B and retained declarations.")
        .await;
    // A successor request is issued after the preceding native output settles.
    // Bound each of the four distinct notebook calls through that observation.
    for index in 1..=4 {
        observed(
            fixture.root(),
            "fresh",
            &format!("request-{index}.json"),
            &mut fresh,
        )
        .await;
    }
    observed(fixture.root(), "fresh", "final.json", &mut fresh).await;
    let source_b = source::SourceLayer::new(&fixture.run)
        .reopen_active()
        .unwrap();
    let accepted_request: ResponsesRequest = serde_json::from_slice(
        &std::fs::read(evidence(fixture.root(), "fresh", "request-4.json")).unwrap(),
    )
    .unwrap();
    assert!(accepted_request.instructions.contains("accepted prompt B"));
    assert!(!accepted_request.instructions.contains("unaccepted draft C"));

    assert_eq!(
        std::fs::read_to_string(fixture.workspace().join("initializer-count.txt")).unwrap(),
        "x"
    );
    assert_eq!(
        std::fs::read_to_string(fixture.workspace().join("effect-count.txt")).unwrap(),
        "x"
    );
    let configuration = read_project_config(fixture.workspace()).unwrap().0;
    let accounts =
        SavedAccounts::beside(configuration.launch.embedded.unwrap().credential_file).unwrap();
    let selection = accounts.projection(accounts.launch_id().clone()).accounts;
    assert_eq!(
        first["selected_account"],
        serde_json::to_value(accounts.launch_id()).unwrap()
    );
    let second = selection
        .into_iter()
        .find(|account| account.label == "second@example.test")
        .unwrap()
        .id;
    let identity: harness::embedding::HostIdentity =
        serde_json::from_value(first["identity"].clone()).unwrap();
    let retained_final = fixture.settled(&identity, "fresh", &mut fresh).await;
    let selected = fixture
        .command(
            &first,
            harness::server::HostCommand::SelectAccount {
                target: identity.clone(),
                account_id: second.clone(),
            },
        )
        .await;
    let selected_receipt = fixture
        .command_receipt(&identity, selected, &mut fresh)
        .await;
    assert!(matches!(selected_receipt.outcome,
        harness::server::CommandReceiptOutcome::AccountSelected { target, account_id }
        if target == identity && account_id == second));
    assert_eq!(
        fixture.store().run_account(&identity.run).unwrap(),
        Some(second.clone())
    );
    let read_only = rusqlite::Connection::open_with_flags(
        fixture.run.join("harness/store.sqlite"),
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .unwrap();
    fresh.stop().await;
    assert!(evidence(fixture.root(), "fresh", "released.json").exists());
    crate::actor_host::HostIncarnationLease::verify_released(&fixture.run).unwrap();
    assert!(
        read_only
            .query_row("SELECT count(*) FROM requests", [], |row| row
                .get::<_, u64>(0))
            .unwrap()
            > 0
    );
    fixture.draft_c();
    resources.restart(fixture.root()).await;
    let mut crash = Process::start(fixture.root(), "crash", None);
    let restarted = fixture.ready("crash", &mut crash).await;
    let restart_identity = serde_json::from_value(restarted["identity"].clone()).unwrap();
    assert_eq!(
        fixture
            .settled(&restart_identity, "crash", &mut crash)
            .await,
        retained_final
    );
    assert_eq!(
        restarted["selected_account"],
        serde_json::to_value(&second).unwrap()
    );
    assert_eq!(
        source::SourceLayer::new(&fixture.run)
            .reopen_active()
            .unwrap()
            .identity,
        source_b.identity
    );
    assert!(
        !evidence(fixture.root(), "crash", "request-0.json").exists(),
        "settled conversation must await explicit input"
    );
    fixture
        .input(
            &restarted,
            "An effect may run before its provider stream ends.",
        )
        .await;
    tokio::time::timeout(DEADLINE, async {
        loop {
            crash.assert_live();
            if let Ok(error) =
                std::fs::read_to_string(evidence(fixture.root(), "crash", "failure.json"))
            {
                panic!("provider observed original failure: {error}");
            }
            if std::fs::read_to_string(fixture.workspace().join("effect-count.txt")).unwrap()
                == "xy"
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    let restarted_request: ResponsesRequest = serde_json::from_slice(
        &std::fs::read(evidence(fixture.root(), "crash", "request-0.json")).unwrap(),
    )
    .unwrap();
    assert!(
        restarted_request.instructions == accepted_request.instructions,
        "clean restart changed the exact accepted instructions"
    );
    let claims = fixture
        .store()
        .claims(&harness::model::CallId("crash-0".into()))
        .unwrap();
    assert_eq!(claims.len(), 1);
    let original = claims[0].operation.clone();
    let original_output = tokio::time::timeout(DEADLINE, async {
        loop {
            crash.assert_live();
            if let Ok(error) =
                std::fs::read_to_string(evidence(fixture.root(), "crash", "failure.json"))
            {
                panic!("provider observed original failure: {error}");
            }
            let store = fixture.store();
            if let Some(output) = store.replay_tool_output_operation(&original).unwrap() {
                assert!(matches!(
                    output.terminal,
                    harness::store::TerminalOutcome::Success
                ));
                let retained = store.claims_for_operation(&original).unwrap();
                assert_eq!(retained.len(), 1);
                assert_eq!(retained[0].state, harness::store::ClaimState::Settled);
                break output;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("original operation did not retain its completed result before the crash");
    let inflight = observed(fixture.root(), "crash", "inflight.json", &mut crash).await;
    let inflight_request: harness::model::RequestId =
        serde_json::from_value(inflight["request"].clone()).unwrap();
    let observed_request: ResponsesRequest = serde_json::from_slice(
        &std::fs::read(evidence(
            fixture.root(),
            "crash",
            &format!("request-{}.json", inflight["index"].as_u64().unwrap()),
        ))
        .unwrap(),
    )
    .unwrap();
    let original_model_output =
        projected_operation_output(&fixture.store(), &restart_identity, &original);
    assert_ne!(original_model_output, original_output.item);
    assert_eq!(
        observed_request
            .input
            .iter()
            .filter(|item| **item == original_model_output)
            .count(),
        1,
        "the in-flight request must contain the exact qualified original presentation once"
    );

    write_json(
        &fixture.root().join("issued-crash-operation.json"),
        &original,
    );
    crash.crash().await;
    std::fs::write(fixture.workspace().join("expected-effects.txt"), "xy").unwrap();
    let mut recovered = Process::start(fixture.root(), "recovered", None);
    let third = fixture.ready("recovered", &mut recovered).await;
    assert_eq!(
        third["selected_account"],
        serde_json::to_value(&second).unwrap()
    );
    let retained = fixture.store().claims_for_operation(&original).unwrap();
    assert_eq!(retained.len(), 1);
    assert_eq!(retained[0].state, harness::store::ClaimState::Settled);
    let restored_output = fixture
        .store()
        .replay_tool_output_operation(&original)
        .unwrap()
        .unwrap();
    assert_eq!(restored_output.item, original_output.item);
    assert!(matches!(
        restored_output.terminal,
        harness::store::TerminalOutcome::Success
    ));
    let successor: harness::embedding::HostIdentity =
        serde_json::from_value(third["identity"].clone()).unwrap();
    let harness::embedding::HostInferenceState::Blocked { block } = fixture
        .store()
        .embedded_inference_state(&successor)
        .unwrap()
    else {
        panic!("host loss must retain a durable inference block before explicit recovery");
    };
    assert_eq!(block.request, inflight_request);
    assert!(matches!(
        block.reason,
        harness::embedding::InferenceBlockReason::HostInterrupted
    ));
    let queued = fixture
        .input(
            &third,
            "Inspect retained B without replaying statements or initialization.",
        )
        .await;
    let admitted = fixture
        .command_receipt(&successor, queued, &mut recovered)
        .await;
    assert!(
        matches!(&admitted.outcome, harness::server::CommandReceiptOutcome::Admitted {
            target: Some(target),
            continuation: Some(harness::embedding::InputContinuation::Granted { block: granted }),
            wake_error: None,
            ..
        } if target == &successor && granted == &block),
        "ordinary Send must authorize exact continuation of the retained block: {:?}",
        admitted.outcome
    );
    observed(
        fixture.root(),
        "recovered",
        "request-0.json",
        &mut recovered,
    )
    .await;
    let retained = fixture.store().claims_for_operation(&original).unwrap();
    assert!(retained
        .iter()
        .all(|claim| claim.state == harness::store::ClaimState::Settled));
    let retained_output = fixture
        .store()
        .replay_tool_output_operation(&original)
        .unwrap()
        .unwrap();
    assert_eq!(retained_output.item, original_output.item);
    assert!(matches!(
        retained_output.terminal,
        harness::store::TerminalOutcome::Success
    ));
    observed(fixture.root(), "recovered", "final.json", &mut recovered).await;
    let resumed: ResponsesRequest = serde_json::from_slice(
        &std::fs::read(evidence(fixture.root(), "recovered", "request-0.json")).unwrap(),
    )
    .unwrap();
    assert!(
        resumed.instructions == accepted_request.instructions,
        "crash recovery changed the exact accepted instructions"
    );
    assert_eq!(
        resumed
            .input
            .iter()
            .filter(|item| item.0["call_id"] == "crash-0" && item.0["type"] == "custom_tool_call")
            .count(),
        1
    );
    let recovered_model_output =
        projected_operation_output(&fixture.store(), &successor, &original);
    assert_eq!(recovered_model_output, original_model_output);
    assert_eq!(
        resumed
            .input
            .iter()
            .filter(|item| **item == recovered_model_output)
            .count(),
        1
    );
    assert_eq!(
        std::fs::read_to_string(fixture.workspace().join("effect-count.txt")).unwrap(),
        "xy"
    );
    assert_eq!(
        std::fs::read_to_string(fixture.workspace().join("initializer-count.txt")).unwrap(),
        "x"
    );
    assert_eq!(
        source::SourceLayer::new(&fixture.run)
            .reopen_active()
            .unwrap()
            .identity,
        source_b.identity
    );
    recovered.stop().await;
}

#[tokio::test]
#[ignore = "requires the declared delegated user slice and isolated systemd service"]
async fn outer_host_rolls_split_binding_admission_forward_and_refuses_missing_issued_evidence() {
    let fixture = Fixture::new(true).await;
    let _resources = ResourceService::start(fixture.root()).await;
    for checkpoint in ["admitted", "store", "manifest", "bound", "released"] {
        let phase = format!("split-{checkpoint}");
        let mut process = Process::start(fixture.root(), &phase, Some(checkpoint));
        assert_eq!(process.wait().await.code(), Some(77), "inspect {phase}.log");
        assert_eq!(
            std::fs::read_to_string(evidence(fixture.root(), &phase, "checkpoint")).unwrap(),
            checkpoint
        );
    }
    let mut restored = Process::start(fixture.root(), "split-restored", None);
    fixture.ready("split-restored", &mut restored).await;
    assert_eq!(
        std::fs::read_to_string(fixture.workspace().join("initializer-count.txt")).unwrap(),
        "x"
    );
    restored.stop().await;
    let selected_source = source::SourceLayer::new(&fixture.run)
        .reopen_active()
        .unwrap()
        .identity;
    let side_record = fixture.run.join("workspace/active.json");
    std::fs::rename(
        &side_record,
        fixture
            .run
            .join("workspace/active-sidecar.acceptance-retained"),
    )
    .unwrap();
    let mut sidecar_recovered = Process::start(fixture.root(), "source-sidecar-recovered", None);
    fixture
        .ready("source-sidecar-recovered", &mut sidecar_recovered)
        .await;
    assert!(side_record.is_file());
    assert_eq!(
        source::SourceLayer::new(&fixture.run)
            .reopen_active()
            .unwrap()
            .identity,
        selected_source,
        "recovering a derived side record must preserve the selected publication"
    );
    assert_eq!(
        std::fs::read_to_string(fixture.workspace().join("initializer-count.txt")).unwrap(),
        "x"
    );
    assert!(!evidence(fixture.root(), "source-sidecar-recovered", "request-0.json").exists());
    sidecar_recovered.stop().await;
    let coordinates: RunLaunchCoordinates =
        serde_json::from_slice(&std::fs::read(fixture.run.join("launch.json")).unwrap()).unwrap();
    let run_journal = exomonad_journal_path(fixture.workspace(), &coordinates.run_id);
    for (name, paths) in [
        ("launch", vec![fixture.run.join("launch.json")]),
        ("source", vec![fixture.run.join("workspace/active")]),
        ("actors", vec![fixture.run.join("actor-lifecycle.v2.jsonl")]),
        ("run", vec![run_journal.clone()]),
        (
            "journals",
            vec![fixture.run.join("actor-lifecycle.v2.jsonl"), run_journal],
        ),
        ("store", vec![fixture.run.join("harness/store.sqlite")]),
        ("native-store", vec![fixture.run.join("recovery.sqlite")]),
    ] {
        let epoch = std::fs::read(fixture.run.join("host-incarnation.json")).unwrap();
        let retained = paths
            .iter()
            .map(|path| {
                let saved = path.with_extension("acceptance-retained");
                std::fs::rename(path, &saved).unwrap();
                (path.clone(), saved)
            })
            .collect::<Vec<_>>();
        let phase = format!("refusal-{name}");
        let mut refused = Process::start(fixture.root(), &phase, None);
        assert!(refused.wait().await.success(), "inspect {phase}.log");
        let receipt: Value = serde_json::from_slice(
            &std::fs::read(evidence(fixture.root(), &phase, "refusal.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(receipt["permanent"], true);
        assert!(!evidence(fixture.root(), &phase, "assembled.json").exists());
        assert!(!evidence(fixture.root(), &phase, "request-0.json").exists());
        for (path, saved) in retained {
            assert!(
                !path.exists(),
                "refusal recreated missing issued evidence {name}"
            );
            std::fs::rename(saved, path).unwrap();
        }
        if name == "launch" {
            assert_eq!(
                std::fs::read(fixture.run.join("host-incarnation.json")).unwrap(),
                epoch
            );
        }
    }
}

#[tokio::test]
#[ignore = "requires the declared delegated user slice and isolated systemd service"]
async fn outer_host_before_launch_admission_is_nonrunnable_without_allocating_an_epoch() {
    let fixture = Fixture::new(false).await;
    assert!(!fixture.run.join("launch.json").exists());
    assert!(!fixture.run.join("host-incarnation.json").exists());
    let mut refused = Process::start(fixture.root(), "refusal-unadmitted", None);
    assert!(refused.wait().await.success());
    assert!(!fixture.run.join("host-incarnation.json").exists());
    assert!(!fixture.run.join("launch.json").exists());
}

#[tokio::test]
#[ignore = "requires the declared delegated user slice and isolated systemd service"]
async fn outer_host_uncertain_initializer_refuses_before_successor_without_replaying_effects() {
    let fixture = Fixture::with_initializer_pause(true, true).await;
    let _resources = ResourceService::start(fixture.root()).await;
    let mut preparing = Process::start(fixture.root(), "initializer-preparing", None);
    tokio::time::timeout(DEADLINE, async {
        loop {
            preparing.assert_live();
            if std::fs::read_to_string(fixture.workspace().join("initializer-count.txt")).unwrap()
                == "x"
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("the real initializer must perform its independently observed effect");
    preparing.crash().await;
    let journal_path = fixture.run.join("actor-lifecycle.v2.jsonl");
    let before = std::fs::read(&journal_path).unwrap();
    let records = exomonad_actor::ActorRecoveryJournal::read_observed(&journal_path).unwrap();
    assert_eq!(records.len(), 1);
    let original = &records[0];
    assert!(matches!(
        original.initial_context.as_ref().unwrap().state,
        exomonad_actor::InitialContextState::Preparing
    ));
    let binding = std::fs::read(fixture.run.join("root-binding.json")).ok();
    write_json(
        &evidence(fixture.root(), "initializer-preparing", "issued.json"),
        &json!({
            "actor":original.admission.actor, "startup":original.startup,
            "initial_context":original.initial_context,
        }),
    );
    let mut refused = Process::start(fixture.root(), "refusal-uncertain-initializer", None);
    assert!(refused.wait().await.success());
    let receipt: Value = serde_json::from_slice(
        &std::fs::read(evidence(
            fixture.root(),
            "refusal-uncertain-initializer",
            "refusal.json",
        ))
        .unwrap(),
    )
    .unwrap();
    assert_eq!(receipt["permanent"], true);
    assert_eq!(std::fs::read(&journal_path).unwrap(), before, "refusal must preserve the exact prior actor and operation evidence without successor admission");
    assert_eq!(
        std::fs::read(fixture.run.join("root-binding.json")).ok(),
        binding
    );
    assert_eq!(
        std::fs::read_to_string(fixture.workspace().join("initializer-count.txt")).unwrap(),
        "x"
    );
    assert!(!evidence(
        fixture.root(),
        "refusal-uncertain-initializer",
        "assembled.json"
    )
    .exists());
    assert!(!evidence(
        fixture.root(),
        "refusal-uncertain-initializer",
        "request-0.json"
    )
    .exists());
}

#[tokio::test]
#[ignore = "requires the declared delegated user slice and isolated systemd service"]
async fn outer_host_pending_effect_after_crash_remains_blocked_without_replay() {
    let fixture = Fixture::new(true).await;
    let _resources = ResourceService::start(fixture.root()).await;
    let mut first = Process::start(fixture.root(), "uncertain", None);
    let ready = fixture.ready("uncertain", &mut first).await;
    fixture
        .input(
            &ready,
            "An admitted effect may have escaped before process loss.",
        )
        .await;
    tokio::time::timeout(DEADLINE, async {
        loop {
            first.assert_live();
            if std::fs::read_to_string(fixture.workspace().join("effect-count.txt")).unwrap() == "y"
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("the real external effect must be independently observable");
    let claims = fixture
        .store()
        .claims(&harness::model::CallId("uncertain-0".into()))
        .unwrap();
    assert_eq!(claims.len(), 1);
    assert_eq!(claims[0].state, harness::store::ClaimState::Pending);
    let original = claims[0].operation.clone();
    write_json(
        &fixture.root().join("issued-uncertain-operation.json"),
        &original,
    );
    first.crash().await;
    let mut restored = Process::start(fixture.root(), "uncertain-restored", None);
    let ready = fixture.ready("uncertain-restored", &mut restored).await;
    let identity: harness::embedding::HostIdentity =
        serde_json::from_value(ready["identity"].clone()).unwrap();
    let harness::embedding::HostInferenceState::Blocked { block } =
        fixture.store().embedded_inference_state(&identity).unwrap()
    else {
        panic!("the unresolved effect must remain blocked after host replacement");
    };
    assert_eq!(block.request, original.request);
    assert!(matches!(
        block.reason,
        harness::embedding::InferenceBlockReason::HostInterrupted
    ));
    let mut expected_block = block;
    for mode in [
        harness::embedding::RecoveryMode::Exact,
        harness::embedding::RecoveryMode::VisibleHistory,
    ] {
        let recovery = fixture
            .command(
                &ready,
                harness::server::HostCommand::Recover {
                    target: identity.clone(),
                    expected_block: expected_block.clone(),
                    mode,
                },
            )
            .await;
        let receipt = fixture
            .command_receipt(&identity, recovery, &mut restored)
            .await;
        assert!(matches!(
            &receipt.outcome,
            harness::server::CommandReceiptOutcome::ContinuationRefused {
                target,
                block: refused,
                reason: harness::embedding::ContinuationRefusal::UncertainEffects,
            } if target == &identity && refused == &expected_block
        ));
        let harness::embedding::HostInferenceState::Blocked { block: current } =
            fixture.store().embedded_inference_state(&identity).unwrap()
        else {
            panic!("exact and visible-history recovery must retain the uncertain-effect block");
        };
        assert_eq!(current.request, expected_block.request);
        assert_eq!(current.reason, expected_block.reason);
        assert_eq!(
            current.continuation_refusal,
            Some(harness::embedding::ContinuationRefusal::UncertainEffects)
        );
        if expected_block.continuation_refusal.is_none() {
            assert!(current.generation > expected_block.generation);
        } else {
            assert_eq!(current, expected_block);
        }
        expected_block = current;
    }
    let retained = fixture.store().claims_for_operation(&original).unwrap();
    assert_eq!(retained.len(), 1);
    assert_eq!(retained[0].state, harness::store::ClaimState::Pending);
    assert!(fixture
        .store()
        .replay_tool_output_operation(&original)
        .unwrap()
        .is_none());
    assert!(!evidence(fixture.root(), "uncertain-restored", "request-0.json").exists());
    assert_eq!(
        std::fs::read_to_string(fixture.workspace().join("effect-count.txt")).unwrap(),
        "y"
    );
    assert_eq!(
        std::fs::read_to_string(fixture.workspace().join("initializer-count.txt")).unwrap(),
        "x"
    );
    restored.stop().await;
}
