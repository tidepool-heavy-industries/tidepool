//! Fast-tier acceptance for `merge::merge_branch_into` — the typed
//! worktree-coordination merge primitive. Real temporary repositories, real
//! `git worktree add`, no mock of git.

use tidepool_worktree::git::inspect;
use tidepool_worktree::merge::{merge_branch_into, MergeOutcome};
use tidepool_worktree::testing::TestRepo;
use tidepool_worktree::{BranchName, WorktreeError};

/// Add a real linked worktree at `label`, on a fresh branch off `base`.
fn add_worktree(repo: &TestRepo, branch: &str, base: &str) -> std::path::PathBuf {
    let path = repo.path().parent().unwrap().join(format!(
        "{}-{}",
        repo.path().file_name().unwrap().to_string_lossy(),
        branch
    ));
    repo.git()
        .try_run(
            repo.path(),
            &[
                "worktree",
                "add",
                "-q",
                "-b",
                branch,
                path.to_str().unwrap(),
                base,
            ],
        )
        .expect("git worktree add");
    path
}

#[test]
fn clean_merge_lands_a_merge_commit() {
    let repo = TestRepo::init().expect("init repo");
    repo.writer()
        .commit_file("README.md", "base\n", "base commit")
        .expect("base commit");

    let child_path = add_worktree(&repo, "child", "main");
    repo.writer_at(&child_path)
        .commit_file("child.txt", "child content\n", "child work")
        .expect("child commit");

    let node_path = add_worktree(&repo, "node", "main");

    let outcome = merge_branch_into(
        repo.git(),
        &node_path,
        &BranchName::from_raw("child"),
        "fold child into node",
    )
    .expect("merge should run");

    match outcome {
        MergeOutcome::Merged { commit } => {
            assert!(!commit.as_str().is_empty(), "a real commit oid came back");
        }
        MergeOutcome::Conflict { paths } => {
            panic!("expected a clean merge, got conflict: {paths:?}")
        }
    }

    assert!(
        node_path.join("child.txt").exists(),
        "the child's file is now present in the node's worktree"
    );
    // A real merge commit, never a fast-forward: two parents.
    let parents = repo
        .git()
        .try_run(&node_path, &["rev-list", "--parents", "-n", "1", "HEAD"])
        .expect("rev-list HEAD");
    assert_eq!(
        parents.trimmed().split(' ').count(),
        3,
        "HEAD plus two parents: {}",
        parents.trimmed()
    );
}

#[test]
fn conflicting_merge_reports_paths_and_restores_clean_state() {
    let repo = TestRepo::init().expect("init repo");
    repo.writer()
        .commit_file("shared.txt", "base\n", "base commit")
        .expect("base commit");

    let child_path = add_worktree(&repo, "child", "main");
    repo.writer_at(&child_path)
        .commit_file("shared.txt", "child version\n", "child edits shared")
        .expect("child commit");

    let node_path = add_worktree(&repo, "node", "main");
    repo.writer_at(&node_path)
        .commit_file("shared.txt", "node version\n", "node edits shared")
        .expect("node commit");

    let outcome = merge_branch_into(
        repo.git(),
        &node_path,
        &BranchName::from_raw("child"),
        "fold child into node",
    )
    .expect("merge should run (a conflict is a typed outcome, not an Err)");

    match outcome {
        MergeOutcome::Conflict { paths } => {
            assert_eq!(paths, vec!["shared.txt".to_string()]);
        }
        MergeOutcome::Merged { commit } => {
            panic!("expected a conflict, got a clean merge: {commit:?}")
        }
    }

    // Clean state restored: no in-progress merge, no dirty index, and the
    // node's own pre-merge content is exactly what it was before.
    let in_progress = inspect::in_progress(repo.git(), &node_path).expect("in_progress read");
    assert_eq!(
        in_progress, None,
        "the abort must leave no MERGE_HEAD behind"
    );
    let dirty = inspect::dirty_summary(repo.git(), &node_path).expect("dirty_summary");
    assert!(
        dirty.is_clean(),
        "the worktree must be clean after an aborted merge: {dirty:?}"
    );
    let content = std::fs::read_to_string(node_path.join("shared.txt")).expect("read shared.txt");
    assert_eq!(
        content, "node version\n",
        "the node's own content is untouched by the aborted merge"
    );
}

#[test]
fn merging_an_unknown_branch_is_a_typed_git_failure_not_a_conflict() {
    let repo = TestRepo::init().expect("init repo");
    repo.writer()
        .commit_file("README.md", "base\n", "base commit")
        .expect("base commit");
    let node_path = add_worktree(&repo, "node", "main");

    let err = merge_branch_into(
        repo.git(),
        &node_path,
        &BranchName::from_raw("does-not-exist"),
        "fold nothing into node",
    )
    .expect_err("an unknown branch never enters a merge to abort");

    match err {
        WorktreeError::GitFailure(receipt) => {
            assert!(receipt.args.iter().any(|a| a == "merge"));
        }
        other => panic!("expected GitFailure, got {other:?}"),
    }
    // Never entered a merge, so nothing to abort and nothing to clean up.
    let in_progress = inspect::in_progress(repo.git(), &node_path).expect("in_progress read");
    assert_eq!(in_progress, None);
}
