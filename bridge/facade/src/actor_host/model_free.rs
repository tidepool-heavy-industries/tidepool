//! The real resident driver without native providers, used by recipe checks and tests.

use super::*;

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

pub(super) struct ModelFreeSession {
    pub session_root: tempfile::TempDir,
    pub worktrees: WorktreeManager,
    pub bindings: Arc<Mutex<BindingTable>>,
    pub authority: ActorWorktreeAuthority,
    pub actor: exomonad_actor::LocalActorRef,
    pub forest: Arc<ResidentForest<ExomonadHandlerStack, CapturedOutput>>,
    pub _program: Arc<tidepool_runtime::session::CompiledTurn>,
    /// The same [`exomonad_actor::ChildSessionFactory`] installed on `forest`
    /// — kept accessible for tests that call it directly rather than
    /// through the (still-unwired) launch path.
    pub _child_session_factory:
        exomonad_actor::ChildSessionFactory<ExomonadHandlerStack, CapturedOutput>,
    pub hosted: tokio::task::JoinHandle<()>,
    pub deployments: tokio::sync::mpsc::Receiver<LocalResidentDeployment>,
    pub root_installation: exomonad_actor::LocalResidentInstallation,
}

impl ModelFreeSession {
    pub async fn start(
        config: &ActorHostConfig,
        transform: impl FnOnce(Arc<dyn ForkWorkspaceAdmission>) -> Arc<dyn ForkWorkspaceAdmission>,
    ) -> Result<Self> {
        Self::start_with_conversation(config, transform, None).await
    }

    /// As [`Self::start`], with a reader for the root's own conversation.
    /// Without one `reflect` reports every context unbound, so a test that
    /// needs real history supplies it here — the same seam the host uses at
    /// `actor_host.rs`'s `with_conversation_reader`.
    pub async fn start_with_conversation(
        config: &ActorHostConfig,
        transform: impl FnOnce(Arc<dyn ForkWorkspaceAdmission>) -> Arc<dyn ForkWorkspaceAdmission>,
        conversation: Option<exomonad_actor::ConversationReader>,
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
        let source_layers = super::source_service(config, session_root.path(), worktrees.clone());
        let (source, root, program, child_session_factory, image_registry) = compile_root(
            config,
            session_root.path(),
            worktrees.clone(),
            authority.clone(),
            source_layers.as_ref(),
            exomonad_actor::Incarnation::FIRST,
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
            exomonad_actor::Incarnation::FIRST,
            Some(worker_launch_resolver(config)),
        );
        let returned_child_session_factory = Arc::clone(&child_session_factory);
        let mut forest = forest
            .with_usage_pointers(exomonad_actor::UsagePointerTable::discover(
                &config.workspace,
            )?)
            .with_child_session_factory(child_session_factory)
            .with_child_bootstrap_program(Arc::clone(&program))
            .with_image_registry(image_registry);
        forest.set_jev_backend(super::jev_backend(config));
        if let Some(layers) = &source_layers {
            forest.set_source_layers(layers.clone());
        }
        if let Some(conversation) = conversation {
            forest = forest.with_conversation_reader(conversation);
        }
        let forest = Arc::new(forest);
        let (actor, hosted) = forest.admit_root(descriptor, outcome).await?;
        authority.install_grant(actor.identity().into(), ActorWorktreeGrant::Repository);
        if let Some(layers) = &source_layers {
            layers.bind_run(actor.identity().into());
        }
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
            _child_session_factory: returned_child_session_factory,
            hosted,
            deployments,
            root_installation: *root_installation,
        })
    }

    pub async fn shutdown(self) -> Result<()> {
        self.forest.shutdown().await;
        self.hosted.await?;
        Ok(())
    }
}
