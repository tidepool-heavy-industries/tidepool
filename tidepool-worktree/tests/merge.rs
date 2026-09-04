//! Fast-tier acceptance for `merge::merge_branch_into` — the typed
//! worktree-coordination merge primitive. Real temporary repositories, real
//! `git worktree add`, no mock of git.

use tidepool_worktree::git::inspect;
use tidepool_worktree::merge::{try_merge, MergeOutcome};
use tidepool_worktree::testing::TestRepo;
use tidepool_worktree::{BranchName, GitOid, WorktreeError};

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
fn divergent_merge_lands_a_merge_commit() {
    let repo = TestRepo::init().expect("init repo");
    repo.writer()
        .commit_file("README.md", "base\n", "base commit")
        .expect("base commit");

    let child_path = add_worktree(&repo, "child", "main");
    repo.writer_at(&child_path)
        .commit_file("child.txt", "child content\n", "child work")
        .expect("child commit");

    let node_path = add_worktree(&repo, "node", "main");
    repo.writer_at(&node_path)
        .commit_file("node.txt", "node content\n", "node work")
        .expect("node commit");
    let source = GitOid::from_raw(
        repo.git()
            .try_run(&child_path, &["rev-parse", "HEAD"])
            .expect("child head")
            .trimmed(),
    );

    let outcome = try_merge(
        repo.git(),
        &node_path,
        &source,
        Some(&BranchName::from_raw("child")),
        "fold child into node",
    )
    .expect("merge should run");

    match outcome {
        MergeOutcome::CreatedMergeCommit { commit, .. } => {
            assert!(!commit.as_str().is_empty(), "a real commit oid came back");
        }
        other => panic!("expected a merge commit, got {other:?}"),
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
fn direct_descendant_fast_forwards_then_reports_already_contained() {
    let repo = TestRepo::init().expect("init repo");
    repo.writer()
        .commit_file("README.md", "base\n", "base commit")
        .expect("base commit");
    let child_path = add_worktree(&repo, "child", "main");
    repo.writer_at(&child_path)
        .commit_file("child.txt", "child\n", "child work")
        .expect("child commit");
    let source = GitOid::from_raw(
        repo.git()
            .try_run(&child_path, &["rev-parse", "HEAD"])
            .expect("child head")
            .trimmed(),
    );
    let node_path = add_worktree(&repo, "node", "main");

    let first =
        try_merge(repo.git(), &node_path, &source, None, "fold").expect("fast-forward should work");
    assert!(matches!(first, MergeOutcome::FastForwarded { ref after, .. } if after == &source));

    let second = try_merge(repo.git(), &node_path, &source, None, "fold again")
        .expect("already-contained should work");
    assert!(
        matches!(second, MergeOutcome::AlreadyContained { ref target, .. } if target == &source)
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

    let source = GitOid::from_raw(
        repo.git()
            .try_run(&child_path, &["rev-parse", "HEAD"])
            .expect("child head")
            .trimmed(),
    );
    let outcome = try_merge(
        repo.git(),
        &node_path,
        &source,
        Some(&BranchName::from_raw("child")),
        "fold child into node",
    )
    .expect("merge should run (a conflict is a typed outcome, not an Err)");

    match outcome {
        MergeOutcome::ManualGitRequired { paths, .. } => {
            assert_eq!(paths, vec!["shared.txt".to_string()]);
        }
        other => panic!("expected a conflict handoff, got {other:?}"),
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
fn unknown_source_commit_is_a_typed_git_failure() {
    let repo = TestRepo::init().expect("init repo");
    repo.writer()
        .commit_file("README.md", "base\n", "base commit")
        .expect("base commit");
    let node_path = add_worktree(&repo, "node", "main");

    let err = try_merge(
        repo.git(),
        &node_path,
        &GitOid::from_raw("does-not-exist"),
        None,
        "fold nothing into node",
    )
    .expect_err("an unknown branch never enters a merge to abort");

    match err {
        WorktreeError::GitFailure(receipt) => {
            assert!(receipt.args.iter().any(|a| a == "cat-file"));
        }
        other => panic!("expected GitFailure, got {other:?}"),
    }
    // Never entered a merge, so nothing to abort and nothing to clean up.
    let in_progress = inspect::in_progress(repo.git(), &node_path).expect("in_progress read");
    assert_eq!(in_progress, None);
}

#[test]
fn moved_readable_branch_returns_manual_handoff_without_mutation() {
    let repo = TestRepo::init().expect("init repo");
    repo.writer()
        .commit_file("README.md", "base\n", "base commit")
        .expect("base commit");
    let expected = GitOid::from_raw(
        repo.git()
            .try_run(repo.path(), &["rev-parse", "HEAD"])
            .expect("base head")
            .trimmed(),
    );
    let child_path = add_worktree(&repo, "child", "main");
    repo.writer_at(&child_path)
        .commit_file("child.txt", "child\n", "child work")
        .expect("move child branch");
    let node_path = add_worktree(&repo, "node", "main");

    let outcome = try_merge(
        repo.git(),
        &node_path,
        &expected,
        Some(&BranchName::from_raw("child")),
        "must not merge moved branch",
    )
    .expect("moved branch is a typed handoff");

    assert!(
        matches!(outcome, MergeOutcome::ManualGitRequired { ref reason, ref paths, .. }
        if reason.contains("moved") && paths.is_empty())
    );
    assert_eq!(
        repo.git()
            .try_run(&node_path, &["rev-parse", "HEAD"])
            .expect("node head")
            .trimmed(),
        expected.as_str()
    );
}
