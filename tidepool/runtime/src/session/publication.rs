//! One execution's cancellation and public visibility decision.

use parking_lot::Mutex;
use std::sync::Arc;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PublicationPhase {
    Running,
    CancellationRequested,
    CommitClaimed { cancellation_pending: bool },
    Published,
    Terminated,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PublicationCancellation {
    RequestedBeforeCommit,
    PendingCommitOutcome,
    AlreadyPublished,
    AlreadyTerminated,
}

#[derive(Debug)]
pub struct PublicationDecision {
    phase: Mutex<PublicationPhase>,
}

/// Proof that this execution won the short cancellation/commit decision.
/// Dropping it leaves the outcome unconfirmed; the durable manifest remains
/// authoritative if a process fails after rename.
pub struct PublicationClaim {
    decision: Arc<PublicationDecision>,
}

impl PublicationDecision {
    #[must_use]
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            phase: Mutex::new(PublicationPhase::Running),
        })
    }

    #[must_use]
    pub fn phase(&self) -> PublicationPhase {
        *self.phase.lock()
    }

    /// Cancellation before a commit claim prevents publication. Once claimed,
    /// cancellation waits for the manifest outcome and cannot turn a published
    /// write into a retryable failure.
    pub fn request_cancellation(&self) -> PublicationCancellation {
        let mut phase = self.phase.lock();
        match *phase {
            PublicationPhase::Running | PublicationPhase::CancellationRequested => {
                *phase = PublicationPhase::CancellationRequested;
                PublicationCancellation::RequestedBeforeCommit
            }
            PublicationPhase::CommitClaimed { .. } => {
                *phase = PublicationPhase::CommitClaimed {
                    cancellation_pending: true,
                };
                PublicationCancellation::PendingCommitOutcome
            }
            PublicationPhase::Published => PublicationCancellation::AlreadyPublished,
            PublicationPhase::Terminated => PublicationCancellation::AlreadyTerminated,
        }
    }

    /// Call under the machine checkout after staging and revalidation. The
    /// returned claim holds no lock while the existing atomic-write owner
    /// renames and confirms the manifest.
    pub fn claim_commit(self: &Arc<Self>) -> Option<PublicationClaim> {
        let mut phase = self.phase.lock();
        if *phase != PublicationPhase::Running {
            return None;
        }
        *phase = PublicationPhase::CommitClaimed {
            cancellation_pending: false,
        };
        Some(PublicationClaim {
            decision: Arc::clone(self),
        })
    }

    /// Finish an execution that never claimed durable publication.
    pub fn terminate(&self) -> bool {
        let mut phase = self.phase.lock();
        match *phase {
            PublicationPhase::Running | PublicationPhase::CancellationRequested => {
                *phase = PublicationPhase::Terminated;
                true
            }
            PublicationPhase::CommitClaimed { .. }
            | PublicationPhase::Published
            | PublicationPhase::Terminated => false,
        }
    }
}

impl PublicationClaim {
    /// A pre-rename failure has no public visibility. This is a terminal
    /// failed publication; a later execution requires its own decision owner.
    pub fn before_rename_failure(self) -> bool {
        let mut phase = self.decision.phase.lock();
        let PublicationPhase::CommitClaimed {
            cancellation_pending,
        } = *phase
        else {
            unreachable!("only a claimed publication can fail before rename");
        };
        *phase = PublicationPhase::Terminated;
        cancellation_pending
    }

    /// A visible manifest is authoritative even when directory durability
    /// still needs confirmation. The caller retains the atomic-write owner's
    /// confirmation handle separately and withholds normal success until it
    /// has completed.
    pub fn published(self) -> bool {
        let mut phase = self.decision.phase.lock();
        let PublicationPhase::CommitClaimed {
            cancellation_pending,
        } = *phase
        else {
            unreachable!("only a claimed publication can become visible");
        };
        *phase = PublicationPhase::Published;
        cancellation_pending
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cancellation_before_claim_prevents_publication() {
        let decision = PublicationDecision::new();
        assert_eq!(
            decision.request_cancellation(),
            PublicationCancellation::RequestedBeforeCommit
        );
        assert!(decision.claim_commit().is_none());
        assert!(decision.terminate());
        assert_eq!(decision.phase(), PublicationPhase::Terminated);
    }

    #[test]
    fn cancellation_after_claim_waits_for_visible_outcome() {
        let decision = PublicationDecision::new();
        let claim = decision.claim_commit().expect("running execution claims");
        assert_eq!(
            decision.request_cancellation(),
            PublicationCancellation::PendingCommitOutcome
        );
        assert!(!decision.terminate());
        assert!(claim.published());
        assert_eq!(decision.phase(), PublicationPhase::Published);
        assert_eq!(
            decision.request_cancellation(),
            PublicationCancellation::AlreadyPublished
        );
    }

    #[test]
    fn before_rename_failure_never_publishes() {
        let decision = PublicationDecision::new();
        let claim = decision.claim_commit().expect("running execution claims");
        assert!(!claim.before_rename_failure());
        assert_eq!(decision.phase(), PublicationPhase::Terminated);
    }

    #[test]
    fn abandoned_claim_remains_unconfirmed() {
        let decision = PublicationDecision::new();
        drop(decision.claim_commit().expect("running execution claims"));
        assert_eq!(
            decision.phase(),
            PublicationPhase::CommitClaimed {
                cancellation_pending: false
            }
        );
        assert!(!decision.terminate());
    }
}
