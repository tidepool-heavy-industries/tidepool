//! Hosted acceptance executes the application entrypoint and observes its owners.

use super::*;
use futures_util::FutureExt;
use serde_json::json;
use std::time::Duration;
use tokio::sync::oneshot;

const STARTUP_BUDGET: Duration = Duration::from_secs(300);
const SHUTDOWN_BUDGET: Duration = Duration::from_secs(60);
const STARTUP_DIAGNOSTIC_SECONDS: &str = "TIDEPOOL_HOSTED_STARTUP_DIAGNOSTIC_SECONDS";

#[derive(Clone, Copy, Debug, serde::Serialize, PartialEq, Eq)]
#[serde(tag = "mode", rename_all = "snake_case")]
enum StartupPolicy {
    Standard,
    Diagnostic { seconds: u64 },
}

impl StartupPolicy {
    fn parse(value: Option<&str>) -> Result<Self, String> {
        match value {
            None => Ok(Self::Standard),
            Some(value) => {
                let seconds = value.parse::<u64>().map_err(|_| {
                    format!("{STARTUP_DIAGNOSTIC_SECONDS} must be an integer number of seconds")
                })?;
                if seconds <= STARTUP_BUDGET.as_secs() || seconds > 600 {
                    return Err(format!("{STARTUP_DIAGNOSTIC_SECONDS} must exceed the standard 300-second budget and be at most 600 seconds"));
                }
                Ok(Self::Diagnostic { seconds })
            }
        }
    }

    fn budget(self) -> Duration {
        match self {
            Self::Standard => STARTUP_BUDGET,
            Self::Diagnostic { seconds } => Duration::from_secs(seconds),
        }
    }
}

#[derive(Clone, Copy, Debug, serde::Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum StartupStage {
    Assembly,
    EmbeddedReadiness,
}

#[derive(Clone, Debug, serde::Serialize, PartialEq, Eq)]
#[serde(tag = "cause", rename_all = "snake_case")]
pub(super) enum StartupFailure {
    Deadline,
    AssemblyObserverClosed,
    ReadinessObserverClosed,
    RootRetired {
        actor: ActorRef,
        terminal: exomonad_actor::ActorTerminal,
    },
    HostExited {
        message: String,
    },
    CoordinationFailed {
        message: String,
    },
}

#[derive(Debug)]
pub(super) enum HostedStartupError {
    Setup(String),
    Refused {
        failure: StartupFailure,
        cleanup: CleanupOutcome,
        detail: String,
    },
}

impl From<String> for HostedStartupError {
    fn from(detail: String) -> Self {
        Self::Setup(detail)
    }
}

impl std::fmt::Display for HostedStartupError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Setup(detail) | Self::Refused { detail, .. } => formatter.write_str(detail),
        }
    }
}

impl std::error::Error for HostedStartupError {}

impl HostedStartupError {
    pub(super) fn root_terminal(&self) -> Option<(ActorRef, &exomonad_actor::ActorTerminal)> {
        match self {
            Self::Refused {
                failure: StartupFailure::RootRetired { actor, terminal },
                ..
            } => Some((*actor, terminal)),
            _ => None,
        }
    }

    pub(super) fn cleanup_confirmed(&self) -> bool {
        matches!(
            self,
            Self::Refused {
                cleanup: CleanupOutcome::Confirmed,
                ..
            }
        )
    }
}

impl StartupFailure {
    fn message(&self) -> String {
        match self {
            Self::Deadline => "production startup exceeded its budget".into(),
            Self::AssemblyObserverClosed => "production assembly observer closed".into(),
            Self::ReadinessObserverClosed => "production readiness owner closed".into(),
            Self::RootRetired { actor, terminal } => format!(
                "production root {actor} exited before embedded readiness ({:?}): {}",
                terminal.kind, terminal.summary
            ),
            Self::HostExited { message } | Self::CoordinationFailed { message } => message.clone(),
        }
    }
}

#[derive(Clone, Debug, serde::Serialize, PartialEq, Eq)]
#[serde(tag = "status", rename_all = "snake_case")]
enum StartupOutcome {
    Pending,
    Ready,
    Failed { failure: StartupFailure },
}

#[derive(Clone, Debug, serde::Serialize)]
struct StartupEvidence {
    policy: StartupPolicy,
    baseline_budget_seconds: u64,
    elapsed_ms: Option<u128>,
    over_baseline_budget: Option<bool>,
    stage: StartupStage,
    outcome: StartupOutcome,
    root_before_cleanup: Option<(ActorRef, Option<exomonad_actor::ActorTerminal>)>,
}

impl StartupEvidence {
    fn pending(policy: StartupPolicy) -> Self {
        Self {
            policy,
            baseline_budget_seconds: STARTUP_BUDGET.as_secs(),
            elapsed_ms: None,
            over_baseline_budget: None,
            stage: StartupStage::Assembly,
            outcome: StartupOutcome::Pending,
            root_before_cleanup: None,
        }
    }
}

type HostOutcome =
    futures_util::future::Shared<futures_util::future::BoxFuture<'static, Result<(), String>>>;
type ScenarioResult = Result<(), Box<dyn std::any::Any + Send>>;

struct HostTermination {
    result: Option<Result<(), String>>,
    joined: Result<(), String>,
}

impl HostTermination {
    fn into_result(self) -> Result<(), String> {
        self.joined?;
        self.result
            .ok_or_else(|| "production host outcome remains unavailable".to_owned())?
    }

    fn startup_failure(
        self,
        failure: StartupFailure,
        assembly_observed: bool,
        owner_admission: HostOwnerAdmission,
    ) -> (String, CleanupOutcome) {
        let detail = failure.message();
        if assembly_observed {
            return (detail, CleanupOutcome::from_result(&self.into_result()));
        }
        // A closed assembly observer can hide its host's own startup refusal.
        // A deadline or an observed failure was established before teardown.
        let detail = match (&failure, &self.result) {
            (StartupFailure::AssemblyObserverClosed, Some(Err(error))) => {
                format!("production host failed during startup: {error}")
            }
            _ => detail,
        };
        let cleanup = match &self.joined {
            Err(error) => CleanupOutcome::from_result(&Err(error.clone())),
            Ok(()) if matches!(self.result, Some(Ok(()))) => CleanupOutcome::Confirmed,
            Ok(())
                if owner_admission == HostOwnerAdmission::NotAdmitted
                    && matches!(self.result, Some(Err(_))) =>
            {
                CleanupOutcome::NotStarted {
                    domain: CleanupDomain::HostRuntime,
                    owner_admission,
                    executor_joined: true,
                }
            }
            // Executor termination does not acknowledge production teardown
            // when startup returned an error before issuing the hosted context.
            Ok(()) => CleanupOutcome::Unknown,
        };
        (detail, cleanup)
    }
}

#[derive(Clone, Debug, serde::Serialize, PartialEq, Eq)]
#[serde(tag = "status", rename_all = "snake_case")]
enum ScenarioOutcome {
    Unknown,
    Passed,
    Failed {
        phase: ScenarioPhase,
        message: String,
    },
}

#[derive(Clone, Debug, serde::Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum ScenarioPhase {
    Startup,
    Scenario,
}

#[derive(Clone, Debug, serde::Serialize, PartialEq, Eq)]
#[serde(tag = "status", rename_all = "snake_case")]
pub(super) enum CleanupOutcome {
    Unknown,
    Confirmed,
    NotStarted {
        domain: CleanupDomain,
        owner_admission: HostOwnerAdmission,
        executor_joined: bool,
    },
    Failed {
        message: String,
    },
}

#[derive(Clone, Debug, serde::Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(super) enum CleanupDomain {
    HostRuntime,
}

#[derive(Clone, Copy, Debug, serde::Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(super) enum HostOwnerAdmission {
    NotAdmitted,
    Admitted,
}

impl CleanupOutcome {
    fn from_result(result: &Result<(), String>) -> Self {
        match result {
            Ok(()) => Self::Confirmed,
            Err(error) => Self::Failed {
                message: error.chars().take(2048).collect(),
            },
        }
    }
}

fn panic_message(payload: &(dyn std::any::Any + Send)) -> String {
    payload
        .downcast_ref::<String>()
        .map(String::as_str)
        .or_else(|| payload.downcast_ref::<&str>().copied())
        .unwrap_or("non-string panic payload")
        .chars()
        .take(2048)
        .collect()
}

/// Reporting failure must never skip the production cleanup owner, or replace
/// an assertion's original panic payload.
async fn settle_scenario<S, C>(
    scenario: S,
    cleanup: C,
    mut report: impl FnMut(&ScenarioOutcome, &CleanupOutcome) -> Result<(), String>,
) -> (ScenarioResult, Result<(), String>, Vec<String>)
where
    S: std::future::Future<Output = ()>,
    C: std::future::Future<Output = Result<(), String>>,
{
    let scenario = std::panic::AssertUnwindSafe(scenario).catch_unwind().await;
    let scenario_outcome = match &scenario {
        Ok(()) => ScenarioOutcome::Passed,
        Err(payload) => ScenarioOutcome::Failed {
            phase: ScenarioPhase::Scenario,
            message: panic_message(payload.as_ref()),
        },
    };
    let mut report_errors = Vec::new();
    if let Err(error) = report(&scenario_outcome, &CleanupOutcome::Unknown) {
        report_errors.push(error);
    }
    let cleanup = match std::panic::AssertUnwindSafe(cleanup).catch_unwind().await {
        Ok(result) => result,
        Err(payload) => Err(format!(
            "production host cleanup panicked: {}",
            panic_message(payload.as_ref())
        )),
    };
    let cleanup_outcome = CleanupOutcome::from_result(&cleanup);
    if let Err(error) = report(&scenario_outcome, &cleanup_outcome) {
        report_errors.push(error);
    }
    (scenario, cleanup, report_errors)
}

#[derive(Clone)]
struct HostedTestDiagnostics {
    root: std::path::PathBuf,
    workspace: std::path::PathBuf,
    run_root: std::path::PathBuf,
    startup: StartupEvidence,
}

impl HostedTestDiagnostics {
    fn root_from_environment() -> Result<Option<std::path::PathBuf>, String> {
        if std::env::var_os("TIDEPOOL_TEST_DIAGNOSTIC_SCOPE").as_deref()
            != Some(std::ffi::OsStr::new("1"))
        {
            return Ok(None);
        }
        let root = std::env::var_os("TIDEPOOL_TEST_ARTIFACT_ROOT")
            .map(std::path::PathBuf::from)
            .ok_or_else(|| "diagnostic scope requires TIDEPOOL_TEST_ARTIFACT_ROOT".to_owned())?;
        if !root.is_absolute() {
            return Err("TIDEPOOL_TEST_ARTIFACT_ROOT must be absolute".into());
        }
        std::fs::create_dir_all(&root).map_err(|error| error.to_string())?;
        let directory = tempfile::Builder::new()
            .prefix("hosted-campaign-")
            .tempdir_in(root)
            .map_err(|error| error.to_string())?;
        Ok(Some(directory.keep()))
    }

    fn report(&self, scenario: &ScenarioOutcome, cleanup: &CleanupOutcome) -> Result<(), String> {
        let bytes = serde_json::to_vec_pretty(&json!({
            "schema": 2,
            "startup": self.startup,
            "scenario": scenario,
            "cleanup": cleanup,
            "workspace": self.workspace,
            "run_root": self.run_root,
        }))
        .map_err(|error| error.to_string())?;
        tidepool_atomic_write::write_durable(&self.root.join("hosted-outcome.json"), &bytes)
            .map_err(|error| error.to_string())
    }
}

#[derive(Clone)]
pub(super) struct ObservedInstallation {
    pub(super) actor: LocalActorRef,
    pub(super) checkpoint: bool,
    pub(super) context_parent: Option<ActorRef>,
    pub(super) tools: Vec<exomonad_tool::HostedTool>,
    pub(super) installed_at: std::time::Instant,
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
                tools: installation.policy.tools().to_vec(),
                installed_at: std::time::Instant::now(),
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
    pub(super) owner_admission: Arc<Mutex<HostOwnerAdmission>>,
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

#[derive(Debug)]
pub(super) struct RootExitedBeforeBarrier {
    pub(super) actor: ActorRef,
    pub(super) terminal: exomonad_actor::ActorTerminal,
    barrier: &'static str,
}

impl std::fmt::Display for RootExitedBeforeBarrier {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            formatter,
            "production root {} exited before {} ({:?}): {}",
            self.actor, self.barrier, self.terminal.kind, self.terminal.summary
        )
    }
}

impl std::error::Error for RootExitedBeforeBarrier {}

impl HostedActorContext {
    pub(super) fn binding(
        &self,
        actor: ActorRef,
    ) -> Option<embedded_harness::EmbeddedActorBinding> {
        embedded_binding(&self.owners, actor)
    }

    /// Guard only barriers that require this exact root incarnation to remain
    /// live. A failed cell or provider round is not an actor terminal, and
    /// independently owned child barriers must observe their own lifecycle.
    pub(super) async fn while_root_live<F: std::future::Future>(
        &self,
        barrier: &'static str,
        future: F,
    ) -> Result<F::Output, RootExitedBeforeBarrier> {
        tokio::select! {
            biased;
            terminal = self.actor.terminal().wait() => Err(RootExitedBeforeBarrier {
                actor: self.actor.identity(),
                terminal,
                barrier,
            }),
            result = future => Ok(result),
        }
    }
}

/// One test executor for the existing production run, including its shutdown.
pub(super) struct HostedTestRuntime {
    pub(super) context: HostedActorContext,
    pub(super) runtime: Arc<embedded_harness::EmbeddedHarnessRuntime>,
    pub(super) address: std::net::SocketAddr,
    stop: watch::Sender<bool>,
    outcome: HostOutcome,
    thread: Option<std::thread::JoinHandle<()>>,
    diagnostics: Option<HostedTestDiagnostics>,
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
    /// Settle assertions and the production host independently. The run and
    /// workspace exist under the case root before execution, so a watchdog kill
    /// leaves its pending report and original inputs available.
    pub(super) async fn run_scenario(
        mut self,
        scenario: impl for<'a> FnOnce(&'a Self) -> futures_util::future::LocalBoxFuture<'a, ()>,
    ) {
        let diagnostics = self.diagnostics.clone();
        let cleanup = Self::shutdown(self.stop.clone(), self.outcome.clone(), self.thread.take());
        let (scenario, cleanup, report_errors) = settle_scenario(
            async { scenario(&self).await },
            cleanup,
            |scenario, cleanup| {
                let Some(diagnostics) = &diagnostics else {
                    return Ok(());
                };
                diagnostics.report(scenario, cleanup)?;
                if matches!(
                    scenario,
                    ScenarioOutcome::Failed {
                        phase: ScenarioPhase::Scenario,
                        ..
                    }
                ) && matches!(cleanup, CleanupOutcome::Unknown)
                {
                    let graph = self
                        .context
                        .forest
                        .inspect_host_graph()
                        .into_iter()
                        .take(64)
                        .map(|node| format!("{node:?}").chars().take(2048).collect::<String>())
                        .collect::<Vec<_>>();
                    let bytes =
                        serde_json::to_vec_pretty(&graph).map_err(|error| error.to_string())?;
                    tidepool_atomic_write::write_durable(
                        &diagnostics.root.join("host-graph-before-cleanup.json"),
                        &bytes,
                    )
                    .map_err(|error| error.to_string())?;
                }
                Ok(())
            },
        )
        .await;
        if let Err(error) = &cleanup {
            eprintln!("hosted campaign cleanup failed: {error}");
        }
        for error in &report_errors {
            eprintln!("hosted campaign evidence failed: {error}");
        }
        if let Err(payload) = scenario {
            std::panic::resume_unwind(payload);
        }
        cleanup.expect("production host cleanup is acknowledged");
        assert!(report_errors.is_empty(), "{report_errors:?}");
    }

    /// Admit ordinary user input through the actual attached root conversation.
    /// Startup remains idle until a scenario explicitly calls this or submits
    /// input through the browser command owner.
    pub(super) async fn input(
        &self,
        text: &str,
    ) -> Result<harness::embedding::InputReceipt, String> {
        let conversation = self
            .context
            .binding(self.context.actor.identity())
            .and_then(|binding| binding.conversation())
            .ok_or_else(|| "production root conversation is not attached".to_owned())?;
        let receipt = conversation
            .input(&uuid::Uuid::new_v4().simple().to_string(), "operator", text)
            .await
            .map_err(|error| error.to_string())?;
        if let Some(error) = &receipt.wake_error {
            return Err(format!(
                "user input {} was admitted but its wake failed: {error}",
                receipt.envelope_id
            ));
        }
        Ok(receipt)
    }

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
    ) -> Result<Self, HostedStartupError> {
        Self::start_configured(settings, transport, |_| {}).await
    }

    pub(super) async fn start_configured(
        settings: &crate::exomonad::EmbeddedLaunchConfig,
        transport: &Arc<dyn harness::engine::ResponsesTransport>,
        configure: impl FnOnce(&mut ActorHostConfig),
    ) -> Result<Self, HostedStartupError> {
        Self::start_owned(
            settings,
            configure,
            Some(Arc::clone(transport)),
            None,
            false,
            None,
        )
        .await
    }

    async fn start_configured_with_diagnostics(
        settings: &crate::exomonad::EmbeddedLaunchConfig,
        transport: &Arc<dyn harness::engine::ResponsesTransport>,
        configure: impl FnOnce(&mut ActorHostConfig),
        diagnostics_root: std::path::PathBuf,
    ) -> Result<Self, HostedStartupError> {
        Self::start_owned(
            settings,
            configure,
            Some(Arc::clone(transport)),
            None,
            false,
            Some(diagnostics_root),
        )
        .await
    }

    pub(super) async fn start_prepared_configured(
        settings: &crate::exomonad::EmbeddedLaunchConfig,
        transport: &Arc<dyn harness::engine::ResponsesTransport>,
        configure: impl FnOnce(&mut ActorHostConfig),
    ) -> Result<Self, HostedStartupError> {
        Self::start_owned(settings, configure, Some(Arc::clone(transport)), None, true, None).await
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
    ) -> Result<Self, HostedStartupError> {
        Self::start_owned(settings, configure, None, Some(Box::new(transport)), false, None).await
    }

    async fn start_owned(
        settings: &crate::exomonad::EmbeddedLaunchConfig,
        configure: impl FnOnce(&mut ActorHostConfig),
        transport: Option<Arc<dyn harness::engine::ResponsesTransport>>,
        transport_factory: Option<HostTransportFactory>,
        prepare: bool,
        diagnostics_root_override: Option<std::path::PathBuf>,
    ) -> Result<Self, HostedStartupError> {
        super::test_campaign::install_tracing();
        let startup_policy = StartupPolicy::parse(
            std::env::var(STARTUP_DIAGNOSTIC_SECONDS)
                .map(Some)
                .or_else(|error| match error {
                    std::env::VarError::NotPresent => Ok(None),
                    error => Err(error.to_string()),
                })?
                .as_deref(),
        )?;
        let mut settings = settings.clone();
        if let Some(root) = std::env::var_os("EXOMONAD_EMBEDDED_ASSET_ROOT") {
            settings.asset_root = std::path::PathBuf::from(root);
        }
        settings.validate().map_err(|error| error.to_string())?;
        tidepool_testing::eval_harness::require_extract();
        let diagnostic_root = match diagnostics_root_override {
            Some(root) => {
                if !root.is_absolute() {
                    return Err("hosted test diagnostics root must be absolute".into());
                }
                std::fs::create_dir_all(&root).map_err(|error| error.to_string())?;
                Some(root)
            }
            None => HostedTestDiagnostics::root_from_environment()?,
        };
        let mut repository = match &diagnostic_root {
            Some(root) => exomonad_worktree::testing::TestRepo::init_in(root),
            None => exomonad_worktree::testing::TestRepo::init(),
        }
        .map_err(|error| error.to_string())?;
        repository.disable_cleanup(diagnostic_root.is_some());
        repository
            .writer()
            .commit_file("README.md", "source\n", "seed")
            .map_err(|error| error.to_string())?;
        let mut runtime_directory = match &diagnostic_root {
            Some(root) => tempfile::Builder::new().prefix("runtime-").tempdir_in(root),
            None => tempfile::tempdir(),
        }
        .map_err(|error| error.to_string())?;
        runtime_directory.disable_cleanup(diagnostic_root.is_some());
        let run_directory =
            tidepool_atomic_write::DirectoryAnchor::open_existing(runtime_directory.path())
                .and_then(|root| root.child("exomonad/runs/run"))
                .map_err(|error| error.to_string())?;
        let run_root = run_directory.path().to_path_buf();
        let mut diagnostics = diagnostic_root.map(|root| HostedTestDiagnostics {
            root,
            workspace: repository.path().to_path_buf(),
            run_root: run_root.clone(),
            startup: StartupEvidence::pending(startup_policy),
        });
        if let Some(diagnostics) = &diagnostics {
            diagnostics.report(&ScenarioOutcome::Unknown, &CleanupOutcome::Unknown)?;
        }
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
            run_directory,
            embedded: Some(settings),
            tmux_session: "unused-hosted-acceptance".into(),
            model: "test-model".into(),
            effort: exomonad_actor::ForkEffort::Low,
            research_policy: exomonad_actor::ResearchPolicy::default(),
            pane_environment: BTreeMap::new(),
            jev: None,
        };
        configure(&mut config);
        if prepare {
            let started = std::time::Instant::now();
            let directory = Arc::new(
                config
                    .run_directory
                    .child("prepared-deployment")
                    .map_err(|error| error.to_string())?,
            );
            let mut frozen = crate::exomonad::workspace::FrozenWorkspace::begin_preparation(
                &config.workspace,
                &directory,
            )
            .map_err(|error| error.to_string())?;
            let source = Arc::new(
                crate::exomonad::source::ExomonadSourceReload::new_owned(
                    frozen.clone(),
                    config.workspace.clone(),
                    directory.path().to_owned(),
                    frozen.runtime_actors(),
                    crate::exomonad::source::SourceRootOwner::Prepared(Arc::clone(&directory)),
                )
                .map_err(|error| error.to_string())?,
            );
            let entries = super::prepare_workspace_toolsets(
                &config.workspace,
                &directory,
                frozen.clone(),
                source,
            )
            .await
            .map_err(|error| error.to_string())?;
            let revision = crate::exomonad::source::SourceLayer::new(directory.path())
                .read_active()
                .map_err(|error| error.to_string())?
                .ok_or_else(|| {
                    "production preparation did not settle its source revision".to_owned()
                })?;
            frozen
                .complete_preparation(&directory, revision.identity, entries)
                .map_err(|error| error.to_string())?;
            eprintln!(
                "prepared-runtime-first-preparation {}",
                serde_json::json!({
                    "elapsed_ns": started.elapsed().as_nanos(), "deployment": directory.path(),
                    "completed": true,
                })
            );
            config.workspace_inputs = Some(
                crate::exomonad::workspace::FrozenWorkspace::select_prepared(
                    &config.workspace,
                    config.run_directory.path(),
                    Some(directory.path()),
                )
                .map_err(|error| error.to_string())?,
            );
        }
        if let Some(diagnostics) = &mut diagnostics {
            diagnostics.workspace = config.workspace.clone();
            diagnostics.run_root = config.run_directory.path().to_path_buf();
            diagnostics.report(&ScenarioOutcome::Unknown, &CleanupOutcome::Unknown)?;
        }
        let lease = HostIncarnationLease::claim(&config.run_directory)
            .map_err(|error| error.to_string())?;
        let observer = HostTestObserver::default();
        let (assembled, mut assembly) = oneshot::channel();
        let (stop, stopping) = watch::channel(false);
        let owner_admission = Arc::new(Mutex::new(HostOwnerAdmission::NotAdmitted));
        let hooks = HostTestHooks {
            observer,
            assembled: Some(assembled),
            stopping,
            transport: transport_factory,
            owner_admission: Arc::clone(&owner_admission),
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
        let mut exited_during_startup = None;
        let mut assembly_observed = false;
        let mut observed_root = None;
        let started_at = std::time::Instant::now();
        let started = tokio::time::timeout(startup_policy.budget(), async {
            let context = tokio::select! {
                assembled = &mut assembly => assembled.map_err(|_| StartupFailure::AssemblyObserverClosed)?,
                result = &mut outcome => {
                    let result = result.unwrap_or_else(|error| Err(error.to_string()));
                    let detail = match &result {
                        Err(error) => format!("production host failed during startup: {error}"),
                        Ok(()) => "production host exited during startup: Ok(())".into(),
                    };
                    exited_during_startup = Some(result);
                    return Err(StartupFailure::HostExited { message: detail });
                },
            };
            assembly_observed = true;
            observed_root = Some(context.actor.clone());
            let address = context.while_root_live("embedded readiness", async {
                loop {
                    tokio::select! {
                        event = readiness.recv() => match event {
                            Some(ActorHostReadiness::EmbeddedReady { root, address }) if root == context.actor.identity() => return Ok(address),
                            Some(ActorHostReadiness::CoordinationFailed { error, .. }) => return Err(StartupFailure::CoordinationFailed { message: error }),
                            Some(_) => {},
                            None => return Err(StartupFailure::ReadinessObserverClosed),
                        },
                        result = &mut outcome => {
                            let result = result.unwrap_or_else(|error| Err(error.to_string()));
                            let detail = format!("production host exited before readiness: {result:?}");
                            exited_during_startup = Some(result);
                            return Err(StartupFailure::HostExited { message: detail });
                        },
                    }
                }
            }).await.map_err(|error| StartupFailure::RootRetired {
                actor: error.actor,
                terminal: error.terminal,
            })??;
            Ok((context, address))
        }).await;
        let elapsed = started_at.elapsed();
        if let Some(diagnostics) = &mut diagnostics {
            diagnostics.startup.elapsed_ms = Some(elapsed.as_millis());
            diagnostics.startup.over_baseline_budget = Some(elapsed > STARTUP_BUDGET);
            diagnostics.startup.stage = if assembly_observed {
                StartupStage::EmbeddedReadiness
            } else {
                StartupStage::Assembly
            };
            diagnostics.startup.root_before_cleanup = observed_root
                .as_ref()
                .map(|actor| (actor.identity(), actor.terminal().get()));
        }
        let (context, address) = match started {
            Ok(Ok(started)) => {
                if let Some(diagnostics) = &mut diagnostics {
                    diagnostics.startup.outcome = StartupOutcome::Ready;
                    if let Err(error) =
                        diagnostics.report(&ScenarioOutcome::Unknown, &CleanupOutcome::Unknown)
                    {
                        eprintln!("startup readiness evidence failed: {error}");
                    }
                }
                started
            }
            failure => {
                let failure = match failure {
                    Ok(Err(error)) => error,
                    Err(_) => StartupFailure::Deadline,
                    Ok(Ok(_)) => unreachable!(),
                };
                if let Some(diagnostics) = &mut diagnostics {
                    diagnostics.startup.outcome = StartupOutcome::Failed {
                        failure: failure.clone(),
                    };
                    // Preserve the pre-cleanup observation even if teardown cannot join.
                    if let Err(error) = diagnostics.report(
                        &ScenarioOutcome::Failed {
                            phase: ScenarioPhase::Startup,
                            message: failure.message().chars().take(2048).collect(),
                        },
                        &CleanupOutcome::Unknown,
                    ) {
                        eprintln!("startup evidence before cleanup failed: {error}");
                    }
                }
                let outcome: HostOutcome = match exited_during_startup {
                    Some(result) => futures_util::future::ready(result).boxed().shared(),
                    None => outcome
                        .map(|result| result.unwrap_or_else(|error| Err(error.to_string())))
                        .boxed()
                        .shared(),
                };
                let termination = Self::terminate(stop.clone(), outcome, Some(thread)).await;
                let (detail, cleanup) = termination.startup_failure(
                    failure.clone(),
                    assembly_observed,
                    *owner_admission.lock(),
                );
                let evidence = if let Some(diagnostics) = &diagnostics {
                    diagnostics.report(
                        &ScenarioOutcome::Failed {
                            phase: ScenarioPhase::Startup,
                            message: detail.chars().take(2048).collect(),
                        },
                        &cleanup,
                    )
                } else {
                    Ok(())
                };
                return Err(HostedStartupError::Refused { failure, detail: format!(
                    "production startup failed: {detail}; cleanup: {cleanup:?}; evidence: {evidence:?}"
                ), cleanup });
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
            diagnostics,
            _repository: repository,
            _runtime: runtime_directory,
        })
    }

    pub(super) async fn host_outcome(&mut self) -> Result<(), String> {
        self.outcome.clone().await
    }

    pub(super) async fn stop(mut self) -> Result<(), String> {
        let outcome =
            Self::shutdown(self.stop.clone(), self.outcome.clone(), self.thread.take()).await;
        if let Some(diagnostics) = &self.diagnostics {
            diagnostics.report(
                &ScenarioOutcome::Passed,
                &CleanupOutcome::from_result(&outcome),
            )?;
        }
        outcome
    }

    async fn shutdown(
        stop: watch::Sender<bool>,
        outcome: HostOutcome,
        thread: Option<std::thread::JoinHandle<()>>,
    ) -> Result<(), String> {
        Self::terminate(stop, outcome, thread).await.into_result()
    }

    async fn terminate(
        stop: watch::Sender<bool>,
        outcome: HostOutcome,
        thread: Option<std::thread::JoinHandle<()>>,
    ) -> HostTermination {
        stop.send_replace(true);
        let result = match tokio::time::timeout(SHUTDOWN_BUDGET, outcome).await {
            Ok(result) => result,
            Err(_) => {
                return HostTermination {
                    result: None,
                    joined: Err("production host shutdown remains unconfirmed".to_owned()),
                };
            }
        };
        let joined = if let Some(thread) = thread {
            thread
                .join()
                .map_err(|_| "production host executor panicked".to_owned())
        } else {
            Ok(())
        };
        HostTermination {
            result: Some(result),
            joined,
        }
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

#[cfg(test)]
mod tests {
    use super::*;

    async fn terminated_host(panic: bool) -> (HostTermination, watch::Receiver<bool>) {
        let (stop, stopping) = watch::channel(false);
        let (complete, outcome) = oneshot::channel();
        let thread = std::thread::spawn(move || {
            complete
                .send(Err("injected startup refusal".to_owned()))
                .unwrap();
            assert!(!panic, "injected executor panic");
        });
        let termination = HostedTestRuntime::terminate(
            stop,
            outcome
                .map(|result| result.unwrap_or_else(|error| Err(error.to_string())))
                .boxed()
                .shared(),
            Some(thread),
        )
        .await;
        (termination, stopping)
    }

    #[tokio::test]
    async fn startup_host_error_is_primary_after_executor_join() {
        let (termination, stopping) = terminated_host(false).await;
        assert!(*stopping.borrow());
        assert!(termination.joined.is_ok());
        let (message, cleanup) = termination.startup_failure(
            StartupFailure::AssemblyObserverClosed,
            false,
            HostOwnerAdmission::Admitted,
        );
        assert_eq!(
            message,
            "production host failed during startup: injected startup refusal"
        );
        assert_eq!(cleanup, CleanupOutcome::Unknown);
    }

    #[tokio::test]
    async fn joined_startup_refusal_before_owner_admission_never_claims_cleanup() {
        let (termination, stopping) = terminated_host(false).await;
        assert!(*stopping.borrow());
        let (_, cleanup) = termination.startup_failure(
            StartupFailure::AssemblyObserverClosed,
            false,
            HostOwnerAdmission::NotAdmitted,
        );
        assert_eq!(
            cleanup,
            CleanupOutcome::NotStarted {
                domain: CleanupDomain::HostRuntime,
                owner_admission: HostOwnerAdmission::NotAdmitted,
                executor_joined: true,
            }
        );
        let directory = tempfile::tempdir().unwrap();
        diagnostics(&directory)
            .report(
                &ScenarioOutcome::Failed {
                    phase: ScenarioPhase::Startup,
                    message: "refused".into(),
                },
                &cleanup,
            )
            .unwrap();
        let report = report(&directory);
        assert_eq!(report["cleanup"]["status"], "not_started");
        assert_eq!(report["cleanup"]["domain"], "host_runtime");
        assert_eq!(report["cleanup"]["owner_admission"], "not_admitted");
        assert_eq!(report["cleanup"]["executor_joined"], true);
    }

    #[test]
    fn owner_admission_precedes_partial_worktree_resource_creation() {
        let files = tempfile::tempdir().unwrap();
        let run_directory = tidepool_atomic_write::DirectoryAnchor::open_existing(files.path())
            .unwrap()
            .child("exomonad/runs/run")
            .unwrap();
        run_directory.create_dir_all("").unwrap();
        let repository = exomonad_worktree::testing::TestRepo::init().unwrap();
        let root = actor_worktree_storage_root(repository.path(), run_directory.path()).unwrap();
        std::fs::create_dir_all(root.parent().unwrap()).unwrap();
        std::fs::write(&root, "obstruct resource directory").unwrap();
        let mut admitted = false;
        assert!(
            actor_worktree_resources(repository.path(), &run_directory, || admitted = true)
                .is_err()
        );
        assert!(
            admitted,
            "partial resource construction must cross the admission fence"
        );
        let mut admitted = false;
        assert!(
            actor_worktree_resources(files.path(), &run_directory, || admitted = true).is_err()
        );
        assert!(
            !admitted,
            "Git ownership refusal must precede resource admission"
        );
    }

    #[tokio::test]
    async fn production_startup_refusal_preserves_worktree_error() {
        let files = tempfile::tempdir().unwrap();
        let settings = super::super::test_campaign::hosted_test_settings(&files, 1);
        let (transport, _requests) = super::super::test_campaign::hosted_script_provider();
        let workspace = files.path().join("not-a-repository");
        std::fs::create_dir(&workspace).unwrap();
        let failure = match HostedTestRuntime::start_configured(&settings, &transport, |config| {
            config.workspace = workspace.clone();
        })
        .await
        {
            Ok(_) => panic!("startup must refuse a workspace without Git ownership"),
            Err(error) => error,
        };
        assert!(
            matches!(
                &failure,
                HostedStartupError::Refused {
                    failure: cause,
                    cleanup: CleanupOutcome::NotStarted {
                        owner_admission: HostOwnerAdmission::NotAdmitted,
                        executor_joined: true,
                        ..
                    },
                    ..
                } if matches!(cause,
                    StartupFailure::AssemblyObserverClosed | StartupFailure::HostExited { .. })
            ),
            "{failure:?}"
        );
        assert!(failure.root_terminal().is_none());
        assert!(
            failure
                .to_string()
                .contains(&workspace.display().to_string()),
            "{failure}"
        );
    }

    #[tokio::test]
    async fn startup_host_error_and_executor_failure_are_independent() {
        let (termination, _) = terminated_host(true).await;
        let (message, cleanup) = termination.startup_failure(
            StartupFailure::AssemblyObserverClosed,
            false,
            HostOwnerAdmission::Admitted,
        );
        assert_eq!(
            message,
            "production host failed during startup: injected startup refusal"
        );
        assert_eq!(
            cleanup,
            CleanupOutcome::Failed {
                message: "production host executor panicked".into(),
            }
        );
    }

    #[test]
    fn successful_host_acknowledges_cleanup_after_startup_observer_failure() {
        let termination = HostTermination {
            result: Some(Ok(())),
            joined: Ok(()),
        };
        let (message, cleanup) = termination.startup_failure(
            StartupFailure::AssemblyObserverClosed,
            false,
            HostOwnerAdmission::Admitted,
        );
        assert_eq!(message, "production assembly observer closed");
        assert_eq!(cleanup, CleanupOutcome::Confirmed);
    }

    #[test]
    fn observed_assembly_preserves_primary_readiness_failure_and_cleanup_error() {
        let termination = HostTermination {
            result: Some(Err("injected production cleanup failure".into())),
            joined: Ok(()),
        };
        let (message, cleanup) = termination.startup_failure(
            StartupFailure::CoordinationFailed {
                message: "injected readiness failure".into(),
            },
            true,
            HostOwnerAdmission::Admitted,
        );
        assert_eq!(message, "injected readiness failure");
        assert_eq!(
            cleanup,
            CleanupOutcome::Failed {
                message: "injected production cleanup failure".into(),
            }
        );
    }

    #[test]
    fn startup_policy_requires_an_explicit_finite_diagnostic_allowance() {
        assert_eq!(StartupPolicy::parse(None).unwrap(), StartupPolicy::Standard);
        assert_eq!(StartupPolicy::Standard.budget(), Duration::from_secs(300));
        assert_eq!(
            StartupPolicy::parse(Some("600")).unwrap(),
            StartupPolicy::Diagnostic { seconds: 600 }
        );
        for invalid in ["", "0", "300", "601", "18446744073709551616", "NaN"] {
            assert!(StartupPolicy::parse(Some(invalid)).is_err(), "{invalid}");
        }
    }

    #[test]
    fn startup_deadline_remains_primary_when_teardown_retires_the_root() {
        for assembly_observed in [false, true] {
            let termination = HostTermination {
                result: Some(Err(
                    "actor retired before machine admission: forest host shutdown".into(),
                )),
                joined: Ok(()),
            };
            let (message, cleanup) = termination.startup_failure(
                StartupFailure::Deadline,
                assembly_observed,
                HostOwnerAdmission::Admitted,
            );
            assert_eq!(message, "production startup exceeded its budget");
            assert_eq!(
                cleanup,
                if assembly_observed {
                    CleanupOutcome::Failed {
                        message: "actor retired before machine admission: forest host shutdown"
                            .into(),
                    }
                } else {
                    CleanupOutcome::Unknown
                }
            );
        }
    }

    #[test]
    fn root_retirement_before_cleanup_keeps_its_exact_cause() {
        let actor = ActorRef::first(exomonad_actor::ActorId(7));
        let terminal = exomonad_actor::ActorTerminal {
            kind: exomonad_actor::ActorExitKind::Failed,
            summary: "tool installation refused".into(),
            diagnostic: None,
        };
        let failure = StartupFailure::RootRetired {
            actor,
            terminal: terminal.clone(),
        };
        let termination = HostTermination {
            result: Some(Err("later teardown refusal".into())),
            joined: Ok(()),
        };
        let (message, cleanup) =
            termination.startup_failure(failure.clone(), true, HostOwnerAdmission::Admitted);
        assert!(message.contains("7@1") && message.contains("tool installation refused"));
        assert!(!message.contains("later teardown refusal"));
        assert_eq!(
            cleanup,
            CleanupOutcome::Failed {
                message: "later teardown refusal".into()
            }
        );
        let directory = tempfile::tempdir().unwrap();
        let mut diagnostics = diagnostics(&directory);
        diagnostics.startup.outcome = StartupOutcome::Failed { failure };
        diagnostics.startup.root_before_cleanup = Some((actor, Some(terminal)));
        diagnostics
            .report(
                &ScenarioOutcome::Failed {
                    phase: ScenarioPhase::Startup,
                    message,
                },
                &cleanup,
            )
            .unwrap();
        let recorded = report(&directory);
        assert_eq!(
            recorded["startup"]["outcome"]["failure"]["cause"],
            "root_retired"
        );
        assert_eq!(
            recorded["startup"]["root_before_cleanup"][1]["summary"],
            "tool installation refused"
        );
    }

    #[test]
    fn diagnostic_startup_evidence_survives_scenario_and_cleanup_reporting() {
        let directory = tempfile::tempdir().unwrap();
        let mut diagnostics = diagnostics(&directory);
        diagnostics.startup = StartupEvidence {
            policy: StartupPolicy::Diagnostic { seconds: 600 },
            baseline_budget_seconds: 300,
            elapsed_ms: Some(310_000),
            over_baseline_budget: Some(true),
            stage: StartupStage::EmbeddedReadiness,
            outcome: StartupOutcome::Ready,
            root_before_cleanup: None,
        };
        diagnostics
            .report(&ScenarioOutcome::Passed, &CleanupOutcome::Confirmed)
            .unwrap();
        let recorded = report(&directory);
        assert_eq!(recorded["schema"], 2);
        assert_eq!(
            recorded["startup"]["policy"],
            json!({"mode": "diagnostic", "seconds": 600})
        );
        assert_eq!(recorded["startup"]["elapsed_ms"], 310_000);
        assert_eq!(recorded["startup"]["over_baseline_budget"], true);
        assert_eq!(recorded["startup"]["outcome"]["status"], "ready");
        diagnostics.startup.outcome = StartupOutcome::Failed {
            failure: StartupFailure::Deadline,
        };
        diagnostics
            .report(
                &ScenarioOutcome::Failed {
                    phase: ScenarioPhase::Startup,
                    message: StartupFailure::Deadline.message(),
                },
                &CleanupOutcome::Confirmed,
            )
            .unwrap();
        assert_eq!(
            report(&directory)["startup"]["outcome"]["failure"]["cause"],
            "deadline"
        );
    }

    fn diagnostics(directory: &tempfile::TempDir) -> HostedTestDiagnostics {
        HostedTestDiagnostics {
            root: directory.path().to_path_buf(),
            workspace: directory.path().join("workspace"),
            run_root: directory.path().join("run"),
            startup: StartupEvidence::pending(StartupPolicy::Standard),
        }
    }

    fn report(directory: &tempfile::TempDir) -> serde_json::Value {
        serde_json::from_slice(
            &std::fs::read(directory.path().join("hosted-outcome.json")).unwrap(),
        )
        .unwrap()
    }

    #[tokio::test]
    async fn post_start_scenario_panic_retains_failure_and_confirms_host_cleanup() {
        let directory = tempfile::tempdir().unwrap();
        let settings = super::super::test_campaign::hosted_test_settings(&directory, 1);
        let (transport, _requests) = super::super::test_campaign::hosted_script_provider();
        let host = HostedTestRuntime::start_configured_with_diagnostics(
            &settings,
            &transport,
            |_| {},
            directory.path().to_path_buf(),
        )
        .await
        .expect("production host starts before the deliberate scenario failure");
        let root = host.context.actor.clone();
        let forest = Arc::clone(&host.context.forest);
        let failure = std::panic::AssertUnwindSafe(host.run_scenario(|_| {
            Box::pin(async { panic!("injected post-start assertion failure") })
        }))
        .catch_unwind()
        .await;

        assert!(failure.is_err(), "run_scenario preserves the assertion panic");
        let recorded = report(&directory);
        assert_eq!(recorded["scenario"]["status"], "failed");
        assert_eq!(recorded["scenario"]["phase"], "scenario");
        assert_eq!(
            recorded["scenario"]["message"],
            "injected post-start assertion failure"
        );
        assert_eq!(recorded["cleanup"]["status"], "confirmed");
        let terminal = root.terminal().get().expect("root owner publishes shutdown");
        assert!(
            root.terminal()
                .cleanup()
                .is_some_and(|cleanup| cleanup.is_confirmed()),
            "production root cleanup is semantically confirmed: {terminal:?}"
        );
        assert_eq!(
            forest
                .measurement_snapshot()
                .and_then(|snapshot| snapshot.parked),
            Some(0),
            "production shutdown releases resident parked work"
        );
    }

    #[tokio::test]
    async fn scenario_panic_waits_for_cleanup_and_preserves_original_payload() {
        let directory = tempfile::tempdir().unwrap();
        let diagnostics = diagnostics(&directory);
        let (acknowledge, acknowledged) = oneshot::channel();
        let worker = tokio::spawn(async move {
            tokio::task::yield_now().await;
            acknowledge.send(()).unwrap();
        });
        let (scenario, cleanup, evidence) = settle_scenario(
            async { std::panic::panic_any(37_u32) },
            async {
                // Scenario failure has been retained before cleanup starts.
                assert_eq!(report(&directory)["scenario"]["status"], "failed");
                assert_eq!(report(&directory)["cleanup"]["status"], "unknown");
                acknowledged.await.unwrap();
                worker.await.unwrap();
                Ok(())
            },
            |scenario, cleanup| diagnostics.report(scenario, cleanup),
        )
        .await;
        let payload = scenario.unwrap_err();
        assert_eq!(payload.downcast_ref::<u32>(), Some(&37));
        assert!(cleanup.is_ok());
        assert!(evidence.is_empty());
        assert_eq!(report(&directory)["scenario"]["phase"], "scenario");
        assert_eq!(report(&directory)["cleanup"]["status"], "confirmed");
    }

    #[tokio::test]
    async fn scenario_and_cleanup_failures_are_retained_independently() {
        let directory = tempfile::tempdir().unwrap();
        let diagnostics = diagnostics(&directory);
        let (scenario, cleanup, evidence) = settle_scenario(
            async { panic!("injected scenario failure") },
            async { Err("injected cleanup refusal".to_owned()) },
            |scenario, cleanup| diagnostics.report(scenario, cleanup),
        )
        .await;
        assert_eq!(
            panic_message(scenario.unwrap_err().as_ref()),
            "injected scenario failure"
        );
        assert_eq!(cleanup.unwrap_err(), "injected cleanup refusal");
        assert!(evidence.is_empty());
        let report = report(&directory);
        assert_eq!(report["scenario"]["status"], "failed");
        assert_eq!(report["scenario"]["message"], "injected scenario failure");
        assert_eq!(report["cleanup"]["status"], "failed");
        assert_eq!(report["cleanup"]["message"], "injected cleanup refusal");
    }

    #[tokio::test]
    async fn successful_scenario_does_not_hide_cleanup_panic() {
        let directory = tempfile::tempdir().unwrap();
        let diagnostics = diagnostics(&directory);
        let (scenario, cleanup, evidence) = settle_scenario(
            async {},
            async { panic!("injected cleanup panic") },
            |scenario, cleanup| diagnostics.report(scenario, cleanup),
        )
        .await;
        assert!(scenario.is_ok());
        assert_eq!(
            cleanup.unwrap_err(),
            "production host cleanup panicked: injected cleanup panic"
        );
        assert!(evidence.is_empty());
        assert_eq!(report(&directory)["scenario"]["status"], "passed");
        assert_eq!(report(&directory)["cleanup"]["status"], "failed");
    }

    #[tokio::test]
    async fn evidence_failure_still_awaits_cleanup() {
        let mut stopped = false;
        let (scenario, cleanup, evidence) = settle_scenario(
            async { panic!("injected scenario failure") },
            async {
                tokio::task::yield_now().await;
                stopped = true;
                Ok(())
            },
            |_, _| Err("injected evidence failure".into()),
        )
        .await;
        assert!(scenario.is_err());
        assert!(cleanup.is_ok());
        assert!(stopped);
        assert_eq!(evidence, vec!["injected evidence failure"; 2]);
    }
}
