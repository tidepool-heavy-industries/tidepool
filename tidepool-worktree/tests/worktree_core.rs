//! LANE L1 acceptance tests — durable registry, clean worktree creation,
//! restart lookup, and the one-worktree-one-agent binding state machine.
//!
//! Every test drives a REAL temporary git repository via
//! [`tidepool_worktree::testing::TestRepo`] and [`ScriptedWriter`]. There is
//! no mock of git anywhere.

use std::collections::BTreeMap;
use std::path::Path;

use tidepool_worktree::git::inspect;
use tidepool_worktree::testing::{fingerprint, TestRepo};
use tidepool_worktree::{
    AgentRef, BindingTable, BranchName, DirtySummary, GitCli, GitOid, GitRef, InProgressKind,
    WorktreeError, WorktreeId, WorktreeManager, WorktreeOrigin, WorktreeReceipt,
    WorktreeRecordStatus, WorktreeRegistry, WorktreeSpec,
};

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

    assert!(handle.branch().as_str().starts_with("tidepool/worktree/"));
    assert_eq!(handle.source_head().as_str(), before.head);
    assert!(handle.cwd().exists());
    assert!(!handle.cwd().starts_with(repo.path()));
    assert!(handle.cwd().join("a.txt").exists());
    assert!(matches!(
        handle.receipt().origin,
        WorktreeOrigin::CurrentRepository
    ));
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

/// Regression for the from-worktree provenance bug: a worktree created from
/// another managed worktree must record `source_repository` as the resolved
/// underlying repository root, never the parent worktree's checkout path —
/// including through a chain of from-worktree creations. A wrong value here
/// sends downstream consumers (e.g. the sandbox writable-root derivation)
/// down `<checkout>/.git`, which is a gitdir link file, not a directory.
#[test]
fn from_worktree_creation_records_the_repository_root_not_the_parent_checkout() {
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
    assert_eq!(
        b.receipt().source_repository,
        repo.path(),
        "b must record the repository root, not a's checkout path {}",
        a.cwd().display()
    );
    assert_ne!(b.receipt().source_repository, a.cwd());

    // Chained: c from b must resolve the same repository root, not b's
    // checkout path either.
    let c = manager
        .create(&WorktreeSpec::from_worktree(b.id().clone(), "c"))
        .expect("create c from b");
    assert_eq!(
        c.receipt().source_repository,
        repo.path(),
        "c must record the repository root through the chain, not b's checkout path {}",
        b.cwd().display()
    );
    assert_ne!(c.receipt().source_repository, b.cwd());
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
        branch: BranchName::from_raw(format!("tidepool/worktree/provisional-{}", id.as_str())),
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

    // lookup(): today's lookup only checks directory presence, not status —
    // so an unfinalized row with no directory reports exactly the same
    // WorktreeLost a hand-deleted FINALIZED row would (see
    // `hand_deleted_worktree_is_lost_not_recreated` above).
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

/// The binding table enforces isolation from IN-MEMORY rows, so exactly one
/// live table may own a root — a second owner could see "unbound" and bind
/// the same worktree to a second agent. A second `open` (same process or, via
/// the same flock, another process) must refuse loudly; releasing the first
/// owner frees the root.
#[test]
fn binding_table_refuses_a_second_live_owner_over_one_root() {
    use tidepool_worktree::BindingTable;
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
