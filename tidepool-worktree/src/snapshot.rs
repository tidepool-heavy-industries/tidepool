//! Dirty-source snapshots.
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

use std::path::{Path, PathBuf};

use crate::create::TIDEPOOL_SNAPSHOT_REF_PREFIX;
use crate::error::{DirtySummary, WorktreeError};
use crate::git::{inspect, GitCli};
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
    // Refuse first — before a single write happens. A refusal that happens
    // after a partial write is not a refusal.
    if let Some(kind) = inspect::in_progress(git, source)? {
        return Err(WorktreeError::SourceOperationInProgress(kind));
    }
    let changed_clean_submodules = refuse_dirty_submodules(git, source)?;

    let pre_status = inspect::dirty_summary(git, source)?;
    let source_head = GitOid::from_raw(
        git.try_run(source, &["rev-parse", "HEAD"])?
            .trimmed()
            .to_string(),
    );

    let mut captured_paths: Vec<String> = pre_status
        .staged
        .iter()
        .cloned()
        .chain(pre_status.unstaged.iter().cloned())
        .chain(pre_status.untracked.iter().cloned())
        .chain(changed_clean_submodules)
        .collect();
    captured_paths.sort();
    captured_paths.dedup();

    // Callers hand this dir in as a path, not a promise — the manager-owned
    // path (`<worktree_root>/.tidepool-snapshot-index/<id>`) does not exist on
    // a fresh root, and git creates the index FILE but never its parent dirs.
    std::fs::create_dir_all(temp_index_dir).map_err(|e| WorktreeError::StorageFailure {
        path: temp_index_dir.to_path_buf(),
        detail: e.to_string(),
    })?;
    let temp_index_path = temp_index_dir.join(format!("{}.index", worktree_id.as_str()));
    // `GIT_INDEX_FILE` routes git's own read/write to this path — a lossy
    // mangle here would silently point git at the wrong file. Decode once,
    // typed, rather than handing git text that doesn't name the file we just
    // created.
    let temp_index_path_utf8 = camino::Utf8Path::from_path(&temp_index_path).ok_or_else(|| {
        WorktreeError::StorageFailure {
            path: temp_index_path.clone(),
            detail: "temp index path is not valid UTF-8".to_string(),
        }
    })?;
    let temp_git = git.with_env("GIT_INDEX_FILE", temp_index_path_utf8.as_str());

    temp_git.try_run(source, &["read-tree", "HEAD"])?;
    if !captured_paths.is_empty() {
        let mut add_args: Vec<String> = vec!["add".to_string(), "--".to_string()];
        add_args.extend(captured_paths.iter().cloned());
        temp_git.try_run(source, &add_args)?;
    }
    let tree = temp_git
        .try_run(source, &["write-tree"])?
        .trimmed()
        .to_string();
    let _ = std::fs::remove_file(&temp_index_path);

    let message = format!(
        "tidepool: dirty-source snapshot for worktree {worktree_id}\n\nSource HEAD: {source_head}\n{pre_status}"
    );
    let snapshot_commit = GitOid::from_raw(
        git.try_run(
            source,
            &[
                "commit-tree",
                &tree,
                "-p",
                source_head.as_str(),
                "-m",
                &message,
            ],
        )?
        .trimmed()
        .to_string(),
    );

    let snapshot_ref = GitRef::from_raw(format!("{TIDEPOOL_SNAPSHOT_REF_PREFIX}/{worktree_id}"));
    git.try_run(
        source,
        &[
            "update-ref",
            snapshot_ref.as_str(),
            snapshot_commit.as_str(),
        ],
    )?;

    Ok(SnapshotReceipt {
        snapshot_ref,
        snapshot_commit,
        source_head,
        captured_paths,
        pre_status,
    })
}

/// Refuse if any submodule has uncommitted changes of its own (staged,
/// unstaged, or a merge conflict inside it) — a synthetic commit would then
/// point at a gitlink whose content the snapshot never captured. Returns the
/// paths of CLEAN submodules whose checked-out commit differs from the one
/// recorded in `source`'s HEAD, so the caller can capture the gitlink bump
/// explicitly (this is a legitimate tracked change, not dirtiness).
///
/// `git submodule status` exits 0 with empty output when there are no
/// submodules at all, so this is a no-op on the common case.
fn refuse_dirty_submodules(git: &GitCli, source: &Path) -> Result<Vec<String>, WorktreeError> {
    // A FAILED `git submodule status` is never the no-submodules case (that
    // exits 0 with empty output, per above) — swallowing it here would let a
    // snapshot proceed past the very check that guards it. Fail loud.
    let out = git.try_run(source, &["submodule", "status"])?;

    let mut changed_clean = Vec::new();
    for line in out.lines() {
        if line.is_empty() {
            continue;
        }
        let mut chars = line.chars();
        let status = chars.next().unwrap_or(' ');
        let rest = &line[status.len_utf8()..];
        let path = match rest.split_whitespace().nth(1) {
            Some(p) => p.to_string(),
            None => continue,
        };

        match status {
            // Not initialized: no working tree to be dirty, and no gitlink
            // change to capture either.
            '-' => continue,
            // Merge conflict inside the submodule itself.
            'U' => {
                return Err(WorktreeError::DirtySubmoduleUnsupported(PathBuf::from(
                    path,
                )))
            }
            '+' | ' ' => {
                let sub_path = source.join(&path);
                let sub_status = git
                    .run(&sub_path, &["status", "--porcelain"])
                    .map_err(WorktreeError::GitFailure)?;
                if !sub_status.stdout.trim().is_empty() {
                    return Err(WorktreeError::DirtySubmoduleUnsupported(PathBuf::from(
                        path,
                    )));
                }
                if status == '+' {
                    changed_clean.push(path);
                }
            }
            _ => {}
        }
    }
    Ok(changed_clean)
}
