#![cfg(target_os = "linux")]
use std::{fs, path::Path, process::Command};
use tidepool_worktree::{
    testing::TestRepo, EventId, EventJournal, GitCli, WorktreeId, WorktreeMonitor,
};

#[test]
fn journal_fault_child() {
    let Ok(root) = std::env::var("JOURNAL_TEST_ROOT") else {
        return;
    };
    let root = Path::new(&root);
    let path = root.join("new/deep/events.jsonl");
    let armed = root.join("armed");
    let mut journal = EventJournal::open(&path).unwrap();
    journal.append(&[], EventId(1)).unwrap();
    fs::write(&armed, "armed").unwrap();
    assert!(journal.append(&[], EventId(2)).is_err());
    fs::remove_file(&armed).unwrap();
    let uncertain = fs::read(&path).unwrap();
    assert_eq!(
        uncertain
            .split(|b| *b == b'\n')
            .filter(|s| !s.is_empty())
            .count(),
        3
    );
    assert_eq!(
        journal.end_cursor(),
        1,
        "only acknowledged snapshot is exposed"
    );
    assert!(journal.append(&[], EventId(3)).is_err());
    assert_eq!(
        fs::read(&path).unwrap(),
        uncertain,
        "poison refuses writes even after fault clears"
    );
    assert!(
        EventJournal::open(&path).is_err(),
        "uncertain writer retains lock"
    );
    drop(journal);
    fs::write(&armed, "armed").unwrap();
    assert!(
        EventJournal::open(&path).is_err(),
        "reopen must confirm persistence"
    );
    fs::remove_file(&armed).unwrap();
    let mut recovered = EventJournal::open(&path).unwrap();
    assert_eq!(recovered.end_cursor(), 2);
    assert_eq!(recovered.append(&[], EventId(3)).unwrap(), 3);
    assert_eq!(
        recovered
            .since(0)
            .iter()
            .map(|r| (r.cursor, r.event_id.0))
            .collect::<Vec<_>>(),
        [(1, 1), (2, 2), (3, 3)]
    );
}

#[test]
fn monitor_fault_child() {
    let Ok(root) = std::env::var("JOURNAL_TEST_ROOT") else {
        return;
    };
    let root = Path::new(&root);
    let path = root.join("new/deep/events.jsonl");
    let repo = TestRepo::init().unwrap();
    repo.writer().commit_empty("initial").unwrap();
    let id = WorktreeId::from_raw("wt-uncertainty");
    let journal = EventJournal::open(&path).unwrap();
    let mut monitor = WorktreeMonitor::new(GitCli::new(), journal);
    monitor
        .register(id.clone(), repo.path().to_path_buf())
        .unwrap();
    repo.writer()
        .commit_empty("visible-failed-observation")
        .unwrap();
    fs::write(root.join("armed"), "armed").unwrap();
    assert!(monitor.reconcile(&id).is_err());
    fs::remove_file(root.join("armed")).unwrap();
    let uncertain = fs::read(&path).unwrap();
    assert!(monitor.reconcile(&id).is_err());
    assert!(monitor
        .register(id.clone(), repo.path().to_path_buf())
        .is_err());
    assert_eq!(fs::read(&path).unwrap(), uncertain);
    drop(monitor);
    let mut monitor = WorktreeMonitor::new(GitCli::new(), EventJournal::open(&path).unwrap());
    monitor
        .register(id.clone(), repo.path().to_path_buf())
        .unwrap();
    assert!(
        monitor.reconcile(&id).unwrap().is_empty(),
        "reopen uses retained observation baseline"
    );
    repo.writer().commit_empty("next").unwrap();
    let events = monitor.reconcile(&id).unwrap();
    assert!(!events.is_empty());
    assert!(events.iter().all(|event| event.event_id == EventId(2)));
    drop(monitor);
    let journal = EventJournal::open(&path).unwrap();
    assert_eq!(
        journal
            .since(0)
            .iter()
            .map(|r| (r.cursor, r.event_id.0))
            .collect::<Vec<_>>(),
        [(1, 1), (2, 2)]
    );
}

#[test]
fn owning_paths_poison_and_reopen_without_reusing_sequences() {
    let temp = tempfile::tempdir().unwrap();
    let library = temp.path().join("journal-fault.so");
    assert!(Command::new("cc")
        .args(["-shared", "-fPIC", "-Wall", "-Werror"])
        .arg(Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/journal_fault.c"))
        .arg("-o")
        .arg(&library)
        .arg("-ldl")
        .status()
        .unwrap()
        .success());
    for child in ["journal_fault_child", "monitor_fault_child"] {
        for (kind, target) in [
            ("open", "new/deep"),
            ("sync", "new/deep"),
            ("sync", "new/deep/events.jsonl"),
        ] {
            let root = temp
                .path()
                .join(format!("{child}-{kind}-{}", target.replace('/', "-")));
            fs::create_dir(&root).unwrap();
            let log = root.join("hits");
            let result = Command::new(std::env::current_exe().unwrap())
                .args(["--exact", child, "--nocapture"])
                .env("LD_PRELOAD", &library)
                .env("JOURNAL_TEST_ROOT", &root)
                .env("JOURNAL_FAULT_PATH", root.join(target))
                .env("JOURNAL_FAULT_KIND", kind)
                .env("JOURNAL_FAULT_ARMED", root.join("armed"))
                .env("JOURNAL_FAULT_LOG", &log)
                .output()
                .unwrap();
            assert!(
                result.status.success(),
                "{child}/{kind}/{target}: {} {}",
                String::from_utf8_lossy(&result.stdout),
                String::from_utf8_lossy(&result.stderr)
            );
            let hits = fs::read_to_string(log).unwrap();
            assert_eq!(
                hits.lines().count(),
                if child == "journal_fault_child" { 2 } else { 1 },
                "each planned injection reached once"
            );
        }
    }
}
