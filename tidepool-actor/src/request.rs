use std::collections::HashMap;

use parking_lot::Mutex;
use serde::{Deserialize, Serialize};

use crate::{ActorExitKind, ActorRef, ActorTerminal};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct RequestId(pub u64);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct WatchId(pub u64);

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ResponseFailure {
    TargetUnavailable,
    TargetFailed(String),
    TargetCancelled(String),
    RequesterStopped,
    Abandoned,
    Cancelled,
    DeadlineExceeded,
    SettlementFailed(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResponseObservation {
    Pending,
    CancellationPending(CancellationReason),
    Ready,
    Unavailable(ResponseFailure),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CancelRequestOutcome {
    Requested,
    AlreadyRequested,
    AlreadyTerminal,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AbandonResponseOutcome {
    AbandonedNow,
    AlreadyAbandoned,
    AlreadyTerminal,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ForgetResponseOutcome {
    Forgotten,
    StillPending,
    TargetStillActive,
    RetainedByWatches(Vec<WatchId>),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ForgetWatchOutcome {
    Forgotten,
    StillPending,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum CancellationReason {
    RequesterCancelled,
    DeadlineExpired,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReplyObservation {
    Open,
    CancellationRequested(CancellationReason),
    Closed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReplyError {
    Stale,
    AlreadySettled,
    Unauthorized,
    WrongIncarnation,
    CancellationRequested,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum WatchTransition {
    Ready,
    Unavailable {
        request: RequestId,
        failure: ResponseFailure,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WatchObservation {
    Pending,
    Ready(Vec<(RequestId, ResponseFailure)>),
    Unavailable {
        request: RequestId,
        failure: ResponseFailure,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WatchNotification {
    pub owner: ActorRef,
    pub watch: WatchId,
    pub transition: WatchTransition,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RequestCancellationNotification {
    pub target: ActorRef,
    pub request: RequestId,
    pub reason: CancellationReason,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum TargetState {
    Reserved,
    Queued,
    Presented,
    CancellationRequested {
        presented: bool,
        reason: CancellationReason,
    },
    AcknowledgingCancellation(CancellationReason),
    Settling,
    Closed,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum OwnerState {
    Observing,
    Ready,
    Unavailable(ResponseFailure),
    Abandoned,
}

struct RequestRecord {
    owner: ActorRef,
    target: ActorRef,
    label: String,
    target_state: TargetState,
    owner_state: OwnerState,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum WatchState {
    Pending,
    Ready,
    Unavailable {
        request: RequestId,
        failure: ResponseFailure,
    },
}

struct WatchRecord {
    owner: ActorRef,
    label: String,
    dependencies: Vec<WatchDependency>,
    state: WatchState,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct WatchDependency {
    request: RequestId,
    allow_failure: bool,
}

#[derive(Default)]
struct RequestStateTable {
    next_request: u64,
    next_watch: u64,
    requests: HashMap<RequestId, RequestRecord>,
    watches: HashMap<WatchId, WatchRecord>,
}

/// One process-local owner for request identity, terminal state, and watch
/// readiness. Live request inputs and results remain in the resident Haskell
/// heap; this table stores only exact actor identities and closed transitions.
#[derive(Default)]
pub(crate) struct RequestRegistry {
    state: Mutex<RequestStateTable>,
}

pub(crate) struct ActorRequestStatus {
    pub pending_responses: Vec<(RequestId, String)>,
    pub ready_responses: Vec<(RequestId, String)>,
    pub unavailable_responses: Vec<(RequestId, String)>,
    pub pending_watches: Vec<(WatchId, String)>,
    pub ready_watches: Vec<(WatchId, String)>,
    pub unavailable_watches: Vec<(WatchId, String)>,
}

impl RequestRegistry {
    pub(crate) fn active_for_target(&self, target: ActorRef) -> Vec<(RequestId, String)> {
        let state = self.state.lock();
        let mut active = state
            .requests
            .iter()
            .filter(|(_, record)| {
                record.target == target
                    && !matches!(
                        record.target_state,
                        TargetState::Reserved | TargetState::Closed
                    )
            })
            .map(|(request, record)| (*request, record.label.clone()))
            .collect::<Vec<_>>();
        active.sort_unstable();
        active
    }

    pub(crate) fn status_for(&self, owner: ActorRef) -> ActorRequestStatus {
        let state = self.state.lock();
        let mut status = ActorRequestStatus {
            pending_responses: Vec::new(),
            ready_responses: Vec::new(),
            unavailable_responses: Vec::new(),
            pending_watches: Vec::new(),
            ready_watches: Vec::new(),
            unavailable_watches: Vec::new(),
        };
        for (request, record) in &state.requests {
            if record.owner != owner {
                continue;
            }
            match record.owner_state {
                OwnerState::Ready => status
                    .ready_responses
                    .push((*request, record.label.clone())),
                OwnerState::Unavailable(_) | OwnerState::Abandoned => status
                    .unavailable_responses
                    .push((*request, record.label.clone())),
                OwnerState::Observing => status
                    .pending_responses
                    .push((*request, record.label.clone())),
            }
        }
        for (watch, record) in &state.watches {
            if record.owner != owner {
                continue;
            }
            match record.state {
                WatchState::Pending => status.pending_watches.push((*watch, record.label.clone())),
                WatchState::Ready => status.ready_watches.push((*watch, record.label.clone())),
                WatchState::Unavailable { .. } => status
                    .unavailable_watches
                    .push((*watch, record.label.clone())),
            }
        }
        status.pending_responses.sort_unstable();
        status.ready_responses.sort_unstable();
        status.unavailable_responses.sort_unstable();
        status.pending_watches.sort_unstable();
        status.ready_watches.sort_unstable();
        status.unavailable_watches.sort_unstable();
        status
    }

    #[cfg(test)]
    pub(crate) fn reserve(&self, owner: ActorRef, target: ActorRef) -> RequestId {
        self.reserve_labeled(owner, target, "request".into())
    }

    pub(crate) fn reserve_labeled(
        &self,
        owner: ActorRef,
        target: ActorRef,
        label: String,
    ) -> RequestId {
        let mut state = self.state.lock();
        state.next_request = state.next_request.saturating_add(1);
        let id = RequestId(state.next_request);
        state.requests.insert(
            id,
            RequestRecord {
                owner,
                target,
                label,
                target_state: TargetState::Reserved,
                owner_state: OwnerState::Observing,
            },
        );
        id
    }

    pub(crate) fn mark_target_unavailable(
        &self,
        owner: ActorRef,
        request: RequestId,
    ) -> Vec<WatchNotification> {
        self.transition_request(owner, request, |record| {
            record.target_state = TargetState::Closed;
            if record.owner_state == OwnerState::Observing {
                record.owner_state = OwnerState::Unavailable(ResponseFailure::TargetUnavailable);
            }
        })
    }

    pub(crate) fn mark_queued(
        &self,
        owner: ActorRef,
        target: ActorRef,
        request: RequestId,
    ) -> Result<(), ReplyError> {
        let mut state = self.state.lock();
        let record = state.requests.get_mut(&request).ok_or(ReplyError::Stale)?;
        authorize_owner(record, owner)?;
        if record.target != target {
            return Err(identity_error(record.target, target));
        }
        match record.target_state {
            TargetState::Reserved => {
                record.target_state = TargetState::Queued;
                Ok(())
            }
            _ => Err(ReplyError::AlreadySettled),
        }
    }

    pub(crate) fn present(
        &self,
        target: ActorRef,
        request: RequestId,
    ) -> Result<Option<CancellationReason>, ReplyError> {
        let mut state = self.state.lock();
        let record = state.requests.get_mut(&request).ok_or(ReplyError::Stale)?;
        authorize_target(record, target)?;
        match record.target_state {
            TargetState::Queued => {
                record.target_state = TargetState::Presented;
                Ok(None)
            }
            TargetState::CancellationRequested {
                presented: false,
                reason,
            } => {
                record.target_state = TargetState::CancellationRequested {
                    presented: true,
                    reason,
                };
                Ok(Some(reason))
            }
            TargetState::Closed => Err(ReplyError::AlreadySettled),
            TargetState::Reserved
            | TargetState::Presented
            | TargetState::CancellationRequested {
                presented: true, ..
            }
            | TargetState::AcknowledgingCancellation(_)
            | TargetState::Settling => Err(ReplyError::Stale),
        }
    }

    pub(crate) fn begin_reply(
        &self,
        target: ActorRef,
        request: RequestId,
    ) -> Result<(), ReplyError> {
        let mut state = self.state.lock();
        let record = state.requests.get_mut(&request).ok_or(ReplyError::Stale)?;
        authorize_target(record, target)?;
        match record.target_state {
            TargetState::Presented => {
                record.target_state = TargetState::Settling;
                Ok(())
            }
            TargetState::CancellationRequested { .. }
            | TargetState::AcknowledgingCancellation(_) => Err(ReplyError::CancellationRequested),
            TargetState::Closed => Err(ReplyError::AlreadySettled),
            TargetState::Reserved | TargetState::Queued | TargetState::Settling => {
                Err(ReplyError::Stale)
            }
        }
    }

    pub(crate) fn finish_reply(&self, request: RequestId) -> Vec<WatchNotification> {
        let mut state = self.state.lock();
        let Some(record) = state.requests.get_mut(&request) else {
            return Vec::new();
        };
        if record.target_state != TargetState::Settling {
            return Vec::new();
        }
        record.target_state = TargetState::Closed;
        if record.owner_state == OwnerState::Observing {
            record.owner_state = OwnerState::Ready;
        }
        reevaluate_watches(&mut state)
    }

    pub(crate) fn fail_reply_settlement(
        &self,
        request: RequestId,
        detail: impl Into<String>,
    ) -> Vec<WatchNotification> {
        let mut state = self.state.lock();
        if let Some(record) = state.requests.get_mut(&request) {
            if record.target_state == TargetState::Settling {
                record.target_state = TargetState::Closed;
                if record.owner_state == OwnerState::Observing {
                    record.owner_state =
                        OwnerState::Unavailable(ResponseFailure::SettlementFailed(detail.into()));
                }
            }
        }
        reevaluate_watches(&mut state)
    }

    pub(crate) fn observe_response(
        &self,
        owner: ActorRef,
        request: RequestId,
    ) -> Result<ResponseObservation, ReplyError> {
        let state = self.state.lock();
        let record = state.requests.get(&request).ok_or(ReplyError::Stale)?;
        authorize_owner(record, owner)?;
        Ok(match &record.owner_state {
            OwnerState::Ready => ResponseObservation::Ready,
            OwnerState::Unavailable(failure) => ResponseObservation::Unavailable(failure.clone()),
            OwnerState::Abandoned => ResponseObservation::Unavailable(ResponseFailure::Abandoned),
            OwnerState::Observing => match record.target_state {
                TargetState::CancellationRequested { reason, .. } => {
                    ResponseObservation::CancellationPending(reason)
                }
                _ => ResponseObservation::Pending,
            },
        })
    }

    pub(crate) fn cancel_request(
        &self,
        owner: ActorRef,
        request: RequestId,
        reason: CancellationReason,
    ) -> Result<
        (
            CancelRequestOutcome,
            Option<RequestCancellationNotification>,
        ),
        ReplyError,
    > {
        let mut state = self.state.lock();
        let record = state.requests.get_mut(&request).ok_or(ReplyError::Stale)?;
        authorize_owner(record, owner)?;
        if is_owner_terminal(&record.owner_state) || record.target_state == TargetState::Closed {
            return Ok((CancelRequestOutcome::AlreadyTerminal, None));
        }
        if matches!(
            record.target_state,
            TargetState::CancellationRequested { .. } | TargetState::AcknowledgingCancellation(_)
        ) {
            return Ok((CancelRequestOutcome::AlreadyRequested, None));
        }
        if record.target_state == TargetState::Settling {
            return Ok((CancelRequestOutcome::AlreadyTerminal, None));
        }
        let presented = record.target_state == TargetState::Presented;
        record.target_state = TargetState::CancellationRequested { presented, reason };
        let notification = presented.then_some(RequestCancellationNotification {
            target: record.target,
            request,
            reason,
        });
        Ok((CancelRequestOutcome::Requested, notification))
    }

    pub(crate) fn deadline_request(
        &self,
        owner: ActorRef,
        request: RequestId,
    ) -> Option<RequestCancellationNotification> {
        self.cancel_request(owner, request, CancellationReason::DeadlineExpired)
            .ok()
            .and_then(|(_, notification)| notification)
    }

    pub(crate) fn observe_reply(
        &self,
        target: ActorRef,
        request: RequestId,
    ) -> Result<ReplyObservation, ReplyError> {
        let state = self.state.lock();
        let record = state.requests.get(&request).ok_or(ReplyError::Stale)?;
        authorize_target(record, target)?;
        Ok(match record.target_state {
            TargetState::CancellationRequested { reason, .. } => {
                ReplyObservation::CancellationRequested(reason)
            }
            TargetState::AcknowledgingCancellation(reason) => {
                ReplyObservation::CancellationRequested(reason)
            }
            TargetState::Closed => ReplyObservation::Closed,
            _ => ReplyObservation::Open,
        })
    }

    pub(crate) fn begin_cancellation_acknowledgement(
        &self,
        target: ActorRef,
        request: RequestId,
    ) -> Result<CancellationReason, ReplyError> {
        let mut state = self.state.lock();
        let record = state.requests.get_mut(&request).ok_or(ReplyError::Stale)?;
        authorize_target(record, target)?;
        let reason = match record.target_state {
            TargetState::CancellationRequested { reason, .. } => reason,
            TargetState::Closed => return Err(ReplyError::AlreadySettled),
            _ => return Err(ReplyError::Stale),
        };
        record.target_state = TargetState::AcknowledgingCancellation(reason);
        Ok(reason)
    }

    pub(crate) fn finish_cancellation_acknowledgement(
        &self,
        request: RequestId,
    ) -> Vec<WatchNotification> {
        let mut state = self.state.lock();
        let Some(record) = state.requests.get_mut(&request) else {
            return Vec::new();
        };
        let TargetState::AcknowledgingCancellation(reason) = record.target_state else {
            return Vec::new();
        };
        record.target_state = TargetState::Closed;
        if record.owner_state == OwnerState::Observing {
            record.owner_state = OwnerState::Unavailable(match reason {
                CancellationReason::RequesterCancelled => ResponseFailure::Cancelled,
                CancellationReason::DeadlineExpired => ResponseFailure::DeadlineExceeded,
            });
        }
        reevaluate_watches(&mut state)
    }

    pub(crate) fn rollback_cancellation_acknowledgement(&self, request: RequestId) {
        let mut state = self.state.lock();
        if let Some(record) = state.requests.get_mut(&request) {
            if let TargetState::AcknowledgingCancellation(reason) = record.target_state {
                record.target_state = TargetState::CancellationRequested {
                    presented: true,
                    reason,
                };
            }
        }
    }

    pub(crate) fn abandon_response(
        &self,
        owner: ActorRef,
        request: RequestId,
    ) -> Result<(AbandonResponseOutcome, Vec<WatchNotification>), ReplyError> {
        let mut state = self.state.lock();
        let record = state.requests.get_mut(&request).ok_or(ReplyError::Stale)?;
        authorize_owner(record, owner)?;
        let outcome = match record.owner_state {
            OwnerState::Observing => {
                record.owner_state = OwnerState::Abandoned;
                AbandonResponseOutcome::AbandonedNow
            }
            OwnerState::Abandoned => AbandonResponseOutcome::AlreadyAbandoned,
            OwnerState::Ready | OwnerState::Unavailable(_) => {
                AbandonResponseOutcome::AlreadyTerminal
            }
        };
        let notifications = reevaluate_watches(&mut state);
        Ok((outcome, notifications))
    }

    pub(crate) fn forget_response(
        &self,
        owner: ActorRef,
        request: RequestId,
    ) -> Result<ForgetResponseOutcome, ReplyError> {
        let mut state = self.state.lock();
        let record = state.requests.get(&request).ok_or(ReplyError::Stale)?;
        authorize_owner(record, owner)?;
        if record.owner_state == OwnerState::Observing {
            return Ok(ForgetResponseOutcome::StillPending);
        }
        if record.target_state != TargetState::Closed {
            return Ok(ForgetResponseOutcome::TargetStillActive);
        }
        let mut watches = state
            .watches
            .iter()
            .filter(|(_, watch)| {
                watch
                    .dependencies
                    .iter()
                    .any(|dependency| dependency.request == request)
            })
            .map(|(watch, _)| *watch)
            .collect::<Vec<_>>();
        watches.sort_unstable();
        if !watches.is_empty() {
            return Ok(ForgetResponseOutcome::RetainedByWatches(watches));
        }
        state.requests.remove(&request);
        Ok(ForgetResponseOutcome::Forgotten)
    }

    #[cfg(test)]
    pub(crate) fn register_watch(
        &self,
        owner: ActorRef,
        dependencies: Vec<RequestId>,
    ) -> Result<(WatchId, Vec<WatchNotification>), ReplyError> {
        self.register_watch_labeled(
            owner,
            "watch".into(),
            dependencies
                .into_iter()
                .map(|request| (request, false))
                .collect(),
        )
    }

    pub(crate) fn register_watch_labeled(
        &self,
        owner: ActorRef,
        label: String,
        dependencies: Vec<(RequestId, bool)>,
    ) -> Result<(WatchId, Vec<WatchNotification>), ReplyError> {
        let mut state = self.state.lock();
        for (request, _) in &dependencies {
            let record = state.requests.get(request).ok_or(ReplyError::Stale)?;
            authorize_owner(record, owner)?;
        }
        state.next_watch = state.next_watch.saturating_add(1);
        let id = WatchId(state.next_watch);
        state.watches.insert(
            id,
            WatchRecord {
                owner,
                label,
                dependencies: dependencies
                    .into_iter()
                    .map(|(request, allow_failure)| WatchDependency {
                        request,
                        allow_failure,
                    })
                    .collect(),
                state: WatchState::Pending,
            },
        );
        let notifications = reevaluate_watches(&mut state);
        Ok((id, notifications))
    }

    pub(crate) fn observe_watch(
        &self,
        owner: ActorRef,
        watch: WatchId,
    ) -> Result<WatchObservation, ReplyError> {
        let state = self.state.lock();
        let record = state.watches.get(&watch).ok_or(ReplyError::Stale)?;
        if record.owner != owner {
            return Err(identity_error(record.owner, owner));
        }
        Ok(match &record.state {
            WatchState::Pending => WatchObservation::Pending,
            WatchState::Ready => WatchObservation::Ready(
                record
                    .dependencies
                    .iter()
                    .filter_map(|dependency| {
                        let request = state.requests.get(&dependency.request)?;
                        match &request.owner_state {
                            OwnerState::Unavailable(failure) => {
                                Some((dependency.request, failure.clone()))
                            }
                            OwnerState::Abandoned => {
                                Some((dependency.request, ResponseFailure::Abandoned))
                            }
                            _ => None,
                        }
                    })
                    .collect(),
            ),
            WatchState::Unavailable { request, failure } => WatchObservation::Unavailable {
                request: *request,
                failure: failure.clone(),
            },
        })
    }

    pub(crate) fn forget_watch(
        &self,
        owner: ActorRef,
        watch: WatchId,
    ) -> Result<ForgetWatchOutcome, ReplyError> {
        let mut state = self.state.lock();
        let record = state.watches.get(&watch).ok_or(ReplyError::Stale)?;
        if record.owner != owner {
            return Err(identity_error(record.owner, owner));
        }
        if record.state == WatchState::Pending {
            return Ok(ForgetWatchOutcome::StillPending);
        }
        state.watches.remove(&watch);
        Ok(ForgetWatchOutcome::Forgotten)
    }

    pub(crate) fn forget_terminal_actor_metadata(
        &self,
        actor: ActorRef,
    ) -> Result<(), (Vec<RequestId>, Vec<WatchId>)> {
        let mut state = self.state.lock();
        let mut requests = state
            .requests
            .iter()
            .filter(|(_, record)| {
                record.target == actor
                    || (record.owner == actor && record.target_state != TargetState::Closed)
            })
            .map(|(request, _)| *request)
            .collect::<Vec<_>>();
        let mut watches = state
            .watches
            .iter()
            .filter(|(_, record)| record.owner == actor && record.state == WatchState::Pending)
            .map(|(watch, _)| *watch)
            .collect::<Vec<_>>();
        requests.sort_unstable();
        watches.sort_unstable();
        if !requests.is_empty() || !watches.is_empty() {
            return Err((requests, watches));
        }
        state.watches.retain(|_, record| record.owner != actor);
        state.requests.retain(|_, record| record.owner != actor);
        Ok(())
    }

    pub(crate) fn actor_stopped(
        &self,
        actor: ActorRef,
        terminal: &ActorTerminal,
    ) -> Vec<WatchNotification> {
        let mut state = self.state.lock();
        for record in state.requests.values_mut() {
            if record.target == actor {
                if record.target_state != TargetState::Closed {
                    record.target_state = TargetState::Closed;
                    if record.owner_state == OwnerState::Observing {
                        record.owner_state = OwnerState::Unavailable(match terminal.kind {
                            ActorExitKind::Completed => ResponseFailure::TargetUnavailable,
                            ActorExitKind::Failed => {
                                ResponseFailure::TargetFailed(terminal.summary.clone())
                            }
                            ActorExitKind::Cancelled => {
                                ResponseFailure::TargetCancelled(terminal.summary.clone())
                            }
                        });
                    }
                }
            } else if record.owner == actor && record.owner_state == OwnerState::Observing {
                record.owner_state = OwnerState::Unavailable(ResponseFailure::RequesterStopped);
            }
        }
        reevaluate_watches(&mut state)
    }

    fn transition_request(
        &self,
        owner: ActorRef,
        request: RequestId,
        transition: impl FnOnce(&mut RequestRecord),
    ) -> Vec<WatchNotification> {
        let mut state = self.state.lock();
        let Some(record) = state.requests.get_mut(&request) else {
            return Vec::new();
        };
        if record.owner != owner || is_owner_terminal(&record.owner_state) {
            return Vec::new();
        }
        transition(record);
        reevaluate_watches(&mut state)
    }
}

fn is_owner_terminal(state: &OwnerState) -> bool {
    matches!(
        state,
        OwnerState::Ready | OwnerState::Unavailable(_) | OwnerState::Abandoned
    )
}

fn authorize_owner(record: &RequestRecord, actor: ActorRef) -> Result<(), ReplyError> {
    if record.owner == actor {
        Ok(())
    } else {
        Err(identity_error(record.owner, actor))
    }
}

fn authorize_target(record: &RequestRecord, actor: ActorRef) -> Result<(), ReplyError> {
    if record.target == actor {
        Ok(())
    } else {
        Err(identity_error(record.target, actor))
    }
}

fn identity_error(expected: ActorRef, actual: ActorRef) -> ReplyError {
    if expected.id == actual.id {
        ReplyError::WrongIncarnation
    } else {
        ReplyError::Unauthorized
    }
}

fn reevaluate_watches(state: &mut RequestStateTable) -> Vec<WatchNotification> {
    let mut notifications = Vec::new();
    for (watch_id, watch) in &mut state.watches {
        if watch.state != WatchState::Pending {
            continue;
        }
        let transition = watch.dependencies.iter().find_map(|dependency| {
            let record = state.requests.get(&dependency.request)?;
            match &record.owner_state {
                OwnerState::Unavailable(failure) if !dependency.allow_failure => {
                    Some(WatchTransition::Unavailable {
                        request: dependency.request,
                        failure: failure.clone(),
                    })
                }
                OwnerState::Abandoned if !dependency.allow_failure => {
                    Some(WatchTransition::Unavailable {
                        request: dependency.request,
                        failure: ResponseFailure::Abandoned,
                    })
                }
                _ => None,
            }
        });
        let transition = transition.or_else(|| {
            watch
                .dependencies
                .iter()
                .all(|dependency| {
                    state
                        .requests
                        .get(&dependency.request)
                        .is_some_and(|record| match record.owner_state {
                            OwnerState::Ready => true,
                            OwnerState::Unavailable(_) | OwnerState::Abandoned => {
                                dependency.allow_failure
                            }
                            _ => false,
                        })
                })
                .then_some(WatchTransition::Ready)
        });
        let Some(transition) = transition else {
            continue;
        };
        watch.state = match &transition {
            WatchTransition::Ready => WatchState::Ready,
            WatchTransition::Unavailable { request, failure } => WatchState::Unavailable {
                request: *request,
                failure: failure.clone(),
            },
        };
        notifications.push(WatchNotification {
            owner: watch.owner,
            watch: *watch_id,
            transition,
        });
    }
    notifications
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ActorId, Incarnation};

    fn actor(id: u64) -> ActorRef {
        ActorRef::first(ActorId(id))
    }

    #[test]
    fn settlement_is_exactly_once_and_wakes_registered_fan_in_once() {
        let registry = RequestRegistry::default();
        let owner = actor(1);
        let left_target = actor(2);
        let right_target = actor(3);
        let left = registry.reserve(owner, left_target);
        let right = registry.reserve(owner, right_target);
        registry.mark_queued(owner, left_target, left).unwrap();
        registry.mark_queued(owner, right_target, right).unwrap();
        registry.present(left_target, left).unwrap();
        registry.present(right_target, right).unwrap();
        let (watch, initial) = registry.register_watch(owner, vec![left, right]).unwrap();
        assert!(initial.is_empty());

        registry.begin_reply(left_target, left).unwrap();
        assert!(registry.finish_reply(left).is_empty());
        registry.begin_reply(right_target, right).unwrap();
        let notifications = registry.finish_reply(right);
        assert_eq!(notifications.len(), 1);
        assert_eq!(notifications[0].watch, watch);
        assert_eq!(notifications[0].transition, WatchTransition::Ready);
        assert_eq!(registry.finish_reply(right), Vec::new());
        assert_eq!(
            registry.begin_reply(right_target, right),
            Err(ReplyError::AlreadySettled)
        );
        assert_eq!(
            registry.observe_watch(owner, watch),
            Ok(WatchObservation::Ready(Vec::new()))
        );
    }

    #[test]
    fn ready_before_watch_registration_still_publishes_one_notification() {
        let registry = RequestRegistry::default();
        let owner = actor(1);
        let target = actor(2);
        let request = registry.reserve(owner, target);
        registry.mark_queued(owner, target, request).unwrap();
        registry.present(target, request).unwrap();
        registry.begin_reply(target, request).unwrap();
        assert!(registry.finish_reply(request).is_empty());

        let (watch, notifications) = registry.register_watch(owner, vec![request]).unwrap();
        assert_eq!(notifications.len(), 1);
        assert_eq!(notifications[0].watch, watch);
        assert_eq!(notifications[0].transition, WatchTransition::Ready);
    }

    #[test]
    fn accepted_reply_failure_is_terminal_and_wakes_watch_once() {
        let registry = RequestRegistry::default();
        let owner = actor(1);
        let target = actor(2);
        let request = registry.reserve(owner, target);
        registry.mark_queued(owner, target, request).unwrap();
        registry.present(target, request).unwrap();
        let (watch, initial) = registry.register_watch(owner, vec![request]).unwrap();
        assert!(initial.is_empty());

        registry.begin_reply(target, request).unwrap();
        let notifications = registry.fail_reply_settlement(request, "continuation trapped");
        assert_eq!(notifications.len(), 1);
        assert_eq!(notifications[0].watch, watch);
        assert_eq!(
            notifications[0].transition,
            WatchTransition::Unavailable {
                request,
                failure: ResponseFailure::SettlementFailed("continuation trapped".into()),
            }
        );
        assert_eq!(
            registry.observe_response(owner, request),
            Ok(ResponseObservation::Unavailable(
                ResponseFailure::SettlementFailed("continuation trapped".into())
            ))
        );
        assert_eq!(
            registry.begin_reply(target, request),
            Err(ReplyError::AlreadySettled)
        );
        assert!(registry
            .fail_reply_settlement(request, "second failure")
            .is_empty());
    }

    #[test]
    fn stale_incarnation_cannot_settle_a_request() {
        let registry = RequestRegistry::default();
        let owner = actor(1);
        let target = actor(2);
        let request = registry.reserve(owner, target);
        registry.mark_queued(owner, target, request).unwrap();
        registry.present(target, request).unwrap();
        let restarted = ActorRef {
            id: target.id,
            incarnation: Incarnation(2),
        };
        assert_eq!(
            registry.begin_reply(restarted, request),
            Err(ReplyError::WrongIncarnation)
        );
        assert_eq!(
            registry.observe_response(owner, request),
            Ok(ResponseObservation::Pending)
        );
    }

    #[test]
    fn target_failure_settles_response_and_watch_once() {
        let registry = RequestRegistry::default();
        let owner = actor(1);
        let target = actor(2);
        let request = registry.reserve(owner, target);
        registry.mark_queued(owner, target, request).unwrap();
        let (watch, initial) = registry.register_watch(owner, vec![request]).unwrap();
        assert!(initial.is_empty());
        let terminal = ActorTerminal {
            kind: ActorExitKind::Failed,
            summary: "boom".into(),
        };
        let notifications = registry.actor_stopped(target, &terminal);
        assert_eq!(notifications.len(), 1);
        assert_eq!(notifications[0].watch, watch);
        assert_eq!(registry.actor_stopped(target, &terminal), Vec::new());
        assert_eq!(
            registry.observe_response(owner, request),
            Ok(ResponseObservation::Unavailable(
                ResponseFailure::TargetFailed("boom".into())
            ))
        );
    }

    #[test]
    fn collect_all_watch_becomes_ready_with_typed_failures() {
        let registry = RequestRegistry::default();
        let owner = actor(1);
        let failed_target = actor(2);
        let ready_target = actor(3);
        let failed = registry.reserve(owner, failed_target);
        let ready = registry.reserve(owner, ready_target);
        registry.mark_queued(owner, failed_target, failed).unwrap();
        registry.mark_queued(owner, ready_target, ready).unwrap();
        registry.present(ready_target, ready).unwrap();
        let (watch, initial) = registry
            .register_watch_labeled(
                owner,
                "collect-all".into(),
                vec![(failed, true), (ready, true)],
            )
            .unwrap();
        assert!(initial.is_empty());

        let terminal = ActorTerminal {
            kind: ActorExitKind::Failed,
            summary: "boom".into(),
        };
        assert!(registry.actor_stopped(failed_target, &terminal).is_empty());
        registry.begin_reply(ready_target, ready).unwrap();
        let notifications = registry.finish_reply(ready);
        assert_eq!(notifications.len(), 1);
        assert_eq!(notifications[0].watch, watch);
        assert_eq!(
            registry.observe_watch(owner, watch),
            Ok(WatchObservation::Ready(vec![(
                failed,
                ResponseFailure::TargetFailed("boom".into())
            )]))
        );
    }

    #[test]
    fn response_cancellation_requires_target_acknowledgement() {
        let registry = RequestRegistry::default();
        let owner = actor(1);
        let target = actor(2);
        let intruder = actor(3);
        let request = registry.reserve(owner, target);
        registry.mark_queued(owner, target, request).unwrap();
        let (watch, _) = registry.register_watch(owner, vec![request]).unwrap();

        assert_eq!(
            registry.cancel_request(intruder, request, CancellationReason::RequesterCancelled),
            Err(ReplyError::Unauthorized)
        );
        let (outcome, notification) = registry
            .cancel_request(owner, request, CancellationReason::RequesterCancelled)
            .unwrap();
        assert_eq!(outcome, CancelRequestOutcome::Requested);
        assert_eq!(notification, None);
        assert_eq!(
            registry.observe_response(owner, request),
            Ok(ResponseObservation::CancellationPending(
                CancellationReason::RequesterCancelled
            ))
        );
        assert_eq!(
            registry.present(target, request),
            Ok(Some(CancellationReason::RequesterCancelled))
        );
        assert_eq!(
            registry.observe_reply(target, request),
            Ok(ReplyObservation::CancellationRequested(
                CancellationReason::RequesterCancelled
            ))
        );
        registry
            .begin_cancellation_acknowledgement(target, request)
            .unwrap();
        let notifications = registry.finish_cancellation_acknowledgement(request);
        assert_eq!(notifications.len(), 1);
        assert_eq!(notifications[0].watch, watch);
        assert_eq!(
            registry.observe_response(owner, request),
            Ok(ResponseObservation::Unavailable(ResponseFailure::Cancelled))
        );
        assert_eq!(
            registry.cancel_request(owner, request, CancellationReason::RequesterCancelled),
            Ok((CancelRequestOutcome::AlreadyTerminal, None))
        );
    }

    #[test]
    fn response_deadline_uses_the_same_acknowledged_transition() {
        let registry = RequestRegistry::default();
        let owner = actor(1);
        let target = actor(2);
        let request = registry.reserve(owner, target);
        registry.mark_queued(owner, target, request).unwrap();
        let (watch, _) = registry.register_watch(owner, vec![request]).unwrap();

        assert_eq!(registry.deadline_request(owner, request), None);
        assert_eq!(
            registry.observe_response(owner, request),
            Ok(ResponseObservation::CancellationPending(
                CancellationReason::DeadlineExpired
            ))
        );
        registry.present(target, request).unwrap();
        registry
            .begin_cancellation_acknowledgement(target, request)
            .unwrap();
        let notifications = registry.finish_cancellation_acknowledgement(request);
        assert_eq!(notifications.len(), 1);
        assert_eq!(notifications[0].watch, watch);
        assert_eq!(
            registry.observe_response(owner, request),
            Ok(ResponseObservation::Unavailable(
                ResponseFailure::DeadlineExceeded
            ))
        );
        assert_eq!(registry.deadline_request(owner, request), None);
        assert_eq!(
            registry.cancel_request(owner, request, CancellationReason::RequesterCancelled),
            Ok((CancelRequestOutcome::AlreadyTerminal, None))
        );
    }

    #[test]
    fn abandonment_settles_observation_without_closing_target_execution() {
        let registry = RequestRegistry::default();
        let owner = actor(1);
        let target = actor(2);
        let request = registry.reserve(owner, target);
        registry.mark_queued(owner, target, request).unwrap();
        registry.present(target, request).unwrap();

        let (outcome, _) = registry.abandon_response(owner, request).unwrap();
        assert_eq!(outcome, AbandonResponseOutcome::AbandonedNow);
        assert_eq!(
            registry.observe_response(owner, request),
            Ok(ResponseObservation::Unavailable(ResponseFailure::Abandoned))
        );
        assert_eq!(
            registry.observe_reply(target, request),
            Ok(ReplyObservation::Open)
        );
        registry.begin_reply(target, request).unwrap();
        assert!(registry.finish_reply(request).is_empty());
        assert_eq!(
            registry.observe_reply(target, request),
            Ok(ReplyObservation::Closed)
        );
        assert_eq!(
            registry.observe_response(owner, request),
            Ok(ResponseObservation::Unavailable(ResponseFailure::Abandoned))
        );
    }

    #[test]
    fn requester_stop_terminalizes_owned_responses_once() {
        let registry = RequestRegistry::default();
        let owner = actor(1);
        let target = actor(2);
        let request = registry.reserve(owner, target);
        registry.mark_queued(owner, target, request).unwrap();
        let terminal = ActorTerminal {
            kind: ActorExitKind::Cancelled,
            summary: "owner stopped".into(),
        };

        assert!(registry.actor_stopped(owner, &terminal).is_empty());
        assert!(registry.actor_stopped(owner, &terminal).is_empty());
        assert_eq!(
            registry.observe_response(owner, request),
            Ok(ResponseObservation::Unavailable(
                ResponseFailure::RequesterStopped
            ))
        );
    }

    #[test]
    fn another_actor_cannot_observe_or_watch_a_response() {
        let registry = RequestRegistry::default();
        let owner = actor(1);
        let target = actor(2);
        let intruder = actor(3);
        let request = registry.reserve(owner, target);
        registry.mark_queued(owner, target, request).unwrap();

        assert_eq!(
            registry.observe_response(intruder, request),
            Err(ReplyError::Unauthorized)
        );
        assert_eq!(
            registry.register_watch(intruder, vec![request]),
            Err(ReplyError::Unauthorized)
        );
    }

    #[test]
    fn explicit_cleanup_refuses_live_dependencies_and_succeeds_inside_out() {
        let registry = RequestRegistry::default();
        let owner = actor(1);
        let target = actor(2);
        let request = registry.reserve(owner, target);
        registry.mark_queued(owner, target, request).unwrap();
        registry.present(target, request).unwrap();
        let (watch, _) = registry.register_watch(owner, vec![request]).unwrap();

        assert_eq!(
            registry.forget_response(owner, request),
            Ok(ForgetResponseOutcome::StillPending)
        );
        assert_eq!(
            registry.forget_watch(owner, watch),
            Ok(ForgetWatchOutcome::StillPending)
        );
        registry.begin_reply(target, request).unwrap();
        registry.finish_reply(request);
        assert_eq!(
            registry.forget_response(owner, request),
            Ok(ForgetResponseOutcome::RetainedByWatches(vec![watch]))
        );
        assert_eq!(
            registry.forget_watch(owner, watch),
            Ok(ForgetWatchOutcome::Forgotten)
        );
        assert_eq!(
            registry.forget_response(owner, request),
            Ok(ForgetResponseOutcome::Forgotten)
        );
        assert_eq!(
            registry.observe_response(owner, request),
            Err(ReplyError::Stale)
        );
    }
}
