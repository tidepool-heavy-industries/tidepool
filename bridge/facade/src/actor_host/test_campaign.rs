//! Shared real resident setup for focused and recursive actor scenarios.

use super::*;
use exomonad_tool::{ToolArguments, ToolInvocation, ToolInvocationContext};

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
        include_str!("fixtures/shell_agent_spec.hs"),
    )
    .unwrap();
    std::fs::write(
        authored.join("config.toml"),
        "[defaults]\nmodel='test-model'\n[haskell]\nsource_roots=['.']\nspec='AgentSpec.agentSpec'\n",
    )
    .unwrap();
    commit_workspace(&config.workspace);
    config.workspace_inputs = Some(
        crate::exomonad::workspace::FrozenWorkspace::load(&config.workspace, &config.run_root)
            .unwrap(),
    );
}

/// Install the real notebook and lookup surfaces with Jev in the notebook row.
pub(super) fn configure_notebook_lookup_workspace(config: &mut ActorHostConfig) {
    let authored = config.workspace.join(".exomonad");
    std::fs::create_dir_all(&authored).unwrap();
    std::fs::write(
        authored.join("AgentSpec.hs"),
        include_str!("fixtures/notebook_lookup_agent_spec.hs"),
    )
    .unwrap();
    std::fs::write(
        authored.join("config.toml"),
        "[defaults]\nmodel='test-model'\n[haskell]\nsource_roots=['.']\nmodules=['AgentSpec']\nspec='AgentSpec.agentSpec'\n",
    )
    .unwrap();
    commit_workspace(&config.workspace);
    config.workspace_inputs = Some(
        crate::exomonad::workspace::FrozenWorkspace::load(&config.workspace, &config.run_root)
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
        include_str!("fixtures/notebook_jev_agent_spec.hs"),
    )
    .unwrap();
    std::fs::write(
        authored.join("config.toml"),
        "[defaults]\nmodel='test-model'\n[haskell]\nsource_roots=['.']\nmodules=['AgentSpec']\nspec='AgentSpec.agentSpec'\n",
    )
    .unwrap();
    commit_workspace(&config.workspace);
    config.workspace_inputs = Some(
        crate::exomonad::workspace::FrozenWorkspace::load(&config.workspace, &config.run_root)
            .unwrap(),
    );
}

struct FixtureJev;

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

    /// Attach the browser service to the resident campaign's existing run owner.
    pub async fn prepare_embedded_service(
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

    pub async fn start() -> Self {
        Self::start_with_research_policy(exomonad_actor::ResearchPolicy::default()).await
    }

    pub async fn start_with_shell() -> Self {
        Self::start_with_config(
            exomonad_actor::ResearchPolicy::default(),
            |admission| admission,
            configure_shell_workspace,
        )
        .await
    }

    pub async fn start_with_research_policy(
        research_policy: exomonad_actor::ResearchPolicy,
    ) -> Self {
        Self::start_with_admission(research_policy, |admission| admission).await
    }

    pub async fn start_with_admission(
        research_policy: exomonad_actor::ResearchPolicy,
        transform: impl FnOnce(Arc<dyn ForkWorkspaceAdmission>) -> Arc<dyn ForkWorkspaceAdmission>,
    ) -> Self {
        Self::start_with_config(research_policy, transform, |_| {}).await
    }

    pub async fn start_with_config(
        research_policy: exomonad_actor::ResearchPolicy,
        transform: impl FnOnce(Arc<dyn ForkWorkspaceAdmission>) -> Arc<dyn ForkWorkspaceAdmission>,
        configure: impl FnOnce(&mut ActorHostConfig),
    ) -> Self {
        Self::start_with_conversation(research_policy, transform, configure, None).await
    }

    /// A campaign whose root can read its own conversation, so `reflect`
    /// returns the supplied turns instead of reporting the context unbound.
    pub async fn start_with_conversation(
        research_policy: exomonad_actor::ResearchPolicy,
        transform: impl FnOnce(Arc<dyn ForkWorkspaceAdmission>) -> Arc<dyn ForkWorkspaceAdmission>,
        configure: impl FnOnce(&mut ActorHostConfig),
        conversation: Option<exomonad_actor::ConversationReader>,
    ) -> Self {
        Self::start_with_model_factory(research_policy, transform, configure, conversation, None)
            .await
    }

    pub(super) async fn start_with_model_factory(
        research_policy: exomonad_actor::ResearchPolicy,
        transform: impl FnOnce(Arc<dyn ForkWorkspaceAdmission>) -> Arc<dyn ForkWorkspaceAdmission>,
        configure: impl FnOnce(&mut ActorHostConfig),
        conversation: Option<exomonad_actor::ConversationReader>,
        model_factory: Option<Arc<dyn exomonad_actor::CellModelFactory>>,
    ) -> Self {
        tidepool_testing::eval_harness::require_extract();
        install_trace_file();
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
            run_root: runtime.path().join("run"),
            root_binding_path: runtime.path().join("root-binding.json"),
            backend: test_backend_options(),
            embedded: None,
            tmux_session: "unused-in-resident-test".into(),
            model: "test-model".into(),
            effort: ReasoningEffort::Low,
            research_policy,
            root_launch_mode: InteractiveLaunchMode::Fresh,
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
        } = super::model_free::ModelFreeSession::start_with_model_factory(
            &config,
            transform,
            conversation,
            model_factory,
        )
        .await
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

fn test_backend_options() -> crate::exomonad::HostBackendOptions {
    #[cfg(feature = "codex-compat")]
    {
        crate::exomonad::HostBackendOptions::Codex(
            exomonad_agent::native_interactive_agent_from_parts(
                std::env::current_exe().unwrap(),
                "test installation".into(),
            )
            .unwrap(),
        )
    }
    #[cfg(not(feature = "codex-compat"))]
    {
        crate::exomonad::HostBackendOptions::Embedded
    }
}

/// Diagnostic tracing for a campaign run: with `TIDEPOOL_TEST_TRACE=<path>`,
/// every `tidepool*` target at debug level is appended to that file (the
/// filter can be replaced through `RUST_LOG`). Unset, nothing is installed.
fn install_trace_file() {
    let Some(path) = std::env::var_os("TIDEPOOL_TEST_TRACE") else {
        return;
    };
    let file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .unwrap_or_else(|error| panic!("TIDEPOOL_TEST_TRACE {path:?}: {error}"));
    let filter = tracing_subscriber::EnvFilter::try_from_env("RUST_LOG").unwrap_or_else(|_| {
        tracing_subscriber::EnvFilter::new(
            "warn,tidepool=debug,exomonad_actor=debug,tidepool_runtime=debug",
        )
    });
    // A second campaign in the same process keeps the first subscriber.
    // best-effort: a global subscriber may already be installed.
    tracing_subscriber::fmt()
        .with_ansi(false)
        .with_thread_names(true)
        .with_env_filter(filter)
        .with_writer(std::sync::Mutex::new(file))
        .try_init()
        .ok();
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
    let call_id = uuid::Uuid::new_v4().simple().to_string();
    let result = endpoint
        .dispatch_boxed(ToolInvocation {
            context: Some(ToolInvocationContext::external(
                "actor-host-vertical".into(),
                call_id.clone(),
                call_id.clone(),
                Some(call_id.clone()),
                Some("haskell".into()),
            )),
            name: exomonad_actor::HASKELL_TOOL.into(),
            arguments: ToolArguments::Raw(script.into()),
        })
        .await;
    endpoint
        .complete_boxed(tidepool_runtime::session::WorkbenchForkBoundary::external(
            "actor-host-vertical".into(),
            call_id.clone(),
            call_id,
        ))
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
    let result = endpoint
        .dispatch_boxed(ToolInvocation {
            context: Some(ToolInvocationContext::external(
                "actor-host-vertical".into(),
                call_id.clone(),
                call_id.clone(),
                Some(call_id.clone()),
                Some(name.into()),
            )),
            name: name.into(),
            arguments: ToolArguments::Structured(arguments),
        })
        .await
        .unwrap_or_else(|error| panic!("{name} tool failed: {error}"));
    endpoint
        .complete_boxed(tidepool_runtime::session::WorkbenchForkBoundary::external(
            "actor-host-vertical".into(),
            call_id.clone(),
            call_id,
        ))
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
    std::fs::write(
        authored.join("config.toml"),
        format!(
            "[defaults]\nmodel = 'test-model'\n\n[haskell]\nsource_roots = ['{}']\n\n[haskell.flake_sources]\njev-dsl = ['core']\n",
            package.join(".exomonad").display()
        ),
    )
    .unwrap();
    for name in ["flake.nix", "flake.lock"] {
        std::fs::copy(package.join(name), config.workspace.join(name)).unwrap();
    }
    commit_workspace(&config.workspace);
    config.workspace_inputs = Some(
        crate::exomonad::workspace::FrozenWorkspace::load(&config.workspace, &config.run_root)
            .expect("resolve the pinned Haskell source"),
    );
}
