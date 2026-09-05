//! One durable single-consumer append-and-ack queue.
//!
//! Payload rows use [`tidepool_repr::jsonl`], cursor replacement uses
//! [`tidepool_atomic_write`], and this module owns the sequencing contract that
//! composes them: append first, deliver, then monotonically acknowledge.

use std::collections::{BTreeMap, VecDeque};
use std::path::PathBuf;
use std::sync::Mutex;

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use tidepool_repr::jsonl::{self, SyncPolicy, TailPolicy};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DurableEnvelope<T> {
    pub sequence: u64,
    pub payload: T,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub publication: Option<PublicationStamp>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PublicationStamp {
    pub stream: String,
    pub revision: u64,
}

#[derive(Default, Serialize, Deserialize)]
struct InboxCheckpoint {
    sequence: u64,
    watermarks: BTreeMap<String, u64>,
}

#[derive(Debug, thiserror::Error)]
pub enum InboxError {
    #[error("durable inbox io: {0}")]
    Io(#[from] std::io::Error),
    #[error("durable inbox is corrupt: {0}")]
    Corrupt(String),
    #[error("cannot acknowledge sequence {requested}; current cursor is {current}")]
    AckRegression { current: u64, requested: u64 },
    #[error("cannot acknowledge unpublished sequence {requested}; last published is {last}")]
    AckBeyondEnd { last: u64, requested: u64 },
}

const COMPACT_ACKNOWLEDGED_ROWS: u64 = 64;

struct InboxState<T> {
    watermarks: BTreeMap<String, u64>,
    next_sequence: u64,
    cursor: u64,
    compacted_through: u64,
    pending: VecDeque<DurableEnvelope<T>>,
}

/// Durable ordered delivery for one logical consumer.
///
/// All reads, appends, tail repair, and cursor writes share one lock. That is
/// load-bearing: repairing a torn tail concurrently with an append can
/// otherwise truncate a newly committed row.
pub struct DurableInbox<T> {
    rows_path: PathBuf,
    cursor_path: PathBuf,
    state: Mutex<InboxState<T>>,
}

impl<T> DurableInbox<T>
where
    T: Clone + Serialize + DeserializeOwned,
{
    pub fn open(rows_path: PathBuf, cursor_path: PathBuf) -> Result<Self, InboxError> {
        create_parent(&rows_path)?;
        create_parent(&cursor_path)?;
        let rows = read_rows::<T>(&rows_path)?;
        let checkpoint = read_cursor(&cursor_path)?;
        let cursor = checkpoint.sequence;
        let mut watermarks = checkpoint.watermarks;
        for row in &rows {
            if let Some(stamp) = &row.publication {
                watermarks
                    .entry(stamp.stream.clone())
                    .and_modify(|revision| *revision = (*revision).max(stamp.revision))
                    .or_insert(stamp.revision);
            }
        }
        let first = rows.first().map(|row| row.sequence);
        let last = rows.last().map(|row| row.sequence).unwrap_or(cursor);
        for pair in rows.windows(2) {
            if pair[1].sequence != pair[0].sequence + 1 {
                return Err(InboxError::Corrupt(format!(
                    "sequence {} follows {}",
                    pair[1].sequence, pair[0].sequence
                )));
            }
        }
        if let Some(first) = first {
            if first > cursor.saturating_add(1) {
                return Err(InboxError::Corrupt(format!(
                    "first retained sequence is {first}, beyond cursor {cursor}"
                )));
            }
        }
        if cursor > last {
            return Err(InboxError::Corrupt(format!(
                "cursor {cursor} is beyond last published sequence {last}"
            )));
        }
        Ok(Self {
            rows_path,
            cursor_path,
            state: Mutex::new(InboxState {
                watermarks,
                next_sequence: last + 1,
                cursor,
                compacted_through: first.map_or(cursor, |sequence| sequence.saturating_sub(1)),
                pending: rows
                    .into_iter()
                    .filter(|row| row.sequence > cursor)
                    .collect(),
            }),
        })
    }

    pub fn publish(&self, payload: T) -> Result<DurableEnvelope<T>, InboxError> {
        self.publish_inner(payload, None)
            .map(|envelope| envelope.expect("unkeyed publications are never deduplicated"))
    }

    /// Publish only when this stream advances. The stamp commits with the row;
    /// acknowledgement checkpoints it before compaction can remove that row.
    pub fn publish_latest(
        &self,
        stream: String,
        revision: u64,
        payload: T,
    ) -> Result<Option<DurableEnvelope<T>>, InboxError> {
        self.publish_inner(payload, Some(PublicationStamp { stream, revision }))
    }

    fn publish_inner(
        &self,
        payload: T,
        publication: Option<PublicationStamp>,
    ) -> Result<Option<DurableEnvelope<T>>, InboxError> {
        let mut state = lock(&self.state);
        if publication.as_ref().is_some_and(|stamp| {
            state
                .watermarks
                .get(&stamp.stream)
                .is_some_and(|revision| *revision >= stamp.revision)
        }) {
            return Ok(None);
        }
        let envelope = DurableEnvelope {
            sequence: state.next_sequence,
            payload,
            publication,
        };
        let line = serde_json::to_string(&envelope)
            .map_err(|error| InboxError::Corrupt(error.to_string()))?;
        jsonl::append_new_line(&self.rows_path, &line, SyncPolicy::All)?;
        state.next_sequence += 1;
        if let Some(stamp) = &envelope.publication {
            state
                .watermarks
                .insert(stamp.stream.clone(), stamp.revision);
        }
        state.pending.push_back(envelope.clone());
        Ok(Some(envelope))
    }

    pub fn pending(&self) -> Result<Vec<DurableEnvelope<T>>, InboxError> {
        let state = lock(&self.state);
        Ok(state.pending.iter().cloned().collect())
    }

    pub fn cursor(&self) -> u64 {
        lock(&self.state).cursor
    }

    /// Highest sequence durably published for this consumer.
    pub fn watermark(&self) -> u64 {
        lock(&self.state).next_sequence.saturating_sub(1)
    }

    /// Monotonically acknowledge delivery through `sequence`.
    ///
    /// Repeating the current ack is idempotent. Skipping intermediate rows is
    /// allowed only when the consumer has delivered the whole prefix and is
    /// acknowledging it as a batch.
    pub fn acknowledge(&self, sequence: u64) -> Result<(), InboxError> {
        let mut state = lock(&self.state);
        if sequence < state.cursor {
            return Err(InboxError::AckRegression {
                current: state.cursor,
                requested: sequence,
            });
        }
        let last = state.next_sequence - 1;
        if sequence > last {
            return Err(InboxError::AckBeyondEnd {
                last,
                requested: sequence,
            });
        }
        if sequence == state.cursor {
            return Ok(());
        }
        let checkpoint = serde_json::to_vec(&InboxCheckpoint {
            sequence,
            watermarks: state.watermarks.clone(),
        })
        .map_err(|error| InboxError::Corrupt(error.to_string()))?;
        tidepool_atomic_write::write_durable(&self.cursor_path, &checkpoint)
            .map_err(|error| InboxError::Corrupt(error.to_string()))?;
        state.cursor = sequence;
        while state
            .pending
            .front()
            .is_some_and(|row| row.sequence <= sequence)
        {
            state.pending.pop_front();
        }
        if state.cursor.saturating_sub(state.compacted_through) >= COMPACT_ACKNOWLEDGED_ROWS
            && rewrite_pending(&self.rows_path, &state.pending).is_ok()
        {
            state.compacted_through = state.cursor;
        }
        Ok(())
    }
}

fn rewrite_pending<T: Serialize>(
    path: &std::path::Path,
    pending: &VecDeque<DurableEnvelope<T>>,
) -> Result<(), InboxError> {
    let mut bytes = Vec::new();
    for envelope in pending {
        serde_json::to_writer(&mut bytes, envelope)
            .map_err(|error| InboxError::Corrupt(error.to_string()))?;
        bytes.push(b'\n');
    }
    tidepool_atomic_write::write_durable(path, &bytes)
        .map_err(|error| InboxError::Corrupt(error.to_string()))
}

fn create_parent(path: &std::path::Path) -> Result<(), std::io::Error> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    Ok(())
}

fn read_cursor(path: &std::path::Path) -> Result<InboxCheckpoint, InboxError> {
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum StoredCursor {
        Legacy(u64),
        Checkpoint(InboxCheckpoint),
    }
    match std::fs::read_to_string(path) {
        Ok(value) => serde_json::from_str::<StoredCursor>(&value)
            .map(|stored| match stored {
                StoredCursor::Legacy(sequence) => InboxCheckpoint {
                    sequence,
                    ..Default::default()
                },
                StoredCursor::Checkpoint(checkpoint) => checkpoint,
            })
            .map_err(|error| InboxError::Corrupt(format!("invalid cursor: {error}"))),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            Ok(InboxCheckpoint::default())
        }
        Err(error) => Err(error.into()),
    }
}

fn read_rows<T: DeserializeOwned>(
    path: &std::path::Path,
) -> Result<Vec<DurableEnvelope<T>>, InboxError> {
    let (rows, _torn) = jsonl::read_tail(
        path,
        |line| serde_json::from_str(line).map_err(|error| error.to_string()),
        TailPolicy::Repair,
    )
    .map_err(|error| InboxError::Corrupt(error.to_string()))?;
    Ok(rows)
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

#[cfg(test)]
mod tests {
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
}
