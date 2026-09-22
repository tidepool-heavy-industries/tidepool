//! Fast-tier acceptance for `merge::merge_branch_into` — the typed
//! worktree-coordination merge primitive. Real temporary repositories, real
//! `git worktree add`, no mock of git.

use exomonad_worktree::git::inspect;
use exomonad_worktree::merge::{try_merge, MergeOutcome};
use exomonad_worktree::testing::TestRepo;
use exomonad_worktree::{BranchName, GitOid, WorktreeError};

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
        None,
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

    let first = try_merge(repo.git(), &node_path, &source, None, None, "fold")
        .expect("fast-forward should work");
    assert!(matches!(first, MergeOutcome::FastForwarded { ref after, .. } if after == &source));

    let second = try_merge(repo.git(), &node_path, &source, None, None, "fold again")
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
        None,
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
        None,
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

/// The fold publishes to an integration branch nobody has checked out: the
/// merge lands in the target worktree and the named branch moves to the same
/// commit, in one call.
#[test]
fn an_advance_branch_moves_to_the_merge_result() {
    let repo = TestRepo::init().expect("init repo");
    repo.writer()
        .commit_file("README.md", "base\n", "base commit")
        .expect("base commit");
    repo.git()
        .try_run(repo.path(), &["branch", "integration", "main"])
        .expect("integration branch");

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
        Some(&BranchName::from_raw("integration")),
        "fold child into node",
    )
    .expect("merge should run");

    let MergeOutcome::CreatedMergeCommit { commit, .. } = outcome else {
        panic!("expected a merge commit, got {outcome:?}");
    };
    assert_eq!(
        repo.git()
            .try_run(repo.path(), &["rev-parse", "refs/heads/integration"])
            .expect("integration head")
            .trimmed(),
        commit.as_str(),
        "the advance branch now names the merge result"
    );
}

/// An advance the repository refuses — the branch moved under the merge, or
/// the ref could not be written — is a handoff over a merge that DID land, so
/// the reason says so and `target` is the merge result.
#[test]
fn a_refused_advance_is_a_manual_handoff_over_a_landed_merge() {
    let repo = TestRepo::init().expect("init repo");
    repo.writer()
        .commit_file("README.md", "base\n", "base commit")
        .expect("base commit");
    repo.git()
        .try_run(repo.path(), &["branch", "integration", "main"])
        .expect("integration branch");
    let integration_before = repo
        .git()
        .try_run(repo.path(), &["rev-parse", "refs/heads/integration"])
        .expect("integration head")
        .trimmed()
        .to_string();

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

    // Reject exactly this one ref's transaction, which is what a branch that
    // moved between the pre-merge read and the update looks like to us.
    let hook = repo.path().join(".git/hooks/reference-transaction");
    std::fs::create_dir_all(hook.parent().expect("hooks dir")).expect("hooks dir");
    std::fs::write(
        &hook,
        "#!/bin/sh\nwhile read -r old new ref; do case \"$ref\" in refs/heads/integration) exit 1;; esac; done\nexit 0\n",
    )
    .expect("write hook");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&hook, std::fs::Permissions::from_mode(0o755))
            .expect("hook permissions");
    }

    let outcome = try_merge(
        repo.git(),
        &node_path,
        &source,
        None,
        Some(&BranchName::from_raw("integration")),
        "fold child into node",
    )
    .expect("a refused advance is a typed outcome, not an Err");

    let node_head = repo
        .git()
        .try_run(&node_path, &["rev-parse", "HEAD"])
        .expect("node head")
        .trimmed()
        .to_string();
    match outcome {
        MergeOutcome::ManualGitRequired {
            target,
            reason,
            paths,
            ..
        } => {
            assert_eq!(target.as_str(), node_head, "target is the merge result");
            assert!(reason.contains("integration"), "reason: {reason}");
            assert!(paths.is_empty(), "no conflicted paths: {paths:?}");
        }
        other => panic!("expected a manual handoff, got {other:?}"),
    }
    assert_ne!(node_head, integration_before, "the merge itself landed");
    assert_eq!(
        repo.git()
            .try_run(repo.path(), &["rev-parse", "refs/heads/integration"])
            .expect("integration head")
            .trimmed(),
        integration_before,
        "the refused branch did not move"
    );
}

/// The advance branch is read before anything is mutated, so a name that does
/// not resolve is the ordinary git failure over an untouched repository.
#[test]
fn an_unknown_advance_branch_fails_before_the_merge() {
    let repo = TestRepo::init().expect("init repo");
    repo.writer()
        .commit_file("README.md", "base\n", "base commit")
        .expect("base commit");
    let child_path = add_worktree(&repo, "child", "main");
    repo.writer_at(&child_path)
        .commit_file("child.txt", "child\n", "child work")
        .expect("child commit");
    let node_path = add_worktree(&repo, "node", "main");
    let before = repo
        .git()
        .try_run(&node_path, &["rev-parse", "HEAD"])
        .expect("node head")
        .trimmed()
        .to_string();
    let source = GitOid::from_raw(
        repo.git()
            .try_run(&child_path, &["rev-parse", "HEAD"])
            .expect("child head")
            .trimmed(),
    );

    let err = try_merge(
        repo.git(),
        &node_path,
        &source,
        None,
        Some(&BranchName::from_raw("no-such-branch")),
        "fold child into node",
    )
    .expect_err("an unresolvable advance branch never enters a merge");

    match err {
        WorktreeError::GitFailure(receipt) => {
            assert!(receipt.args.iter().any(|a| a == "rev-parse"));
        }
        other => panic!("expected GitFailure, got {other:?}"),
    }
    assert_eq!(
        repo.git()
            .try_run(&node_path, &["rev-parse", "HEAD"])
            .expect("node head after refusal")
            .trimmed(),
        before
    );
}

#[test]
fn dirty_target_is_refused_before_merge_mutates_it() {
    let repo = TestRepo::init().expect("init repo");
    repo.writer()
        .commit_file("README.md", "base\n", "base commit")
        .expect("base commit");
    let child_path = add_worktree(&repo, "child", "main");
    repo.writer_at(&child_path)
        .commit_file("child.txt", "child\n", "child work")
        .expect("child commit");
    let node_path = add_worktree(&repo, "node", "main");
    std::fs::write(node_path.join("mine.txt"), "user state\n").expect("dirty target");
    let before = repo
        .git()
        .try_run(&node_path, &["rev-parse", "HEAD"])
        .expect("target head")
        .trimmed()
        .to_string();
    let source = GitOid::from_raw(
        repo.git()
            .try_run(&child_path, &["rev-parse", "HEAD"])
            .expect("child head")
            .trimmed(),
    );

    assert!(matches!(
        try_merge(repo.git(), &node_path, &source, None, None, "must refuse"),
        Err(WorktreeError::SourceDirty(_))
    ));
    assert_eq!(
        repo.git()
            .try_run(&node_path, &["rev-parse", "HEAD"])
            .expect("target head after refusal")
            .trimmed(),
        before
    );
    assert_eq!(
        std::fs::read_to_string(node_path.join("mine.txt")).expect("user state retained"),
        "user state\n"
    );
}
