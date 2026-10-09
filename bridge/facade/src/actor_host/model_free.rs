//! The real resident driver without native providers, used by recipe checks and tests.

use super::*;

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

// Cold semantic acceptance includes toolset compilation and native attachment.
// This bounded installation watchdog follows the hosted startup allowance;
// performance gates retain their own budgets.
const ROOT_POLICY_INSTALL_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(300);
const FAILED_START_FOREST_SHUTDOWN_TIMEOUT: std::time::Duration =
    std::time::Duration::from_secs(35);
const FAILED_START_HOSTED_JOIN_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

pub(super) struct ModelFreeSession {
    pub session_root: Arc<tempfile::TempDir>,
    #[cfg(test)]
    pub host_incarnation: Arc<HostIncarnationLease>,
    pub worktrees: WorktreeManager,
    pub bindings: Arc<Mutex<BindingTable>>,
    pub authority: ActorWorktreeAuthority,
    pub actor: exomonad_actor::LocalActorRef,
    pub forest: Arc<ResidentForest<ExomonadHandlerStack, CapturedOutput>>,
    pub _program: Arc<tidepool_runtime::session::CompiledTurn>,
    /// The same [`exomonad_actor::ChildSessionFactory`] installed on `forest`
    /// — retained for direct factory tests as well as opt-in launch tests.
    pub _child_session_factory:
        exomonad_actor::ChildSessionFactory<ExomonadHandlerStack, CapturedOutput>,
    pub hosted: tokio::task::JoinHandle<()>,
    pub deployments: tokio::sync::mpsc::Receiver<LocalResidentDeployment>,
    pub root_installation: exomonad_actor::LocalResidentInstallation,
}

impl ModelFreeSession {
    pub async fn start(
        config: &ActorHostConfig,
        transform: impl FnOnce(Arc<dyn WorkspaceAdmission>) -> Arc<dyn WorkspaceAdmission>,
    ) -> Result<Self> {
        Self::start_with_conversation(config, transform, None).await
    }

    /// As [`Self::start`], with a reader for the root's own conversation.
    /// Without one `reflect` reports every context unbound, so a test that
    /// needs real history supplies it here — the same seam the host uses at
    /// `actor_host.rs`'s `with_conversation_reader`.
    pub async fn start_with_conversation(
        config: &ActorHostConfig,
        transform: impl FnOnce(Arc<dyn WorkspaceAdmission>) -> Arc<dyn WorkspaceAdmission>,
        conversation: Option<exomonad_actor::ConversationReader>,
    ) -> Result<Self> {
        Self::start_with_model_factory(config, transform, conversation, None).await
    }

    pub(super) async fn start_with_model_factory(
        config: &ActorHostConfig,
        transform: impl FnOnce(Arc<dyn WorkspaceAdmission>) -> Arc<dyn WorkspaceAdmission>,
        conversation: Option<exomonad_actor::ConversationReader>,
        model_factory: Option<Arc<dyn exomonad_actor::CellModelFactory>>,
    ) -> Result<Self> {
        Self::start_configured(
            config,
            transform,
            conversation,
            model_factory,
            ROOT_POLICY_INSTALL_TIMEOUT,
            |forest, _, _| Ok(forest),
        )
        .await
    }

    /// Qualify the existing optional dedicated-machine factory with the same
    /// compiled bootstrap as the root. Default model-free hosts share a machine.
    #[cfg(test)]
    pub(super) async fn start_with_child_sessions(
        config: &ActorHostConfig,
        transform: impl FnOnce(Arc<dyn WorkspaceAdmission>) -> Arc<dyn WorkspaceAdmission>,
        conversation: Option<exomonad_actor::ConversationReader>,
        model_factory: Option<Arc<dyn exomonad_actor::CellModelFactory>>,
    ) -> Result<Self> {
        Self::start_configured(
            config,
            transform,
            conversation,
            model_factory,
            ROOT_POLICY_INSTALL_TIMEOUT,
            |forest, program, _| Ok(forest.with_child_bootstrap_program(program)),
        )
        .await
    }

    #[cfg(test)]
    pub(super) async fn start_with_form_host(
        config: &ActorHostConfig,
        transform: impl FnOnce(Arc<dyn WorkspaceAdmission>) -> Arc<dyn WorkspaceAdmission>,
        host: Arc<dyn exomonad_actor::FormHost>,
    ) -> Result<Self> {
        Self::start_configured(
            config,
            transform,
            None,
            None,
            ROOT_POLICY_INSTALL_TIMEOUT,
            |forest, _, _| Ok(forest.with_form_host(host)),
        )
        .await
    }

    async fn start_configured(
        config: &ActorHostConfig,
        transform: impl FnOnce(Arc<dyn WorkspaceAdmission>) -> Arc<dyn WorkspaceAdmission>,
        conversation: Option<exomonad_actor::ConversationReader>,
        model_factory: Option<Arc<dyn exomonad_actor::CellModelFactory>>,
        root_policy_install_timeout: std::time::Duration,
        configure_forest: impl FnOnce(
            ResidentForest<ExomonadHandlerStack, CapturedOutput>,
            Arc<tidepool_runtime::session::CompiledTurn>,
            &Path,
        )
            -> Result<ResidentForest<ExomonadHandlerStack, CapturedOutput>>,
    ) -> Result<Self> {
        let session_root = Arc::new(tempfile::tempdir()?);
        let session_directory =
            tidepool_atomic_write::DirectoryAnchor::open_existing(session_root.path())?;
        let host_incarnation = Arc::new(HostIncarnationLease::claim(&session_directory)?);
        let worktree_directory = config.run_directory.child("check-worktrees")?;
        let (worktrees, bindings) =
            actor_worktree_resources_at(&worktree_directory, &config.workspace)?;
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
            &session_directory,
            &worktree_directory,
            worktrees.clone(),
            authority.clone(),
            source_layers.as_ref(),
            Arc::clone(&host_incarnation),
            super::JournalOpenMode::Create,
            None,
        )?;
        let (descriptor, mut machine, entry) = root.into_parts();
        let outcome = match entry {
            exomonad_actor::ResidentRootEntry::Prepared(outcome) => outcome,
            exomonad_actor::ResidentRootEntry::Startup(entry) => {
                machine.run_startup_entry(entry)?
            }
        };
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
            exomonad_actor::Incarnation::FIRST,
        );
        let returned_child_session_factory = Arc::clone(&child_session_factory);
        let mut forest = forest
            .with_usage_pointers(exomonad_actor::UsagePointerTable::discover(
                &config.workspace,
            )?)
            .with_child_session_factory(child_session_factory)
            .with_handler_effect_support(
                tidepool_mcp::InstalledEffectSupport::installed_effect_support,
            )
            .with_image_registry(image_registry);
        // Bootstrap and optional host services are installed before admission,
        // so the actor derives its available effects from the actual instances.
        forest = configure_forest(forest, Arc::clone(&program), session_root.path())?;
        forest.set_jev_backend(super::jev_backend(config));
        if let Some(layers) = &source_layers {
            forest.set_source_layers(layers.clone());
        }
        if let Some(conversation) = conversation {
            forest = forest.with_conversation_reader(conversation);
        }
        if let Some(factory) = model_factory {
            forest = forest.with_cell_model_factory(factory);
        }
        let forest = Arc::new(forest);
        let (actor, hosted) = forest.admit_root(descriptor, outcome).await?;
        match forest
            .bind_durable_root_public_owner(actor.identity())
            .await
        {
            Ok(tidepool_runtime::session::PublicManifestCommit::Durable) => {}
            outcome => {
                return Err(failed_root_start_cleanup(
                    forest,
                    hosted,
                    format!("model-free root declaration ownership is unavailable: {outcome:?}"),
                )
                .await);
            }
        }
        authority.install_grant(actor.identity().into(), ActorWorktreeGrant::Repository);
        if let Some(layers) = &source_layers {
            layers.bind_run(actor.identity().into());
        }
        let root_installation = match tokio::time::timeout(
            root_policy_install_timeout,
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
                        "timed out after {root_policy_install_timeout:?} waiting for root policy installation"
                    ),
                )
                .await);
            }
        };
        Ok(Self {
            session_root,
            #[cfg(test)]
            host_incarnation,
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
    match tokio::time::timeout(FAILED_START_FOREST_SHUTDOWN_TIMEOUT, forest.shutdown()).await {
        Ok(outcomes) => {
            for outcome in outcomes.into_iter().filter(|outcome| !outcome.is_confirmed()) {
                details.push(format!("forest cleanup unconfirmed: {outcome:?}"));
            }
        }
        Err(_) => details.push(format!(
            "forest shutdown exceeded {FAILED_START_FOREST_SHUTDOWN_TIMEOUT:?}; completion is unconfirmed"
        )),
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
