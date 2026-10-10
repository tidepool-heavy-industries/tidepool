use std::sync::Arc;

use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use tokio::sync::watch;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ActorExitKind {
    Completed,
    Failed,
    Cancelled,
}

/// Immutable lifecycle result for one exact actor incarnation.
///
/// This serializable metadata describes lifecycle for Rust and Haskell actors.
/// Typed Haskell domain values are retained by `RetainedActorExit` as a
/// runtime-issued result snapshot, separately from generic Rust completion.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ActorTerminal {
    pub kind: ActorExitKind,
    pub summary: String,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        serialize_with = "serialize_failure_diagnostic",
        deserialize_with = "deserialize_failure_diagnostic"
    )]
    pub diagnostic: Option<tidepool_toolchain::failclass::FailureEnvelope>,
}

const RETAINED_FAILURE_MESSAGE_BYTES: usize = 16 * 1024;

pub(crate) fn retain_failure_diagnostic(
    diagnostic: Option<tidepool_toolchain::failclass::FailureEnvelope>,
) -> Option<tidepool_toolchain::failclass::FailureEnvelope> {
    diagnostic.map(|mut diagnostic| {
        diagnostic.message = crate::workbench_display::bounded_output(
            &diagnostic.message,
            RETAINED_FAILURE_MESSAGE_BYTES,
        );
        diagnostic
    })
}

fn serialize_failure_diagnostic<S: serde::Serializer>(
    diagnostic: &Option<tidepool_toolchain::failclass::FailureEnvelope>,
    serializer: S,
) -> Result<S::Ok, S::Error> {
    use serde::ser::SerializeStruct;
    let Some(diagnostic) = diagnostic else {
        return serializer.serialize_none();
    };
    let mut state = serializer.serialize_struct("ActorFailureDiagnostic", 4)?;
    state.serialize_field("class", &diagnostic.class)?;
    state.serialize_field("phase", &diagnostic.phase)?;
    state.serialize_field("cause", &diagnostic.cause)?;
    state.serialize_field("message", &diagnostic.message)?;
    state.end()
}

fn deserialize_failure_diagnostic<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<tidepool_toolchain::failclass::FailureEnvelope>, D::Error> {
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Fields {
        class: tidepool_toolchain::failclass::FailureClass,
        phase: tidepool_toolchain::failclass::Phase,
        cause: Option<tidepool_toolchain::failclass::CompileFailureCause>,
        message: String,
    }
    let fields = Option::<Fields>::deserialize(deserializer)?;
    Ok(retain_failure_diagnostic(fields.map(|fields| {
        tidepool_toolchain::failclass::FailureEnvelope {
            class: fields.class,
            phase: fields.phase,
            cause: fields.cause,
            message: fields.message,
        }
    })))
}

impl ActorTerminal {
    pub fn new(kind: ActorExitKind, summary: impl Into<String>) -> Self {
        Self {
            kind,
            summary: summary.into(),
            diagnostic: None,
        }
    }

    pub(crate) fn failed(
        summary: String,
        diagnostic: Option<tidepool_toolchain::failclass::FailureEnvelope>,
    ) -> Self {
        Self {
            kind: ActorExitKind::Failed,
            summary,
            diagnostic,
        }
        .bound_diagnostic()
    }

    pub(crate) fn bound_diagnostic(mut self) -> Self {
        self.diagnostic = retain_failure_diagnostic(self.diagnostic);
        self
    }
}

/// Runtime lifecycle facts; `Live` does not claim application readiness.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ActorLifecycle {
    Live,
    Paused(String),
    Exited(ActorTerminal),
}

type LifecycleSink = dyn Fn(ActorLifecycle) -> bool + Send + Sync;

/// Retaining the connection keeps publications attached. Its sink must only
/// admit a mailbox message: publication holds the lifecycle owner's lock.
pub struct ActorLifecycleConnection {
    _sink: Arc<LifecycleSink>,
}

struct LifecycleState {
    current: ActorLifecycle,
    connections: Vec<std::sync::Weak<LifecycleSink>>,
    result: Option<Arc<crate::owned_result::OwnedResultSnapshot>>,
}

impl LifecycleState {
    fn publish(&mut self, current: ActorLifecycle) {
        self.current = current;
        self.connections.retain(|connection| {
            connection
                .upgrade()
                .is_some_and(|sink| sink(self.current.clone()))
        });
        if matches!(self.current, ActorLifecycle::Exited(_)) {
            self.connections.clear();
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("actor exit was already published as {existing:?}")]
pub struct ActorExitAlreadyPublished {
    pub existing: ActorTerminal,
}

/// A pending close is recorded by the lifecycle owner before native work starts.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CompilerWorkClose {
    Pending,
    Settled(tidepool_runtime::CompilerTransactionClose),
    Abandoned,
}

impl CompilerWorkClose {
    pub(crate) fn is_confirmed(&self) -> bool {
        matches!(
            self,
            Self::Settled(
                tidepool_runtime::CompilerTransactionClose::Clean
                    | tidepool_runtime::CompilerTransactionClose::NotStarted
            )
        )
    }
}

#[derive(Clone, Debug)]
pub(crate) struct CompilerWorkReceipt {
    close: Arc<Mutex<CompilerWorkClose>>,
    attempts: Arc<Mutex<Vec<tidepool_runtime::CompilerTransactionClose>>>,
    cancellation: Option<tidepool_runtime::CompilerTransactionCancellation>,
    purpose: CompilerWorkPurpose,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum CompilerWorkPurpose {
    Active,
    Finalization,
}

impl CompilerWorkReceipt {
    pub(crate) fn pending() -> Self {
        Self {
            close: Arc::new(Mutex::new(CompilerWorkClose::Pending)),
            attempts: Arc::new(Mutex::new(Vec::new())),
            cancellation: Some(tidepool_runtime::CompilerTransactionCancellation::new()),
            purpose: CompilerWorkPurpose::Active,
        }
    }
    pub(crate) fn observation(&self) -> CompilerWorkClose {
        self.close.lock().clone()
    }

    /// A dependent waiter observes settlement without owning the shared producer's stop edge.
    pub(crate) fn observation_only(&self) -> Self {
        Self {
            close: Arc::clone(&self.close),
            attempts: Arc::clone(&self.attempts),
            cancellation: None,
            purpose: self.purpose,
        }
    }

    pub(crate) fn request_cancellation(&self) {
        let cancellation = {
            let close = self.close.lock();
            matches!(*close, CompilerWorkClose::Pending)
                .then(|| self.cancellation.clone())
                .flatten()
        };
        if let Some(cancellation) = cancellation {
            cancellation.cancel();
        }
    }
}

/// Affine native-work obligation. Its blocking owner settles even without a waiter.
#[must_use]
pub(crate) struct CompilerWorkTicket {
    receipt: CompilerWorkReceipt,
    settled: bool,
    owner: crate::resident_workbench::CompilerCloseOwner,
}

impl CompilerWorkTicket {
    pub(crate) fn new(
        receipt: CompilerWorkReceipt,
        owner: crate::resident_workbench::CompilerCloseOwner,
    ) -> Self {
        Self {
            receipt,
            settled: false,
            owner,
        }
    }
    pub(crate) fn cancellation(&self) -> tidepool_runtime::CompilerTransactionCancellation {
        self.receipt
            .cancellation
            .as_ref()
            .expect("compiler ticket owns cancellation")
            .clone()
    }

    pub(crate) fn run<T>(
        self,
        action: impl FnOnce(&mut dyn FnMut(tidepool_runtime::CompilerTransactionClose)) -> T,
    ) -> T {
        self.run_with_close(action).action
    }

    pub(crate) fn run_with_close<T>(
        self,
        action: impl FnOnce(&mut dyn FnMut(tidepool_runtime::CompilerTransactionClose)) -> T,
    ) -> tidepool_runtime::CompilerTransactionOutcome<T> {
        self.run_for_workload_with_close(
            tidepool_toolchain::artifacts::CompileWorkload::Foreground,
            action,
        )
    }

    /// Own the complete compiler scope with its declared admission workload.
    /// Borrowed commands inside the action do not settle this ticket early.
    pub(crate) fn run_for_workload<T>(
        self,
        workload: tidepool_toolchain::artifacts::CompileWorkload,
        action: impl FnOnce(&mut dyn FnMut(tidepool_runtime::CompilerTransactionClose)) -> T,
    ) -> T {
        self.run_for_workload_with_close(workload, action).action
    }

    fn run_for_workload_with_close<T>(
        self,
        workload: tidepool_toolchain::artifacts::CompileWorkload,
        action: impl FnOnce(&mut dyn FnMut(tidepool_runtime::CompilerTransactionClose)) -> T,
    ) -> tidepool_runtime::CompilerTransactionOutcome<T> {
        let cancellation = self.cancellation();
        let attempts = Arc::clone(&self.receipt.attempts);
        let receipt = self.receipt.clone();
        let mut outcome =
            tidepool_toolchain::artifacts::with_compiler_transaction_cancellable_for_workload(
                workload,
                cancellation,
                move |close| {
                    self.consume(tidepool_runtime::CompilerTransactionOutcome { action: (), close })
                },
                || action(&mut |close| attempts.lock().push(close)),
            );
        if let CompilerWorkClose::Settled(close) = receipt.observation() {
            outcome.close = close;
        }
        outcome
    }

    pub(crate) fn consume<T>(
        mut self,
        outcome: tidepool_runtime::CompilerTransactionOutcome<T>,
    ) -> T {
        let mut close = outcome.close;
        for observed in self.receipt.attempts.lock().drain(..) {
            match observed {
                tidepool_runtime::CompilerTransactionClose::Unconfirmed(mut evidence) => {
                    if let tidepool_runtime::CompilerTransactionClose::Unconfirmed(previous) = close
                    {
                        evidence.earlier.push(previous);
                    }
                    close = tidepool_runtime::CompilerTransactionClose::Unconfirmed(evidence);
                }
                tidepool_runtime::CompilerTransactionClose::Clean
                    if matches!(
                        close,
                        tidepool_runtime::CompilerTransactionClose::NotStarted
                    ) =>
                {
                    close = tidepool_runtime::CompilerTransactionClose::Clean;
                }
                _ => {}
            }
        }
        *self.receipt.close.lock() = CompilerWorkClose::Settled(close);
        self.settled = true;
        self.owner.close_settled();
        outcome.action
    }
}

impl Drop for CompilerWorkTicket {
    fn drop(&mut self) {
        if !self.settled {
            *self.receipt.close.lock() = CompilerWorkClose::Abandoned;
            self.owner.close_settled();
        }
    }
}

/// The current invocation's compiler cleanup authority. Capturing it preserves
/// the existing lifecycle owner when native work outlives its async waiter.
#[derive(Clone)]
pub struct ActorCompilerCloseOwner {
    owner: crate::resident_workbench::CompilerCloseOwner,
}

impl ActorCompilerCloseOwner {
    pub fn current() -> Result<Self, crate::ResidentActorWorkbenchError> {
        crate::resident_workbench::CompilerCloseOwner::current().map(|owner| Self { owner })
    }

    pub fn run<T>(
        &self,
        action: impl FnOnce(&mut dyn FnMut(tidepool_runtime::CompilerTransactionClose)) -> T,
    ) -> Result<tidepool_runtime::CompilerTransactionOutcome<T>, crate::ResidentActorWorkbenchError>
    {
        Ok(self.owner.register_work()?.run_with_close(action))
    }
}

/// A compiler preparation lifetime before any actor is admitted. Retain this
/// owner independently of its cancelable operation future.
#[derive(Default)]
pub struct CompilerPreparationOwner {
    retained: RetainedActorExit,
}

impl CompilerPreparationOwner {
    pub fn new() -> Self {
        Self::default()
    }

    /// Admit synchronous preparation through the same retained cleanup owner.
    /// The recipient is borrowed by every helper; the ticket settles on unwind.
    pub fn run<T>(
        &mut self,
        action: impl FnOnce(&mut dyn FnMut(tidepool_runtime::CompilerTransactionClose)) -> T,
    ) -> CompilerPreparationOutcome<Result<T, crate::ResidentActorWorkbenchError>> {
        let mut admission = PreparationAdmissionGuard {
            owner: self.retained.clone(),
            completed: false,
        };
        let cleanup = self.cleanup();
        let owner =
            crate::resident_workbench::CompilerCloseOwner::ActorLifecycle(self.retained.clone());
        let action = owner.register_work().map(|ticket| ticket.run(action));
        admission.completed = true;
        drop(admission);
        CompilerPreparationOutcome { action, cleanup }
    }

    pub fn cleanup(&self) -> CompilerPreparationCleanup {
        CompilerPreparationCleanup {
            retained: self.retained.clone(),
        }
    }

    /// The action and compiler cleanup are independent. Normal completion or
    /// dropping this future closes admission, including before its first poll.
    pub fn scope<'a, T: 'a>(
        &'a mut self,
        operation: impl std::future::Future<Output = T> + 'a,
    ) -> impl std::future::Future<Output = CompilerPreparationOutcome<T>> + 'a {
        let mut admission = PreparationAdmissionGuard {
            owner: self.retained.clone(),
            completed: false,
        };
        let cleanup = self.cleanup();
        let owner =
            crate::resident_workbench::CompilerCloseOwner::ActorLifecycle(self.retained.clone());
        async move {
            let action = owner.scope(operation).await;
            admission.completed = true;
            drop(admission);
            CompilerPreparationOutcome { action, cleanup }
        }
    }
}

struct PreparationAdmissionGuard {
    owner: RetainedActorExit,
    completed: bool,
}
impl Drop for PreparationAdmissionGuard {
    fn drop(&mut self) {
        self.owner.close_compiler_admission();
        if !self.completed {
            self.owner.cancel_compiler_work();
        }
    }
}

#[must_use]
pub struct CompilerPreparationOutcome<T> {
    pub action: T,
    pub cleanup: CompilerPreparationCleanup,
}

/// Finalization uses the same retained actor receipts while active admission
/// stays closed. Dropping the guard fences and interrupts unfinished cleanup.
pub(crate) struct CompilerFinalizationGuard {
    retained: RetainedActorExit,
}

impl CompilerFinalizationGuard {
    pub(crate) fn scope<F: std::future::Future>(
        &self,
        operation: F,
    ) -> impl std::future::Future<Output = F::Output> {
        let owner =
            crate::resident_workbench::CompilerCloseOwner::ActorFinalization(self.retained.clone());
        async move { owner.scope(operation).await }
    }
}

impl Drop for CompilerFinalizationGuard {
    fn drop(&mut self) {
        self.retained.close_compiler_finalization();
    }
}

/// Repeatable observation retaining the same receipts and native child custody.
#[derive(Clone)]
pub struct CompilerPreparationCleanup {
    retained: RetainedActorExit,
}

impl std::fmt::Debug for CompilerPreparationCleanup {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_tuple("CompilerPreparationCleanup")
            .field(&self.observation())
            .finish()
    }
}

#[must_use]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CompilerPreparationCleanupObservation {
    pub admission_closed: bool,
    pub work: Vec<CompilerWorkClose>,
}

impl CompilerPreparationCleanupObservation {
    pub fn is_confirmed(&self) -> bool {
        self.admission_closed && self.work.iter().all(CompilerWorkClose::is_confirmed)
    }

    fn is_settled(&self) -> bool {
        self.admission_closed
            && !self
                .work
                .iter()
                .any(|work| matches!(work, CompilerWorkClose::Pending))
    }
}

impl CompilerPreparationCleanup {
    pub fn observation(&self) -> CompilerPreparationCleanupObservation {
        let state = self.retained.state.cleanup.lock();
        CompilerPreparationCleanupObservation {
            admission_closed: state.compiler_admission == CompilerAdmission::Closed,
            work: state
                .compilers
                .iter()
                .map(CompilerWorkReceipt::observation)
                .collect(),
        }
    }

    /// The deadline bounds observation, not native retirement. Timeout retains
    /// Pending and exact custody; it cannot manufacture confirmed cleanup.
    pub async fn wait_for_settlement(
        &self,
        timeout: std::time::Duration,
    ) -> CompilerPreparationCleanupObservation {
        let mut changed = self.retained.state.changed.subscribe();
        let waiting = async {
            loop {
                let observation = self.observation();
                if observation.is_settled() {
                    return observation;
                }
                if changed.changed().await.is_err() {
                    return self.observation();
                }
            }
        };
        match tokio::time::timeout(timeout, waiting).await {
            Ok(observation) => observation,
            Err(_) => self.observation(),
        }
    }
}

#[derive(Clone, Copy, Default, PartialEq, Eq)]
enum CompilerAdmission {
    #[default]
    Active,
    Finalizing,
    Closed,
}

#[derive(Default)]
struct RetainedCleanupState {
    compiler_admission: CompilerAdmission,
    outcome: Option<crate::ResidentCleanupOutcome>,
    compilers: Vec<CompilerWorkReceipt>,
}

struct ExitState {
    successor: Mutex<Option<crate::ActorRef>>,
    lifecycle: Mutex<LifecycleState>,
    cleanup: Mutex<RetainedCleanupState>,
    requested_shutdown: Mutex<Option<ActorTerminal>>,
    acknowledged_retirement: Mutex<Option<crate::ActorRef>>,
    changed: watch::Sender<u64>,
}

/// Cloneable observation of one actor's single-assignment terminal record.
///
/// The record retains lifecycle and owned cleanup evidence, including any
/// unconfirmed compiler process custody. Cloning it creates no second root for
/// the actor's Haskell exit value. Observation is repeatable, and a waiter
/// cannot miss publication between checking and parking.
#[derive(Clone)]
pub struct RetainedActorExit {
    state: Arc<ExitState>,
}

impl std::fmt::Debug for RetainedActorExit {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_tuple("RetainedActorExit")
            .field(&self.get())
            .finish()
    }
}

impl Default for RetainedActorExit {
    fn default() -> Self {
        Self::new()
    }
}

impl RetainedActorExit {
    /// Capture the current state and attach under the publication lock. Every
    /// later transition is delivered in order until exit, detach, or rejection.
    pub fn connect_lifecycle(
        &self,
        sink: impl Fn(ActorLifecycle) -> bool + Send + Sync + 'static,
    ) -> ActorLifecycleConnection {
        let sink: Arc<LifecycleSink> = Arc::new(sink);
        let mut lifecycle = self.state.lifecycle.lock();
        let admitted = sink(lifecycle.current.clone());
        if admitted && !matches!(lifecycle.current, ActorLifecycle::Exited(_)) {
            lifecycle
                .connections
                .retain(|connection| connection.strong_count() != 0);
            lifecycle.connections.push(Arc::downgrade(&sink));
        }
        ActorLifecycleConnection { _sink: sink }
    }

    pub(crate) fn publish_paused(&self, detail: String) {
        let mut lifecycle = self.state.lifecycle.lock();
        if !matches!(lifecycle.current, ActorLifecycle::Exited(_)) {
            lifecycle.publish(ActorLifecycle::Paused(detail));
        }
    }

    /// Exact replacement identity retained even if the controlling RPC loses
    /// its waiter. This is observation metadata, never an address alias.
    pub fn successor(&self) -> Option<crate::ActorRef> {
        *self.state.successor.lock()
    }

    pub(crate) fn retain_successor(&self, successor: crate::ActorRef) {
        self.state.successor.lock().get_or_insert(successor);
    }
    /// Terminal-only legacy/forced exits deliberately have no cleanup proof.
    pub fn cleanup(&self) -> Option<crate::ResidentCleanupOutcome> {
        let state = self.state.cleanup.lock();
        let mut outcome = state.outcome.clone()?;
        let unconfirmed: Vec<_> = state
            .compilers
            .iter()
            .map(CompilerWorkReceipt::observation)
            .filter(|close| !close.is_confirmed())
            .collect();
        if !unconfirmed.is_empty() {
            outcome.hook = crate::CleanupComponentOutcome::Unconfirmed(format!(
                "actor hook {:?}; compiler close {unconfirmed:?}",
                outcome.hook
            ));
        }
        Some(outcome)
    }

    pub(crate) fn retain_cleanup(&self, outcome: crate::ResidentCleanupOutcome) {
        let receipts = {
            let mut state = self.state.cleanup.lock();
            state.outcome.get_or_insert(outcome);
            state.compiler_admission = CompilerAdmission::Closed;
            state.compilers.clone()
        };
        for receipt in receipts {
            receipt.request_cancellation();
        }
        self.notify_compiler_close();
    }

    pub(crate) fn register_compiler_work(&self, receipt: CompilerWorkReceipt) -> bool {
        let shutdown = self.state.requested_shutdown.lock();
        let mut state = self.state.cleanup.lock();
        if state.compiler_admission != CompilerAdmission::Active
            || shutdown.is_some()
            || state.outcome.is_some()
        {
            return false;
        }
        state.compilers.push(receipt);
        true
    }

    fn close_compiler_admission(&self) {
        let mut state = self.state.cleanup.lock();
        let changed = state.compiler_admission != CompilerAdmission::Closed;
        state.compiler_admission = CompilerAdmission::Closed;
        drop(state);
        if changed {
            self.notify_compiler_close();
        }
    }

    fn cancel_compiler_work(&self) {
        let receipts = self.state.cleanup.lock().compilers.clone();
        for receipt in receipts {
            if receipt.purpose == CompilerWorkPurpose::Active {
                receipt.request_cancellation();
            }
        }
    }

    pub(crate) fn begin_compiler_finalization(
        &self,
    ) -> Result<CompilerFinalizationGuard, crate::ResidentActorWorkbenchError> {
        let mut state = self.state.cleanup.lock();
        if state.compiler_admission != CompilerAdmission::Active || state.outcome.is_some() {
            return Err(crate::ResidentActorWorkbenchError::CompilerCleanupAdmissionClosed);
        }
        state.compiler_admission = CompilerAdmission::Finalizing;
        drop(state);
        self.cancel_compiler_work();
        self.notify_compiler_close();
        Ok(CompilerFinalizationGuard {
            retained: self.clone(),
        })
    }

    pub(crate) fn register_compiler_finalization_work(
        &self,
        mut receipt: CompilerWorkReceipt,
    ) -> bool {
        let mut state = self.state.cleanup.lock();
        if state.compiler_admission != CompilerAdmission::Finalizing || state.outcome.is_some() {
            return false;
        }
        receipt.purpose = CompilerWorkPurpose::Finalization;
        state.compilers.push(receipt);
        true
    }

    fn close_compiler_finalization(&self) {
        let receipts = {
            let mut state = self.state.cleanup.lock();
            state.compiler_admission = CompilerAdmission::Closed;
            state.compilers.clone()
        };
        for receipt in receipts {
            receipt.request_cancellation();
        }
        self.notify_compiler_close();
    }

    pub(crate) fn notify_compiler_close(&self) {
        self.state.changed.send_modify(|revision| *revision += 1);
    }

    pub(crate) fn compiler_close_observations(&self) -> Vec<CompilerWorkClose> {
        self.state
            .cleanup
            .lock()
            .compilers
            .iter()
            .map(CompilerWorkReceipt::observation)
            .collect()
    }

    /// Record intent without publishing an exit. Bootstrap checks this at safe
    /// boundaries; only ordinary lifecycle cleanup may publish the terminal.
    pub(crate) fn request_shutdown(&self, terminal: ActorTerminal) -> ActorTerminal {
        let terminal = terminal.bound_diagnostic();
        let terminal = self
            .state
            .requested_shutdown
            .lock()
            .get_or_insert(terminal)
            .clone();
        self.cancel_compiler_work();
        self.state.changed.send_modify(|revision| *revision += 1);
        terminal
    }

    /// Fence every selected incarnation before admission closure can wake work
    /// that settles a peer. Aliases share one lock; overlapping batches acquire
    /// their locks in the same order. Existing exits and intents remain intact.
    ///
    /// The caller must release directory and request-registry locks first. The
    /// closure may only close admissions: it must not acquire retirement locks
    /// or publish cleanup. Intent is not terminal or cleanup evidence.
    pub(crate) fn request_shutdown_batch(
        requests: &[(RetainedActorExit, ActorTerminal)],
        close_admissions: impl FnOnce(),
    ) {
        let mut owners: Vec<_> = requests.iter().collect();
        owners.sort_by_key(|(owner, _)| Arc::as_ptr(&owner.state));
        owners.dedup_by_key(|(owner, _)| Arc::as_ptr(&owner.state));
        let mut guards: Vec<_> = owners
            .iter()
            .map(|(owner, _)| owner.state.requested_shutdown.lock())
            .collect();
        for ((owner, terminal), requested) in owners.iter().zip(&mut guards) {
            if requested.is_none() && owner.get().is_none() {
                **requested = Some(terminal.clone().bound_diagnostic());
            }
        }
        close_admissions();
        drop(guards);
        for (owner, _) in owners {
            owner.cancel_compiler_work();
            owner.state.changed.send_modify(|revision| *revision += 1);
        }
    }

    pub(crate) fn requested_shutdown(&self) -> Option<ActorTerminal> {
        self.state.requested_shutdown.lock().clone()
    }

    /// Order a synchronous wake claim against this incarnation's retirement.
    /// The callback may only claim the caller's control boundary; native work
    /// starts after this lock is released.
    pub(crate) fn claim_before_shutdown(
        &self,
        claim: impl FnOnce() -> bool,
    ) -> Result<bool, ActorTerminal> {
        let shutdown = self.state.requested_shutdown.lock();
        match shutdown.as_ref() {
            Some(terminal) => Err(terminal.clone()),
            None => Ok(claim()),
        }
    }

    pub(crate) async fn wait_requested_shutdown(&self) -> ActorTerminal {
        let mut changed = self.state.changed.subscribe();
        loop {
            if let Some(terminal) = self.requested_shutdown() {
                return terminal;
            }
            #[allow(
                clippy::expect_used,
                reason = "the corresponding watch::Sender lives in self.state \
                          alongside this receiver, so it cannot be dropped \
                          while this &self borrow is held across the await"
            )]
            changed
                .changed()
                .await
                .expect("retained actor exit owns its lifecycle sender");
        }
    }

    pub(crate) fn acknowledge_retirement(&self, supervisor: crate::ActorRef) {
        *self.state.acknowledged_retirement.lock() = Some(supervisor);
    }

    /// Whether this supervisor already received the explicit retirement result.
    pub fn retirement_acknowledged_by(&self, supervisor: crate::ActorRef) -> bool {
        *self.state.acknowledged_retirement.lock() == Some(supervisor)
    }

    #[must_use]
    pub fn new() -> Self {
        let (changed, _) = watch::channel(0);
        Self {
            state: Arc::new(ExitState {
                successor: Mutex::new(None),
                lifecycle: Mutex::new(LifecycleState {
                    current: ActorLifecycle::Live,
                    connections: Vec::new(),
                    result: None,
                }),
                cleanup: Mutex::new(RetainedCleanupState::default()),
                requested_shutdown: Mutex::new(None),
                acknowledged_retirement: Mutex::new(None),
                changed,
            }),
        }
    }

    /// Publish the sole terminal result.
    ///
    /// The actor lifecycle owner is the sole expected writer:
    /// `local_actor::publish_exit` is the only caller outside this module's
    /// own tests. Returning an error rather than replacing the value makes
    /// competing cleanup paths an explicit invariant violation while
    /// preserving the first result.
    pub(crate) fn publish(&self, terminal: ActorTerminal) -> Result<(), ActorExitAlreadyPublished> {
        let terminal = terminal.bound_diagnostic();
        {
            let mut retained = self.state.lifecycle.lock();
            if let ActorLifecycle::Exited(existing) = &retained.current {
                return Err(ActorExitAlreadyPublished {
                    existing: existing.clone(),
                });
            }
            if terminal.kind != ActorExitKind::Completed {
                retained.result.take();
            }
            retained.publish(ActorLifecycle::Exited(terminal));
        }
        self.state
            .changed
            .send_modify(|generation| *generation += 1);
        Ok(())
    }

    pub(crate) fn retain_result(
        &self,
        result: Arc<crate::owned_result::OwnedResultSnapshot>,
    ) -> Result<(), &'static str> {
        let mut retained = self.state.lifecycle.lock();
        if matches!(retained.current, ActorLifecycle::Exited(_)) || retained.result.is_some() {
            return Err("actor exit result was already settled");
        }
        retained.result = Some(result);
        Ok(())
    }

    pub(crate) fn result(&self) -> Option<Arc<crate::owned_result::OwnedResultSnapshot>> {
        let retained = self.state.lifecycle.lock();
        match &retained.current {
            ActorLifecycle::Exited(terminal) if terminal.kind == ActorExitKind::Completed => {
                retained.result.clone()
            }
            _ => None,
        }
    }

    /// Return the immutable result without consuming it.
    #[must_use]
    pub fn get(&self) -> Option<ActorTerminal> {
        match &self.state.lifecycle.lock().current {
            ActorLifecycle::Exited(terminal) => Some(terminal.clone()),
            _ => None,
        }
    }

    /// Wait for publication, then clone the immutable result.
    pub async fn wait(&self) -> ActorTerminal {
        let mut changed = self.state.changed.subscribe();
        loop {
            if let Some(terminal) = self.get() {
                return terminal;
            }
            // `RetainedActorExit` itself owns the sender, so closure would be
            // an internal invariant violation rather than a lifecycle case.
            if changed.changed().await.is_err() {
                unreachable!("retained actor exit signal closed while its record was live");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    fn uncertain_close() -> tidepool_runtime::CompilerTransactionClose {
        tidepool_runtime::CompilerTransactionClose::Unconfirmed(
            tidepool_extract_cmd::CompilerTransactionCloseEvidence::new(
                tidepool_extract_cmd::CompilerTransactionCloseReason::Cancelled,
                tidepool_extract_cmd::CompilerTransactionRetirement::DaemonUnobserved {
                    disconnect: None,
                },
            ),
        )
    }

    #[test]
    fn synchronous_preparation_retains_callback_uncertainty_and_primary_action() {
        let mut owner = super::CompilerPreparationOwner::new();
        let expected = uncertain_close();
        let outcome = owner.run(|settlement| {
            settlement(expected.clone());
            Err::<(), _>("primary source refusal")
        });
        assert_eq!(outcome.action.unwrap(), Err("primary source refusal"));
        assert!(outcome.cleanup.observation().admission_closed);
        assert!(!outcome.cleanup.observation().is_confirmed());
        assert_eq!(
            outcome.cleanup.observation().work,
            vec![super::CompilerWorkClose::Settled(expected)]
        );
    }

    #[test]
    fn callback_uncertainty_survives_preparation_unwind() {
        let mut owner = super::CompilerPreparationOwner::new();
        let cleanup = owner.cleanup();
        let expected = uncertain_close();
        let unwind = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            owner.run(|settlement| {
                settlement(expected.clone());
                panic!("primary action unwind");
            });
        }));
        assert!(unwind.is_err());
        assert!(cleanup.observation().admission_closed);
        assert!(!cleanup.observation().is_confirmed());
        assert_eq!(
            cleanup.observation().work,
            vec![super::CompilerWorkClose::Settled(expected)]
        );
    }

    #[test]
    fn public_compiler_owner_refuses_absent_lifecycle_authority() {
        assert!(matches!(
            super::ActorCompilerCloseOwner::current(),
            Err(crate::ResidentActorWorkbenchError::CompilerCleanupOwnerUnavailable)
        ));
    }

    #[tokio::test]
    async fn public_compiler_owner_retains_current_receipt_and_independent_action() {
        let mut owner = super::CompilerPreparationOwner::new();
        let expected = uncertain_close();
        let outcome = owner
            .scope(async {
                let authority = super::ActorCompilerCloseOwner::current().unwrap();
                let result = authority
                    .run(|settlement| {
                        settlement(expected.clone());
                        42
                    })
                    .unwrap();
                assert_eq!(result.action, 42);
                assert_eq!(result.close, expected);
            })
            .await;
        assert_eq!(
            outcome.cleanup.observation().work,
            vec![super::CompilerWorkClose::Settled(expected)]
        );
        assert!(!outcome.cleanup.observation().is_confirmed());
    }

    #[test]
    fn fresh_source_failure_closes_preparation_and_fresh_owner_can_compile() {
        let mut failed = super::CompilerPreparationOwner::new();
        let refusal = failed.run(|settlement| {
            tidepool_runtime::compile_haskell(
                "module ReceiptFailure where\nvalue =\n",
                "value",
                &[],
                settlement,
            )
        });
        assert!(matches!(
            refusal.action.unwrap(),
            Err(tidepool_runtime::CompileError::Diagnostics(_))
        ));
        assert!(refusal.cleanup.observation().is_confirmed());
        assert_eq!(
            refusal.cleanup.observation().work,
            vec![super::CompilerWorkClose::Settled(
                tidepool_runtime::CompilerTransactionClose::Clean
            )]
        );
        let mut fresh = super::CompilerPreparationOwner::new();
        let accepted = fresh.run(|settlement| {
            tidepool_runtime::compile_haskell(
                "module ReceiptSuccess where\nvalue = (42 :: Int)\n",
                "value",
                &[],
                settlement,
            )
        });
        assert!(accepted.action.unwrap().is_ok());
        assert!(accepted.cleanup.observation().is_confirmed());
        assert_eq!(
            accepted.cleanup.observation().work,
            vec![super::CompilerWorkClose::Settled(
                tidepool_runtime::CompilerTransactionClose::Clean
            )]
        );
    }

    #[tokio::test]
    async fn source_preparation_scope_preserves_action_and_confirms_no_native_work() {
        let mut owner = super::CompilerPreparationOwner::new();
        assert!(!owner.cleanup().observation().is_confirmed());
        let outcome = owner.scope(async { Ok::<_, &str>(42) }).await;
        assert_eq!(outcome.action, Ok(42));
        assert!(outcome.cleanup.observation().is_confirmed());
        assert!(outcome.cleanup.observation().work.is_empty());
    }

    async fn abandoned_source_action<T>(action: T) -> super::CompilerPreparationOutcome<T> {
        let mut owner = super::CompilerPreparationOwner::new();
        owner
            .scope(async move {
                let ticket = crate::resident_workbench::CompilerCloseOwner::current()
                    .unwrap()
                    .register_work()
                    .unwrap();
                drop(ticket);
                action
            })
            .await
    }

    #[tokio::test]
    async fn source_preparation_success_survives_abandoned_compiler_obligation() {
        let outcome = abandoned_source_action(Ok::<_, &str>(42)).await;
        assert_eq!(outcome.action, Ok(42));
        assert!(!outcome.cleanup.observation().is_confirmed());
        assert_eq!(
            outcome.cleanup.observation().work,
            vec![super::CompilerWorkClose::Abandoned]
        );
    }

    #[tokio::test]
    async fn source_preparation_primary_failure_survives_abandoned_compiler_obligation() {
        let outcome = abandoned_source_action(Err::<(), _>("primary source refusal")).await;
        assert_eq!(outcome.action, Err("primary source refusal"));
        assert!(!outcome.cleanup.observation().is_confirmed());
        assert_eq!(
            outcome.cleanup.observation().work,
            vec![super::CompilerWorkClose::Abandoned]
        );
    }

    #[test]
    fn dropping_unpolled_source_scope_closes_admission_without_fabricated_actor() {
        let mut owner = super::CompilerPreparationOwner::new();
        let captured =
            crate::resident_workbench::CompilerCloseOwner::ActorLifecycle(owner.retained.clone());
        let observation = owner.cleanup();
        let operation = owner.scope(async {
            panic!("unpolled action cannot execute");
        });
        drop(operation);
        assert!(observation.observation().is_confirmed());
        assert!(captured.register_work().is_err());
        assert!(owner.retained.get().is_none());
        assert!(owner.retained.cleanup().is_none());
    }

    #[tokio::test]
    async fn cancelled_source_scope_retains_abandonment_and_closes_late_admission() {
        let mut owner = super::CompilerPreparationOwner::new();
        let captured =
            crate::resident_workbench::CompilerCloseOwner::ActorLifecycle(owner.retained.clone());
        let observation = owner.cleanup();
        let mut operation = Box::pin(owner.scope(async {
            let _ticket = crate::resident_workbench::CompilerCloseOwner::current()
                .unwrap()
                .register_work()
                .unwrap();
            std::future::pending::<()>().await;
        }));
        std::future::poll_fn(|context| {
            assert!(std::future::Future::poll(operation.as_mut(), context).is_pending());
            std::task::Poll::Ready(())
        })
        .await;
        assert_eq!(
            observation.observation().work,
            vec![super::CompilerWorkClose::Pending]
        );
        drop(operation);
        assert!(observation.observation().admission_closed);
        assert_eq!(
            observation.observation().work,
            vec![super::CompilerWorkClose::Abandoned]
        );
        assert!(!observation.observation().is_confirmed());
        assert!(captured.register_work().is_err());
    }

    #[tokio::test]
    async fn source_cleanup_deadline_retains_pending_until_actual_late_scope_finish() {
        let mut owner = super::CompilerPreparationOwner::new();
        let (release, proceed) = std::sync::mpsc::channel();
        let outcome = owner
            .scope(async {
                let ticket = crate::resident_workbench::CompilerCloseOwner::current()
                    .unwrap()
                    .register_work()
                    .unwrap();
                let _native = tidepool_runtime::spawn_blocking_in_span(move || {
                    ticket.run_for_workload(
                        tidepool_toolchain::artifacts::CompileWorkload::Preparation,
                        |_settlement| {
                            proceed
                                .recv_timeout(std::time::Duration::from_secs(5))
                                .unwrap()
                        },
                    );
                });
                42
            })
            .await;
        assert_eq!(outcome.action, 42);
        let pending = outcome
            .cleanup
            .wait_for_settlement(std::time::Duration::ZERO)
            .await;
        assert!(pending.admission_closed);
        assert_eq!(pending.work, vec![super::CompilerWorkClose::Pending]);
        assert!(!pending.is_confirmed());
        release.send(()).unwrap();
        let settled = outcome
            .cleanup
            .wait_for_settlement(std::time::Duration::from_secs(5))
            .await;
        assert!(settled.is_confirmed());
        assert_eq!(
            settled.work,
            vec![super::CompilerWorkClose::Settled(
                tidepool_runtime::CompilerTransactionClose::NotStarted
            )]
        );
    }

    use std::time::Duration;

    use super::*;

    fn clean_actor_cleanup(actor: crate::ActorRef) -> crate::ResidentCleanupOutcome {
        crate::ResidentCleanupOutcome {
            actor,
            hook: crate::CleanupComponentOutcome::Confirmed,
            realm: crate::CleanupComponentOutcome::Confirmed,
            children: crate::CleanupComponentOutcome::Confirmed,
        }
    }

    fn admitted_compiler(owner: &RetainedActorExit) -> CompilerWorkTicket {
        let receipt = CompilerWorkReceipt::pending();
        assert!(owner.register_compiler_work(receipt.clone()));
        CompilerWorkTicket::new(
            receipt,
            crate::resident_workbench::CompilerCloseOwner::ActorLifecycle(owner.clone()),
        )
    }

    #[test]
    fn pending_compiler_prevents_confirmed_actor_stop_until_actual_late_close() {
        let owner = RetainedActorExit::new();
        let ticket = admitted_compiler(&owner);
        let actor = crate::ActorRef::first(crate::ActorId(1));
        owner.retain_cleanup(clean_actor_cleanup(actor));
        owner
            .publish(completed("actor stopped while compiler was pending"))
            .unwrap();
        assert!(!owner.cleanup().unwrap().is_confirmed());
        assert_eq!(
            owner.compiler_close_observations(),
            vec![CompilerWorkClose::Pending]
        );
        // The blocking owner survives the vanished async waiter and settles its
        // affine obligation independently of the actor's immutable terminal.
        assert_eq!(
            ticket.consume(tidepool_runtime::CompilerTransactionOutcome {
                action: Ok::<_, &'static str>(42),
                close: tidepool_runtime::CompilerTransactionClose::Clean,
            }),
            Ok(42)
        );
        assert!(owner.cleanup().unwrap().is_confirmed());
        assert_eq!(
            owner.get(),
            Some(completed("actor stopped while compiler was pending"))
        );
        assert!(!owner.register_compiler_work(CompilerWorkReceipt::pending()));
    }

    #[test]
    fn completed_compiler_close_is_retained_before_actor_stop() {
        let owner = RetainedActorExit::new();
        let ticket = admitted_compiler(&owner);
        assert_eq!(
            ticket.consume(tidepool_runtime::CompilerTransactionOutcome {
                action: Err::<(), _>("primary failure"),
                close: tidepool_runtime::CompilerTransactionClose::Clean,
            }),
            Err("primary failure")
        );
        owner.retain_cleanup(clean_actor_cleanup(crate::ActorRef::first(crate::ActorId(
            1,
        ))));
        owner.publish(completed("stopped after close")).unwrap();
        assert!(owner.cleanup().unwrap().is_confirmed());
        assert_eq!(
            owner.compiler_close_observations(),
            vec![CompilerWorkClose::Settled(
                tidepool_runtime::CompilerTransactionClose::Clean
            )]
        );
    }

    #[test]
    fn abandoned_compiler_obligation_cannot_certify_actor_cleanup() {
        let owner = RetainedActorExit::new();
        let ticket = admitted_compiler(&owner);
        owner.retain_cleanup(clean_actor_cleanup(crate::ActorRef::first(crate::ActorId(
            1,
        ))));
        drop(ticket);
        assert!(!owner.cleanup().unwrap().is_confirmed());
        assert_eq!(
            owner.compiler_close_observations(),
            vec![CompilerWorkClose::Abandoned]
        );
    }

    fn completed(summary: &str) -> ActorTerminal {
        ActorTerminal {
            kind: ActorExitKind::Completed,
            summary: summary.into(),
            diagnostic: None,
        }
    }

    #[test]
    fn terminal_retains_bounded_diagnostic_and_serializes_original_category() {
        use tidepool_toolchain::failclass::{
            CompileFailureCause, FailureClass, FailureEnvelope, Phase,
        };
        let original = FailureEnvelope {
            class: FailureClass::UserHaskell,
            phase: Phase::Compile,
            cause: Some(CompileFailureCause::SourceDiagnostics),
            message: format!(
                "FailureOrigin.hs:4:9\n{}\nmissingChildFailureOrigin",
                "αβγ\n".repeat(10_000)
            ),
        };
        let raw = ActorTerminal {
            kind: ActorExitKind::Failed,
            summary: "Haskell compilation failed (1 diagnostic)".into(),
            diagnostic: Some(original.clone()),
        };
        let intent = RetainedActorExit::new();
        let terminal = intent.request_shutdown(raw.clone());
        assert_eq!(intent.requested_shutdown(), Some(terminal.clone()));
        assert_eq!(
            intent.claim_before_shutdown(|| panic!("intent already owns retirement")),
            Err(terminal.clone())
        );
        assert_eq!(terminal, ActorTerminal::failed(raw.summary, raw.diagnostic));
        let diagnostic = terminal.diagnostic.as_ref().expect("original diagnostic");
        assert_eq!(diagnostic.class, original.class);
        assert_eq!(diagnostic.phase, original.phase);
        assert_eq!(diagnostic.cause, original.cause);
        assert!(diagnostic.message.len() <= RETAINED_FAILURE_MESSAGE_BYTES);
        assert!(diagnostic.message.starts_with("FailureOrigin.hs:4:9"));
        assert!(diagnostic.message.ends_with("missingChildFailureOrigin"));
        let encoded = serde_json::to_value(&terminal).expect("operator terminal evidence");
        assert_eq!(encoded["diagnostic"]["class"], "user-haskell");
        assert_eq!(encoded["diagnostic"]["phase"], "compile");
        assert_eq!(
            encoded["diagnostic"]["cause"],
            serde_json::to_value(&diagnostic.cause).unwrap()
        );
        assert_eq!(encoded["diagnostic"]["message"], diagnostic.message);
        let retained = RetainedActorExit::new();
        retained.publish(terminal.clone()).unwrap();
        assert!(retained.publish(completed("later cleanup")).is_err());
        assert_eq!(retained.get(), Some(terminal));
        assert!(serde_json::to_value(completed("ordinary exit"))
            .unwrap()
            .get("diagnostic")
            .is_none());
    }

    #[test]
    fn terminal_deserialization_bounds_diagnostics_and_refuses_malformed_categories() {
        let terminal = serde_json::json!({
            "kind": "failed", "summary": "concise original summary",
            "diagnostic": {
                "class": "user-haskell", "phase": "compile",
                "message": "λ\n".repeat(RETAINED_FAILURE_MESSAGE_BYTES),
            },
        });
        // Cause encoding is owned by the existing classifier, not duplicated here.
        let mut terminal = terminal;
        terminal["diagnostic"]["cause"] = serde_json::to_value(
            tidepool_toolchain::failclass::CompileFailureCause::SourceDiagnostics,
        )
        .unwrap();
        let parsed: ActorTerminal = serde_json::from_value(terminal.clone()).unwrap();
        let diagnostic = parsed.diagnostic.as_ref().unwrap();
        assert!(diagnostic.message.len() <= RETAINED_FAILURE_MESSAGE_BYTES);
        assert_eq!(
            diagnostic.class,
            tidepool_toolchain::failclass::FailureClass::UserHaskell
        );
        assert_eq!(
            diagnostic.phase,
            tidepool_toolchain::failclass::Phase::Compile
        );
        assert_eq!(parsed.summary, "concise original summary");
        for (key, invalid) in [
            ("class", serde_json::json!("invented-class")),
            ("phase", serde_json::json!("invented-phase")),
            ("cause", serde_json::json!({"invented": true})),
            ("message", serde_json::json!(17)),
            ("extra", serde_json::json!("unchecked metadata")),
        ] {
            let mut malformed = terminal.clone();
            malformed["diagnostic"][key] = invalid;
            assert!(serde_json::from_value::<ActorTerminal>(malformed).is_err());
        }
        let mut unknown_terminal_field = terminal;
        unknown_terminal_field["extra"] = serde_json::json!("unchecked field");
        assert!(serde_json::from_value::<ActorTerminal>(unknown_terminal_field).is_err());
    }

    #[test]
    fn lifecycle_retains_current_and_delivers_each_transition_until_detached() {
        let retained = RetainedActorExit::new();
        let (send, receive) = std::sync::mpsc::channel();
        let connection = retained.connect_lifecycle(move |event| send.send(event).is_ok());
        retained.publish_paused("failed input retained".into());
        assert_eq!(
            receive.try_iter().collect::<Vec<_>>(),
            vec![
                ActorLifecycle::Live,
                ActorLifecycle::Paused("failed input retained".into())
            ]
        );
        assert_eq!(retained.get(), None);
        let (late_send, late_receive) = std::sync::mpsc::channel();
        let _late = retained.connect_lifecycle(move |event| late_send.send(event).is_ok());
        drop(connection);
        retained.publish(completed("done")).unwrap();
        assert!(receive.try_iter().next().is_none());
        assert_eq!(
            late_receive.try_iter().collect::<Vec<_>>(),
            vec![
                ActorLifecycle::Paused("failed input retained".into()),
                ActorLifecycle::Exited(completed("done"))
            ]
        );
        retained.publish_paused("too late".into());
        assert_eq!(retained.get(), Some(completed("done")));
        assert!(late_receive.try_iter().next().is_none());
        let (final_send, final_receive) = std::sync::mpsc::channel();
        let _terminal = retained.connect_lifecycle(move |event| final_send.send(event).is_ok());
        assert_eq!(
            final_receive.try_iter().collect::<Vec<_>>(),
            vec![ActorLifecycle::Exited(completed("done"))]
        );
        assert!(retained.state.lifecycle.lock().connections.is_empty());
    }

    #[test]
    fn lifecycle_attachment_racing_publication_never_misses_or_duplicates_exit() {
        for _ in 0..32 {
            let retained = RetainedActorExit::new();
            let barrier = Arc::new(std::sync::Barrier::new(2));
            let publisher = retained.clone();
            let publish_barrier = barrier.clone();
            let task = std::thread::spawn(move || {
                publish_barrier.wait();
                publisher.publish_paused("paused".into());
                publisher.publish(completed("done")).unwrap();
            });
            let (send, receive) = std::sync::mpsc::channel();
            barrier.wait();
            let _connection = retained.connect_lifecycle(move |event| send.send(event).is_ok());
            task.join().unwrap();
            let events: Vec<_> = receive.try_iter().collect();
            let expected = [
                ActorLifecycle::Live,
                ActorLifecycle::Paused("paused".into()),
                ActorLifecycle::Exited(completed("done")),
            ];
            assert!(!events.is_empty());
            assert!(expected.ends_with(&events), "{events:?}");
        }
    }

    #[test]
    fn shutdown_intent_does_not_publish_terminal_or_replace_first_request() {
        let retained = RetainedActorExit::new();
        let requested = ActorTerminal {
            kind: ActorExitKind::Cancelled,
            summary: "cancel bootstrap".into(),
            diagnostic: None,
        };
        assert_eq!(retained.request_shutdown(requested.clone()), requested);
        assert_eq!(
            retained.request_shutdown(completed("later request")),
            requested
        );
        assert_eq!(retained.requested_shutdown(), Some(requested.clone()));
        assert_eq!(retained.get(), None);
        retained.publish(requested.clone()).unwrap();
        assert_eq!(retained.get(), Some(requested));
    }

    #[test]
    fn shutdown_intent_prevents_a_later_wake_claim() {
        let owner = RetainedActorExit::new();
        let terminal = completed("retirement owns this boundary");
        owner.request_shutdown(terminal.clone());
        assert_eq!(
            owner.claim_before_shutdown(|| panic!("retirement must refuse native wake")),
            Err(terminal)
        );
    }

    #[test]
    fn shutdown_batch_holds_every_fence_before_admission_wake() {
        let target = RetainedActorExit::new();
        let waiting = RetainedActorExit::new();
        let terminal = ActorTerminal::new(ActorExitKind::Cancelled, "batch retirement");
        let target_changes = target.state.changed.subscribe();
        let waiting_changes = waiting.state.changed.subscribe();
        let control = crate::WorkbenchExecutionControl::untracked();
        control.arm_sleep();
        let claiming_owner = waiting.clone();
        let claiming_control = control.clone();
        let (ready, observation) = std::sync::mpsc::channel();
        let (entered, claiming) = std::sync::mpsc::channel();
        let claimant = std::thread::spawn(move || {
            observation.recv_timeout(Duration::from_secs(5)).unwrap();
            entered.send(()).unwrap();
            claiming_owner.claim_before_shutdown(|| claiming_control.claim_expiry())
        });

        RetainedActorExit::request_shutdown_batch(
            &[
                (target.clone(), terminal.clone()),
                (waiting.clone(), terminal.clone()),
                (
                    target.clone(),
                    completed("duplicate must not replace intent"),
                ),
            ],
            || {
                assert!(target.state.requested_shutdown.try_lock().is_none());
                assert!(waiting.state.requested_shutdown.try_lock().is_none());
                assert!(!target_changes.has_changed().unwrap());
                assert!(!waiting_changes.has_changed().unwrap());
                // Admission closure can make a peer's observation ready before
                // the explicit retirement notification reaches that peer.
                ready.send(()).unwrap();
                claiming.recv_timeout(Duration::from_secs(5)).unwrap();
            },
        );

        assert_eq!(claimant.join().unwrap(), Err(terminal.clone()));
        assert_eq!(target.requested_shutdown(), Some(terminal.clone()));
        assert_eq!(waiting.requested_shutdown(), Some(terminal));
        assert!(target_changes.has_changed().unwrap());
        assert!(waiting_changes.has_changed().unwrap());
        assert!(control.claim_expiry(), "ready wake never claimed execution");
        assert!(target.get().is_none());
        assert!(waiting.get().is_none());
        assert!(target.cleanup().is_none());
        assert!(waiting.cleanup().is_none());
    }

    #[test]
    fn overlapping_shutdown_batches_preserve_one_intent_in_opposite_input_orders() {
        let first = RetainedActorExit::new();
        let second = RetainedActorExit::new();
        let barrier = Arc::new(std::sync::Barrier::new(2));
        let (finished, completions) = std::sync::mpsc::channel();
        let mut workers = Vec::new();
        for (requests, summary) in [
            (vec![first.clone(), second.clone()], "first batch"),
            (vec![second.clone(), first.clone()], "second batch"),
        ] {
            let barrier = barrier.clone();
            let finished = finished.clone();
            workers.push(std::thread::spawn(move || {
                let requests: Vec<_> = requests
                    .into_iter()
                    .map(|owner| (owner, ActorTerminal::new(ActorExitKind::Cancelled, summary)))
                    .collect();
                barrier.wait();
                RetainedActorExit::request_shutdown_batch(&requests, || {});
                finished.send(()).unwrap();
            }));
        }
        for _ in 0..2 {
            completions.recv_timeout(Duration::from_secs(5)).unwrap();
        }
        for worker in workers {
            worker.join().unwrap();
        }
        assert_eq!(first.requested_shutdown(), second.requested_shutdown());
        assert!(matches!(
            first.requested_shutdown(),
            Some(ActorTerminal {
                kind: ActorExitKind::Cancelled,
                ..
            })
        ));
        assert!(first.get().is_none());
        assert!(second.get().is_none());
        assert!(first.cleanup().is_none());
        assert!(second.cleanup().is_none());
    }

    #[test]
    fn shutdown_batch_preserves_completed_exit_prior_intent_and_unsettled_work() {
        let exited = RetainedActorExit::new();
        let prior = RetainedActorExit::new();
        let pending = RetainedActorExit::new();
        exited
            .publish(completed("completed before cancellation"))
            .unwrap();
        let original = ActorTerminal::new(ActorExitKind::Failed, "original retirement");
        prior.request_shutdown(original.clone());
        let ticket = admitted_compiler(&pending);
        let cancellation = ActorTerminal::new(ActorExitKind::Cancelled, "later batch");
        let mut admissions_closed = false;
        RetainedActorExit::request_shutdown_batch(
            &[
                (exited.clone(), cancellation.clone()),
                (prior.clone(), cancellation.clone()),
                (pending.clone(), cancellation.clone()),
            ],
            || admissions_closed = true,
        );
        assert!(admissions_closed);
        assert_eq!(
            exited.get(),
            Some(completed("completed before cancellation"))
        );
        assert!(exited.requested_shutdown().is_none());
        assert_eq!(prior.requested_shutdown(), Some(original));
        assert_eq!(pending.requested_shutdown(), Some(cancellation));
        assert_eq!(
            pending.compiler_close_observations(),
            vec![CompilerWorkClose::Pending]
        );
        drop(ticket);
        assert_eq!(
            pending.compiler_close_observations(),
            vec![CompilerWorkClose::Abandoned]
        );
        assert!(exited.cleanup().is_none());
        assert!(prior.cleanup().is_none());
        assert!(pending.cleanup().is_none());
    }

    #[test]
    fn wake_claim_orders_before_concurrent_shutdown_intent() {
        let owner = RetainedActorExit::new();
        let control = crate::WorkbenchExecutionControl::untracked();
        control.arm_sleep();
        let claiming_owner = owner.clone();
        let claiming_control = control.clone();
        let (entered, observed) = std::sync::mpsc::channel();
        let (release, proceed) = std::sync::mpsc::channel();
        let claiming = std::thread::spawn(move || {
            claiming_owner.claim_before_shutdown(|| {
                entered.send(()).unwrap();
                proceed.recv().unwrap();
                claiming_control.claim_expiry()
            })
        });
        observed.recv().unwrap();
        let retiring_owner = owner.clone();
        let (started, starting) = std::sync::mpsc::channel();
        let (done, completed_request) = std::sync::mpsc::channel();
        let retiring = std::thread::spawn(move || {
            started.send(()).unwrap();
            let terminal = retiring_owner.request_shutdown(completed("later retirement"));
            done.send(terminal).unwrap();
        });
        starting.recv().unwrap();
        assert!(matches!(
            completed_request.try_recv(),
            Err(std::sync::mpsc::TryRecvError::Empty)
        ));
        release.send(()).unwrap();
        assert_eq!(claiming.join().unwrap(), Ok(true));
        let terminal = completed_request.recv().unwrap();
        retiring.join().unwrap();
        assert_eq!(owner.requested_shutdown(), Some(terminal));
        assert!(
            !control.request_cancellation(),
            "wake already owns native delivery"
        );
    }

    #[tokio::test]
    async fn observation_is_repeatable_before_and_after_publication() {
        let retained = RetainedActorExit::new();
        let waiting = retained.clone();
        let waiter = tokio::spawn(async move { waiting.wait().await });

        tokio::time::sleep(Duration::from_millis(10)).await;
        assert!(!waiter.is_finished());
        retained.publish(completed("done")).expect("first publish");

        assert_eq!(waiter.await.expect("wait task"), completed("done"));
        assert_eq!(retained.wait().await, completed("done"));
        assert_eq!(retained.get(), Some(completed("done")));
    }

    #[test]
    fn competing_publication_preserves_the_first_result() {
        let retained = RetainedActorExit::new();
        retained.publish(completed("first")).expect("first publish");

        let error = retained
            .publish(ActorTerminal {
                kind: ActorExitKind::Failed,
                summary: "second".into(),
                diagnostic: None,
            })
            .expect_err("second publish must fail");

        assert_eq!(error.existing, completed("first"));
        assert_eq!(retained.get(), Some(completed("first")));
    }
}
