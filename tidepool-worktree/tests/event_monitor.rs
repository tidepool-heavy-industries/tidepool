//! Acceptance tests for poll/reconciliation, coalesced deltas, and honest
//! classification, the no-replay journal) — all against real temporary git
//! repositories via [`TestRepo`] + [`ScriptedWriter`], never a mock of git.
//!
//! These tests do not go through `WorktreeRegistry`/`WorktreeManager`:
//! `WorktreeMonitor::register` only needs a [`WorktreeId`] and a path; most
//! tests supply both directly, pointing at a
//! [`TestRepo`]'s own working tree (the monitor treats any git working
//! directory uniformly — nothing here depends on it being a *linked*
//! worktree). One test (`real_git_worktree_add_is_monitored_directly`) uses an
//! actual `git worktree add` to confirm that assumption holds for a linked
//! worktree too.

use std::path::PathBuf;

use tidepool_worktree::testing::TestRepo;
use tidepool_worktree::{
    EventId, EventJournal, GitCli, GitOid, HeadChangeKind, HeadChangeReceipt, Observed,
    RepositoryEvent, WorktreeError, WorktreeId, WorktreeMonitor,
};

/// A fresh journal path inside its own temp dir, and a monitor over it.
fn open_monitor() -> (WorktreeMonitor, PathBuf, tempfile::TempDir) {
    let journal_dir = tempfile::TempDir::new().expect("create journal temp dir");
    let journal_path = journal_dir.path().join("events.jsonl");
    let journal = EventJournal::open(&journal_path).expect("open journal");
    (
        WorktreeMonitor::new(GitCli::new(), journal),
        journal_path,
        journal_dir,
    )
}

fn wt(raw: &str) -> WorktreeId {
    WorktreeId::from_raw(raw)
}

fn head_changed(
    events: &[Observed<RepositoryEvent>],
) -> Vec<&tidepool_worktree::HeadChangeReceipt> {
    events
        .iter()
        .filter_map(|e| match &e.value {
            RepositoryEvent::HeadChanged(r) => Some(r),
            RepositoryEvent::Commit(_) => None,
        })
        .collect()
}

fn commits(events: &[Observed<RepositoryEvent>]) -> Vec<&tidepool_worktree::CommitReceipt> {
    events
        .iter()
        .filter_map(|e| match &e.value {
            RepositoryEvent::Commit(r) => Some(r),
            RepositoryEvent::HeadChanged(_) => None,
        })
        .collect()
}

#[test]
fn first_reconcile_after_register_establishes_baseline_without_emitting() {
    let repo = TestRepo::init().expect("init");
    let w = repo.writer();
    w.commit_file("a.txt", "one", "first").expect("commit");

    let (mut monitor, _journal_path, _tmp) = open_monitor();
    let id = wt("w1");
    monitor
        .register(id.clone(), repo.path().to_path_buf())
        .expect("register");

    // No writer activity between register() and this first reconcile: there is
    // no prior state for the worktree to have moved FROM, so nothing is
    // reported. This is the documented design decision for "first observation
    // of a worktree with no recorded baseline" — see monitor.rs module docs.
    let events = monitor.reconcile(&id).expect("reconcile");
    assert!(
        events.is_empty(),
        "priming reconcile must emit nothing, got {events:?}"
    );
}

#[test]
fn commit_yields_commit_and_head_changed_sharing_one_event_id() {
    let repo = TestRepo::init().expect("init");
    let w = repo.writer();
    w.commit_file("a.txt", "one", "first").expect("commit");

    let (mut monitor, _journal_path, _tmp) = open_monitor();
    let id = wt("w1");
    monitor
        .register(id.clone(), repo.path().to_path_buf())
        .expect("register");
    monitor.reconcile(&id).expect("priming reconcile");

    let new_head = w.commit_file("b.txt", "two", "second").expect("commit");
    let events = monitor.reconcile(&id).expect("reconcile");

    assert_eq!(
        events.len(),
        2,
        "expected one Commit and one HeadChanged: {events:?}"
    );
    let cs = commits(&events);
    let hcs = head_changed(&events);
    assert_eq!(cs.len(), 1);
    assert_eq!(hcs.len(), 1);
    assert_eq!(cs[0].oid, new_head);
    assert_eq!(cs[0].subject, "second");
    assert_eq!(
        hcs[0].kind,
        HeadChangeKind::Advanced(vec![new_head.clone()])
    );
    assert_eq!(hcs[0].new_head, new_head);
    assert!(
        matches!(events[0].value, RepositoryEvent::Commit(_)),
        "observation order: Commit before HeadChanged, got {events:?}"
    );
    assert_eq!(
        events[0].event_id, events[1].event_id,
        "the Observed wrapper reconcile now returns must itself carry one shared EventId \
         across co-emitted views, not just the journal rows behind them"
    );

    // Event id sharing, cross-checked against the journal (not just the
    // returned Observed values above) — the journal is what a restart
    // diagnosis reads, so it must agree with what the caller was handed.
    let entries = EventJournal::open(&_journal_path)
        .expect("reopen journal")
        .since(0);
    let ids: Vec<_> = entries
        .iter()
        .filter(|e| e.event.worktree() == &id)
        .map(|e| e.event_id)
        .collect();
    // Two entries from the priming-less second reconcile (priming emitted
    // nothing, so all recorded rows belong to this pass).
    assert_eq!(
        ids.len(),
        2,
        "both observations should be journalled: {ids:?}"
    );
    assert_eq!(
        ids[0], ids[1],
        "Commit and HeadChanged must share one EventId"
    );
    assert_eq!(
        events[0].event_id, ids[0],
        "the id reconcile returned must be the id the journal actually recorded"
    );
}

#[test]
fn two_commits_between_polls_coalesce_into_one_advanced() {
    let repo = TestRepo::init().expect("init");
    let w = repo.writer();
    let baseline_head = w.commit_file("a.txt", "one", "first").expect("commit");

    let (mut monitor, _journal_path, _tmp) = open_monitor();
    let id = wt("w1");
    monitor
        .register(id.clone(), repo.path().to_path_buf())
        .expect("register");
    monitor.reconcile(&id).expect("priming reconcile");

    let h1 = w.commit_file("b.txt", "two", "second").expect("commit");
    let h2 = w.commit_file("c.txt", "three", "third").expect("commit");

    let events = monitor.reconcile(&id).expect("reconcile");
    let hcs = head_changed(&events);
    assert_eq!(
        hcs.len(),
        1,
        "must coalesce into ONE HeadChanged, not two: {events:?}"
    );
    assert_eq!(
        hcs[0].kind,
        HeadChangeKind::Advanced(vec![h1.clone(), h2.clone()])
    );
    assert_eq!(hcs[0].old_head, Some(baseline_head));
    assert_eq!(hcs[0].new_head, h2);

    // Each real, honestly-inferable commit is still individually reported —
    // that is the "commit" signal's job (review/test/receipt reactions), and
    // is orthogonal to headChanged staying a single coalesced delta.
    let cs = commits(&events);
    assert_eq!(
        cs.len(),
        2,
        "expected one Commit per gained commit: {events:?}"
    );
    assert_eq!(cs[0].oid, h1);
    assert_eq!(cs[1].oid, h2);
}

#[test]
fn amend_yields_amended_and_a_commit_for_the_new_tip() {
    let repo = TestRepo::init().expect("init");
    let w = repo.writer();
    w.commit_file("a.txt", "one", "first").expect("commit");
    let old = w.commit_file("b.txt", "two", "second").expect("commit");

    let (mut monitor, _journal_path, _tmp) = open_monitor();
    let id = wt("w1");
    monitor
        .register(id.clone(), repo.path().to_path_buf())
        .expect("register");
    monitor.reconcile(&id).expect("priming reconcile");

    let new = w.amend("second, reworded").expect("amend");
    let events = monitor.reconcile(&id).expect("reconcile");

    let hcs = head_changed(&events);
    assert_eq!(hcs.len(), 1);
    assert_eq!(hcs[0].kind, HeadChangeKind::Amended(old, new.clone()));

    let cs = commits(&events);
    assert_eq!(
        cs.len(),
        1,
        "amend replaces the tip with a real commit object"
    );
    assert_eq!(cs[0].oid, new);
    assert_eq!(cs[0].subject, "second, reworded");
}

#[test]
fn reset_hard_backwards_yields_rewound_with_no_commit() {
    let repo = TestRepo::init().expect("init");
    let w = repo.writer();
    let a = w.commit_file("a.txt", "one", "first").expect("commit");
    w.commit_file("b.txt", "two", "second").expect("commit");

    let (mut monitor, _journal_path, _tmp) = open_monitor();
    let id = wt("w1");
    monitor
        .register(id.clone(), repo.path().to_path_buf())
        .expect("register");
    monitor.reconcile(&id).expect("priming reconcile");

    w.reset_hard(a.as_str()).expect("reset");
    let events = monitor.reconcile(&id).expect("reconcile");

    let hcs = head_changed(&events);
    assert_eq!(hcs.len(), 1);
    assert_eq!(hcs[0].kind, HeadChangeKind::Rewound);
    assert_eq!(hcs[0].new_head, a);
    assert!(
        commits(&events).is_empty(),
        "reset creates no new commit object"
    );
}

#[test]
fn branch_checkout_yields_switched_with_no_commit() {
    let repo = TestRepo::init().expect("init");
    let w = repo.writer();
    w.commit_file("a.txt", "one", "first").expect("commit");

    let (mut monitor, _journal_path, _tmp) = open_monitor();
    let id = wt("w1");
    monitor
        .register(id.clone(), repo.path().to_path_buf())
        .expect("register");
    monitor.reconcile(&id).expect("priming reconcile");

    w.checkout_new_branch("side").expect("checkout -b side");
    let events = monitor.reconcile(&id).expect("reconcile");

    let hcs = head_changed(&events);
    assert_eq!(hcs.len(), 1);
    assert_eq!(hcs[0].kind, HeadChangeKind::Switched);
    assert_eq!(hcs[0].branch.as_ref().map(|b| b.as_str()), Some("side"));
    assert!(
        commits(&events).is_empty(),
        "a branch switch creates no new commit"
    );
}

#[test]
fn rebase_onto_yields_rewritten_with_matched_pairs() {
    let repo = TestRepo::init().expect("init");
    let w = repo.writer();
    w.commit_file("root.txt", "root", "root").expect("commit");
    w.checkout_new_branch("feature").expect("branch feature");
    let c = w.commit_file("c.txt", "c", "c").expect("commit c");
    let d = w.commit_file("d.txt", "d", "d").expect("commit d");

    w.checkout("main").expect("back to main");
    w.commit_file("e.txt", "e", "e").expect("commit e on main");

    w.checkout("feature").expect("back to feature");

    let (mut monitor, _journal_path, _tmp) = open_monitor();
    let id = wt("w1");
    monitor
        .register(id.clone(), repo.path().to_path_buf())
        .expect("register");
    monitor.reconcile(&id).expect("priming reconcile");

    w.rebase_onto("main").expect("rebase feature onto main");
    let events = monitor.reconcile(&id).expect("reconcile");

    let hcs = head_changed(&events);
    assert_eq!(hcs.len(), 1);
    match &hcs[0].kind {
        HeadChangeKind::Rewritten(pairs) => {
            assert_eq!(pairs.len(), 2, "expected c and d to be matched: {pairs:?}");
            assert_eq!(pairs[0].0, c);
            assert_eq!(pairs[1].0, d);
            assert_ne!(pairs[0].1, c, "the rebased commit must be a new oid");
            assert_ne!(pairs[1].1, d, "the rebased commit must be a new oid");
        }
        other => panic!("expected Rewritten, got {other:?}"),
    }
    assert!(
        commits(&events).is_empty(),
        "a rebase's synthetic commits are not reported as `commit` observations"
    );
}

#[test]
fn unrelated_history_on_the_same_branch_yields_unknown_change_rather_than_a_guess() {
    // A branch checkout would be honestly classified `Switched`, so this
    // scenario keeps the branch name fixed ("main" throughout) and instead
    // moves the SAME branch ref to a commit with no shared ancestry at all —
    // e.g. `reset --hard` onto an object built on an orphan branch elsewhere
    // in the repo. Neither direction of `merge-base --is-ancestor` holds and
    // the subjects share nothing to match, so classification must degrade
    // rather than invent an `Advanced`/`Rewound`/`Rewritten`.
    let repo = TestRepo::init().expect("init");
    let w = repo.writer();
    w.commit_file("a.txt", "one", "first").expect("commit");
    let second = w.commit_file("b.txt", "two", "second").expect("commit");

    repo.git()
        .try_run(repo.path(), &["checkout", "-q", "--orphan", "unrelated"])
        .expect("checkout --orphan");
    let unrelated = w
        .commit_file("z.txt", "z", "totally unrelated")
        .expect("commit on orphan branch");
    repo.git()
        .try_run(repo.path(), &["checkout", "-q", "main"])
        .expect("back to main");
    assert_eq!(
        w.head().expect("head"),
        second,
        "main is unaffected by the orphan branch"
    );

    let (mut monitor, _journal_path, _tmp) = open_monitor();
    let id = wt("w1");
    monitor
        .register(id.clone(), repo.path().to_path_buf())
        .expect("register");
    monitor.reconcile(&id).expect("priming reconcile");

    w.reset_hard(unrelated.as_str())
        .expect("reset main onto the unrelated commit");
    let events = monitor.reconcile(&id).expect("reconcile");
    let hcs = head_changed(&events);
    assert_eq!(hcs.len(), 1);
    assert_eq!(hcs[0].branch.as_ref().map(|b| b.as_str()), Some("main"));
    assert_eq!(
        hcs[0].kind,
        HeadChangeKind::UnknownChange,
        "disjoint histories with no shared ancestry or matching subjects must not be guessed at"
    );
    assert!(commits(&events).is_empty());
}

#[test]
fn unreachable_old_head_after_gc_yields_unknown_change() {
    // The trap the classifier must not fall into: after the old head is
    // garbage-collected, `merge-base --is-ancestor` cannot even resolve it.
    // That is exactly an UnknownChange, not a false `No` that lets
    // classification continue to a wrong guess.
    let repo = TestRepo::init().expect("init");
    let w = repo.writer();
    w.commit_file("a.txt", "one", "first").expect("commit");
    w.commit_file("b.txt", "two", "second").expect("commit");

    let (mut monitor, _journal_path, _tmp) = open_monitor();
    let id = wt("w1");
    monitor
        .register(id.clone(), repo.path().to_path_buf())
        .expect("register");
    monitor.reconcile(&id).expect("priming reconcile");

    w.amend("second, reworded").expect("amend");
    repo.git()
        .try_run(repo.path(), &["reflog", "expire", "--expire=now", "--all"])
        .expect("reflog expire");
    repo.git()
        .try_run(repo.path(), &["gc", "--prune=now", "--quiet"])
        .expect("gc --prune=now");

    let events = monitor.reconcile(&id).expect("reconcile");
    let hcs = head_changed(&events);
    assert_eq!(hcs.len(), 1);
    assert_eq!(hcs[0].kind, HeadChangeKind::UnknownChange);
}

#[test]
fn reconciling_with_no_writer_activity_is_idempotent() {
    let repo = TestRepo::init().expect("init");
    let w = repo.writer();
    w.commit_file("a.txt", "one", "first").expect("commit");

    let (mut monitor, _journal_path, _tmp) = open_monitor();
    let id = wt("w1");
    monitor
        .register(id.clone(), repo.path().to_path_buf())
        .expect("register");
    assert!(monitor.reconcile(&id).expect("first reconcile").is_empty());

    w.commit_file("b.txt", "two", "second").expect("commit");
    let events = monitor.reconcile(&id).expect("reconcile after commit");
    assert!(!events.is_empty());

    // No writer activity since — reconciling again must yield nothing.
    let again = monitor.reconcile(&id).expect("reconcile again");
    assert!(
        again.is_empty(),
        "idempotent reconcile must yield nothing: {again:?}"
    );
    let again2 = monitor.reconcile(&id).expect("reconcile a third time");
    assert!(again2.is_empty());
}

#[test]
fn fresh_subscription_sees_none_of_the_prior_rows() {
    let repo = TestRepo::init().expect("init");
    let w = repo.writer();
    w.commit_file("a.txt", "one", "first").expect("commit");

    let (mut monitor, journal_path, _tmp) = open_monitor();
    let id = wt("w1");
    monitor
        .register(id.clone(), repo.path().to_path_buf())
        .expect("register");
    monitor.reconcile(&id).expect("priming reconcile");

    w.commit_file("b.txt", "two", "second").expect("commit");
    monitor.reconcile(&id).expect("reconcile 1").len();

    // A handler subscribes NOW: it stores the journal's current end.
    let subscribed_at = EventJournal::open(&journal_path)
        .expect("reopen journal")
        .end_cursor();

    w.commit_file("c.txt", "three", "third").expect("commit");
    let later_events = monitor.reconcile(&id).expect("reconcile 2");
    assert!(!later_events.is_empty());

    let visible = EventJournal::open(&journal_path)
        .expect("reopen journal")
        .since(subscribed_at);

    assert_eq!(
        visible.len(),
        later_events.len(),
        "a fresh subscription must see only rows written after it registered"
    );
    for entry in &visible {
        if let RepositoryEvent::Commit(c) = &entry.event {
            assert_ne!(
                c.subject, "second",
                "the pre-subscription commit must not replay"
            );
        }
    }
}

#[test]
fn journal_survives_restart_and_skips_a_torn_final_row() {
    let repo = TestRepo::init().expect("init");
    let w = repo.writer();
    w.commit_file("a.txt", "one", "first").expect("commit");

    let (mut monitor, journal_path, _tmp) = open_monitor();
    let id = wt("w1");
    monitor
        .register(id.clone(), repo.path().to_path_buf())
        .expect("register");
    monitor.reconcile(&id).expect("priming reconcile");

    w.commit_file("b.txt", "two", "second").expect("commit");
    let events = monitor.reconcile(&id).expect("reconcile");
    assert!(!events.is_empty());

    let before_restart = EventJournal::open(&journal_path)
        .expect("reopen journal")
        .since(0);
    assert_eq!(before_restart.len(), events.len());

    // Simulate a crash mid-write: append a syntactically-broken trailing line
    // directly, bypassing EventJournal::append.
    {
        use std::io::Write;
        let mut file = std::fs::OpenOptions::new()
            .append(true)
            .open(&journal_path)
            .expect("open journal for corruption");
        write!(file, "{{\"cursor\":999,\"event_id\"").expect("write torn row");
        // No trailing newline / closing brace: a torn write.
    }

    let reopened = EventJournal::open(&journal_path).expect("reopen journal after tear");
    let recovered = reopened.since(0);
    assert_eq!(
        recovered.len(),
        before_restart.len(),
        "the torn final row must be skipped, not counted, and the earlier rows must survive"
    );
    assert_eq!(recovered, before_restart);
}

/// Mirrors `tidepool-atomic-write`'s `concurrent_writers_never_observe_a_torn_file`:
/// several independent writers appending to the SAME journal file at once.
/// Each writer opens its own [`EventJournal`] handle (never a shared,
/// in-process-mutex-serialized one) so this actually exercises the O_APPEND
/// write path across independent file descriptors — the same shape as
/// separate processes appending, which is the guarantee `EventJournal::append`
/// documents relying on.
#[test]
fn concurrent_appends_never_produce_a_torn_row() {
    let journal_dir = tempfile::TempDir::new().expect("create journal temp dir");
    let journal_path = journal_dir.path().join("events.jsonl");
    // Create the file up front so every writer thread opens an existing path.
    EventJournal::open(&journal_path).expect("open journal");

    const WRITERS: u64 = 8;
    const ROWS_PER_WRITER: u64 = 25;

    let handles: Vec<_> = (0..WRITERS)
        .map(|w| {
            let journal_path = journal_path.clone();
            std::thread::spawn(move || {
                let mut journal = EventJournal::open(&journal_path).expect("open journal");
                for row in 0..ROWS_PER_WRITER {
                    let event = RepositoryEvent::HeadChanged(HeadChangeReceipt {
                        worktree: wt(&format!("writer-{w}")),
                        old_head: None,
                        new_head: GitOid::from_raw(format!("oid-{w}-{row}")),
                        kind: HeadChangeKind::Switched,
                        branch: None,
                        observed_at_ms: (w * ROWS_PER_WRITER + row) as i64,
                    });
                    journal
                        .append(&event, EventId(w * ROWS_PER_WRITER + row))
                        .expect("append");
                }
            })
        })
        .collect();
    for h in handles {
        h.join().expect("writer thread panicked");
    }

    // A torn row would either fail this open outright (mid-journal corruption
    // is fatal — see the module docs) or silently drop a row; either way the
    // count below would not match.
    let reopened =
        EventJournal::open(&journal_path).expect("reopen journal after concurrent writes");
    let rows = reopened.since(0);
    assert_eq!(
        rows.len() as u64,
        WRITERS * ROWS_PER_WRITER,
        "every concurrently-appended row must be present and parseable"
    );
}

#[test]
fn real_git_worktree_add_is_monitored_directly() {
    let repo = TestRepo::init().expect("init");
    let w = repo.writer();
    let base = w.commit_file("a.txt", "one", "first").expect("commit");

    let linked_dir = tempfile::TempDir::new().expect("create linked worktree temp dir");
    let linked_path = linked_dir.path().join("linked");
    repo.git()
        .try_run(
            repo.path(),
            &[
                "worktree",
                "add",
                "-q",
                "-b",
                "linked-branch",
                linked_path.to_str().expect("utf8 path"),
                base.as_str(),
            ],
        )
        .expect("git worktree add");

    let (mut monitor, _journal_path, _tmp) = open_monitor();
    let id = wt("linked");
    monitor
        .register(id.clone(), linked_path.clone())
        .expect("register linked worktree");
    monitor.reconcile(&id).expect("priming reconcile");

    let linked_writer = repo.writer_at(&linked_path);
    let new_head = linked_writer
        .commit_file("only-in-linked.txt", "x", "linked commit")
        .expect("commit in linked worktree");

    let events = monitor.reconcile(&id).expect("reconcile");
    let hcs = head_changed(&events);
    assert_eq!(hcs.len(), 1);
    assert_eq!(
        hcs[0].kind,
        HeadChangeKind::Advanced(vec![new_head.clone()])
    );
    let cs = commits(&events);
    assert_eq!(cs.len(), 1);
    assert_eq!(cs[0].oid, new_head);
}

/// `reconcile` on an id that `register` never saw must
/// return a typed, matchable failure — never panic. Worktree ids reach
/// `reconcile` from author-supplied values at the effect surface, so an
/// unregistered id is an ordinary authoring mistake, not a process-ending
/// event.
#[test]
fn reconcile_on_unregistered_worktree_returns_worktree_not_registered() {
    let (mut monitor, _journal_path, _tmp) = open_monitor();
    let id = wt("never-registered");

    let err = monitor
        .reconcile(&id)
        .expect_err("reconcile on an unregistered worktree must fail, not panic");
    assert!(
        matches!(&err, WorktreeError::WorktreeNotRegistered(bad) if bad == &id),
        "expected WorktreeNotRegistered({id:?}), got {err:?}"
    );
}

/// A worktree that was registered but has
/// since been removed from disk (the retain-first "a human deleted it" case,
/// same condition `WorktreeManager::lookup`/`worktree_head` already type as
/// `WorktreeLost`) must reconcile as `WorktreeLost`, not surface the raw,
/// opaque `GitFailure` that `git rev-parse HEAD` against a missing directory
/// would otherwise produce.
#[test]
fn reconcile_of_a_worktree_removed_from_disk_returns_worktree_lost() {
    let repo = TestRepo::init().expect("init");
    repo.writer()
        .commit_file("a.txt", "one", "first")
        .expect("commit");

    let (mut monitor, _journal_path, _tmp) = open_monitor();
    let id = wt("w1");
    monitor
        .register(id.clone(), repo.path().to_path_buf())
        .expect("register");
    monitor.reconcile(&id).expect("priming reconcile");

    std::fs::remove_dir_all(repo.path()).expect("remove worktree dir by hand");

    let err = monitor
        .reconcile(&id)
        .expect_err("reconcile of a removed worktree must fail");
    assert!(
        matches!(&err, WorktreeError::WorktreeLost(lost) if lost == &id),
        "expected WorktreeLost({id:?}), got {err:?}"
    );
}

/// The [`EventId`] `reconcile`
/// returns on each [`Observed`] must be the SAME id the journal recorded for
/// that pass — not a fresh id minted independently at the return path, which
/// would make every other test in this file pass while correlation to the
/// journal stayed impossible.
///
/// Wrong-reason guard: an implementation that mints a disconnected id would
/// still pass a naive "some id came back" check, and one that always returns
/// a constant (e.g. `EventId(0)`) would still pass a naive "ids agree" check
/// if the journal also happened to start numbering from a fixed value. This
/// gate closes both: it forces two DISTINCT passes to mint two DISTINCT ids
/// (`assert_ne!` below — a constant/shared id fails here), then verifies the
/// second pass's returned id matches the journal rows recorded under that
/// SAME id, by both count and content (an empty/disconnected journal fails
/// the count check; a coincidental id match fails the content check).
#[test]
fn reconcile_returned_event_id_matches_the_journalled_event_id_for_that_pass() {
    let repo = TestRepo::init().expect("init");
    let w = repo.writer();
    w.commit_file("a.txt", "one", "first").expect("commit");

    let (mut monitor, journal_path, _tmp) = open_monitor();
    let id = wt("w1");
    monitor
        .register(id.clone(), repo.path().to_path_buf())
        .expect("register");
    monitor.reconcile(&id).expect("priming reconcile");

    w.commit_file("b.txt", "two", "second").expect("commit");
    let first_pass = monitor.reconcile(&id).expect("reconcile 1");
    assert!(!first_pass.is_empty());
    let first_id = first_pass[0].event_id;

    w.commit_file("c.txt", "three", "third").expect("commit");
    let second_pass = monitor.reconcile(&id).expect("reconcile 2");
    assert!(!second_pass.is_empty());
    let second_id = second_pass[0].event_id;

    assert_ne!(
        first_id, second_id,
        "each reconciliation pass must mint a distinct id, or this gate cannot tell a \
         correctly-wired id from a constant/shared one"
    );
    assert!(
        second_pass.iter().all(|e| e.event_id == second_id),
        "every observation the second pass returns must carry that pass's id"
    );

    let entries = EventJournal::open(&journal_path)
        .expect("reopen journal")
        .since(0);
    let journalled_under_second_id: Vec<_> =
        entries.iter().filter(|e| e.event_id == second_id).collect();
    assert_eq!(
        journalled_under_second_id.len(),
        second_pass.len(),
        "the journal must carry exactly the rows the second pass returned, under the id it \
         returned — a disconnected fresh id, or a journal that recorded nothing for it, fails \
         this count"
    );
    for observed in &second_pass {
        assert!(
            journalled_under_second_id
                .iter()
                .any(|e| e.event == observed.value),
            "returned event {:?} must appear in the journal under the id reconcile returned \
             for it: {observed:?}",
            observed.value
        );
    }
}

/// Tail repair: forgiving a torn final row must also TRUNCATE it away, or the
/// O_APPEND writer lands the next row after the garbage — manufacturing the
/// corrupted-middle shape `open()` (rightly) refuses, so one crash would make
/// the journal permanently unopenable. Sequence under test:
/// tear -> open (repairs) -> append -> reopen (the crash-recovery read path).
#[test]
fn journal_append_after_torn_row_recovery_keeps_the_journal_openable() {
    let repo = TestRepo::init().expect("init");
    let w = repo.writer();
    w.commit_file("a.txt", "one", "first").expect("commit");

    let (mut monitor, journal_path, _tmp) = open_monitor();
    let id = wt("w1");
    monitor
        .register(id.clone(), repo.path().to_path_buf())
        .expect("register");
    monitor.reconcile(&id).expect("priming reconcile");
    w.commit_file("b.txt", "two", "second").expect("commit");
    monitor.reconcile(&id).expect("reconcile");

    {
        use std::io::Write;
        let mut file = std::fs::OpenOptions::new()
            .append(true)
            .open(&journal_path)
            .expect("open journal for corruption");
        write!(file, "{{\"cursor\":999,\"event_id\"").expect("write torn row");
    }

    let mut repaired = EventJournal::open(&journal_path).expect("reopen after tear");
    let survivors = repaired.since(0);
    let sample = survivors
        .last()
        .expect("at least one surviving row")
        .clone();
    repaired
        .append(&sample.event, sample.event_id)
        .expect("append after repair");

    let reopened = EventJournal::open(&journal_path)
        .expect("append-after-tear must not corrupt the journal for the next open");
    assert_eq!(
        reopened.since(0).len(),
        survivors.len() + 1,
        "every surviving row plus the appended one must be readable"
    );
}

/// Retry idempotency for a mid-batch failure. A pass that died AFTER
/// journalling a gained commit but BEFORE its `HeadChanged` never advanced
/// the baseline, so the retry (or a restarted process — `register` recovers
/// baselines from `HeadChanged` rows only) rebuilds the same observations.
/// The already-journalled commit must be DELIVERED (nobody saw the failed
/// pass's events) but NOT journalled twice, and it keeps its journalled
/// EventId.
#[test]
fn reconcile_retry_delivers_but_does_not_rejournal_a_failed_pass_leftover() {
    use tidepool_worktree::{CommitReceipt, EventId, EventJournal, WorktreeMonitor};

    let repo = TestRepo::init().expect("init");
    let w = repo.writer();
    w.commit_file("a.txt", "one", "first").expect("commit");
    let first = repo
        .git()
        .try_run(repo.path(), &["rev-parse", "HEAD"])
        .expect("rev-parse")
        .trimmed()
        .to_string();
    w.commit_file("b.txt", "two", "second").expect("commit");
    let second = repo
        .git()
        .try_run(repo.path(), &["rev-parse", "HEAD"])
        .expect("rev-parse")
        .trimmed()
        .to_string();

    // Wind HEAD back so registration establishes the PRE-movement baseline.
    repo.git()
        .try_run(repo.path(), &["reset", "--hard", &first])
        .expect("reset to first");

    // The failed pass's leftover: the gained commit's row, journalled, with
    // no HeadChanged after it.
    let journal_dir = tempfile::TempDir::new().expect("journal dir");
    let journal_path = journal_dir.path().join("events.jsonl");
    let planted_id = EventId(424242);
    {
        let mut journal = EventJournal::open(&journal_path).expect("open journal");
        journal
            .append(
                &RepositoryEvent::Commit(CommitReceipt {
                    worktree: wt("w1"),
                    oid: tidepool_worktree::GitOid::from_raw(second.clone()),
                    parents: vec![tidepool_worktree::GitOid::from_raw(first.clone())],
                    subject: "second".to_string(),
                    author: "test".to_string(),
                    committed_at_ms: 0,
                    files: vec!["b.txt".to_string()],
                }),
                planted_id,
            )
            .expect("plant leftover row");
    }

    let journal = EventJournal::open(&journal_path).expect("reopen journal");
    let mut monitor = WorktreeMonitor::new(GitCli::new(), journal);
    let id = wt("w1");
    monitor
        .register(id.clone(), repo.path().to_path_buf())
        .expect("register");

    // The movement happens again (the same transition the failed pass saw).
    repo.git()
        .try_run(repo.path(), &["reset", "--hard", &second])
        .expect("reset forward to second");

    let events = monitor.reconcile(&id).expect("retry reconcile");

    // Delivered: the commit (with the PLANTED id — id-based dedup downstream
    // stays sound) and the HeadChanged (fresh id).
    let delivered_commits = commits(&events);
    assert_eq!(delivered_commits.len(), 1, "the gained commit is delivered");
    assert_eq!(delivered_commits[0].oid.as_str(), second);
    let commit_event_id = events
        .iter()
        .find(|o| matches!(o.value, RepositoryEvent::Commit(_)))
        .expect("commit observed")
        .event_id;
    assert_eq!(
        commit_event_id, planted_id,
        "a re-delivered leftover keeps its journalled EventId"
    );

    // Journalled: exactly ONE commit row for that oid (the planted one), plus
    // the HeadChanged. No duplicate receipt under a fresh id.
    let rows = EventJournal::open(&journal_path)
        .expect("reopen for audit")
        .since(0);
    let commit_rows: Vec<_> = rows
        .iter()
        .filter(|e| matches!(&e.event, RepositoryEvent::Commit(c) if c.oid.as_str() == second))
        .collect();
    assert_eq!(commit_rows.len(), 1, "no duplicate commit row");
    assert_eq!(commit_rows[0].event_id, planted_id);
    assert_eq!(
        rows.iter()
            .filter(|e| matches!(e.event, RepositoryEvent::HeadChanged(_)))
            .count(),
        1,
        "the retry journalled its HeadChanged"
    );
}
