//! Storage tests using a genuine filesystem I/O failure against
//! runtime-owned storage (registry, binding table, journal, worktree root)
//! must surface as [`WorktreeError::StorageFailure`] naming the path that
//! actually failed, not abort the process.
//!
//! Two induction techniques, per the spec:
//!
//! - a regular FILE where the code expects to create a directory
//!   (`create_dir_all` then fails with "not a directory") — deterministic,
//!   works identically whether the test runs as root or not.
//! - a read-only directory so a write into it fails with `EACCES` — this one
//!   is NOT enforced for root, which ignores permission bits, so every test
//!   using it checks its own postcondition and skips with a loud message
//!   rather than silently passing when run as root.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use tidepool_worktree::testing::TestRepo;
use tidepool_worktree::{
    AgentRef, BindingTable, BranchName, EventId, EventJournal, GitCli, GitOid, HeadChangeKind,
    HeadChangeReceipt, RepositoryEvent, WorktreeError, WorktreeId, WorktreeManager, WorktreeOrigin,
    WorktreeReceipt, WorktreeRecordStatus, WorktreeRegistry, WorktreeSpec,
};

/// Make `dir` unwritable (`r-xr-xr-x`) so a create/write inside it fails.
fn make_read_only(dir: &Path) {
    let mut perms = fs::metadata(dir).expect("stat dir").permissions();
    perms.set_mode(0o555);
    fs::set_permissions(dir, perms).expect("chmod read-only");
}

/// Restore write permission so `TempDir`'s drop can clean up.
fn make_writable(dir: &Path) {
    let mut perms = fs::metadata(dir).expect("stat dir").permissions();
    perms.set_mode(0o755);
    fs::set_permissions(dir, perms).expect("chmod writable");
}

fn minimal_receipt(id: &WorktreeId) -> WorktreeReceipt {
    WorktreeReceipt {
        worktree_id: id.clone(),
        cwd: PathBuf::from("/nonexistent/cwd"),
        branch: BranchName::from_raw("tidepool/worktree/fixture"),
        source_head: GitOid::from_raw("0".repeat(40)),
        snapshot_ref: None,
        origin: WorktreeOrigin::CurrentRepository,
        source_repository: PathBuf::from("/nonexistent/source"),
        created_at_ms: 0,
        status: WorktreeRecordStatus::Finalized,
    }
}

/// Depth-first search under `root` for a file whose name contains `needle` —
/// used instead of hardcoding the registry/binding on-disk layout, which is a
/// private implementation detail these tests should not need to know.
fn find_file_containing(root: &Path, needle: &str) -> PathBuf {
    fn walk(dir: &Path, needle: &str, found: &mut Option<PathBuf>) {
        for entry in fs::read_dir(dir).expect("read dir") {
            let entry = entry.expect("dir entry");
            let path = entry.path();
            if path.is_dir() {
                walk(&path, needle, found);
            } else if path
                .file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.contains(needle))
            {
                *found = Some(path);
            }
        }
    }
    let mut found = None;
    walk(root, needle, &mut found);
    found.unwrap_or_else(|| panic!("no file under {} containing {needle}", root.display()))
}

// ---------------------------------------------------------------------------
// registry.rs
// ---------------------------------------------------------------------------

#[test]
fn registry_open_reports_typed_failure_when_a_file_blocks_the_root() {
    let base = tempfile::TempDir::new().expect("tempdir");
    let blocker = base.path().join("registry-root");
    fs::write(&blocker, b"not a directory").expect("write blocking file");

    let err = WorktreeRegistry::open(&blocker).expect_err("a file cannot become a directory");
    match err {
        WorktreeError::StorageFailure { path, detail } => {
            assert_eq!(path, blocker, "the failure must name the root itself");
            assert!(!detail.is_empty());
        }
        other => panic!("expected StorageFailure, got {other:?}"),
    }
}

#[test]
fn registry_put_reports_typed_failure_when_the_records_dir_is_read_only() {
    let base = tempfile::TempDir::new().expect("tempdir");
    let root = base.path().join("registry");
    let registry = WorktreeRegistry::open(&root).expect("open registry");
    let records_dir = registry.root().join("records");

    make_read_only(&records_dir);
    let id = WorktreeId::from_raw("wt-perm-test");
    let result = registry.put(&minimal_receipt(&id));
    make_writable(&records_dir);

    match result {
        Err(WorktreeError::StorageFailure { path, detail }) => {
            assert_eq!(
                path, records_dir,
                "the failure must name the directory the write actually happened in"
            );
            assert!(!detail.is_empty());
        }
        Ok(()) => {
            eprintln!(
                "SKIPPED: registry_put_reports_typed_failure_when_the_records_dir_is_read_only \
                 — the write succeeded despite chmod 0o555, so permission bits are not enforced \
                 in this environment (likely running as root); cannot exercise this failure here."
            );
        }
        other => panic!("expected StorageFailure, got {other:?}"),
    }
}

#[test]
fn registry_get_reports_typed_failure_for_a_corrupt_record() {
    let base = tempfile::TempDir::new().expect("tempdir");
    let registry = WorktreeRegistry::open(base.path().join("registry")).expect("open registry");
    let id = WorktreeId::from_raw("wt-corrupt-get");
    registry.put(&minimal_receipt(&id)).expect("put fixture");

    let record_path = find_file_containing(registry.root(), id.as_str());
    fs::write(&record_path, b"{ not valid json").expect("corrupt the record");

    let err = registry
        .get(&id)
        .expect_err("a corrupt record must not deserialize");
    match err {
        WorktreeError::StorageFailure { path, detail } => {
            assert_eq!(
                path, record_path,
                "the failure must name the specific corrupt record, not the registry root"
            );
            assert!(!detail.is_empty());
        }
        other => panic!("expected StorageFailure, got {other:?}"),
    }
}

#[test]
fn registry_list_reports_typed_failure_for_a_corrupt_record() {
    let base = tempfile::TempDir::new().expect("tempdir");
    let registry = WorktreeRegistry::open(base.path().join("registry")).expect("open registry");
    let id = WorktreeId::from_raw("wt-corrupt-list");
    registry.put(&minimal_receipt(&id)).expect("put fixture");

    let record_path = find_file_containing(registry.root(), id.as_str());
    fs::write(&record_path, b"not json at all").expect("corrupt the record");

    let err = registry
        .list()
        .expect_err("a corrupt record must fail list, not be swallowed");
    assert!(
        matches!(&err, WorktreeError::StorageFailure { path, .. } if path == &record_path),
        "expected StorageFailure naming {}, got {err:?}",
        record_path.display()
    );
}

// ---------------------------------------------------------------------------
// binding.rs
// ---------------------------------------------------------------------------

#[test]
fn binding_open_reports_typed_failure_when_a_file_blocks_the_root() {
    let base = tempfile::TempDir::new().expect("tempdir");
    let blocker = base.path().join("bindings-root");
    fs::write(&blocker, b"not a directory").expect("write blocking file");

    let err = BindingTable::open(&blocker).expect_err("a file cannot become a directory");
    match err {
        WorktreeError::StorageFailure { path, detail } => {
            assert_eq!(path, blocker);
            assert!(!detail.is_empty());
        }
        other => panic!("expected StorageFailure, got {other:?}"),
    }
}

#[test]
fn binding_bind_reports_typed_failure_when_the_root_is_read_only() {
    let base = tempfile::TempDir::new().expect("tempdir");
    let root = base.path().join("bindings");
    let mut table = BindingTable::open(&root).expect("open bindings");

    make_read_only(&root);
    let worktree = WorktreeId::from_raw("wt-bind-perm-test");
    let agent = AgentRef::from_raw("agent-a");
    let result = table.bind(&worktree, &agent, 1000);
    make_writable(&root);

    match result {
        Err(WorktreeError::StorageFailure { path, detail }) => {
            assert_eq!(
                path, root,
                "the failure must name the directory being written to"
            );
            assert!(!detail.is_empty());
        }
        Ok(_) => {
            eprintln!(
                "SKIPPED: binding_bind_reports_typed_failure_when_the_root_is_read_only — \
                 the write succeeded despite chmod 0o555, so permission bits are not enforced \
                 in this environment (likely running as root); cannot exercise this failure here."
            );
        }
        other => panic!("expected StorageFailure, got {other:?}"),
    }
}

#[test]
fn binding_open_reports_typed_failure_for_a_corrupt_record() {
    let base = tempfile::TempDir::new().expect("tempdir");
    let root = base.path().join("bindings");
    {
        let mut table = BindingTable::open(&root).expect("open bindings");
        table
            .bind(
                &WorktreeId::from_raw("wt-bind-corrupt"),
                &AgentRef::from_raw("agent-a"),
                1000,
            )
            .expect("bind fixture");
    }

    let record_path = find_file_containing(&root, "wt-bind-corrupt");
    fs::write(&record_path, b"[ not valid json").expect("corrupt the record");

    let err = BindingTable::open(&root).expect_err("a corrupt binding record must not deserialize");
    match err {
        WorktreeError::StorageFailure { path, detail } => {
            assert_eq!(path, record_path);
            assert!(!detail.is_empty());
        }
        other => panic!("expected StorageFailure, got {other:?}"),
    }
}

// ---------------------------------------------------------------------------
// journal.rs
// ---------------------------------------------------------------------------

#[test]
fn journal_open_reports_typed_failure_when_a_file_blocks_the_directory() {
    let base = tempfile::TempDir::new().expect("tempdir");
    let blocker = base.path().join("journal-dir");
    fs::write(&blocker, b"not a directory").expect("write blocking file");
    let journal_path = blocker.join("events.jsonl");

    let err = EventJournal::open(&journal_path).expect_err("a file cannot become a directory");
    match err {
        WorktreeError::StorageFailure { path, detail } => {
            assert_eq!(
                path, blocker,
                "the failure must name the directory that could not be created, not the file inside it"
            );
            assert!(!detail.is_empty());
        }
        other => panic!("expected StorageFailure, got {other:?}"),
    }
}

#[test]
fn journal_append_reports_typed_failure_when_its_directory_is_gone() {
    let base = tempfile::TempDir::new().expect("tempdir");
    let dir = base.path().join("journal-dir");
    let journal_path = dir.join("events.jsonl");
    let mut journal = EventJournal::open(&journal_path).expect("open journal");

    // The directory disappearing out from under an open handle is a stand-in
    // for "the volume holding it went away" — a fault a resident with many
    // worktrees can plausibly hit for one of them and must not die over.
    fs::remove_dir_all(&dir).expect("remove journal directory out from under the handle");

    let event = RepositoryEvent::HeadChanged(HeadChangeReceipt {
        worktree: WorktreeId::from_raw("wt-journal-perm-test"),
        old_head: None,
        new_head: GitOid::from_raw("1".repeat(40)),
        kind: HeadChangeKind::Switched,
        branch: None,
        observed_at_ms: 0,
    });

    let err = journal
        .append(&[event], EventId(1))
        .expect_err("append into a missing directory must fail");
    match err {
        WorktreeError::StorageFailure { path, detail } => {
            assert_eq!(path, journal_path);
            assert!(!detail.is_empty());
        }
        other => panic!("expected StorageFailure, got {other:?}"),
    }
}

// ---------------------------------------------------------------------------
// create.rs
// ---------------------------------------------------------------------------

#[test]
fn create_reports_typed_failure_when_a_file_blocks_the_worktree_root() {
    let repo = TestRepo::init().expect("init");
    repo.writer()
        .commit_file("a.txt", "one", "first")
        .expect("commit");

    let base = tempfile::TempDir::new().expect("tempdir");
    let worktree_root = base.path().join("worktrees");
    fs::write(&worktree_root, b"not a directory").expect("write blocking file");

    let registry = WorktreeRegistry::open(base.path().join("registry")).expect("open registry");
    let manager = WorktreeManager::new(GitCli::new(), registry, worktree_root.clone(), repo.path());

    let err = manager
        .create(&WorktreeSpec::from_current_repository("child"))
        .expect_err("a file cannot become the worktree root");
    match err {
        WorktreeError::StorageFailure { path, detail } => {
            assert_eq!(path, worktree_root);
            assert!(!detail.is_empty());
        }
        other => panic!("expected StorageFailure, got {other:?}"),
    }

    // No registry row from the failed attempt: the failure happened before
    // any receipt could be written.
    assert!(manager.list().expect("list").is_empty());
}

/// A malformed row in the MIDDLE of the journal is a corrupted receipt, not a
/// torn write, and must fail loudly.
///
/// The recovery contract tolerates exactly one shape: a crash mid-`writeln!`
/// leaving an incomplete FINAL line. A bad row with valid rows after it cannot
/// have been produced that way. Silently skipping it would delete precisely the
/// evidence the journal exists to preserve — `EventJournal` is the
/// traceability substrate post-mortems rely on, so a quietly shorter journal
/// is worse than an unopenable one.
#[test]
fn journal_malformed_middle_row_fails_loudly_rather_than_being_skipped() {
    let base = tempfile::TempDir::new().expect("tempdir");
    let path = base.path().join("events.jsonl");

    let mut journal = EventJournal::open(&path).expect("open");
    let ev = RepositoryEvent::HeadChanged(HeadChangeReceipt {
        worktree: WorktreeId::from_raw("wt-mid"),
        old_head: None,
        new_head: GitOid::from_raw("a".repeat(40)),
        kind: HeadChangeKind::UnknownChange,
        branch: None,
        observed_at_ms: 1,
    });
    journal
        .append(std::slice::from_ref(&ev), EventId(1))
        .expect("append 1");
    journal.append(&[ev], EventId(2)).expect("append 2");
    drop(journal);

    // Corrupt the first EVENT row (line 1 is the version-stamp header;
    // line 2 is the first entry), leaving the second entry intact after it.
    let text = fs::read_to_string(&path).expect("read journal");
    let mut lines: Vec<String> = text.lines().map(str::to_string).collect();
    assert_eq!(
        lines.len(),
        3,
        "fixture must have a header plus two rows to corrupt a middle one"
    );
    lines[1] = "{ this is not valid json".to_string();
    fs::write(&path, format!("{}\n", lines.join("\n"))).expect("write corrupted journal");

    match EventJournal::open(&path) {
        Err(WorktreeError::StorageFailure { path: p, detail }) => {
            assert_eq!(p, path, "the failure names the journal file");
            assert!(
                detail.contains("followed by"),
                "the failure must say the bad row was not final, so a reader can \
                 tell corruption from a torn write: {detail}"
            );
        }
        Ok(j) => panic!(
            "expected StorageFailure; journal opened with {} entries — a corrupted \
             middle receipt was silently elided",
            j.since(0).len()
        ),
        other => panic!("expected StorageFailure, got {other:?}"),
    }
}

/// WRONG-REASON GUARD for the gate above: a torn FINAL row must still be
/// tolerated. Without this, the middle-row gate would also pass if the fix had
/// simply made every malformed row fatal — which would break restart recovery
/// rather than tighten it.
#[test]
fn journal_torn_final_row_is_still_tolerated_after_the_middle_row_fix() {
    let base = tempfile::TempDir::new().expect("tempdir");
    let path = base.path().join("events.jsonl");

    let mut journal = EventJournal::open(&path).expect("open");
    let ev = RepositoryEvent::HeadChanged(HeadChangeReceipt {
        worktree: WorktreeId::from_raw("wt-tail"),
        old_head: None,
        new_head: GitOid::from_raw("b".repeat(40)),
        kind: HeadChangeKind::UnknownChange,
        branch: None,
        observed_at_ms: 1,
    });
    journal.append(&[ev], EventId(1)).expect("append");
    drop(journal);

    let text = fs::read_to_string(&path).expect("read journal");
    fs::write(&path, format!("{text}{{ torn")).expect("append torn final row");

    let reopened = EventJournal::open(&path).expect("a torn FINAL row stays recoverable");
    assert_eq!(
        reopened.since(0).len(),
        1,
        "the intact row survives; only the torn final row is dropped"
    );
}

/// A failed binding write fences the owner even after permissions recover.
#[test]
fn binding_failed_bind_persist_requires_reopen() {
    let base = tempfile::TempDir::new().expect("tempdir");
    let root = base.path().join("bindings");
    let mut table = BindingTable::open(&root).expect("open bindings");

    make_read_only(&root);
    let worktree = WorktreeId::from_raw("wt-uncertain");
    let agent = AgentRef::from_raw("agent-a");
    let result = table.bind(&worktree, &agent, 1000);
    make_writable(&root);

    match result {
        Err(WorktreeError::StorageFailure { .. }) => {
            assert!(table.current(&worktree).is_none());
            assert!(table.active_for_agent(&agent).is_none());
            assert!(table.bind(&worktree, &agent, 2000).is_err());
            drop(table);
            // Chmod stopped publication before rename; authoritative disk has no row.
            let mut table = BindingTable::open(&root).expect("reopen after fault clears");
            table
                .bind(&worktree, &agent, 2000)
                .expect("fresh bind after reconciliation");
            assert_eq!(table.current(&worktree).expect("bound").agent(), &agent);
        }
        Ok(_) => eprintln!(
            "SKIPPED: binding_failed_bind_persist_requires_reopen — the write \
             succeeded despite chmod 0o555, so permission bits are not enforced here \
             (likely running as root); the permission-failure path cannot be exercised."
        ),
        other => panic!("expected StorageFailure, got {other:?}"),
    }
}

/// Failed settlement denies custody until reopen confirms retained Active disk state.
#[test]
fn binding_failed_settle_persist_requires_reopen() {
    let base = tempfile::TempDir::new().expect("tempdir");
    let root = base.path().join("bindings");
    let mut table = BindingTable::open(&root).expect("open bindings");

    let worktree = WorktreeId::from_raw("wt-settle-uncertain");
    let agent = AgentRef::from_raw("agent-a");
    let lease = table.bind(&worktree, &agent, 1000).expect("initial bind");

    make_read_only(&root);
    let result = lease.release(&mut table);
    make_writable(&root);

    match result {
        Err(WorktreeError::StorageFailure { .. }) => {
            assert!(table.current(&worktree).is_none());
            assert!(table.active_for_agent(&agent).is_none());
            assert!(table.bind(&worktree, &agent, 2000).is_err());
            drop(table);
            let mut table = BindingTable::open(&root).expect("reopen retained disk state");
            assert_eq!(
                table.current(&worktree).expect("retained Active").agent(),
                &agent
            );
            assert!(matches!(
                table.bind(&worktree, &agent, 2000),
                Err(WorktreeError::WorktreeBusy { .. })
            ));
        }
        Ok(()) => eprintln!(
            "SKIPPED: binding_failed_settle_persist_requires_reopen — the write \
             succeeded despite chmod 0o555, so permission bits are not enforced here \
             (likely running as root); the permission-failure path cannot be exercised."
        ),
        other => panic!("expected StorageFailure, got {other:?}"),
    }
}
