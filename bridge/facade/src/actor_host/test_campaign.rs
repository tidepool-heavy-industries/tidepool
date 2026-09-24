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
    deployments: tokio::sync::mpsc::Receiver<LocalResidentDeployment>,
    /// Deployments scanned by [`Self::next_deployment`] that did not match
    /// what the caller was awaiting. Parked here, in arrival order, rather
    /// than dropped, so a later call can still find them.
    pending: std::collections::VecDeque<LocalResidentDeployment>,
    pub root_installation: exomonad_actor::LocalResidentInstallation,
}

impl TestCampaign {
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
                    "{what}: timed out or the deployment channel closed; still pending: {:?}",
                    self.pending_kinds()
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
            pending: std::collections::VecDeque::new(),
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
    // best-effort: a global subscriber may already be installed.
    tracing_subscriber::fmt()
        .with_ansi(false)
        .with_thread_names(true)
        .with_env_filter(filter)
        .with_writer(std::sync::Mutex::new(file))
        .try_init()
        .ok();
}
