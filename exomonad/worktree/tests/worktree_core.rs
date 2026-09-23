//! Acceptance tests for durable registry, clean worktree creation,
//! restart lookup, and the one-worktree-one-agent binding state machine.
//!
//! Every test drives a REAL temporary git repository via
//! [`exomonad_worktree::testing::TestRepo`] and [`ScriptedWriter`]. There is
//! no mock of git anywhere.

use std::collections::BTreeMap;
use std::path::Path;

use exomonad_worktree::git::inspect;
use exomonad_worktree::testing::{fingerprint, TestRepo};
use exomonad_worktree::{
    AgentRef, BindingTable, BranchName, DirtySummary, GitCli, GitOid, GitRef, InProgressKind,
    WorktreeError, WorktreeId, WorktreeManager, WorktreeOrigin, WorktreeReceipt,
    WorktreeRecordStatus, WorktreeRegistry, WorktreeSource, WorktreeSpec,
};
use tidepool_repr::ActorPath;

fn manager_over(repo: &TestRepo, base: &Path) -> WorktreeManager {
    let registry = WorktreeRegistry::open(base.join("registry")).expect("open registry");
    WorktreeManager::new(GitCli::new(), registry, base.join("worktrees"), repo.path())
}

/// Everything a "did we dirty this repository" comparison needs: working-tree
/// bytes, the checked-out branch, `HEAD`, and the reconciled dirty summary.
struct SourceState {
    fingerprint: BTreeMap<String, (Vec<u8>, u32)>,
    branch: Option<String>,
    head: String,
    dirty: DirtySummary,
}

fn capture_state(git: &GitCli, path: &Path) -> SourceState {
    let fingerprint = fingerprint::working_tree(path);
    let branch = git
        .run(path, &["symbolic-ref", "--short", "HEAD"])
        .ok()
        .map(|o| o.trimmed().to_string());
    let head = git
        .run(path, &["rev-parse", "HEAD"])
        .expect("rev-parse HEAD")
        .trimmed()
        .to_string();
    let dirty = inspect::dirty_summary(git, path).expect("dirty_summary");
    SourceState {
        fingerprint,
        branch,
        head,
        dirty,
    }
}

fn assert_untouched(before: &SourceState, after: &SourceState) {
    assert_eq!(
        before.fingerprint, after.fingerprint,
        "working tree bytes must be unchanged"
    );
    assert_eq!(
        before.branch, after.branch,
        "checked-out branch must be unchanged"
    );
    assert_eq!(before.head, after.head, "HEAD must be unchanged");
    assert_eq!(before.dirty, after.dirty, "dirty summary must be unchanged");
}

#[test]
fn clean_creation_from_current_repository_leaves_source_untouched() {
    let repo = TestRepo::init().expect("init");
    let w = repo.writer();
    w.commit_file("a.txt", "one", "first").expect("commit");

    let base = tempfile::TempDir::new().expect("tempdir");
    let manager = manager_over(&repo, base.path());
    let git = GitCli::new();

    let before = capture_state(&git, repo.path());
    let handle = manager
        .create(&WorktreeSpec::from_current_repository("child"))
        .expect("create");
    let after = capture_state(&git, repo.path());

    assert_untouched(&before, &after);

    assert!(handle.branch().as_str().starts_with("exomonad/worktree/"));
    assert_eq!(handle.source_head().as_str(), before.head);
    assert!(handle.cwd().exists());
    assert!(!handle.cwd().starts_with(repo.path()));
    assert!(handle.cwd().join("a.txt").exists());
    assert!(matches!(
        handle.receipt().origin,
        WorktreeOrigin::CurrentRepository
    ));
    assert!(handle.cwd().join(".git").is_file());
    assert_eq!(
        inspect::git_common_dir(&git, handle.cwd()).expect("resolve shared Git metadata"),
        repo.path().join(".git")
    );
}

#[test]
fn created_worktree_initializes_submodules_at_the_recorded_gitlink() {
    let workspace = TestRepo::init().expect("init workspace repository");
    workspace
        .writer()
        .commit_file("Project/Generic.hs", "module Generic where\n", "workspace")
        .expect("commit workspace module");

    let repo = TestRepo::init().expect("init project repository");
    repo.git()
        .try_run(
            repo.path(),
            &[
                "-c",
                "protocol.file.allow=always",
                "submodule",
                "add",
                workspace.path().to_str().expect("workspace path is utf8"),
                ".exomonad/workspace",
            ],
        )
        .expect("add workspace submodule");
    repo.git()
        .try_run(repo.path(), &["commit", "-q", "-m", "add workspace"])
        .expect("commit workspace gitlink");
    let recorded = repo
        .git()
        .try_run(repo.path(), &["rev-parse", "HEAD:.exomonad/workspace"])
        .expect("read recorded workspace gitlink");

    let base = tempfile::TempDir::new().expect("tempdir");
    // Test-only transport permission for the local fixture URL. Production
    // worktree creation does not broaden Git's protocol policy.
    let registry = WorktreeRegistry::open(base.path().join("registry")).expect("open registry");
    let git = GitCli::new().with_env("GIT_ALLOW_PROTOCOL", "file:https:ssh:git");
    let child = WorktreeManager::new(git, registry, base.path().join("worktrees"), repo.path())
        .create(&WorktreeSpec::from_current_repository("workspace-child"))
        .expect("create child worktree");

    assert_eq!(
        GitCli::new()
            .try_run(
                &child.cwd().join(".exomonad/workspace"),
                &["rev-parse", "HEAD"]
            )
            .expect("read child workspace HEAD")
            .trimmed(),
        recorded.trimmed(),
        "the child must check out the exact gitlink recorded by its source commit"
    );
    assert!(child
        .cwd()
        .join(".exomonad/workspace/Project/Generic.hs")
        .is_file());
    let pointer = std::fs::read_to_string(child.cwd().join(".exomonad/workspace/.git"))
        .expect("read child submodule pointer");
    assert!(
        pointer.starts_with("gitdir: /"),
        "mounted child views need an absolute submodule Git pointer: {pointer}"
    );
}

#[test]
fn inherited_source_prepares_private_authored_workspace_before_mount() {
    let nested = TestRepo::init().expect("init nested repository");
    nested
        .writer()
        .commit_file("Nested.hs", "module Nested where\n", "nested module")
        .expect("commit nested module");
    let workspace = TestRepo::init().expect("init workspace repository");
    workspace
        .writer()
        .commit_file("Project/Generic.hs", "module Generic where\n", "workspace")
        .expect("commit workspace module");
    workspace
        .git()
        .try_run(
            workspace.path(),
            &[
                "-c",
                "protocol.file.allow=always",
                "submodule",
                "add",
                nested.path().to_str().expect("nested path is utf8"),
                "Modules/nested",
            ],
        )
        .expect("add nested submodule");
    workspace
        .git()
        .try_run(
            workspace.path(),
            &["commit", "-q", "-m", "record nested module"],
        )
        .expect("commit nested gitlink");
    let repo = TestRepo::init().expect("init project repository");
    repo.git()
        .try_run(
            repo.path(),
            &[
                "-c",
                "protocol.file.allow=always",
                "submodule",
                "add",
                "--name",
                "exomonad-workspace",
                workspace.path().to_str().expect("workspace path is utf8"),
                ".exomonad/workspace",
            ],
        )
        .expect("add workspace submodule");
    repo.writer()
        .commit_file(".exomonad/config.toml", "[defaults]\n", "authored config")
        .expect("commit authored config and gitlink");
    repo.writer()
        .write_file(".exomonad/config.toml", "[defaults]\nmodel = \"local\"\n")
        .expect("edit authored config");
    let committed = repo
        .git()
        .checkpoint_source(repo.path(), &[])
        .expect("checkpoint authored config");

    let storage = tempfile::TempDir::new().expect("tempdir");
    let manager = WorktreeManager::new(
        GitCli::new(),
        WorktreeRegistry::open(storage.path().join("registry")).expect("open registry"),
        storage.path().join("worktrees"),
        repo.path(),
    );
    let prepared = manager
        .prepare_inherited_source(
            &WorktreeSource::CurrentRepository,
            &tidepool_repr::ActorPath::parse("root/child").expect("actor path"),
        )
        .expect("prepare inherited child");
    let child = prepared.receipt().cwd.as_path();
    assert_eq!(prepared.receipt().source_head, committed);
    assert_eq!(
        std::fs::read_to_string(child.join(".exomonad/config.toml"))
            .expect("read child authored config"),
        "[defaults]\nmodel = \"local\"\n"
    );
    assert!(child
        .join(".exomonad/workspace/Project/Generic.hs")
        .is_file());
    assert!(
        std::fs::read_to_string(child.join(".exomonad/workspace/.git"))
            .expect("read child submodule pointer")
            .starts_with("gitdir: /")
    );
    assert!(
        std::fs::read_to_string(child.join(".exomonad/workspace/Modules/nested/.git"))
            .expect("read nested submodule pointer")
            .starts_with("gitdir: /")
    );
    assert!(
        !child.join("README.md").exists(),
        "ordinary files await the mounted view"
    );
    assert_eq!(
        std::fs::read_to_string(repo.path().join(".exomonad/config.toml"))
            .expect("read parent config"),
        "[defaults]\nmodel = \"local\"\n"
    );
}

#[test]
fn created_worktree_fetches_an_unpublished_workspace_commit_from_its_parent() {
    let upstream_workspace = TestRepo::init().expect("init upstream workspace");
    upstream_workspace
        .writer()
        .commit_file(
            "Project/Generic.hs",
            "module Generic where\n",
            "published workspace",
        )
        .expect("commit published workspace module");

    let repo = TestRepo::init().expect("init project repository");
    repo.git()
        .try_run(
            repo.path(),
            &[
                "-c",
                "protocol.file.allow=always",
                "submodule",
                "add",
                "--name",
                "exomonad-workspace",
                upstream_workspace
                    .path()
                    .to_str()
                    .expect("workspace path is utf8"),
                ".exomonad/workspace",
            ],
        )
        .expect("add workspace submodule");
    let parent_workspace = repo.path().join(".exomonad/workspace");
    repo.git()
        .try_run(&parent_workspace, &["checkout", "-q", "-b", "local-only"])
        .expect("create unpublished workspace branch");
    let unpublished = repo
        .writer_at(&parent_workspace)
        .commit_file(
            "Project/Private.hs",
            "module Private where\n",
            "unpublished workspace change",
        )
        .expect("commit unpublished workspace change");
    repo.git()
        .try_run(repo.path(), &["add", "--", ".exomonad/workspace"])
        .expect("stage unpublished gitlink");
    repo.git()
        .try_run(
            repo.path(),
            &["commit", "-q", "-m", "record local workspace commit"],
        )
        .expect("commit project gitlink");
    let upstream_url = repo
        .git()
        .try_run(
            repo.path(),
            &[
                "config",
                "--file",
                ".gitmodules",
                "--get",
                "submodule.exomonad-workspace.url",
            ],
        )
        .expect("read upstream URL");

    let base = tempfile::TempDir::new().expect("tempdir");
    let registry = WorktreeRegistry::open(base.path().join("registry")).expect("open registry");
    // Parent-local transport must work with ordinary production Git policy;
    // the child cannot fetch this commit from the upstream URL.
    let git = GitCli::new();
    let manager = WorktreeManager::new(git, registry, base.path().join("worktrees"), repo.path());
    let parent = manager
        .create(&WorktreeSpec::from_current_repository(
            "local-workspace-parent",
        ))
        .expect("create parent from local workspace commit");
    let child = manager
        .create(&WorktreeSpec::from_worktree(
            parent.id().clone(),
            "local-workspace-child",
        ))
        .expect("create child from parent's local workspace repository");

    let child_workspace = child.cwd().join(".exomonad/workspace");
    assert_eq!(
        GitCli::new()
            .try_run(&child_workspace, &["rev-parse", "HEAD"])
            .expect("read child workspace HEAD")
            .trimmed(),
        unpublished.as_str(),
        "child should receive the parent-local commit absent from the upstream repo"
    );
    assert!(child_workspace.join("Project/Private.hs").is_file());
    assert_eq!(
        GitCli::new()
            .try_run(
                child.cwd(),
                &[
                    "config",
                    "--file",
                    ".gitmodules",
                    "--get",
                    "submodule.exomonad-workspace.url",
                ],
            )
            .expect("read child's recorded upstream URL")
            .trimmed(),
        upstream_url.trimmed(),
        "the command-scoped URL override must not rewrite .gitmodules"
    );
    assert_eq!(
        GitCli::new()
            .try_run(
                child.cwd(),
                &[
                    "config",
                    "--local",
                    "--get",
                    "submodule.exomonad-workspace.url",
                ],
            )
            .expect("read child's initialized upstream URL")
            .trimmed(),
        upstream_url.trimmed(),
        "initialization must persist the upstream URL, not the command override"
    );
    assert_eq!(
        GitCli::new()
            .try_run(&child_workspace, &["remote", "get-url", "origin"])
            .expect("read child's workspace clone origin")
            .trimmed(),
        upstream_url.trimmed(),
        "the local bootstrap transport must not persist as the clone origin"
    );
}

#[test]
fn source_checkout_registration_is_clean_idempotent_and_non_mutating() {
    let repo = TestRepo::init().expect("init");
    repo.writer()
        .commit_file("base.txt", "base", "base")
        .expect("commit base");
    let base = tempfile::TempDir::new().expect("tempdir");
    let manager = manager_over(&repo, base.path());
    let git = GitCli::new();
    let before = capture_state(&git, repo.path());

    let first = manager
        .register_source_checkout()
        .expect("register clean source");
    let second = manager
        .register_source_checkout()
        .expect("repeat registration");

    assert_eq!(first.id(), second.id());
    assert_eq!(first.cwd(), repo.path());
    assert_eq!(first.receipt().origin, WorktreeOrigin::SourceCheckout);
    assert_eq!(first.branch().as_str(), before.branch.as_deref().unwrap());
    assert_untouched(&before, &capture_state(&git, repo.path()));
}

#[test]
fn source_checkout_registration_refuses_user_changes_without_recording_a_target() {
    let repo = TestRepo::init().expect("init");
    repo.writer()
        .commit_file("base.txt", "base", "base")
        .expect("commit base");
    repo.writer()
        .write_file("untracked.txt", "mine")
        .expect("write user file");
    let base = tempfile::TempDir::new().expect("tempdir");
    let manager = manager_over(&repo, base.path());

    assert!(matches!(
        manager.register_source_checkout(),
        Err(WorktreeError::SourceDirty(_))
    ));
    assert!(manager.list().expect("list registry").is_empty());
    assert_eq!(
        std::fs::read_to_string(repo.path().join("untracked.txt")).expect("read user file"),
        "mine"
    );
}

#[test]
fn actor_and_descendant_branches_coexist_in_git_namespace() {
    let repo = TestRepo::init().expect("init");
    repo.writer()
        .commit_file("base.txt", "base", "base")
        .expect("commit base");
    let base = tempfile::TempDir::new().expect("tempdir");
    let manager = manager_over(&repo, base.path());

    let parent_path = ActorPath::parse("campaign/runtime/scaffold").expect("parent path");
    let child_path =
        ActorPath::parse("campaign/runtime/scaffold/leaves/parser").expect("child path");
    let parent = manager
        .create_for_actor_path(
            &WorktreeSpec::from_current_repository(parent_path.to_string()),
            &parent_path,
        )
        .expect("create parent actor worktree");
    let child = manager
        .create_for_actor_path(
            &WorktreeSpec::from_worktree(parent.id().clone(), child_path.to_string()),
            &child_path,
        )
        .expect("create descendant actor worktree");

    assert_eq!(
        parent.branch().as_str(),
        "exomonad/campaign/runtime/branches/scaffold"
    );
    assert_eq!(
        child.branch().as_str(),
        "exomonad/campaign/runtime/scaffold/leaves/branches/parser"
    );
}

#[test]
fn worker_commit_is_visible_in_shared_namespace_without_moving_source_head() {
    let repo = TestRepo::init().expect("init");
    repo.writer()
        .commit_file("base.txt", "base", "base")
        .expect("commit base");
    let source_head = repo.writer().head().expect("source head");

    let base = tempfile::TempDir::new().expect("tempdir");
    let manager = manager_over(&repo, base.path());
    let handle = manager
        .create(&WorktreeSpec::from_current_repository("shared-git"))
        .expect("create");
    let worker = repo.writer_at(handle.cwd());
    worker
        .commit_file("candidate.txt", "candidate", "candidate")
        .expect("commit candidate");
    let candidate = worker.head().expect("candidate head");
    let git = GitCli::new();
    git.try_run(handle.cwd(), &["config", "tidepool.worker", "yes"])
        .expect("write shared config");

    assert_eq!(
        repo.writer().head().expect("source head after work"),
        source_head
    );
    let source_worker_ref = format!("refs/heads/{}", handle.branch().as_str());
    assert_eq!(
        git.run(
            repo.path(),
            &["show-ref", "--hash", "--verify", &source_worker_ref]
        )
        .expect("worker branch visible at source")
        .trimmed(),
        candidate.as_str()
    );
    assert_eq!(
        git.run(repo.path(), &["config", "--get", "tidepool.worker"])
            .expect("shared config visible")
            .trimmed(),
        "yes"
    );
    assert!(git
        .run(
            repo.path(),
            &[
                "cat-file",
                "-e",
                &format!("{}^{{commit}}", candidate.as_str())
            ]
        )
        .is_ok());
    assert!(!repo.path().join("candidate.txt").exists());
}

#[test]
fn clean_creation_from_a_ref() {
    let repo = TestRepo::init().expect("init");
    let w = repo.writer();
    w.commit_file("a.txt", "one", "first").expect("commit");
    let tagged = w.head().expect("head");
    repo.git()
        .try_run(repo.path(), &["tag", "v1"])
        .expect("tag");
    w.commit_file("b.txt", "two", "second").expect("commit");

    let base = tempfile::TempDir::new().expect("tempdir");
    let manager = manager_over(&repo, base.path());

    let handle = manager
        .create(&WorktreeSpec::from_ref(GitRef::from_raw("v1"), "from-ref"))
        .expect("create");

    assert_eq!(handle.source_head().as_str(), tagged.as_str());
    assert!(handle.cwd().join("a.txt").exists());
    assert!(!handle.cwd().join("b.txt").exists());
    assert!(matches!(
        &handle.receipt().origin,
        WorktreeOrigin::Ref(r) if r.as_str() == "v1"
    ));
}

#[test]
fn clean_creation_from_another_managed_worktree() {
    let repo = TestRepo::init().expect("init");
    let w = repo.writer();
    w.commit_file("a.txt", "one", "first").expect("commit");

    let base = tempfile::TempDir::new().expect("tempdir");
    let manager = manager_over(&repo, base.path());
    let git = GitCli::new();

    let parent = manager
        .create(&WorktreeSpec::from_current_repository("parent"))
        .expect("create parent");
    let parent_writer = repo.writer_at(parent.cwd());
    parent_writer
        .commit_file("p.txt", "parent-only", "parent commit")
        .expect("commit in parent worktree");

    let before = capture_state(&git, parent.cwd());
    let child = manager
        .create(&WorktreeSpec::from_worktree(parent.id().clone(), "child"))
        .expect("create from worktree");
    let after = capture_state(&git, parent.cwd());

    assert_untouched(&before, &after);

    assert_eq!(child.source_head().as_str(), before.head);
    assert!(child.cwd().join("p.txt").exists());
    match &child.receipt().origin {
        WorktreeOrigin::Worktree(id) => assert_eq!(id, parent.id()),
        other => panic!("expected Worktree origin, got {other:?}"),
    }
}

/// The durable receipt records the immediate checkout a child was seeded
/// from, even though every linked worktree shares one Git namespace.
#[test]
fn from_worktree_creation_records_each_immediate_seed_repository() {
    let repo = TestRepo::init().expect("init");
    repo.writer()
        .commit_file("a.txt", "one", "first")
        .expect("commit");

    let base = tempfile::TempDir::new().expect("tempdir");
    let manager = manager_over(&repo, base.path());

    let a = manager
        .create(&WorktreeSpec::from_current_repository("a"))
        .expect("create a");
    assert_eq!(a.receipt().source_repository, repo.path());

    let b = manager
        .create(&WorktreeSpec::from_worktree(a.id().clone(), "b"))
        .expect("create b from a");
    assert_eq!(b.receipt().source_repository, a.cwd());

    // Chained: c is seeded from b's current state.
    let c = manager
        .create(&WorktreeSpec::from_worktree(b.id().clone(), "c"))
        .expect("create c from b");
    assert_eq!(c.receipt().source_repository, b.cwd());
}

#[test]
fn dirty_source_refuses_by_default() {
    let repo = TestRepo::init().expect("init");
    let w = repo.writer();
    w.commit_file("a.txt", "one", "first").expect("commit");
    w.write_file("a.txt", "dirty")
        .expect("dirty the working tree");

    let base = tempfile::TempDir::new().expect("tempdir");
    let manager = manager_over(&repo, base.path());

    let err = manager
        .create(&WorktreeSpec::from_current_repository("child"))
        .expect_err("dirty source must refuse");
    match err {
        WorktreeError::SourceDirty(summary) => {
            assert_eq!(summary.unstaged, vec!["a.txt".to_string()]);
            assert!(summary.staged.is_empty());
            assert!(summary.untracked.is_empty());
        }
        other => panic!("expected SourceDirty, got {other:?}"),
    }

    // Refused before any registry row or worktree materializes.
    assert!(manager.list().expect("list").is_empty());
}

#[test]
fn source_mid_rebase_refuses() {
    let repo = TestRepo::init().expect("init");
    let w = repo.writer();
    w.commit_file("x.txt", "base\n", "base")
        .expect("base commit");
    w.checkout_new_branch("feature").expect("branch");
    w.commit_file("x.txt", "feature change\n", "feature commit")
        .expect("feature commit");
    w.checkout("main").expect("checkout main");
    w.commit_file("x.txt", "main change\n", "main commit")
        .expect("main commit");
    w.checkout("feature").expect("checkout feature");
    assert!(
        w.rebase_onto("main").is_err(),
        "expected a conflicting rebase to stop mid-way"
    );

    let base = tempfile::TempDir::new().expect("tempdir");
    let manager = manager_over(&repo, base.path());

    let err = manager
        .create(&WorktreeSpec::from_current_repository("child"))
        .expect_err("mid-rebase source must refuse");
    assert!(
        matches!(
            err,
            WorktreeError::SourceOperationInProgress(InProgressKind::Rebase)
        ),
        "expected SourceOperationInProgress(Rebase), got {err:?}"
    );
}

#[test]
fn registry_and_lookup_survive_a_fresh_manager_over_the_same_root() {
    let repo = TestRepo::init().expect("init");
    repo.writer()
        .commit_file("a.txt", "one", "first")
        .expect("commit");

    let base = tempfile::TempDir::new().expect("tempdir");
    let id = {
        let manager = manager_over(&repo, base.path());
        let handle = manager
            .create(&WorktreeSpec::from_current_repository("child"))
            .expect("create");
        handle.id().clone()
    };

    // A second, independent manager/registry over the same on-disk root — as
    // a fresh process restarting would build.
    let fresh = manager_over(&repo, base.path());
    let found = fresh
        .lookup(&id)
        .expect("lookup")
        .expect("still registered after restart");
    assert_eq!(found.id(), &id);
    assert!(found.cwd().exists());

    let listed = fresh.list().expect("list");
    assert!(listed
        .iter()
        .any(|s| s.receipt.worktree_id == id && s.present));
}

#[test]
fn hand_deleted_worktree_is_lost_not_recreated() {
    let repo = TestRepo::init().expect("init");
    repo.writer()
        .commit_file("a.txt", "one", "first")
        .expect("commit");

    let base = tempfile::TempDir::new().expect("tempdir");
    let manager = manager_over(&repo, base.path());
    let handle = manager
        .create(&WorktreeSpec::from_current_repository("child"))
        .expect("create");
    let id = handle.id().clone();
    let cwd = handle.cwd().to_path_buf();

    std::fs::remove_dir_all(&cwd).expect("remove worktree dir by hand");

    let err = manager.lookup(&id).expect_err("lost worktree must error");
    assert!(matches!(&err, WorktreeError::WorktreeLost(lost) if lost == &id));

    // Second lookup: still lost, never silently recreated.
    let err2 = manager.lookup(&id).expect_err("still lost");
    assert!(matches!(err2, WorktreeError::WorktreeLost(_)));
    assert!(
        !cwd.exists(),
        "a lost worktree must never be recreated on disk"
    );

    let listed = manager.list().expect("list");
    let summary = listed
        .iter()
        .find(|s| s.receipt.worktree_id == id)
        .expect("still listed after loss");
    assert!(!summary.present);
}

/// The crash window `create` exists to make discoverable: a `Provisional` row
/// written before `git worktree add` runs, with no matching directory on disk
/// (as if the process died in between the two registry writes). Retain-first
/// means this row is never quietly dropped or auto-finalized — this pins what
/// `list`/`lookup` actually do with it today, not any new recovery behavior.
#[test]
fn provisional_row_with_no_directory_is_visible_not_recreated() {
    let repo = TestRepo::init().expect("init");
    repo.writer()
        .commit_file("a.txt", "one", "first")
        .expect("commit");

    let base = tempfile::TempDir::new().expect("tempdir");
    let manager = manager_over(&repo, base.path());

    let id = manager.registry().mint_id().expect("mint id");
    let cwd = base.path().join("worktrees").join(id.as_str());
    let receipt = WorktreeReceipt {
        worktree_id: id.clone(),
        cwd: cwd.clone(),
        branch: BranchName::from_raw(format!("exomonad/worktree/provisional-{}", id.as_str())),
        source_head: GitOid::from_raw("f".repeat(40)),
        snapshot_ref: None,
        origin: WorktreeOrigin::CurrentRepository,
        source_repository: repo.path().to_path_buf(),
        created_at_ms: 0,
        status: WorktreeRecordStatus::Provisional,
    };
    manager
        .registry()
        .put(&receipt)
        .expect("put provisional row");
    assert!(
        !cwd.exists(),
        "the crash window: no directory was ever materialized"
    );

    // list(): the row is simply visible, with its recorded status intact —
    // retain-first means it stays, it is not hidden or auto-finalized.
    let listed = manager.list().expect("list");
    let summary = listed
        .iter()
        .find(|s| s.receipt.worktree_id == id)
        .expect("provisional row is listed like any other");
    assert!(!summary.present, "no directory backs it");
    assert_eq!(summary.receipt.status, WorktreeRecordStatus::Provisional);

    // Missing storage remains lost, even for a provisional row. Present but
    // provisional storage is separately refused as unfinished initialization.
    let err = manager
        .lookup(&id)
        .expect_err("no directory backs this row");
    assert!(matches!(&err, WorktreeError::WorktreeLost(lost) if lost == &id));
}

/// An unknown id and a LOST id are different failures and must stay
/// distinguishable. Collapsing them would tell an operator investigating a
/// vanished worktree that it never existed — hiding data loss behind a typo.
#[test]
fn an_unregistered_id_is_not_reported_as_a_lost_worktree() {
    let repo = TestRepo::init().expect("init");
    repo.writer()
        .commit_file("a.txt", "one", "first")
        .expect("commit");
    let base = tempfile::TempDir::new().expect("registry base");
    let manager = manager_over(&repo, base.path());

    // Never registered anywhere: lookup says "no such record", and seeding a
    // worktree from it names it as unregistered rather than lost.
    let unknown = WorktreeId::from_raw("wt-never-existed");
    assert!(
        manager
            .lookup(&unknown)
            .expect("lookup is not an error")
            .is_none(),
        "an unregistered id is Ok(None), not an error"
    );

    let err = manager
        .create(&WorktreeSpec::from_worktree(unknown.clone(), "child"))
        .expect_err("seeding from an unregistered id must fail");
    assert!(
        matches!(&err, WorktreeError::WorktreeNotRegistered(id) if id == &unknown),
        "expected WorktreeNotRegistered, got {err:?}"
    );

    // Registered then removed by hand: the OTHER failure, still reported as loss.
    let handle = manager
        .create(&WorktreeSpec::from_current_repository("real"))
        .expect("create");
    let lost_id = handle.id().clone();
    std::fs::remove_dir_all(handle.cwd()).expect("remove worktree dir by hand");

    let lost = manager
        .lookup(&lost_id)
        .expect_err("lost worktree must error");
    assert!(
        matches!(&lost, WorktreeError::WorktreeLost(id) if id == &lost_id),
        "a removed worktree is lost, not unregistered: {lost:?}"
    );
}

#[test]
fn list_never_fails_when_one_of_several_worktrees_is_lost() {
    let repo = TestRepo::init().expect("init");
    repo.writer()
        .commit_file("a.txt", "one", "first")
        .expect("commit");

    let base = tempfile::TempDir::new().expect("tempdir");
    let manager = manager_over(&repo, base.path());
    let survivor = manager
        .create(&WorktreeSpec::from_current_repository("survivor"))
        .expect("create survivor");
    let victim = manager
        .create(&WorktreeSpec::from_current_repository("victim"))
        .expect("create victim");

    std::fs::remove_dir_all(victim.cwd()).expect("remove victim by hand");

    let listed = manager.list().expect("list must not fail on a lost entry");
    let survivor_row = listed
        .iter()
        .find(|s| s.receipt.worktree_id == *survivor.id())
        .expect("survivor listed");
    let victim_row = listed
        .iter()
        .find(|s| s.receipt.worktree_id == *victim.id())
        .expect("victim listed");
    assert!(survivor_row.present);
    assert!(!victim_row.present);
}

#[test]
fn dirty_summary_classifies_staged_unstaged_untracked_and_counts_ignored() {
    let repo = TestRepo::init().expect("init");
    let w = repo.writer();
    w.commit_file("tracked.txt", "one", "first")
        .expect("commit");
    w.write_file("tracked.txt", "changed").expect("write");
    w.write_file("staged.txt", "new").expect("write");
    w.stage("staged.txt").expect("stage");
    w.write_file("untracked.txt", "new").expect("write");
    w.write_file(".gitignore", "ignored.txt\n").expect("write");
    w.stage(".gitignore").expect("stage");
    w.write_file("ignored.txt", "ignored").expect("write");

    let git = GitCli::new();
    let summary = inspect::dirty_summary(&git, repo.path()).expect("dirty_summary");

    assert_eq!(summary.unstaged, vec!["tracked.txt".to_string()]);
    assert_eq!(
        summary.staged,
        vec![".gitignore".to_string(), "staged.txt".to_string()]
    );
    assert_eq!(summary.untracked, vec!["untracked.txt".to_string()]);
    assert_eq!(summary.ignored_excluded, 1);
    assert!(!summary.is_clean());

    let summary_again = inspect::dirty_summary(&git, repo.path()).expect("dirty_summary again");
    assert_eq!(
        summary, summary_again,
        "two reads of the same state must compare equal"
    );
}

#[test]
fn dirty_summary_is_empty_and_sorted_on_a_clean_repository() {
    let repo = TestRepo::init().expect("init");
    let w = repo.writer();
    w.commit_file("z.txt", "one", "first").expect("commit");
    w.commit_file("a.txt", "two", "second").expect("commit");

    let git = GitCli::new();
    let summary = inspect::dirty_summary(&git, repo.path()).expect("dirty_summary");
    assert!(summary.is_clean());
    assert_eq!(summary.ignored_excluded, 0);
}

#[test]
fn binding_refuses_second_agent_and_permits_rebind_after_settle() {
    let base = tempfile::TempDir::new().expect("tempdir");
    let mut table = BindingTable::open(base.path().join("bindings")).expect("open bindings");

    let worktree = WorktreeId::from_raw("wt-test");
    let agent_a = AgentRef::from_raw("agent-a");
    let agent_b = AgentRef::from_raw("agent-b");

    let lease = table
        .bind(&worktree, &agent_a, 1000)
        .expect("first bind succeeds");

    let err = table
        .bind(&worktree, &agent_b, 2000)
        .expect_err("second bind must fail while the first is active");
    match err {
        WorktreeError::WorktreeBusy {
            worktree: w,
            holder,
        } => {
            assert_eq!(w, worktree);
            assert_eq!(holder, "agent-a");
        }
        other => panic!("expected WorktreeBusy, got {other:?}"),
    }

    lease.complete(&mut table).expect("settle");
    table
        .bind(&worktree, &agent_b, 3000)
        .expect("rebind succeeds once the previous binding is settled");

    assert_eq!(table.current(&worktree).expect("current").agent(), &agent_b);

    // Durability: a fresh table over the same root sees the current binding.
    // The first owner must be gone first — the table is SINGLE-OWNER (a live
    // second owner is refused), so a restart is drop-then-reopen.
    drop(table);
    let fresh = BindingTable::open(base.path().join("bindings")).expect("reopen bindings");
    assert_eq!(fresh.current(&worktree).expect("current").agent(), &agent_b);
}

#[test]
fn settling_a_released_binding_also_permits_rebind() {
    let base = tempfile::TempDir::new().expect("tempdir");
    let mut table = BindingTable::open(base.path().join("bindings")).expect("open bindings");

    let worktree = WorktreeId::from_raw("wt-release-test");
    let agent_a = AgentRef::from_raw("agent-a");
    let agent_b = AgentRef::from_raw("agent-b");

    let lease = table.bind(&worktree, &agent_a, 1000).expect("bind");
    lease.release(&mut table).expect("release");
    table
        .bind(&worktree, &agent_b, 2000)
        .expect("rebind after release succeeds");
    assert_eq!(table.current(&worktree).expect("current").agent(), &agent_b);
}

/// The never-dirty-the-source invariant, caught at the one moment it can still
/// be prevented. Typed rather than a panic so a resident can catch it and fall
/// back to a correct root instead of dying on a misconfiguration.
#[test]
fn registry_open_refuses_a_root_inside_a_working_tree() {
    let repo = TestRepo::init().expect("init");
    let nested = repo.path().join("nested-registry");

    match WorktreeRegistry::open(&nested) {
        Err(WorktreeError::InvalidRegistryRoot { root, inside }) => {
            assert!(
                root.ends_with("nested-registry"),
                "the refusal names the offending root, not some ancestor: {}",
                root.display()
            );
            assert!(
                nested.starts_with(&inside),
                "the refusal names the working tree it is inside: {}",
                inside.display()
            );
        }
        other => panic!("expected InvalidRegistryRoot, got {other:?}"),
    }
}

/// A git failure that is NOT "no repository here" — a corrupt `.git` — must
/// REFUSE the nesting check, not silently read it as "safe, nothing to nest
/// inside". Fail-open here would let the never-dirty-the-source invariant
/// skip itself on any git malfunction, not just a genuine absence of a
/// repository.
#[test]
fn registry_open_refuses_a_root_it_cannot_even_check_for_nesting() {
    let base = tempfile::TempDir::new().expect("tempdir");
    let root = base.path().join("corrupt-git-root");
    std::fs::create_dir_all(&root).expect("mkdir");
    // A `.git` that exists but is not a valid gitfile pointer or repository
    // directory — `git rev-parse` fails with "invalid gitfile format", not
    // with "not a git repository", so this must NOT be read as "clean, no
    // nesting" the way a genuine non-repository is.
    std::fs::write(root.join(".git"), "not a real gitfile pointer\n").expect("write garbage .git");

    match WorktreeRegistry::open(&root) {
        Err(WorktreeError::GitFailure(receipt)) => {
            assert!(
                receipt.cwd.ends_with("corrupt-git-root"),
                "the refusal names the offending path: {}",
                receipt.cwd.display()
            );
        }
        other => panic!("expected GitFailure naming the corrupt path, got {other:?}"),
    }
}

/// The binding table enforces isolation from IN-MEMORY rows, so exactly one
/// live table may own a root — a second owner could see "unbound" and bind
/// the same worktree to a second agent. A second `open` (same process or, via
/// the same flock, another process) must refuse loudly; releasing the first
/// owner frees the root.
#[test]
fn binding_table_refuses_a_second_live_owner_over_one_root() {
    use exomonad_worktree::BindingTable;
    let base = tempfile::TempDir::new().expect("tempdir");
    let root = base.path().join("bindings");

    let first = BindingTable::open(&root).expect("first owner opens");
    let second = BindingTable::open(&root);
    assert!(
        matches!(second, Err(WorktreeError::StorageFailure { .. })),
        "a second live owner must be refused, got {second:?}"
    );

    drop(first);
    BindingTable::open(&root).expect("the root is free once the owner drops");
}

#[test]
fn binding_table_waits_for_release_without_stealing_live_ownership() {
    use exomonad_worktree::BindingTable;
    use std::time::Duration;
    let base = tempfile::TempDir::new().unwrap();
    let root = base.path().join("bindings");
    let first = BindingTable::open(&root).unwrap();
    assert!(BindingTable::open_with_timeout(&root, Duration::from_millis(20)).is_err());
    let release = std::thread::spawn(move || {
        #[allow(clippy::disallowed_methods, reason = "sync test thread, not async")]
        std::thread::sleep(Duration::from_millis(50));
        drop(first);
    });
    let second = BindingTable::open_with_timeout(&root, Duration::from_secs(2)).unwrap();
    release.join().unwrap();
    assert!(BindingTable::open(&root).is_err());
    drop(second);
    BindingTable::open(&root).unwrap();
}
