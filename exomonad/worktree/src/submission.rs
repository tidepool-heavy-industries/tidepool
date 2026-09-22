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
    /// Repository-relative paths changed by commits after `base_head`.
    pub committed_paths: Vec<String>,
    pub submitted_head: HeadState,
    pub working_state: WorkingState,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct RepositorySample {
    submitted_head: HeadState,
    committed_paths: Vec<String>,
    working_state: WorkingState,
}

fn sample(git: &GitCli, cwd: &Path, base: &GitOid) -> Result<RepositorySample, WorktreeError> {
    let oid = GitOid::from_raw(git.try_run(cwd, &["rev-parse", "HEAD"])?.trimmed());
    let committed_paths = git
        .try_run(
            cwd,
            &[
                "diff",
                "--name-only",
                "-z",
                &format!("{}..{}", base.as_str(), oid.as_str()),
            ],
        )?
        .nul_fields()
        .into_iter()
        .map(String::from)
        .collect();
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
        committed_paths,
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
    if !git.try_exists(&handle.cwd().join(".git"))? {
        return Err(WorktreeError::WorktreeLost(handle.id().clone()));
    }

    let stable = stable_sample(handle.id(), || {
        sample(git, handle.cwd(), handle.source_head())
    })?;
    Ok(SubmissionObservation {
        worktree_id: handle.id().clone(),
        base_head: handle.source_head().clone(),
        committed_paths: stable.committed_paths,
        submitted_head: stable.submitted_head,
        working_state: stable.working_state,
    })
}

fn stable_sample(
    worktree: &WorktreeId,
    mut next: impl FnMut() -> Result<RepositorySample, WorktreeError>,
) -> Result<RepositorySample, WorktreeError> {
    let mut previous = next()?;
    for _ in 0..MAX_STABILITY_ATTEMPTS {
        let current = next()?;
        if current == previous {
            return Ok(current);
        }
        previous = current;
    }
    Err(WorktreeError::SubmissionUnstable(worktree.clone()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_at(oid: &str) -> RepositorySample {
        RepositorySample {
            submitted_head: HeadState::Detached {
                oid: GitOid::from_raw(oid),
            },
            committed_paths: Vec::new(),
            working_state: WorkingState {
                changes: DirtySummary::default(),
                operation: None,
            },
        }
    }

    #[test]
    fn bounded_sampling_refuses_a_checkout_that_never_stabilizes() {
        let worktree = WorktreeId::from_raw("moving");
        let mut generation = 0;
        let result = stable_sample(&worktree, || {
            generation += 1;
            Ok(sample_at(&format!("oid-{generation}")))
        });
        assert_eq!(result, Err(WorktreeError::SubmissionUnstable(worktree)));
        assert_eq!(generation, MAX_STABILITY_ATTEMPTS + 1);
    }

    #[test]
    fn bounded_sampling_returns_the_first_consecutive_complete_match() {
        let worktree = WorktreeId::from_raw("settling");
        let mut samples = [sample_at("a"), sample_at("b"), sample_at("b")].into_iter();
        assert_eq!(
            stable_sample(&worktree, || Ok(samples.next().unwrap())),
            Ok(sample_at("b"))
        );
    }
}
