use super::*;

fn inbox() -> (tempfile::TempDir, PathBuf, PathBuf, DurableInbox<String>) {
    let dir = tempfile::tempdir().unwrap();
    let rows = dir.path().join("inbox/rows.jsonl");
    let cursor = dir.path().join("inbox/cursor");
    let inbox = DurableInbox::open(rows.clone(), cursor.clone()).unwrap();
    (dir, rows, cursor, inbox)
}

#[test]
fn unacknowledged_rows_survive_reopen_and_sequences_continue() {
    let (_dir, rows, cursor, inbox) = inbox();
    assert_eq!(inbox.publish("one".into()).unwrap().sequence, 1);
    assert_eq!(inbox.publish("two".into()).unwrap().sequence, 2);
    inbox.acknowledge(1).unwrap();
    drop(inbox);

    let reopened = DurableInbox::<String>::open(rows, cursor).unwrap();
    assert_eq!(reopened.cursor(), 1);
    assert_eq!(
        reopened.pending().unwrap(),
        vec![DurableEnvelope {
            sequence: 2,
            payload: "two".to_string(),
            publication: None,
            receipt_context: None,
        }]
    );
    assert_eq!(reopened.publish("three".into()).unwrap().sequence, 3);
}

#[test]
fn acknowledgement_is_monotonic_and_bounded_by_published_data() {
    let (_dir, _rows, _cursor, inbox) = inbox();
    inbox.publish("one".into()).unwrap();
    inbox.acknowledge(1).unwrap();
    inbox.acknowledge(1).unwrap();
    assert!(matches!(
        inbox.acknowledge(0),
        Err(InboxError::AckRegression { .. })
    ));
    assert!(matches!(
        inbox.acknowledge(2),
        Err(InboxError::AckBeyondEnd { .. })
    ));
}

#[test]
fn a_cursor_beyond_the_log_is_corruption_not_silent_message_loss() {
    let (_dir, rows, cursor, inbox) = inbox();
    inbox.publish("one".into()).unwrap();
    drop(inbox);
    std::fs::write(&cursor, "2").unwrap();
    assert!(matches!(
        DurableInbox::<String>::open(rows, cursor),
        Err(InboxError::Corrupt(_))
    ));
}

#[test]
fn acknowledged_prefixes_compact_without_resetting_sequence_identity() {
    let (_dir, rows, cursor, inbox) = inbox();
    for sequence in 1..=COMPACT_ACKNOWLEDGED_ROWS {
        let envelope = inbox.publish(format!("message-{sequence}")).unwrap();
        inbox.acknowledge(envelope.sequence).unwrap();
    }
    assert_eq!(std::fs::read_to_string(&rows).unwrap(), "");
    drop(inbox);

    let reopened = DurableInbox::<String>::open(rows, cursor).unwrap();
    assert_eq!(reopened.cursor(), COMPACT_ACKNOWLEDGED_ROWS);
    assert_eq!(
        reopened.publish("next".into()).unwrap().sequence,
        COMPACT_ACKNOWLEDGED_ROWS + 1
    );
}

#[test]
fn legacy_numeric_cursor_migrates_on_acknowledgement() {
    let (_dir, rows, cursor, inbox) = inbox();
    let first = inbox.publish("first".into()).unwrap();
    let second = inbox.publish("second".into()).unwrap();
    drop(inbox);
    std::fs::write(&cursor, first.sequence.to_string()).unwrap();
    let reopened = DurableInbox::<String>::open(rows.clone(), cursor.clone()).unwrap();
    assert_eq!(reopened.pending().unwrap().len(), 1);
    reopened.acknowledge(second.sequence).unwrap();
    assert!(std::fs::read_to_string(&cursor).unwrap().starts_with('{'));
    drop(reopened);
    assert!(DurableInbox::<String>::open(rows, cursor)
        .unwrap()
        .pending()
        .unwrap()
        .is_empty());
}

#[test]
fn publication_watermark_survives_reopen_ack_and_compaction() {
    let (_dir, rows, cursor, inbox) = inbox();
    let first = inbox
        .publish_latest("actor-thread".into(), 10, "failed".into())
        .unwrap()
        .unwrap();
    drop(inbox);
    let reopened = DurableInbox::<String>::open(rows.clone(), cursor.clone()).unwrap();
    assert!(reopened
        .publish_latest("actor-thread".into(), 10, "replay".into())
        .unwrap()
        .is_none());
    assert_eq!(reopened.pending().unwrap().len(), 1);
    reopened.acknowledge(first.sequence).unwrap();
    for _ in 1..COMPACT_ACKNOWLEDGED_ROWS {
        let row = reopened.publish("ordinary".into()).unwrap();
        reopened.acknowledge(row.sequence).unwrap();
    }
    assert_eq!(std::fs::read_to_string(&rows).unwrap(), "");
    drop(reopened);
    let reopened = DurableInbox::<String>::open(rows, cursor).unwrap();
    assert!(reopened
        .publish_latest("actor-thread".into(), 9, "delayed".into())
        .unwrap()
        .is_none());
    assert!(reopened
        .publish_latest("actor-thread".into(), 10, "replay".into())
        .unwrap()
        .is_none());
    assert!(reopened
        .publish_latest("actor-thread".into(), 11, "new failure".into())
        .unwrap()
        .is_some());
    assert!(reopened
        .publish_latest("another-incarnation".into(), 10, "independent".into())
        .unwrap()
        .is_some());
}

fn tracked() -> (
    tempfile::TempDir,
    PathBuf,
    PathBuf,
    DurableInbox<String, String>,
) {
    let dir = tempfile::tempdir().unwrap();
    let rows = dir.path().join("rows.jsonl");
    let cursor = dir.path().join("cursor");
    let inbox = DurableInbox::open(rows.clone(), cursor.clone()).unwrap();
    (dir, rows, cursor, inbox)
}

fn phase(inbox: &DurableInbox<String, String>, sequence: u64) -> DeliveryPhase {
    match inbox.observe_receipt(sequence).unwrap() {
        ReceiptLookup::Retained(evidence) => evidence.phase,
        ReceiptLookup::Unavailable => panic!("receipt unavailable"),
    }
}

#[test]
fn tracked_migration_is_opt_in_and_old_readers_reject_before_rows_are_published() {
    let (_dir, rows, cursor, inbox) = tracked();
    let legacy = inbox.publish("legacy".into()).unwrap();
    inbox.acknowledge(legacy.sequence).unwrap();
    assert!(serde_json::from_slice::<LegacyCheckpoint>(&std::fs::read(&cursor).unwrap()).is_ok());
    let row = inbox
        .publish_tracked("tracked".into(), "sender".into())
        .unwrap();
    let checkpoint = std::fs::read(&cursor).unwrap();
    #[derive(Deserialize)]
    #[allow(dead_code)]
    struct OldCheckpoint {
        sequence: u64,
        watermarks: BTreeMap<String, u64>,
    }
    #[derive(Deserialize)]
    #[serde(untagged)]
    #[allow(dead_code)]
    enum OldCursor {
        Numeric(u64),
        Object(OldCheckpoint),
    }
    assert!(serde_json::from_slice::<OldCursor>(&checkpoint).is_err());
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&checkpoint).unwrap()["version"],
        1
    );
    drop(inbox);
    let reopened = DurableInbox::<String, String>::open(rows, cursor).unwrap();
    assert_eq!(phase(&reopened, row.sequence), DeliveryPhase::Accepted);
}

#[test]
fn pre_send_fence_survives_drop_and_reopen_and_cannot_be_retried_or_acked() {
    let (_dir, rows, cursor, inbox) = tracked();
    let row = inbox
        .publish_tracked("message".into(), "sender".into())
        .unwrap();
    let attempt = inbox.begin_tracked_delivery(row.sequence).unwrap();
    assert_eq!(attempt.envelope().payload, "message");
    assert_eq!(phase(&inbox, row.sequence), DeliveryPhase::InFlight);
    drop(attempt);
    assert_eq!(phase(&inbox, row.sequence), DeliveryPhase::Unconfirmed);
    assert!(matches!(
        inbox.begin_tracked_delivery(row.sequence),
        Err(InboxError::ReceiptTransition { .. })
    ));
    assert!(matches!(
        inbox.acknowledge(row.sequence),
        Err(InboxError::TrackedBarrier { .. })
    ));
    drop(inbox);
    let reopened = DurableInbox::<String, String>::open(rows, cursor).unwrap();
    assert_eq!(phase(&reopened, row.sequence), DeliveryPhase::Unconfirmed);
    assert!(reopened.begin_tracked_delivery(row.sequence).is_err());
    // Late, exact correlation can establish presentation without resubmitting.
    reopened.confirm_presented(row.sequence).unwrap();
    assert_eq!(phase(&reopened, row.sequence), DeliveryPhase::Presented);
    assert_eq!(reopened.cursor(), row.sequence);
}

#[test]
fn submitted_is_not_presented_and_provenance_survives_compaction_and_reopen() {
    let (_dir, rows, cursor, inbox) = tracked();
    let row = inbox
        .publish_tracked("message".into(), "actor-7@3".into())
        .unwrap();
    assert!(matches!(
        inbox.confirm_presented(row.sequence),
        Err(InboxError::ReceiptTransition {
            phase: DeliveryPhase::Accepted,
            ..
        })
    ));
    inbox
        .begin_tracked_delivery(row.sequence)
        .unwrap()
        .submitted()
        .unwrap();
    assert_eq!(phase(&inbox, row.sequence), DeliveryPhase::Submitted);
    assert_eq!(inbox.cursor(), row.sequence);
    for _ in 1..COMPACT_ACKNOWLEDGED_ROWS {
        let row = inbox.publish("legacy".into()).unwrap();
        inbox.acknowledge(row.sequence).unwrap();
    }
    assert_eq!(std::fs::read_to_string(&rows).unwrap(), "");
    drop(inbox);
    let reopened = DurableInbox::<String, String>::open(rows.clone(), cursor.clone()).unwrap();
    assert_eq!(
        reopened.observe_receipt(row.sequence).unwrap(),
        ReceiptLookup::Retained(ReceiptEvidence {
            context: "actor-7@3".into(),
            phase: DeliveryPhase::Submitted
        })
    );
    reopened.confirm_presented(row.sequence).unwrap();
    drop(reopened);
    let reopened = DurableInbox::<String, String>::open(rows, cursor).unwrap();
    assert_eq!(phase(&reopened, row.sequence), DeliveryPhase::Presented);
}

#[test]
fn proven_not_submitted_releases_only_its_attempt_and_mixed_queue_stops_at_tracked_row() {
    let (_dir, _rows, _cursor, inbox) = tracked();
    let before = inbox.publish("before".into()).unwrap();
    let tracked = inbox
        .publish_tracked("tracked".into(), "owner".into())
        .unwrap();
    let after = inbox.publish("after".into()).unwrap();
    assert_eq!(inbox.legacy_pending_prefix().unwrap(), vec![before.clone()]);
    assert!(inbox.begin_tracked_delivery(tracked.sequence).is_err());
    assert!(inbox.acknowledge(after.sequence).is_err());
    inbox.acknowledge(before.sequence).unwrap();
    assert!(inbox.legacy_pending_prefix().unwrap().is_empty());
    inbox
        .begin_tracked_delivery(tracked.sequence)
        .unwrap()
        .not_submitted()
        .unwrap();
    assert_eq!(phase(&inbox, tracked.sequence), DeliveryPhase::Accepted);
    inbox
        .begin_tracked_delivery(tracked.sequence)
        .unwrap()
        .submitted()
        .unwrap();
    assert_eq!(inbox.legacy_pending_prefix().unwrap(), vec![after.clone()]);
    inbox.acknowledge(after.sequence).unwrap();
}

#[test]
fn quota_evicts_only_acknowledged_evidence_and_old_receipts_become_unavailable() {
    let (_dir, rows, cursor, inbox) = tracked();
    for _ in 0..MAX_RETAINED_RECEIPTS {
        let row = inbox
            .publish_tracked("message".into(), "owner".into())
            .unwrap();
        inbox
            .begin_tracked_delivery(row.sequence)
            .unwrap()
            .submitted()
            .unwrap();
    }
    let row = inbox
        .publish_tracked("latest".into(), "owner".into())
        .unwrap();
    assert_eq!(
        inbox.observe_receipt(1).unwrap(),
        ReceiptLookup::Unavailable
    );
    assert_eq!(
        inbox.observe_receipt(row.sequence + 1).unwrap(),
        ReceiptLookup::Unavailable
    );
    drop(inbox);
    let reopened = DurableInbox::<String, String>::open(rows, cursor).unwrap();
    assert_eq!(
        reopened.observe_receipt(1).unwrap(),
        ReceiptLookup::Unavailable
    );
    assert_eq!(phase(&reopened, row.sequence), DeliveryPhase::Accepted);
}

#[test]
fn unresolved_receipts_and_context_size_are_bounded_without_losing_fences() {
    let (_dir, _rows, _cursor, inbox) = tracked();
    assert!(matches!(
        inbox.publish_tracked("large".into(), "x".repeat(MAX_RECEIPT_CONTEXT_BYTES)),
        Err(InboxError::ReceiptContextTooLarge { .. })
    ));
    let first = inbox
        .publish_tracked("first".into(), "x".repeat(4000))
        .unwrap();
    inbox
        .begin_tracked_delivery(first.sequence)
        .unwrap()
        .unconfirmed()
        .unwrap();
    let mut count = 1;
    while inbox
        .publish_tracked("next".into(), "x".repeat(4000))
        .is_ok()
    {
        count += 1;
    }
    assert!(count < MAX_RETAINED_RECEIPTS);
    assert_eq!(phase(&inbox, first.sequence), DeliveryPhase::Unconfirmed);
    assert!(matches!(
        inbox.publish_tracked("next".into(), "x".repeat(4000)),
        Err(InboxError::ReceiptCapacity)
    ));
}

#[test]
fn uncertain_append_never_reuses_a_sequence_until_reopen_reconciles_the_row() {
    let (_dir, rows, cursor, inbox) = tracked();
    *lock(&inbox.fault) = Some(FaultPoint::AfterAppend);
    assert!(matches!(
        inbox.publish_tracked("written".into(), "owner".into()),
        Err(InboxError::UncertainWrite {
            operation: InboxWriteOperation::Append,
            ..
        })
    ));
    assert!(matches!(
        inbox.publish("retry".into()),
        Err(InboxError::Poisoned)
    ));
    assert!(matches!(inbox.pending(), Err(InboxError::Poisoned)));
    drop(inbox);
    let reopened = DurableInbox::<String, String>::open(rows, cursor).unwrap();
    assert_eq!(reopened.pending().unwrap().len(), 1);
    assert_eq!(phase(&reopened, 1), DeliveryPhase::Accepted);
    assert_eq!(reopened.publish("new".into()).unwrap().sequence, 2);
}

#[test]
fn uncertain_fence_write_cannot_escape_as_a_send_permit() {
    let (_dir, rows, cursor, inbox) = tracked();
    let row = inbox
        .publish_tracked("message".into(), "owner".into())
        .unwrap();
    *lock(&inbox.fault) = Some(FaultPoint::AfterCheckpoint);
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
    drop(inbox);
    let reopened = DurableInbox::<String, String>::open(rows, cursor).unwrap();
    assert_eq!(phase(&reopened, row.sequence), DeliveryPhase::Unconfirmed);
    assert!(reopened.begin_tracked_delivery(row.sequence).is_err());
}

#[test]
fn future_and_malformed_checkpoints_and_provenance_disagreement_fail_closed() {
    let (_dir, rows, cursor, inbox) = tracked();
    let row = inbox
        .publish_tracked("message".into(), "owner".into())
        .unwrap();
    inbox
        .begin_tracked_delivery(row.sequence)
        .unwrap()
        .submitted()
        .unwrap();
    drop(inbox);
    let original = std::fs::read(&cursor).unwrap();
    for version in [
        serde_json::json!(2),
        serde_json::json!("1"),
        serde_json::json!(-1),
    ] {
        let mut value: serde_json::Value = serde_json::from_slice(&original).unwrap();
        value["version"] = version;
        std::fs::write(&cursor, serde_json::to_vec(&value).unwrap()).unwrap();
        assert!(DurableInbox::<String, String>::open(rows.clone(), cursor.clone()).is_err());
    }
    let mut value: serde_json::Value = serde_json::from_slice(&original).unwrap();
    value["checkpoint"]["receipts"]["1"]["context"] = serde_json::json!("forged");
    std::fs::write(&cursor, serde_json::to_vec(&value).unwrap()).unwrap();
    assert!(DurableInbox::<String, String>::open(rows.clone(), cursor.clone()).is_err());
    value["checkpoint"]["receipts"]["1"]["context"] = serde_json::json!("owner");
    value["checkpoint"]["receipts"]["2"] =
        serde_json::json!({"context":"owner", "phase":"accepted"});
    std::fs::write(&cursor, serde_json::to_vec(&value).unwrap()).unwrap();
    assert!(DurableInbox::<String, String>::open(rows, cursor).is_err());
}

#[test]
fn presentation_racing_with_submission_is_monotone_and_exact() {
    let (_dir, _rows, _cursor, inbox) = tracked();
    let row = inbox
        .publish_tracked("message".into(), "owner".into())
        .unwrap();
    let attempt = inbox.begin_tracked_delivery(row.sequence).unwrap();
    assert!(matches!(
        inbox.confirm_presented(row.sequence + 1),
        Err(InboxError::ReceiptUnavailable { .. })
    ));
    inbox.confirm_presented(row.sequence).unwrap();
    attempt.submitted().unwrap();
    assert_eq!(phase(&inbox, row.sequence), DeliveryPhase::Presented);
}

#[test]
fn explicit_null_context_remains_tracked_after_reopen() {
    let (_dir, rows, cursor, inbox) = inbox();
    let row = inbox.publish_tracked("unit provenance".into(), ()).unwrap();
    drop(inbox);
    let reopened = DurableInbox::<String>::open(rows, cursor).unwrap();
    assert_eq!(reopened.pending().unwrap()[0].receipt_context, Some(()));
    assert!(reopened.legacy_pending_prefix().unwrap().is_empty());
    assert!(matches!(
        reopened.acknowledge(row.sequence),
        Err(InboxError::TrackedBarrier { .. })
    ));
}

#[test]
fn unresolved_count_capacity_never_evicts_a_pending_receipt() {
    let (_dir, _rows, _cursor, inbox) = tracked();
    for _ in 0..MAX_RETAINED_RECEIPTS {
        inbox
            .publish_tracked("pending".into(), "owner".into())
            .unwrap();
    }
    assert!(matches!(
        inbox.publish_tracked("overflow".into(), "owner".into()),
        Err(InboxError::ReceiptCapacity)
    ));
    assert_eq!(phase(&inbox, 1), DeliveryPhase::Accepted);
    assert_eq!(inbox.pending().unwrap().len(), MAX_RETAINED_RECEIPTS);
}

#[test]
fn panicked_mutation_owner_requires_reopen_not_mutex_poison_recovery() {
    let (_dir, rows, cursor, inbox) = tracked();
    inbox
        .publish_tracked("pending".into(), "owner".into())
        .unwrap();
    let _ = std::panic::catch_unwind(|| {
        let _state = inbox.state.lock().unwrap();
        panic!("interrupted mutation");
    });
    assert!(matches!(
        inbox.publish("new".into()),
        Err(InboxError::Poisoned)
    ));
    drop(inbox);
    assert_eq!(
        DurableInbox::<String, String>::open(rows, cursor)
            .unwrap()
            .pending()
            .unwrap()
            .len(),
        1
    );
}
