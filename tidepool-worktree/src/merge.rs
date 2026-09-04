//! A single typed merge primitive for the worktree-coordination fold: a node
//! merges each child's branch into its own worktree, in declared branch
//! order.
//!
//! This is NOT a reopening of the "no git workflow verbs" boundary
//! (`crate::git`'s module docs, this crate's `CLAUDE.md`, and
//! `Tidepool.Worktree`'s own docstring all say the same thing: rebase,
//! cherry-pick, and conflict RESOLUTION belong to coding agents with their
//! native tools, and Tidepool only observes what the repository became). It
//! is one narrowly-typed primitive for the coordination fold specifically.
//! It IS exposed as a `Worktree` effect verb (`WorktreeTryMerge` /
//! `tryMerge`, generated from `tidepool-protocol`'s schema) — the
//! consolidation is deliberate, not a widening of the boundary: two authored
//! Haskell reimplementations of exactly this primitive
//! (`harness-dogfooding/dev-tree/Harness.hs`'s `mergeChild` and
//! `harness-dogfooding/recursive-companion/Harness.hs`'s `mergeChildInto`)
//! had drifted from this crate's own conflict-vs-failure classification —
//! every nonzero exit read as a conflict, a failed `merge --abort` silently
//! ignored — so the fix is exposing the one ground truth, not re-deriving it
//! a third time. The boundary this module does NOT reopen is a GENERAL git
//! workflow surface: there is still no `rebase`, `cherryPick`, or conflict
//! resolution verb, and resolving a reported conflict stays authored policy.
//! This module exists so that semantics is defined ONCE, typed, and pinned by
//! a fast-tier test against a real repository, rather than re-derived ad hoc
//! at each authored call site.
//!
//! Every outcome is DATA. A conflict never leaves the target worktree
//! mid-merge: [`try_merge`] runs `git merge --abort` before returning
//! [`MergeOutcome::ManualGitRequired`], so the caller always finds a clean
//! tree either way. A merge failure that is not a real conflict (an unknown
//! branch, for instance) surfaces as `Err(WorktreeError::GitFailure(_))`,
//! the crate's ordinary git-failure shape — never a panic, never a
//! half-merged tree.

use std::path::Path;

use crate::error::{InProgressKind, WorktreeError};
use crate::git::{inspect, GitCli};
use crate::id::{BranchName, GitOid};

/// The result of merging one exact commit into a target worktree. A `git`
/// invocation failure that is NOT this — an unknown branch, a locked index,
/// anything that never entered a merge at all — is `Err(WorktreeError::
/// GitFailure(_))` instead; this type only ever describes a merge that
/// actually started.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MergeOutcome {
    AlreadyContained {
        source: GitOid,
        target: GitOid,
    },
    FastForwarded {
        source: GitOid,
        before: GitOid,
        after: GitOid,
    },
    CreatedMergeCommit {
        source: GitOid,
        before: GitOid,
        commit: GitOid,
    },
    /// The conservative operation could not finish automatically. Any merge
    /// that started has been aborted; `target` is both the starting and final
    /// target HEAD when this value is returned.
    ManualGitRequired {
        source: GitOid,
        target: GitOid,
        reason: String,
        paths: Vec<String>,
    },
}

/// Merge `source` into the worktree at `target_cwd`. A direct descendant
/// fast-forwards; divergent histories use one explicit merge commit.
///
/// On conflict, `git diff --name-only --diff-filter=U` is read and THEN
/// `git merge --abort` runs — abort-before-return is the whole of "never a
/// half-merged tree": every caller of this function, success or conflict,
/// finds `target_cwd` in a clean, non-merging state.
///
/// A `git merge` failure that leaves no `MERGE_HEAD` behind (an unknown
/// branch name, for instance) is not a conflict at all — it never started a
/// merge to abort — and surfaces as `Err(WorktreeError::GitFailure(_))`
/// carrying the full invocation receipt.
pub fn try_merge(
    git: &GitCli,
    target_cwd: &Path,
    source: &GitOid,
    source_branch: Option<&BranchName>,
    message: &str,
) -> Result<MergeOutcome, WorktreeError> {
    if let Some(kind) = inspect::in_progress(git, target_cwd)? {
        return Err(WorktreeError::SourceOperationInProgress(kind));
    }
    let dirty = inspect::dirty_summary(git, target_cwd)?;
    if !dirty.is_clean() {
        return Err(WorktreeError::SourceDirty(dirty));
    }
    let target = GitOid::from_raw(git.try_run(target_cwd, &["rev-parse", "HEAD"])?.trimmed());

    git.try_run(
        target_cwd,
        &["cat-file", "-e", &format!("{}^{{commit}}", source.as_str())],
    )?;
    if let Some(branch) = source_branch {
        let observed = GitOid::from_raw(
            git.try_run(target_cwd, &["rev-parse", branch.as_str()])?
                .trimmed(),
        );
        if &observed != source {
            return Ok(MergeOutcome::ManualGitRequired {
                source: source.clone(),
                target,
                reason: format!(
                    "source branch `{}` moved to {}; expected {}",
                    branch.as_str(),
                    observed.as_str(),
                    source.as_str()
                ),
                paths: Vec::new(),
            });
        }
    }

    if is_ancestor(git, target_cwd, source, &target)? {
        return Ok(MergeOutcome::AlreadyContained {
            source: source.clone(),
            target,
        });
    }
    if is_ancestor(git, target_cwd, &target, source)? {
        git.try_run(target_cwd, &["merge", "--ff-only", source.as_str()])?;
        let after = head(git, target_cwd)?;
        return Ok(MergeOutcome::FastForwarded {
            source: source.clone(),
            before: target,
            after,
        });
    }

    let receipt = match git.run(
        target_cwd,
        &["merge", "--no-ff", "-m", message, source.as_str()],
    ) {
        Ok(_) => {
            return Ok(MergeOutcome::CreatedMergeCommit {
                source: source.clone(),
                before: target,
                commit: head(git, target_cwd)?,
            });
        }
        Err(receipt) => receipt,
    };

    // Only a genuine mid-merge state (a real `MERGE_HEAD`) is a conflict.
    // Anything else — a bad branch name, a locked index — never entered a
    // merge in the first place, so there is nothing to abort and the
    // failure is reported as the ordinary git failure it is.
    if inspect::in_progress(git, target_cwd)? != Some(InProgressKind::Merge) {
        return Err(WorktreeError::GitFailure(receipt));
    }

    let paths = git
        .try_run(
            target_cwd,
            &["diff", "--name-only", "--diff-filter=U", "-z"],
        )
        .map(|out| out.nul_fields().into_iter().map(String::from).collect())
        .unwrap_or_default();

    // Abort unconditionally, even though the conflict-path read above could
    // have failed — a caller must never be handed a mid-merge worktree, and
    // an empty `paths` on an abort failure is still an honest "we could not
    // enumerate them", not a claim that nothing conflicted.
    git.try_run(target_cwd, &["merge", "--abort"])?;

    let restored = head(git, target_cwd)?;
    if restored != target || inspect::in_progress(git, target_cwd)?.is_some() {
        return Err(WorktreeError::GitFailure(receipt));
    }

    Ok(MergeOutcome::ManualGitRequired {
        source: source.clone(),
        target,
        reason: "merge conflict; target was restored to its starting state".into(),
        paths,
    })
}

fn head(git: &GitCli, cwd: &Path) -> Result<GitOid, WorktreeError> {
    Ok(GitOid::from_raw(
        git.try_run(cwd, &["rev-parse", "HEAD"])?.trimmed(),
    ))
}

fn is_ancestor(
    git: &GitCli,
    cwd: &Path,
    possible_ancestor: &GitOid,
    descendant: &GitOid,
) -> Result<bool, WorktreeError> {
    match git.run(
        cwd,
        &[
            "merge-base",
            "--is-ancestor",
            possible_ancestor.as_str(),
            descendant.as_str(),
        ],
    ) {
        Ok(_) => Ok(true),
        Err(receipt) if receipt.exit_code == Some(1) => Ok(false),
        Err(receipt) => Err(WorktreeError::GitFailure(receipt)),
    }
}
