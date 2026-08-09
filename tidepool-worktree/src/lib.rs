//! Managed git worktrees and typed repository events — the Rust substrate for
//! [PRD 19](../../plans/self-iterating-harness/19-managed-worktrees-events-prd.md).
//!
//! This crate owns everything that is *git truth*: creating retained worktrees,
//! recording them durably so a restart can still find them, snapshotting a dirty
//! source without touching it, observing HEAD movement, and journalling what was
//! observed. It knows nothing about effects, the JIT, Haskell, or agents.
//!
//! The effect surface that exposes this to authored Haskell (`Tidepool.Worktree`,
//! `Tidepool.Event`, `withHandler`) lives outside this crate — see
//! `plans/post-restart/worktree-lanes/README.md` for the lane map. Keeping the
//! git substrate free of effect machinery is what lets every behaviour here be
//! tested against a REAL temporary repository driven by a scripted writer,
//! with no mock of git anywhere.
//!
//! ## What this crate deliberately does NOT have
//!
//! No `rebase`, `merge`, `cherry_pick`, conflict resolution, or branch
//! promotion. PRD 19's boundary is creation, lookup, inspection, and events;
//! choreography is authored code and the git work itself belongs to coding
//! agents using their native tools. Adding a workflow verb here is a design
//! regression, not a convenience.
//!
//! No deletion, GC, or retention policy. Retain-first is a locked decision:
//! every worktree, branch, snapshot ref, and receipt survives indefinitely in
//! v1. A worktree a human removed by hand becomes
//! [`WorktreeError::WorktreeLost`]; it is never silently recreated.

pub mod binding;
pub mod create;
pub mod error;
pub mod git;
pub mod id;
pub mod journal;
pub mod monitor;
pub mod registry;
pub mod snapshot;
pub mod testing;

pub use binding::{AgentRef, Binding, BindingState};
pub use create::{
    DirtyPolicy, WorktreeHandle, WorktreeManager, WorktreeSource, WorktreeSpec,
    TIDEPOOL_BRANCH_PREFIX, TIDEPOOL_SNAPSHOT_REF_PREFIX,
};
pub use error::{DirtySummary, GitFailureReceipt, InProgressKind, WorktreeError};
pub use git::{GitCli, GitOutput};
pub use id::{BranchName, EventId, GitOid, GitRef, SubscriptionId, WorktreeId};
pub use journal::{EventJournal, JournalEntry};
pub use monitor::{
    CommitReceipt, HeadChangeKind, HeadChangeReceipt, Observed, RepositoryEvent, WorktreeMonitor,
};
pub use registry::{WorktreeOrigin, WorktreeReceipt, WorktreeRegistry, WorktreeSummary};
pub use snapshot::SnapshotReceipt;
