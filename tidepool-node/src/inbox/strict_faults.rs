//! Real syscall failures, scoped to an armed subprocess; not power-loss tests.
use super::*;
use std::{fs, process::Command};

#[test]
fn fault_child() {
    let Ok(root) = std::env::var("INBOX_FAULT_ROOT") else {
        return;
    };
    let root = Path::new(&root);
    let case = std::env::var("INBOX_FAULT_CASE").unwrap();
    let arm = PathBuf::from(std::env::var("INBOX_FAULT_ARM").unwrap());
    let rows = root.join("rows/new/deep/rows.jsonl");
    let cursor = root.join("cursor/new/deep/checkpoint");
    let open = || DurableInbox::<String, String>::open(rows.clone(), cursor.clone());
    let enable = || fs::write(&arm, b"armed").unwrap();
    let disable = || fs::remove_file(&arm).unwrap();
    if case.starts_with("fresh-") {
        enable();
        assert!(open().is_err());
        // A visible hierarchy after mkdir failure must not bypass synchronization.
        assert!(open().is_err());
        disable();
        let inbox = open().unwrap();
        assert_eq!(
            inbox
                .publish_tracked("ok".into(), "owner".into())
                .unwrap()
                .sequence,
            1
        );
        return;
    }
    let inbox = open().unwrap();
    if case == "append" || case == "migration" {
        enable();
        let expected = if case == "append" {
            InboxWriteOperation::Append
        } else {
            InboxWriteOperation::Checkpoint
        };
        assert!(
            matches!(inbox.publish_tracked("first".into(), "owner".into()),
            Err(InboxError::UncertainWrite { operation, .. }) if operation == expected)
        );
        assert!(matches!(
            inbox.publish("retry".into()),
            Err(InboxError::Poisoned)
        ));
        assert!(matches!(
            inbox.observe_receipt(1),
            Err(InboxError::Poisoned)
        ));
        disable();
        if case == "append" {
            assert!(fs::read_to_string(&rows).unwrap().contains("first"));
        }
        drop(inbox);
        let reopened = open().unwrap();
        if case == "append" {
            assert_eq!(phase(&reopened, 1), DeliveryPhase::Accepted);
            assert_eq!(reopened.publish("next".into()).unwrap().sequence, 2);
        } else {
            assert_eq!(reopened.publish("next".into()).unwrap().sequence, 1);
        }
        return;
    }
    let row = inbox
        .publish_tracked("message".into(), "owner".into())
        .unwrap();
    if case == "fence" {
        enable();
        assert!(matches!(
            inbox.begin_tracked_delivery(row.sequence),
            Err(InboxError::UncertainWrite {
                operation: InboxWriteOperation::Checkpoint,
                ..
            })
        ));
        assert!(matches!(
            inbox.begin_tracked_delivery(row.sequence),
            Err(InboxError::Poisoned)
        ));
        assert!(matches!(
            inbox.publish("retry".into()),
            Err(InboxError::Poisoned)
        ));
        disable();
        assert_eq!(
            read_cursor::<String>(&cursor).unwrap().0.receipts[&row.sequence].phase,
            DeliveryPhase::InFlight
        );
        drop(inbox);
        let reopened = open().unwrap();
        assert_eq!(phase(&reopened, row.sequence), DeliveryPhase::Unconfirmed);
        assert!(reopened.begin_tracked_delivery(row.sequence).is_err());
        return;
    }
    if case == "compaction" {
        inbox
            .begin_tracked_delivery(row.sequence)
            .unwrap()
            .submitted()
            .unwrap();
        for _ in 1..COMPACT_ACKNOWLEDGED_ROWS {
            inbox.publish("legacy".into()).unwrap();
        }
        enable();
        assert!(matches!(
            inbox.acknowledge(COMPACT_ACKNOWLEDGED_ROWS),
            Err(InboxError::UncertainWrite {
                operation: InboxWriteOperation::Compaction,
                ..
            })
        ));
        assert!(matches!(
            inbox.publish("retry".into()),
            Err(InboxError::Poisoned)
        ));
        disable();
        drop(inbox);
        let reopened = open().unwrap();
        assert_eq!(reopened.cursor(), COMPACT_ACKNOWLEDGED_ROWS);
        assert!(reopened.pending().unwrap().is_empty());
        assert_eq!(
            reopened.observe_receipt(row.sequence).unwrap(),
            ReceiptLookup::Retained(ReceiptEvidence {
                context: "owner".into(),
                phase: DeliveryPhase::Submitted
            })
        );
        assert_eq!(
            reopened.publish("next".into()).unwrap().sequence,
            COMPACT_ACKNOWLEDGED_ROWS + 1
        );
        return;
    }
    assert!(case.starts_with("reopen-"));
    drop(inbox.begin_tracked_delivery(row.sequence).unwrap());
    drop(inbox);
    enable();
    assert!(open().is_err());
    disable();
    let reopened = open().unwrap();
    assert_eq!(phase(&reopened, row.sequence), DeliveryPhase::Unconfirmed);
    assert!(reopened.begin_tracked_delivery(row.sequence).is_err());
}

#[test]
fn strict_inbox_faults_propagate_without_retry() {
    let temp = tempfile::tempdir().unwrap();
    let library = temp.path().join("fault.so");
    let source = Path::new(env!("CARGO_MANIFEST_DIR")).join("src/inbox/fixtures/directory_fault.c");
    assert!(Command::new("cc")
        .args(["-shared", "-fPIC", "-Wall", "-Werror"])
        .arg(source)
        .arg("-o")
        .arg(&library)
        .arg("-ldl")
        .status()
        .unwrap()
        .success());
    for (index, (case, target)) in [
        ("fresh-rows", "rows/new/deep"),
        ("fresh-cursor", "cursor/new/deep"),
        ("fresh-rows-ancestor", "rows"),
        ("fresh-cursor-ancestor", "cursor"),
        ("append", "rows/new/deep"),
        ("migration", "cursor/new/deep"),
        ("fence", "cursor/new/deep"),
        ("compaction", "rows/new/deep"),
        ("reopen-rows", "rows/new/deep/rows.jsonl"),
        ("reopen-cursor", "cursor/new/deep/checkpoint"),
    ]
    .into_iter()
    .enumerate()
    {
        for kind in ["open", "sync"] {
            let root = temp.path().join(format!("{index}-{kind}"));
            fs::create_dir(&root).unwrap();
            let log = root.join("hits");
            let output = Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "inbox::tests::strict_faults::fault_child",
                    "--nocapture",
                ])
                .env("LD_PRELOAD", &library)
                .env("INBOX_FAULT_PATH", root.join(target))
                .env("INBOX_FAULT_ROOT", &root)
                .env("INBOX_FAULT_LOG", &log)
                .env("INBOX_FAULT_CASE", case)
                .env("INBOX_FAULT_KIND", kind)
                .env("INBOX_FAULT_ARM", root.join("armed"))
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "{case}/{kind}: {} {}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
            assert!(
                !fs::read(&log).expect("injector must be reached").is_empty(),
                "{case}/{kind}"
            );
            eprintln!("verified {case}/{kind}");
        }
    }
}
