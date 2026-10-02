//! Shared real embedded host composition for HTTP and browser acceptance.

use super::*;

const HOST_STARTUP_TIMEOUT: Duration = Duration::from_secs(60);
const HOST_SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(30);
const FOREST_SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(30);
const HOSTED_SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(30);
const ABORT_JOIN_TIMEOUT: Duration = Duration::from_secs(5);

pub(in crate::actor_host) fn cell_output_matches(
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

pub(in crate::actor_host) struct RunningBrowserHost {
    pub(in crate::actor_host) campaign: test_campaign::TestCampaign,
    pub(in crate::actor_host) runtime: Arc<embedded_harness::EmbeddedHarnessRuntime>,
    pub(in crate::actor_host) address: std::net::SocketAddr,
    shutdown: watch::Sender<Option<NativeRetirement>>,
    // Keep the root configuration watch alive for the entire host lifetime.
    _config_tx: watch::Sender<ActorHostConfig>,
    host: tokio::task::JoinHandle<Result<(), String>>,
    forward: tokio::task::JoinHandle<()>,
}

impl Drop for RunningBrowserHost {
    fn drop(&mut self) {
        if !self.host.is_finished() {
            self.host.abort();
        }
        if !self.campaign.hosted.is_finished() {
            self.campaign.hosted.abort();
        }
        if !self.forward.is_finished() {
            self.forward.abort();
        }
    }
}

impl RunningBrowserHost {
    pub(in crate::actor_host) async fn host_outcome(&mut self) -> Result<(), String> {
        (&mut self.host)
            .await
            .map_err(|error| format!("embedded host task: {error}"))?
    }

    pub(in crate::actor_host) async fn cell_settlement_diagnostic(&self, call_id: &str) -> String {
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
            self.campaign.actor.hosted_cell_computing(),
            claims.len(),
            self.campaign
                .actor
                .terminal()
                .get()
                .map(|terminal| terminal.kind),
            self.campaign.actor.terminal().get().and_then(|terminal| {
                (terminal.kind == exomonad_actor::ActorExitKind::Failed)
                    .then(|| terminal.summary.chars().take(2048).collect::<String>())
            }),
        )
    }

    pub(in crate::actor_host) async fn start(
        settings: &crate::exomonad::EmbeddedLaunchConfig,
        transport: &Arc<dyn harness::engine::ResponsesTransport>,
    ) -> Result<Self, String> {
        Self::start_configured(settings, transport, |_| {}).await
    }

    pub(in crate::actor_host) async fn start_configured(
        settings: &crate::exomonad::EmbeddedLaunchConfig,
        transport: &Arc<dyn harness::engine::ResponsesTransport>,
        configure: impl FnOnce(&mut ActorHostConfig),
    ) -> Result<Self, String> {
        let _ = tracing_subscriber::fmt()
            .with_env_filter(
                tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| {
                    tracing_subscriber::EnvFilter::new("exomonad_actor::workbench_phase=info")
                }),
            )
            .with_test_writer()
            .try_init();
        let mut campaign = test_campaign::TestCampaign::start_with_config(
            exomonad_actor::ResearchPolicy::default(),
            |admission| admission,
            |config| {
                config.backend = crate::exomonad::HostBackendOptions::Embedded;
                config.embedded = Some(settings.clone());
                configure(config);
            },
        )
        .await;
        if let Err(error) = std::fs::create_dir_all(&campaign.config.run_root) {
            let mut errors = vec![format!("create embedded run root: {error}")];
            finish_campaign(&mut campaign, &mut errors).await;
            return Err(errors.join("; "));
        }

        let root_installation = campaign.root_installation.clone();
        let mut service = match tokio::time::timeout(
            HOST_STARTUP_TIMEOUT,
            embedded_service::EmbeddedService::prepare(&campaign.config.run_root, settings),
        )
        .await
        {
            Ok(Ok(service)) => service,
            Ok(Err(error)) => {
                let mut errors = vec![format!("prepare embedded service: {error}")];
                finish_campaign(&mut campaign, &mut errors).await;
                return Err(errors.join("; "));
            }
            Err(_) => {
                let mut errors = vec![format!(
                    "prepare embedded service exceeded {HOST_STARTUP_TIMEOUT:?}"
                )];
                finish_campaign(&mut campaign, &mut errors).await;
                return Err(errors.join("; "));
            }
        };
        service.runtime.configure_context_models(&campaign.config)?;
        service.set_test_transport(Arc::clone(transport));
        let runtime = Arc::clone(&service.runtime);

        let (lifecycle_tx, lifecycle_rx) = mpsc::channel(32);
        if lifecycle_tx
            .send(LocalResidentDeployment::PolicyInstalled(Box::new(
                root_installation.clone(),
            )))
            .await
            .is_err()
        {
            let mut errors = vec!["publish root resident installation: receiver closed".into()];
            if let Err(error) = service.shutdown().await {
                errors.push(format!("stop embedded service: {error}"));
            }
            finish_campaign(&mut campaign, &mut errors).await;
            return Err(errors.join("; "));
        }

        let (readiness_tx, mut readiness_rx) = mpsc::unbounded_channel();
        let (shutdown, shutdown_rx) = watch::channel(None);
        let (config_tx, config_rx) = watch::channel(campaign.config.clone());
        let host_graph_forest = Arc::clone(&campaign.forest);
        #[cfg(feature = "codex-compat")]
        let tmux = match TmuxSession::new(&campaign.config.tmux_session) {
            Ok(tmux) => tmux,
            Err(error) => {
                let mut errors = vec![format!("prepare embedded host tmux state: {error}")];
                if let Err(error) = service.shutdown().await {
                    errors.push(format!("stop embedded service: {error}"));
                }
                finish_campaign(&mut campaign, &mut errors).await;
                return Err(errors.join("; "));
            }
        };
        #[cfg(feature = "codex-compat")]
        let actor_recovery = match exomonad_actor::ActorRecoveryJournal::open(
            campaign.config.run_root.join("actor-lifecycle.v2.jsonl"),
        ) {
            Ok(journal) => journal,
            Err(error) => {
                let mut errors = vec![format!("open actor recovery journal: {error}")];
                if let Err(error) = service.shutdown().await {
                    errors.push(format!("stop embedded service: {error}"));
                }
                finish_campaign(&mut campaign, &mut errors).await;
                return Err(errors.join("; "));
            }
        };
        let fleet = InteractiveFleet {
            provider_forest: Arc::clone(&campaign.forest),
            root: campaign.actor.clone(),
            config: campaign.config.clone(),
            run_root: campaign.config.run_root.clone(),
            #[cfg(feature = "codex-compat")]
            tmux,
            #[cfg(feature = "codex-compat")]
            backend: HostRuntimeMode::Embedded,
            worktrees: campaign.worktrees.clone(),
            #[cfg(feature = "codex-compat")]
            bindings: campaign.bindings.clone(),
            readiness: readiness_tx,
            worktree_authority: campaign.authority.clone(),
            #[cfg(feature = "codex-compat")]
            watch_retention: Arc::new(|_, _| false),
            #[cfg(feature = "codex-compat")]
            watch_observation: Arc::new(|_, _, _| false),
            #[cfg(feature = "codex-compat")]
            open_request: Arc::new(|_| None),
            #[cfg(feature = "codex-compat")]
            source_layers: None,
            #[cfg(feature = "codex-compat")]
            actor_recovery,
            #[cfg(feature = "codex-compat")]
            recovered_threads: Arc::new(BTreeMap::new()),
            #[cfg(feature = "codex-compat")]
            recovered_root_predecessor: None,
            host_graph: Arc::new(move || host_graph_forest.inspect_host_graph()),
        };
        let deployments = campaign.take_deployments();
        let forward = tokio::spawn(async move {
            let mut deployments = deployments;
            while let Some(deployment) = deployments.recv().await {
                if lifecycle_tx.send(deployment).await.is_err() {
                    break;
                }
            }
        });
        let host = tokio::spawn(run_interactive_applications(
            lifecycle_rx,
            Arc::new(Mutex::new(HashMap::new())),
            fleet,
            shutdown_rx,
            config_rx,
            Some(service),
        ));
        let mut running = Self {
            campaign,
            runtime,
            address: std::net::SocketAddr::from(([127, 0, 0, 1], 0)),
            shutdown,
            _config_tx: config_tx,
            host,
            forward,
        };

        let readiness = tokio::time::timeout(HOST_STARTUP_TIMEOUT, readiness_rx.recv()).await;
        let readiness_error = match readiness {
            Ok(Some(ActorHostReadiness::EmbeddedReady { root, address }))
                if root == running.campaign.actor.identity() =>
            {
                running.address = address;
                None
            }
            Ok(Some(ActorHostReadiness::EmbeddedReady { root, .. })) => Some(format!(
                "embedded host reported readiness for unexpected root {root:?}"
            )),
            Ok(Some(other)) => Some(format!("unexpected embedded host readiness: {other:?}")),
            Ok(None) => Some("embedded host exited before readiness".into()),
            Err(_) => Some(format!(
                "embedded host readiness exceeded {HOST_STARTUP_TIMEOUT:?}"
            )),
        };
        if let Some(error) = readiness_error {
            let mut errors = vec![error];
            running.finish(&mut errors).await;
            return Err(errors.join("; "));
        }
        Ok(running)
    }

    pub(in crate::actor_host) async fn stop(mut self) -> Result<(), String> {
        let mut errors = Vec::new();
        self.finish(&mut errors).await;
        if errors.is_empty() {
            Ok(())
        } else {
            Err(errors.join("; "))
        }
    }

    async fn finish(&mut self, errors: &mut Vec<String>) {
        self.shutdown
            .send_replace(Some(NativeRetirement::Terminate));
        match tokio::time::timeout(HOST_SHUTDOWN_TIMEOUT, &mut self.host).await {
            Ok(Ok(Ok(()))) => {}
            Ok(Ok(Err(error))) => errors.push(format!("embedded host failed: {error}")),
            Ok(Err(error)) => errors.push(format!("embedded host task failed: {error}")),
            Err(_) => {
                errors.push(format!(
                    "embedded host did not stop within {HOST_SHUTDOWN_TIMEOUT:?}; abort requested"
                ));
                self.host.abort();
                match tokio::time::timeout(ABORT_JOIN_TIMEOUT, &mut self.host).await {
                    Ok(Ok(Ok(()))) => {}
                    Ok(Ok(Err(error))) => {
                        errors.push(format!("aborted embedded host failed: {error}"));
                    }
                    Ok(Err(error)) if error.is_cancelled() => {
                        errors.push(
                            "embedded host abort was joined; shutdown remains unconfirmed".into(),
                        );
                    }
                    Ok(Err(error)) => errors.push(format!("joining aborted embedded host: {error}")),
                    Err(_) => errors.push(format!(
                        "embedded host abort join exceeded {ABORT_JOIN_TIMEOUT:?}; completion is unconfirmed"
                    )),
                }
            }
        }

        finish_campaign(&mut self.campaign, errors).await;

        self.forward.abort();
        match tokio::time::timeout(ABORT_JOIN_TIMEOUT, &mut self.forward).await {
            Ok(Ok(())) => {}
            Ok(Err(error)) if error.is_cancelled() => {}
            Ok(Err(error)) => errors.push(format!("joining deployment forwarder: {error}")),
            Err(_) => errors.push(format!(
                "deployment forwarder abort join exceeded {ABORT_JOIN_TIMEOUT:?}; completion is unconfirmed"
            )),
        }
    }
}

async fn finish_campaign(campaign: &mut test_campaign::TestCampaign, errors: &mut Vec<String>) {
    match tokio::time::timeout(FOREST_SHUTDOWN_TIMEOUT, campaign.forest.shutdown()).await {
        Ok(outcomes) => {
            for outcome in outcomes.into_iter().filter(|outcome| !outcome.is_confirmed()) {
                errors.push(format!("resident forest cleanup unconfirmed: {outcome:?}"));
            }
        }
        Err(_) => errors.push(format!(
            "resident forest shutdown exceeded {FOREST_SHUTDOWN_TIMEOUT:?}; completion is unconfirmed"
        )),
    }

    match tokio::time::timeout(HOSTED_SHUTDOWN_TIMEOUT, &mut campaign.hosted).await {
        Ok(Ok(())) => {}
        Ok(Err(error)) => errors.push(format!("hosted root task failed: {error}")),
        Err(_) => {
            errors.push(format!(
                "hosted root task did not stop within {HOSTED_SHUTDOWN_TIMEOUT:?}; abort requested"
            ));
            campaign.hosted.abort();
            match tokio::time::timeout(ABORT_JOIN_TIMEOUT, &mut campaign.hosted).await {
                Ok(Ok(())) => {}
                Ok(Err(error)) if error.is_cancelled() => {
                    errors.push("hosted root abort was joined; shutdown remains unconfirmed".into())
                }
                Ok(Err(error)) => errors.push(format!("joining aborted hosted root task: {error}")),
                Err(_) => errors.push(format!(
                    "hosted root abort join exceeded {ABORT_JOIN_TIMEOUT:?}; completion is unconfirmed"
                )),
            }
        }
    }
}
