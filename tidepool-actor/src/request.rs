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
    Cancelled,
    DeadlineExceeded,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResponseObservation {
    Pending,
    Ready,
    Unavailable(ResponseFailure),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReplyError {
    Stale,
    AlreadySettled,
    Unauthorized,
    WrongIncarnation,
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
enum RequestState {
    Reserved,
    Queued,
    Presented,
    Settling,
    Ready,
    Unavailable(ResponseFailure),
}

struct RequestRecord {
    owner: ActorRef,
    target: ActorRef,
    label: String,
    state: RequestState,
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
            match record.state {
                RequestState::Ready => status
                    .ready_responses
                    .push((*request, record.label.clone())),
                RequestState::Unavailable(_) => status
                    .unavailable_responses
                    .push((*request, record.label.clone())),
                RequestState::Reserved
                | RequestState::Queued
                | RequestState::Presented
                | RequestState::Settling => status
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
                state: RequestState::Reserved,
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
            record.state = RequestState::Unavailable(ResponseFailure::TargetUnavailable);
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
        match record.state {
            RequestState::Reserved => {
                record.state = RequestState::Queued;
                Ok(())
            }
            _ => Err(ReplyError::AlreadySettled),
        }
    }

    pub(crate) fn present(&self, target: ActorRef, request: RequestId) -> Result<(), ReplyError> {
        let mut state = self.state.lock();
        let record = state.requests.get_mut(&request).ok_or(ReplyError::Stale)?;
        authorize_target(record, target)?;
        match record.state {
            RequestState::Queued => {
                record.state = RequestState::Presented;
                Ok(())
            }
            RequestState::Unavailable(_) | RequestState::Ready => Err(ReplyError::AlreadySettled),
            RequestState::Reserved | RequestState::Presented | RequestState::Settling => {
                Err(ReplyError::Stale)
            }
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
        match record.state {
            RequestState::Presented => {
                record.state = RequestState::Settling;
                Ok(())
            }
            RequestState::Ready | RequestState::Unavailable(_) => Err(ReplyError::AlreadySettled),
            RequestState::Reserved | RequestState::Queued | RequestState::Settling => {
                Err(ReplyError::Stale)
            }
        }
    }

    pub(crate) fn finish_reply(&self, request: RequestId) -> Vec<WatchNotification> {
        let mut state = self.state.lock();
        let Some(record) = state.requests.get_mut(&request) else {
            return Vec::new();
        };
        if record.state != RequestState::Settling {
            return Vec::new();
        }
        record.state = RequestState::Ready;
        reevaluate_watches(&mut state)
    }

    pub(crate) fn rollback_reply(&self, request: RequestId) {
        let mut state = self.state.lock();
        if let Some(record) = state.requests.get_mut(&request) {
            if record.state == RequestState::Settling {
                record.state = RequestState::Presented;
            }
        }
    }

    pub(crate) fn observe_response(
        &self,
        owner: ActorRef,
        request: RequestId,
    ) -> Result<ResponseObservation, ReplyError> {
        let state = self.state.lock();
        let record = state.requests.get(&request).ok_or(ReplyError::Stale)?;
        authorize_owner(record, owner)?;
        Ok(match &record.state {
            RequestState::Reserved
            | RequestState::Queued
            | RequestState::Presented
            | RequestState::Settling => ResponseObservation::Pending,
            RequestState::Ready => ResponseObservation::Ready,
            RequestState::Unavailable(failure) => ResponseObservation::Unavailable(failure.clone()),
        })
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
                        match &request.state {
                            RequestState::Unavailable(failure) => {
                                Some((dependency.request, failure.clone()))
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

    pub(crate) fn actor_stopped(
        &self,
        actor: ActorRef,
        terminal: &ActorTerminal,
    ) -> Vec<WatchNotification> {
        let mut state = self.state.lock();
        for record in state.requests.values_mut() {
            if is_terminal(&record.state) {
                continue;
            }
            if record.target == actor {
                record.state = RequestState::Unavailable(match terminal.kind {
                    ActorExitKind::Completed => ResponseFailure::TargetUnavailable,
                    ActorExitKind::Failed => {
                        ResponseFailure::TargetFailed(terminal.summary.clone())
                    }
                    ActorExitKind::Cancelled => {
                        ResponseFailure::TargetCancelled(terminal.summary.clone())
                    }
                });
            } else if record.owner == actor {
                record.state = RequestState::Unavailable(ResponseFailure::RequesterStopped);
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
        if record.owner != owner || is_terminal(&record.state) {
            return Vec::new();
        }
        transition(record);
        reevaluate_watches(&mut state)
    }
}

fn is_terminal(state: &RequestState) -> bool {
    matches!(state, RequestState::Ready | RequestState::Unavailable(_))
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
            match &record.state {
                RequestState::Unavailable(failure) if !dependency.allow_failure => {
                    Some(WatchTransition::Unavailable {
                        request: dependency.request,
                        failure: failure.clone(),
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
                        .is_some_and(|record| match record.state {
                            RequestState::Ready => true,
                            RequestState::Unavailable(_) => dependency.allow_failure,
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
}
