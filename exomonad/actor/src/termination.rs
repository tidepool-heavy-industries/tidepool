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
/// Successful domain values are deliberately absent. They remain in the
/// shared Haskell exit cell carried by `Tidepool.Actor.ActorRef`; this record
/// supplies only the Rust-owned lifecycle fact that sequences reading it.
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
pub(crate) enum CompilerWorkClose {
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
pub(crate) struct CompilerWorkReceipt(Arc<Mutex<CompilerWorkClose>>);

impl CompilerWorkReceipt {
    pub(crate) fn pending() -> Self {
        Self(Arc::new(Mutex::new(CompilerWorkClose::Pending)))
    }
    pub(crate) fn observation(&self) -> CompilerWorkClose {
        self.0.lock().clone()
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
    pub(crate) fn run<T>(
        self,
        cancellation: tidepool_runtime::CompilerTransactionCancellation,
        action: impl FnOnce() -> T,
    ) -> T {
        self.run_for_workload(
            tidepool_toolchain::artifacts::CompileWorkload::Foreground,
            cancellation,
            action,
        )
    }

    /// Own the complete compiler scope with its declared admission workload.
    /// Borrowed commands inside the action do not settle this ticket early.
    pub(crate) fn run_for_workload<T>(
        self,
        workload: tidepool_toolchain::artifacts::CompileWorkload,
        cancellation: tidepool_runtime::CompilerTransactionCancellation,
        action: impl FnOnce() -> T,
    ) -> T {
        tidepool_toolchain::artifacts::with_compiler_transaction_cancellable_for_workload(
            workload,
            cancellation,
            move |close| {
                self.consume(tidepool_runtime::CompilerTransactionOutcome { action: (), close })
            },
            action,
        )
        .action
    }

    pub(crate) fn consume<T>(
        mut self,
        outcome: tidepool_runtime::CompilerTransactionOutcome<T>,
    ) -> T {
        *self.receipt.0.lock() = CompilerWorkClose::Settled(outcome.close);
        self.settled = true;
        self.owner.close_settled();
        outcome.action
    }
}

impl Drop for CompilerWorkTicket {
    fn drop(&mut self) {
        if !self.settled {
            *self.receipt.0.lock() = CompilerWorkClose::Abandoned;
            self.owner.close_settled();
        }
    }
}

#[derive(Default)]
struct RetainedCleanupState {
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
        self.state.cleanup.lock().outcome.get_or_insert(outcome);
    }

    pub(crate) fn register_compiler_work(&self, receipt: CompilerWorkReceipt) -> bool {
        let mut state = self.state.cleanup.lock();
        if state.outcome.is_some() {
            return false;
        }
        state.compilers.push(receipt);
        true
    }

    pub(crate) fn notify_compiler_close(&self) {
        self.state.changed.send_modify(|revision| *revision += 1);
    }

    #[cfg(test)]
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
        self.state.changed.send_modify(|revision| *revision += 1);
        terminal
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
            retained.publish(ActorLifecycle::Exited(terminal));
        }
        self.state
            .changed
            .send_modify(|generation| *generation += 1);
        Ok(())
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
            crate::resident_workbench::CompilerCloseOwner::Initialization(owner.clone()),
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
