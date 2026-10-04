//! Hosted acceptance executes the application entrypoint and observes its owners.

use super::*;
use futures_util::FutureExt;
use std::time::Duration;
use tokio::sync::oneshot;

const STARTUP_BUDGET: Duration = Duration::from_secs(300);
const SHUTDOWN_BUDGET: Duration = Duration::from_secs(60);

#[derive(Clone)]
pub(super) struct ObservedInstallation {
    pub(super) actor: LocalActorRef,
    pub(super) checkpoint: bool,
    pub(super) context_parent: Option<ActorRef>,
}

#[derive(Clone)]
pub(super) struct HostTestObserver {
    installations: Arc<Mutex<HashMap<ActorRef, ObservedInstallation>>>,
    changed: watch::Sender<u64>,
}

impl Default for HostTestObserver {
    fn default() -> Self {
        Self {
            installations: Arc::default(),
            changed: watch::channel(0).0,
        }
    }
}

impl HostTestObserver {
    pub(super) fn installed(&self, installation: &LocalResidentInstallation) {
        self.installations.lock().insert(
            installation.actor.identity(),
            ObservedInstallation {
                actor: installation.actor.clone(),
                checkpoint: installation.checkpoint.is_some(),
                context_parent: installation.context_parent,
            },
        );
        self.changed.send_modify(|revision| *revision += 1);
    }

    pub(super) fn installations(&self) -> Vec<ObservedInstallation> {
        self.installations.lock().values().cloned().collect()
    }

    pub(super) async fn installation(&self, actor: ActorRef) -> ObservedInstallation {
        let mut changed = self.changed.subscribe();
        loop {
            if let Some(installation) = self.installations.lock().get(&actor).cloned() {
                return installation;
            }
            changed
                .changed()
                .await
                .expect("host installation observer closed");
        }
    }
}

pub(super) type HostTransportFactory = Box<
    dyn FnOnce(
            &Arc<embedded_harness::EmbeddedHarnessRuntime>,
            &ActorHostConfig,
        ) -> Arc<dyn harness::engine::ResponsesTransport>
        + Send,
>;

pub(super) struct HostTestHooks {
    pub(super) observer: HostTestObserver,
    pub(super) assembled: Option<oneshot::Sender<HostedActorContext>>,
    pub(super) stopping: watch::Receiver<bool>,
    pub(super) transport: Option<HostTransportFactory>,
}

/// These handles are issued by the production assembly after durable startup.
#[derive(Clone)]
pub(super) struct HostedActorContext {
    pub(super) config: ActorHostConfig,
    pub(super) actor: LocalActorRef,
    pub(super) forest: Arc<ResidentForest<ExomonadHandlerStack, CapturedOutput>>,
    pub(super) runtime: Arc<embedded_harness::EmbeddedHarnessRuntime>,
    pub(super) observer: HostTestObserver,
    pub(super) owners: InteractiveOwners,
}

impl HostedActorContext {
    pub(super) fn binding(
        &self,
        actor: ActorRef,
    ) -> Option<embedded_harness::EmbeddedActorBinding> {
        embedded_binding(&self.owners, actor)
    }
}

/// One test executor for the existing production run, including its shutdown.
pub(super) struct HostedTestRuntime {
    pub(super) context: HostedActorContext,
    pub(super) runtime: Arc<embedded_harness::EmbeddedHarnessRuntime>,
    pub(super) address: std::net::SocketAddr,
    stop: watch::Sender<bool>,
    outcome:
        futures_util::future::Shared<futures_util::future::BoxFuture<'static, Result<(), String>>>,
    thread: Option<std::thread::JoinHandle<()>>,
    _repository: exomonad_worktree::testing::TestRepo,
    _runtime: tempfile::TempDir,
}

impl Drop for HostedTestRuntime {
    fn drop(&mut self) {
        // The production owner drains its services and forest even on assertion
        // unwinding. Only an acknowledged `stop` qualifies successful cleanup.
        self.stop.send_replace(true);
    }
}

impl HostedTestRuntime {
    pub(super) async fn cell_settlement_diagnostic(&self, call_id: &str) -> String {
        let call = harness::model::CallId(call_id.into());
        let store = self.runtime.store();
        let Ok(claims) = store.claims(&call) else {
            return format!("call={call_id}, claims=lookup_failed");
        };
        let mut settlements = Vec::new();
        for claim in claims.iter().take(4) {
            let scheduler = self.runtime.scheduler().output(&claim.operation).await;
            let stage = match &scheduler {
                Ok(None) => "pending",
                Ok(Some(harness::turn::JobOutput::Completed(Ok(_)))) => "completed",
                Ok(Some(harness::turn::JobOutput::Completed(Err(_)))) => "failed",
                Ok(Some(
                    harness::turn::JobOutput::Cancelled
                    | harness::turn::JobOutput::CancelledWithReceipt(_),
                )) => "cancelled",
                Ok(Some(harness::turn::JobOutput::Interrupted)) => "interrupted",
                Ok(Some(harness::turn::JobOutput::CancellationUnconfirmed(_))) => "unconfirmed",
                Err(_) => "lookup_failed",
            };
            let failure = match &scheduler {
                Ok(Some(
                    harness::turn::JobOutput::Completed(Err(error))
                    | harness::turn::JobOutput::CancelledWithReceipt(Err(error)),
                )) => Some(error.message().chars().take(2048).collect::<String>()),
                Ok(Some(harness::turn::JobOutput::CancellationUnconfirmed(error))) => {
                    Some(error.chars().take(2048).collect::<String>())
                }
                Ok(Some(
                    harness::turn::JobOutput::Completed(Ok(value))
                    | harness::turn::JobOutput::CancelledWithReceipt(Ok(value)),
                )) => {
                    let items = value["items"].as_array();
                    let failures = items
                        .into_iter()
                        .flatten()
                        .filter(|item| {
                            use tidepool_runtime::session::WorkbenchItemStatus;
                            [
                                WorkbenchItemStatus::Stopped,
                                WorkbenchItemStatus::Diagnostic,
                                WorkbenchItemStatus::Rejected,
                            ]
                            .into_iter()
                            .any(|status| item["status"] == serde_json::to_value(status).unwrap())
                        })
                        .take(4)
                        .map(|item| {
                            let diagnostics = item["diagnostics"]
                                .as_array()
                                .into_iter()
                                .flatten()
                                .take(4)
                                .filter_map(|diagnostic| diagnostic["message"].as_str())
                                .map(|message| message.chars().take(512).collect::<String>())
                                .collect::<Vec<_>>();
                            json!({
                                "status": item["status"].as_str(),
                                "failureLayer": item["failureLayer"].as_str(),
                                "diagnostics": diagnostics,
                            })
                        })
                        .collect::<Vec<_>>();
                    (!failures.is_empty()).then(|| {
                        serde_json::to_string(&json!({
                            "items": failures,
                            "summary": value["summary"]
                                .as_str()
                                .map(|summary| summary.chars().take(2048).collect::<String>()),
                        }))
                        .unwrap()
                        .chars()
                        .take(2048)
                        .collect::<String>()
                    })
                }
                _ => None,
            };
            settlements.push(format!(
                "request={}, claim={:?}, scheduler={stage}, failure={failure:?}, persisted_output={:?}",
                claim.request.0,
                claim.state,
                store
                    .replay_output_operation(&claim.operation)
                    .map(|value| value.is_some()),
            ));
        }
        format!(
            "call={call_id}, legacy_codex_computing={}, claims={}, settlements={settlements:?}, actor_terminal={:?}, actor_failure={:?}",
            self.context.actor.hosted_cell_computing(),
            claims.len(),
            self.context
                .actor
                .terminal()
                .get()
                .map(|terminal| terminal.kind),
            self.context.actor.terminal().get().and_then(|terminal| {
                (terminal.kind == exomonad_actor::ActorExitKind::Failed)
                    .then(|| terminal.summary.chars().take(2048).collect::<String>())
            }),
        )
    }

    pub(super) async fn start(
        settings: &crate::exomonad::EmbeddedLaunchConfig,
        transport: &Arc<dyn harness::engine::ResponsesTransport>,
    ) -> Result<Self, String> {
        Self::start_configured(settings, transport, |_| {}).await
    }

    pub(super) async fn start_configured(
        settings: &crate::exomonad::EmbeddedLaunchConfig,
        transport: &Arc<dyn harness::engine::ResponsesTransport>,
        configure: impl FnOnce(&mut ActorHostConfig),
    ) -> Result<Self, String> {
        Self::start_owned(settings, configure, Some(Arc::clone(transport)), None).await
    }

    pub(super) async fn start_with_factory(
        settings: &crate::exomonad::EmbeddedLaunchConfig,
        configure: impl FnOnce(&mut ActorHostConfig),
        transport: impl FnOnce(
                &Arc<embedded_harness::EmbeddedHarnessRuntime>,
                &ActorHostConfig,
            ) -> Arc<dyn harness::engine::ResponsesTransport>
            + Send
            + 'static,
    ) -> Result<Self, String> {
        Self::start_owned(settings, configure, None, Some(Box::new(transport))).await
    }

    async fn start_owned(
        settings: &crate::exomonad::EmbeddedLaunchConfig,
        configure: impl FnOnce(&mut ActorHostConfig),
        transport: Option<Arc<dyn harness::engine::ResponsesTransport>>,
        transport_factory: Option<HostTransportFactory>,
    ) -> Result<Self, String> {
        tidepool_testing::eval_harness::require_extract();
        let repository =
            exomonad_worktree::testing::TestRepo::init().map_err(|error| error.to_string())?;
        repository
            .writer()
            .commit_file("README.md", "source\n", "seed")
            .map_err(|error| error.to_string())?;
        let runtime_directory = tempfile::tempdir().map_err(|error| error.to_string())?;
        let run_root = runtime_directory.path().join("run");
        let mut config = ActorHostConfig {
            systemd_slice: None,
            source_exclude: Vec::new(),
            source_import: Default::default(),
            command_resources: None,
            exomonad_executable: std::env::current_exe().map_err(|error| error.to_string())?,
            workspace_inputs: None,
            haskell_root: crate::haskell_sources::ensure_exomonad_haskell()
                .map_err(|error| error.to_string())?,
            workspace: repository.path().to_path_buf(),
            root_binding_path: run_root.join("root-binding.json"),
            run_root,
            embedded: Some(settings.clone()),
            tmux_session: "unused-hosted-acceptance".into(),
            model: "test-model".into(),
            effort: exomonad_actor::ForkEffort::Low,
            research_policy: exomonad_actor::ResearchPolicy::default(),
            pane_environment: BTreeMap::new(),
            jev: None,
        };
        configure(&mut config);
        let lease =
            HostIncarnationLease::claim(&config.run_root).map_err(|error| error.to_string())?;
        let observer = HostTestObserver::default();
        let (assembled, mut assembly) = oneshot::channel();
        let (stop, stopping) = watch::channel(false);
        let hooks = HostTestHooks {
            observer,
            assembled: Some(assembled),
            stopping,
            transport: transport_factory,
        };
        let (ready, mut readiness) = mpsc::unbounded_channel();
        let (complete, mut outcome) = oneshot::channel();
        // The application retains non-Send errors across its cleanup awaits.
        // Keep that future on this executor, rather than changing its owners.
        let thread = std::thread::Builder::new()
            .name("hosted-acceptance".into())
            .spawn(move || {
                let result = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .map_err(|error| error.to_string())
                    .and_then(|runtime| {
                        runtime.block_on(async move {
                            run_owned(config, ready, lease, transport, Some(hooks))
                                .await
                                .map_err(|error| error.to_string())
                        })
                    });
                let _ = complete.send(result);
            })
            .map_err(|error| error.to_string())?;
        let mut exited_during_startup = false;
        let started = tokio::time::timeout(STARTUP_BUDGET, async {
            let context = tokio::select! {
                assembled = &mut assembly => assembled.map_err(|_| "production assembly observer closed".to_owned())?,
                result = &mut outcome => {
                    exited_during_startup = true;
                    return Err(format!("production host exited during startup: {result:?}"));
                },
            };
            loop {
                tokio::select! {
                    event = readiness.recv() => match event {
                        Some(ActorHostReadiness::EmbeddedReady { root, address }) if root == context.actor.identity() => return Ok((context, address)),
                        Some(ActorHostReadiness::CoordinationFailed { error, .. }) => return Err(error),
                        Some(_) => {},
                        None => return Err("production readiness owner closed".into()),
                    },
                    result = &mut outcome => {
                        exited_during_startup = true;
                        return Err(format!("production host exited before readiness: {result:?}"));
                    },
                }
            }
        }).await;
        let (context, address) = match started {
            Ok(Ok(started)) => started,
            failure => {
                let detail = match failure {
                    Ok(Err(error)) => error,
                    Err(_) => "production startup exceeded its budget".into(),
                    Ok(Ok(_)) => unreachable!(),
                };
                if exited_during_startup {
                    thread
                        .join()
                        .map_err(|_| "production host executor panicked".to_owned())?;
                    return Err(detail);
                }
                stop.send_replace(true);
                let cleanup = tokio::time::timeout(SHUTDOWN_BUDGET, &mut outcome).await;
                return Err(format!(
                    "production startup failed: {detail}; cleanup: {cleanup:?}"
                ));
            }
        };
        let runtime = Arc::clone(&context.runtime);
        Ok(Self {
            context,
            runtime,
            address,
            stop,
            outcome: outcome
                .map(|result| {
                    result.unwrap_or_else(|error| Err(format!("production host outcome: {error}")))
                })
                .boxed()
                .shared(),
            thread: Some(thread),
            _repository: repository,
            _runtime: runtime_directory,
        })
    }

    pub(super) async fn host_outcome(&mut self) -> Result<(), String> {
        self.outcome.clone().await
    }

    pub(super) async fn stop(mut self) -> Result<(), String> {
        self.stop.send_replace(true);
        let outcome = tokio::time::timeout(SHUTDOWN_BUDGET, self.outcome.clone())
            .await
            .map_err(|_| "production host shutdown remains unconfirmed".to_owned())?;
        if let Some(thread) = self.thread.take() {
            thread
                .join()
                .map_err(|_| "production host executor panicked".to_owned())?;
        }
        outcome
    }
}

pub(super) async fn test_stop(hooks: &mut Option<HostTestHooks>) {
    let Some(hooks) = hooks else {
        return std::future::pending().await;
    };
    while !*hooks.stopping.borrow_and_update() {
        if hooks.stopping.changed().await.is_err() {
            return;
        }
    }
}

pub(super) fn cell_output_matches(
    item: &harness::item::Item,
    call_id: &str,
    expected: &str,
) -> bool {
    if item.0["type"] != "custom_tool_call_output" || item.0["call_id"] != call_id {
        return false;
    }
    let Some(output) = item.0["output"].as_str() else {
        return false;
    };
    let Ok(response) = serde_json::from_str::<serde_json::Value>(output) else {
        return false;
    };
    matches!(response["status"].as_str(), Some("completed" | "committed"))
        && response["total"] == 1
        && response["nextIndex"] == 1
        && response["items"].as_array().is_some_and(|items| {
            items.len() == 1
                && items[0]["status"] == "committed"
                && items[0]["output"]
                    .as_str()
                    .is_some_and(|value| value.trim() == expected)
        })
}
