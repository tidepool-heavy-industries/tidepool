//! Managed coding checkouts and typed repository events — the Rust substrate
//! for authored Haskell to drive.
//!
//! `Worktree` is the public workflow concept. Its current managed storage is an
//! ordinary private repository whose mutable Git metadata lives inside the
//! checkout and whose initial objects are borrowed from a retained source via
//! Git alternates. This lets an actor use normal Git without sharing refs,
//! config, or locks with the source checkout.
//!
//! This crate owns everything that is *git truth*: creating and recording those
//! retained checkouts, snapshotting a dirty source without touching it,
//! observing HEAD movement, and journalling what was observed. It knows nothing
//! about effects, the JIT, Haskell, or agents.
//!
//! The effect surface that exposes this to authored Haskell (`Tidepool.Worktree`,
//! `Tidepool.Event`, `withHandler`) lives outside this crate. Keeping the
//! git substrate free of effect machinery is what lets every behaviour here be
//! tested against a REAL temporary repository driven by a scripted writer,
//! with no mock of git anywhere.
//!
//! ## What this crate deliberately does NOT have
//!
//! No deletion, GC, or retention policy. Retain-first is a locked decision:
//! every checkout, branch, snapshot ref, and receipt survives indefinitely in
//! v1. A checkout a human removed by hand becomes
//! [`WorktreeError::WorktreeLost`]; it is never silently recreated.

#![warn(clippy::unwrap_used, clippy::expect_used)]
pub mod binding;
pub mod create;
pub mod error;
pub mod git;
pub mod id;
pub mod journal;
mod journal_version;
pub mod label;
pub mod merge;
pub mod monitor;
pub mod registry;
pub mod snapshot;
pub mod storage;
pub mod submission;
pub mod testing;

pub use binding::{ActiveBinding, AgentRef, Binding, BindingState, BindingTable, BindingTerminal};
pub use create::{
    DirtyPolicy, WorktreeHandle, WorktreeManager, WorktreeSource, WorktreeSpec,
    TIDEPOOL_BRANCH_PREFIX, TIDEPOOL_SNAPSHOT_REF_PREFIX,
};
pub use error::{DirtySummary, GitFailureReceipt, InProgressKind, WorktreeError};
pub use git::{GitCli, GitOutput};
pub use id::{BranchName, EventId, GitOid, GitRef, SubscriptionId, WorktreeId};
pub use journal::{EventJournal, ObservationBatch};
pub use label::{sanitize_agent_label, sanitize_branch_label};
pub use merge::{merge_branch_into, MergeOutcome};
pub use monitor::{
    CommitReceipt, HeadChangeKind, HeadChangeReceipt, Observed, RepositoryEvent, WorktreeMonitor,
    DEFAULT_POLL_INTERVAL_MS,
};
pub use registry::{
    WorktreeOrigin, WorktreeReceipt, WorktreeRecordStatus, WorktreeRegistry, WorktreeSummary,
};
pub use snapshot::SnapshotReceipt;
pub use submission::{HeadState, SubmissionObservation, WorkingState};
