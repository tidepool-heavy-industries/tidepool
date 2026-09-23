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
//! (`exomonad/harness-dogfooding/dev-tree/Harness.hs`'s `mergeChild` and
//! `exomonad/harness-dogfooding/recursive-companion/Harness.hs`'s `mergeChildInto`)
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
//! [`MergeOutcome::ManualGitRequired`]. A landed merge whose workspace checkout
//! cannot be synchronized also returns that outcome, naming the landed commit.
//! A merge failure that is not a real conflict (an unknown
//! branch, for instance) surfaces as `Err(WorktreeError::GitFailure(_))`,
//! the crate's ordinary git-failure shape — never a panic, never a
//! half-merged tree.

use std::collections::BTreeMap;
use std::path::Path;

use crate::error::{InProgressKind, WorktreeError};
use crate::git::{inspect, GitCli};
use crate::id::{BranchName, GitOid};

const WORKSPACE_PATH: &str = ".exomonad/workspace";

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
    /// The conservative operation could not finish automatically. A conflicted
    /// merge was aborted, so `target` is both the starting and final HEAD.
    /// If the merge landed but workspace synchronization or branch advance
    /// failed, `target` is the landed result. `reason` distinguishes the cases.
    ManualGitRequired {
        source: GitOid,
        target: GitOid,
        reason: String,
        paths: Vec<String>,
    },
}

/// Merge `source` into the worktree at `target_cwd`. A direct descendant
/// fast-forwards; divergent histories use one explicit merge commit.
/// `source_cwd` is the explicitly registered checkout that supplied `source`,
/// not a path inferred from a branch name. If both checkouts have initialized
/// `.exomonad/workspace`, a missing source gitlink commit is fetched from that
/// source workspace repository before Git performs the superproject merge.
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
///
/// `advance` names a branch to move to the merge result once the merge lands —
/// the integration branch a fold publishes to, which is not the branch checked
/// out in `target_cwd`. Its value is read BEFORE the merge and passed to
/// `git update-ref` as the expected old value, so a branch that moved
/// meanwhile fails the compare-and-swap instead of losing the commit that
/// moved it; that refusal, and any other `update-ref` failure, is reported as
/// `ManualGitRequired` over a merge that did happen. The same applies when
/// synchronizing an initialized workspace checkout fails after the merge.
pub fn try_merge(
    git: &GitCli,
    target_cwd: &Path,
    source_cwd: &Path,
    source: &GitOid,
    source_branch: Option<&BranchName>,
    advance: Option<&BranchName>,
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

    // Read before any mutation: this is the expected old value the advance is
    // pinned to, and an advance branch that does not resolve at all is an
    // ordinary git failure over a repository nothing has touched yet.
    let advance_from = advance
        .map(|branch| read_branch(git, target_cwd, branch))
        .transpose()?;
    let advancing = advance.zip(advance_from.as_ref());

    if is_ancestor(git, target_cwd, source, &target)? {
        return Ok(MergeOutcome::AlreadyContained {
            source: source.clone(),
            target,
        });
    }
    fetch_missing_workspace_gitlink(git, target_cwd, source_cwd, source)?;
    if is_ancestor(git, target_cwd, &target, source)? {
        git.try_run(target_cwd, &["merge", "--ff-only", source.as_str()])?;
        let after = head(git, target_cwd)?;
        if let Err(error) = update_initialized_workspace(git, target_cwd, &after) {
            return Ok(workspace_sync_handoff(source, after, error));
        }
        if let Err(reason) = advance_branch(git, target_cwd, advancing, &after) {
            return Ok(MergeOutcome::ManualGitRequired {
                source: source.clone(),
                target: after,
                reason,
                paths: Vec::new(),
            });
        }
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
            let commit = head(git, target_cwd)?;
            if let Err(error) = update_initialized_workspace(git, target_cwd, &commit) {
                return Ok(workspace_sync_handoff(source, commit, error));
            }
            if let Err(reason) = advance_branch(git, target_cwd, advancing, &commit) {
                return Ok(MergeOutcome::ManualGitRequired {
                    source: source.clone(),
                    target: commit,
                    reason,
                    paths: Vec::new(),
                });
            }
            return Ok(MergeOutcome::CreatedMergeCommit {
                source: source.clone(),
                before: target,
                commit,
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

    let paths: Vec<String> = git
        .try_run(
            target_cwd,
            &["diff", "--name-only", "--diff-filter=U", "-z"],
        )
        .map(|out| out.nul_fields().into_iter().map(String::from).collect())
        .unwrap_or_default();
    let gitlink_report = gitlink_conflict_report(git, target_cwd, &paths);

    // Abort unconditionally, even though the conflict-path read above could
    // have failed — a caller must never be handed a mid-merge worktree, and
    // an empty `paths` on an abort failure is still an honest "we could not
    // enumerate them", not a claim that nothing conflicted.
    git.try_run(target_cwd, &["merge", "--abort"])?;

    let restored = head(git, target_cwd)?;
    if restored != target || inspect::in_progress(git, target_cwd)?.is_some() {
        return Err(WorktreeError::GitFailure(receipt));
    }

    let reason = match gitlink_report {
        Some(report) => format!(
            "merge conflict contains only gitlinks; merge the workspace commits inside the workspace and bump the gitlink once ({report}); target was restored to its starting state"
        ),
        None => "merge conflict; target was restored to its starting state".into(),
    };

    Ok(MergeOutcome::ManualGitRequired {
        source: source.clone(),
        target,
        reason,
        paths,
    })
}

/// Make a source commit's workspace gitlink object available to the target
/// workspace repository when both checkouts have initialized
/// `.exomonad/workspace`. This transfers objects only; it never checks out,
/// merges, or changes either nested worktree. Missing/uninitialized workspace
/// checkouts keep ordinary Git's existing behavior.
fn fetch_missing_workspace_gitlink(
    git: &GitCli,
    target_cwd: &Path,
    source_cwd: &Path,
    source: &GitOid,
) -> Result<(), WorktreeError> {
    let Some(workspace_commit) = tree_gitlink_oid(git, target_cwd, source, WORKSPACE_PATH)? else {
        return Ok(());
    };
    let target_workspace = target_cwd.join(WORKSPACE_PATH);
    let source_workspace = source_cwd.join(WORKSPACE_PATH);
    if !git.try_exists(&target_workspace.join(".git"))?
        || !git.try_exists(&source_workspace.join(".git"))?
    {
        return Ok(());
    }

    let object = format!("{workspace_commit}^{{commit}}");
    if git
        .try_run(&target_workspace, &["cat-file", "-e", &object])
        .is_ok()
    {
        return Ok(());
    }

    let Some(source_workspace) = source_workspace.to_str() else {
        return Ok(());
    };
    git.try_run(
        &target_workspace,
        &["fetch", "--no-tags", source_workspace, &workspace_commit],
    )?;
    Ok(())
}

/// Keep an already initialized checkout aligned with the gitlink that landed.
/// Git refuses this checkout if it would overwrite local workspace changes.
fn update_initialized_workspace(
    git: &GitCli,
    target_cwd: &Path,
    merged: &GitOid,
) -> Result<(), WorktreeError> {
    let Some(workspace_commit) = tree_gitlink_oid(git, target_cwd, merged, WORKSPACE_PATH)? else {
        return Ok(());
    };
    let workspace = target_cwd.join(WORKSPACE_PATH);
    if !git.try_exists(&workspace.join(".git"))? {
        return Ok(());
    }
    let current = git.try_run(&workspace, &["rev-parse", "HEAD"])?;
    if current.trimmed() != workspace_commit {
        git.try_run(&workspace, &["checkout", "--detach", &workspace_commit])?;
    }
    Ok(())
}

fn workspace_sync_handoff(source: &GitOid, landed: GitOid, error: WorktreeError) -> MergeOutcome {
    MergeOutcome::ManualGitRequired {
        source: source.clone(),
        reason: format!(
            "the merge landed {} in the target worktree, but its initialized workspace checkout could not be synchronized with the merged gitlink: {error}",
            landed.as_str()
        ),
        target: landed,
        paths: Vec::new(),
    }
}

fn tree_gitlink_oid(
    git: &GitCli,
    cwd: &Path,
    commit: &GitOid,
    path: &str,
) -> Result<Option<String>, WorktreeError> {
    let output = git.try_run(cwd, &["ls-tree", "-z", commit.as_str(), "--", path])?;
    let Some(record) = output.nul_fields().into_iter().next() else {
        return Ok(None);
    };
    let Some((metadata, listed_path)) = record.split_once('\t') else {
        return Ok(None);
    };
    if listed_path != path {
        return Ok(None);
    }
    let mut fields = metadata.split_ascii_whitespace();
    let mode = fields.next();
    let kind = fields.next();
    let oid = fields.next();
    if mode != Some("160000") || kind != Some("commit") || fields.next().is_some() {
        return Ok(None);
    }
    Ok(oid.map(str::to_string))
}

/// Return an actionable report only when every conflicted path is a gitlink
/// and both sides name a commit. This is deliberately report-only: resolving
/// the nested repository conflict remains authored integration policy.
fn gitlink_conflict_report(git: &GitCli, cwd: &Path, paths: &[String]) -> Option<String> {
    if paths.is_empty() {
        return None;
    }

    // `ls-files -u -z` records are `mode oid stage<TAB>path<NUL>`. The index
    // is the authoritative source for both competing gitlink commits; the
    // conflicted worktree path itself may not contain a checked-out submodule.
    let output = git.try_run(cwd, &["ls-files", "-u", "-z"]).ok()?;
    let mut entries: BTreeMap<String, Vec<(String, String, u8)>> = BTreeMap::new();
    for record in output.nul_fields() {
        let (metadata, path) = record.split_once('\t')?;
        let mut fields = metadata.split_ascii_whitespace();
        let mode = fields.next()?.to_string();
        let oid = fields.next()?.to_string();
        let stage = fields.next()?.parse().ok()?;
        if fields.next().is_some() {
            return None;
        }
        entries
            .entry(path.to_string())
            .or_default()
            .push((mode, oid, stage));
    }

    if entries.len() != paths.len() || paths.iter().any(|path| !entries.contains_key(path)) {
        return None;
    }

    let mut reports = Vec::with_capacity(paths.len());
    for path in paths {
        let records = entries.get(path)?;
        if records.iter().any(|(mode, _, _)| mode != "160000") {
            return None;
        }
        let ours = records.iter().find(|(_, _, stage)| *stage == 2)?.1.as_str();
        let theirs = records.iter().find(|(_, _, stage)| *stage == 3)?.1.as_str();
        reports.push(format!("{path} (target {ours}, source {theirs})"));
    }

    Some(reports.join(", "))
}

fn branch_ref(branch: &BranchName) -> String {
    format!("refs/heads/{}", branch.as_str())
}

fn read_branch(git: &GitCli, cwd: &Path, branch: &BranchName) -> Result<GitOid, WorktreeError> {
    let reference = branch_ref(branch);
    Ok(GitOid::from_raw(
        git.try_run(cwd, &["rev-parse", "--verify", reference.as_str()])?
            .trimmed(),
    ))
}

/// Move `branch` from the value read before the merge to `result`, or describe
/// why the caller has to finish by hand. The expected old value is the whole
/// check: a branch someone else advanced during the merge fails the
/// compare-and-swap rather than losing their commit.
fn advance_branch(
    git: &GitCli,
    cwd: &Path,
    advancing: Option<(&BranchName, &GitOid)>,
    result: &GitOid,
) -> Result<(), String> {
    let Some((branch, before)) = advancing else {
        return Ok(());
    };
    let reference = branch_ref(branch);
    match git.run(
        cwd,
        &[
            "update-ref",
            reference.as_str(),
            result.as_str(),
            before.as_str(),
        ],
    ) {
        Ok(_) => Ok(()),
        Err(receipt) => Err(format!(
            "the merge landed {} in the target worktree, but branch `{}` was not advanced from \
             {}: it moved, or its ref could not be written ({})",
            result.as_str(),
            branch.as_str(),
            before.as_str(),
            receipt.stderr.trim()
        )),
    }
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
