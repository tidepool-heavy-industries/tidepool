//! A single typed merge primitive for PRD 21 C5's worktree-coordination fold
//! (`plans/self-iterating-harness/21-recursive-companion-prd.md`, "Worktree
//! coordination"): a node merges each child's branch into its own worktree,
//! in declared branch order.
//!
//! This is NOT a reopening of the "no git workflow verbs" boundary
//! (`crate::git`'s module docs, this crate's `CLAUDE.md`, and
//! `Tidepool.Worktree`'s own docstring all say the same thing: rebase, merge,
//! cherry-pick, and conflict resolution belong to coding agents with their
//! native tools, and Tidepool only observes what the repository became). It
//! is one narrowly-typed primitive for the coordination fold specifically,
//! and it is deliberately NOT exposed as a new `Worktree` effect verb —
//! `Worktree`'s decl is generated from `tidepool-protocol`, a schema this
//! crate does not own, and widening the Haskell-facing effect surface with a
//! general merge verb is exactly the "workflow verb" regression the
//! boundary refuses. The authored Haskell side
//! (`harness-dogfooding/recursive-companion/Harness.hs`) reaches the same
//! semantics through `Exec`, mirroring `harness-dogfooding/dev-tree
//! /Harness.hs`'s own `mergeChild` — mechanical git run as authored policy in
//! a worktree the node owns, never a runtime-exposed workflow verb. This
//! module exists so that semantics is defined ONCE, typed, and pinned by a
//! fast-tier test against a real repository, rather than re-derived ad hoc
//! at each authored call site.
//!
//! Every outcome is DATA. A conflict never leaves the target worktree
//! mid-merge: [`merge_branch_into`] runs `git merge --abort` before
//! returning [`MergeOutcome::Conflict`], so the caller always finds a clean
//! tree either way. A merge failure that is not a real conflict (an unknown
//! branch, for instance) surfaces as `Err(WorktreeError::GitFailure(_))`,
//! the crate's ordinary git-failure shape — never a panic, never a
//! half-merged tree.

use std::path::Path;

use crate::error::{InProgressKind, WorktreeError};
use crate::git::{inspect, GitCli};
use crate::id::{BranchName, GitOid};

/// The result of merging one branch into a target worktree. A `git`
/// invocation failure that is NOT this — an unknown branch, a locked index,
/// anything that never entered a merge at all — is `Err(WorktreeError::
/// GitFailure(_))` instead; this type only ever describes a merge that
/// actually started.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MergeOutcome {
    /// The merge landed a new commit. `commit` is the target's `HEAD` after
    /// the merge — a real merge commit (`--no-ff` is always passed, so a
    /// fast-forward never silently loses the "this was a merge" fact).
    Merged { commit: GitOid },
    /// The merge conflicted. `paths` are the repository-relative paths git
    /// reported unmerged (`diff --name-only --diff-filter=U`), read BEFORE
    /// the abort. The target worktree is guaranteed clean by the time this
    /// is returned — no `MERGE_HEAD`, no partial index state — because the
    /// abort runs before this outcome is constructed, never after.
    Conflict { paths: Vec<String> },
}

/// Merge `branch` into the worktree at `target_cwd`, as one `git merge
/// --no-ff` — never a fast-forward, so the result always carries a genuine
/// merge commit when it lands one, which is what lets a caller build a
/// legible bottom-up history rather than a silently-rebased one.
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
pub fn merge_branch_into(
    git: &GitCli,
    target_cwd: &Path,
    branch: &BranchName,
    message: &str,
) -> Result<MergeOutcome, WorktreeError> {
    let receipt = match git.run(
        target_cwd,
        &["merge", "--no-ff", "-m", message, branch.as_str()],
    ) {
        Ok(_) => {
            let out = git.try_run(target_cwd, &["rev-parse", "HEAD"])?;
            return Ok(MergeOutcome::Merged {
                commit: GitOid::from_raw(out.trimmed()),
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

    Ok(MergeOutcome::Conflict { paths })
}
