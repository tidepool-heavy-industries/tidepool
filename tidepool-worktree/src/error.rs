//! Typed failures. Every one of these is case-matchable by an authored
//! resident — that is the point. A worktree operation never returns a bare
//! string an author has to regex.

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

use crate::id::WorktreeId;

/// PRD 19's `WorktreeError`, plus two variants the PRD's prose requires but its
/// illustrative ADT did not spell out.
///
/// EXTENSIONS OVER THE PRD SNIPPET, and why each is not a scope creep:
///
/// - [`WorktreeError::SourceOperationInProgress`] — the PRD's dirty-snapshot
///   section says a source with `MERGE_HEAD`/`REBASE_HEAD` present must "fail
///   loudly", because a synthetic commit of a half-merged tree is a
///   reproducible base for the WRONG program. Folding that into `SourceDirty`
///   would tell an author to commit their changes, which is exactly the wrong
///   advice mid-rebase.
/// - [`WorktreeError::WorktreeBusy`] — the coupling revision requires that
///   binding a second agent to a bound worktree "fails explicitly". An explicit
///   failure needs a variant to be explicit *as*.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, thiserror::Error)]
pub enum WorktreeError {
    /// The source working tree has uncommitted state and the spec did not opt
    /// into a snapshot. Carries enough detail for an author to decide whether
    /// to commit, stash, or re-run with `allowDirtySnapshot`.
    #[error("source repository is dirty: {0}")]
    SourceDirty(DirtySummary),

    /// The path is not inside a git repository at all.
    #[error("not a git repository: {}", .0.display())]
    NotARepository(PathBuf),

    /// The registry still has a record for this id, but the worktree it names
    /// is gone from disk (a human removed it). NEVER silently recreated —
    /// retain-first means a lost tree is reported, not reconstructed.
    #[error("managed worktree {0} is registered but missing on disk")]
    WorktreeLost(WorktreeId),

    /// No registry record for this id — it was never registered here.
    ///
    /// DISTINCT FROM [`WorktreeError::WorktreeLost`], and the distinction is
    /// the point: this is a typo or a stale id from another registry, while
    /// `WorktreeLost` is data loss. Collapsing them hides the second behind the
    /// first, so an operator investigating a vanished worktree would be told it
    /// never existed. `lookup` distinguishes them by returning `Ok(None)` here;
    /// paths that must produce a handle (seeding a worktree `fromWorktree` off
    /// an unknown id) have no `None` to return and raise this instead.
    #[error("no managed worktree registered with id {0}")]
    WorktreeNotRegistered(WorktreeId),

    /// The caller pointed the registry at a root inside a git working tree.
    ///
    /// A deployment misconfiguration rather than a per-call outcome, but typed
    /// rather than a panic because it is the never-dirty-the-source invariant
    /// caught at the one moment it can still be prevented, and a resident that
    /// can catch it can fall back to a correct root instead of dying. Note the
    /// check is the broader "inside ANY working tree", since `open` is given
    /// only a root and not the source repository it must stay out of.
    #[error("registry root {} resolves inside the git working tree at {} — the registry must live outside every source repository", .root.display(), .inside.display())]
    InvalidRegistryRoot { root: PathBuf, inside: PathBuf },

    /// A dirty submodule in the source. v1 refuses rather than snapshotting a
    /// gitlink whose pointed-at content it did not capture.
    #[error("dirty submodule is unsupported in v1: {}", .0.display())]
    DirtySubmoduleUnsupported(PathBuf),

    /// The source is mid-merge / mid-rebase / mid-cherry-pick. See the type
    /// docs for why this is not `SourceDirty`.
    #[error("source repository has an operation in progress: {0}")]
    SourceOperationInProgress(InProgressKind),

    /// One worktree, one agent. Binding a second agent to an already-bound
    /// worktree fails here rather than silently producing two writers.
    #[error("worktree {worktree} is already bound to agent {holder}")]
    WorktreeBusy {
        worktree: WorktreeId,
        holder: String,
    },

    /// git itself failed. The receipt carries the invocation and its output so
    /// the failure is diagnosable without re-running anything.
    #[error("git failed: {0}")]
    GitFailure(GitFailureReceipt),
}

/// Which in-progress operation blocked the source. Distinguished rather than
/// collapsed to a string so a resident can branch on it (a rebase might be
/// worth waiting out; a conflicted cherry-pick probably needs the operator).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum InProgressKind {
    Merge,
    Rebase,
    CherryPick,
    Revert,
    Bisect,
}

impl std::fmt::Display for InProgressKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            InProgressKind::Merge => "merge",
            InProgressKind::Rebase => "rebase",
            InProgressKind::CherryPick => "cherry-pick",
            InProgressKind::Revert => "revert",
            InProgressKind::Bisect => "bisect",
        })
    }
}

/// What was dirty, at path granularity. Paths are repository-relative and
/// sorted, so two summaries of the same state compare equal.
///
/// `ignored` is a COUNT, not a list: ignored files are deliberately excluded
/// from a snapshot, and listing them invites an author to think they were
/// captured. The count exists so a surprised author can tell the exclusion
/// happened at all.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DirtySummary {
    pub staged: Vec<String>,
    pub unstaged: Vec<String>,
    pub untracked: Vec<String>,
    pub ignored_excluded: usize,
}

impl DirtySummary {
    pub fn is_clean(&self) -> bool {
        self.staged.is_empty() && self.unstaged.is_empty() && self.untracked.is_empty()
    }
}

impl std::fmt::Display for DirtySummary {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{} staged, {} unstaged, {} untracked",
            self.staged.len(),
            self.unstaged.len(),
            self.untracked.len()
        )
    }
}

/// A failed git invocation, recorded verbatim.
///
/// Per the VERIFY discipline this wave runs under, downstream receipts count
/// per-binary outcomes and never treat an exit code as a summary — so this
/// keeps stdout AND stderr, not just the status.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct GitFailureReceipt {
    pub args: Vec<String>,
    pub cwd: PathBuf,
    /// `None` when the process was killed by a signal before exiting.
    pub exit_code: Option<i32>,
    pub stdout: String,
    pub stderr: String,
}

impl std::fmt::Display for GitFailureReceipt {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "git {} (in {}) exited {}: {}",
            self.args.join(" "),
            self.cwd.display(),
            match self.exit_code {
                Some(c) => c.to_string(),
                None => "by signal".to_string(),
            },
            self.stderr.trim()
        )
    }
}
