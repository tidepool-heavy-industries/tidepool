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
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActorTerminal {
    pub kind: ActorExitKind,
    pub summary: String,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("actor exit was already published as {existing:?}")]
pub struct ActorExitAlreadyPublished {
    pub existing: ActorTerminal,
}

struct ExitState {
    terminal: Mutex<Option<ActorTerminal>>,
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
    #[must_use]
    pub fn new() -> Self {
        let (changed, _) = watch::channel(0);
        Self {
            state: Arc::new(ExitState {
                terminal: Mutex::new(None),
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
            let mut retained = self.state.terminal.lock();
            if let Some(existing) = retained.as_ref() {
                return Err(ActorExitAlreadyPublished {
                    existing: existing.clone(),
                });
            }
            *retained = Some(terminal);
        }
        self.state
            .changed
            .send_modify(|generation| *generation += 1);
        Ok(())
    }

    /// Return the immutable result without consuming it.
    #[must_use]
    pub fn get(&self) -> Option<ActorTerminal> {
        self.state.terminal.lock().clone()
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
