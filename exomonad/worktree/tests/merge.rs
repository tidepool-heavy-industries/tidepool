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
        &child_path,
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

    let first = try_merge(
        repo.git(),
        &node_path,
        &child_path,
        &source,
        None,
        None,
        "fold",
    )
    .expect("fast-forward should work");
    assert!(matches!(first, MergeOutcome::FastForwarded { ref after, .. } if after == &source));

    let second = try_merge(
        repo.git(),
        &node_path,
        &child_path,
        &source,
        None,
        None,
        "fold again",
    )
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
        &child_path,
        &source,
        Some(&BranchName::from_raw("child")),
        None,
        "fold child into node",
    )
    .expect("merge should run (a conflict is a typed outcome, not an Err)");

    match outcome {
        MergeOutcome::ManualGitRequired { paths, reason, .. } => {
            assert_eq!(paths, vec!["shared.txt".to_string()]);
            assert_eq!(
                reason,
                "merge conflict; target was restored to its starting state"
            );
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
fn gitlink_only_conflict_reports_both_nested_commits_without_resolving_them() {
    let workspace_repo = TestRepo::init().expect("init workspace repo");
    let workspace_base = workspace_repo
        .writer()
        .commit_file("README.md", "workspace base\n", "workspace base")
        .expect("workspace base commit");
    let child_workspace_path = add_worktree(&workspace_repo, "workspace-child", "main");
    let child_workspace_commit = workspace_repo
        .writer_at(&child_workspace_path)
        .commit_file("child.txt", "child\n", "child workspace work")
        .expect("child workspace commit");
    let node_workspace_path = add_worktree(&workspace_repo, "workspace-node", "main");
    let node_workspace_commit = workspace_repo
        .writer_at(&node_workspace_path)
        .commit_file("node.txt", "node\n", "node workspace work")
        .expect("node workspace commit");

    let repo = TestRepo::init().expect("init repo");
    repo.writer()
        .commit_file("README.md", "base\n", "base commit")
        .expect("base commit");
    repo.writer()
        .write_file(
            ".gitmodules",
            &format!(
                "[submodule \"exomonad-workspace\"]\n\tpath = .exomonad/workspace\n\turl = {}\n",
                workspace_repo.path().display()
            ),
        )
        .expect("write .gitmodules");
    repo.git()
        .try_run(
            repo.path(),
            &[
                "clone",
                "-q",
                workspace_repo.path().to_str().unwrap(),
                ".exomonad/workspace",
            ],
        )
        .expect("clone source workspace");
    repo.git()
        .try_run(
            repo.path(),
            &[
                "update-index",
                "--add",
                "--cacheinfo",
                &format!("160000,{},.exomonad/workspace", workspace_base.as_str()),
            ],
        )
        .expect("record base workspace gitlink");
    repo.writer()
        .stage(".gitmodules")
        .expect("stage .gitmodules");
    repo.writer()
        .commit_file("project-base.txt", "base\n", "record workspace submodule")
        .expect("commit workspace submodule");

    let child_path = add_worktree(&repo, "child", "main");
    let child_workspace_path = child_path.join(".exomonad/workspace");
    std::fs::create_dir_all(child_workspace_path.parent().unwrap())
        .expect("create child workspace parent");
    repo.git()
        .try_run(
            &child_path,
            &[
                "clone",
                "-q",
                "--no-checkout",
                workspace_repo.path().to_str().unwrap(),
                ".exomonad/workspace",
            ],
        )
        .expect("clone nested workspace into child");
    repo.git()
        .try_run(
            &child_workspace_path,
            &["checkout", "-q", child_workspace_commit.as_str()],
        )
        .expect("checkout child workspace commit");
    repo.git()
        .try_run(
            &child_path,
            &[
                "update-index",
                "--add",
                "--cacheinfo",
                &format!(
                    "160000,{},.exomonad/workspace",
                    child_workspace_commit.as_str()
                ),
            ],
        )
        .expect("record child workspace gitlink");
    let child = repo
        .writer_at(&child_path)
        .commit_file(
            "child-side.txt",
            "child side\n",
            "bump child workspace gitlink",
        )
        .expect("child project commit");

    let node_path = add_worktree(&repo, "node", "main");
    let node_workspace_path = node_path.join(".exomonad/workspace");
    std::fs::create_dir_all(node_workspace_path.parent().unwrap())
        .expect("create node workspace parent");
    repo.git()
        .try_run(
            &node_path,
            &[
                "clone",
                "-q",
                "--no-checkout",
                workspace_repo.path().to_str().unwrap(),
                ".exomonad/workspace",
            ],
        )
        .expect("clone nested workspace into node");
    repo.git()
        .try_run(
            &node_workspace_path,
            &["checkout", "-q", node_workspace_commit.as_str()],
        )
        .expect("checkout node workspace commit");
    repo.git()
        .try_run(
            &node_path,
            &[
                "update-index",
                "--add",
                "--cacheinfo",
                &format!(
                    "160000,{},.exomonad/workspace",
                    node_workspace_commit.as_str()
                ),
            ],
        )
        .expect("record node workspace gitlink");
    repo.writer_at(&node_path)
        .commit_file(
            "node-side.txt",
            "node side\n",
            "bump node workspace gitlink",
        )
        .expect("node project commit");

    let outcome = try_merge(
        repo.git(),
        &node_path,
        &child_path,
        &child,
        None,
        None,
        "fold child",
    )
    .expect("gitlink conflict is a reportable handoff");
    match outcome {
        MergeOutcome::ManualGitRequired { reason, paths, .. } => {
            assert_eq!(paths, vec![".exomonad/workspace"]);
            assert!(reason.contains("only gitlinks"), "reason: {reason}");
            assert!(
                reason.contains(child_workspace_commit.as_str()),
                "reason: {reason}"
            );
            assert!(
                reason.contains(node_workspace_commit.as_str()),
                "reason: {reason}"
            );
            assert!(reason.contains("merge the workspace commits inside the workspace"));
        }
        other => panic!("expected a gitlink conflict handoff, got {other:?}"),
    }

    assert_eq!(
        repo.git()
            .try_run(&node_path, &["rev-parse", "HEAD"])
            .expect("node HEAD")
            .trimmed(),
        repo.git()
            .try_run(&node_path, &["rev-parse", "node"])
            .expect("node branch")
            .trimmed(),
        "the report does not resolve or advance the target"
    );
    assert_eq!(
        inspect::in_progress(repo.git(), &node_path).expect("in_progress read"),
        None,
        "the merge remains aborted after reporting"
    );
    assert_eq!(
        repo.git()
            .try_run(&node_workspace_path, &["rev-parse", "HEAD"])
            .expect("target workspace HEAD after abort")
            .trimmed(),
        node_workspace_commit.as_str(),
        "the conflict must preserve the target workspace checkout"
    );
    assert!(!node_workspace_path.join("child.txt").exists());
}

#[test]
fn merge_fetches_a_child_only_workspace_commit_before_merging_its_gitlink() {
    let workspace_upstream = TestRepo::init().expect("init workspace upstream");
    let workspace_base = workspace_upstream
        .writer()
        .commit_file("README.md", "workspace base\n", "workspace base")
        .expect("workspace base commit");

    let repo = TestRepo::init().expect("init project repo");
    repo.writer()
        .commit_file("README.md", "project base\n", "project base")
        .expect("project base commit");
    repo.writer()
        .write_file(
            ".gitmodules",
            &format!(
                "[submodule \"exomonad-workspace\"]\n\tpath = .exomonad/workspace\n\turl = {}\n",
                workspace_upstream.path().display()
            ),
        )
        .expect("write .gitmodules");
    repo.git()
        .try_run(
            repo.path(),
            &[
                "clone",
                "-q",
                workspace_upstream.path().to_str().unwrap(),
                ".exomonad/workspace",
            ],
        )
        .expect("clone workspace into project source");
    repo.git()
        .try_run(
            repo.path(),
            &[
                "update-index",
                "--add",
                "--cacheinfo",
                &format!("160000,{},.exomonad/workspace", workspace_base.as_str()),
            ],
        )
        .expect("record source workspace gitlink");
    repo.writer()
        .stage(".gitmodules")
        .expect("stage .gitmodules");
    repo.writer()
        .commit_file("project.txt", "base\n", "record workspace submodule")
        .expect("commit workspace submodule");

    let child_path = add_worktree(&repo, "child", "main");
    let child_workspace = child_path.join(".exomonad/workspace");
    std::fs::create_dir_all(child_workspace.parent().unwrap())
        .expect("create child workspace parent");
    repo.git()
        .try_run(
            &child_path,
            &[
                "clone",
                "-q",
                workspace_upstream.path().to_str().unwrap(),
                ".exomonad/workspace",
            ],
        )
        .expect("clone workspace into child");
    repo.git()
        .try_run(
            &child_workspace,
            &["checkout", "--detach", workspace_base.as_str()],
        )
        .expect("match the detached submodule checkout used by a worktree");
    let child_workspace_commit = repo
        .writer_at(&child_workspace)
        .commit_file(
            "child-only.txt",
            "unpublished\n",
            "child-only workspace commit",
        )
        .expect("create unpublished child workspace commit");
    assert!(
        workspace_upstream
            .git()
            .try_run(
                workspace_upstream.path(),
                &[
                    "cat-file",
                    "-e",
                    &format!("{}^{{commit}}", child_workspace_commit.as_str()),
                ],
            )
            .is_err(),
        "the workspace commit must not exist in the upstream repository"
    );
    repo.git()
        .try_run(&child_path, &["add", "--", ".exomonad/workspace"])
        .expect("stage child gitlink bump");
    let child = repo
        .writer_at(&child_path)
        .commit_file("child-side.txt", "child\n", "child project work")
        .expect("commit child project work");

    let node_path = add_worktree(&repo, "node", "main");
    let node_workspace = node_path.join(".exomonad/workspace");
    std::fs::create_dir_all(node_workspace.parent().unwrap())
        .expect("create node workspace parent");
    repo.git()
        .try_run(
            &node_path,
            &[
                "clone",
                "-q",
                workspace_upstream.path().to_str().unwrap(),
                ".exomonad/workspace",
            ],
        )
        .expect("clone workspace into target");
    assert!(
        repo.git()
            .try_run(
                &node_workspace,
                &[
                    "cat-file",
                    "-e",
                    &format!("{}^{{commit}}", child_workspace_commit.as_str()),
                ],
            )
            .is_err(),
        "the target workspace must not have the child-only object before merge"
    );
    repo.writer_at(&node_path)
        .commit_file("node-side.txt", "node\n", "node project work")
        .expect("commit node work");

    let outcome = try_merge(
        repo.git(),
        &node_path,
        &child_path,
        &child,
        None,
        None,
        "fold child workspace bump",
    )
    .expect("fetch should make the source gitlink mergeable");
    assert!(
        matches!(outcome, MergeOutcome::CreatedMergeCommit { .. }),
        "the project changes are otherwise independent: {outcome:?}"
    );
    let merged_link = repo
        .git()
        .try_run(
            &node_path,
            &["ls-tree", "HEAD", "--", ".exomonad/workspace"],
        )
        .expect("read merged workspace gitlink");
    assert_eq!(
        merged_link.trimmed().split_ascii_whitespace().nth(2),
        Some(child_workspace_commit.as_str())
    );
    repo.git()
        .try_run(
            &node_workspace,
            &[
                "cat-file",
                "-e",
                &format!("{}^{{commit}}", child_workspace_commit.as_str()),
            ],
        )
        .expect("target workspace now has the child-only object");
    assert_eq!(
        repo.git()
            .try_run(&node_workspace, &["rev-parse", "HEAD"])
            .expect("target workspace HEAD after merge")
            .trimmed(),
        child_workspace_commit.as_str(),
        "the initialized checkout must advance to the merged gitlink"
    );
    assert_eq!(
        std::fs::read_to_string(node_workspace.join("child-only.txt"))
            .expect("merged workspace file"),
        "unpublished\n"
    );
    assert!(
        inspect::dirty_summary(repo.git(), &node_path)
            .expect("merged target status")
            .is_clean(),
        "the target should be clean after its submodule is synchronized"
    );
}

#[test]
fn workspace_checkout_failure_reports_the_landed_merge() {
    for divergent in [false, true] {
        let workspace_repo = TestRepo::init().expect("init workspace repo");
        let workspace_base = workspace_repo
            .writer()
            .commit_file("README.md", "base\n", "workspace base")
            .expect("workspace base commit");

        let repo = TestRepo::init().expect("init project repo");
        repo.writer()
            .commit_file("README.md", "project base\n", "project base")
            .expect("project base commit");
        repo.writer()
            .write_file(
                ".gitmodules",
                &format!(
                    "[submodule \"exomonad-workspace\"]\n\tpath = .exomonad/workspace\n\turl = {}\n\tignore = dirty\n",
                    workspace_repo.path().display()
                ),
            )
            .expect("write .gitmodules");
        repo.git()
            .try_run(
                repo.path(),
                &[
                    "clone",
                    "-q",
                    workspace_repo.path().to_str().unwrap(),
                    ".exomonad/workspace",
                ],
            )
            .expect("clone base workspace");
        repo.git()
            .try_run(
                repo.path(),
                &[
                    "update-index",
                    "--add",
                    "--cacheinfo",
                    &format!("160000,{},.exomonad/workspace", workspace_base.as_str()),
                ],
            )
            .expect("record base gitlink");
        repo.writer()
            .stage(".gitmodules")
            .expect("stage .gitmodules");
        repo.writer()
            .commit_file("project.txt", "base\n", "record workspace")
            .expect("project commit");

        let child_path = add_worktree(&repo, "child", "main");
        let child_workspace = child_path.join(".exomonad/workspace");
        std::fs::create_dir_all(child_workspace.parent().unwrap()).expect("child workspace parent");
        repo.git()
            .try_run(
                &child_path,
                &[
                    "clone",
                    "-q",
                    workspace_repo.path().to_str().unwrap(),
                    ".exomonad/workspace",
                ],
            )
            .expect("clone child workspace");
        repo.git()
            .try_run(
                &child_workspace,
                &["checkout", "--detach", workspace_base.as_str()],
            )
            .expect("detach child workspace");
        let child_workspace_commit = repo
            .writer_at(&child_workspace)
            .commit_file("README.md", "child version\n", "child workspace work")
            .expect("child workspace commit");
        repo.git()
            .try_run(&child_path, &["add", "--", ".exomonad/workspace"])
            .expect("stage child gitlink");
        let child = repo
            .writer_at(&child_path)
            .commit_file("child.txt", "child\n", "child project work")
            .expect("child project commit");

        let node_path = add_worktree(&repo, "node", "main");
        let node_workspace = node_path.join(".exomonad/workspace");
        std::fs::create_dir_all(node_workspace.parent().unwrap()).expect("node workspace parent");
        repo.git()
            .try_run(
                &node_path,
                &[
                    "clone",
                    "-q",
                    workspace_repo.path().to_str().unwrap(),
                    ".exomonad/workspace",
                ],
            )
            .expect("clone node workspace");
        if divergent {
            repo.writer_at(&node_path)
                .commit_file("node.txt", "node\n", "node project work")
                .expect("node project commit");
        }
        std::fs::write(node_workspace.join("README.md"), "local workspace edit\n")
            .expect("edit node workspace");
        assert!(
            inspect::dirty_summary(repo.git(), &node_path)
                .expect("node status")
                .is_clean(),
            "the project checkout must be eligible for merging"
        );

        let outcome = try_merge(
            repo.git(),
            &node_path,
            &child_path,
            &child,
            None,
            None,
            "fold child",
        )
        .expect("workspace sync failure is a typed handoff");
        let landed = repo
            .git()
            .try_run(&node_path, &["rev-parse", "HEAD"])
            .expect("node HEAD")
            .trimmed()
            .to_string();
        match outcome {
            MergeOutcome::ManualGitRequired {
                target,
                reason,
                paths,
                ..
            } => {
                assert_eq!(target.as_str(), landed, "handoff names the landed commit");
                assert!(reason.contains(&landed), "reason: {reason}");
                assert!(reason.contains("workspace checkout"), "reason: {reason}");
                // Assert on the git invocation THIS CRATE issued (the
                // `checkout --detach` in `update_initialized_workspace`),
                // not on git's own English error text, which is
                // version/locale-dependent and not something this crate
                // controls.
                assert!(reason.contains("git checkout --detach"), "reason: {reason}");
                assert!(paths.is_empty(), "no merge conflicts: {paths:?}");
            }
            other => panic!("expected a workspace sync handoff, got {other:?}"),
        }
        assert_eq!(
            repo.git()
                .try_run(
                    &node_path,
                    &["ls-tree", "HEAD", "--", ".exomonad/workspace"]
                )
                .expect("merged gitlink")
                .trimmed()
                .split_ascii_whitespace()
                .nth(2),
            Some(child_workspace_commit.as_str()),
            "the gitlink bump remains landed"
        );
        assert_eq!(
            std::fs::read_to_string(node_workspace.join("README.md"))
                .expect("local workspace edit"),
            "local workspace edit\n",
            "failed synchronization must not discard local edits"
        );
        assert_eq!(
            inspect::in_progress(repo.git(), &node_path).expect("merge state"),
            None
        );
    }
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
        repo.path(),
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
        &child_path,
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
        &child_path,
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
        &child_path,
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
        &child_path,
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
        try_merge(
            repo.git(),
            &node_path,
            &child_path,
            &source,
            None,
            None,
            "must refuse"
        ),
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
