//! Shared real resident setup for focused and recursive actor scenarios.

use super::*;

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

pub(super) struct TestCampaign {
    pub _repository: exomonad_worktree::testing::TestRepo,
    pub _runtime: tempfile::TempDir,
    pub session_root: tempfile::TempDir,
    pub worktrees: WorktreeManager,
    pub bindings: Arc<Mutex<BindingTable>>,
    pub authority: ActorWorktreeAuthority,
    pub actor: exomonad_actor::LocalActorRef,
    pub forest: Arc<ResidentForest<ExomonadHandlerStack, CapturedOutput>>,
    pub program: Arc<tidepool_runtime::session::CompiledTurn>,
    pub hosted: tokio::task::JoinHandle<()>,
    pub deployments: tokio::sync::mpsc::UnboundedReceiver<LocalResidentDeployment>,
    pub root_installation: exomonad_actor::LocalResidentInstallation,
}

impl TestCampaign {
    pub async fn await_watch_ready(&mut self) {
        tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                match self.deployments.recv().await {
                    Some(LocalResidentDeployment::WatchChanged { notification })
                        if notification.owner == self.actor.identity()
                            && notification.transition
                                == exomonad_actor::WatchTransition::Ready =>
                    {
                        return;
                    }
                    Some(_) => {}
                    None => panic!("deployment channel closed before watch readiness"),
                }
            }
        })
        .await
        .expect("watch readiness timed out");
    }

    pub async fn start() -> Self {
        Self::start_with_research_policy(exomonad_actor::ResearchPolicy::default()).await
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
            command_resources: None,
            exomonad_executable: std::env::current_exe().unwrap(),
            workspace_inputs: None,
            haskell_root: crate::haskell_sources::ensure_exomonad_haskell().unwrap(),
            workspace: repository.path().to_path_buf(),
            run_root: runtime.path().join("run"),
            root_binding_path: runtime.path().join("root-binding.json"),
            interactive_agent: exomonad_agent::native_interactive_agent_from_parts(
                std::env::current_exe().unwrap(),
                "test installation".into(),
            )
            .unwrap(),
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
            worktrees,
            bindings,
            authority,
            actor,
            forest,
            _program: program,
            hosted,
            deployments,
            root_installation,
        } = super::model_free::ModelFreeSession::start_with_conversation(
            &config,
            transform,
            conversation,
        )
        .await
        .unwrap();
        Self {
            _repository: repository,
            _runtime: runtime,
            session_root,
            worktrees,
            bindings,
            authority,
            actor,
            forest,
            program,
            hosted,
            deployments,
            root_installation,
        }
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
    let _ = tracing_subscriber::fmt()
        .with_ansi(false)
        .with_thread_names(true)
        .with_env_filter(filter)
        .with_writer(std::sync::Mutex::new(file))
        .try_init();
}
