//! Shared real resident setup for focused and recursive actor scenarios.

use super::*;
use exomonad_tool::{
    ConversationOrigin, OriginalOperation, ToolArguments, ToolInvocation, ToolInvocationContext,
    ToolInvocationOrigin,
};

/// Dispatch and completion share one original operation; namespaces identify nested calls.
pub(super) fn original_tool_call(
    operation: OriginalOperation,
) -> (
    ToolInvocationContext,
    tidepool_runtime::session::ContextCheckpointBoundary,
) {
    let invocation = ToolInvocationContext {
        call_id: operation.call_id.clone(),
        origin: ToolInvocationOrigin::Model(operation.clone()),
        namespace: None,
    };
    (
        invocation,
        tidepool_runtime::session::ContextCheckpointBoundary::Hosted(operation),
    )
}

/// Require the structured receipt shape produced by an authored GHC source
/// rejection. Workbench receipts do not expose the full failure envelope, but
/// a compile-layer rejection with an authored error diagnostic comes from the
/// compiler's structured diagnostics path.
pub(super) fn require_ghc_compile_rejection(
    response: &serde_json::Value,
    required_fragments: &[&str],
) -> Result<(), String> {
    let items = response["items"]
        .as_array()
        .ok_or_else(|| "workbench rejection has no item receipts".to_owned())?;
    let found = items.iter().any(|item| {
        item["status"] == "rejected"
            && item["failureLayer"] == "compile"
            && item["diagnostics"].as_array().is_some_and(|diagnostics| {
                diagnostics.iter().any(|diagnostic| {
                    diagnostic["severity"] == "error"
                        && diagnostic["location"]["kind"] == "authored"
                        && diagnostic["message"].as_str().is_some_and(|message| {
                            required_fragments
                                .iter()
                                .all(|fragment| message.contains(fragment))
                        })
                })
            })
    });
    if found {
        Ok(())
    } else {
        Err(format!(
            "workbench response lacks an authored GHC compile error containing {required_fragments:?}: {response}"
        ))
    }
}

// Semantic acceptance includes cold whole-cell compilation, validation and
// native attachment in a debug build. Performance gates keep their own budgets.
pub(super) const COLD_DEBUG_CELL_SETTLEMENT_BUDGET: Duration = Duration::from_secs(300);

/// Commit everything a campaign's workspace carries before the run selects it.
///
/// Two mechanisms read the tree rather than the directory: `nix` resolves a
/// project's pinned Haskell source from its tracked `flake.nix`, and a child's
/// worktree admission refuses a dirty source repository. An installed workspace
/// package therefore gets committed, not merely written.
pub(super) fn commit_workspace(workspace: &std::path::Path) {
    let git = exomonad_worktree::GitCli::new();
    git.try_run(workspace, &["add", "--all", "--", "."])
        .unwrap();
    git.try_run(
        workspace,
        &[
            "-c",
            "commit.gpgsign=false",
            "commit",
            "--quiet",
            "-m",
            "workspace package",
        ],
    )
    .unwrap();
}

/// Select the shell record explicitly for tests of the hosted command tools.
pub(super) fn configure_shell_workspace(config: &mut ActorHostConfig) {
    let authored = config.workspace.join(".exomonad");
    std::fs::create_dir_all(&authored).unwrap();
    std::fs::write(
        authored.join("AgentSpec.hs"),
        tidepool_testing::fixture_source(
            "bridge/facade/src/actor_host/fixtures/shell_agent_spec.hs",
        ),
    )
    .unwrap();
    crate::exomonad::write_fixture_project_config(&authored, "test-model", |project| {
        project.haskell.source_roots = vec![".".into()];
        project.haskell.spec = Some("AgentSpec.agentSpec".into());
    });
    commit_workspace(&config.workspace);
    config.workspace_inputs = Some(
        crate::exomonad::workspace::FrozenWorkspace::load(
            &config.workspace,
            &config.run_directory.path(),
        )
        .unwrap(),
    );
}

/// Install the real notebook and lookup surfaces with Jev in the notebook row.
pub(super) fn configure_notebook_lookup_workspace(config: &mut ActorHostConfig) {
    let authored = config.workspace.join(".exomonad");
    std::fs::create_dir_all(&authored).unwrap();
    std::fs::write(
        authored.join("AgentSpec.hs"),
        tidepool_testing::fixture_source(
            "bridge/facade/src/actor_host/fixtures/notebook_lookup_agent_spec.hs",
        ),
    )
    .unwrap();
    crate::exomonad::write_fixture_project_config(&authored, "test-model", |project| {
        project.haskell.source_roots = vec![".".into()];
        project.haskell.modules = vec!["AgentSpec".into()];
        project.haskell.spec = Some("AgentSpec.agentSpec".into());
    });
    commit_workspace(&config.workspace);
    config.workspace_inputs = Some(
        crate::exomonad::workspace::FrozenWorkspace::load(
            &config.workspace,
            &config.run_directory.path(),
        )
        .unwrap(),
    );
}

/// Install a Jev-capable notebook row with a deterministic in-process backend.
pub(super) fn configure_notebook_jev_workspace(config: &mut ActorHostConfig) {
    config.jev = Some(Arc::new(FixtureJev) as exomonad_actor::JevBackendHandle);
    let authored = config.workspace.join(".exomonad");
    std::fs::create_dir_all(&authored).unwrap();
    std::fs::write(
        authored.join("AgentSpec.hs"),
        tidepool_testing::fixture_source(
            "bridge/facade/src/actor_host/fixtures/notebook_jev_agent_spec.hs",
        ),
    )
    .unwrap();
    crate::exomonad::write_fixture_project_config(&authored, "test-model", |project| {
        project.haskell.source_roots = vec![".".into()];
        project.haskell.modules = vec!["AgentSpec".into()];
        project.haskell.spec = Some("AgentSpec.agentSpec".into());
    });
    commit_workspace(&config.workspace);
    config.workspace_inputs = Some(
        crate::exomonad::workspace::FrozenWorkspace::load(
            &config.workspace,
            &config.run_directory.path(),
        )
        .unwrap(),
    );
}

pub(super) struct FixtureJev;

impl exomonad_actor::JevBackend for FixtureJev {
    fn ask(
        &self,
        _request: String,
    ) -> futures_util::future::BoxFuture<'_, Result<String, exomonad_actor::JevCallFailure>> {
        Box::pin(async {
            Ok(r#"{"model":"fixture","answers":{"value":{"type":"choice","choice":"yes","probabilities":{"yes":1.0,"no":0.0},"confidence":1.0}},"usage":{}}"#.into())
        })
    }
}

pub(super) struct TestCampaign {
    pub config: ActorHostConfig,
    pub _repository: exomonad_worktree::testing::TestRepo,
    pub _runtime: tempfile::TempDir,
    pub session_root: Arc<tempfile::TempDir>,
    host_incarnation: Arc<HostIncarnationLease>,
    pub worktrees: WorktreeManager,
    pub bindings: Arc<Mutex<BindingTable>>,
    pub authority: ActorWorktreeAuthority,
    pub actor: exomonad_actor::LocalActorRef,
    pub forest: Arc<ResidentForest<ExomonadHandlerStack, CapturedOutput>>,
    pub program: Arc<tidepool_runtime::session::CompiledTurn>,
    pub child_session_factory:
        exomonad_actor::ChildSessionFactory<ExomonadHandlerStack, CapturedOutput>,
    pub hosted: tokio::task::JoinHandle<()>,
    deployments: tokio::sync::mpsc::Receiver<LocalResidentDeployment>,
    /// Deployments scanned by [`Self::next_deployment`] that did not match
    /// what the caller was awaiting. Parked here, in arrival order, rather
    /// than dropped, so a later call can still find them.
    pending: std::collections::VecDeque<LocalResidentDeployment>,
    pub root_installation: exomonad_actor::LocalResidentInstallation,
}

enum CampaignRoot {
    Shared {
        conversation: Option<exomonad_actor::ConversationReader>,
        model_factory: Option<Arc<dyn exomonad_actor::CellModelFactory>>,
    },
    Dedicated,
    FormHost(Arc<dyn exomonad_actor::FormHost>),
}

impl TestCampaign {
    /// Drive the production durable output sink without creating a provider turn.
    /// Other deployments remain available to the campaign's existing consumers.
    pub async fn drive_actor_output<F: std::future::Future>(
        &mut self,
        store: &harness::store::Store,
        future: F,
    ) -> F::Output {
        let run = super::runtime_namespace(self.session_root.path());
        let mut index = 0;
        while index < self.pending.len() {
            if matches!(
                self.pending[index],
                LocalResidentDeployment::DisplayPublished(_)
            ) {
                let LocalResidentDeployment::DisplayPublished(request) =
                    self.pending.remove(index).unwrap()
                else {
                    unreachable!()
                };
                super::display_output::publish(&self.forest, store, &run, None, None, &request);
            } else {
                index += 1;
            }
        }
        tokio::pin!(future);
        loop {
            tokio::select! {
                result = &mut future => return result,
                event = self.deployments.recv() => match event.expect("campaign deployment channel closed while awaiting output") {
                    LocalResidentDeployment::DisplayPublished(request) => {
                        super::display_output::publish(&self.forest, store, &run, None, None, &request);
                    }
                    event => self.pending.push_back(event),
                }
            }
        }
    }

    /// A browser service for isolated Engine component fixtures. Hosted acceptance
    /// uses HostedTestRuntime and the production application entrypoint.
    pub async fn prepare_engine_component_service(
        &self,
        settings: &crate::exomonad::EmbeddedLaunchConfig,
    ) -> Result<super::embedded_service::EmbeddedService, String> {
        super::embedded_service::EmbeddedService::prepare_owned(
            self.session_root.path(),
            settings,
            Arc::clone(&self.host_incarnation),
        )
        .await
    }

    /// Await the next deployment matching `pick`, scanning previously parked
    /// deployments first (in arrival order) so legitimate interleaving with
    /// other deployment kinds never loses one. A deployment `pick` rejects
    /// is parked, never dropped, so a later call can still consume it.
    ///
    /// Panics naming `what` and the kinds still parked if the channel closes
    /// or `timeout` elapses first.
    pub async fn next_deployment<T>(
        &mut self,
        what: &str,
        timeout: Duration,
        pick: impl FnMut(LocalResidentDeployment) -> Result<T, LocalResidentDeployment>,
    ) -> T {
        self.next_deployment_opt(timeout, pick)
            .await
            .unwrap_or_else(|| {
                panic!(
                    "{what}: timed out or the deployment channel closed; still pending: {:?}{}",
                    self.pending_kinds(),
                    self.pending_retirements()
                )
            })
    }

    /// As [`Self::next_deployment`], but `None` on timeout or channel close
    /// instead of panicking — for callers that treat "nothing matched in
    /// time" as a legitimate outcome (e.g. asserting silence).
    pub async fn next_deployment_opt<T>(
        &mut self,
        timeout: Duration,
        mut pick: impl FnMut(LocalResidentDeployment) -> Result<T, LocalResidentDeployment>,
    ) -> Option<T> {
        let mut rescan = std::collections::VecDeque::new();
        while let Some(event) = self.pending.pop_front() {
            match pick(event) {
                Ok(value) => {
                    self.pending.extend(rescan);
                    return Some(value);
                }
                Err(event) => rescan.push_back(event),
            }
        }
        self.pending = rescan;
        tokio::time::timeout(timeout, async {
            loop {
                match self.deployments.recv().await {
                    Some(event) => match pick(event) {
                        Ok(value) => return Some(value),
                        Err(event) => self.pending.push_back(event),
                    },
                    None => return None,
                }
            }
        })
        .await
        .unwrap_or(None)
    }

    /// Assert that no parked or currently-buffered deployment matches
    /// `pick`. Draining leaves every non-matching deployment parked, never
    /// dropped.
    pub fn assert_no_deployment(
        &mut self,
        what: &str,
        mut pick: impl FnMut(&LocalResidentDeployment) -> bool,
    ) {
        if let Some(event) = self.pending.iter().find(|event| pick(event)) {
            panic!(
                "{what}: already parked a matching deployment: {}",
                event.kind()
            );
        }
        while let Ok(event) = self.deployments.try_recv() {
            if pick(&event) {
                panic!("{what}: published {}", event.kind());
            }
            self.pending.push_back(event);
        }
    }

    /// Drain every deployment parked or currently buffered, in arrival
    /// order, for assertions over everything published so far. Nothing is
    /// awaited: this never blocks on a deployment that has not arrived yet.
    pub fn drain_ready(&mut self) -> Vec<LocalResidentDeployment> {
        let mut drained: Vec<_> = self.pending.drain(..).collect();
        while let Ok(event) = self.deployments.try_recv() {
            drained.push(event);
        }
        drained
    }

    /// Kinds of every deployment currently parked, for diagnostics.
    /// Why each parked retirement ended, so a timeout names the cause
    /// instead of only the event kinds.
    fn pending_retirements(&self) -> String {
        self.pending
            .iter()
            .filter_map(|deployment| match deployment {
                LocalResidentDeployment::Retired { actor, terminal } => {
                    Some(format!("\n  {actor:?} retired: {terminal:?}"))
                }
                _ => None,
            })
            .collect()
    }

    pub fn pending_kinds(&self) -> Vec<&'static str> {
        self.pending
            .iter()
            .map(LocalResidentDeployment::kind)
            .collect()
    }

    /// Take exclusive, permanent ownership of the deployment channel away
    /// from this campaign, for a caller that will drain it itself for the
    /// rest of the test (e.g. a background command-backend responder
    /// spawned for the test's duration). `next_deployment` and friends must
    /// not be called on this campaign again afterward.
    pub fn take_deployments(&mut self) -> tokio::sync::mpsc::Receiver<LocalResidentDeployment> {
        assert!(
            self.pending.is_empty(),
            "deployments already parked: {:?}; drain them before detaching the channel",
            self.pending_kinds()
        );
        std::mem::replace(&mut self.deployments, tokio::sync::mpsc::channel(1).1)
    }

    pub async fn await_watch_ready(&mut self) {
        let owner = self.actor.identity();
        self.next_deployment(
            "watch readiness",
            Duration::from_secs(10),
            move |event| match event {
                LocalResidentDeployment::WatchChanged { notification }
                    if notification.owner == owner
                        && notification.transition == exomonad_actor::WatchTransition::Ready =>
                {
                    Ok(())
                }
                other => Err(other),
            },
        )
        .await
    }

    /// This campaign has no provider process. Acknowledge the exact spawn
    /// after its native policy and workspace attachment are installed.
    pub fn acknowledge_native_spawn(
        &self,
        installation: &exomonad_actor::LocalResidentInstallation,
    ) {
        let actor = installation.actor.identity();
        let admission = installation
            .spawn_admission
            .as_ref()
            .expect("native child retains its spawn admission");
        admission.validate_child(actor).unwrap();
        assert!(installation.actor.terminal().get().is_none());
        let installed_policy = installation
            .policy
            .snapshot_for_request()
            .expect("native tools and source are installed before spawn acknowledgement");
        assert_eq!(installation.policy.tools(), installed_policy.tools());
        if !installation.launch_worktrees.is_empty() {
            assert!(installation.worktree_custody.is_some());
            let principal = WorktreePrincipal::exact_actor(
                &runtime_namespace(self.session_root.path()),
                actor.id.0,
                actor.incarnation.0,
            );
            for worktree in &installation.launch_worktrees {
                let tree = self
                    .worktrees
                    .lookup(&WorktreeId::from_raw(worktree))
                    .unwrap()
                    .expect("native child workspace is registered");
                assert!(self
                    .bindings
                    .lock()
                    .membership(tree.id(), &principal)
                    .is_some());
            }
        }
        admission.acknowledge(actor).unwrap();
    }

    pub async fn start() -> Self {
        Self::start_with_admission(|admission| admission).await
    }

    pub(super) async fn start_with_form_host(host: Arc<dyn exomonad_actor::FormHost>) -> Self {
        Self::start_configured(|admission| admission, |_| {}, CampaignRoot::FormHost(host)).await
    }

    /// Opt into the existing dedicated-machine factory. Ordinary campaigns
    /// retain the production host's shared-machine policy.
    pub async fn start_with_child_sessions() -> Self {
        Self::start_configured(|admission| admission, |_| {}, CampaignRoot::Dedicated).await
    }

    pub async fn start_with_shell() -> Self {
        Self::start_with_config(|admission| admission, configure_shell_workspace).await
    }

    pub async fn start_with_admission(
        transform: impl FnOnce(Arc<dyn WorkspaceAdmission>) -> Arc<dyn WorkspaceAdmission>,
    ) -> Self {
        Self::start_with_config(transform, |_| {}).await
    }

    pub async fn start_with_config(
        transform: impl FnOnce(Arc<dyn WorkspaceAdmission>) -> Arc<dyn WorkspaceAdmission>,
        configure: impl FnOnce(&mut ActorHostConfig),
    ) -> Self {
        Self::start_with_conversation(transform, configure, None).await
    }

    /// Install the conversation reader before admission so the root admits
    /// Reflect. The reader may return turns or a typed unavailable result.
    pub async fn start_with_conversation(
        transform: impl FnOnce(Arc<dyn WorkspaceAdmission>) -> Arc<dyn WorkspaceAdmission>,
        configure: impl FnOnce(&mut ActorHostConfig),
        conversation: Option<exomonad_actor::ConversationReader>,
    ) -> Self {
        Self::start_with_model_factory(transform, configure, conversation, None).await
    }

    pub(super) async fn start_with_model_factory(
        transform: impl FnOnce(Arc<dyn WorkspaceAdmission>) -> Arc<dyn WorkspaceAdmission>,
        configure: impl FnOnce(&mut ActorHostConfig),
        conversation: Option<exomonad_actor::ConversationReader>,
        model_factory: Option<Arc<dyn exomonad_actor::CellModelFactory>>,
    ) -> Self {
        Self::start_configured(
            transform,
            configure,
            CampaignRoot::Shared {
                conversation,
                model_factory,
            },
        )
        .await
    }

    async fn start_configured(
        transform: impl FnOnce(Arc<dyn WorkspaceAdmission>) -> Arc<dyn WorkspaceAdmission>,
        configure: impl FnOnce(&mut ActorHostConfig),
        root: CampaignRoot,
    ) -> Self {
        tidepool_testing::eval_harness::require_extract();
        install_tracing();
        let repository = exomonad_worktree::testing::TestRepo::init().unwrap();
        repository
            .writer()
            .commit_file("README.md", "source\n", "seed")
            .unwrap();
        let runtime = tempfile::tempdir().unwrap();
        let mut config = ActorHostConfig {
            systemd_slice: None,
            source_exclude: Vec::new(),
            source_import: Default::default(),
            command_resources: None,
            exomonad_executable: std::env::current_exe().unwrap(),
            workspace_inputs: None,
            haskell_root: crate::haskell_sources::ensure_exomonad_haskell().unwrap(),
            workspace: repository.path().to_path_buf(),
            run_directory: tidepool_atomic_write::DirectoryAnchor::open_existing(runtime.path())
                .unwrap()
                .child("exomonad/runs/run")
                .unwrap(),
            root_binding_path: runtime.path().join("root-binding.json"),

            embedded: None,
            tmux_session: "unused-in-resident-test".into(),
            model: "test-model".into(),
            effort: ForkEffort::Low,

            pane_environment: BTreeMap::new(),
            jev: Some(exomonad_actor::unconfigured_jev()),
        };
        configure(&mut config);
        let super::model_free::ModelFreeSession {
            session_root,
            host_incarnation,
            worktrees,
            bindings,
            authority,
            actor,
            forest,
            _program: program,
            _child_session_factory: child_session_factory,
            hosted,
            deployments,
            root_installation,
        } = match root {
            CampaignRoot::Shared {
                conversation,
                model_factory,
            } => {
                super::model_free::ModelFreeSession::start_with_model_factory(
                    &config,
                    transform,
                    conversation,
                    model_factory,
                )
                .await
            }
            CampaignRoot::Dedicated => {
                super::model_free::ModelFreeSession::start_with_child_sessions(
                    &config, transform, None, None,
                )
                .await
            }
            CampaignRoot::FormHost(host) => {
                super::model_free::ModelFreeSession::start_with_form_host(&config, transform, host)
                    .await
            }
        }
        .unwrap();
        Self {
            config,
            _repository: repository,
            _runtime: runtime,
            session_root,
            host_incarnation,
            worktrees,
            bindings,
            authority,
            actor,
            forest,
            program,
            child_session_factory,
            hosted,
            deployments,
            pending: std::collections::VecDeque::new(),
            root_installation,
        }
    }
}

/// Retain phase and compile observations across the host's executor threads.
/// An isolated native test owns the global subscriber; an already-installed
/// measurement subscriber keeps its filter and writer. Explicit trace files
/// remain supported, otherwise libtest's writer retains JSON in test output.
pub(super) fn install_tracing() {
    let filter = tracing_subscriber::EnvFilter::new(
        "warn,tidepool::actor_host::startup=info,tidepool_runtime::compile=info,\
         tidepool_runtime::compile::modules=debug,tidepool_runtime::session::turn=info,\
         exomonad_harness::timing=debug,tidepool_runtime::prepared_install=info,\
         tidepool_codegen::prepared_compile=info,tidepool_extract_cmd::endpoint=debug,\
         exomonad_actor::workbench_phase=info,exomonad::content=off",
    );
    let subscriber = tracing_subscriber::fmt()
        .json()
        .with_ansi(false)
        .with_thread_names(true)
        .with_env_filter(filter)
        .with_span_events(
            tracing_subscriber::fmt::format::FmtSpan::NEW
                | tracing_subscriber::fmt::format::FmtSpan::CLOSE,
        )
        .with_test_writer();
    if let Some(path) = std::env::var_os("TIDEPOOL_TEST_TRACE") {
        let file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .unwrap_or_else(|error| panic!("TIDEPOOL_TEST_TRACE {path:?}: {error}"));
        subscriber
            .with_writer(std::sync::Mutex::new(file))
            .try_init()
            .ok();
    } else {
        subscriber.try_init().ok();
    }
}

pub(super) fn committed_display_text(response: &serde_json::Value) -> &str {
    assert_eq!(response["status"], "committed", "{response}");
    let items = response["items"].as_array().unwrap();
    for item in items {
        assert_eq!(item["status"], "committed", "{response}");
    }
    explicit_display_text(response)
}

pub(super) fn explicit_display_text(response: &serde_json::Value) -> &str {
    let items = response["items"].as_array().expect("cell item receipts");
    let displays = items
        .iter()
        .flat_map(|item| item["operations"].as_array().into_iter().flatten())
        .filter_map(|operation| operation.get("display"))
        .collect::<Vec<_>>();
    let [display] = displays.as_slice() else {
        panic!("expected one explicit display receipt: {response}");
    };
    assert!(
        display["output"]["sequence"].as_i64().is_some(),
        "{response}"
    );
    assert!(display["output"]["run"].as_str().is_some(), "{response}");
    display["text"].as_str().unwrap()
}

pub(super) async fn dispatch_haskell_script(
    endpoint: &dyn exomonad_actor::ResidentToolEndpoint,
    script: &str,
) -> serde_json::Value {
    dispatch_haskell_script_result(endpoint, script)
        .await
        .unwrap_or_else(|error| panic!("Haskell script failed:\n{script}\n\n{error}"))
}

pub(super) async fn dispatch_haskell_script_result(
    endpoint: &dyn exomonad_actor::ResidentToolEndpoint,
    script: &str,
) -> Result<serde_json::Value, exomonad_actor::ResidentToolError> {
    dispatch_haskell_script_response(endpoint, script)
        .await?
        .into_json()
        .map_err(exomonad_actor::ResidentToolError::Encoding)
}

pub(super) async fn dispatch_haskell_script_response(
    endpoint: &dyn exomonad_actor::ResidentToolEndpoint,
    script: &str,
) -> Result<exomonad_actor::ResidentToolResponse, exomonad_actor::ResidentToolError> {
    let call_id = uuid::Uuid::new_v4().simple().to_string();
    let (invocation, completion) = original_tool_call(OriginalOperation {
        origin: ConversationOrigin::External {
            thread_id: "actor-host-vertical".into(),
        },
        request_id: call_id.clone(),
        call_id,
    });
    let result = endpoint
        .dispatch_boxed(ToolInvocation {
            context: Some(invocation),
            name: exomonad_actor::HASKELL_TOOL.into(),
            arguments: ToolArguments::Raw(script.into()),
        })
        .await;
    endpoint
        .complete_boxed(completion)
        .await
        .expect("recorded tool completion");
    result
}

pub(super) async fn dispatch_structured_tool(
    endpoint: &dyn exomonad_actor::ResidentToolEndpoint,
    name: &str,
    arguments: serde_json::Value,
) -> serde_json::Value {
    let call_id = uuid::Uuid::new_v4().simple().to_string();
    let (invocation, completion) = original_tool_call(OriginalOperation {
        origin: ConversationOrigin::External {
            thread_id: "actor-host-vertical".into(),
        },
        request_id: call_id.clone(),
        call_id,
    });
    let result = endpoint
        .dispatch_json_boxed(ToolInvocation {
            context: Some(invocation),
            name: name.into(),
            arguments: ToolArguments::Structured(arguments),
        })
        .await
        .unwrap_or_else(|error| panic!("{name} tool failed: {error}"));
    endpoint
        .complete_boxed(completion)
        .await
        .expect("recorded tool completion");
    result
}

pub(super) async fn dispatch_lookup(
    endpoint: &dyn exomonad_actor::ResidentToolEndpoint,
    queries: &[&str],
) -> serde_json::Value {
    dispatch_structured_tool(
        endpoint,
        "lookup",
        serde_json::json!({ "queries": queries }),
    )
    .await
}

pub(super) async fn dispatch_status(
    endpoint: &dyn exomonad_actor::ResidentToolEndpoint,
    view: &str,
) -> serde_json::Value {
    dispatch_structured_tool(endpoint, "status", serde_json::json!({ "view": view })).await
}

/// The Jev surface is pinned source, not Tidepool library: a run reaches it
/// through a workspace whose `flake.nix` names the jev-dsl revision and whose
/// own `Jev/Operators.hs` fixes that library's JSON type to Tidepool's. These
/// tests select the package this repository ships, so what they compile is
/// what a project gets — including the pin.
pub(super) fn pinned_jev_workspace(config: &mut ActorHostConfig) {
    let package = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../exomonad/examples/workspace")
        .canonicalize()
        .expect("the Exomonad workspace package this repository ships");
    let authored = config.workspace.join(".exomonad");
    std::fs::create_dir_all(&authored).unwrap();
    crate::exomonad::write_fixture_project_config(&authored, "test-model", |project| {
        project.haskell.source_roots = vec![package.join(".exomonad")];
        project
            .haskell
            .flake_sources
            .insert("jev-dsl".into(), vec!["core".into()]);
    });
    for name in ["flake.nix", "flake.lock"] {
        std::fs::copy(package.join(name), config.workspace.join(name)).unwrap();
    }
    commit_workspace(&config.workspace);
    config.workspace_inputs = Some(
        crate::exomonad::workspace::FrozenWorkspace::load(
            &config.workspace,
            &config.run_directory.path(),
        )
        .expect("resolve the pinned Haskell source"),
    );
}

/// A scripted provider reply. This observes requests from the real host; it
/// neither attaches an actor nor constructs a tool installation.
pub(super) struct HostedScriptRound {
    pub request: harness::transport::ResponsesRequest,
    reply: tokio::sync::oneshot::Sender<harness::transport::ResponsesTurn>,
}

impl HostedScriptRound {
    pub fn origin(&self) -> harness::model::ConversationIdentity {
        let (prefix, incarnation) = self.request.session_id.rsplit_once(':').unwrap();
        let (run, actor) = prefix.rsplit_once(':').unwrap();
        harness::model::ConversationIdentity::Embedded {
            run: run.into(),
            actor: harness::model::AgentPath(actor.into()),
            incarnation: incarnation.into(),
        }
    }

    pub fn call(self, call_id: &str, source: &str) {
        let advertised = self
            .request
            .tools
            .iter()
            .map(|tool| {
                format!(
                    "{} ({})",
                    tool["name"].as_str().unwrap_or("unnamed"),
                    tool["type"].as_str().unwrap_or("unknown kind")
                )
            })
            .collect::<Vec<_>>();
        assert!(
            self.request
                .tools
                .iter()
                .any(|tool| tool["name"] == "haskell_sync" && tool["type"] == "custom"),
            "scripted cell label {call_id:?} sends a custom_tool_call named `haskell_sync`; \
             the issuing request advertised [{}]",
            advertised.join(", ")
        );
        self.reply
            .send(harness::transport::ResponsesTurn {
                response_id: format!("script-{call_id}"),
                items: vec![harness::item::Item(serde_json::json!({
                    "type":"custom_tool_call", "call_id":call_id,
                    "name":"haskell_sync", "input":source,
                }))],
                usage: Default::default(),
            })
            .expect("production provider request remains live");
    }

    pub fn function(self, call_id: &str, name: &str, arguments: serde_json::Value) {
        self.reply
            .send(harness::transport::ResponsesTurn {
                response_id: format!("script-{call_id}"),
                items: vec![harness::item::Item(serde_json::json!({
                    "type": "function_call", "call_id": call_id, "name": name,
                    "arguments": serde_json::to_string(&arguments).unwrap(),
                }))],
                usage: Default::default(),
            })
            .expect("production provider request remains live");
    }

    pub fn settled_output(&self, call_id: &str) -> serde_json::Value {
        serde_json::from_str(self.settled_text(call_id)).unwrap()
    }

    pub fn settled_text(&self, call_id: &str) -> &str {
        let output = self
            .request
            .input
            .iter()
            .find(|item| {
                matches!(
                    item.0["type"].as_str(),
                    Some("custom_tool_call_output" | "function_call_output")
                ) && item.0["call_id"] == call_id
            })
            .unwrap_or_else(|| panic!("{call_id}: actual provider request has no settled output"));
        output.0["output"].as_str().unwrap()
    }

    pub fn finish(self) {
        self.reply
            .send(harness::transport::ResponsesTurn {
                response_id: uuid::Uuid::new_v4().simple().to_string(),
                items: vec![harness::item::Item(serde_json::json!({
                    "type":"message", "role":"assistant", "phase":"final_answer",
                    "content":[{"type":"output_text","text":"fixture complete"}],
                }))],
                usage: Default::default(),
            })
            .expect("production provider request remains live");
    }

    pub fn assert_committed(&self, call_id: &str) {
        let output = self
            .request
            .input
            .iter()
            .find(|item| {
                item.0["type"] == "custom_tool_call_output" && item.0["call_id"] == call_id
            })
            .unwrap_or_else(|| panic!("{call_id}: actual provider request has no settled output"));
        let value: serde_json::Value =
            serde_json::from_str(output.0["output"].as_str().unwrap()).unwrap();
        assert!(
            matches!(value["status"].as_str(), Some("completed" | "committed")),
            "{value}"
        );
        assert!(
            value["items"]
                .as_array()
                .unwrap()
                .iter()
                .all(|item| item["status"] == "committed"),
            "{value}"
        );
    }

    pub fn assert_failure(&self, call_id: &str, expected: &str) {
        let output = self
            .request
            .input
            .iter()
            .find(|item| {
                item.0["type"] == "custom_tool_call_output" && item.0["call_id"] == call_id
            })
            .unwrap_or_else(|| panic!("{call_id}: actual provider request has no failure output"));
        let value: serde_json::Value =
            serde_json::from_str(output.0["output"].as_str().unwrap()).unwrap();
        assert!(
            !matches!(value["status"].as_str(), Some("completed" | "committed")),
            "{value}"
        );
        assert!(value.to_string().contains(expected), "{value}");
    }

    pub fn assert_value(&self, call_id: &str, expected: &str) {
        let output = self
            .request
            .input
            .iter()
            .find(|item| {
                item.0["type"] == "custom_tool_call_output" && item.0["call_id"] == call_id
            })
            .unwrap_or_else(|| panic!("{call_id}: actual provider request has no settled output"));
        let value: serde_json::Value =
            serde_json::from_str(output.0["output"].as_str().unwrap()).unwrap();
        assert!(
            matches!(value["status"].as_str(), Some("completed" | "committed")),
            "{value}"
        );
        let item = value["items"].as_array().unwrap().last().unwrap();
        assert_eq!(item["status"], "committed", "{value}");
        assert_eq!(explicit_display_text(&value), expected, "{value}");
    }
}

struct HostedScriptProvider(tokio::sync::mpsc::UnboundedSender<HostedScriptRound>);

#[async_trait::async_trait]
impl harness::engine::ResponsesTransport for HostedScriptProvider {
    async fn create(
        &self,
        request: harness::transport::ResponsesRequest,
    ) -> Result<harness::transport::ResponsesTurn, harness::transport::TransportError> {
        let (reply, response) = tokio::sync::oneshot::channel();
        self.0
            .send(HostedScriptRound { request, reply })
            .map_err(|_| {
                harness::transport::TransportError::Stream("script observer closed".into())
            })?;
        response.await.map_err(|_| {
            harness::transport::TransportError::Stream("script abandoned the provider reply".into())
        })
    }
}

pub(super) fn hosted_script_provider() -> (
    Arc<dyn harness::engine::ResponsesTransport>,
    tokio::sync::mpsc::UnboundedReceiver<HostedScriptRound>,
) {
    let (requests, receiver) = tokio::sync::mpsc::unbounded_channel();
    (Arc::new(HostedScriptProvider(requests)), receiver)
}

/// Preserve interleaved provider requests while one actor's reply is awaited.
pub(super) async fn next_hosted_script_round(
    requests: &mut tokio::sync::mpsc::UnboundedReceiver<HostedScriptRound>,
    pending: &mut std::collections::VecDeque<HostedScriptRound>,
    actor: &harness::model::AgentPath,
) -> HostedScriptRound {
    let matches = |round: &HostedScriptRound| match round.origin() {
        harness::model::ConversationIdentity::Embedded { actor: path, .. } => &path == actor,
        _ => false,
    };
    if let Some(index) = pending.iter().position(matches) {
        return pending.remove(index).unwrap();
    }
    tokio::time::timeout(COLD_DEBUG_CELL_SETTLEMENT_BUDGET, async {
        loop {
            let round = requests
                .recv()
                .await
                .expect("production provider requests closed");
            if matches(&round) {
                return round;
            }
            pending.push_back(round);
        }
    })
    .await
    .unwrap_or_else(|_| {
        let retained_origins: Vec<_> = pending.iter().map(HostedScriptRound::origin).collect();
        panic!(
            "production actor {actor:?} did not request its next scripted reply; \
             retained provider request origins: {retained_origins:?}"
        )
    })
}

pub(super) fn hosted_test_settings(
    files: &tempfile::TempDir,
    concurrent_jobs: usize,
) -> crate::exomonad::EmbeddedLaunchConfig {
    let assets = files.path().join("assets");
    std::fs::create_dir_all(&assets).unwrap();
    std::fs::write(assets.join("index.html"), "<!doctype html>").unwrap();
    let session_secret_file = files.path().join("session-secret");
    std::fs::write(&session_secret_file, "hosted-script-test-secret-32-bytes").unwrap();
    let credential_file = files.path().join("codex-auth.json");
    std::fs::write(&credential_file, "{}").unwrap();
    crate::exomonad::EmbeddedLaunchConfig {
        listen: "127.0.0.1:0".parse().unwrap(),
        public_origin_scheme: crate::exomonad::EmbeddedPublicOriginScheme::Https,
        public_origin: None,
        asset_root: assets,
        browser_auth: crate::exomonad::EmbeddedBrowserAuth::Secret,
        session_secret_file: Some(session_secret_file),
        provider: crate::exomonad::EmbeddedModelProvider::Codex,
        credential_file,
        context_capacity_tokens: 200_000,
        concurrent_jobs,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ghc_compile_rejection_requires_authored_error_diagnostic_data() {
        let response = serde_json::json!({
            "items": [{
                "status": "rejected",
                "failureLayer": "compile",
                "diagnostics": [{
                    "severity": "error",
                    "location": {"kind": "authored", "label": "<cell>"},
                    "message": "Couldn't match type Bool with Int"
                }]
            }]
        });
        assert!(require_ghc_compile_rejection(&response, &["Bool", "Int"]).is_ok());

        let mut wrong_layer = response.clone();
        wrong_layer["items"][0]["failureLayer"] = serde_json::json!("install");
        assert!(require_ghc_compile_rejection(&wrong_layer, &["Bool", "Int"]).is_err());

        let mut wrong_location = response.clone();
        wrong_location["items"][0]["diagnostics"][0]["location"]["kind"] =
            serde_json::json!("foreign");
        assert!(require_ghc_compile_rejection(&wrong_location, &["Bool", "Int"]).is_err());

        let mut wrong_severity = response.clone();
        wrong_severity["items"][0]["diagnostics"][0]["severity"] = serde_json::json!("warning");
        assert!(require_ghc_compile_rejection(&wrong_severity, &["Bool", "Int"]).is_err());

        let mut wrong_message = response.clone();
        wrong_message["items"][0]["diagnostics"][0]["message"] =
            serde_json::json!("Couldn't match type Char with Word");
        assert!(require_ghc_compile_rejection(&wrong_message, &["Bool", "Int"]).is_err());

        let mut missing_diagnostics = response;
        missing_diagnostics["items"][0]
            .as_object_mut()
            .unwrap()
            .remove("diagnostics");
        assert!(require_ghc_compile_rejection(&missing_diagnostics, &["Bool", "Int"]).is_err());
    }
    use harness::transport::{ResponsesRequest, TransportError};

    fn request() -> ResponsesRequest {
        ResponsesRequest {
            input: Vec::new(),
            instructions: String::new(),
            tools: Default::default(),
            tools_allowed: None,
            model: "test-model".into(),
            pinned_effort: harness::model::Effort::Low,
            session_id: "test-run:/root:1".into(),
        }
    }

    #[tokio::test]
    async fn dropped_script_observer_returns_transport_error() {
        let (provider, requests) = hosted_script_provider();
        drop(requests);
        let result = provider.create(request()).await;
        assert!(matches!(
            result,
            Err(TransportError::Stream(message)) if message == "script observer closed"
        ));
    }

    #[tokio::test]
    async fn dropped_held_script_reply_returns_transport_error() {
        let (provider, mut requests) = hosted_script_provider();
        let pending = tokio::spawn(async move { provider.create(request()).await });
        let held = requests
            .recv()
            .await
            .expect("provider publishes its request");
        drop(held);
        let result = tokio::time::timeout(Duration::from_secs(5), pending)
            .await
            .expect("abandoned reply settles")
            .expect("provider returns without panicking");
        assert!(matches!(
            result,
            Err(TransportError::Stream(message)) if message == "script abandoned the provider reply"
        ));
    }

    #[tokio::test]
    async fn live_script_reply_still_completes_provider_request() {
        let (provider, mut requests) = hosted_script_provider();
        let pending = tokio::spawn(async move { provider.create(request()).await });
        requests
            .recv()
            .await
            .expect("provider publishes its request")
            .finish();
        let result = tokio::time::timeout(Duration::from_secs(5), pending)
            .await
            .expect("scripted reply settles")
            .expect("provider returns without panicking")
            .expect("live script reply succeeds");
        assert_eq!(result.items.len(), 1);
        assert_eq!(result.items[0].0["type"], "message");
        assert_eq!(result.items[0].0["phase"], "final_answer");
    }

    #[tokio::test]
    async fn scripted_cell_label_does_not_replace_the_declared_tool_name() {
        let (reply, response) = tokio::sync::oneshot::channel();
        let mut request = request();
        request.tools = vec![serde_json::json!({
            "type": "custom",
            "name": "haskell_sync",
            "description": "Run a cell"
        })]
        .into();
        HostedScriptRound { request, reply }.call("prepared-children", "display True");
        let response = response.await.expect("script replies on its declared tool");
        assert_eq!(response.items[0].0["call_id"], "prepared-children");
        assert_eq!(response.items[0].0["name"], "haskell_sync");
    }

    #[tokio::test]
    async fn scripted_cell_refuses_an_unregistered_tool_before_provider_admission() {
        let (reply, response) = tokio::sync::oneshot::channel();
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            HostedScriptRound {
                request: request(),
                reply,
            }
            .call("prepared-children", "display True");
        }));
        assert!(
            result.is_err(),
            "the fixture must reject its mismatched tool locally"
        );
        assert!(response.await.is_err(), "no invalid tool call was sent");
    }
}
