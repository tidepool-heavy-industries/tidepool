//! Public append/consumer/receipt/reopen histories against an in-memory event model.
//! Production owns the files and sequence issuance; the model uses input facts.

use super::*;
use proptest::prelude::*;
use proptest::test_runner::{Config, FileFailurePersistence, TestCaseError, TestRunner};
use std::cell::RefCell;

#[derive(Clone, Copy, Debug)]
enum Sequence {
    First,
    Front,
    Last,
    Cursor,
    Future,
    Slot(u8),
    Zero,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Native {
    Admitted,
    Presented,
    Withdrawn,
    Rejected,
    Compacted,
}

#[derive(Clone, Copy, Debug)]
enum Finish {
    NotSubmitted,
    Submitted,
    Unconfirmed,
    Drop,
}

#[derive(Clone, Debug)]
enum Operation {
    Append {
        payload: u8,
        context: Option<u8>,
    },
    Latest {
        stream: u8,
        revision: u8,
        payload: u8,
    },
    Attempt {
        sequence: Sequence,
        finish: Finish,
        race: Option<Native>,
        context: u8,
    },
    Native {
        sequence: Sequence,
        evidence: Native,
        context: u8,
    },
    Redeliver {
        sequence: Sequence,
        context: u8,
        possibly_seen: bool,
    },
    Ack(Sequence),
    Surface(Vec<Sequence>),
    Reopen,
    Read,
}

fn sequence_strategy() -> impl Strategy<Value = Sequence> {
    prop_oneof![3 => Just(Sequence::Front), 2 => Just(Sequence::Last), 1 => Just(Sequence::First),
        1 => Just(Sequence::Cursor), 1 => Just(Sequence::Future), 2 => (0u8..8).prop_map(Sequence::Slot), 1 => Just(Sequence::Zero)]
}

fn native_strategy() -> impl Strategy<Value = Native> {
    prop_oneof![
        Just(Native::Admitted),
        Just(Native::Presented),
        Just(Native::Withdrawn),
        Just(Native::Rejected),
        Just(Native::Compacted)
    ]
}

fn finish_strategy() -> impl Strategy<Value = Finish> {
    prop_oneof![
        Just(Finish::NotSubmitted),
        Just(Finish::Submitted),
        Just(Finish::Unconfirmed),
        Just(Finish::Drop)
    ]
}

fn operation_strategy() -> impl Strategy<Value = Operation> {
    prop_oneof![
        5 => (0u8..4, prop::option::of(0u8..3)).prop_map(|(payload, context)| Operation::Append { payload, context }),
        3 => (0u8..2, 0u8..4, 0u8..4).prop_map(|(stream, revision, payload)| Operation::Latest { stream, revision, payload }),
        5 => (sequence_strategy(), finish_strategy(), prop::option::of(native_strategy()), 0u8..3)
            .prop_map(|(sequence, finish, race, context)| Operation::Attempt { sequence, finish, race, context }),
        5 => (sequence_strategy(), native_strategy(), 0u8..3)
            .prop_map(|(sequence, evidence, context)| Operation::Native { sequence, evidence, context }),
        3 => (sequence_strategy(), 0u8..3, any::<bool>())
            .prop_map(|(sequence, context, possibly_seen)| Operation::Redeliver { sequence, context, possibly_seen }),
        4 => sequence_strategy().prop_map(Operation::Ack),
        2 => prop::collection::vec(sequence_strategy(), 0..4).prop_map(Operation::Surface),
        2 => Just(Operation::Reopen), 2 => Just(Operation::Read),
    ]
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Stage {
    Ready,
    Attempting,
    Admitted,
    Visible,
    Withdrawn,
    Rejected,
    Unknown,
    Compacted,
}

impl Stage {
    fn phase(self) -> DeliveryPhase {
        match self {
            Self::Ready => DeliveryPhase::Accepted,
            Self::Attempting => DeliveryPhase::InFlight,
            Self::Admitted => DeliveryPhase::Submitted,
            Self::Visible => DeliveryPhase::Presented,
            Self::Withdrawn => DeliveryPhase::Withdrawn,
            Self::Rejected => DeliveryPhase::Rejected,
            Self::Unknown => DeliveryPhase::Unconfirmed,
            Self::Compacted => DeliveryPhase::Compacted,
        }
    }
    fn acknowledges(self) -> bool {
        matches!(self, Self::Visible | Self::Withdrawn | Self::Rejected)
    }
}

impl Native {
    fn stage(self) -> Stage {
        match self {
            Self::Admitted => Stage::Admitted,
            Self::Presented => Stage::Visible,
            Self::Withdrawn => Stage::Withdrawn,
            Self::Rejected => Stage::Rejected,
            Self::Compacted => Stage::Compacted,
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
enum Refusal {
    Regression(u64, u64),
    Beyond(u64, u64),
    Unavailable(u64),
    Transition(u64, DeliveryPhase),
    Mismatch(u64),
    Barrier(u64),
}

fn actual_refusal(error: InboxError) -> Result<Refusal, TestCaseError> {
    Ok(match error {
        InboxError::AckRegression { current, requested } => Refusal::Regression(current, requested),
        InboxError::AckBeyondEnd { last, requested } => Refusal::Beyond(last, requested),
        InboxError::ReceiptUnavailable { sequence } => Refusal::Unavailable(sequence),
        InboxError::ReceiptTransition { sequence, phase } => Refusal::Transition(sequence, phase),
        InboxError::ReceiptContextMismatch { sequence } => Refusal::Mismatch(sequence),
        InboxError::TrackedBarrier { sequence } => Refusal::Barrier(sequence),
        other => {
            return Err(TestCaseError::fail(format!(
                "unexpected durable operation failure: {other:?}"
            )))
        }
    })
}

fn compare<T: std::fmt::Debug + PartialEq>(
    actual: Result<T, InboxError>,
    expected: Result<T, Refusal>,
    coverage: &mut Coverage,
) -> Result<(), TestCaseError> {
    coverage.refusals += usize::from(expected.is_err());
    if let Err(refusal) = &expected {
        match refusal {
            Refusal::Regression(..) => coverage.ack_regressions += 1,
            Refusal::Beyond(..) => coverage.ack_beyond_end += 1,
            Refusal::Unavailable(_) => coverage.receipt_unavailable += 1,
            Refusal::Transition(..) => coverage.receipt_transitions_refused += 1,
            Refusal::Mismatch(_) => coverage.context_mismatches += 1,
            Refusal::Barrier(_) => coverage.tracked_barriers += 1,
        }
    }
    let actual = match actual {
        Ok(value) => Ok(value),
        Err(error) => Err(actual_refusal(error)?),
    };
    prop_assert_eq!(actual, expected);
    Ok(())
}

// Rows are never removed from this oracle. Pending content, deduplication and
// acknowledgement barriers are full scans over original publication facts.
struct Model<R> {
    rows: Vec<DurableEnvelope<u8, R>>,
    receipts: Vec<(u64, R, Stage)>,
    cursor: u64,
    surfaced: Vec<u64>,
}

impl<R: Clone + PartialEq> Default for Model<R> {
    fn default() -> Self {
        Self {
            rows: vec![],
            receipts: vec![],
            cursor: 0,
            surfaced: vec![],
        }
    }
}

impl<R: Clone + PartialEq> Model<R> {
    fn sequence(&self, sequence: Sequence) -> u64 {
        match sequence {
            Sequence::First => 1,
            Sequence::Front => self
                .rows
                .iter()
                .find(|row| row.sequence > self.cursor)
                .map_or(0, |row| row.sequence),
            Sequence::Last => self.rows.len() as u64,
            Sequence::Cursor => self.cursor,
            Sequence::Future => self.rows.len() as u64 + 1,
            Sequence::Slot(key) => u64::from(key) + 1,
            Sequence::Zero => 0,
        }
    }

    fn append(
        &mut self,
        payload: u8,
        context: Option<R>,
        publication: Option<PublicationStamp>,
        redelivery: Option<Redelivery>,
    ) -> DurableEnvelope<u8, R> {
        let sequence = self.rows.len() as u64 + 1;
        let row = DurableEnvelope {
            sequence,
            payload,
            receipt_context: context.clone(),
            publication,
            redelivery,
        };
        if let Some(context) = context {
            self.receipts.push((sequence, context, Stage::Ready));
        }
        self.rows.push(row.clone());
        row
    }

    fn receipt(&self, sequence: u64) -> Result<(&R, Stage), Refusal> {
        self.receipts
            .iter()
            .find(|entry| entry.0 == sequence)
            .map(|(_, context, stage)| (context, *stage))
            .ok_or(Refusal::Unavailable(sequence))
    }

    fn set_stage(&mut self, sequence: u64, stage: Stage) {
        self.receipts
            .iter_mut()
            .find(|entry| entry.0 == sequence)
            .expect("accepted receipt fact exists")
            .2 = stage;
    }

    fn acknowledge(&mut self, sequence: u64) -> Result<(), Refusal> {
        if sequence < self.cursor {
            return Err(Refusal::Regression(self.cursor, sequence));
        }
        if sequence > self.rows.len() as u64 {
            return Err(Refusal::Beyond(self.rows.len() as u64, sequence));
        }
        if sequence != self.cursor {
            if let Some(row) = self.rows.iter().find(|row| {
                row.sequence > self.cursor
                    && row.sequence <= sequence
                    && row.receipt_context.is_some()
            }) {
                return Err(Refusal::Barrier(row.sequence));
            }
            self.advance(sequence);
        }
        Ok(())
    }

    fn advance(&mut self, sequence: u64) {
        self.cursor = sequence;
        self.surfaced.retain(|seen| *seen > sequence);
    }

    fn begin(&mut self, sequence: u64) -> Result<(), Refusal> {
        let (_, stage) = self.receipt(sequence)?;
        if stage != Stage::Ready {
            return Err(Refusal::Transition(sequence, stage.phase()));
        }
        if self.sequence(Sequence::Front) != sequence {
            return Err(Refusal::Barrier(sequence));
        }
        self.set_stage(sequence, Stage::Attempting);
        Ok(())
    }

    fn native(&mut self, sequence: u64, context: &R, evidence: Native) -> Result<(), Refusal> {
        let (original, stage) = self.receipt(sequence)?;
        if original != context {
            return Err(Refusal::Mismatch(sequence));
        }
        let destination = evidence.stage();
        if stage == destination {
            return Ok(());
        }
        let permitted = match evidence {
            Native::Admitted => matches!(stage, Stage::Attempting | Stage::Unknown),
            Native::Presented | Native::Compacted => {
                matches!(stage, Stage::Attempting | Stage::Admitted | Stage::Unknown)
            }
            Native::Withdrawn | Native::Rejected => matches!(
                stage,
                Stage::Ready | Stage::Attempting | Stage::Admitted | Stage::Unknown
            ),
        };
        if !permitted {
            return Err(Refusal::Transition(sequence, stage.phase()));
        }
        if destination.acknowledges()
            && sequence > self.cursor
            && self.sequence(Sequence::Front) != sequence
        {
            return Err(Refusal::Barrier(sequence));
        }
        self.set_stage(sequence, destination);
        if destination.acknowledges() && sequence > self.cursor {
            self.advance(sequence);
        }
        Ok(())
    }

    fn finish(&mut self, sequence: u64, finish: Finish) -> Result<(), Refusal> {
        let (_, stage) = self.receipt(sequence)?;
        if matches!(finish, Finish::Drop) {
            if stage == Stage::Attempting {
                self.set_stage(sequence, Stage::Unknown);
            }
            return Ok(());
        }
        let destination = match finish {
            Finish::NotSubmitted => Stage::Ready,
            Finish::Submitted => Stage::Admitted,
            Finish::Unconfirmed => Stage::Unknown,
            Finish::Drop => unreachable!("drop handled before terminal outcome"),
        };
        if stage == destination || (stage == Stage::Visible && destination == Stage::Admitted) {
            return Ok(());
        }
        if stage != Stage::Attempting {
            return Err(Refusal::Transition(sequence, stage.phase()));
        }
        self.set_stage(sequence, destination);
        Ok(())
    }

    fn redeliver(
        &mut self,
        sequence: u64,
        context: &R,
        possibly_seen: bool,
    ) -> Result<DurableEnvelope<u8, R>, Refusal> {
        let (original, stage) = self.receipt(sequence)?;
        if original != context {
            return Err(Refusal::Mismatch(sequence));
        }
        if !matches!(stage, Stage::Admitted | Stage::Unknown) {
            return Err(Refusal::Transition(sequence, stage.phase()));
        }
        if self.sequence(Sequence::Front) != sequence {
            return Err(Refusal::Barrier(sequence));
        }
        let payload = self
            .rows
            .iter()
            .find(|row| row.sequence == sequence)
            .expect("front row exists")
            .payload;
        let replacement = self.append(
            payload,
            Some(context.clone()),
            None,
            Some(Redelivery {
                of: sequence,
                possibly_seen,
            }),
        );
        self.set_stage(sequence, Stage::Withdrawn);
        self.advance(sequence);
        Ok(replacement)
    }
}

#[derive(Debug, Default)]
struct Coverage {
    callbacks: usize,
    operations: usize,
    tracked_appends: usize,
    null_context_appends: usize,
    untracked_appends: usize,
    latest_appends: usize,
    latest_duplicates: usize,
    attempts: usize,
    attempt_races: usize,
    presented: usize,
    admitted: usize,
    withdrawn: usize,
    rejected: usize,
    compacted: usize,
    redelivered: usize,
    ack_advanced: usize,
    refusals: usize,
    ack_regressions: usize,
    ack_beyond_end: usize,
    receipt_unavailable: usize,
    receipt_transitions_refused: usize,
    context_mismatches: usize,
    tracked_barriers: usize,
    successful_after_refusal: usize,
    reopens: usize,
    reads: usize,
    blocked_legacy_reads: usize,
    surfaced: usize,
    max_pending: usize,
}

impl Coverage {
    fn native_accepted(&mut self, evidence: Native) {
        match evidence {
            Native::Admitted => self.admitted += 1,
            Native::Presented => self.presented += 1,
            Native::Withdrawn => self.withdrawn += 1,
            Native::Rejected => self.rejected += 1,
            Native::Compacted => self.compacted += 1,
        }
    }
}

fn apply_native<R>(
    inbox: &DurableInbox<u8, R>,
    sequence: u64,
    context: &R,
    evidence: Native,
) -> Result<(), InboxError>
where
    R: Clone + PartialEq + Serialize + DeserializeOwned,
{
    match evidence {
        Native::Admitted => inbox.confirm_admitted(sequence, context),
        Native::Presented => inbox.confirm_presented_exact(sequence, context),
        Native::Withdrawn => inbox.confirm_withdrawn(sequence, context),
        Native::Rejected => inbox.confirm_rejected(sequence, context),
        Native::Compacted => inbox.confirm_compacted_exact(sequence, context),
    }
}

fn observe<R>(
    inbox: &DurableInbox<u8, R>,
    model: &Model<R>,
    coverage: &mut Coverage,
) -> Result<(), TestCaseError>
where
    R: Clone + PartialEq + std::fmt::Debug + Serialize + DeserializeOwned,
{
    coverage.reads += 1;
    prop_assert_eq!(inbox.cursor(), model.cursor);
    prop_assert_eq!(inbox.watermark(), model.rows.len() as u64);
    let pending = model
        .rows
        .iter()
        .filter(|row| row.sequence > model.cursor)
        .cloned()
        .collect::<Vec<_>>();
    coverage.max_pending = coverage.max_pending.max(pending.len());
    prop_assert_eq!(
        inbox
            .front_pending()
            .map_err(|error| TestCaseError::fail(format!("front: {error:?}")))?,
        pending.first().cloned()
    );
    let prefix = pending
        .iter()
        .take_while(|row| row.receipt_context.is_none())
        .cloned()
        .collect::<Vec<_>>();
    prop_assert_eq!(
        inbox
            .legacy_pending_prefix()
            .map_err(|error| TestCaseError::fail(format!("prefix: {error:?}")))?,
        prefix
    );
    let barrier = pending
        .iter()
        .find(|row| row.receipt_context.is_some())
        .map(|row| row.sequence);
    let expected_pending = barrier.map_or_else(
        || Ok(pending.clone()),
        |sequence| Err(Refusal::Barrier(sequence)),
    );
    let actual_pending = match inbox.pending() {
        Ok(rows) => Ok(rows),
        Err(error) => Err(actual_refusal(error)?),
    };
    coverage.blocked_legacy_reads += usize::from(barrier.is_some());
    prop_assert_eq!(actual_pending, expected_pending);
    let notices = pending
        .iter()
        .filter(|row| row.receipt_context.is_none() && !model.surfaced.contains(&row.sequence))
        .cloned()
        .collect::<Vec<_>>();
    prop_assert_eq!(
        inbox
            .legacy_notices_beyond_barrier()
            .map_err(|error| TestCaseError::fail(format!("notices: {error:?}")))?,
        notices
    );
    let mut surfaced = model.surfaced.clone();
    surfaced.sort_unstable();
    prop_assert_eq!(
        inbox
            .surfaced_out_of_order()
            .into_iter()
            .collect::<Vec<_>>(),
        surfaced
    );
    for sequence in 0..=model.rows.len() as u64 + 1 {
        let expected =
            model
                .receipt(sequence)
                .map_or(ReceiptLookup::Unavailable, |(context, stage)| {
                    ReceiptLookup::Retained(ReceiptEvidence {
                        context: context.clone(),
                        phase: stage.phase(),
                    })
                });
        prop_assert_eq!(
            inbox
                .observe_receipt(sequence)
                .map_err(|error| TestCaseError::fail(format!("receipt: {error:?}")))?,
            expected
        );
    }
    Ok(())
}

fn replay<R>(
    operations: &[Operation],
    contexts: &[R],
    observe_each: bool,
    coverage: &mut Coverage,
) -> Result<(), TestCaseError>
where
    R: Clone + PartialEq + std::fmt::Debug + Serialize + DeserializeOwned,
{
    coverage.callbacks += 1;
    let directory = tempfile::tempdir().map_err(|error| TestCaseError::fail(error.to_string()))?;
    let anchor = DirectoryAnchor::open_existing(directory.path())
        .map_err(|error| TestCaseError::fail(error.to_string()))?;
    let mut inbox = DurableInbox::<u8, R>::open(&anchor, "rows.jsonl", "cursor")
        .map_err(|error| TestCaseError::fail(error.to_string()))?;
    let mut model = Model::default();
    observe(&inbox, &model, coverage)?;
    let mut preceding_refusal = false;
    for (index, operation) in operations.iter().enumerate() {
        coverage.operations += 1;
        let previous_refusals = coverage.refusals;
        match *operation {
            Operation::Append { payload, context } => {
                let context = context.map(|key| contexts[key as usize].clone());
                let expected = model.append(payload, context.clone(), None, None);
                let actual = match context {
                    Some(context) => {
                        coverage.tracked_appends += 1;
                        coverage.null_context_appends +=
                            usize::from(serde_json::to_value(&context).unwrap().is_null());
                        inbox.publish_tracked(payload, context)
                    }
                    None => {
                        coverage.untracked_appends += 1;
                        inbox.publish(payload)
                    }
                };
                compare(actual, Ok(expected), coverage)?;
            }
            Operation::Latest {
                stream,
                revision,
                payload,
            } => {
                let stream = format!("stream-{stream}");
                let duplicate = model
                    .rows
                    .iter()
                    .filter_map(|row| row.publication.as_ref())
                    .any(|stamp| stamp.stream == stream && stamp.revision >= u64::from(revision));
                let expected = if duplicate {
                    coverage.latest_duplicates += 1;
                    None
                } else {
                    coverage.latest_appends += 1;
                    Some(model.append(
                        payload,
                        None,
                        Some(PublicationStamp {
                            stream: stream.clone(),
                            revision: u64::from(revision),
                        }),
                        None,
                    ))
                };
                compare(
                    inbox.publish_latest(stream, u64::from(revision), payload),
                    Ok(expected),
                    coverage,
                )?;
            }
            Operation::Attempt {
                sequence,
                finish,
                race,
                context,
            } => {
                let sequence = model.sequence(sequence);
                let expected = model.begin(sequence);
                match (inbox.begin_tracked_delivery(sequence), expected) {
                    (Ok(attempt), Ok(())) => {
                        coverage.attempts += 1;
                        let original = model
                            .rows
                            .iter()
                            .find(|row| row.sequence == sequence)
                            .unwrap();
                        prop_assert_eq!(attempt.envelope(), original);
                        // Establish the real in-flight premise before injecting
                        // the native observation; this read does not consume it.
                        prop_assert!(
                            matches!(
                                inbox.observe_receipt(sequence),
                                Ok(ReceiptLookup::Retained(ReceiptEvidence {
                                    phase: DeliveryPhase::InFlight,
                                    ..
                                }))
                            ),
                            "admitted receipt {} must be InFlight before native evidence",
                            sequence
                        );
                        if let Some(evidence) = race {
                            coverage.attempt_races += 1;
                            let context = &contexts[context as usize];
                            let expected = model.native(sequence, context, evidence);
                            if expected.is_ok() {
                                coverage.native_accepted(evidence);
                            }
                            compare(
                                apply_native(&inbox, sequence, context, evidence),
                                expected,
                                coverage,
                            )?;
                        }
                        let expected = model.finish(sequence, finish);
                        let actual = match finish {
                            Finish::NotSubmitted => attempt.not_submitted(),
                            Finish::Submitted => attempt.submitted(),
                            Finish::Unconfirmed => attempt.unconfirmed(),
                            Finish::Drop => {
                                drop(attempt);
                                Ok(())
                            }
                        };
                        compare(actual, expected, coverage)?;
                    }
                    (Err(actual), Err(expected)) => {
                        compare::<()>(Err(actual), Err(expected), coverage)?
                    }
                    (actual, expected) => {
                        return Err(TestCaseError::fail(format!(
                            "begin disagreement at {index}: admitted={} expected={expected:?}",
                            actual.is_ok()
                        )))
                    }
                }
            }
            Operation::Native {
                sequence,
                evidence,
                context,
            } => {
                let sequence = model.sequence(sequence);
                let context = &contexts[context as usize];
                let expected = model.native(sequence, context, evidence);
                if expected.is_ok() {
                    coverage.native_accepted(evidence);
                }
                compare(
                    apply_native(&inbox, sequence, context, evidence),
                    expected,
                    coverage,
                )?;
            }
            Operation::Redeliver {
                sequence,
                context,
                possibly_seen,
            } => {
                let sequence = model.sequence(sequence);
                let context = &contexts[context as usize];
                let expected = model.redeliver(sequence, context, possibly_seen);
                coverage.redelivered += usize::from(expected.is_ok());
                compare(
                    inbox.redeliver_withdrawn(sequence, context, possibly_seen),
                    expected,
                    coverage,
                )?;
            }
            Operation::Ack(sequence) => {
                let sequence = model.sequence(sequence);
                let previous = model.cursor;
                let expected = model.acknowledge(sequence);
                coverage.ack_advanced += usize::from(model.cursor > previous);
                compare(inbox.acknowledge(sequence), expected, coverage)?;
            }
            Operation::Surface(ref sequences) => {
                let sequences = sequences
                    .iter()
                    .map(|sequence| model.sequence(*sequence))
                    .collect::<Vec<_>>();
                inbox.mark_surfaced_out_of_order(sequences.clone());
                for sequence in sequences {
                    if !model.surfaced.contains(&sequence) {
                        model.surfaced.push(sequence);
                        coverage.surfaced += 1;
                    }
                }
            }
            Operation::Reopen => {
                // The previous owner is closed before a new one opens these paths.
                drop(inbox);
                inbox = DurableInbox::open(&anchor, "rows.jsonl", "cursor").map_err(|error| {
                    TestCaseError::fail(format!("reopen at {index}: {error:?}"))
                })?;
                model.surfaced.clear();
                coverage.reopens += 1;
            }
            Operation::Read => observe(&inbox, &model, coverage)?,
        }
        let refused = coverage.refusals > previous_refusals;
        coverage.successful_after_refusal += usize::from(preceding_refusal && !refused);
        preceding_refusal = refused;
        if observe_each || index + 1 == operations.len() {
            observe(&inbox, &model, coverage)?;
        }
    }
    Ok(())
}

fn guided(context: u8, finish: Finish, possibly_seen: bool) -> Vec<Operation> {
    vec![
        Operation::Latest {
            stream: 0,
            revision: 4,
            payload: 0,
        },
        Operation::Ack(Sequence::Front),
        Operation::Append {
            payload: 1,
            context: Some(context),
        },
        Operation::Append {
            payload: 2,
            context: None,
        },
        Operation::Attempt {
            sequence: Sequence::Front,
            finish,
            race: None,
            context,
        },
        Operation::Read,
        Operation::Ack(Sequence::Last),
        Operation::Surface(vec![Sequence::Last]),
        Operation::Reopen,
        Operation::Native {
            sequence: Sequence::Front,
            evidence: Native::Admitted,
            context,
        },
        Operation::Redeliver {
            sequence: Sequence::Front,
            context,
            possibly_seen,
        },
        Operation::Ack(Sequence::Front),
        Operation::Attempt {
            sequence: Sequence::Front,
            finish: Finish::Submitted,
            race: Some(Native::Presented),
            context,
        },
        Operation::Reopen,
        Operation::Latest {
            stream: 0,
            revision: 4,
            payload: 3,
        },
        Operation::Latest {
            stream: 0,
            revision: 5,
            payload: 3,
        },
        Operation::Ack(Sequence::Last),
    ]
}

fn prefix_strategy() -> impl Strategy<Value = Operation> {
    // Prior untracked history is freely mutable and cannot leave a tracked
    // barrier that preempts the causal delivery/redelivery core.
    prop_oneof![
        3 => (0u8..2, 0u8..4, 0u8..4).prop_map(|(stream, revision, payload)| Operation::Latest { stream, revision, payload }),
        2 => (0u8..4).prop_map(|payload| Operation::Append { payload, context: None }),
        1 => prop::collection::vec(sequence_strategy(), 0..4).prop_map(Operation::Surface),
        1 => Just(Operation::Read), 1 => Just(Operation::Reopen),
    ]
}

fn histories() -> impl Strategy<Value = (Vec<Operation>, bool, bool)> {
    let general = prop::collection::vec(operation_strategy(), 0..33);
    let guided = (
        0u8..3,
        finish_strategy(),
        any::<bool>(),
        prop::collection::vec(prefix_strategy(), 0..8),
        prop::collection::vec(operation_strategy(), 0..12),
    )
        .prop_map(|(context, finish, possibly_seen, prefix, suffix)| {
            let mut operations = prefix;
            operations.push(Operation::Ack(Sequence::Last));
            operations.extend(guided(context, finish, possibly_seen));
            operations.extend(suffix);
            operations
        });
    (
        prop_oneof![1 => general, 2 => guided],
        any::<bool>(),
        any::<bool>(),
    )
}

fn configuration(name: &'static str) -> Config {
    let mut config = Config {
        source_file: Some(file!()),
        test_name: Some(name),
        ..Config::default()
    };
    if std::env::var_os("PROPTEST_CASES").is_none() {
        config.cases = 64;
    }
    if let Some(path) = option_env!("TIDEPOOL_PROPTEST_REGRESSIONS") {
        config.failure_persistence = Some(Box::new(FileFailurePersistence::Direct(path)));
    }
    let mut config = proptest::test_runner::contextualize_config(config);
    config.source_file = Some(file!());
    config.test_name = Some(name);
    config
}

#[test]
fn generated_inbox_delivery_histories_match_sequence_and_receipt_model() {
    let config = configuration(concat!(
        module_path!(),
        "::generated_inbox_delivery_histories_match_sequence_and_receipt_model"
    ));
    eprintln!("durable inbox configured fresh cases: {}", config.cases);
    let mut runner = TestRunner::new(config);
    let coverage = RefCell::new(Coverage::default());
    let result = runner.run(&histories(), |(operations, unit_context, observe_each)| {
        if unit_context {
            replay(
                &operations,
                &[(), (), ()],
                observe_each,
                &mut coverage.borrow_mut(),
            )
        } else {
            replay(
                &operations,
                &[
                    serde_json::Value::Null,
                    serde_json::json!(0),
                    serde_json::json!({"actor": 1, "incarnation": 2}),
                ],
                observe_each,
                &mut coverage.borrow_mut(),
            )
        }
    });
    eprintln!(
        "durable inbox generated coverage (callbacks include seed replay/shrink): {:#?}",
        coverage.borrow()
    );
    result.unwrap();
}

#[test]
fn deterministic_inbox_support_covers_delivery_reopen_and_redelivery() {
    let mut coverage = Coverage::default();
    for finish in [
        Finish::NotSubmitted,
        Finish::Submitted,
        Finish::Unconfirmed,
        Finish::Drop,
    ] {
        for possibly_seen in [false, true] {
            for observe_each in [false, true] {
                replay(
                    &guided(0, finish, possibly_seen),
                    &[(), (), ()],
                    observe_each,
                    &mut coverage,
                )
                .unwrap();
                replay(
                    &guided(0, finish, possibly_seen),
                    &[
                        serde_json::Value::Null,
                        serde_json::json!(0),
                        serde_json::json!(1),
                    ],
                    observe_each,
                    &mut coverage,
                )
                .unwrap();
            }
        }
    }
    assert!(coverage.tracked_appends > 0 && coverage.null_context_appends > 0);
    assert!(coverage.untracked_appends > 0 && coverage.latest_duplicates > 0);
    assert!(coverage.attempts > 0 && coverage.attempt_races > 0);
    assert!(coverage.redelivered > 0 && coverage.ack_advanced > 0);
    assert!(coverage.refusals > 0 && coverage.successful_after_refusal > 0);
    assert!(coverage.reopens > 0 && coverage.surfaced > 0);
    eprintln!("durable inbox deterministic support coverage: {coverage:#?}");
}

#[test]
fn deterministic_inbox_native_evidence_covers_all_terminal_barriers() {
    let mut coverage = Coverage::default();
    for evidence in [
        Native::Admitted,
        Native::Presented,
        Native::Withdrawn,
        Native::Rejected,
        Native::Compacted,
    ] {
        for finish in [
            Finish::NotSubmitted,
            Finish::Submitted,
            Finish::Unconfirmed,
            Finish::Drop,
        ] {
            for observe_each in [false, true] {
                let operations = vec![
                    Operation::Append {
                        payload: 1,
                        context: Some(0),
                    },
                    Operation::Attempt {
                        sequence: Sequence::Front,
                        finish,
                        race: Some(evidence),
                        context: 0,
                    },
                    Operation::Reopen,
                    Operation::Append {
                        payload: 2,
                        context: None,
                    },
                    Operation::Ack(Sequence::Last),
                    Operation::Read,
                ];
                replay(&operations, &[(), (), ()], observe_each, &mut coverage).unwrap();
                replay(
                    &operations,
                    &[
                        serde_json::Value::Null,
                        serde_json::json!(0),
                        serde_json::json!(1),
                    ],
                    observe_each,
                    &mut coverage,
                )
                .unwrap();
            }
        }
    }
    assert!(coverage.attempt_races > 0 && coverage.refusals > 0);
    assert!(
        coverage.admitted > 0
            && coverage.presented > 0
            && coverage.withdrawn > 0
            && coverage.rejected > 0
            && coverage.compacted > 0
    );
    eprintln!("durable inbox native evidence support coverage: {coverage:#?}");
}

#[test]
fn acknowledged_receipt_retention_crosses_limit_and_row_compaction() {
    let directory = tempfile::tempdir().unwrap();
    let anchor = DirectoryAnchor::open_existing(directory.path()).unwrap();
    let mut inbox =
        DurableInbox::<u8, serde_json::Value>::open(&anchor, "rows.jsonl", "cursor").unwrap();
    for index in 0..(MAX_RETAINED_RECEIPTS + 2) {
        let row = inbox
            .publish_tracked((index % 4) as u8, serde_json::Value::Null)
            .unwrap();
        assert_eq!(row.sequence, index as u64 + 1);
        assert_eq!(
            inbox.observe_receipt(row.sequence).unwrap(),
            ReceiptLookup::Retained(ReceiptEvidence {
                context: serde_json::Value::Null,
                phase: DeliveryPhase::Accepted
            })
        );
        inbox
            .confirm_rejected(row.sequence, &serde_json::Value::Null)
            .unwrap();
        assert_eq!(inbox.cursor(), row.sequence);
        if index == MAX_RETAINED_RECEIPTS - 1
            || index == MAX_RETAINED_RECEIPTS
            || index == MAX_RETAINED_RECEIPTS + 1
        {
            drop(inbox);
            inbox = DurableInbox::open(&anchor, "rows.jsonl", "cursor").unwrap();
            // All originals are acknowledged. The retained receipt set is the
            // latest capacity-sized suffix, independently of queue storage.
            let first_retained = row.sequence.saturating_sub(MAX_RETAINED_RECEIPTS as u64) + 1;
            for sequence in 1..=row.sequence {
                let expected = if sequence < first_retained {
                    ReceiptLookup::Unavailable
                } else {
                    ReceiptLookup::Retained(ReceiptEvidence {
                        context: serde_json::Value::Null,
                        phase: DeliveryPhase::Rejected,
                    })
                };
                assert_eq!(
                    inbox.observe_receipt(sequence).unwrap(),
                    expected,
                    "sequence {sequence} at publication {}",
                    row.sequence
                );
            }
            assert!(inbox.front_pending().unwrap().is_none());
        }
    }
    // Production row compaction has crossed its acknowledged-row thresholds. Receipt
    // evidence remains independently retained in the durable checkpoint.
    assert_eq!(inbox.watermark(), MAX_RETAINED_RECEIPTS as u64 + 2);
    let rows = std::fs::read_to_string(directory.path().join("rows.jsonl")).unwrap();
    assert_eq!(
        rows.lines().count(),
        (MAX_RETAINED_RECEIPTS + 2) % COMPACT_ACKNOWLEDGED_ROWS as usize
    );
}

#[test]
fn deterministic_inbox_context_refusals_preserve_continued_delivery() {
    let operations = vec![
        Operation::Append {
            payload: 1,
            context: Some(0),
        },
        Operation::Attempt {
            sequence: Sequence::Front,
            finish: Finish::Submitted,
            race: None,
            context: 0,
        },
        Operation::Native {
            sequence: Sequence::Front,
            evidence: Native::Presented,
            context: 1,
        },
        Operation::Redeliver {
            sequence: Sequence::Front,
            context: 2,
            possibly_seen: false,
        },
        Operation::Ack(Sequence::Last),
        Operation::Reopen,
        Operation::Native {
            sequence: Sequence::Front,
            evidence: Native::Presented,
            context: 0,
        },
        Operation::Ack(Sequence::Zero),
        Operation::Ack(Sequence::Future),
        Operation::Native {
            sequence: Sequence::First,
            evidence: Native::Admitted,
            context: 0,
        },
        Operation::Native {
            sequence: Sequence::Future,
            evidence: Native::Presented,
            context: 0,
        },
        Operation::Append {
            payload: 2,
            context: Some(2),
        },
        Operation::Attempt {
            sequence: Sequence::Front,
            finish: Finish::Drop,
            race: None,
            context: 2,
        },
        Operation::Reopen,
        Operation::Native {
            sequence: Sequence::Front,
            evidence: Native::Rejected,
            context: 2,
        },
        Operation::Read,
    ];
    let mut coverage = Coverage::default();
    for observe_each in [false, true] {
        replay(
            &operations,
            &[
                serde_json::Value::Null,
                serde_json::json!(0),
                serde_json::json!({"actor": 1, "incarnation": 2}),
            ],
            observe_each,
            &mut coverage,
        )
        .unwrap();
    }
    // This fixed history reaches each refusal's own intended boundary and then
    // resumes through a genuine publisher/consumer operation, not a reset.
    assert!(coverage.context_mismatches > 0 && coverage.tracked_barriers > 0);
    assert!(coverage.ack_regressions > 0 && coverage.ack_beyond_end > 0);
    assert!(coverage.receipt_unavailable > 0 && coverage.receipt_transitions_refused > 0);
    assert!(coverage.successful_after_refusal > 0 && coverage.rejected > 0);
    eprintln!("durable inbox refusal/recovery support coverage: {coverage:#?}");
}
