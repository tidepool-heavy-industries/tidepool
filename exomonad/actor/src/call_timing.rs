//! One summary per hosted tool call or Haskell cell, plus compiler-round
//! timings that identify the owning callsite.
//!
//! Admission opens one [`CallScope`]. Its registration scopes every owned
//! successor and synchronous actor advance into the same totals. Every site
//! downstream that spends wall time on the caller's behalf —
//! `resident_workbench.rs`'s resident-machine checkout wait/hold and its
//! off-checkout GHC compile calls, the `Jev` effect handler, the command
//! effect handler — adds to the open scope with a one-line call
//! (`add_checkout_wait_ms`, `add_checkout_hold_ms`, `timed_compile`,
//! `add_jev_ms`, `add_exec_ms`). The scope carries as a task-local, so any
//! of those sites can reach it without threading an accumulator parameter
//! through every intervening signature; a call to one of these functions
//! outside an open scope (an ordinary interactive turn is not itself one
//! hosted call or cell) is a harmless no-op.
//!
//! `CallScope::finish` emits the single summary line when the call settles,
//! however it settles; the execution retains its single `finish` owner.

use std::future::Future;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::Instant;

#[derive(Default)]
struct Totals {
    execution: Option<tidepool_runtime::session::WorkbenchExecutionId>,
    checkout_wait_ms: AtomicU64,
    checkout_hold_ms: AtomicU64,
    compile_ms: AtomicU64,
    compile_count: AtomicU64,
    compile_observations: AtomicU64,
    preparation_wait_ms: AtomicU64,
    preparation_wait_count: AtomicU64,
    semantic_rebases: AtomicU64,
    metadata_restages: AtomicU64,
    publication_attempts: AtomicU64,
    latest_join_reservation: AtomicU64,
    jev_ms: AtomicU64,
    jev_count: AtomicU64,
    exec_ms: AtomicU64,
}

tokio::task_local! {
    static CURRENT: Arc<Totals>;
}

fn saturating_add(field: &AtomicU64, ms: u128) {
    field.fetch_add(u64::try_from(ms).unwrap_or(u64::MAX), Ordering::Relaxed);
}

// This origin is local to this process and clock domain. Reporter consumers
// must not compare it with the compiler worker's CLOCK_MONOTONIC timestamps.
fn observation_ns() -> u64 {
    static ORIGIN: OnceLock<Instant> = OnceLock::new();
    ORIGIN.get_or_init(Instant::now).elapsed().as_nanos() as u64
}

/// A wait is caller work; the shared producer's service is recorded once under
/// its preparation identity instead of being added to every caller's compile sum.
pub(crate) struct PreparationWait {
    interval: PreparationInterval,
    kind: PreparationWaitKind,
    totals: Option<Arc<Totals>>,
}

enum PreparationWaitKind {
    Lookup(crate::agent_spec::preparation::PreparationLookupDisposition),
    Restore,
}

impl PreparationWaitKind {
    fn as_str(&self) -> &'static str {
        match self {
            Self::Lookup(disposition) => disposition.as_str(),
            Self::Restore => "restore",
        }
    }
}

impl PreparationWait {
    pub(crate) fn new(
        preparation: uuid::Uuid,
        disposition: crate::agent_spec::preparation::PreparationLookupDisposition,
    ) -> Self {
        Self::with_kind(preparation, PreparationWaitKind::Lookup(disposition))
    }

    pub(crate) fn restore(preparation: uuid::Uuid) -> Self {
        Self::with_kind(preparation, PreparationWaitKind::Restore)
    }

    fn with_kind(preparation: uuid::Uuid, kind: PreparationWaitKind) -> Self {
        Self {
            interval: PreparationInterval::new(preparation, PreparationPhase::Wait),
            kind,
            totals: CURRENT.try_with(Arc::clone).ok(),
        }
    }

    pub(crate) fn complete(&mut self, succeeded: bool) {
        self.interval.complete(succeeded);
    }
}

impl Drop for PreparationWait {
    fn drop(&mut self) {
        let elapsed = self.interval.started.elapsed();
        if let Some(totals) = &self.totals {
            saturating_add(&totals.preparation_wait_ms, elapsed.as_millis());
            totals
                .preparation_wait_count
                .fetch_add(1, Ordering::Relaxed);
        }
        tracing::info!(target: "exomonad_actor::toolset_preparation",
            process_id = std::process::id(), clock_domain = "actor_process_monotonic",
            preparation = %self.interval.preparation,
            execution = self.totals.as_ref().and_then(|totals| totals.execution.as_ref()).map(|id| id.as_str()),
            disposition = self.kind.as_str(),
            start_ns = self.interval.start_ns, end_ns = observation_ns(),
            wall_ns = elapsed.as_nanos() as u64, outcome = self.interval.outcome.as_str(),
            "toolset preparation wait");
    }
}

pub(crate) struct PreparationInterval {
    preparation: uuid::Uuid,
    phase: PreparationPhase,
    started: Instant,
    start_ns: u64,
    outcome: PreparationOutcome,
}

enum PreparationOutcome {
    Abandoned,
    Succeeded,
    Failed,
}

impl PreparationOutcome {
    fn as_str(&self) -> &'static str {
        match self {
            Self::Abandoned => "abandoned",
            Self::Succeeded => "succeeded",
            Self::Failed => "failed",
        }
    }
}

#[derive(Clone, Copy)]
pub(crate) enum PreparationPhase {
    Wait,
    Producer,
    Service,
}

impl PreparationPhase {
    fn as_str(self) -> &'static str {
        match self {
            Self::Wait => "wait",
            Self::Producer => "producer",
            Self::Service => "service",
        }
    }
}

impl PreparationInterval {
    pub(crate) fn new(preparation: uuid::Uuid, phase: PreparationPhase) -> Self {
        Self {
            preparation,
            phase,
            started: Instant::now(),
            start_ns: observation_ns(),
            outcome: PreparationOutcome::Abandoned,
        }
    }

    pub(crate) fn complete(&mut self, succeeded: bool) {
        self.outcome = if succeeded {
            PreparationOutcome::Succeeded
        } else {
            PreparationOutcome::Failed
        };
    }
}

impl Drop for PreparationInterval {
    fn drop(&mut self) {
        // The wait owns its additional caller/disposition fields and emits once.
        if matches!(self.phase, PreparationPhase::Wait) {
            return;
        }
        tracing::info!(target: "exomonad_actor::toolset_preparation",
            process_id = std::process::id(), clock_domain = "actor_process_monotonic",
            preparation = %self.preparation, phase = self.phase.as_str(),
            start_ns = self.start_ns, end_ns = observation_ns(),
            wall_ns = self.started.elapsed().as_nanos() as u64, outcome = self.outcome.as_str(),
            "toolset preparation producer");
    }
}

struct CompilerInterval {
    totals: Option<Arc<Totals>>,
    caller: &'static std::panic::Location<'static>,
    started: Instant,
    start_ns: u64,
    sequence: u64,
    completed: bool,
}

impl CompilerInterval {
    fn new(totals: Option<Arc<Totals>>, caller: &'static std::panic::Location<'static>) -> Self {
        let sequence = totals.as_ref().map_or(0, |value| {
            value.compile_observations.fetch_add(1, Ordering::Relaxed) + 1
        });
        Self {
            totals,
            caller,
            started: Instant::now(),
            start_ns: observation_ns(),
            sequence,
            completed: false,
        }
    }

    fn complete(&mut self) {
        self.completed = true;
    }
}

impl Drop for CompilerInterval {
    fn drop(&mut self) {
        if let Some(totals) = &self.totals {
            let elapsed = self.started.elapsed();
            let round = self.completed.then(|| {
                saturating_add(&totals.compile_ms, elapsed.as_millis());
                totals.compile_count.fetch_add(1, Ordering::Relaxed) + 1
            });
            tracing::info!(target: "exomonad_actor::call_timing",
                process_id = std::process::id(), clock_domain = "actor_process_monotonic",
                start_ns = self.start_ns, end_ns = observation_ns(), wall_ns = elapsed.as_nanos() as u64,
                observation_sequence = self.sequence, compile_round = round,
                execution = totals.execution.as_ref().map(|id| id.as_str()),
                compile_ms = elapsed.as_millis(), caller_file = self.caller.file(), caller_line = self.caller.line(),
                outcome = if self.completed { "returned" } else { "abandoned" },
                "compiler round timing");
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub(crate) enum PublicationRetryReason {
    Semantic,
    Metadata,
}
pub(crate) fn publication_attempt(reservation: Option<tidepool_repr::Generation>) {
    CURRENT
        .try_with(|totals| {
            totals.publication_attempts.fetch_add(1, Ordering::Relaxed);
            if let Some(reservation) = reservation {
                totals
                    .latest_join_reservation
                    .store(reservation.0, Ordering::Relaxed);
            }
        })
        .ok();
}
pub(crate) fn publication_retry(reason: PublicationRetryReason) {
    CURRENT
        .try_with(|totals| {
            let field = match reason {
                PublicationRetryReason::Semantic => &totals.semantic_rebases,
                PublicationRetryReason::Metadata => &totals.metadata_restages,
            };
            field.fetch_add(1, Ordering::Relaxed);
        })
        .ok();
}

/// Add to the open scope's checkout-wait total; a no-op with no open scope.
pub fn add_checkout_wait_ms(ms: u128) {
    CURRENT
        .try_with(|totals| saturating_add(&totals.checkout_wait_ms, ms))
        .ok();
}

/// Add to the open scope's checkout-hold total; a no-op with no open scope.
pub fn add_checkout_hold_ms(ms: u128) {
    CURRENT
        .try_with(|totals| saturating_add(&totals.checkout_hold_ms, ms))
        .ok();
}

/// Add to the open scope's Jev-request total; a no-op with no open scope.
pub fn add_jev_ms(ms: u128) {
    CURRENT
        .try_with(|totals| {
            saturating_add(&totals.jev_ms, ms);
            totals.jev_count.fetch_add(1, Ordering::Relaxed);
        })
        .ok();
}

/// Add to the open scope's command-execution total; a no-op with no open scope.
pub fn add_exec_ms(ms: u128) {
    CURRENT
        .try_with(|totals| saturating_add(&totals.exec_ms, ms))
        .ok();
}

/// Time one compiler operation, including native preparation when the caller
/// wraps it. Record its callsite and elapsed time in the open scope. With no
/// open scope, the body still runs but contributes no timing evidence.
#[track_caller]
pub fn timed_compile<F: Future>(body: F) -> impl Future<Output = F::Output> {
    // Capture at construction rather than inside the async block: the latter
    // identifies its poll machinery instead of the owning compiler operation.
    let caller = std::panic::Location::caller();
    async move {
        let mut interval = CompilerInterval::new(CURRENT.try_with(Arc::clone).ok(), caller);
        let result = body.await;
        interval.complete();
        result
    }
}

/// One open per-call accumulator: `new`, `run` the call future inside it
/// (nested sites add to it as they spend time), then `finish` to emit the
/// one summary line.
pub struct CallScope {
    kind: String,
    actor_id: u64,
    incarnation: u64,
    started: Instant,
    start_ns: u64,
    totals: Arc<Totals>,
}

#[derive(Clone)]
pub(crate) struct CallTimingRegistration(Arc<Totals>);

impl CallTimingRegistration {
    pub(crate) fn scope<F: Future>(&self, body: F) -> impl Future<Output = F::Output> {
        CURRENT.scope(Arc::clone(&self.0), body)
    }

    pub(crate) fn sync_scope<T>(&self, body: impl FnOnce() -> T) -> T {
        CURRENT.sync_scope(Arc::clone(&self.0), body)
    }

    /// Time one synchronous compiler request made by a blocking worker.
    /// Record each actual batch at its boundary, including lookup fallbacks
    /// and discovery requests.
    #[track_caller]
    pub(crate) fn timed_compile_sync<T>(&self, body: impl FnOnce() -> T) -> T {
        let caller = std::panic::Location::caller();
        let mut interval = CompilerInterval::new(Some(Arc::clone(&self.0)), caller);
        let result = self.sync_scope(body);
        interval.complete();
        result
    }
}

pub(crate) fn current_registration() -> Option<CallTimingRegistration> {
    CURRENT
        .try_with(|totals| CallTimingRegistration(Arc::clone(totals)))
        .ok()
}

impl CallScope {
    pub(crate) fn registration(&self) -> CallTimingRegistration {
        CallTimingRegistration(Arc::clone(&self.totals))
    }
    /// `kind` names the call: a hosted tool's name, or `"cell"` for a
    /// Haskell cell.
    pub fn new(kind: impl Into<String>, actor_id: u64, incarnation: u64) -> Self {
        Self::for_execution(kind, actor_id, incarnation, None)
    }

    pub(crate) fn for_execution(
        kind: impl Into<String>,
        actor_id: u64,
        incarnation: u64,
        execution: Option<tidepool_runtime::session::WorkbenchExecutionId>,
    ) -> Self {
        Self {
            kind: kind.into(),
            actor_id,
            incarnation,
            started: Instant::now(),
            start_ns: observation_ns(),
            totals: Arc::new(Totals {
                execution,
                ..Totals::default()
            }),
        }
    }

    /// Run `body` with this scope open as the task-local target for every
    /// nested `add_*`/`timed_compile` call it makes (directly, or through
    /// any function it awaits — task-locals cross `.await` points within one
    /// task, though not into a separate `spawn`/`spawn_blocking` task).
    pub fn run<F: Future>(&self, body: F) -> impl Future<Output = F::Output> {
        CURRENT.scope(Arc::clone(&self.totals), body)
    }

    /// The open scope's compile-round-trip count so far — one `timed_compile`
    /// call each, regardless of how many compiler phases run inside one
    /// request. Exposed for tests
    /// that assert on it directly rather than parsing `finish`'s log line.
    #[cfg(test)]
    pub(crate) fn compile_count(&self) -> u64 {
        self.totals.compile_count.load(Ordering::Relaxed)
    }

    /// Emit the one summary INFO line for this call and consume the scope.
    pub fn finish(self, outcome: &str) {
        tracing::info!(
            target: "exomonad_actor::call_timing",
            process_id = std::process::id(), clock_domain = "actor_process_monotonic",
            start_ns = self.start_ns, end_ns = observation_ns(), wall_ns = self.started.elapsed().as_nanos() as u64,
            tool = %self.kind,
            actor = self.actor_id,
            incarnation = self.incarnation,
            execution = self.totals.execution.as_ref().map(|id| id.as_str()),
            total_ms = self.started.elapsed().as_millis(),
            checkout_wait_ms = self.totals.checkout_wait_ms.load(Ordering::Relaxed),
            checkout_hold_ms = self.totals.checkout_hold_ms.load(Ordering::Relaxed),
            compile_ms = self.totals.compile_ms.load(Ordering::Relaxed),
            compile_count = self.totals.compile_count.load(Ordering::Relaxed),
            preparation_wait_ms = self.totals.preparation_wait_ms.load(Ordering::Relaxed),
            preparation_wait_count = self.totals.preparation_wait_count.load(Ordering::Relaxed),
            semantic_rebases = self.totals.semantic_rebases.load(Ordering::Relaxed),
            metadata_restages = self.totals.metadata_restages.load(Ordering::Relaxed),
            publication_attempts = self.totals.publication_attempts.load(Ordering::Relaxed),
            latest_join_reservation = match self.totals.latest_join_reservation.load(Ordering::Relaxed) { 0 => None, generation => Some(generation) },
            jev_ms = self.totals.jev_ms.load(Ordering::Relaxed),
            jev_count = self.totals.jev_count.load(Ordering::Relaxed),
            exec_ms = self.totals.exec_ms.load(Ordering::Relaxed),
            outcome = %outcome,
            "call timing"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn shared_preparation_waits_do_not_multiply_compile_work() {
        use crate::agent_spec::preparation::PreparationLookupDisposition;

        let log = tempfile::NamedTempFile::new().unwrap();
        let subscriber = tracing_subscriber::fmt()
            .json()
            .with_ansi(false)
            .with_writer(log.reopen().unwrap())
            .finish();
        let guard = tracing::subscriber::set_default(subscriber);
        let preparation = uuid::Uuid::new_v4();
        let execution = tidepool_runtime::session::WorkbenchExecutionId::from_digest([1; 16]);
        let first = CallScope::for_execution("reload", 1, 1, Some(execution.clone()));
        let joined = CallScope::new("reload", 2, 1);
        let mut producer = PreparationInterval::new(preparation, PreparationPhase::Producer);
        let mut service = PreparationInterval::new(preparation, PreparationPhase::Service);
        first
            .run(async {
                let mut wait = PreparationWait::new(preparation, PreparationLookupDisposition::New);
                wait.interval.started -= std::time::Duration::from_millis(17);
                wait.complete(true);
            })
            .await;
        joined
            .run(async {
                let mut wait =
                    PreparationWait::new(preparation, PreparationLookupDisposition::JoinedPending);
                wait.interval.started -= std::time::Duration::from_millis(11);
                // An abandoned wait still reports time without cancelling its producer.
            })
            .await;
        assert!(first.totals.preparation_wait_ms.load(Ordering::Relaxed) >= 17);
        assert!(joined.totals.preparation_wait_ms.load(Ordering::Relaxed) >= 11);
        for scope in [&first, &joined] {
            assert_eq!(
                scope.totals.preparation_wait_count.load(Ordering::Relaxed),
                1
            );
            assert_eq!(scope.compile_count(), 0);
            assert_eq!(scope.totals.compile_ms.load(Ordering::Relaxed), 0);
        }
        let unrelated = CallScope::new("cell", 3, 1);
        assert_eq!(
            unrelated
                .totals
                .preparation_wait_count
                .load(Ordering::Relaxed),
            0
        );
        service.complete(true);
        drop(service);
        producer.complete(true);
        drop(producer);
        first.finish("Committed");
        joined.finish("Cancelled");
        drop(guard);
        let rows = std::fs::read_to_string(log.path())
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
            .collect::<Vec<_>>();
        let waits = rows
            .iter()
            .filter(|row| row["fields"]["message"] == "toolset preparation wait")
            .map(|row| &row["fields"])
            .collect::<Vec<_>>();
        assert_eq!(waits.len(), 2);
        assert!(waits
            .iter()
            .all(|row| row["preparation"] == preparation.to_string()));
        assert_eq!(waits[0]["execution"], execution.as_str());
        assert_eq!(waits[0]["disposition"], "new");
        assert_eq!(waits[0]["outcome"], "succeeded");
        assert_eq!(waits[1]["disposition"], "joined_pending");
        assert_eq!(waits[1]["outcome"], "abandoned");
        let producers = rows
            .iter()
            .filter(|row| row["fields"]["message"] == "toolset preparation producer")
            .map(|row| &row["fields"])
            .collect::<Vec<_>>();
        assert_eq!(producers.len(), 2);
        assert_eq!(producers[0]["phase"], "service");
        assert_eq!(producers[1]["phase"], "producer");
        assert!(producers
            .iter()
            .all(|row| row["preparation"] == preparation.to_string()));
        let summaries = rows
            .iter()
            .filter(|row| row["fields"]["message"] == "call timing")
            .map(|row| &row["fields"])
            .collect::<Vec<_>>();
        assert_eq!(summaries.len(), 2);
        for summary in summaries {
            assert_eq!(summary["preparation_wait_count"], 1);
            assert_eq!(summary["compile_count"], 0);
        }
    }

    #[tokio::test]
    async fn accumulates_only_inside_its_own_scope() {
        // Outside any scope, every add is a documented no-op.
        add_checkout_wait_ms(5);
        add_jev_ms(7);

        let scope = CallScope::new("bash", 1, 2);
        scope
            .run(async {
                add_checkout_wait_ms(10);
                add_checkout_hold_ms(20);
                add_jev_ms(30);
                add_jev_ms(40);
                add_exec_ms(50);
                timed_compile(async { 1 + 1 }).await
                    + current_registration()
                        .expect("current call registration")
                        .timed_compile_sync(|| 2 + 2)
            })
            .await;
        assert_eq!(scope.totals.checkout_wait_ms.load(Ordering::Relaxed), 10);
        assert_eq!(scope.totals.checkout_hold_ms.load(Ordering::Relaxed), 20);
        assert_eq!(scope.totals.jev_ms.load(Ordering::Relaxed), 70);
        assert_eq!(scope.totals.jev_count.load(Ordering::Relaxed), 2);
        assert_eq!(scope.totals.exec_ms.load(Ordering::Relaxed), 50);
        assert_eq!(scope.totals.compile_count.load(Ordering::Relaxed), 2);
    }

    #[tokio::test]
    async fn cancelled_compile_is_observed_without_claiming_completion() {
        let scope = CallScope::new("cell", 1, 1);
        scope
            .run(async {
                let mut pending = Box::pin(timed_compile(std::future::pending::<()>()));
                std::future::poll_fn(|context| {
                    assert!(pending.as_mut().poll(context).is_pending());
                    std::task::Poll::Ready(())
                })
                .await;
                drop(pending);
                assert_eq!(scope.compile_count(), 0);
                timed_compile(async {}).await;
            })
            .await;
        assert_eq!(scope.compile_count(), 1);
        assert_eq!(scope.totals.compile_observations.load(Ordering::Relaxed), 2);
    }

    #[tokio::test]
    async fn successor_tasks_and_synchronous_joins_share_only_their_execution_totals() {
        let first = CallScope::new("cell", 1, 1);
        let second = CallScope::new("cell", 2, 1);
        let registration = first.registration();
        let successor = registration.clone();
        tokio::spawn(async move {
            successor
                .scope(async {
                    add_checkout_wait_ms(13);
                    timed_compile(async {}).await;
                })
                .await;
        })
        .await
        .expect("owned successor");
        registration.sync_scope(|| add_checkout_hold_ms(17));
        second
            .run(async {
                add_checkout_wait_ms(23);
            })
            .await;
        add_checkout_wait_ms(29);
        assert_eq!(first.totals.checkout_wait_ms.load(Ordering::Relaxed), 13);
        assert_eq!(first.totals.checkout_hold_ms.load(Ordering::Relaxed), 17);
        assert_eq!(first.compile_count(), 1);
        assert_eq!(second.totals.checkout_wait_ms.load(Ordering::Relaxed), 23);
        assert_eq!(second.compile_count(), 0);
    }
}
