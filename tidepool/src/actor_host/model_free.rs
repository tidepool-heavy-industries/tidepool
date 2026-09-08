//! The real resident driver without native providers, used by recipe checks and tests.

use super::*;

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

pub(super) struct ModelFreeSession {
    pub session_root: tempfile::TempDir,
    pub worktrees: WorktreeManager,
    pub bindings: Arc<Mutex<BindingTable>>,
    pub authority: ActorWorktreeAuthority,
    pub actor: tidepool_actor::LocalActorRef,
    pub forest: Arc<ResidentForest<ShoalHandlerStack, CapturedOutput>>,
    pub _program: Arc<tidepool_runtime::session::CompiledTurn>,
    pub hosted: tokio::task::JoinHandle<()>,
    pub deployments: tokio::sync::mpsc::UnboundedReceiver<LocalResidentDeployment>,
    pub root_installation: tidepool_actor::LocalResidentInstallation,
}

impl ModelFreeSession {
    pub async fn start(
        config: &ActorHostConfig,
        transform: impl FnOnce(Arc<dyn ForkWorkspaceAdmission>) -> Arc<dyn ForkWorkspaceAdmission>,
    ) -> Result<Self> {
        let session_root = tempfile::tempdir()?;
        let (worktrees, bindings) = actor_worktree_resources_at(
            &config.run_root.join("check-worktrees"),
            &config.workspace,
        )?;
        let bindings = Arc::new(Mutex::new(bindings));
        let authority = ActorWorktreeAuthority::new(
            runtime_namespace(session_root.path()),
            Arc::clone(&bindings),
        );
        let (source, root, program) = compile_root(
            config,
            session_root.path(),
            worktrees.clone(),
            authority.clone(),
        )?;
        let (descriptor, machine, outcome) = root.into_parts();
        let (forest, mut deployments) = ResidentForest::new_with_launch_resolver(
            source,
            descriptor.placement().session,
            machine,
            Some(transform(fork_workspace_admission(
                worktrees.clone(),
                authority.clone(),
                bindings.clone(),
                runtime_namespace(session_root.path()),
                None,
            ))),
            tidepool_actor::Incarnation::FIRST,
            Some(worker_launch_resolver(config)),
        );
        let forest = Arc::new(forest);
        let (actor, hosted) = forest.admit_root(descriptor, outcome).await?;
        authority.install_grant(actor.identity().into(), ActorWorktreeGrant::Repository);
        let Some(LocalResidentDeployment::PolicyInstalled(root_installation)) =
            deployments.recv().await
        else {
            forest.shutdown().await;
            hosted.await?;
            return Err(runtime_error(
                "root retired before installing its application",
            ));
        };
        Ok(Self {
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
        })
    }

    pub async fn shutdown(self) -> Result<()> {
        self.forest.shutdown().await;
        self.hosted.await?;
        Ok(())
    }
}
