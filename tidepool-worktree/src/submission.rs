//! Truthful, bounded observation of a worker's submitted repository state.
//!
//! A submission is an observation, not a seal: native processes may continue
//! to mutate the checkout after this returns. A clean commit OID is immutable
//! and therefore suitable as an integration artifact; dirty state is useful
//! evidence but is not made durable by observing it.

use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::create::WorktreeHandle;
use crate::error::{DirtySummary, InProgressKind, WorktreeError};
use crate::git::{inspect, GitCli};
use crate::id::{BranchName, GitOid, WorktreeId};

const MAX_STABILITY_ATTEMPTS: usize = 3;

/// The checked-out identity at observation time. Managed worktrees normally
/// remain on their allocated branch, but native Git tools may detach HEAD and
/// the receipt must report that truth rather than manufacture a branch name.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum HeadState {
    OnBranch { branch: BranchName, oid: GitOid },
    Detached { oid: GitOid },
}

/// Mutable repository state observed alongside [`HeadState`].
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkingState {
    pub changes: DirtySummary,
    pub operation: Option<InProgressKind>,
}

/// Repository facts attached to a successful worker submission.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SubmissionObservation {
    pub worktree_id: WorktreeId,
    pub base_head: GitOid,
    pub submitted_head: HeadState,
    pub working_state: WorkingState,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct RepositorySample {
    submitted_head: HeadState,
    working_state: WorkingState,
}

fn sample(git: &GitCli, cwd: &Path) -> Result<RepositorySample, WorktreeError> {
    let oid = GitOid::from_raw(git.try_run(cwd, &["rev-parse", "HEAD"])?.trimmed());
    let branch_output = git.try_run(cwd, &["rev-parse", "--abbrev-ref", "HEAD"])?;
    let branch = branch_output.trimmed();
    let submitted_head = if branch == "HEAD" {
        HeadState::Detached { oid }
    } else {
        HeadState::OnBranch {
            branch: BranchName::from_raw(branch),
            oid,
        }
    };

    Ok(RepositorySample {
        submitted_head,
        working_state: WorkingState {
            changes: inspect::dirty_summary(git, cwd)?,
            operation: inspect::in_progress(git, cwd)?,
        },
    })
}

/// Observe one managed checkout through its owning Git substrate.
///
/// Two consecutive complete samples must agree. This catches HEAD, branch,
/// index, working-tree, untracked-file, ignored-file, and operation-marker
/// movement detected while the result is assembled. The retry count is fixed
/// so an actively changing checkout cannot park an actor indefinitely.
pub(crate) fn observe(
    git: &GitCli,
    handle: &WorktreeHandle,
) -> Result<SubmissionObservation, WorktreeError> {
    if !handle.cwd().join(".git").exists() {
        return Err(WorktreeError::WorktreeLost(handle.id().clone()));
    }

    let mut previous = sample(git, handle.cwd())?;
    for _ in 0..MAX_STABILITY_ATTEMPTS {
        let current = sample(git, handle.cwd())?;
        if current == previous {
            return Ok(SubmissionObservation {
                worktree_id: handle.id().clone(),
                base_head: handle.source_head().clone(),
                submitted_head: current.submitted_head,
                working_state: current.working_state,
            });
        }
        previous = current;
    }

    Err(WorktreeError::SubmissionUnstable(handle.id().clone()))
}
