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
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ActorTerminal {
    pub kind: ActorExitKind,
    pub summary: String,
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

struct ExitState {
    successor: Mutex<Option<crate::ActorRef>>,
    lifecycle: Mutex<LifecycleState>,
    cleanup: Mutex<Option<crate::ResidentCleanupOutcome>>,
    requested_shutdown: Mutex<Option<ActorTerminal>>,
    acknowledged_retirement: Mutex<Option<crate::ActorRef>>,
    changed: watch::Sender<u64>,
}

/// Cloneable observation of one actor's single-assignment terminal record.
///
/// The record retains metadata only. Keeping or cloning it never creates a
/// second root for the actor's Haskell exit value. Observation is repeatable,
/// and a waiter cannot miss publication between checking and parking.
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
        self.state.cleanup.lock().clone()
    }

    pub(crate) fn retain_cleanup(&self, outcome: crate::ResidentCleanupOutcome) {
        self.state.cleanup.lock().get_or_insert(outcome);
    }

    /// Record intent without publishing an exit. Bootstrap checks this at safe
    /// boundaries; only ordinary lifecycle cleanup may publish the terminal.
    pub(crate) fn request_shutdown(&self, terminal: ActorTerminal) -> ActorTerminal {
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

    pub(crate) async fn wait_requested_shutdown(&self) -> ActorTerminal {
        let mut changed = self.state.changed.subscribe();
        loop {
            if let Some(terminal) = self.requested_shutdown() {
                return terminal;
            }
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
                cleanup: Mutex::new(None),
                requested_shutdown: Mutex::new(None),
                acknowledged_retirement: Mutex::new(None),
                changed,
            }),
        }
    }

    /// Publish the sole terminal result.
    ///
    /// The actor lifecycle owner is the sole expected writer. Returning an
    /// error rather than replacing the value makes competing cleanup paths an
    /// explicit invariant violation while preserving the first result.
    pub fn publish(&self, terminal: ActorTerminal) -> Result<(), ActorExitAlreadyPublished> {
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

    fn completed(summary: &str) -> ActorTerminal {
        ActorTerminal {
            kind: ActorExitKind::Completed,
            summary: summary.into(),
        }
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
            })
            .expect_err("second publish must fail");

        assert_eq!(error.existing, completed("first"));
        assert_eq!(retained.get(), Some(completed("first")));
    }
}
