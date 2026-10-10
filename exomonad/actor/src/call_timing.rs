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
