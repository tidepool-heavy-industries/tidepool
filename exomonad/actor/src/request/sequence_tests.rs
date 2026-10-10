//! Stateful request arbitration with independently forced native result roots.
//! Logical request keys survive shrinking; missing watch keys are exercised as stale
//! handles. The oracle records accepted workflow facts and the owner's first outcome,
//! never reads the registry's private table, and checks public observations after every
//! command. Deadlines use the explicit expiry hook, so there are no timing races.

use super::*;
use crate::{ActorId, Incarnation};
use proptest::prelude::*;
use proptest::test_runner::{Config, FileFailurePersistence, TestCaseError, TestRunner};
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
    FinishReply {
        failed: bool,
    },
    Cancel,
    Deadline,
    BeginAck,
    FinishAck,
    RollbackAck,
    Abandon,
    Release,
    Watch {
        dependencies: u8,
        any: bool,
        allow_failure: bool,
    },
    ObserveWatch {
        key: usize,
        as_owner: bool,
    },
    ForgetWatch {
        key: usize,
    },
    StopTarget {
        failed: bool,
    },
    StopOwner,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Operation {
    request: usize,
    identity: Identity,
    action: Action,
}

#[derive(Clone, Debug)]
struct History {
    target_keys: Vec<u8>,
    operations: Vec<Operation>,
}

fn operation(request_count: usize) -> impl Strategy<Value = Operation> {
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
        4 => (1u8..(1 << request_count), any::<bool>(), any::<bool>()).prop_map(|(dependencies, any, allow_failure)| Action::Watch { dependencies, any, allow_failure }),
        3 => (0usize..8, any::<bool>()).prop_map(|(key, as_owner)| Action::ObserveWatch { key, as_owner }),
        2 => (0usize..8).prop_map(|key| Action::ForgetWatch { key }),
        1 => any::<bool>().prop_map(|failed| Action::StopTarget { failed }),
        1 => Just(Action::StopOwner),
    ];
    (0usize..request_count, identity, action).prop_map(|(request, identity, action)| Operation {
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

fn guided(target_keys: Vec<u8>, mode: u8, failed: bool, allow_failure: bool) -> History {
    // No mutating prefix can preempt these workflows. Parameters vary topology,
    // transfer outcome and subscription policy; arbitrary suffixes probe reuse.
    let mut operations = Vec::new();
    for request in 0..target_keys.len() {
        operations.extend([
            exact(request, Action::Queue),
            exact(request, Action::Present),
        ]);
    }
    operations.extend([
        exact(
            0,
            Action::Watch {
                dependencies: 3,
                any: false,
                allow_failure,
            },
        ),
        exact(
            0,
            Action::Watch {
                dependencies: 3,
                any: true,
                allow_failure,
            },
        ),
    ]);
    match mode {
        0 => operations.extend([
            exact(0, Action::BeginReply),
            exact(0, Action::Deadline),
            exact(0, Action::FinishReply { failed }),
            exact(1, Action::Cancel),
            exact(1, Action::BeginAck),
            exact(1, Action::FinishAck),
        ]),
        1 => operations.extend([
            exact(0, Action::Cancel),
            exact(0, Action::Deadline),
            exact(0, Action::BeginReply),
            exact(0, Action::BeginAck),
            exact(0, Action::RollbackAck),
            exact(0, Action::BeginAck),
            exact(0, Action::FinishAck),
            exact(1, Action::BeginReply),
            exact(1, Action::FinishReply { failed }),
        ]),
        2 => operations.extend([
            exact(0, Action::Abandon),
            exact(0, Action::Release),
            exact(0, Action::BeginReply),
            exact(0, Action::FinishReply { failed }),
            exact(1, Action::BeginReply),
            exact(1, Action::FinishReply { failed: false }),
        ]),
        3 => operations.extend([
            exact(0, Action::StopTarget { failed }),
            exact(1, Action::BeginReply),
            exact(1, Action::FinishReply { failed: false }),
        ]),
        _ => unreachable!("guided mode is bounded"),
    }
    // Observe after settlement, with a foreign read preceding owner acknowledgement.
    operations.extend([
        exact(
            0,
            Action::ObserveWatch {
                key: 0,
                as_owner: false,
            },
        ),
        exact(
            0,
            Action::ObserveWatch {
                key: 1,
                as_owner: false,
            },
        ),
        exact(
            0,
            Action::ObserveWatch {
                key: 0,
                as_owner: true,
            },
        ),
        exact(
            0,
            Action::ObserveWatch {
                key: 1,
                as_owner: true,
            },
        ),
        exact(0, Action::Release),
        exact(1, Action::Release),
        exact(
            0,
            Action::ObserveWatch {
                key: 0,
                as_owner: false,
            },
        ),
        exact(0, Action::ForgetWatch { key: 0 }),
        exact(0, Action::ForgetWatch { key: 1 }),
    ]);
    History {
        target_keys,
        operations,
    }
}

fn histories() -> impl Strategy<Value = History> {
    let general = proptest::collection::vec(0u8..3, 1..4).prop_flat_map(|target_keys| {
        proptest::collection::vec(operation(target_keys.len()), 1..41).prop_map(move |operations| {
            History {
                target_keys: target_keys.clone(),
                operations,
            }
        })
    });
    let guided = (
        proptest::collection::vec(0u8..3, 2..4),
        0u8..4,
        any::<bool>(),
        any::<bool>(),
    )
        .prop_flat_map(|(topology, mode, failed, allow_failure)| {
            let core = guided(topology, mode, failed, allow_failure);
            // Always generate the complete valid workflow, then let standard
            // boolean shrinking remove individual commands from its core.
            (
                proptest::collection::vec(proptest::bool::weighted(1.0), core.operations.len()),
                proptest::collection::vec(operation(core.target_keys.len()), 0..8),
            )
                .prop_map(move |(included, suffix)| {
                    let mut history = History {
                        target_keys: core.target_keys.clone(),
                        operations: core
                            .operations
                            .iter()
                            .zip(included)
                            .filter_map(|(operation, include)| include.then_some(*operation))
                            .collect(),
                    };
                    history.operations.extend(suffix);
                    history
                })
        });
    prop_oneof![1 => general, 2 => guided]
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
    Failed {
        request: usize,
        failure: ResponseFailure,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct WatchModel {
    id: WatchId,
    owner: ActorRef,
    dependencies: Vec<usize>,
    any: bool,
    allow_failure: bool,
    captured: Vec<Option<Result<(), ResponseFailure>>>,
    decision: Option<readiness::Decision>,
    result: WatchResult,
    forgotten: bool,
    observed_by_owner: bool,
}

#[derive(Default, Debug)]
struct Coverage {
    operations: BTreeMap<String, usize>,
    changes_by_operation: BTreeMap<String, usize>,
    refusals_by_operation: BTreeMap<String, usize>,
    changed: usize,
    unchanged: usize,
    refusals: usize,
    overlapping_target_work: usize,
    terminal_owner_with_active_target: usize,
    stale_incarnation_attempts: usize,
    after_terminal: usize,
    watch_transitions: usize,
    settlement_notices: usize,
    shared_target_work: usize,
    retirement_fanout: usize,
    multi_dependency_watches: usize,
    owner_acknowledgements: usize,
    foreign_terminal_observations: usize,
    accepted_replies: usize,
    completed_acknowledgements: usize,
    acknowledgement_retries: usize,
    released_responses: usize,
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

fn run_history(history: &History, coverage: &mut Coverage) -> Result<(), TestCaseError> {
    let registry = RequestRegistry::default();
    let owner = ActorRef::first(ActorId(1));
    let targets: Vec<_> = history
        .target_keys
        .iter()
        .map(|key| ActorRef::first(ActorId(2 + u64::from(*key))))
        .collect();
    let ids: Vec<_> = targets
        .iter()
        .map(|target| registry.reserve(owner, *target))
        .collect();
    let mut requests = vec![RequestModel::default(); targets.len()];
    let mut reply_claims: Vec<Option<RequestReplyClaim>> =
        (0..targets.len()).map(|_| None).collect();
    let mut watches: Vec<WatchModel> = Vec::new();
    let mut event_sequences = BTreeMap::<ActorRef, Vec<u64>>::new();
    let mut last_sequence = BTreeMap::<ActorRef, u64>::new();

    for (step, operation) in history.operations.iter().enumerate() {
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
        let operation_name = format!("{action:?}");
        *coverage
            .operations
            .entry(operation_name.clone())
            .or_default() += 1;
        let previous_refusals = coverage.refusals;
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
                        coverage.accepted_replies += 1;
                        Ok(())
                    } else {
                        Err(ReplyError::Stale)
                    }
                });
                let actual = registry.begin_reply(caller_target, id).map(|claim| {
                    reply_claims[key] = Some(claim);
                });
                compare(actual, expected, coverage)?;
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
                    reply_claims[key] = None;
                    registry.fail_reply_settlement(id, "failed transfer")
                } else {
                    test_support::complete_optional_reply(
                        &registry,
                        &mut reply_claims[key],
                        Some("reply".into()),
                    )
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
                    coverage.completed_acknowledgements += 1;
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
                    coverage.acknowledgement_retries += 1;
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
                        coverage.released_responses += 1;
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
            Action::Watch {
                dependencies,
                any,
                allow_failure,
            } => {
                let dependencies: Vec<_> = (0..requests.len())
                    .filter(|key| dependencies & (1 << key) != 0)
                    .collect();
                let requirements: Vec<_> = dependencies
                    .iter()
                    .map(|key| (ids[*key], WatchRequirement::Response { allow_failure }))
                    .collect();
                let groups = if any {
                    vec![requirements]
                } else {
                    requirements
                        .into_iter()
                        .map(|requirement| vec![requirement])
                        .collect()
                };
                let actual = registry.register_watch_requirement_groups(
                    caller_owner,
                    "watch".into(),
                    groups,
                );
                if dependencies.iter().any(|key| requests[*key].released) {
                    compare(actual.map(|_| ()), Err(ReplyError::Stale), coverage)?;
                } else {
                    let (watch, changed) = actual.map_err(|error| {
                        TestCaseError::fail(format!("live dependency watch refused: {error:?}"))
                    })?;
                    notices = changed;
                    coverage.multi_dependency_watches += usize::from(dependencies.len() > 1);
                    watches.push(WatchModel {
                        id: watch,
                        owner: caller_owner,
                        captured: vec![None; dependencies.len()],
                        decision: None,
                        dependencies,
                        any,
                        allow_failure,
                        result: WatchResult::Pending,
                        forgotten: false,
                        observed_by_owner: false,
                    });
                }
            }
            Action::ObserveWatch {
                key: watch_key,
                as_owner,
            } => {
                let watch = watches.get_mut(watch_key);
                let watch_id = watch
                    .as_ref()
                    .map_or(WatchId(u64::MAX - watch_key as u64), |watch| watch.id);
                let observer = if as_owner {
                    watch.as_ref().map_or(caller_owner, |watch| watch.owner)
                } else {
                    ActorRef::first(ActorId(10))
                };
                let expected = watch.as_ref().map_or(Err(ReplyError::Stale), |watch| {
                    watch.observation(&requests, &ids)
                });
                if let Some(watch) =
                    watch.filter(|watch| !watch.forgotten && watch.result != WatchResult::Pending)
                {
                    if as_owner {
                        coverage.owner_acknowledgements += usize::from(!watch.observed_by_owner);
                        watch.observed_by_owner = true;
                    } else {
                        coverage.foreign_terminal_observations += 1;
                    }
                }
                compare(
                    registry.observe_watch(observer, watch_id),
                    expected,
                    coverage,
                )?;
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
                    diagnostic: None,
                };
                let mut changed = 0;
                for (key, request) in requests.iter_mut().enumerate() {
                    if targets[key] == caller_target
                        && !request.released
                        && !request.target_finished
                    {
                        request.target_finished = true;
                        request.first_outcome(Err(if failed {
                            ResponseFailure::TargetFailed(terminal.summary.clone())
                        } else {
                            ResponseFailure::TargetUnavailable
                        }));
                        changed += 1;
                    }
                }
                coverage.retirement_fanout += usize::from(changed > 1);
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
                        diagnostic: None,
                    },
                );
            }
        }

        for (request, model) in ids.iter().zip(&requests) {
            if !model.released && model.outcome == Some(Ok(())) {
                let root = registry
                    .observe_response_result(owner, *request)
                    .map_err(|error| {
                        TestCaseError::fail(format!("successful request has no root: {error:?}"))
                    })?;
                prop_assert_eq!(test_support::force(&root), 41);
            }
        }

        // Observe primary request facts independently. Each leaf keeps its
        // first terminal fact; the whole expression keeps its first terminal
        // outcome, including an initial left-biased race.
        let mut expected_notices = Vec::new();
        for watch in watches.iter_mut().filter(|watch| !watch.forgotten) {
            let previous = watch.result.clone();
            if watch.result == WatchResult::Pending {
                for (index, key) in watch.dependencies.iter().enumerate() {
                    if watch.captured[index].is_none() {
                        watch.captured[index] = if requests[*key].released {
                            Some(Err(ResponseFailure::Released))
                        } else {
                            requests[*key].outcome.clone()
                        };
                    }
                }
                let selected = if watch.any {
                    watch
                        .captured
                        .iter()
                        .position(Option::is_some)
                        .map(|index| vec![index])
                        .or_else(|| watch.dependencies.is_empty().then(Vec::new))
                } else if watch.captured.iter().all(Option::is_some) {
                    Some((0..watch.dependencies.len()).collect())
                } else {
                    None
                };
                let failure = if watch.any {
                    selected.as_ref().and_then(|indexes| {
                        indexes
                            .iter()
                            .find_map(|index| match &watch.captured[*index] {
                                Some(Err(failure)) if !watch.allow_failure => {
                                    Some((watch.dependencies[*index], failure.clone()))
                                }
                                _ => None,
                            })
                    })
                } else {
                    watch
                        .captured
                        .iter()
                        .enumerate()
                        .find_map(|(index, fact)| match fact {
                            Some(Err(failure)) if !watch.allow_failure => {
                                Some((watch.dependencies[index], failure.clone()))
                            }
                            _ => None,
                        })
                };
                if let Some((request, failure)) = failure {
                    watch.result = WatchResult::Failed { request, failure };
                } else if let Some(indexes) = selected {
                    let mut decision = readiness::Decision::default();
                    for index in &indexes {
                        let node = if *index == 0 { 0 } else { index * 2 - 1 };
                        let failure = watch.captured[*index]
                            .as_ref()
                            .unwrap()
                            .as_ref()
                            .err()
                            .cloned();
                        decision.leaves.push((node, failure));
                    }
                    if watch.any && !indexes.is_empty() {
                        let selected = indexes[0];
                        for right_index in (1..watch.dependencies.len()).rev() {
                            let left = selected < right_index;
                            decision.choices.push((right_index * 2, left));
                            if !left {
                                break;
                            }
                        }
                    }
                    watch.decision = Some(decision);
                    watch.result = WatchResult::Ready;
                }
            }
            if watch.result == WatchResult::Ready {
                for &(node, ref failure) in &watch.decision.as_ref().unwrap().leaves {
                    if failure.is_none() {
                        let root = registry
                            .observe_watch_snapshot_response(watch.id, &[], node)
                            .map_err(|error| {
                                TestCaseError::fail(format!(
                                    "captured watch lost result root: {error:?}"
                                ))
                            })?;
                        prop_assert_eq!(test_support::force(&root), 41);
                    }
                }
            }
            if previous != watch.result {
                expected_notices.push((
                    watch.id,
                    watch.owner,
                    projection(&previous, &ids),
                    projection(&watch.result, &ids),
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
                if !watches.iter().any(|watch| {
                    !watch.forgotten && watch.owner == owner && watch.dependencies.contains(&key)
                }) {
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
                !watch.forgotten
                    && watch.dependencies.contains(&key)
                    && watch.result == WatchResult::Pending
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
            coverage.terminal_owner_with_active_target +=
                usize::from(retained_work && request.outcome.is_some());
        }
        for target in targets
            .iter()
            .copied()
            .collect::<std::collections::BTreeSet<_>>()
        {
            let mut current = Vec::new();
            let mut queued = Vec::new();
            let mut active = Vec::new();
            let mut admitted = 0;
            for (key, request) in requests
                .iter()
                .enumerate()
                .filter(|(key, _)| targets[*key] == target)
            {
                admitted += u64::from(request.admitted);
                if request.released || request.target_finished {
                    continue;
                }
                let executing = request.delivered || request.reply_accepted || request.ack_accepted;
                if executing {
                    current.push(ids[key]);
                } else {
                    queued.push(ids[key]);
                }
                if executing || request.admitted || request.cancellation.is_some() {
                    active.push(ids[key]);
                }
            }
            coverage.shared_target_work += usize::from(active.len() > 1);
            prop_assert_eq!(registry.work_for_target(target), (current, queued));
            prop_assert_eq!(
                registry
                    .active_for_target(target)
                    .into_iter()
                    .map(|(id, _)| id)
                    .collect::<Vec<_>>(),
                active
            );
            prop_assert_eq!(registry.received_counts(target).0, admitted);
            prop_assert_eq!(
                registry.work_for_target(Identity::Restarted.actor(target)),
                (vec![], vec![])
            );
        }
        for watch in &watches {
            // Passive checking uses a foreign observer and verifies it cannot
            // warm owner acknowledgement; only ObserveWatch(as_owner) may do so.
            let expected_ack = !watch.forgotten && watch.observed_by_owner;
            prop_assert_eq!(
                registry.watch_observed_since(watch.owner, watch.id, 0),
                expected_ack
            );
            prop_assert_eq!(
                registry.observe_watch(ActorRef::first(ActorId(10)), watch.id),
                watch.observation(&requests, &ids)
            );
            prop_assert_eq!(
                registry.watch_observed_since(watch.owner, watch.id, 0),
                expected_ack
            );
            prop_assert!(!registry.watch_observed_since(ActorRef::first(ActorId(10)), watch.id, 0));
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
        *coverage
            .refusals_by_operation
            .entry(operation_name.clone())
            .or_default() += coverage.refusals - previous_refusals;
        if before == (requests.clone(), watches.clone()) {
            coverage.unchanged += 1;
        } else {
            coverage.changed += 1;
            *coverage
                .changes_by_operation
                .entry(operation_name)
                .or_default() += 1;
        }
    }
    Ok(())
}

impl WatchModel {
    fn observation(
        &self,
        _requests: &[RequestModel],
        ids: &[RequestId],
    ) -> Result<WatchObservation, ReplyError> {
        if self.forgotten {
            return Err(ReplyError::Stale);
        }
        Ok(match &self.result {
            WatchResult::Pending => WatchObservation::Pending(PendingProgress {
                actor_terminal: None,
                provider_turn: None,
                last_activity_unix_ms: None,
                progress_revision: None,
                watched: true,
            }),
            WatchResult::Ready => {
                WatchObservation::Ready(self.decision.clone().expect("captured decision"))
            }
            WatchResult::Failed { request, failure } => WatchObservation::Unavailable {
                request: ids[*request],
                failure: failure.clone(),
            },
        })
    }
}

fn projection(result: &WatchResult, ids: &[RequestId]) -> WatchStateProjection {
    match result {
        WatchResult::Pending => WatchStateProjection::Pending,
        WatchResult::Ready => WatchStateProjection::Ready,
        WatchResult::Failed { request, failure } => WatchStateProjection::Unavailable {
            request: ids[*request],
            failure: failure.clone(),
        },
    }
}

#[test]
fn generated_request_lifecycle_matches_observable_model() {
    let mut config = Config {
        source_file: Some(file!()),
        test_name: Some(concat!(
            module_path!(),
            "::generated_request_lifecycle_matches_observable_model"
        )),
        ..Config::default()
    };
    if std::env::var_os("PROPTEST_CASES").is_none() {
        config.cases = 96;
    }
    if std::env::var_os("PROPTEST_MAX_SHRINK_ITERS").is_none() {
        config.max_shrink_iters = 4_096;
    }
    if let Some(path) = option_env!("TIDEPOOL_PROPTEST_REGRESSIONS") {
        config.failure_persistence = Some(Box::new(FileFailurePersistence::Direct(path)));
        eprintln!("request sequence seed persistence: {path}");
    }
    let mut config = proptest::test_runner::contextualize_config(config);
    config.source_file = Some(file!());
    config.test_name = Some(concat!(
        module_path!(),
        "::generated_request_lifecycle_matches_observable_model"
    ));
    let mut runner = TestRunner::new(config);
    let mut cohort = Coverage::default();
    for topology in [vec![0, 0], vec![0, 1], vec![0, 0, 1], vec![0, 1, 2]] {
        for mode in 0..4 {
            for failed in [false, true] {
                for allow_failure in [false, true] {
                    run_history(
                        &guided(topology.clone(), mode, failed, allow_failure),
                        &mut cohort,
                    )
                    .unwrap();
                }
            }
        }
    }
    // These lower bounds come from the explicit cohort, never a random seed.
    assert!(cohort.shared_target_work > 0 && cohort.retirement_fanout > 0);
    assert!(cohort.multi_dependency_watches > 0 && cohort.owner_acknowledgements > 0);
    assert!(cohort.foreign_terminal_observations > 0 && cohort.accepted_replies > 0);
    assert!(cohort.completed_acknowledgements > 0 && cohort.acknowledgement_retries > 0);
    assert!(cohort.released_responses > 0 && cohort.terminal_owner_with_active_target > 0);
    eprintln!("request deterministic cohort coverage: {cohort:#?}");
    let coverage = RefCell::new(Coverage::default());
    let result = runner.run(&histories(), |history| {
        run_history(&history, &mut coverage.borrow_mut())
    });
    eprintln!("request sequence coverage: {:#?}", coverage.borrow());
    result.unwrap();
}
