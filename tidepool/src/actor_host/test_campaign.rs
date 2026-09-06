//! Shared real resident setup for focused and recursive actor scenarios.

use super::*;

pub(super) struct TestCampaign {
    pub _repository: tidepool_worktree::testing::TestRepo,
    pub _runtime: tempfile::TempDir,
    pub session_root: tempfile::TempDir,
    pub worktrees: WorktreeManager,
    pub bindings: Arc<Mutex<BindingTable>>,
    pub authority: ActorWorktreeAuthority,
    pub actor: tidepool_actor::LocalActorRef,
    pub forest: Arc<ResidentForest<ShoalHandlerStack, CapturedOutput>>,
    pub program: Arc<tidepool_runtime::session::CompiledTurn>,
    pub hosted: tokio::task::JoinHandle<()>,
    pub deployments: tokio::sync::mpsc::UnboundedReceiver<LocalResidentDeployment>,
    pub root_installation: tidepool_actor::LocalResidentInstallation,
}

impl TestCampaign {
    pub async fn await_watch_ready(&mut self) {
        tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                match self.deployments.recv().await {
                    Some(LocalResidentDeployment::WatchChanged { notification })
                        if notification.owner == self.actor.identity()
                            && notification.transition
                                == tidepool_actor::WatchTransition::Ready =>
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
        Self::start_with_research_policy(tidepool_actor::ResearchPolicy::default()).await
    }

    pub async fn start_with_research_policy(
        research_policy: tidepool_actor::ResearchPolicy,
    ) -> Self {
        Self::start_with_admission(research_policy, |admission| admission).await
    }

    pub async fn start_with_admission(
        research_policy: tidepool_actor::ResearchPolicy,
        transform: impl FnOnce(Arc<dyn ForkWorkspaceAdmission>) -> Arc<dyn ForkWorkspaceAdmission>,
    ) -> Self {
        Self::start_with_config(research_policy, transform, |_| {}).await
    }

    pub async fn start_with_config(
        research_policy: tidepool_actor::ResearchPolicy,
        transform: impl FnOnce(Arc<dyn ForkWorkspaceAdmission>) -> Arc<dyn ForkWorkspaceAdmission>,
        configure: impl FnOnce(&mut ActorHostConfig),
    ) -> Self {
        tidepool_testing::eval_harness::require_extract();
        let repository = tidepool_worktree::testing::TestRepo::init().unwrap();
        repository
            .writer()
            .commit_file("README.md", "source\n", "seed")
            .unwrap();
        let runtime = tempfile::tempdir().unwrap();
        let mut config = ActorHostConfig {
            haskell_root: crate::haskell_sources::ensure_shoal_haskell().unwrap(),
            workspace: repository.path().to_path_buf(),
            run_root: runtime.path().join("run"),
            root_binding_path: runtime.path().join("root-binding.json"),
            interactive_agent: tidepool_agent::native_interactive_agent_from_parts(
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
        };
        configure(&mut config);
        let session_root = tempfile::tempdir().expect("session root");
        let (worktrees, bindings) =
            actor_worktree_resources_at(&runtime.path().join("worktrees"), repository.path())
                .expect("worktree resources");
        let bindings = Arc::new(Mutex::new(bindings));
        let authority = ActorWorktreeAuthority::new(
            runtime_namespace(session_root.path()),
            Arc::clone(&bindings),
        );
        let (source, root, program) = compile_root(
            &config,
            session_root.path(),
            worktrees.clone(),
            authority.clone(),
        )
        .expect("compile permanent root");
        let (descriptor, machine, outcome) = root.into_parts();
        let (forest, mut deployments) = ResidentForest::new(
            source,
            descriptor.placement().session,
            machine,
            Some(transform(fork_workspace_admission(
                worktrees.clone(),
                authority.clone(),
                bindings.clone(),
                runtime_namespace(session_root.path()),
            ))),
            tidepool_actor::Incarnation::FIRST,
        );
        let forest = Arc::new(forest);
        let (actor, hosted) = forest
            .admit_root(descriptor, outcome)
            .await
            .expect("spawn permanent root");
        authority.install_root(actor.identity().into());
        let Some(LocalResidentDeployment::PolicyInstalled(root_installation)) =
            deployments.recv().await
        else {
            panic!("root retired before installing its application")
        };
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
