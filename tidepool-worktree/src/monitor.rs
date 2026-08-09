//! Repository observation — LANE L3.
//!
//! Polling and reconciliation turn "the repository moved" into typed facts.
//! Hooks, when they arrive, are only a wake-up; the source of truth is always a
//! fresh git read, never a hook payload, a filesystem notification, or an
//! agent's account of what it did.
//!
//! ## Coalesced deltas, stated honestly
//!
//! An observer that finds `HEAD` at C after last seeing A reports ONE
//! transition, even if the tree passed through B. This stream is a sequence of
//! state deltas, not a movement log, and no consumer may treat it as exhaustive
//! history. The dependency-propagation job — "children should rebase onto the
//! parent's latest" — needs only latest-state semantics, which coalescing
//! preserves exactly.
//!
//! When the intermediate history is not recoverable, classification degrades to
//! [`HeadChangeKind::UnknownChange`]. That is a correct answer, not a failure:
//! inventing `Advanced` for a movement that was actually a reset would send a
//! child rebasing onto a commit that no longer means what the classification
//! claimed.
//!
//! ## Honest `commit` classification
//!
//! [`RepositoryEvent::Commit`] is emitted only when a commit can be honestly
//! inferred from git state. The monitor NEVER attributes causality to an agent
//! or a model — it reports what the repository became, and the agent receipts
//! from PRD 18 separately report what the harness observed a worker doing.
//! Those two are complementary evidence; conflating them would let a worker's
//! prose become proof of a commit.
//!
//! ## One event, one id
//!
//! A normal commit produces a `Commit` observation and a `HeadChanged`
//! observation sharing one [`EventId`], because they are two views of one
//! underlying change. The id is minted once per reconciliation pass.

use serde::{Deserialize, Serialize};

use crate::error::WorktreeError;
use crate::git::GitCli;
use crate::id::{BranchName, EventId, GitOid, WorktreeId};

/// An observation, carrying the runtime identity that ties co-emitted views of
/// one change together.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Observed<T> {
    pub event_id: EventId,
    pub value: T,
}

/// How `HEAD` moved. PRD 19 fixes this set.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum HeadChangeKind {
    /// Fast-forward: the old head is an ancestor of the new one. Carries the
    /// commits gained, oldest first.
    Advanced(Vec<GitOid>),
    /// The tip commit was replaced by one with the same parent(s): `(old, new)`.
    Amended(GitOid, GitOid),
    /// History was rewritten: old/new pairs where they could be matched up.
    Rewritten(Vec<(GitOid, GitOid)>),
    /// The new head is an ancestor of the old one.
    Rewound,
    /// The worktree changed which branch it is on.
    Switched,
    /// The movement is real but its shape is not honestly recoverable.
    UnknownChange,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct HeadChangeReceipt {
    pub worktree: WorktreeId,
    /// `None` on the first observation of a worktree that had no recorded head.
    pub old_head: Option<GitOid>,
    pub new_head: GitOid,
    pub kind: HeadChangeKind,
    /// `None` on a detached HEAD.
    pub branch: Option<BranchName>,
    pub observed_at_ms: i64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CommitReceipt {
    pub worktree: WorktreeId,
    pub oid: GitOid,
    pub parents: Vec<GitOid>,
    pub subject: String,
    pub author: String,
    pub committed_at_ms: i64,
    /// Repository-relative paths the commit touched.
    pub files: Vec<String>,
}

/// One reconciled repository fact.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum RepositoryEvent {
    HeadChanged(HeadChangeReceipt),
    Commit(CommitReceipt),
}

impl RepositoryEvent {
    pub fn worktree(&self) -> &WorktreeId {
        match self {
            RepositoryEvent::HeadChanged(r) => &r.worktree,
            RepositoryEvent::Commit(r) => &r.worktree,
        }
    }
}

/// Watches one or more managed worktrees.
///
/// The monitor holds the LAST OBSERVED state per worktree — that is legitimate
/// state (a delta needs a previous), unlike caching git facts, which is not.
/// It must survive restart by re-reading its last journalled observation rather
/// than by assuming an in-memory baseline.
#[derive(Debug)]
pub struct WorktreeMonitor {
    git: GitCli,
}

impl WorktreeMonitor {
    pub fn new(git: GitCli) -> Self {
        Self { git }
    }

    /// Reconcile one worktree against its last observed state and return the
    /// facts that follow, in observation order, sharing one [`EventId`] when
    /// they describe one underlying change. Returns empty when nothing moved.
    ///
    /// Idempotent: reconciling twice with no writer in between yields nothing
    /// the second time.
    pub fn reconcile(
        &mut self,
        worktree: &WorktreeId,
    ) -> Result<Vec<RepositoryEvent>, WorktreeError> {
        let _ = worktree;
        todo!("L3")
    }
}
