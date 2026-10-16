//! One durable single-consumer queue and its bounded receipt evidence.
//!
//! Receipt context stays in the original row and, after acknowledgement, in the
//! existing checkpoint. A current checkpoint is persisted before the first tracked
//! row. This type assumes one open owner per pair of paths.
//! The owner also controls both hierarchies against concurrent rename/removal;
//! symlink entries, targets and target ancestry must already be durable.

use std::collections::{BTreeMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use tidepool_atomic_write::DirectoryAnchor;
use tidepool_repr::jsonl::{self, SyncPolicy, TailPolicy};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(bound(deserialize = "T: Deserialize<'de>, R: Deserialize<'de>"))]
pub struct DurableEnvelope<T, R = ()> {
    pub sequence: u64,
    pub payload: T,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub publication: Option<PublicationStamp>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "deserialize_receipt_context"
    )]
    pub receipt_context: Option<R>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub redelivery: Option<Redelivery>,
}

/// Marks a tracked row that carries the payload and provenance of an earlier
/// row whose native input was withdrawn (see `DurableInbox::redeliver_withdrawn`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Redelivery {
    /// The retired sequence this row replaces.
    pub of: u64,
    /// Native input control had admitted the retired row and could not say
    /// whether it reached the model, so the recipient may have seen it.
    pub possibly_seen: bool,
}

fn deserialize_receipt_context<'de, D, R>(deserializer: D) -> Result<Option<R>, D::Error>
where
    D: serde::Deserializer<'de>,
    R: Deserialize<'de>,
{
    // A present null is valid provenance for R=(), not an untracked row.
    R::deserialize(deserializer).map(Some)
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PublicationStamp {
    pub stream: String,
    pub revision: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeliveryPhase {
    Accepted,
    InFlight,
    Submitted,
    Presented,
    Withdrawn,
    Rejected,
    Unconfirmed,
    Compacted,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReceiptEvidence<R> {
    pub context: R,
    pub phase: DeliveryPhase,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReceiptLookup<R> {
    Retained(ReceiptEvidence<R>),
    Unavailable,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields, bound(deserialize = "R: Deserialize<'de>"))]
struct InboxCheckpoint<R> {
    sequence: u64,
    watermarks: BTreeMap<String, u64>,
    receipts: BTreeMap<u64, ReceiptEvidence<R>>,
}

impl<R> Default for InboxCheckpoint<R> {
    fn default() -> Self {
        Self {
            sequence: 0,
            watermarks: BTreeMap::new(),
            receipts: BTreeMap::new(),
        }
    }
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields, bound(deserialize = "R: Deserialize<'de>"))]
struct VersionedCheckpoint<R> {
    version: u32,
    checkpoint: InboxCheckpoint<R>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InboxWriteOperation {
    Append,
    Checkpoint,
    Compaction,
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
    #[error("tracked receipt {sequence} is not retained")]
    ReceiptUnavailable { sequence: u64 },
    #[error("tracked receipt {sequence} cannot transition from {phase:?}")]
    ReceiptTransition { sequence: u64, phase: DeliveryPhase },
    #[error("tracked receipt {sequence} provenance does not match its original row")]
    ReceiptContextMismatch { sequence: u64 },
    #[error("tracked receipt {sequence} blocks this delivery/acknowledgement")]
    TrackedBarrier { sequence: u64 },
    #[error("receipt evidence capacity is exhausted")]
    ReceiptCapacity,
    #[error("receipt context exceeds {limit} serialized bytes")]
    ReceiptContextTooLarge { limit: usize },
    #[error("{operation:?} write is uncertain; reopen before any further delivery: {detail}")]
    UncertainWrite {
        operation: InboxWriteOperation,
        detail: String,
    },
    #[error("inbox mutation is fenced after an uncertain write; reopen required")]
    Poisoned,
}

const COMPACT_ACKNOWLEDGED_ROWS: u64 = 64;
pub const MAX_RETAINED_RECEIPTS: usize = 256;
pub const MAX_RECEIPT_CONTEXT_BYTES: usize = 4096;
pub const MAX_TOTAL_RECEIPT_CONTEXT_BYTES: usize = 65536;

struct InboxState<T, R> {
    checkpoint: InboxCheckpoint<R>,
    next_sequence: u64,
    compacted_through: u64,
    pending: VecDeque<DurableEnvelope<T, R>>,
    poisoned: bool,
}

pub struct DurableInbox<T, R = ()> {
    rows_path: PathBuf,
    cursor_path: PathBuf,
    state: Mutex<InboxState<T, R>>,
    #[cfg(test)]
    fault: Mutex<Option<FaultPoint>>,
}

/// A noncloneable send permit. Its fence was durable before this value escaped.
/// Dropping it leaves the operation unconfirmed; it never authorizes a retry.
pub struct DeliveryAttempt<'a, T, R>
where
    T: Clone + Serialize + DeserializeOwned,
    R: Clone + PartialEq + Serialize + DeserializeOwned,
{
    inbox: &'a DurableInbox<T, R>,
    envelope: DurableEnvelope<T, R>,
    finished: bool,
}

impl<T, R> DeliveryAttempt<'_, T, R>
where
    T: Clone + Serialize + DeserializeOwned,
    R: Clone + PartialEq + Serialize + DeserializeOwned,
{
    pub fn envelope(&self) -> &DurableEnvelope<T, R> {
        &self.envelope
    }
    /// Use only with proof that transport submission did not occur.
    pub fn not_submitted(mut self) -> Result<(), InboxError> {
        let result = self
            .inbox
            .finish_attempt(self.envelope.sequence, DeliveryPhase::Accepted);
        self.finished = result.is_ok();
        result
    }
    /// Native admission fences redispatch but does not acknowledge this row or
    /// permit a later row to overtake it.
    pub fn submitted(mut self) -> Result<(), InboxError> {
        let result = self
            .inbox
            .finish_attempt(self.envelope.sequence, DeliveryPhase::Submitted);
        self.finished = result.is_ok();
        result
    }
    pub fn unconfirmed(mut self) -> Result<(), InboxError> {
        let result = self
            .inbox
            .finish_attempt(self.envelope.sequence, DeliveryPhase::Unconfirmed);
        self.finished = result.is_ok();
        result
    }
}

impl<T, R> Drop for DeliveryAttempt<'_, T, R>
where
    T: Clone + Serialize + DeserializeOwned,
    R: Clone + PartialEq + Serialize + DeserializeOwned,
{
    fn drop(&mut self) {
        if !self.finished {
            if let Some(receipt) = lock_state(&self.inbox.state)
                .checkpoint
                .receipts
                .get_mut(&self.envelope.sequence)
            {
                if receipt.phase == DeliveryPhase::InFlight {
                    // Disk still has InFlight, which reopens as Unconfirmed.
                    receipt.phase = DeliveryPhase::Unconfirmed;
                }
            }
        }
    }
}

impl<T, R> DurableInbox<T, R>
where
    T: Clone + Serialize + DeserializeOwned,
    R: Clone + PartialEq + Serialize + DeserializeOwned,
{
    pub fn open(
        anchor: &DirectoryAnchor,
        relative_rows: impl AsRef<Path>,
        relative_cursor: impl AsRef<Path>,
    ) -> Result<Self, InboxError> {
        // Resolve both names before creating either parent so an invalid cursor
        // path cannot leave a newly-created rows hierarchy behind.
        let rows_path = anchor
            .resolve(relative_rows.as_ref())
            .map_err(std::io::Error::from)?;
        let cursor_path = anchor
            .resolve(relative_cursor.as_ref())
            .map_err(std::io::Error::from)?;
        create_parent(anchor, relative_rows.as_ref())?;
        create_parent(anchor, relative_cursor.as_ref())?;
        // Reject an unsupported checkpoint before repairing any row tail.
        let checkpoint = read_cursor::<R>(&cursor_path)?;
        let checkpoint_present = checkpoint.is_some();
        let mut checkpoint = checkpoint.unwrap_or_default();
        let rows = read_rows::<T, R>(&rows_path)?;
        // Reopening visible bytes after uncertain I/O is reconciliation, not proof
        // they reached storage. Stabilize both surviving files (including any
        // repaired row tail) before exposing receipts or a new send capability.
        stabilize_existing_file(&rows_path)?;
        stabilize_existing_file(&cursor_path)?;
        let cursor = checkpoint.sequence;
        let first = rows.first().map(|row| row.sequence);
        let last = rows.last().map(|row| row.sequence).unwrap_or(cursor);
        if rows.iter().any(|row| row.sequence == 0)
            || rows
                .windows(2)
                .any(|pair| pair[0].sequence.checked_add(1) != Some(pair[1].sequence))
        {
            return Err(InboxError::Corrupt(
                "noncontiguous or zero row sequence".into(),
            ));
        }
        if first.is_some_and(|first| first > cursor.saturating_add(1)) || cursor > last {
            return Err(InboxError::Corrupt(
                "cursor and retained rows disagree".into(),
            ));
        }
        for (sequence, evidence) in &mut checkpoint.receipts {
            if *sequence == 0 || *sequence > last {
                return Err(InboxError::Corrupt(
                    "receipt sequence outside published rows".into(),
                ));
            }
            context_bytes(&evidence.context)?;
            if *sequence <= cursor
                && !matches!(
                    evidence.phase,
                    DeliveryPhase::Presented | DeliveryPhase::Withdrawn | DeliveryPhase::Rejected
                )
            {
                return Err(InboxError::Corrupt(
                    "acknowledged receipt lacks consumer acceptance".into(),
                ));
            }
            if *sequence > cursor
                && matches!(
                    evidence.phase,
                    DeliveryPhase::Presented | DeliveryPhase::Withdrawn | DeliveryPhase::Rejected
                )
            {
                return Err(InboxError::Corrupt(
                    "accepted receipt was not atomically acknowledged".into(),
                ));
            }
            if evidence.phase == DeliveryPhase::InFlight {
                evidence.phase = DeliveryPhase::Unconfirmed;
            }
        }
        for row in &rows {
            if let Some(stamp) = &row.publication {
                checkpoint
                    .watermarks
                    .entry(stamp.stream.clone())
                    .and_modify(|value| *value = (*value).max(stamp.revision))
                    .or_insert(stamp.revision);
            }
            match &row.receipt_context {
                Some(context) => {
                    if !checkpoint_present {
                        return Err(InboxError::Corrupt(
                            "tracked row requires a prepublication checkpoint".into(),
                        ));
                    }
                    context_bytes(context)?;
                    if let Some(evidence) = checkpoint.receipts.get(&row.sequence) {
                        if evidence.context != *context {
                            return Err(InboxError::Corrupt(
                                "receipt provenance disagrees with original row".into(),
                            ));
                        }
                    } else if row.sequence > cursor {
                        // Append may commit before any later checkpoint update.
                        checkpoint.receipts.insert(
                            row.sequence,
                            ReceiptEvidence {
                                context: context.clone(),
                                phase: DeliveryPhase::Accepted,
                            },
                        );
                    }
                }
                None if checkpoint.receipts.contains_key(&row.sequence) => {
                    return Err(InboxError::Corrupt("receipt names an untracked row".into()));
                }
                None => {}
            }
        }
        if checkpoint.receipts.len() > MAX_RETAINED_RECEIPTS
            || total_context_bytes(&checkpoint.receipts)? > MAX_TOTAL_RECEIPT_CONTEXT_BYTES
        {
            return Err(InboxError::Corrupt("receipt quota exceeded on disk".into()));
        }
        let next_sequence = last
            .checked_add(1)
            .ok_or_else(|| InboxError::Corrupt("sequence exhausted".into()))?;
        Ok(Self {
            rows_path,
            cursor_path,
            state: Mutex::new(InboxState {
                checkpoint,
                next_sequence,
                compacted_through: first.map_or(cursor, |sequence| sequence.saturating_sub(1)),
                pending: rows
                    .into_iter()
                    .filter(|row| row.sequence > cursor)
                    .collect(),
                poisoned: false,
            }),
            #[cfg(test)]
            fault: Mutex::new(None),
        })
    }

    pub fn publish(&self, payload: T) -> Result<DurableEnvelope<T, R>, InboxError> {
        self.publish_inner(payload, None, None, None)?
            .ok_or_else(|| InboxError::Corrupt("unkeyed publication was deduplicated".into()))
    }
    pub fn publish_latest(
        &self,
        stream: String,
        revision: u64,
        payload: T,
    ) -> Result<Option<DurableEnvelope<T, R>>, InboxError> {
        self.publish_inner(
            payload,
            Some(PublicationStamp { stream, revision }),
            None,
            None,
        )
    }
    pub fn publish_tracked(
        &self,
        payload: T,
        context: R,
    ) -> Result<DurableEnvelope<T, R>, InboxError> {
        self.publish_inner(payload, None, Some(context), None)?
            .ok_or_else(|| InboxError::Corrupt("tracked publication was deduplicated".into()))
    }

    /// Retire the tracked front row `sequence`, whose native input was
    /// withdrawn, and queue its payload and provenance again as a new tracked
    /// row at the back of the queue. The retired sequence becomes `Withdrawn`
    /// and acknowledged, so it can never be sent again; the new row starts
    /// `Accepted` under a fresh sequence. With `possibly_seen` the native side
    /// had admitted the retired input without proving whether the model saw
    /// it; `Withdrawn` then records only that it will not be dispatched again.
    ///
    /// The append precedes the retirement. If the retirement write is lost,
    /// a repeated call finds the already-queued replacement and only retires,
    /// so a crash between the two writes never queues the payload twice.
    pub fn redeliver_withdrawn(
        &self,
        sequence: u64,
        context: &R,
        possibly_seen: bool,
    ) -> Result<DurableEnvelope<T, R>, InboxError> {
        let (payload, existing) = {
            let state = lock_state(&self.state);
            healthy(&state)?;
            let evidence = state
                .checkpoint
                .receipts
                .get(&sequence)
                .ok_or(InboxError::ReceiptUnavailable { sequence })?;
            if &evidence.context != context {
                return Err(InboxError::ReceiptContextMismatch { sequence });
            }
            if !matches!(
                evidence.phase,
                DeliveryPhase::Submitted | DeliveryPhase::Unconfirmed
            ) {
                return Err(InboxError::ReceiptTransition {
                    sequence,
                    phase: evidence.phase,
                });
            }
            let front = state
                .pending
                .front()
                .filter(|row| row.sequence == sequence)
                .ok_or(InboxError::TrackedBarrier { sequence })?;
            let existing = state
                .pending
                .iter()
                .find(|row| {
                    row.redelivery
                        .is_some_and(|redelivery| redelivery.of == sequence)
                })
                .cloned();
            (front.payload.clone(), existing)
        };
        let envelope = match existing {
            Some(envelope) => envelope,
            None => self
                .publish_inner(
                    payload,
                    None,
                    Some(context.clone()),
                    Some(Redelivery {
                        of: sequence,
                        possibly_seen,
                    }),
                )?
                .ok_or_else(|| InboxError::Corrupt("redelivery was deduplicated".into()))?,
        };
        self.transition_exact(sequence, context, DeliveryPhase::Withdrawn)?;
        Ok(envelope)
    }

    fn publish_inner(
        &self,
        payload: T,
        publication: Option<PublicationStamp>,
        context: Option<R>,
        redelivery: Option<Redelivery>,
    ) -> Result<Option<DurableEnvelope<T, R>>, InboxError> {
        let mut state = lock_state(&self.state);
        healthy(&state)?;
        if publication.as_ref().is_some_and(|stamp| {
            state
                .checkpoint
                .watermarks
                .get(&stamp.stream)
                .is_some_and(|revision| *revision >= stamp.revision)
        }) {
            return Ok(None);
        }
        let next_sequence = state
            .next_sequence
            .checked_add(1)
            .ok_or_else(|| InboxError::Corrupt("sequence exhausted".into()))?;
        let envelope = DurableEnvelope {
            sequence: state.next_sequence,
            payload,
            publication,
            receipt_context: context,
            redelivery,
        };
        let line = serde_json::to_string(&envelope).map_err(corrupt)?;
        if let Some(context) = &envelope.receipt_context {
            let bytes = context_bytes(context)?;
            let mut checkpoint = state.checkpoint.clone();
            while checkpoint.receipts.len() >= MAX_RETAINED_RECEIPTS
                || total_context_bytes(&checkpoint.receipts)? + bytes
                    > MAX_TOTAL_RECEIPT_CONTEXT_BYTES
            {
                let Some(oldest) = checkpoint
                    .receipts
                    .keys()
                    .copied()
                    .find(|sequence| *sequence <= checkpoint.sequence)
                else {
                    return Err(InboxError::ReceiptCapacity);
                };
                checkpoint.receipts.remove(&oldest);
            }
            // Persist prepublication evidence and eviction before publishing a tracked row.
            self.persist(&mut state, &checkpoint)?;
            state.checkpoint = checkpoint;
        }
        let append = self.append_row(&line);
        self.check_write(&mut state, InboxWriteOperation::Append, append)?;
        state.next_sequence = next_sequence;
        if let Some(stamp) = &envelope.publication {
            state
                .checkpoint
                .watermarks
                .insert(stamp.stream.clone(), stamp.revision);
        }
        if let Some(context) = &envelope.receipt_context {
            state.checkpoint.receipts.insert(
                envelope.sequence,
                ReceiptEvidence {
                    context: context.clone(),
                    phase: DeliveryPhase::Accepted,
                },
            );
        }
        state.pending.push_back(envelope.clone());
        Ok(Some(envelope))
    }

    /// Legacy whole-queue delivery fails closed before exposing tracked payloads.
    /// Mixed consumers must explicitly use `legacy_pending_prefix` and send permits.
    pub fn pending(&self) -> Result<Vec<DurableEnvelope<T, R>>, InboxError> {
        let state = lock_state(&self.state);
        healthy(&state)?;
        if let Some(row) = state
            .pending
            .iter()
            .find(|row| row.receipt_context.is_some())
        {
            return Err(InboxError::TrackedBarrier {
                sequence: row.sequence,
            });
        }
        Ok(state.pending.iter().cloned().collect())
    }
    /// Legacy delivery may not concatenate tracked notifications into its batch.
    pub fn legacy_pending_prefix(&self) -> Result<Vec<DurableEnvelope<T, R>>, InboxError> {
        let state = lock_state(&self.state);
        healthy(&state)?;
        Ok(state
            .pending
            .iter()
            .take_while(|row| row.receipt_context.is_none())
            .cloned()
            .collect())
    }
    /// Exact durable front row for an ordered consumer. Receipt state still
    /// decides whether this row may be submitted or only reconciled.
    pub fn front_pending(&self) -> Result<Option<DurableEnvelope<T, R>>, InboxError> {
        let state = lock_state(&self.state);
        healthy(&state)?;
        Ok(state.pending.front().cloned())
    }
    /// Last confirmed in-memory cursor; never evidence of model presentation.
    pub fn cursor(&self) -> u64 {
        lock_state(&self.state).checkpoint.sequence
    }
    pub fn watermark(&self) -> u64 {
        lock_state(&self.state).next_sequence.saturating_sub(1)
    }
    pub fn observe_receipt(&self, sequence: u64) -> Result<ReceiptLookup<R>, InboxError> {
        let state = lock_state(&self.state);
        healthy(&state)?;
        Ok(state
            .checkpoint
            .receipts
            .get(&sequence)
            .cloned()
            .map_or(ReceiptLookup::Unavailable, ReceiptLookup::Retained))
    }
    pub fn begin_tracked_delivery(
        &self,
        sequence: u64,
    ) -> Result<DeliveryAttempt<'_, T, R>, InboxError> {
        let mut state = lock_state(&self.state);
        healthy(&state)?;
        let evidence = state
            .checkpoint
            .receipts
            .get(&sequence)
            .ok_or(InboxError::ReceiptUnavailable { sequence })?;
        if evidence.phase != DeliveryPhase::Accepted {
            return Err(InboxError::ReceiptTransition {
                sequence,
                phase: evidence.phase,
            });
        }
        let envelope = state
            .pending
            .front()
            .filter(|row| row.sequence == sequence)
            .cloned()
            .ok_or(InboxError::TrackedBarrier { sequence })?;
        let mut checkpoint = state.checkpoint.clone();
        checkpoint
            .receipts
            .get_mut(&sequence)
            .ok_or(InboxError::ReceiptUnavailable { sequence })?
            .phase = DeliveryPhase::InFlight;
        self.persist(&mut state, &checkpoint)?;
        state.checkpoint = checkpoint;
        Ok(DeliveryAttempt {
            inbox: self,
            envelope,
            finished: false,
        })
    }
    /// The caller must have correlated actual model-input presentation to this
    /// row and its immutable producer scope. Neither legacy acknowledgement nor
    /// RPC acceptance calls this operation.
    pub fn confirm_presented_exact(&self, sequence: u64, context: &R) -> Result<(), InboxError> {
        self.transition_exact(sequence, context, DeliveryPhase::Presented)
    }
    /// Apply late native admission evidence without resubmitting this row.
    pub fn confirm_admitted(&self, sequence: u64, context: &R) -> Result<(), InboxError> {
        self.transition_exact(sequence, context, DeliveryPhase::Submitted)
    }
    /// Record proof that native dispatch was fenced before presentation.
    pub fn confirm_withdrawn(&self, sequence: u64, context: &R) -> Result<(), InboxError> {
        self.transition_exact(sequence, context, DeliveryPhase::Withdrawn)
    }
    /// Record terminal native evidence loss. This permanently fences redispatch
    /// and later rows, but is neither presentation nor consumer acknowledgement.
    pub fn confirm_compacted_exact(&self, sequence: u64, context: &R) -> Result<(), InboxError> {
        self.transition_exact(sequence, context, DeliveryPhase::Compacted)
    }
    /// Record a terminal native rejection that excludes later presentation.
    pub fn confirm_rejected(&self, sequence: u64, context: &R) -> Result<(), InboxError> {
        self.transition_exact(sequence, context, DeliveryPhase::Rejected)
    }
    fn transition_exact(
        &self,
        sequence: u64,
        context: &R,
        phase: DeliveryPhase,
    ) -> Result<(), InboxError> {
        {
            let state = lock_state(&self.state);
            healthy(&state)?;
            let evidence = state
                .checkpoint
                .receipts
                .get(&sequence)
                .ok_or(InboxError::ReceiptUnavailable { sequence })?;
            if &evidence.context != context {
                return Err(InboxError::ReceiptContextMismatch { sequence });
            }
        }
        self.transition(sequence, phase, false)
    }
    fn finish_attempt(&self, sequence: u64, phase: DeliveryPhase) -> Result<(), InboxError> {
        self.transition(sequence, phase, true)
    }
    fn transition(
        &self,
        sequence: u64,
        phase: DeliveryPhase,
        attempt: bool,
    ) -> Result<(), InboxError> {
        let mut state = lock_state(&self.state);
        healthy(&state)?;
        let current = state
            .checkpoint
            .receipts
            .get(&sequence)
            .ok_or(InboxError::ReceiptUnavailable { sequence })?
            .phase;
        if current == phase {
            return Ok(());
        }
        // A presentation observation may win a race with the transport's
        // admission reply. The weaker reply cannot overwrite it.
        if attempt && current == DeliveryPhase::Presented && phase == DeliveryPhase::Submitted {
            return Ok(());
        }
        let permitted = if attempt {
            current == DeliveryPhase::InFlight
        } else {
            match phase {
                DeliveryPhase::Submitted => matches!(
                    current,
                    DeliveryPhase::InFlight | DeliveryPhase::Unconfirmed
                ),
                DeliveryPhase::Presented => matches!(
                    current,
                    DeliveryPhase::InFlight | DeliveryPhase::Submitted | DeliveryPhase::Unconfirmed
                ),
                DeliveryPhase::Withdrawn | DeliveryPhase::Rejected => matches!(
                    current,
                    DeliveryPhase::Accepted
                        | DeliveryPhase::InFlight
                        | DeliveryPhase::Submitted
                        | DeliveryPhase::Unconfirmed
                ),
                DeliveryPhase::Compacted => matches!(
                    current,
                    DeliveryPhase::InFlight | DeliveryPhase::Submitted | DeliveryPhase::Unconfirmed
                ),
                DeliveryPhase::Accepted | DeliveryPhase::InFlight | DeliveryPhase::Unconfirmed => {
                    false
                }
            }
        };
        if !permitted {
            return Err(InboxError::ReceiptTransition {
                sequence,
                phase: current,
            });
        }
        let mut checkpoint = state.checkpoint.clone();
        checkpoint
            .receipts
            .get_mut(&sequence)
            .ok_or(InboxError::ReceiptUnavailable { sequence })?
            .phase = phase;
        if matches!(
            phase,
            DeliveryPhase::Presented | DeliveryPhase::Withdrawn | DeliveryPhase::Rejected
        ) && sequence > checkpoint.sequence
        {
            if state.pending.front().map(|row| row.sequence) != Some(sequence) {
                return Err(InboxError::TrackedBarrier { sequence });
            }
            checkpoint.sequence = sequence;
        }
        self.persist(&mut state, &checkpoint)?;
        state.checkpoint = checkpoint;
        self.discard_acknowledged(&mut state)
    }
    pub fn acknowledge(&self, sequence: u64) -> Result<(), InboxError> {
        let mut state = lock_state(&self.state);
        healthy(&state)?;
        if sequence < state.checkpoint.sequence {
            return Err(InboxError::AckRegression {
                current: state.checkpoint.sequence,
                requested: sequence,
            });
        }
        if sequence >= state.next_sequence {
            return Err(InboxError::AckBeyondEnd {
                last: state.next_sequence - 1,
                requested: sequence,
            });
        }
        if sequence == state.checkpoint.sequence {
            return Ok(());
        }
        if let Some(row) = state
            .pending
            .iter()
            .find(|row| row.sequence <= sequence && row.receipt_context.is_some())
        {
            return Err(InboxError::TrackedBarrier {
                sequence: row.sequence,
            });
        }
        let mut checkpoint = state.checkpoint.clone();
        checkpoint.sequence = sequence;
        self.persist(&mut state, &checkpoint)?;
        state.checkpoint = checkpoint;
        self.discard_acknowledged(&mut state)
    }
    fn discard_acknowledged(&self, state: &mut InboxState<T, R>) -> Result<(), InboxError> {
        while state
            .pending
            .front()
            .is_some_and(|row| row.sequence <= state.checkpoint.sequence)
        {
            state.pending.pop_front();
        }
        if state
            .checkpoint
            .sequence
            .saturating_sub(state.compacted_through)
            >= COMPACT_ACKNOWLEDGED_ROWS
        {
            let mut bytes = Vec::new();
            for row in &state.pending {
                serde_json::to_writer(&mut bytes, row).map_err(corrupt)?;
                bytes.push(b'\n');
            }
            let result = tidepool_atomic_write::write_durable(&self.rows_path, &bytes)
                .map_err(|error| error.to_string());
            self.check_write(state, InboxWriteOperation::Compaction, result)?;
            state.compacted_through = state.checkpoint.sequence;
        }
        Ok(())
    }
    fn persist(
        &self,
        state: &mut InboxState<T, R>,
        checkpoint: &InboxCheckpoint<R>,
    ) -> Result<(), InboxError> {
        let bytes = serde_json::to_vec(&VersionedCheckpoint {
            version: 2,
            checkpoint: checkpoint.clone(),
        })
        .map_err(corrupt)?;
        let result = self.write_checkpoint(&bytes);
        self.check_write(state, InboxWriteOperation::Checkpoint, result)
    }
    fn check_write(
        &self,
        state: &mut InboxState<T, R>,
        operation: InboxWriteOperation,
        result: Result<(), String>,
    ) -> Result<(), InboxError> {
        result.map_err(|detail| {
            state.poisoned = true;
            InboxError::UncertainWrite { operation, detail }
        })
    }
    fn append_row(&self, line: &str) -> Result<(), String> {
        jsonl::append_new_line(&self.rows_path, line, SyncPolicy::All)
            .map_err(|error| error.to_string())?;
        #[cfg(test)]
        self.fail_at(FaultPoint::AfterAppend)?;
        Ok(())
    }
    fn write_checkpoint(&self, bytes: &[u8]) -> Result<(), String> {
        tidepool_atomic_write::write_durable(&self.cursor_path, bytes)
            .map_err(|error| error.to_string())?;
        #[cfg(test)]
        self.fail_at(FaultPoint::AfterCheckpoint)?;
        Ok(())
    }
    #[cfg(test)]
    fn fail_at(&self, point: FaultPoint) -> Result<(), String> {
        let mut fault = lock(&self.fault);
        if *fault == Some(point) {
            *fault = None;
            return Err("injected error after durable write".into());
        }
        Ok(())
    }
}

#[cfg(test)]
#[derive(Clone, Copy, PartialEq, Eq)]
enum FaultPoint {
    AfterAppend,
    AfterCheckpoint,
}

fn healthy<T, R>(state: &InboxState<T, R>) -> Result<(), InboxError> {
    if state.poisoned {
        Err(InboxError::Poisoned)
    } else {
        Ok(())
    }
}
fn corrupt(error: impl std::fmt::Display) -> InboxError {
    InboxError::Corrupt(error.to_string())
}
fn context_bytes<R: Serialize>(context: &R) -> Result<usize, InboxError> {
    let length = serde_json::to_vec(context).map_err(corrupt)?.len();
    if length > MAX_RECEIPT_CONTEXT_BYTES {
        return Err(InboxError::ReceiptContextTooLarge {
            limit: MAX_RECEIPT_CONTEXT_BYTES,
        });
    }
    Ok(length)
}
fn total_context_bytes<R: Serialize>(
    receipts: &BTreeMap<u64, ReceiptEvidence<R>>,
) -> Result<usize, InboxError> {
    receipts.values().try_fold(0, |total, receipt| {
        Ok(total + context_bytes(&receipt.context)?)
    })
}
fn create_parent(anchor: &DirectoryAnchor, relative_path: &Path) -> Result<(), std::io::Error> {
    let parent = relative_path.parent().unwrap_or_else(|| Path::new(""));
    anchor
        .create_dir_all(parent)
        .map(|_| ())
        .map_err(|error| std::io::Error::new(error.source.kind(), error))
}

fn stabilize_existing_file(path: &Path) -> Result<(), std::io::Error> {
    let file = match std::fs::File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error),
    };
    file.sync_all()?;
    tidepool_atomic_write::sync_parent_directory(path)
        .map_err(|error| std::io::Error::new(error.source.kind(), error))
}
/// A read-only view of a durable inbox checkpoint: the delivery cursor and
/// any retained receipt evidence. Reuses the same current-format parsing path
/// as [`DurableInbox::open`], but never repairs or writes anything, so it is
/// safe for a reader that does not own the inbox files.
pub struct CheckpointView<R> {
    pub sequence: u64,
    pub receipts: BTreeMap<u64, ReceiptEvidence<R>>,
}

/// Reads the checkpoint at `path` without repairing or writing it. Missing
/// files read as an empty checkpoint, matching `DurableInbox::open`.
pub fn read_checkpoint<R: DeserializeOwned>(path: &Path) -> Result<CheckpointView<R>, InboxError> {
    let checkpoint = read_cursor::<R>(path)?.unwrap_or_default();
    Ok(CheckpointView {
        sequence: checkpoint.sequence,
        receipts: checkpoint.receipts,
    })
}

fn read_cursor<R: DeserializeOwned>(path: &Path) -> Result<Option<InboxCheckpoint<R>>, InboxError> {
    let bytes = match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    let stored: VersionedCheckpoint<R> = serde_json::from_slice(&bytes).map_err(corrupt)?;
    if stored.version != 2 {
        return Err(InboxError::Corrupt(format!(
            "unsupported checkpoint version {}; expected 2",
            stored.version
        )));
    }
    Ok(Some(stored.checkpoint))
}
fn read_rows<T: DeserializeOwned, R: DeserializeOwned>(
    path: &Path,
) -> Result<Vec<DurableEnvelope<T, R>>, InboxError> {
    // Only invalid JSON tail syntax is repairable; a valid row with a corrupt
    // schema/provenance is never truncated away as a supposed torn write.
    let (rows, _) = jsonl::read_tail(
        path,
        |line| serde_json::from_str::<serde_json::Value>(line).map_err(|error| error.to_string()),
        TailPolicy::Repair,
    )
    .map_err(corrupt)?;
    rows.into_iter()
        .map(|row| serde_json::from_value(row).map_err(corrupt))
        .collect()
}
fn lock_state<T, R>(
    mutex: &Mutex<InboxState<T, R>>,
) -> std::sync::MutexGuard<'_, InboxState<T, R>> {
    match mutex.lock() {
        Ok(state) => state,
        Err(poisoned) => {
            let mut state = poisoned.into_inner();
            state.poisoned = true;
            state
        }
    }
}
#[cfg(test)]
fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

#[cfg(test)]
mod tests;
