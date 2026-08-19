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
//! No deletion, GC, or retention policy. Retain-first is a locked decision:
//! every worktree, branch, snapshot ref, and receipt survives indefinitely in
//! v1. A worktree a human removed by hand becomes
//! [`WorktreeError::WorktreeLost`]; it is never silently recreated.

#![warn(clippy::unwrap_used, clippy::expect_used)]
pub mod binding;
pub mod create;
pub mod error;
pub mod git;
pub mod id;
pub mod journal;
pub mod label;
pub mod merge;
pub mod monitor;
pub mod registry;
pub mod snapshot;
pub mod storage;
pub mod testing;

pub use binding::{ActiveBinding, AgentRef, Binding, BindingState, BindingTable, BindingTerminal};
pub use create::{
    DirtyPolicy, WorktreeHandle, WorktreeManager, WorktreeSource, WorktreeSpec,
    TIDEPOOL_BRANCH_PREFIX, TIDEPOOL_SNAPSHOT_REF_PREFIX,
};
pub use error::{DirtySummary, GitFailureReceipt, InProgressKind, WorktreeError};
pub use git::{GitCli, GitOutput};
pub use id::{BranchName, EventId, GitOid, GitRef, SubscriptionId, WorktreeId};
pub use journal::{EventJournal, JournalEntry};
pub use label::{AgentLabel, BranchLabel};
pub use merge::{merge_branch_into, MergeOutcome};
pub use monitor::{
    CommitReceipt, HeadChangeKind, HeadChangeReceipt, Observed, RepositoryEvent, WorktreeMonitor,
    DEFAULT_POLL_INTERVAL_MS,
};
pub use registry::{
    WorktreeOrigin, WorktreeReceipt, WorktreeRecordStatus, WorktreeRegistry, WorktreeSummary,
};
pub use snapshot::SnapshotReceipt;
