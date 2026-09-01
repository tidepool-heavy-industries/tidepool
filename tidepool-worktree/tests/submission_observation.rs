//! Real-git acceptance tests for the owning submission observation.

use std::path::Path;

use tidepool_worktree::testing::TestRepo;
use tidepool_worktree::{
    GitCli, HeadState, WorktreeError, WorktreeManager, WorktreeRegistry, WorktreeSpec,
};

fn manager_over(repo: &TestRepo, base: &Path) -> WorktreeManager {
    let registry = WorktreeRegistry::open(base.join("registry")).expect("open registry");
    WorktreeManager::new(GitCli::new(), registry, base.join("worktrees"), repo.path())
}

#[test]
fn clean_branch_observation_carries_base_and_fresh_submitted_head() {
    let repo = TestRepo::init().expect("init");
    repo.writer()
        .commit_file("base.txt", "base", "base")
        .expect("base commit");
    let storage = tempfile::TempDir::new().expect("storage");
    let manager = manager_over(&repo, storage.path());
    let handle = manager
        .create(&WorktreeSpec::from_current_repository("worker"))
        .expect("create worktree");
    let base = handle.source_head().clone();
    let submitted = repo
        .writer_at(handle.cwd())
        .commit_file("answer.txt", "done", "candidate")
        .expect("candidate commit");

    let observed = manager
        .observe_submission(&handle)
        .expect("observe submission");

    assert_eq!(observed.worktree_id, *handle.id());
    assert_eq!(observed.base_head, base);
    assert_eq!(
        observed.submitted_head,
        HeadState::OnBranch {
            branch: handle.branch().clone(),
            oid: submitted,
        }
    );
    assert!(observed.working_state.changes.is_clean());
    assert_eq!(observed.working_state.operation, None);
}

#[test]
fn detached_and_dirty_state_are_reported_truthfully() {
    let repo = TestRepo::init().expect("init");
    repo.writer()
        .commit_file("tracked.txt", "base", "base")
        .expect("base commit");
    let storage = tempfile::TempDir::new().expect("storage");
    let manager = manager_over(&repo, storage.path());
    let handle = manager
        .create(&WorktreeSpec::from_current_repository("worker"))
        .expect("create worktree");
    let git = GitCli::new();
    git.try_run(handle.cwd(), &["checkout", "--detach", "-q"])
        .expect("detach");
    let writer = repo.writer_at(handle.cwd());
    writer
        .write_file("tracked.txt", "unstaged")
        .expect("modify tracked");
    writer
        .write_file("staged.txt", "staged")
        .expect("write staged");
    writer.stage("staged.txt").expect("stage");
    writer
        .write_file("untracked.txt", "untracked")
        .expect("write untracked");

    let observed = manager
        .observe_submission(&handle)
        .expect("observe submission");

    let oid = manager.worktree_head(&handle).expect("head");
    assert_eq!(observed.submitted_head, HeadState::Detached { oid });
    assert_eq!(observed.working_state.changes.staged, ["staged.txt"]);
    assert_eq!(observed.working_state.changes.unstaged, ["tracked.txt"]);
    assert_eq!(observed.working_state.changes.untracked, ["untracked.txt"]);
}

#[test]
fn a_removed_checkout_is_lost_not_an_empty_observation() {
    let repo = TestRepo::init().expect("init");
    repo.writer()
        .commit_file("base.txt", "base", "base")
        .expect("base commit");
    let storage = tempfile::TempDir::new().expect("storage");
    let manager = manager_over(&repo, storage.path());
    let handle = manager
        .create(&WorktreeSpec::from_current_repository("worker"))
        .expect("create worktree");
    std::fs::remove_dir_all(handle.cwd()).expect("remove checkout");

    assert_eq!(
        manager.observe_submission(&handle),
        Err(WorktreeError::WorktreeLost(handle.id().clone()))
    );
}
