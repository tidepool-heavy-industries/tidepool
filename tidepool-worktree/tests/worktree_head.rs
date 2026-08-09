//! LANE L7 acceptance tests — `WorktreeManager::worktree_head`, the fresh-read
//! HEAD lookup PRD 19's rewrite added to the public surface.
//!
//! Every test drives a REAL temporary git repository via
//! [`tidepool_worktree::testing::TestRepo`] and [`ScriptedWriter`]. There is
//! no mock of git anywhere.
//!
//! The whole point of this verb is that it is a FRESH read, never a cached
//! one — not [`tidepool_worktree::WorktreeHandle::source_head`] (the frozen
//! seed commit) and not anything a [`tidepool_worktree::WorktreeMonitor`]
//! last reconciled. Every test here is built to fail an implementation that
//! quietly returns one of those instead.

use std::path::Path;

use tidepool_worktree::testing::TestRepo;
use tidepool_worktree::{
    EventJournal, GitCli, WorktreeError, WorktreeManager, WorktreeRegistry, WorktreeSpec,
};

fn manager_over(repo: &TestRepo, base: &Path) -> WorktreeManager {
    let registry = WorktreeRegistry::open(base.join("registry")).expect("open registry");
    WorktreeManager::new(GitCli::new(), registry, base.join("worktrees"), repo.path())
}

/// The load-bearing test: `worktreeHead` must be a fresh read, not
/// `source_head` under another name. An implementation that just returned
/// `handle.source_head()` would pass every other test in this file and fail
/// only this one.
#[test]
fn worktree_head_is_a_fresh_read_distinct_from_source_head() {
    let repo = TestRepo::init().expect("init");
    repo.writer()
        .commit_file("a.txt", "one", "first")
        .expect("commit");

    let base = tempfile::TempDir::new().expect("tempdir");
    let manager = manager_over(&repo, base.path());

    let handle = manager
        .create(&WorktreeSpec::from_current_repository("child"))
        .expect("create");
    let seed = handle.source_head().clone();

    assert_eq!(
        manager.worktree_head(&handle).expect("worktree_head"),
        seed,
        "before any writer activity, worktree_head must agree with the seed"
    );

    // The scripted writer commits INSIDE the managed worktree, exactly as a
    // coding agent would — bypassing the manager and the registry entirely.
    let child_writer = repo.writer_at(handle.cwd());
    let new_head = child_writer
        .commit_file("b.txt", "child work", "child commit")
        .expect("commit inside the managed worktree");
    assert_ne!(new_head, seed, "the writer must have actually moved HEAD");

    assert_eq!(
        manager
            .worktree_head(&handle)
            .expect("worktree_head after commit"),
        new_head,
        "worktree_head must observe the commit made after creation"
    );
    assert_eq!(
        handle.source_head(),
        &seed,
        "source_head is the frozen seed and must never move"
    );
}

/// Repeated commits: each fresh read tracks the latest tip, proving this is
/// not a value cached at the first call either.
#[test]
fn worktree_head_tracks_successive_commits() {
    let repo = TestRepo::init().expect("init");
    repo.writer()
        .commit_file("a.txt", "one", "first")
        .expect("commit");

    let base = tempfile::TempDir::new().expect("tempdir");
    let manager = manager_over(&repo, base.path());
    let handle = manager
        .create(&WorktreeSpec::from_current_repository("child"))
        .expect("create");

    let child_writer = repo.writer_at(handle.cwd());
    let first = child_writer
        .commit_file("b.txt", "one", "second")
        .expect("commit");
    assert_eq!(manager.worktree_head(&handle).expect("read 1"), first);

    let second = child_writer
        .commit_file("c.txt", "two", "third")
        .expect("commit");
    assert_eq!(manager.worktree_head(&handle).expect("read 2"), second);
    assert_ne!(first, second);
}

/// Closes the exact gap the verb exists for: a resident spanning cycles
/// compares `worktreeHead` against its own checkpoint because the event
/// monitor's subscriptions do not replay and do not survive a cycle
/// boundary. Here the monitor never reconciles this worktree at all — the
/// journal for it stays empty — yet worktree_head still sees the movement.
#[test]
fn worktree_head_reflects_movement_the_monitor_never_reconciled() {
    let repo = TestRepo::init().expect("init");
    repo.writer()
        .commit_file("a.txt", "one", "first")
        .expect("commit");

    let base = tempfile::TempDir::new().expect("tempdir");
    let manager = manager_over(&repo, base.path());
    let handle = manager
        .create(&WorktreeSpec::from_current_repository("child"))
        .expect("create");

    // A monitor is registered (establishing a baseline) but `reconcile` is
    // deliberately never called — the exact window between one resident
    // cycle unregistering its handlers and the next re-registering them.
    let journal_dir = tempfile::TempDir::new().expect("journal tempdir");
    let journal = EventJournal::open(journal_dir.path().join("events.jsonl")).expect("journal");
    let mut monitor = tidepool_worktree::WorktreeMonitor::new(GitCli::new(), journal);
    monitor
        .register(handle.id().clone(), handle.cwd().to_path_buf())
        .expect("register");

    let child_writer = repo.writer_at(handle.cwd());
    let new_head = child_writer
        .commit_file("b.txt", "child work", "child commit")
        .expect("commit inside the managed worktree, unobserved by the monitor");

    assert_eq!(
        manager.worktree_head(&handle).expect("worktree_head"),
        new_head,
        "worktree_head must see HEAD movement the monitor never reconciled"
    );

    let unread_journal =
        EventJournal::open(journal_dir.path().join("events.jsonl")).expect("reopen journal");
    assert!(
        unread_journal.since(0).expect("since").is_empty(),
        "the monitor never reconciled, so nothing was journalled — worktree_head's answer \
         did not come from the journal either"
    );
}

/// A detached HEAD must still resolve to the commit, not fail because there
/// is no symbolic ref to follow.
#[test]
fn worktree_head_on_detached_head_returns_the_commit() {
    let repo = TestRepo::init().expect("init");
    let w = repo.writer();
    w.commit_file("a.txt", "one", "first").expect("commit");

    let base = tempfile::TempDir::new().expect("tempdir");
    let manager = manager_over(&repo, base.path());
    let handle = manager
        .create(&WorktreeSpec::from_current_repository("child"))
        .expect("create");

    let child_writer = repo.writer_at(handle.cwd());
    let detached_at = child_writer
        .commit_file("b.txt", "two", "second")
        .expect("commit");
    repo.git()
        .try_run(handle.cwd(), &["checkout", "-q", "--detach", "HEAD"])
        .expect("detach HEAD");
    assert_eq!(
        child_writer.current_branch().expect("current_branch"),
        None,
        "must actually be detached for this test to mean anything"
    );

    assert_eq!(
        manager.worktree_head(&handle).expect("worktree_head"),
        detached_at
    );
}

/// A worktree removed from disk after the handle was obtained fails the same
/// way `lookup` fails: `WorktreeError::WorktreeLost`, never a new variant.
#[test]
fn worktree_head_of_a_lost_worktree_fails_consistently_with_lookup() {
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

    std::fs::remove_dir_all(handle.cwd()).expect("remove worktree dir by hand");

    let err = manager
        .worktree_head(&handle)
        .expect_err("a lost worktree must fail");
    assert!(
        matches!(&err, WorktreeError::WorktreeLost(lost) if lost == &id),
        "expected WorktreeLost, got {err:?}"
    );

    let lookup_err = manager
        .lookup(&id)
        .expect_err("lookup must also see it as lost");
    assert!(matches!(lookup_err, WorktreeError::WorktreeLost(lost) if lost == id));
}
