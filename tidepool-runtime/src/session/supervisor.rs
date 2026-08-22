//! The turn-abort watchdog primitive shared by every resident-turn driver.
//!
//! A turn that has decided to stop — a timeout window elapsed, or a caller
//! cancelled the RPC — asks a live JIT machine to abort at its next safepoint
//! ([`CancelHandle::cancel`]) and gives it a short, bounded grace period to
//! actually do so before treating it as stuck. That "cancel, then wait a
//! bounded grace for the awaited work to resolve" step is IDENTICAL wherever
//! it appears — `tidepool-repl`'s `server.rs` reimplemented it independently
//! at both its turn-timeout site and its client-cancel site (`drive` and
//! `drive_detached`) before this module existed. [`wait_for_abort_grace`] is
//! the one mechanism; each caller keeps its own policy for what "still
//! running" means (wedge the session, or let a detached resolver keep
//! ownership) and for whether a `tidepool_effect::pause::PauseGate` abort
//! request precedes it.
//!
//! This is deliberately NOT a full turn supervisor unifying
//! `tidepool-runtime`'s own [`super::engine::SessionEngine`] (oneshot,
//! channel-driven, with a resumable `Paused` outcome) with a resident
//! session's direct-JoinHandle driver: their timeout semantics genuinely
//! differ (a resident timeout has no "paused, resumable" state — it recovers
//! to `Idle` or wedges), and forcing them to share one classification would
//! be a real behavior change to safety-critical crash/timeout handling, not
//! a mechanical dedup.

use std::future::Future;
use std::time::Duration;

use tokio::time::timeout;

use tidepool_codegen::jit_machine::CancelHandle;

/// The result of racing `awaited` against a bounded abort grace window.
pub enum GraceOutcome<T> {
    /// `awaited` resolved within the grace window (successfully or not —
    /// callers that need to distinguish a `Result`/`Option` payload do so on
    /// `T` themselves).
    Recovered(T),
    /// The grace window elapsed with no resolution — still running.
    StillRunning,
}

/// Request a cooperative abort on the JIT [`CancelHandle`] lever, then wait up
/// to `grace` for `awaited` to resolve. Callers that also need the
/// `PauseGate` abort lever fire its `request_abort` themselves before calling
/// this — whether that call is unconditional or gated on `cancel` being
/// available is caller policy (the two `tidepool-repl` call sites this
/// factors out of already made different choices there).
pub async fn wait_for_abort_grace<T>(
    cancel: &CancelHandle,
    grace: Duration,
    awaited: impl Future<Output = T>,
) -> GraceOutcome<T> {
    cancel.cancel();
    match timeout(grace, awaited).await {
        Ok(t) => GraceOutcome::Recovered(t),
        Err(_) => GraceOutcome::StillRunning,
    }
}

/// [`wait_for_abort_grace`] for a caller with no [`CancelHandle`] to fire (a
/// runaway before any JIT machine published one) — no abort lever exists, but
/// `awaited` is still raced against `grace` since a `PauseGate` abort alone
/// may still let it resolve.
pub async fn wait_grace_without_cancel<T>(
    grace: Duration,
    awaited: impl Future<Output = T>,
) -> GraceOutcome<T> {
    match timeout(grace, awaited).await {
        Ok(t) => GraceOutcome::Recovered(t),
        Err(_) => GraceOutcome::StillRunning,
    }
}
