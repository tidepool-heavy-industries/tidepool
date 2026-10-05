//! A discovery suite for native request arbitration, independent of Haskell values.
//! Logical request keys survive shrinking; missing watch keys are exercised as stale
//! handles. The oracle records accepted workflow facts and the owner's first outcome,
//! never reads the registry's private table, and checks public observations after every
//! command. Deadlines use the explicit expiry hook, so there are no timing races.

use super::*;
use crate::{ActorId, Incarnation};
use proptest::prelude::*;
use proptest::test_runner::{Config, TestCaseError, TestRunner};
use std::cell::RefCell;
use std::collections::BTreeMap;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Identity {
    Exact,
    Foreign,
    Restarted,
}

impl Identity {
    fn actor(self, expected: ActorRef) -> ActorRef {
        match self {
            Self::Exact => expected,
            Self::Foreign => ActorRef::first(ActorId(9)),
            Self::Restarted => ActorRef {
                incarnation: Incarnation(2),
                ..expected
            },
        }
    }

    fn authorize(self) -> Result<(), ReplyError> {
        match self {
            Self::Exact => Ok(()),
            Self::Foreign => Err(ReplyError::Unauthorized),
            Self::Restarted => Err(ReplyError::WrongIncarnation),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Action {
    Queue,
    Present,
    BeginReply,
    FinishReply { failed: bool },
    Cancel,
    Deadline,
    BeginAck,
    FinishAck,
    RollbackAck,
    Abandon,
    Release,
    Watch { allow_failure: bool },
    ForgetWatch { key: usize },
    StopTarget { failed: bool },
    StopOwner,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Operation {
    request: usize,
    identity: Identity,
    action: Action,
}

fn operation() -> impl Strategy<Value = Operation> {
    let identity = prop_oneof![6 => Just(Identity::Exact), 1 => Just(Identity::Foreign), 2 => Just(Identity::Restarted)];
    let action = prop_oneof![
        3 => Just(Action::Queue),
        3 => Just(Action::Present),
        3 => Just(Action::BeginReply),
        3 => any::<bool>().prop_map(|failed| Action::FinishReply { failed }),
        3 => Just(Action::Cancel),
        2 => Just(Action::Deadline),
        3 => Just(Action::BeginAck),
        3 => Just(Action::FinishAck),
        1 => Just(Action::RollbackAck),
        2 => Just(Action::Abandon),
        3 => Just(Action::Release),
        4 => any::<bool>().prop_map(|allow_failure| Action::Watch { allow_failure }),
        2 => (0usize..8).prop_map(|key| Action::ForgetWatch { key }),
        1 => any::<bool>().prop_map(|failed| Action::StopTarget { failed }),
        1 => Just(Action::StopOwner),
    ];
    (0usize..2, identity, action).prop_map(|(request, identity, action)| Operation {
        request,
        identity,
        action,
    })
}

fn exact(request: usize, action: Action) -> Operation {
    Operation {
        request,
        identity: Identity::Exact,
        action,
    }
}

fn histories() -> impl Strategy<Value = Vec<Operation>> {
    // These start from real reservations, exercise competing terminal transfers,
    // and change the dependency outcomes rather than repeatedly polling one state.
    let patterns = vec![
        vec![
            exact(0, Action::Queue),
            exact(1, Action::Queue),
            exact(0, Action::Present),
            exact(1, Action::Present),
            exact(
                0,
                Action::Watch {
                    allow_failure: false,
                },
            ),
            exact(
                1,
                Action::Watch {
                    allow_failure: true,
                },
            ),
            exact(0, Action::BeginReply),
            exact(0, Action::Deadline),
            exact(0, Action::FinishReply { failed: false }),
            exact(1, Action::Cancel),
            exact(1, Action::BeginAck),
            exact(1, Action::FinishAck),
            exact(0, Action::Release),
        ],
        vec![
            exact(0, Action::Cancel),
            exact(0, Action::Queue),
            exact(0, Action::Present),
            exact(0, Action::BeginReply),
            exact(0, Action::BeginAck),
            exact(0, Action::RollbackAck),
            exact(0, Action::BeginAck),
            exact(0, Action::FinishAck),
            exact(
                0,
                Action::Watch {
                    allow_failure: true,
                },
            ),
            exact(0, Action::Release),
        ],
        vec![
            exact(0, Action::Queue),
            exact(0, Action::Present),
            exact(0, Action::Abandon),
            exact(0, Action::Release),
            exact(0, Action::Cancel),
            exact(0, Action::Deadline),
            exact(0, Action::BeginAck),
            exact(0, Action::FinishAck),
            exact(0, Action::Release),
            exact(0, Action::BeginReply),
        ],
        vec![
            exact(0, Action::Queue),
            exact(0, Action::Present),
            exact(0, Action::BeginReply),
            exact(0, Action::Cancel),
            exact(0, Action::Deadline),
            exact(0, Action::FinishReply { failed: true }),
            Operation {
                request: 0,
                identity: Identity::Foreign,
                action: Action::Watch {
                    allow_failure: false,
                },
            },
            exact(0, Action::Release),
        ],
    ];
    let guided = (
        proptest::collection::vec(operation(), 0..8),
        proptest::sample::select(patterns),
        proptest::collection::vec(operation(), 0..8),
    )
        .prop_map(|(mut prefix, pattern, suffix)| {
            prefix.extend(pattern);
            prefix.extend(suffix);
            prefix
        });
    prop_oneof![1 => proptest::collection::vec(operation(), 1..41), 2 => guided]
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct RequestModel {
    admitted: bool,
    delivered: bool,
    reply_accepted: bool,
    cancellation: Option<CancellationReason>,
    ack_accepted: bool,
    target_finished: bool,
    outcome: Option<Result<(), ResponseFailure>>,
    released: bool,
    notice_claimed: bool,
}

impl RequestModel {
    fn authorize(&self, identity: Identity) -> Result<(), ReplyError> {
        if self.released {
            Err(ReplyError::Stale)
        } else {
            identity.authorize()
        }
    }

    fn first_outcome(&mut self, outcome: Result<(), ResponseFailure>) {
        if self.outcome.is_none() {
            self.outcome = Some(outcome);
        }
    }

    fn response(&self, watched: bool) -> Result<ResponseObservation, ReplyError> {
        if self.released {
            return Err(ReplyError::Stale);
        }
        Ok(match &self.outcome {
            Some(Ok(())) => ResponseObservation::Ready,
            Some(Err(failure)) => ResponseObservation::Unavailable(failure.clone()),
            None => match self.cancellation.filter(|_| !self.ack_accepted) {
                Some(reason) => ResponseObservation::CancellationPending(reason),
                None => ResponseObservation::Pending(PendingProgress {
                    actor_terminal: None,
                    provider_turn: None,
                    last_activity_unix_ms: None,
                    progress_revision: None,
                    watched,
                }),
            },
        })
    }

    fn reply(&self) -> Result<ReplyObservation, ReplyError> {
        if self.released {
            Err(ReplyError::Stale)
        } else if self.target_finished {
            Ok(ReplyObservation::Closed)
        } else if let Some(reason) = self.cancellation {
            Ok(ReplyObservation::CancellationRequested(reason))
        } else {
            Ok(ReplyObservation::Open)
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum WatchResult {
    Pending,
    Ready,
    Failed(ResponseFailure),
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct WatchModel {
    id: WatchId,
    owner: ActorRef,
    request: usize,
    allow_failure: bool,
    result: WatchResult,
    forgotten: bool,
}

#[derive(Default, Debug)]
struct Coverage {
    operations: BTreeMap<String, usize>,
    changed: usize,
    unchanged: usize,
    refusals: usize,
    overlapping_target_work: usize,
    terminal_owner_with_active_target: usize,
    stale_incarnation_attempts: usize,
    after_terminal: usize,
    watch_transitions: usize,
    settlement_notices: usize,
}

fn compare<T: std::fmt::Debug + PartialEq>(
    actual: Result<T, ReplyError>,
    expected: Result<T, ReplyError>,
    coverage: &mut Coverage,
) -> Result<(), TestCaseError> {
    coverage.refusals += usize::from(expected.is_err());
    prop_assert_eq!(actual, expected);
    Ok(())
}

fn run_history(operations: &[Operation], coverage: &mut Coverage) -> Result<(), TestCaseError> {
    let registry = RequestRegistry::default();
    let owner = ActorRef::first(ActorId(1));
    let targets = [ActorRef::first(ActorId(2)), ActorRef::first(ActorId(3))];
    let ids = [
        registry.reserve(owner, targets[0]),
        registry.reserve(owner, targets[1]),
    ];
    let mut requests = [RequestModel::default(), RequestModel::default()];
    let mut watches: Vec<WatchModel> = Vec::new();
    let mut event_sequences = BTreeMap::<ActorRef, Vec<u64>>::new();
    let mut last_sequence = BTreeMap::<ActorRef, u64>::new();

    for (step, operation) in operations.iter().enumerate() {
        let Operation {
            request: key,
            identity,
            action,
        } = *operation;
        let id = ids[key];
        let target = targets[key];
        let caller_owner = identity.actor(owner);
        let caller_target = identity.actor(target);
        let before = (requests.clone(), watches.clone());
        coverage.stale_incarnation_attempts += usize::from(identity == Identity::Restarted);
        coverage.after_terminal += usize::from(requests[key].outcome.is_some());
        *coverage
            .operations
            .entry(format!("{action:?}"))
            .or_default() += 1;
        let mut notices = Vec::new();
        let model = &mut requests[key];
        match action {
            Action::Queue => {
                let expected = model.authorize(identity).and_then(|()| {
                    if model.admitted
                        || model.delivered
                        || model.cancellation.is_some()
                        || model.reply_accepted
                        || model.target_finished
                    {
                        Err(ReplyError::AlreadySettled)
                    } else {
                        model.admitted = true;
                        Ok(())
                    }
                });
                compare(
                    registry.mark_queued(caller_owner, target, id),
                    expected,
                    coverage,
                )?;
            }
            Action::Present => {
                let expected = model.authorize(identity).and_then(|()| {
                    if model.target_finished {
                        Err(ReplyError::AlreadySettled)
                    } else if !model.delivered
                        && !model.reply_accepted
                        && !model.ack_accepted
                        && (model.admitted || model.cancellation.is_some())
                    {
                        model.delivered = true;
                        Ok(model.cancellation)
                    } else {
                        Err(ReplyError::Stale)
                    }
                });
                compare(registry.present(caller_target, id), expected, coverage)?;
            }
            Action::BeginReply => {
                let expected = model.authorize(identity).and_then(|()| {
                    if model.target_finished {
                        Err(ReplyError::AlreadySettled)
                    } else if model.cancellation.is_some() {
                        Err(ReplyError::CancellationRequested)
                    } else if model.delivered && !model.reply_accepted {
                        model.reply_accepted = true;
                        Ok(())
                    } else {
                        Err(ReplyError::Stale)
                    }
                });
                compare(registry.begin_reply(caller_target, id), expected, coverage)?;
            }
            Action::FinishReply { failed } => {
                // These completion hooks consume an already accepted transfer;
                // authority was checked by BeginReply, not by its completion.
                if !model.released && model.reply_accepted && !model.target_finished {
                    model.target_finished = true;
                    model.first_outcome(if failed {
                        Err(ResponseFailure::SettlementFailed("failed transfer".into()))
                    } else {
                        Ok(())
                    });
                }
                notices = if failed {
                    registry.fail_reply_settlement(id, "failed transfer")
                } else {
                    registry.finish_reply(id, Some("reply".into()))
                };
            }
            Action::Cancel => {
                let expected = model.authorize(identity).map(|()| {
                    if model.target_finished || model.reply_accepted {
                        CancelRequestOutcome::AlreadyTerminal
                    } else if model.cancellation.is_some() {
                        CancelRequestOutcome::AlreadyRequested
                    } else {
                        model.cancellation = Some(CancellationReason::RequesterCancelled);
                        CancelRequestOutcome::Requested
                    }
                });
                let actual = registry.cancel_request(
                    caller_owner,
                    id,
                    CancellationReason::RequesterCancelled,
                );
                let should_notify =
                    expected == Ok(CancelRequestOutcome::Requested) && model.delivered;
                if let Ok((_, notice)) = &actual {
                    prop_assert_eq!(notice.is_some(), should_notify);
                    if let Some(notice) = notice {
                        prop_assert_eq!(
                            (notice.target, notice.request, notice.reason),
                            (target, id, CancellationReason::RequesterCancelled)
                        );
                        prop_assert_eq!(notice.sequence, notice.watermark);
                        event_sequences
                            .entry(target)
                            .or_default()
                            .push(notice.sequence.0);
                    }
                }
                compare(actual.map(|(outcome, _)| outcome), expected, coverage)?;
            }
            Action::Deadline => {
                let should_expire = !model.released
                    && identity == Identity::Exact
                    && model.outcome.is_none()
                    && !model.reply_accepted
                    && !model.target_finished;
                let should_notify =
                    should_expire && model.cancellation.is_none() && model.delivered;
                if should_expire {
                    model.first_outcome(Err(ResponseFailure::DeadlineExceeded));
                    model
                        .cancellation
                        .get_or_insert(CancellationReason::DeadlineExpired);
                }
                let (notice, changed) = registry.deadline_request(caller_owner, id);
                notices = changed;
                prop_assert_eq!(notice.is_some(), should_notify);
                if let Some(notice) = notice {
                    prop_assert_eq!(
                        (notice.target, notice.request, notice.reason),
                        (target, id, CancellationReason::DeadlineExpired)
                    );
                    prop_assert_eq!(notice.sequence, notice.watermark);
                    event_sequences
                        .entry(target)
                        .or_default()
                        .push(notice.sequence.0);
                }
            }
            Action::BeginAck => {
                let expected = model.authorize(identity).and_then(|()| {
                    if model.target_finished {
                        Err(ReplyError::AlreadySettled)
                    } else if let Some(reason) = model.cancellation.filter(|_| !model.ack_accepted)
                    {
                        model.ack_accepted = true;
                        Ok(reason)
                    } else {
                        Err(ReplyError::Stale)
                    }
                });
                compare(
                    registry.begin_cancellation_acknowledgement(caller_target, id),
                    expected,
                    coverage,
                )?;
            }
            Action::FinishAck => {
                if !model.released && model.ack_accepted && !model.target_finished {
                    model.target_finished = true;
                    let failure = match model.cancellation.unwrap() {
                        CancellationReason::RequesterCancelled => ResponseFailure::Cancelled,
                        CancellationReason::DeadlineExpired => ResponseFailure::DeadlineExceeded,
                    };
                    model.first_outcome(Err(failure));
                }
                notices = registry.finish_cancellation_acknowledgement(id);
            }
            Action::RollbackAck => {
                if !model.released && model.ack_accepted && !model.target_finished {
                    model.ack_accepted = false;
                    // A retried acknowledgement belongs to target-side execution.
                    model.delivered = true;
                }
                registry.rollback_cancellation_acknowledgement(id);
            }
            Action::Abandon => {
                let expected = model.authorize(identity).map(|()| match &model.outcome {
                    None => {
                        model.first_outcome(Err(ResponseFailure::Abandoned));
                        AbandonResponseOutcome::AbandonedNow
                    }
                    Some(Err(ResponseFailure::Abandoned)) => {
                        AbandonResponseOutcome::AlreadyAbandoned
                    }
                    Some(_) => AbandonResponseOutcome::AlreadyTerminal,
                });
                let actual =
                    registry
                        .abandon_response(caller_owner, id)
                        .map(|(outcome, changed)| {
                            notices = changed;
                            outcome
                        });
                compare(actual, expected, coverage)?;
            }
            Action::Release => {
                let expected = model.authorize(identity).map(|()| {
                    if model.outcome.is_none() {
                        ForgetResponseOutcome::StillPending
                    } else if !model.target_finished {
                        ForgetResponseOutcome::TargetStillActive
                    } else {
                        model.released = true;
                        ForgetResponseOutcome::Forgotten
                    }
                });
                let actual =
                    registry
                        .forget_response(caller_owner, id)
                        .map(|(outcome, changed)| {
                            notices = changed;
                            outcome
                        });
                compare(actual, expected, coverage)?;
            }
            Action::Watch { allow_failure } => {
                let actual = registry.register_watch_labeled(
                    caller_owner,
                    "watch".into(),
                    vec![(id, allow_failure)],
                );
                if model.released {
                    compare(actual.map(|_| ()), Err(ReplyError::Stale), coverage)?;
                } else {
                    let (watch, changed) = actual.map_err(|error| {
                        TestCaseError::fail(format!("live dependency watch refused: {error:?}"))
                    })?;
                    notices = changed;
                    watches.push(WatchModel {
                        id: watch,
                        owner: caller_owner,
                        request: key,
                        allow_failure,
                        result: WatchResult::Pending,
                        forgotten: false,
                    });
                }
            }
            Action::ForgetWatch { key: watch_key } => {
                let watch = watches.get_mut(watch_key);
                let watch_id = watch
                    .as_ref()
                    .map_or(WatchId(u64::MAX - watch_key as u64), |watch| watch.id);
                let expected = match watch {
                    None => Err(ReplyError::Stale),
                    Some(watch) if watch.forgotten => Err(ReplyError::Stale),
                    Some(watch) if watch.owner != caller_owner => {
                        Err(if watch.owner.id == caller_owner.id {
                            ReplyError::WrongIncarnation
                        } else {
                            ReplyError::Unauthorized
                        })
                    }
                    Some(watch) if watch.result == WatchResult::Pending => {
                        Ok(ForgetWatchOutcome::StillPending)
                    }
                    Some(watch) => {
                        watch.forgotten = true;
                        Ok(ForgetWatchOutcome::Forgotten)
                    }
                };
                compare(
                    registry.forget_watch(caller_owner, watch_id),
                    expected,
                    coverage,
                )?;
            }
            Action::StopTarget { failed } => {
                let terminal = ActorTerminal {
                    kind: if failed {
                        ActorExitKind::Failed
                    } else {
                        ActorExitKind::Completed
                    },
                    summary: "target stopped".into(),
                };
                if identity == Identity::Exact && !model.released && !model.target_finished {
                    model.target_finished = true;
                    model.first_outcome(Err(if failed {
                        ResponseFailure::TargetFailed(terminal.summary.clone())
                    } else {
                        ResponseFailure::TargetUnavailable
                    }));
                }
                notices = registry.actor_stopped(caller_target, &terminal);
            }
            Action::StopOwner => {
                if identity == Identity::Exact {
                    for request in &mut requests {
                        if !request.released {
                            request.first_outcome(Err(ResponseFailure::RequesterStopped));
                        }
                    }
                }
                notices = registry.actor_stopped(
                    caller_owner,
                    &ActorTerminal {
                        kind: ActorExitKind::Cancelled,
                        summary: "owner stopped".into(),
                    },
                );
            }
        }

        // A watch retains its first terminal outcome. Releasing a dependency
        // invalidates pending/ready watches, but does not overwrite a failure
        // already reported to its subscriber.
        let mut expected_notices = Vec::new();
        for watch in watches.iter_mut().filter(|watch| !watch.forgotten) {
            let request = &requests[watch.request];
            let previous = watch.result.clone();
            if request.released && !matches!(watch.result, WatchResult::Failed(_)) {
                watch.result = WatchResult::Failed(ResponseFailure::Released);
            } else if watch.result == WatchResult::Pending {
                if let Some(outcome) = &request.outcome {
                    watch.result = match outcome {
                        Ok(()) => WatchResult::Ready,
                        Err(_) if watch.allow_failure => WatchResult::Ready,
                        Err(failure) => WatchResult::Failed(failure.clone()),
                    };
                }
            }
            if previous != watch.result {
                expected_notices.push((
                    watch.id,
                    watch.owner,
                    projection(&previous, ids[watch.request]),
                    projection(&watch.result, ids[watch.request]),
                ));
            }
        }
        let mut actual_notices = Vec::new();
        for notice in notices {
            prop_assert_eq!(notice.sequence, notice.watermark);
            let expected_transition = match &notice.current {
                WatchStateProjection::Ready => WatchTransition::Ready,
                WatchStateProjection::Unavailable { request, failure } => {
                    WatchTransition::Unavailable {
                        request: *request,
                        failure: failure.clone(),
                    }
                }
                other => {
                    return Err(TestCaseError::fail(format!(
                        "unexpected watch notice: {other:?}"
                    )));
                }
            };
            prop_assert_eq!(notice.transition, expected_transition);
            event_sequences
                .entry(notice.owner)
                .or_default()
                .push(notice.sequence.0);
            actual_notices.push((notice.watch, notice.owner, notice.previous, notice.current));
        }
        actual_notices.sort_by_key(|notice| notice.0);
        expected_notices.sort_by_key(|notice| notice.0);
        prop_assert_eq!(
            &actual_notices,
            &expected_notices,
            "step {}: {:?}",
            step,
            operation
        );
        coverage.watch_transitions += expected_notices.len();

        // Named watches owned by the response owner take over the wake. Foreign
        // subscriptions cannot suppress that owner's settlement notification.
        let mut expected_settlements = Vec::new();
        for (key, request) in requests.iter_mut().enumerate() {
            if request.released || request.notice_claimed {
                continue;
            }
            let transition = match &request.outcome {
                Some(Ok(())) => Some(SettlementTransition::Ready),
                Some(Err(ResponseFailure::Abandoned)) | None => None,
                Some(Err(failure)) => Some(SettlementTransition::Unavailable(failure.clone())),
            };
            if let Some(transition) = transition {
                request.notice_claimed = true;
                if !watches
                    .iter()
                    .any(|watch| !watch.forgotten && watch.owner == owner && watch.request == key)
                {
                    expected_settlements.push((ids[key], transition));
                }
            }
        }
        let settlements = registry.take_settlement_notifications();
        let mut actual_settlements = Vec::new();
        for notice in settlements {
            prop_assert_eq!(notice.owner, owner);
            prop_assert_eq!(notice.sequence, notice.watermark);
            prop_assert_eq!(
                notice.reply_preview.as_deref(),
                if notice.transition == SettlementTransition::Ready {
                    Some("reply")
                } else {
                    None
                }
            );
            event_sequences
                .entry(notice.owner)
                .or_default()
                .push(notice.sequence.0);
            actual_settlements.push((notice.request, notice.transition));
        }
        actual_settlements.sort_by_key(|notice| notice.0);
        expected_settlements.sort_by_key(|notice| notice.0);
        coverage.settlement_notices += expected_settlements.len();
        prop_assert_eq!(
            actual_settlements,
            expected_settlements,
            "step {}: {:?}",
            step,
            operation
        );

        // Event ids have one exact-incarnation owner, including events returned
        // through different APIs in the same operation. HashMap delivery order
        // is not part of the contract, so inspect the ids in sequence order.
        for (actor, sequences) in &mut event_sequences {
            sequences.sort_unstable();
            for sequence in sequences.drain(..) {
                let previous = last_sequence.entry(*actor).or_default();
                prop_assert!(
                    sequence > *previous,
                    "duplicate/reordered event id for {:?}: {} <= {}",
                    actor,
                    sequence,
                    previous
                );
                *previous = sequence;
            }
        }

        for (key, request) in requests.iter().enumerate() {
            let watched = watches.iter().any(|watch| {
                !watch.forgotten && watch.request == key && watch.result == WatchResult::Pending
            });
            for observer in [
                owner,
                Identity::Foreign.actor(owner),
                Identity::Restarted.actor(owner),
            ] {
                prop_assert_eq!(
                    registry.observe_response(observer, ids[key]),
                    request.response(watched),
                    "step {} key {}",
                    step,
                    key
                );
                prop_assert_eq!(
                    registry.observe_reply(observer, ids[key]),
                    request.reply(),
                    "step {} key {}",
                    step,
                    key
                );
            }
            let retained_work = !request.released && !request.target_finished;
            let current = retained_work
                && (request.delivered || request.reply_accepted || request.ack_accepted);
            let expected_work = (
                if current { vec![ids[key]] } else { vec![] },
                if retained_work && !current {
                    vec![ids[key]]
                } else {
                    vec![]
                },
            );
            prop_assert_eq!(
                registry.work_for_target(targets[key]),
                expected_work,
                "step {} key {}",
                step,
                key
            );
            let active =
                retained_work && (request.admitted || request.cancellation.is_some() || current);
            prop_assert_eq!(
                registry
                    .active_for_target(targets[key])
                    .into_iter()
                    .map(|(id, _)| id)
                    .collect::<Vec<_>>(),
                if active { vec![ids[key]] } else { vec![] }
            );
            prop_assert_eq!(
                registry.received_counts(targets[key]).0,
                u64::from(request.admitted)
            );
            prop_assert!(
                registry
                    .work_for_target(Identity::Restarted.actor(targets[key]))
                    .0
                    .is_empty()
            );
            prop_assert!(
                registry
                    .work_for_target(Identity::Restarted.actor(targets[key]))
                    .1
                    .is_empty()
            );
            coverage.terminal_owner_with_active_target +=
                usize::from(retained_work && request.outcome.is_some());
        }
        for watch in &watches {
            let expected = if watch.forgotten {
                Err(ReplyError::Stale)
            } else {
                Ok(match &watch.result {
                    WatchResult::Pending => WatchObservation::Pending(PendingProgress {
                        actor_terminal: None,
                        provider_turn: None,
                        last_activity_unix_ms: None,
                        progress_revision: None,
                        watched: true,
                    }),
                    WatchResult::Ready => {
                        WatchObservation::Ready(match &requests[watch.request].outcome {
                            Some(Err(failure)) => vec![(ids[watch.request], failure.clone())],
                            _ => vec![],
                        })
                    }
                    WatchResult::Failed(failure) => WatchObservation::Unavailable {
                        request: ids[watch.request],
                        failure: failure.clone(),
                    },
                })
            };
            // Foreign observation does not acknowledge a queued owner notice.
            prop_assert_eq!(
                registry.observe_watch(ActorRef::first(ActorId(10)), watch.id),
                expected
            );
            prop_assert_eq!(
                registry.retains_watch(watch.owner, watch.id),
                !watch.forgotten
            );
        }
        coverage.overlapping_target_work += usize::from(requests.iter().all(|request| {
            !request.released
                && !request.target_finished
                && (request.admitted || request.cancellation.is_some())
        }));
        if before == (requests.clone(), watches.clone()) {
            coverage.unchanged += 1;
        } else {
            coverage.changed += 1;
        }
    }
    Ok(())
}

fn projection(result: &WatchResult, request: RequestId) -> WatchStateProjection {
    match result {
        WatchResult::Pending => WatchStateProjection::Pending,
        WatchResult::Ready => WatchStateProjection::Ready,
        WatchResult::Failed(failure) => WatchStateProjection::Unavailable {
            request,
            failure: failure.clone(),
        },
    }
}

#[test]
fn generated_request_lifecycle_matches_observable_model() {
    let mut runner = TestRunner::new(Config {
        cases: 96,
        max_shrink_iters: 4_096,
        ..Config::default()
    });
    let coverage = RefCell::new(Coverage::default());
    let result = runner.run(&histories(), |operations| {
        run_history(&operations, &mut coverage.borrow_mut())
    });
    eprintln!("request sequence coverage: {:#?}", coverage.borrow());
    result.unwrap();
}
