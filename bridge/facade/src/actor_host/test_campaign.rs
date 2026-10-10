//! Shared real resident setup for focused and recursive actor scenarios.

use super::*;
use exomonad_tool::{
    ConversationOrigin, OriginalOperation, ToolArguments, ToolInvocation, ToolInvocationContext,
    ToolInvocationOrigin,
};
use serde_json::Value;

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
    executor: CampaignExecutor,
    shutdown_observations: Vec<exomonad_actor::ForestRootShutdown>,
    settled: bool,
    deployments: tokio::sync::mpsc::Receiver<LocalResidentDeployment>,
    /// Deployments scanned by [`Self::next_deployment`] that did not match
    /// what the caller was awaiting. Parked here, in arrival order, rather
    /// than dropped, so a later call can still find them.
    pending: std::collections::VecDeque<LocalResidentDeployment>,
    pub root_installation: exomonad_actor::LocalResidentInstallation,
}

enum CampaignExecutor {
    Running(tokio::task::JoinHandle<()>),
    Aborting(tokio::task::JoinHandle<()>),
    Settled(CampaignHostedJoin),
}

/// Executor completion and resource cleanup are independent observations.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum CampaignHostedJoin {
    Joined,
    Failed(String),
    TimedOut,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum CampaignRootRetirement {
    Settled(exomonad_actor::ResidentShutdown),
    Unconfirmed {
        actor: exomonad_actor::ActorRef,
        terminal: Option<exomonad_actor::ActorTerminal>,
    },
}

/// Every forest outcome remains visible, including failed earlier observations.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct CampaignShutdown {
    pub forest: Vec<exomonad_actor::ForestRootShutdown>,
    pub hosted: CampaignHostedJoin,
    pub root: CampaignRootRetirement,
}

impl CampaignShutdown {
    pub fn is_confirmed(&self) -> bool {
        self.forest.iter().rev().find_map(|outcome| match outcome {
            exomonad_actor::ForestRootShutdown::RunResources(outcome) => Some(outcome),
            _ => None,
        }) == Some(&exomonad_actor::CleanupComponentOutcome::Confirmed)
            && self
                .forest
                .iter()
                .filter(|outcome| {
                    !matches!(outcome, exomonad_actor::ForestRootShutdown::RunResources(_))
                })
                .all(exomonad_actor::ForestRootShutdown::is_confirmed)
            && self.hosted == CampaignHostedJoin::Joined
            && matches!(&self.root, CampaignRootRetirement::Settled(shutdown) if shutdown.cleanup.is_confirmed())
    }
}

impl Drop for TestCampaign {
    fn drop(&mut self) {
        // Drop cannot acknowledge asynchronous resource cleanup. On assertion
        // unwinding preserve the original panic; run_scenario owns settlement.
        if !self.settled && !std::thread::panicking() {
            panic!("TestCampaign left scope without consuming shutdown or run_scenario");
        }
    }
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
    /// Observe a deliberate earlier retirement without giving up campaign
    /// ownership. Recovery scenarios can keep using the forest afterwards.
    pub async fn observe_hosted_completion(&mut self) -> Result<(), CampaignHostedJoin> {
        loop {
            let observation = match &mut self.executor {
                CampaignExecutor::Running(task) => {
                    match tokio::time::timeout(Duration::from_secs(30), &mut *task).await {
                        Ok(Ok(())) => CampaignHostedJoin::Joined,
                        Ok(Err(error)) => CampaignHostedJoin::Failed(error.to_string()),
                        Err(_) => {
                            task.abort();
                            // The executor remains owned across cancellation of
                            // either wait. No observation can lose its handle.
                            let CampaignExecutor::Running(task) = std::mem::replace(
                                &mut self.executor,
                                CampaignExecutor::Settled(CampaignHostedJoin::TimedOut),
                            ) else {
                                unreachable!()
                            };
                            self.executor = CampaignExecutor::Aborting(task);
                            continue;
                        }
                    }
                }
                CampaignExecutor::Aborting(task) => {
                    let _ = tokio::time::timeout(Duration::from_secs(5), &mut *task).await;
                    CampaignHostedJoin::TimedOut
                }
                CampaignExecutor::Settled(observation) => observation.clone(),
            };
            self.executor = CampaignExecutor::Settled(observation.clone());
            return match observation {
                CampaignHostedJoin::Joined => Ok(()),
                failure => Err(failure),
            };
        }
    }

    /// Retain the actual forest owner's outcomes when a scenario must inspect
    /// retirement effects before its final consuming shutdown.
    pub async fn observe_forest_shutdown(&mut self) -> Vec<exomonad_actor::ForestRootShutdown> {
        let outcomes = self.forest.shutdown().await;
        self.shutdown_observations.extend(outcomes.iter().cloned());
        outcomes
    }

    pub async fn observe_shutdown(&mut self) -> Result<CampaignShutdown, CampaignShutdown> {
        self.observe_forest_shutdown().await;
        let _ = self.observe_hosted_completion().await;
        let terminal = self.actor.terminal();
        let root = match (terminal.get(), terminal.cleanup()) {
            (Some(terminal), Some(cleanup)) => {
                CampaignRootRetirement::Settled(exomonad_actor::ResidentShutdown {
                    terminal,
                    cleanup,
                })
            }
            (terminal, _) => CampaignRootRetirement::Unconfirmed {
                actor: self.actor.identity(),
                terminal,
            },
        };
        let observation = CampaignShutdown {
            forest: self.shutdown_observations.clone(),
            hosted: match &self.executor {
                CampaignExecutor::Settled(observation) => observation.clone(),
                _ => unreachable!("executor completion was observed"),
            },
            root,
        };
        if observation.is_confirmed() {
            Ok(observation)
        } else {
            Err(observation)
        }
    }

    /// The consuming owner is the only path that acknowledges final teardown.
    pub async fn shutdown(mut self) -> Result<CampaignShutdown, CampaignShutdown> {
        let observation = self.observe_shutdown().await;
        self.settled = true;
        observation
    }

    /// Settle assertions and cleanup through the same owner as hosted tests.
    pub async fn run_scenario<R>(
        self,
        scenario: impl for<'a> FnOnce(&'a mut Self) -> futures_util::future::LocalBoxFuture<'a, R>,
    ) -> R {
        self.run_scenario_with_cleanup(scenario, |observation| {
            observation.expect("model-free campaign cleanup is confirmed");
        })
        .await
    }

    /// A refusal scenario still consumes teardown, then checks the exact typed
    /// failure. Successful cleanup cannot silently satisfy a negative case.
    pub async fn run_scenario_expecting_cleanup_failure<R>(
        self,
        scenario: impl for<'a> FnOnce(&'a mut Self) -> futures_util::future::LocalBoxFuture<'a, R>,
        check_failure: impl FnOnce(&CampaignShutdown),
    ) -> R {
        self.run_scenario_with_cleanup(scenario, |observation| {
            check_failure(&observation.expect_err("scenario must leave cleanup unconfirmed"));
        })
        .await
    }

    async fn run_scenario_with_cleanup<R>(
        self,
        scenario: impl for<'a> FnOnce(&'a mut Self) -> futures_util::future::LocalBoxFuture<'a, R>,
        check_cleanup: impl FnOnce(Result<CampaignShutdown, CampaignShutdown>),
    ) -> R {
        let campaign = tokio::sync::Mutex::new(Some(self));
        let observation = std::cell::RefCell::new(None);
        let (scenario, cleanup, report_errors) = super::hosted_test_context::settle_scenario(
            async {
                let mut owner = campaign.lock().await;
                scenario(owner.as_mut().expect("scenario owns its campaign")).await
            },
            async {
                let owner = campaign
                    .lock()
                    .await
                    .take()
                    .expect("cleanup consumes its campaign");
                let result = owner.shutdown().await;
                let settled = result
                    .as_ref()
                    .map(|_| ())
                    .map_err(|failure| format!("{failure:?}"));
                observation.replace(Some(result));
                settled
            },
            |_, _| Ok(()),
        )
        .await;
        if let Err(error) = &cleanup {
            eprintln!("model-free campaign cleanup failed: {error}");
        }
        let result = match scenario {
            Ok(result) => result,
            Err(payload) => std::panic::resume_unwind(payload),
        };
        let observation = observation
            .into_inner()
            .unwrap_or_else(|| panic!("model-free campaign cleanup did not settle: {cleanup:?}"));
        check_cleanup(observation);
        assert!(report_errors.is_empty(), "{report_errors:?}");
        result
    }

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
            executor: CampaignExecutor::Running(hosted),
            shutdown_observations: Vec::new(),
            settled: false,
            deployments,
            pending: std::collections::VecDeque::new(),
            root_installation,
        }
    }
}

pub(super) fn campaign_trace_profile() -> &'static str {
    match std::env::var("TIDEPOOL_TEST_TRACE_PROFILE").as_deref() {
        Ok("minimal") => "minimal",
        Ok("full") | Err(_) => "full",
        Ok(other) => panic!("unsupported TIDEPOOL_TEST_TRACE_PROFILE {other:?}"),
    }
}

fn campaign_trace_filter() -> tracing_subscriber::EnvFilter {
    campaign_trace_filter_for(campaign_trace_profile())
}

fn campaign_trace_filter_for(profile: &str) -> tracing_subscriber::EnvFilter {
    let directives = match profile {
        "minimal" => {
            "warn,tidepool::actor_host::startup=info,exomonad_harness::timing=debug,\
                      exomonad_actor::request=info,exomonad_actor::workbench_phase=info,\
                      exomonad::content=off"
        }
        _ => {
            "warn,tidepool::actor_host::startup=info,tidepool_runtime::compile=info,\
             tidepool_runtime::compile::modules=debug,tidepool_runtime::session::turn=info,\
             exomonad_harness::timing=debug,tidepool_runtime::prepared_install=info,\
             tidepool_codegen::prepared_compile=info,tidepool_extract_cmd::endpoint=debug,\
             exomonad_actor::request=info,tidepool_toolchain::module_candidates=debug,\
             tidepool_toolchain::artifacts=info,exomonad_actor::workbench_phase=info,\
             exomonad_actor::call_timing=info,exomonad_actor::resident_actor=info,\
             exomonad_actor::resident_tools=info,exomonad::content=off,harness::runtime_cost=debug"
        }
    };
    tracing_subscriber::EnvFilter::new(directives)
}

/// Retain phase and compile observations across the host's executor threads.
/// An isolated native test owns the global subscriber; an already-installed
/// measurement subscriber keeps its filter and writer. Explicit trace files
/// remain supported, otherwise libtest's writer retains JSON in test output.
pub(super) fn install_tracing() {
    let subscriber = tracing_subscriber::fmt()
        .json()
        .with_ansi(false)
        .with_thread_names(true)
        .with_env_filter(campaign_trace_filter())
        .with_span_events(if campaign_trace_profile() == "full" {
            tracing_subscriber::fmt::format::FmtSpan::NEW
                | tracing_subscriber::fmt::format::FmtSpan::CLOSE
        } else {
            tracing_subscriber::fmt::format::FmtSpan::NONE
        })
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

pub(super) fn prepare_performance_traces() -> (
    std::path::PathBuf,
    std::path::PathBuf,
    std::path::PathBuf,
    Value,
) {
    let artifact_root = std::path::PathBuf::from(
        std::env::var_os("TIDEPOOL_TEST_ARTIFACT_ROOT")
            .expect("owned-resident counted runner supplies per-case artifacts"),
    );
    assert!(
        artifact_root.is_absolute(),
        "per-case artifact root is absolute"
    );
    let host_trace = artifact_root.join("host.jsonl");
    let compiler_trace = artifact_root.join("compiler/compiler.jsonl");
    let phase_trace = artifact_root.join("phases.jsonl");
    std::fs::File::create(&phase_trace).expect("create owned phase JSONL");
    let lifecycle: Value = serde_json::from_slice(
        &std::fs::read(artifact_root.join("compiler/lifecycle.json"))
            .expect("owned compiler runner records its live daemon identity before the test"),
    )
    .expect("owned compiler lifecycle record is JSON");
    assert_eq!(
        lifecycle["cleanup_confirmed"], false,
        "daemon is live for the case"
    );
    assert!(
        compiler_trace.is_file(),
        "owned daemon created its JSONL trace"
    );
    std::env::set_var("TIDEPOOL_TEST_TRACE", &host_trace);
    std::env::set_var("TIDEPOOL_TEST_TRACE_PROFILE", campaign_trace_profile());
    std::env::set_var("TIDEPOOL_PERFORMANCE_COMPILER_TRACE", &compiler_trace);
    (host_trace, compiler_trace, phase_trace, lifecycle)
}

pub(super) fn record_phase(path: &std::path::Path, record: Value) {
    let mut phases = std::fs::OpenOptions::new()
        .append(true)
        .open(path)
        .expect("open owned phase JSONL");
    serde_json::to_writer(&mut phases, &record).expect("serialize phase JSON");
    std::io::Write::write_all(&mut phases, b"\n").expect("terminate phase JSONL record");
    println!("harness-usecase {record}");
}

/// A scripted provider reply. This observes requests from the real host; it
/// neither attaches an actor nor constructs a tool installation.
pub(super) struct HostedScriptRound {
    pub request: harness::transport::ResponsesRequest,
    request_id: Option<harness::model::RequestId>,
    reply: tokio::sync::oneshot::Sender<harness::transport::ResponsesTurn>,
    request_started_at: std::time::Instant,
}

impl HostedScriptRound {
    pub fn operation(&self, call_id: &str) -> harness::model::OperationId {
        harness::model::OperationId {
            origin: self.origin(),
            request: self
                .request_id
                .clone()
                .expect("Engine supplies the exact durable provider request"),
            call: harness::model::CallId(call_id.into()),
        }
    }
    /// Time spent after the scripted transport request began and before this
    /// test supplies its deterministic response. This is harness coordination,
    /// not model-provider latency.
    pub fn scripted_response_hold_ns(&self) -> u128 {
        self.request_started_at.elapsed().as_nanos()
    }

    pub fn origin(&self) -> harness::model::ConversationIdentity {
        let identity = self.host_identity();
        harness::model::ConversationIdentity::Embedded {
            run: identity.run,
            actor: identity.actor,
            incarnation: identity.incarnation,
        }
    }

    pub fn host_identity(&self) -> harness::embedding::HostIdentity {
        let (prefix, incarnation) = self.request.session_id.rsplit_once(':').unwrap();
        let (run, actor) = prefix.rsplit_once(':').unwrap();
        harness::embedding::HostIdentity {
            run: run.into(),
            actor: harness::model::AgentPath(actor.into()),
            incarnation: incarnation.into(),
        }
    }

    fn assert_advertised(&self, call_id: &str, name: &str, kind: &str) {
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
                .any(|tool| tool["name"] == name && tool["type"] == kind),
            "scripted call {call_id:?} sends {kind} tool {name:?}; the issuing request advertised [{}]",
            advertised.join(", ")
        );
    }

    pub fn call(self, call_id: &str, source: &str) {
        self.cell(call_id, source, harness::item::ToolExecution::Synchronous);
    }

    pub fn async_call(self, call_id: &str, source: &str) {
        self.cell(call_id, source, harness::item::ToolExecution::Asynchronous);
    }

    fn cell(self, call_id: &str, source: &str, execution: harness::item::ToolExecution) {
        use harness::item::ToolExecution;
        let name = match execution {
            ToolExecution::Synchronous => "haskell_sync",
            ToolExecution::Asynchronous => "haskell",
        };
        self.assert_advertised(call_id, name, "custom");
        let call = harness::item::Item(serde_json::json!({
            "type":"custom_tool_call", "call_id":call_id, "name":name, "input":source,
            "async": execution == ToolExecution::Asynchronous,
        }));
        assert_eq!(call.tool_call().unwrap().unwrap().execution, execution);
        self.reply
            .send(harness::transport::ResponsesTurn {
                response_id: format!("script-{call_id}"),
                items: vec![call],
                usage: Default::default(),
            })
            .expect("production provider request remains live");
    }

    pub fn function(self, call_id: &str, name: &str, arguments: serde_json::Value) {
        self.assert_advertised(call_id, name, "function");
        let call = harness::item::Item(serde_json::json!({
            "type": "function_call", "call_id": call_id, "name": name,
            "arguments": serde_json::to_string(&arguments).unwrap(),
        }));
        assert_eq!(
            call.tool_call().unwrap().unwrap().execution,
            harness::item::ToolExecution::Synchronous
        );
        self.reply
            .send(harness::transport::ResponsesTurn {
                response_id: format!("script-{call_id}"),
                items: vec![call],
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

impl HostedScriptProvider {
    async fn round(
        &self,
        request_id: Option<harness::model::RequestId>,
        request: harness::transport::ResponsesRequest,
    ) -> Result<harness::transport::ResponsesTurn, harness::transport::TransportError> {
        let request_started_at = std::time::Instant::now();
        let (reply, response) = tokio::sync::oneshot::channel();
        self.0
            .send(HostedScriptRound {
                request,
                request_id,
                reply,
                request_started_at,
            })
            .map_err(|_| {
                harness::transport::TransportError::Stream("script observer closed".into())
            })?;
        response.await.map_err(|_| {
            harness::transport::TransportError::Stream("script abandoned the provider reply".into())
        })
    }
}

#[async_trait::async_trait]
impl harness::engine::ResponsesTransport for HostedScriptProvider {
    async fn create(
        &self,
        request: harness::transport::ResponsesRequest,
    ) -> Result<harness::transport::ResponsesTurn, harness::transport::TransportError> {
        self.round(None, request).await
    }

    async fn create_streaming_for_request(
        &self,
        request_id: &harness::model::RequestId,
        request: harness::transport::ResponsesRequest,
        sink: tokio::sync::mpsc::Sender<harness::transport::sse::StreamEvent>,
    ) -> Result<harness::transport::ResponsesTurn, harness::transport::TransportError> {
        let turn = self.round(Some(request_id.clone()), request).await?;
        for item in &turn.items {
            let _ = sink
                .send(harness::transport::sse::StreamEvent::ItemDone(item.clone()))
                .await;
        }
        Ok(turn)
    }
}

pub(super) fn hosted_script_provider() -> (
    Arc<dyn harness::engine::ResponsesTransport>,
    tokio::sync::mpsc::UnboundedReceiver<HostedScriptRound>,
) {
    let (requests, receiver) = tokio::sync::mpsc::unbounded_channel();
    (Arc::new(HostedScriptProvider(requests)), receiver)
}

#[derive(Clone, Copy, Debug)]
pub(super) enum HostedScriptSelection<'a> {
    Actor(&'a harness::model::AgentPath),
    OtherThan(&'a harness::model::AgentPath),
}

impl HostedScriptSelection<'_> {
    fn matches(self, round: &HostedScriptRound) -> bool {
        let origin = round.origin();
        match self {
            Self::Actor(actor) => origin.actor() == actor,
            Self::OtherThan(actor) => origin.actor() != actor,
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
enum HostedScriptWaitFailure {
    Deadline,
    ProviderClosed,
}

async fn select_hosted_script_round_with_budget(
    requests: &mut tokio::sync::mpsc::UnboundedReceiver<HostedScriptRound>,
    pending: &mut std::collections::VecDeque<HostedScriptRound>,
    selection: HostedScriptSelection<'_>,
    budget: Duration,
) -> Result<HostedScriptRound, HostedScriptWaitFailure> {
    if let Some(index) = pending.iter().position(|round| selection.matches(round)) {
        return Ok(pending.remove(index).unwrap());
    }
    tokio::time::timeout(budget, async {
        loop {
            let round = requests
                .recv()
                .await
                .ok_or(HostedScriptWaitFailure::ProviderClosed)?;
            if selection.matches(&round) {
                return Ok(round);
            }
            pending.push_back(round);
        }
    })
    .await
    .map_err(|_| HostedScriptWaitFailure::Deadline)?
}

/// Select the oldest matching round and retain every nonmatch in arrival order.
/// Deadline and EOF diagnostics name the selection and all retained origins.
pub(super) async fn select_hosted_script_round(
    requests: &mut tokio::sync::mpsc::UnboundedReceiver<HostedScriptRound>,
    pending: &mut std::collections::VecDeque<HostedScriptRound>,
    selection: HostedScriptSelection<'_>,
) -> HostedScriptRound {
    select_hosted_script_round_with_budget(
        requests, pending, selection, COLD_DEBUG_CELL_SETTLEMENT_BUDGET,
    ).await.unwrap_or_else(|failure| {
        let retained_origins: Vec<_> = pending.iter().map(HostedScriptRound::origin).collect();
        panic!("production provider selection {selection:?} failed: {failure:?}; retained provider request origins: {retained_origins:?}")
    })
}

/// Preserve interleaved provider requests while one actor's reply is awaited.
pub(super) async fn next_hosted_script_round(
    requests: &mut tokio::sync::mpsc::UnboundedReceiver<HostedScriptRound>,
    pending: &mut std::collections::VecDeque<HostedScriptRound>,
    actor: &harness::model::AgentPath,
) -> HostedScriptRound {
    select_hosted_script_round(requests, pending, HostedScriptSelection::Actor(actor)).await
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
    use std::future::Future;

    #[test]
    fn scoped_trace_filter_retains_campaign_metadata_and_excludes_content() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("trace.jsonl");
        let file = std::fs::File::create(&path).unwrap();
        let subscriber = tracing_subscriber::fmt()
            .json()
            .with_ansi(false)
            .with_env_filter(campaign_trace_filter())
            .with_writer(std::sync::Mutex::new(file))
            .finish();
        let request = exomonad_actor::RequestId(7);
        let parent = exomonad_actor::ActorRef::first(exomonad_actor::ActorId(1));
        let child = exomonad_actor::ActorRef {
            id: exomonad_actor::ActorId(2),
            incarnation: exomonad_actor::Incarnation(3),
        };
        let execution = tidepool_runtime::session::WorkbenchExecutionId::from_digest([2; 16]);
        let attempt = uuid::Uuid::from_bytes([3; 16]);
        tracing::subscriber::with_default(subscriber, || {
            // The registry's internal queue gate is exercised by actor tests;
            // this control checks the shared subscriber with its typed wire fields.
            let parent_cell = tracing::info_span!(target: "exomonad_actor::workbench_phase",
                "cell", actor = %parent, execution = %execution);
            let _parent_cell = parent_cell.enter();
            tracing::info!(target: "exomonad_actor::request",
                request = request.0, parent_actor = %parent, activation_actor = %child,
                parent_execution = %execution, parent_attempt = %attempt,
                "request activation origin issued");
            tracing::debug!(target: "tidepool_toolchain::module_candidates",
                phase = "configured_package_owner_wait", elapsed_ms = 7u64);
            tracing::info!(target: "tidepool_toolchain::module_candidates",
                phase = "candidate_selection", elapsed_ms = 3u64,
                exact_context = true, offered = 2u64);
            tracing::info!(target: "tidepool_toolchain::artifacts",
                phase = "exact_immutable_materialization", retained_entries = 2u64,
                new_entries = 1u64, recovery_read_bytes = 4096u64,
                "completed private artifact ownership");
            tracing::trace!(target: crate::exomonad::CONTENT_TARGET, source = "private-content");
            tracing::info!(target: crate::exomonad::CONTENT_TARGET, source = "private-content");
            tracing::warn!(target: crate::exomonad::CONTENT_TARGET, source = "private-content");
        });
        let captured = std::fs::read_to_string(&path).unwrap();
        let rows = captured
            .lines()
            .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
            .collect::<Vec<_>>();
        assert_eq!(rows.len(), 4, "only campaign metadata is retained");
        assert_eq!(rows[0]["target"], "exomonad_actor::request");
        assert_eq!(
            rows[0]["span"],
            serde_json::json!({
                "name": "cell", "actor": parent.to_string(), "execution": execution.to_string(),
            })
        );
        assert_eq!(
            rows[0]["fields"],
            serde_json::json!({
                "message": "request activation origin issued",
                "request": request.0,
                "parent_actor": parent.to_string(),
                "activation_actor": child.to_string(),
                "parent_execution": execution.to_string(),
                "parent_attempt": attempt.to_string(),
            })
        );
        assert_eq!(rows[1]["target"], "tidepool_toolchain::module_candidates");
        assert_eq!(
            rows[1]["fields"],
            serde_json::json!({
                "phase": "configured_package_owner_wait", "elapsed_ms": 7,
            })
        );
        assert_eq!(rows[2]["target"], "tidepool_toolchain::module_candidates");
        assert_eq!(
            rows[2]["fields"],
            serde_json::json!({
                "phase": "candidate_selection",
                "elapsed_ms": 3,
                "exact_context": true,
                "offered": 2,
            })
        );
        assert_eq!(rows[3]["target"], "tidepool_toolchain::artifacts");
        assert_eq!(
            rows[3]["fields"],
            serde_json::json!({
                "message": "completed private artifact ownership",
                "phase": "exact_immutable_materialization",
                "retained_entries": 2,
                "new_entries": 1,
                "recovery_read_bytes": 4096,
            })
        );
        assert!(!captured.contains("private-content"));
    }

    #[test]
    fn minimal_campaign_trace_excludes_detailed_events_but_keeps_coarse_metadata() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("minimal.jsonl");
        let file = std::fs::File::create(&path).unwrap();
        let subscriber = tracing_subscriber::fmt()
            .json()
            .with_ansi(false)
            .with_env_filter(campaign_trace_filter_for("minimal"))
            .with_span_events(tracing_subscriber::fmt::format::FmtSpan::NONE)
            .with_writer(std::sync::Mutex::new(file))
            .finish();
        tracing::subscriber::with_default(subscriber, || {
            tracing::info!(target: "exomonad_actor::request", request = 7u64,
                "request activation origin issued");
            tracing::debug!(target: "exomonad_harness::timing", phase = "reply_to_successor",
                wall_ns = 12u64, "coarse workload timing");
            tracing::debug!(target: "tidepool_toolchain::module_candidates",
                phase = "configured_package_owner_wait", elapsed_ms = 7u64);
            tracing::info!(target: "tidepool_toolchain::module_candidates",
                phase = "candidate_selection", offered = 2u64);
            tracing::info!(target: "tidepool_toolchain::artifacts",
                phase = "exact_immutable_materialization", retained_entries = 2u64);
            tracing::info!(target: "tidepool_runtime::compile", "compiler compilation event");
            tracing::info!(target: crate::exomonad::CONTENT_TARGET, source = "private-content");
        });
        let captured = std::fs::read_to_string(path).unwrap();
        assert!(captured.contains("request activation origin issued"));
        assert!(captured.contains("coarse workload timing"));
        assert!(!captured.contains("candidate_selection"));
        assert!(!captured.contains("configured_package_owner_wait"));
        assert!(!captured.contains("exact_immutable_materialization"));
        assert!(!captured.contains("compiler compilation event"));
        assert!(!captured.contains("private-content"));
    }

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

    fn numbered_round(
        actor: &harness::model::AgentPath,
        ordinal: usize,
    ) -> (
        HostedScriptRound,
        tokio::sync::oneshot::Receiver<harness::transport::ResponsesTurn>,
    ) {
        let (reply, response) = tokio::sync::oneshot::channel();
        let mut request = request();
        request.session_id = format!("test-run:{}:1", actor.0);
        request.instructions = ordinal.to_string();
        (
            HostedScriptRound {
                request,
                reply,
                request_id: None,
                request_started_at: std::time::Instant::now(),
            },
            response,
        )
    }

    #[test]
    fn hosted_round_selection_histories_preserve_arrival_order_and_exact_reply_custody() {
        use proptest::prelude::*;
        use proptest::test_runner::{Config, TestRunner};
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .unwrap();
        let actors = ["/root", "/root/a2_i1", "/root/a3_i1"]
            .map(|actor| harness::model::AgentPath(actor.into()));
        let mut config = Config::default();
        if let Some(path) = option_env!("TIDEPOOL_PROPTEST_REGRESSIONS") {
            config.failure_persistence = Some(Box::new(
                proptest::test_runner::FileFailurePersistence::Direct(path),
            ));
        }
        let mut config = proptest::test_runner::contextualize_config(config);
        config.source_file = Some(file!());
        config.test_name = Some(concat!(
            module_path!(),
            "::hosted_round_selection_histories_preserve_arrival_order_and_exact_reply_custody"
        ));
        // The full arrival history plus selected ordinals is an independent
        // oracle: recompute the oldest unconsumed matching request each time.
        TestRunner::new(config)
            .run(
                &(
                    proptest::collection::vec(0usize..3, 0..40),
                    proptest::collection::vec(0usize..4, 0..40),
                ),
                |(arrivals, mut selections)| {
                    runtime.block_on(async {
                        let (sender, mut requests) = tokio::sync::mpsc::unbounded_channel();
                        let mut replies = Vec::new();
                        for (ordinal, actor) in arrivals.iter().enumerate() {
                            let (round, reply) = numbered_round(&actors[*actor], ordinal);
                            sender.send(round).unwrap();
                            replies.push(reply);
                        }
                        drop(sender);
                        let mut pending = std::collections::VecDeque::new();
                        let mut consumed = std::collections::HashSet::new();
                        // Drain every remaining origin, including repeated matches,
                        // after the generated operation sequence.
                        for actor in 0..3 {
                            selections.extend(std::iter::repeat_n(actor, arrivals.len() + 1));
                        }
                        for selected in selections {
                            let selection = if selected == 3 {
                                HostedScriptSelection::OtherThan(&actors[0])
                            } else {
                                HostedScriptSelection::Actor(&actors[selected])
                            };
                            let expected = arrivals
                                .iter()
                                .enumerate()
                                .find(|(ordinal, actor)| {
                                    !consumed.contains(ordinal)
                                        && if selected == 3 {
                                            **actor != 0
                                        } else {
                                            **actor == selected
                                        }
                                })
                                .map(|(ordinal, _)| ordinal);
                            let actual = select_hosted_script_round_with_budget(
                                &mut requests,
                                &mut pending,
                                selection,
                                Duration::from_secs(1),
                            )
                            .await;
                            match (expected, actual) {
                                (Some(expected), Ok(round)) => {
                                    let ordinal =
                                        round.request.instructions.parse::<usize>().unwrap();
                                    prop_assert_eq!(ordinal, expected);
                                    prop_assert!(
                                        consumed.insert(ordinal),
                                        "a request cannot be selected twice"
                                    );
                                    let origin = round.origin();
                                    prop_assert_eq!(origin.actor(), &actors[arrivals[ordinal]]);
                                    round.finish();
                                    prop_assert!(
                                        replies[ordinal].try_recv().is_ok(),
                                        "selection retains the original reply channel"
                                    );
                                }
                                (None, Err(HostedScriptWaitFailure::ProviderClosed)) => {
                                    let actual: Vec<_> = pending
                                        .iter()
                                        .map(|round| {
                                            round.request.instructions.parse::<usize>().unwrap()
                                        })
                                        .collect();
                                    let expected: Vec<_> = (0..arrivals.len())
                                        .filter(|ordinal| !consumed.contains(ordinal))
                                        .collect();
                                    prop_assert_eq!(
                                        actual,
                                        expected,
                                        "EOF must retain all unmatched requests in order"
                                    );
                                }
                                _ => {
                                    return Err(TestCaseError::fail(
                                        "selection differed from recomputed arrival history",
                                    ))
                                }
                            }
                            let retained: Vec<_> = pending
                                .iter()
                                .map(|round| round.request.instructions.parse::<usize>().unwrap())
                                .collect();
                            prop_assert!(
                                retained.windows(2).all(|pair| pair[0] < pair[1]),
                                "nonmatches retain arrival order"
                            );
                            prop_assert!(
                                retained.iter().all(|ordinal| !consumed.contains(ordinal)),
                                "completed requests cannot remain pending"
                            );
                        }
                        prop_assert_eq!(consumed.len(), arrivals.len());
                        prop_assert!(pending.is_empty());
                        Ok(())
                    })
                },
            )
            .unwrap();
    }

    #[tokio::test]
    async fn hosted_round_selection_deadline_and_eof_preserve_interleaved_nonmatches() {
        use futures_util::FutureExt;
        let root = harness::model::AgentPath("/root".into());
        let child = harness::model::AgentPath("/root/a2_i1".into());
        let absent = harness::model::AgentPath("/root/a3_i1".into());
        let (sender, mut requests) = tokio::sync::mpsc::unbounded_channel();
        let (round, mut root_reply) = numbered_round(&root, 0);
        sender.send(round).unwrap();
        let mut pending = std::collections::VecDeque::new();
        let result = select_hosted_script_round_with_budget(
            &mut requests,
            &mut pending,
            HostedScriptSelection::OtherThan(&root),
            Duration::from_millis(1),
        )
        .await;
        assert!(matches!(result, Err(HostedScriptWaitFailure::Deadline)));
        assert_eq!(pending.len(), 1);
        let (round, mut child_reply) = numbered_round(&child, 1);
        sender.send(round).unwrap();
        let round = select_hosted_script_round(
            &mut requests,
            &mut pending,
            HostedScriptSelection::OtherThan(&root),
        )
        .await;
        assert_eq!(round.origin().actor(), &child);
        round.finish();
        assert!(child_reply.try_recv().is_ok());
        drop(sender);
        let failure = std::panic::AssertUnwindSafe(select_hosted_script_round(
            &mut requests,
            &mut pending,
            HostedScriptSelection::Actor(&absent),
        ))
        .catch_unwind()
        .await
        .err()
        .expect("EOF refuses an absent origin");
        let message = failure.downcast_ref::<String>().unwrap();
        for fragment in [
            "ProviderClosed",
            "/root/a3_i1",
            "retained provider request origins",
            "/root",
        ] {
            assert!(message.contains(fragment), "{message}");
        }
        next_hosted_script_round(&mut requests, &mut pending, &root)
            .await
            .finish();
        assert!(root_reply.try_recv().is_ok());
        assert!(pending.is_empty());
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
        HostedScriptRound {
            request,
            reply,
            request_id: None,
            request_started_at: std::time::Instant::now(),
        }
        .call("prepared-children", "display True");
        let response = response.await.expect("script replies on its declared tool");
        assert_eq!(response.items[0].0["call_id"], "prepared-children");
        assert_eq!(response.items[0].0["name"], "haskell_sync");
    }

    #[tokio::test]
    async fn scripted_provider_retains_engine_issued_original_operation() {
        let (provider, mut rounds) = hosted_script_provider();
        let mut request = request();
        request.session_id = "exact-run:/root:7".into();
        request.tools = vec![serde_json::json!({"type":"custom", "name":"haskell_sync"})].into();
        let request_id = harness::model::RequestId("exact-engine-request".into());
        let issued = request_id.clone();
        let (sink, mut events) = tokio::sync::mpsc::channel(1);
        let task = tokio::spawn(async move {
            provider
                .create_streaming_for_request(&issued, request, sink)
                .await
        });
        let round = rounds.recv().await.unwrap();
        assert_eq!(
            round.operation("expected-call"),
            harness::model::OperationId {
                origin: harness::model::ConversationIdentity::Embedded {
                    run: "exact-run".into(),
                    actor: harness::model::AgentPath("/root".into()),
                    incarnation: "7".into(),
                },
                request: request_id,
                call: harness::model::CallId("expected-call".into()),
            }
        );
        round.call("expected-call", "display True");
        let turn = task.await.unwrap().unwrap();
        assert!(
            matches!(events.recv().await, Some(harness::transport::sse::StreamEvent::ItemDone(item)) if item == turn.items[0])
        );
    }

    #[tokio::test]
    async fn scripted_cell_refuses_an_unregistered_tool_before_provider_admission() {
        let (reply, response) = tokio::sync::oneshot::channel();
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            HostedScriptRound {
                request: request(),
                reply,
                request_id: None,
                request_started_at: std::time::Instant::now(),
            }
            .call("prepared-children", "display True");
        }));
        assert!(
            result.is_err(),
            "the fixture must reject its mismatched tool locally"
        );
        assert!(response.await.is_err(), "no invalid tool call was sent");
    }

    #[tokio::test]
    async fn scripted_async_cell_uses_advertised_name_and_typed_execution() {
        let (reply, response) = tokio::sync::oneshot::channel();
        let mut request = request();
        request.tools = vec![serde_json::json!({"type":"custom", "name":"haskell"})].into();
        HostedScriptRound {
            request,
            reply,
            request_id: None,
            request_started_at: std::time::Instant::now(),
        }
        .async_call("held-capture", "display True");
        let response = response.await.unwrap();
        let call = response.items[0].tool_call().unwrap().unwrap();
        assert_eq!(call.name, "haskell");
        assert_eq!(call.execution, harness::item::ToolExecution::Asynchronous);
    }

    #[tokio::test]
    async fn scripted_function_requires_the_advertised_function_kind() {
        let (reply, response) = tokio::sync::oneshot::channel();
        let mut advertised_request = request();
        advertised_request.tools =
            vec![serde_json::json!({"type":"function", "name":"yield"})].into();
        HostedScriptRound {
            request: advertised_request,
            reply,
            request_id: None,
            request_started_at: std::time::Instant::now(),
        }
        .function("held-yield", "yield", serde_json::json!({}));
        let response = response.await.unwrap();
        let call = response.items[0].tool_call().unwrap().unwrap();
        assert_eq!(call.name, "yield");
        assert_eq!(call.execution, harness::item::ToolExecution::Synchronous);

        let (reply, response) = tokio::sync::oneshot::channel();
        let mut wrong_kind = request();
        wrong_kind.tools = vec![serde_json::json!({"type":"custom", "name":"yield"})].into();
        assert!(std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            HostedScriptRound {
                request: wrong_kind,
                reply,
                request_id: None,
                request_started_at: std::time::Instant::now(),
            }
            .function("wrong-kind", "yield", serde_json::json!({}));
        }))
        .is_err());
        assert!(
            response.await.is_err(),
            "wrong-kind invocation never reaches Engine"
        );
    }

    struct ShutdownCommands {
        backend: Arc<super::super::command_test_support::TestCommands>,
        started: tokio::sync::Semaphore,
        cleanup_probes: std::sync::atomic::AtomicUsize,
        unknown: std::sync::atomic::AtomicBool,
    }

    impl ShutdownCommands {
        fn new(unknown: bool) -> Arc<Self> {
            Arc::new(Self {
                backend: super::super::command_test_support::TestCommands::new(),
                started: tokio::sync::Semaphore::new(0),
                cleanup_probes: 0.into(),
                unknown: unknown.into(),
            })
        }

        fn cleanup_outcome(&self) -> tidepool_bridge_effects::CommandCleanup {
            if self.unknown.load(std::sync::atomic::Ordering::Acquire) {
                tidepool_bridge_effects::CommandCleanup::CommandCleanupUnknown(
                    "command stopped but external cleanup remains unknown".into(),
                )
            } else {
                tidepool_bridge_effects::CommandCleanup::CommandClean
            }
        }
    }

    impl exomonad_actor::command_jobs::CommandBackend for ShutdownCommands {
        fn execute<'a>(
            &'a self,
            id: &'a str,
            spec: tidepool_bridge_effects::CommandSpec,
            phase: tokio::sync::watch::Sender<tidepool_bridge_effects::CommandStatus>,
        ) -> futures_util::future::BoxFuture<'a, tidepool_bridge_effects::CommandResult> {
            Box::pin(async move {
                self.started.add_permits(1);
                let mut result = self.backend.execute(id, spec, phase).await;
                result.cleanup = self.cleanup_outcome();
                result
            })
        }

        fn control<'a>(
            &'a self,
            id: &'a str,
            operation: exomonad_actor::command_jobs::CommandControl,
        ) -> futures_util::future::BoxFuture<'a, Result<(), tidepool_bridge_effects::CommandError>>
        {
            self.backend.control(id, operation)
        }

        fn cleanup<'a>(
            &'a self,
            _: &'a str,
        ) -> futures_util::future::BoxFuture<'a, tidepool_bridge_effects::CommandCleanup> {
            Box::pin(async move {
                self.cleanup_probes
                    .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                self.cleanup_outcome()
            })
        }

        fn output<'a>(
            &'a self,
            id: &'a str,
            budget: usize,
        ) -> futures_util::future::BoxFuture<
            'a,
            Result<tidepool_bridge_effects::CommandOutput, tidepool_bridge_effects::CommandError>,
        > {
            self.backend.output(id, budget)
        }

        fn read<'a>(
            &'a self,
            id: &'a str,
            stream: tidepool_bridge_effects::CommandStream,
            position: tidepool_bridge_effects::CommandPosition,
        ) -> futures_util::future::BoxFuture<
            'a,
            Result<tidepool_bridge_effects::CommandPage, tidepool_bridge_effects::CommandError>,
        > {
            self.backend.read(id, stream, position)
        }
    }

    #[derive(Clone, Copy)]
    enum CommandLifetime {
        Actor,
        Run,
    }

    async fn admit_shutdown_command(
        campaign: &mut TestCampaign,
        backend: Arc<ShutdownCommands>,
        lifetime: CommandLifetime,
    ) {
        let source = match lifetime {
            CommandLifetime::Actor => "job <- Cmd.start [bash|sleep 3600|]\nCmd.detach job",
            CommandLifetime::Run => {
                "job <- Cmd.tryStartWith RunOwned [bash|sleep 3600|] >>= liftEither"
            }
        };
        let policy = campaign.root_installation.policy.clone();
        // Run ownership commits only after backend supply captures the original
        // principal and grants. Drive supply while the cell can still be parked.
        tokio::time::timeout(COLD_DEBUG_CELL_SETTLEMENT_BUDGET, async {
            tokio::join!(
                async {
                    let response =
                        super::super::tests::dispatch_haskell_script(policy.as_ref(), source).await;
                    assert_eq!(response["status"], "committed", "{response}");
                },
                async {
                    let request = campaign
                        .next_deployment(
                            "shutdown command backend supply",
                            COLD_DEBUG_CELL_SETTLEMENT_BUDGET,
                            |event| match event {
                                LocalResidentDeployment::CommandBackend(request) => Ok(request),
                                other => Err(other),
                            },
                        )
                        .await;
                    assert_eq!(
                        request.purpose,
                        exomonad_actor::command_jobs::CommandBackendPurpose::Command,
                        "the shutdown fixture starts one command without source capture"
                    );
                    request.supply(Ok(backend.clone()));
                }
            );
        })
        .await
        .expect("shutdown command cell and backend supply settle within the campaign budget");
        tokio::time::timeout(Duration::from_secs(30), backend.started.acquire())
            .await
            .expect("command execution was actually admitted")
            .unwrap()
            .forget();
        assert_eq!(backend.backend.executions(), 1);
        assert_eq!(
            backend.backend.control_count(),
            0,
            "shutdown starts after command admission"
        );
    }

    #[tokio::test]
    async fn consuming_shutdown_confirms_command_cleanup_and_hosted_join() {
        let campaign = TestCampaign::start().await;
        let backend = ShutdownCommands::new(false);
        let observed = backend.clone();
        campaign
            .run_scenario(|campaign| {
                Box::pin(async move {
                    admit_shutdown_command(campaign, backend, CommandLifetime::Actor).await;
                })
            })
            .await;
        assert_eq!(observed.backend.control_count(), 1);
        assert!(observed
            .backend
            .cancelled
            .load(std::sync::atomic::Ordering::Acquire));
    }

    #[tokio::test]
    async fn consuming_shutdown_rejects_unknown_command_cleanup_after_hosted_join() {
        let campaign = TestCampaign::start().await;
        let root_actor = campaign.actor.clone();
        let backend = ShutdownCommands::new(true);
        let observed = backend.clone();
        let first = campaign
            .run_scenario_expecting_cleanup_failure(
                |campaign| {
                    Box::pin(async move {
                        admit_shutdown_command(campaign, backend.clone(), CommandLifetime::Actor)
                            .await;
                        let first = campaign
                            .observe_shutdown()
                            .await
                            .expect_err("external cleanup is initially unknown");
                        assert_eq!(first.hosted, CampaignHostedJoin::Joined);
                        let CampaignRootRetirement::Settled(root) = &first.root else {
                            panic!("root retirement settles independently of external cleanup");
                        };
                        assert_eq!(
                            root.cleanup.hook(),
                            &exomonad_actor::CleanupComponentOutcome::Confirmed
                        );
                        assert_eq!(
                            root.cleanup.realm(),
                            &exomonad_actor::CleanupComponentOutcome::Confirmed
                        );
                        assert_eq!(
                            root.cleanup.children(),
                            &exomonad_actor::CleanupComponentOutcome::Unconfirmed(
                                "command stopped but external cleanup remains unknown".into()
                            )
                        );
                        // The external probe can later clear. The actor's already
                        // published first terminal cleanup remains authoritative.
                        backend
                            .unknown
                            .store(false, std::sync::atomic::Ordering::Release);
                        first
                    })
                },
                |failure| {
                    assert_eq!(failure.hosted, CampaignHostedJoin::Joined);
                    let CampaignRootRetirement::Settled(root) = &failure.root else {
                        panic!("the actual root retired with a cleanup observation: {failure:?}");
                    };
                    assert_eq!(
                        root.cleanup.hook(),
                        &exomonad_actor::CleanupComponentOutcome::Confirmed
                    );
                    assert_eq!(
                        root.cleanup.realm(),
                        &exomonad_actor::CleanupComponentOutcome::Confirmed
                    );
                    assert_eq!(
                        root.cleanup.children(),
                        &exomonad_actor::CleanupComponentOutcome::Unconfirmed(
                            "command stopped but external cleanup remains unknown".into()
                        )
                    );
                    assert!(
                        failure.forest.iter().any(|outcome| matches!(outcome,
                exomonad_actor::ForestRootShutdown::Settled(root) if !root.cleanup.is_confirmed()))
                    );
                },
            )
            .await;
        let CampaignRootRetirement::Settled(first_root) = &first.root else {
            panic!("initial root retirement must remain observable: {first:?}");
        };
        assert_eq!(
            root_actor.terminal().cleanup(),
            Some(first_root.cleanup.clone())
        );
        let controls = observed.backend.controls.lock();
        assert!(
            matches!(
                controls.as_slice(),
                [
                    exomonad_actor::command_jobs::CommandControl::Cancel,
                    exomonad_actor::command_jobs::CommandControl::Cancel
                ]
            ),
            "post_stop cancels execution; retirement retries uncertain external cleanup: {controls:?}"
        );
        assert!(observed
            .backend
            .cancelled
            .load(std::sync::atomic::Ordering::Acquire));
        assert!(
            observed
                .cleanup_probes
                .load(std::sync::atomic::Ordering::SeqCst)
                > 0,
            "the external owner was actually asked for cleanup evidence"
        );
    }

    #[tokio::test]
    async fn scenario_assertion_panic_still_consumes_campaign_cleanup() {
        use futures_util::FutureExt;
        let campaign = TestCampaign::start().await;
        let root = campaign.actor.clone();
        let result = std::panic::AssertUnwindSafe(
            campaign.run_scenario(|_| Box::pin(async { std::panic::panic_any(91_u32) })),
        )
        .catch_unwind()
        .await;
        assert_eq!(result.unwrap_err().downcast_ref::<u32>(), Some(&91));
        assert!(
            root.terminal().get().is_some(),
            "cleanup ran before original panic resumed"
        );
        assert!(root.terminal().cleanup().unwrap().is_confirmed());
    }

    #[tokio::test]
    async fn cancelled_executor_observation_keeps_consuming_shutdown_available() {
        let campaign = TestCampaign::start().await;
        campaign
            .run_scenario(|campaign| {
                Box::pin(async move {
                    let mut observation = Box::pin(campaign.observe_hosted_completion());
                    std::future::poll_fn(|context| {
                        assert!(
                            observation.as_mut().poll(context).is_pending(),
                            "a live idle actor has not completed its task"
                        );
                        std::task::Poll::Ready(())
                    })
                    .await;
                    drop(observation);
                })
            })
            .await;
    }

    #[tokio::test]
    async fn unfinished_campaign_guard_rejects_scope_exit_without_replacing_an_assertion_panic() {
        for original in [None, Some(73_u32)] {
            let mut campaign = TestCampaign::start().await;
            let forest = campaign.forest.clone();
            let _repository = std::mem::replace(
                &mut campaign._repository,
                exomonad_worktree::testing::TestRepo::init().unwrap(),
            );
            let _runtime = std::mem::replace(&mut campaign._runtime, tempfile::tempdir().unwrap());
            let _session_root = campaign.session_root.clone();
            let _host_incarnation = campaign.host_incarnation.clone();
            // This guard test keeps executor custody outside the owner being
            // deliberately dropped, then settles those real resources below.
            let executor = std::mem::replace(
                &mut campaign.executor,
                CampaignExecutor::Settled(CampaignHostedJoin::TimedOut),
            );
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                let _campaign = campaign;
                if let Some(payload) = original {
                    std::panic::panic_any(payload);
                }
            }));
            let outcomes = forest.shutdown().await;
            let CampaignExecutor::Running(mut task) = executor else {
                panic!("the guard fixture owns its live executor");
            };
            tokio::time::timeout(Duration::from_secs(30), &mut task)
                .await
                .expect("guard fixture cleanup joins")
                .unwrap();
            assert!(outcomes
                .iter()
                .all(exomonad_actor::ForestRootShutdown::is_confirmed));
            let panic = result.expect_err("an unfinished campaign cannot leave scope successfully");
            if let Some(original) = original {
                assert_eq!(panic.downcast_ref::<u32>(), Some(&original));
            } else {
                assert!(
                    panic.downcast_ref::<String>().is_some()
                        || panic.downcast_ref::<&str>().is_some()
                );
            }
        }
    }

    #[tokio::test]
    async fn run_resource_cleanup_retry_preserves_history_and_can_confirm_consuming_shutdown() {
        let campaign = TestCampaign::start().await;
        let backend = ShutdownCommands::new(true);
        let observed = backend.clone();
        let controls_after_retry = campaign.run_scenario(|campaign| Box::pin(async move {
            admit_shutdown_command(campaign, backend.clone(), CommandLifetime::Run).await;
            let first = campaign.observe_shutdown().await.expect_err("run-owned cleanup initially refuses confirmation");
            assert_eq!(first.hosted, CampaignHostedJoin::Joined);
            assert!(matches!(&first.root, CampaignRootRetirement::Settled(root) if root.cleanup.is_confirmed()));
            assert!(first.forest.iter().any(|outcome| matches!(outcome,
                exomonad_actor::ForestRootShutdown::RunResources(exomonad_actor::CleanupComponentOutcome::Unconfirmed(_)))));
            assert_eq!(backend.backend.control_count(), 2, "job stop cancels execution and retirement retries unclean descendants");
            let probes_before_retry = backend.cleanup_probes.load(std::sync::atomic::Ordering::SeqCst);
            assert!(probes_before_retry > 0, "the initial external cleanup was probed");
            backend.unknown.store(false, std::sync::atomic::Ordering::Release);
            let retried = campaign.observe_shutdown().await.expect("the actual run owner retries retained cleanup");
            assert!(retried.forest.len() > first.forest.len(), "prior refusal observations remain visible");
            assert!(retried.forest.starts_with(&first.forest));
            assert!(retried.is_confirmed());
            let controls_after_retry = backend.backend.control_count();
            assert!((2..=3).contains(&controls_after_retry), "retirement retries cancellation only if cleanup is still uncertain; settlement can confirm it first");
            controls_after_retry
        })).await;
        assert_eq!(
            observed.backend.control_count(),
            controls_after_retry,
            "consuming shutdown does not recancel confirmed run cleanup"
        );
        assert!(
            observed
                .cleanup_probes
                .load(std::sync::atomic::Ordering::SeqCst)
                > 0
        );
    }
}
