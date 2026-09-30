//! The real resident driver without native providers, used by recipe checks and tests.

use super::*;

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

const ROOT_POLICY_INSTALL_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(60);
const FAILED_START_FOREST_SHUTDOWN_TIMEOUT: std::time::Duration =
    std::time::Duration::from_secs(35);
const FAILED_START_HOSTED_JOIN_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

pub(super) struct ModelFreeSession {
    pub session_root: Arc<tempfile::TempDir>,
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
        let session_root = Arc::new(tempfile::tempdir()?);
        let (worktrees, bindings) = actor_worktree_resources_at(
            &config.run_root.join("check-worktrees"),
            &config.workspace,
        )?;
        let bindings = Arc::new(Mutex::new(bindings));
        let authority = ActorWorktreeAuthority::new(
            runtime_namespace(session_root.path()),
            Arc::clone(&bindings),
        );
        let source_layers = super::source_service(
            config,
            session_root.path(),
            worktrees.clone(),
            crate::exomonad::source::SourceRootOwner::Temporary(Arc::clone(&session_root)),
        )?;
        let (source, root, program, child_session_factory, image_registry) = compile_root(
            config,
            session_root.path(),
            worktrees.clone(),
            authority.clone(),
            source_layers.as_ref(),
            exomonad_actor::Incarnation::FIRST,
            super::JournalOpenMode::Create,
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
                #[cfg(feature = "codex-compat")]
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
            .with_image_registry(image_registry);
        // No child bootstrap program: every launch stays on its launching
        // session, matching the run host.
        let _ = &program;
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
        let root_installation = match tokio::time::timeout(
            ROOT_POLICY_INSTALL_TIMEOUT,
            deployments.recv(),
        )
        .await
        {
            Ok(Some(LocalResidentDeployment::PolicyInstalled(installation)))
                if installation.actor.identity() == actor.identity() =>
            {
                *installation
            }
            Ok(Some(LocalResidentDeployment::PolicyInstalled(installation))) => {
                return Err(failed_root_start_cleanup(
                    forest,
                    hosted,
                    format!(
                        "received root policy installation for unexpected actor {:?}, expected {:?}",
                        installation.actor.identity(),
                        actor.identity()
                    ),
                )
                .await);
            }
            Ok(Some(LocalResidentDeployment::Retired { actor, terminal })) => {
                return Err(failed_root_start_cleanup(
                    forest,
                    hosted,
                    format!(
                        "root {actor:?} retired before installing its application: {terminal:?}"
                    ),
                )
                .await);
            }
            Ok(Some(deployment)) => {
                return Err(failed_root_start_cleanup(
                    forest,
                    hosted,
                    format!(
                        "received {} before root policy installation",
                        deployment.kind()
                    ),
                )
                .await);
            }
            Ok(None) => {
                return Err(failed_root_start_cleanup(
                    forest,
                    hosted,
                    "deployment channel closed before root policy installation".into(),
                )
                .await);
            }
            Err(_) => {
                return Err(failed_root_start_cleanup(
                    forest,
                    hosted,
                    format!(
                        "timed out after {ROOT_POLICY_INSTALL_TIMEOUT:?} waiting for root policy installation"
                    ),
                )
                .await);
            }
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
            root_installation,
        })
    }
}

async fn failed_root_start_cleanup(
    forest: Arc<ResidentForest<ExomonadHandlerStack, CapturedOutput>>,
    mut hosted: tokio::task::JoinHandle<()>,
    cause: String,
) -> Box<dyn std::error::Error> {
    let mut details = vec![cause];
    if tokio::time::timeout(FAILED_START_FOREST_SHUTDOWN_TIMEOUT, forest.shutdown())
        .await
        .is_err()
    {
        details.push(format!(
            "forest shutdown exceeded {FAILED_START_FOREST_SHUTDOWN_TIMEOUT:?}; completion is unconfirmed"
        ));
    }

    hosted.abort();
    match tokio::time::timeout(FAILED_START_HOSTED_JOIN_TIMEOUT, &mut hosted).await {
        Ok(Ok(())) => {}
        Ok(Err(error)) if error.is_cancelled() => {}
        Ok(Err(error)) => details.push(format!("joining hosted root after abort: {error}")),
        Err(_) => details.push(format!(
            "hosted root abort join exceeded {FAILED_START_HOSTED_JOIN_TIMEOUT:?}; completion is unconfirmed"
        )),
    }

    runtime_error(details.join("; "))
}
