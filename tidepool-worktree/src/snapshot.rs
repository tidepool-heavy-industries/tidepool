//! Dirty-source snapshots — LANE L2.
//!
//! `allowDirtySnapshot` gives a child the source's exact current content
//! without the operator having committed anything. It writes a hidden synthetic
//! commit through a TEMPORARY git index and parks it on a Tidepool-owned ref.
//!
//! ## The untouched-source proof
//!
//! This is the whole lane. The source repository must be byte-identical before
//! and after, across every one of:
//!
//! 1. the checked-out branch and `HEAD`;
//! 2. the ordinary index (`.git/index`) — content AND mtime-sensitive staging
//!    state, so a `git status` right after is not slower or different;
//! 3. staged content (what `git diff --cached` reports);
//! 4. unstaged content (what `git diff` reports);
//! 5. working-tree file bytes, including mode bits;
//! 6. untracked files (still untracked, still present, unmodified);
//! 7. ignored files (still ignored, still present, NOT captured);
//! 8. reflogs and the branch namespace (no new branch, no `HEAD` reflog entry).
//!
//! The mechanism: `GIT_INDEX_FILE` pointed at a temp path, `git read-tree HEAD`
//! into it, `git add` the selected paths into it, `git write-tree` from it,
//! `git commit-tree` with the source `HEAD` as parent, `git update-ref` the
//! Tidepool snapshot ref. Every one of those invocations carries the temp index
//! in its environment ([`crate::git::GitCli::with_env`]); the source index is
//! never opened for write. The temp index lives outside the source tree.
//!
//! ## What is captured
//!
//! Tracked staged content, tracked unstaged content, and non-ignored untracked
//! files. Ignored files are excluded — a build directory is not part of the
//! program, and copying one into every child worktree is how a snapshot becomes
//! a disk-space incident.
//!
//! ## What is refused, loudly
//!
//! A dirty submodule ([`WorktreeError::DirtySubmoduleUnsupported`]) and any
//! in-progress merge/rebase/cherry-pick
//! ([`WorktreeError::SourceOperationInProgress`]). A synthetic commit of a
//! half-merged tree is a reproducible base for the wrong program, so refusing
//! is the correct outcome, not a limitation to work around.

use std::path::Path;

use crate::error::{DirtySummary, WorktreeError};
use crate::git::GitCli;
use crate::id::{GitOid, GitRef, WorktreeId};

/// What a snapshot captured, recorded alongside the worktree receipt.
///
/// `pre_status` is the source's dirty state as observed BEFORE the snapshot.
/// It is kept because the synthetic commit deliberately does not claim the
/// operator made a commit, and the only honest account of what the base
/// represents is what the tree actually looked like at the time.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SnapshotReceipt {
    pub snapshot_ref: GitRef,
    pub snapshot_commit: GitOid,
    pub source_head: GitOid,
    /// Repository-relative paths included in the synthetic commit, sorted.
    pub captured_paths: Vec<String>,
    pub pre_status: DirtySummary,
}

/// Write the synthetic snapshot commit. Does not create a worktree; returns the
/// base a managed branch is then rooted at.
pub fn snapshot_source(
    git: &GitCli,
    source: &Path,
    worktree_id: &WorktreeId,
    temp_index_dir: &Path,
) -> Result<SnapshotReceipt, WorktreeError> {
    let _ = (git, source, worktree_id, temp_index_dir);
    todo!("L2")
}
