//! Real journal histories against an acknowledged-row list model.
//! This diagnostic cursor is not a live subscription or a power-loss simulator.

use super::*;
use crate::id::{BranchName, GitOid, WorktreeId};
use crate::monitor::{CommitReceipt, HeadChangeKind, HeadChangeReceipt};
use proptest::prelude::*;
use std::io::Write;

const TORN_ROW: &[u8] = b"{\"cursor\":999,\"event_id\":999,\"events\":[],\"recorded_at_ms\":0}";

#[derive(Clone, Debug)]
struct Change {
    worktree: u8,
    commits: Vec<String>,
    detached: bool,
}

fn changes() -> impl Strategy<Value = Change> {
    (
        0u8..3,
        proptest::collection::vec(
            proptest::collection::vec(
                prop_oneof![
                    Just('a'),
                    Just('"'),
                    Just('\\'),
                    Just('\n'),
                    Just('λ'),
                    Just('🦀')
                ],
                0..13,
            )
            .prop_map(|characters| characters.into_iter().collect::<String>()),
            1..4,
        ),
        any::<bool>(),
    )
        .prop_map(|(worktree, commits, detached)| Change {
            worktree,
            commits,
            detached,
        })
}

// Logical row numbers come from the model, never from a returned cursor or id.
// EventId is caller-supplied at this boundary; monitoring owns its issuance.
fn events(change: &Change, logical_row: usize) -> Vec<RepositoryEvent> {
    let worktree = WorktreeId::from_raw(format!("wt-model-{}", change.worktree));
    let oid = |offset: usize| GitOid::from_raw(format!("{:040x}", logical_row * 8 + offset));
    let gained: Vec<_> = (1..=change.commits.len()).map(oid).collect();
    let mut result: Vec<_> = change
        .commits
        .iter()
        .enumerate()
        .map(|(index, subject)| {
            RepositoryEvent::Commit(CommitReceipt {
                worktree: worktree.clone(),
                oid: gained[index].clone(),
                parents: vec![oid(index)],
                subject: subject.clone(),
                author: "model author".into(),
                committed_at_ms: logical_row as i64,
                files: vec![format!("file-{}", change.worktree)],
            })
        })
        .collect();
    result.push(RepositoryEvent::HeadChanged(HeadChangeReceipt {
        worktree,
        old_head: Some(oid(0)),
        new_head: gained.last().unwrap().clone(),
        kind: HeadChangeKind::Advanced(gained),
        branch: (!change.detached).then(|| BranchName::from_raw("exomonad/model")),
        observed_at_ms: logical_row as i64,
    }));
    result
}

#[derive(Clone, Debug)]
enum Op {
    Append(Change),
    EmptyBatch,
    Read(u8),
    ReadEnd,
    Reopen,
    CompetingOwner,
}

fn operations() -> impl Strategy<Value = Vec<Op>> {
    proptest::collection::vec(
        prop_oneof![
            4 => changes().prop_map(Op::Append),
            1 => Just(Op::EmptyBatch),
            2 => any::<u8>().prop_map(Op::Read),
            1 => Just(Op::ReadEnd),
            1 => Just(Op::Reopen),
            1 => Just(Op::CompetingOwner),
        ],
        0..17,
    )
}

#[derive(Debug)]
struct ModelRow {
    caller_event_id: u64,
    events: Vec<RepositoryEvent>,
}

#[derive(Default, Debug)]
struct Coverage {
    coemitted: usize,
    empty: usize,
    reads: usize,
    reopened: usize,
    owner_refusals: usize,
}

fn check_rows(actual: &[ObservationBatch], model: &[ModelRow], after: u64) {
    let expected: Vec<_> = model
        .iter()
        .enumerate()
        .filter(|(index, _)| (*index as u64 + 1) > after)
        .collect();
    assert_eq!(actual.len(), expected.len(), "missing or extra batches");
    for (row, (index, expected)) in actual.iter().zip(expected) {
        assert_eq!(row.cursor, index as u64 + 1);
        assert_eq!(row.event_id.0, expected.caller_event_id);
        assert_eq!(row.events, expected.events, "batch contents and order");
        assert!(row.recorded_at_ms >= 0);
    }
}

fn replay(first: Change, arbitrary: Vec<Op>, last: Change) {
    let directory = tempfile::tempdir().unwrap();
    let anchor = DirectoryAnchor::open_existing(directory.path()).unwrap();
    let mut journal = Some(EventJournal::open(&anchor, "new/deep/events.jsonl").unwrap());
    let mut model: Vec<ModelRow> = Vec::new();
    let mut coverage = Coverage::default();
    // Required interactions survive shrinking, with arbitrary sparse reads and
    // reopen points between the initial batch and the final append-after-reopen.
    let history = [Op::Append(first), Op::ReadEnd]
        .into_iter()
        .chain(arbitrary)
        .chain([
            Op::EmptyBatch,
            Op::Read(0),
            Op::Read(0),
            Op::CompetingOwner,
            Op::Reopen,
            Op::Append(last),
            Op::Read(0),
            Op::ReadEnd,
        ]);
    for op in history {
        match op {
            Op::Append(change) => {
                let payload = events(&change, model.len() + 1);
                coverage.coemitted += 1;
                append(journal.as_mut().unwrap(), &mut model, payload);
            }
            Op::EmptyBatch => {
                // Empty append is a successful empty ROW, not a no-op poll.
                coverage.empty += 1;
                append(journal.as_mut().unwrap(), &mut model, Vec::new());
            }
            Op::Read(selector) => {
                coverage.reads += 1;
                // Includes start, interior, current end and beyond end, without
                // choosing a cursor from potentially defective production rows.
                let cursor = u64::from(selector) % (model.len() as u64 + 2);
                let handle = journal.as_ref().unwrap();
                check_rows(&handle.since(cursor), &model, cursor);
                assert_eq!(handle.end_cursor(), model.len() as u64);
            }
            Op::ReadEnd => {
                coverage.reads += 1;
                let handle = journal.as_ref().unwrap();
                assert_eq!(handle.end_cursor(), model.len() as u64);
                assert!(handle.since(model.len() as u64).is_empty());
            }
            Op::Reopen => {
                let acknowledged = journal.as_ref().unwrap().since(0);
                drop(journal.take());
                journal = Some(EventJournal::open(&anchor, "new/deep/events.jsonl").unwrap());
                assert_eq!(journal.as_ref().unwrap().since(0), acknowledged);
                check_rows(&journal.as_ref().unwrap().since(0), &model, 0);
                coverage.reopened += 1;
            }
            Op::CompetingOwner => {
                let before = fs::read(directory.path().join("new/deep/events.jsonl")).unwrap();
                let error = EventJournal::open(&anchor, "new/deep/events.jsonl").unwrap_err();
                assert!(matches!(error, WorktreeError::StorageFailure { .. }));
                assert_eq!(
                    fs::read(directory.path().join("new/deep/events.jsonl")).unwrap(),
                    before
                );
                coverage.owner_refusals += 1;
            }
        }
    }
    drop(journal.take());
    let reopened = EventJournal::open(&anchor, "new/deep/events.jsonl").unwrap();
    check_rows(&reopened.since(0), &model, 0);
    assert!(coverage.coemitted >= 2 && coverage.empty >= 1);
    assert!(coverage.reads >= 5 && coverage.reopened >= 1 && coverage.owner_refusals >= 1);
}

fn append(journal: &mut EventJournal, model: &mut Vec<ModelRow>, events: Vec<RepositoryEvent>) {
    // Intentionally separated from the journal cursor and non-contiguous.
    let caller_event_id = 100 + model.len() as u64 * 7;
    let cursor = journal.append(&events, EventId(caller_event_id)).unwrap();
    assert_eq!(cursor, model.len() as u64 + 1);
    model.push(ModelRow {
        caller_event_id,
        events,
    });
}

fn property_config() -> proptest::test_runner::Config {
    let mut config = proptest::test_runner::Config::default();
    if let Some(path) = option_env!("TIDEPOOL_PROPTEST_REGRESSIONS") {
        config.failure_persistence = Some(Box::new(
            proptest::test_runner::FileFailurePersistence::Direct(path),
        ));
    }
    config
}

proptest! {
    #![proptest_config(property_config())]

    #[test]
    fn acknowledged_batch_histories_preserve_cursors_contents_and_exclusive_ownership(
        first in changes(), history in operations(), last in changes(),
    ) {
        replay(first, history, last);
    }

    #[test]
    fn torn_final_row_recovery_preserves_acknowledged_batches_and_next_cursor(
        change in changes(), cut in 1usize..TORN_ROW.len(),
    ) {
        let directory = tempfile::tempdir().unwrap();
        let anchor = DirectoryAnchor::open_existing(directory.path()).unwrap();
        let path = directory.path().join("events.jsonl");
        let mut journal = EventJournal::open(&anchor, "events.jsonl").unwrap();
        let payload = events(&change, 1);
        assert_eq!(journal.append(&payload, EventId(41)).unwrap(), 1);
        let acknowledged = journal.since(0);
        drop(journal);
        let durable = fs::read(&path).unwrap();
        // Proper prefixes of this object are incomplete JSON, without a newline.
        let mut file = OpenOptions::new().append(true).open(&path).unwrap();
        file.write_all(&TORN_ROW[..cut]).unwrap();
        file.sync_all().unwrap();
        drop(file);
        let mut recovered = EventJournal::open(&anchor, "events.jsonl").unwrap();
        prop_assert_eq!(recovered.since(0), acknowledged);
        prop_assert_eq!(fs::read(&path).unwrap(), durable);
        prop_assert_eq!(recovered.append(&payload, EventId(83)).unwrap(), 2);
        drop(recovered);
        let reopened = EventJournal::open(&anchor, "events.jsonl").unwrap();
        let rows = reopened.since(0);
        prop_assert_eq!(rows.len(), 2);
        prop_assert_eq!((rows[0].cursor, rows[0].event_id.0), (1, 41));
        prop_assert_eq!((rows[1].cursor, rows[1].event_id.0), (2, 83));
        prop_assert_eq!(&rows[0].events, &payload);
        prop_assert_eq!(&rows[1].events, &payload);
    }

    #[test]
    fn complete_wrong_shape_and_nonfinal_torn_rows_refuse_without_repair(change in changes()) {
        for defect in [b"{}\n".as_slice(), b"{\"cursor\":\n{}\n".as_slice()] {
            let directory = tempfile::tempdir().unwrap();
            let anchor = DirectoryAnchor::open_existing(directory.path()).unwrap();
            let path = directory.path().join("events.jsonl");
            let mut journal = EventJournal::open(&anchor, "events.jsonl").unwrap();
            journal.append(&events(&change, 1), EventId(41)).unwrap();
            drop(journal);
            let mut file = OpenOptions::new().append(true).open(&path).unwrap();
            file.write_all(defect).unwrap();
            file.sync_all().unwrap();
            drop(file);
            let corrupted = fs::read(&path).unwrap();
            let error = EventJournal::open(&anchor, "events.jsonl").unwrap_err();
            prop_assert!(matches!(error, WorktreeError::StorageFailure { .. }), "typed storage refusal");
            prop_assert_eq!(fs::read(&path).unwrap(), corrupted);
            // Refusal releases its owner lock; the same retained corruption is
            // diagnosed again instead of hiding a stale owner or replacing it.
            prop_assert!(EventJournal::open(&anchor, "events.jsonl").is_err());
        }
    }
}
