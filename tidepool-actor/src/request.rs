pub(crate) mod routes;
mod updates;
pub use updates::{
    RequestUpdateDelivery, RequestUpdateId, RequestUpdatePresentation, RequestUpdateState,
};

use std::collections::HashMap;

use parking_lot::Mutex;
use serde::{Deserialize, Serialize};

use crate::{ActorExitKind, ActorRef, ActorTerminal};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum DeadlineUnit {
    Milliseconds,
    Seconds,
    Minutes,
}

impl std::fmt::Display for DeadlineUnit {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::Milliseconds => "ms",
            Self::Seconds => "s",
            Self::Minutes => "min",
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RequestDeadline {
    value: u64,
    unit: DeadlineUnit,
    milliseconds: u64,
}

impl RequestDeadline {
    pub(crate) fn checked(value: i64, unit: DeadlineUnit) -> Result<Self, String> {
        let milliseconds_per_unit = match unit {
            DeadlineUnit::Milliseconds => 1,
            DeadlineUnit::Seconds => 1_000,
            DeadlineUnit::Minutes => 60_000,
        };
        let value = u64::try_from(value)
            .map_err(|_| format!("request deadline must be non-negative {unit}"))?;
        let milliseconds = value
            .checked_mul(milliseconds_per_unit)
            .ok_or_else(|| format!("request deadline of {value}{unit} is too large"))?;
        Ok(Self {
            value,
            unit,
            milliseconds,
        })
    }

    #[must_use]
    pub fn duration(self) -> std::time::Duration {
        std::time::Duration::from_millis(self.milliseconds)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct RequestId(pub u64);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct WatchId(pub u64);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ActorEventSequence(pub u64);

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
    UpdatePending,
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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum WatchStateProjection {
    Pending,
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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WatchNotification {
    pub owner: ActorRef,
    pub watch: WatchId,
    pub label: String,
    pub previous: WatchStateProjection,
    pub current: WatchStateProjection,
    pub transition: WatchTransition,
    pub occurred_at_unix_ms: u64,
    pub sequence: ActorEventSequence,
    pub watermark: ActorEventSequence,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RequestCancellationNotification {
    pub target: ActorRef,
    pub request: RequestId,
    pub label: String,
    pub reason: CancellationReason,
    pub occurred_at_unix_ms: u64,
    pub sequence: ActorEventSequence,
    pub watermark: ActorEventSequence,
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
    updates: Vec<updates::UpdateRecord>,
    owner: ActorRef,
    target: ActorRef,
    label: String,
    target_state: TargetState,
    owner_state: OwnerState,
    deadline: Option<ActiveRequestDeadline>,
    progress: Option<ProgressSnapshot>,
}

/// Snapshots share custody, not a consumption cursor. Replacing the latest
/// publication drops only the registry's reference; an observer can retain it.
#[derive(Clone)]
pub(crate) struct ProgressSnapshot {
    pub revision: u64,
    pub value: std::sync::Arc<tidepool_runtime::session::RootCustody>,
}

#[derive(Debug, Clone)]
pub(crate) struct ActiveRequestDeadline {
    authored: RequestDeadline,
    due_wall: std::time::SystemTime,
    due_monotonic: tokio::time::Instant,
}

impl ActiveRequestDeadline {
    pub(crate) fn start(authored: RequestDeadline) -> Result<Self, String> {
        let duration = authored.duration();
        let due_wall = std::time::SystemTime::now()
            .checked_add(duration)
            .ok_or_else(|| "request deadline exceeds the wall-clock range".to_string())?;
        let due_monotonic = tokio::time::Instant::now()
            .checked_add(duration)
            .ok_or_else(|| "request deadline exceeds the monotonic-clock range".to_string())?;
        Ok(Self {
            authored,
            due_wall,
            due_monotonic,
        })
    }

    #[must_use]
    pub(crate) fn due_monotonic(&self) -> tokio::time::Instant {
        self.due_monotonic
    }

    fn render(&self, request: RequestId, label: &str) -> String {
        let due_unix_ms = self
            .due_wall
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |duration| duration.as_millis());
        let remaining = self
            .due_monotonic
            .saturating_duration_since(tokio::time::Instant::now());
        format!(
            "{request:?} {label:?} after={}{} due_unix_ms={due_unix_ms} remaining={}ms",
            self.authored.value,
            self.authored.unit,
            remaining.as_millis()
        )
    }
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
    route: Option<routes::WatchRoute>,
    owner: ActorRef,
    label: String,
    dependencies: Vec<WatchDependency>,
    state: WatchState,
    progress: HashMap<(RequestId, u64), ProgressCapture>,
}

#[derive(Clone)]
pub(crate) enum ProgressCapture {
    Update(ProgressSnapshot),
    Closed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum WatchRequirement {
    Response { allow_failure: bool },
    ProgressAfter(u64),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct WatchDependency {
    request: RequestId,
    requirement: WatchRequirement,
}

#[derive(Default)]
struct RequestStateTable {
    next_request: u64,
    received_requests: HashMap<ActorRef, u64>,
    next_watch: u64,
    next_event_by_actor: HashMap<ActorRef, u64>,
    cleanup_revision: HashMap<ActorRef, u64>,
    cleaning: std::collections::HashSet<ActorRef>,
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
    pub unavailable_responses: Vec<(RequestId, String, ResponseFailure)>,
    pub pending_watches: Vec<(WatchId, String)>,
    pub ready_watches: Vec<(WatchId, String)>,
    pub unavailable_watches: Vec<(WatchId, String, ResponseFailure)>,
    pub deadlines: Vec<(RequestId, String)>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct CleanupMetadataOutcome {
    pub forgotten_responses: Vec<RequestId>,
    pub forgotten_watches: Vec<WatchId>,
    pub pending_responses: Vec<RequestId>,
    pub pending_watches: Vec<WatchId>,
}

fn cleanup_blockers(
    state: &RequestStateTable,
    owners: &std::collections::HashSet<ActorRef>,
    targets: &std::collections::HashSet<ActorRef>,
) -> (Vec<RequestId>, Vec<WatchId>) {
    let scoped = state
        .requests
        .iter()
        .filter_map(|(request, record)| {
            (targets.contains(&record.owner) || targets.contains(&record.target))
                .then_some((*request, record))
        })
        .collect::<std::collections::HashMap<_, _>>();
    let mut requests = scoped
        .iter()
        .filter_map(|(request, record)| {
            (record.owner_state == OwnerState::Observing
                || record.target_state != TargetState::Closed)
                .then_some(*request)
        })
        .collect::<Vec<_>>();
    let mut watches = state
        .watches
        .iter()
        .filter_map(|(watch, record)| {
            (owners.contains(&record.owner)
                && (record.state == WatchState::Pending
                    || record
                        .route
                        .as_ref()
                        .is_some_and(routes::WatchRoute::is_active))
                && (targets.contains(&record.owner)
                    || record
                        .dependencies
                        .iter()
                        .any(|dependency| scoped.contains_key(&dependency.request))))
            .then_some(*watch)
        })
        .collect::<Vec<_>>();
    requests.sort_unstable();
    watches.sort_unstable();
    (requests, watches)
}

pub(crate) enum CleanupAdmissionError {
    Stale,
    Busy,
    Pending,
}

pub(crate) struct RequestCleanupGuard {
    registry: std::sync::Arc<RequestRegistry>,
    targets: std::collections::HashSet<ActorRef>,
}

impl Drop for RequestCleanupGuard {
    fn drop(&mut self) {
        let mut state = self.registry.state.lock();
        for actor in &self.targets {
            state.cleaning.remove(actor);
        }
    }
}

impl RequestRegistry {
    pub(crate) fn publish_progress(
        &self,
        target: ActorRef,
        request: RequestId,
        value: tidepool_runtime::session::RootCustody,
    ) -> Result<(u64, Vec<WatchNotification>), ReplyError> {
        let mut state = self.state.lock();
        let record = state.requests.get_mut(&request).ok_or(ReplyError::Stale)?;
        authorize_progress_publication(record, target)?;
        let revision = record
            .progress
            .as_ref()
            .map_or(Some(1), |previous| previous.revision.checked_add(1))
            .filter(|revision| *revision <= i64::MAX as u64)
            .ok_or(ReplyError::Stale)?;
        record.progress = Some(ProgressSnapshot {
            revision,
            value: std::sync::Arc::new(value),
        });
        Ok((revision, reevaluate_watches(&mut state)))
    }

    pub(crate) fn observe_progress(
        &self,
        owner: ActorRef,
        request: RequestId,
    ) -> Result<(Option<ProgressSnapshot>, bool), ReplyError> {
        let state = self.state.lock();
        let record = state.requests.get(&request).ok_or(ReplyError::Stale)?;
        authorize_owner(record, owner)?;
        let closed = record.owner_state != OwnerState::Observing
            || matches!(
                record.target_state,
                TargetState::Closed
                    | TargetState::AcknowledgingCancellation(_)
                    | TargetState::Settling
            );
        Ok((record.progress.clone(), closed))
    }

    pub(crate) fn campaign_cleanup_blockers(
        &self,
        owners: &std::collections::HashSet<ActorRef>,
        targets: &std::collections::HashSet<ActorRef>,
    ) -> (Vec<RequestId>, Vec<WatchId>) {
        cleanup_blockers(&self.state.lock(), owners, targets)
    }

    pub(crate) fn cleanup_revision(&self, actor: ActorRef) -> u64 {
        self.state
            .lock()
            .cleanup_revision
            .get(&actor)
            .copied()
            .unwrap_or(0)
    }

    pub(crate) fn begin_cleanup(
        self: &std::sync::Arc<Self>,
        owner: ActorRef,
        expected: &[(ActorRef, u64)],
    ) -> Result<RequestCleanupGuard, CleanupAdmissionError> {
        let mut state = self.state.lock();
        let targets = expected
            .iter()
            .map(|(actor, _)| *actor)
            .collect::<std::collections::HashSet<_>>();
        if targets.iter().any(|actor| state.cleaning.contains(actor)) {
            return Err(CleanupAdmissionError::Busy);
        }
        if expected.iter().any(|(actor, revision)| {
            state.cleanup_revision.get(actor).copied().unwrap_or(0) != *revision
        }) {
            return Err(CleanupAdmissionError::Stale);
        }
        let mut owners = targets.clone();
        owners.insert(owner);
        let (responses, watches) = cleanup_blockers(&state, &owners, &targets);
        if !responses.is_empty() || !watches.is_empty() {
            return Err(CleanupAdmissionError::Pending);
        }
        state.cleaning.extend(targets.iter().copied());
        Ok(RequestCleanupGuard {
            registry: std::sync::Arc::clone(self),
            targets,
        })
    }

    /// Release terminal request/watch metadata owned by `owner` and wholly
    /// contained in `targets`. Pending dependencies are reported as blockers
    /// and retained. This is the request owner's campaign-cleanup primitive;
    /// callers do not reproduce dependency ordering.
    pub(crate) fn cleanup_campaign_metadata(
        &self,
        owner: ActorRef,
        targets: &std::collections::HashSet<ActorRef>,
    ) -> CleanupMetadataOutcome {
        let mut state = self.state.lock();
        let scoped_requests = state
            .requests
            .iter()
            .filter_map(|(request, record)| {
                (record.owner == owner
                    && (targets.contains(&owner) || targets.contains(&record.target)))
                .then_some(*request)
            })
            .collect::<std::collections::HashSet<_>>();
        let scoped_watches = state
            .watches
            .iter()
            .filter_map(|(watch, record)| {
                (record.owner == owner
                    && (targets.contains(&owner)
                        || (!record.dependencies.is_empty()
                            && record
                                .dependencies
                                .iter()
                                .all(|dependency| scoped_requests.contains(&dependency.request)))))
                .then_some(*watch)
            })
            .collect::<Vec<_>>();

        let mut outcome = CleanupMetadataOutcome::default();
        for watch in scoped_watches {
            let Some(record) = state.watches.get(&watch) else {
                continue;
            };
            if record.state == WatchState::Pending
                || record
                    .route
                    .as_ref()
                    .is_some_and(routes::WatchRoute::is_active)
            {
                outcome.pending_watches.push(watch);
            } else {
                state.watches.remove(&watch);
                outcome.forgotten_watches.push(watch);
            }
        }
        for request in scoped_requests {
            let retained_by_watch = state.watches.values().any(|watch| {
                watch
                    .dependencies
                    .iter()
                    .any(|dependency| dependency.request == request)
            });
            let Some(record) = state.requests.get(&request) else {
                continue;
            };
            if record.owner_state == OwnerState::Observing
                || record.target_state != TargetState::Closed
                || retained_by_watch
            {
                outcome.pending_responses.push(request);
            } else {
                state.requests.remove(&request);
                outcome.forgotten_responses.push(request);
            }
        }
        outcome.forgotten_responses.sort_unstable();
        outcome.forgotten_watches.sort_unstable();
        outcome.pending_responses.sort_unstable();
        outcome.pending_watches.sort_unstable();
        outcome
    }

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

    pub(crate) fn work_for_target(&self, target: ActorRef) -> (Vec<RequestId>, Vec<RequestId>) {
        let state = self.state.lock();
        let mut current = Vec::new();
        let mut queued = Vec::new();
        for (id, record) in &state.requests {
            if record.target != target {
                continue;
            }
            match record.target_state {
                TargetState::Closed => {}
                TargetState::Reserved
                | TargetState::Queued
                | TargetState::CancellationRequested {
                    presented: false, ..
                } => queued.push(*id),
                _ => current.push(*id),
            }
        }
        current.sort_unstable();
        queued.sort_unstable();
        (current, queued)
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
            deadlines: Vec::new(),
        };
        for (request, record) in &state.requests {
            if record.owner != owner {
                continue;
            }
            match record.owner_state {
                OwnerState::Ready => status
                    .ready_responses
                    .push((*request, record.label.clone())),
                OwnerState::Unavailable(ref failure) => status.unavailable_responses.push((
                    *request,
                    record.label.clone(),
                    failure.clone(),
                )),
                OwnerState::Abandoned => status.unavailable_responses.push((
                    *request,
                    record.label.clone(),
                    ResponseFailure::Abandoned,
                )),
                OwnerState::Observing => status
                    .pending_responses
                    .push((*request, record.label.clone())),
            }
            if matches!(record.owner_state, OwnerState::Observing) {
                if let Some(deadline) = &record.deadline {
                    status
                        .deadlines
                        .push((*request, deadline.render(*request, &record.label)));
                }
            }
        }
        for (watch, record) in &state.watches {
            if record.owner != owner {
                continue;
            }
            match record.state {
                WatchState::Pending => status.pending_watches.push((*watch, record.label.clone())),
                WatchState::Ready => status.ready_watches.push((*watch, record.label.clone())),
                WatchState::Unavailable { ref failure, .. } => {
                    status
                        .unavailable_watches
                        .push((*watch, record.label.clone(), failure.clone()))
                }
            }
        }
        status.pending_responses.sort_unstable();
        status.ready_responses.sort_unstable();
        status
            .unavailable_responses
            .sort_unstable_by_key(|entry| entry.0);
        status.pending_watches.sort_unstable();
        status.ready_watches.sort_unstable();
        status
            .unavailable_watches
            .sort_unstable_by_key(|entry| entry.0);
        status.deadlines.sort_unstable();
        status
    }

    #[cfg(test)]
    pub(crate) fn reserve(&self, owner: ActorRef, target: ActorRef) -> RequestId {
        self.reserve_labeled(owner, target, "request".into())
    }

    pub(crate) fn received_counts(&self, actor: ActorRef) -> (u64, u64) {
        let state = self.state.lock();
        (
            state.received_requests.get(&actor).copied().unwrap_or(0),
            state.next_event_by_actor.get(&actor).copied().unwrap_or(0),
        )
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
                updates: Vec::new(),
                owner,
                target,
                label,
                target_state: TargetState::Reserved,
                owner_state: OwnerState::Observing,
                deadline: None,
                progress: None,
            },
        );
        id
    }

    /// Remove request identities which never crossed the admission commit.
    ///
    /// The Haskell facade constructs a live request payload in two private
    /// effect steps because the payload itself contains the runtime-minted
    /// request id. The surrounding workbench invocation is the transaction:
    /// anything still `Reserved` when that invocation settles was never
    /// published and must not leak into observable request state.
    pub(crate) fn abort_unsubmitted(&self, owner: ActorRef) -> Vec<RequestId> {
        let mut state = self.state.lock();
        let mut aborted = state
            .requests
            .iter()
            .filter_map(|(request, record)| {
                (record.owner == owner && record.target_state == TargetState::Reserved)
                    .then_some(*request)
            })
            .collect::<Vec<_>>();
        aborted.sort_unstable();
        for request in &aborted {
            state.requests.remove(request);
        }
        aborted
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

    #[cfg(test)]
    pub(crate) fn mark_queued(
        &self,
        owner: ActorRef,
        target: ActorRef,
        request: RequestId,
    ) -> Result<(), ReplyError> {
        self.mark_queued_with_deadline(owner, target, request, None)
    }

    pub(crate) fn mark_queued_with_deadline(
        &self,
        owner: ActorRef,
        target: ActorRef,
        request: RequestId,
        deadline: Option<ActiveRequestDeadline>,
    ) -> Result<(), ReplyError> {
        let mut state = self.state.lock();
        if state.cleaning.contains(&owner) || state.cleaning.contains(&target) {
            return Err(ReplyError::CancellationRequested);
        }
        let record = state.requests.get_mut(&request).ok_or(ReplyError::Stale)?;
        authorize_owner(record, owner)?;
        if record.target != target {
            return Err(identity_error(record.target, target));
        }
        match record.target_state {
            TargetState::Reserved => {
                record.target_state = TargetState::Queued;
                record.deadline = deadline;
                let received = state.received_requests.entry(target).or_default();
                *received = received.saturating_add(1);
                *state.cleanup_revision.entry(target).or_default() += 1;
                *state.cleanup_revision.entry(owner).or_default() += 1;
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
                if record
                    .updates
                    .iter()
                    .any(updates::UpdateRecord::fences_settlement)
                {
                    return Err(ReplyError::UpdatePending);
                }
                record.target_state = TargetState::Settling;
                record.progress = None;
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
        let target = record.target;
        let label = record.label.clone();
        let notification = presented.then_some(RequestCancellationNotification {
            target,
            request,
            label,
            reason,
            occurred_at_unix_ms: unix_time_ms(),
            sequence: ActorEventSequence(0),
            watermark: ActorEventSequence(0),
        });
        let notification = notification.map(|mut notification| {
            let sequence = next_event_sequence(&mut state, target);
            notification.sequence = sequence;
            notification.watermark = sequence;
            notification
        });
        Ok((CancelRequestOutcome::Requested, notification))
    }

    pub(crate) fn deadline_request(
        &self,
        owner: ActorRef,
        request: RequestId,
    ) -> (
        Option<RequestCancellationNotification>,
        Vec<WatchNotification>,
    ) {
        let mut state = self.state.lock();
        let Some(record) = state.requests.get_mut(&request) else {
            return (None, Vec::new());
        };
        // Reply acceptance and deadline expiry share this lock. An accepted
        // terminal transfer wins even while its result is still settling.
        if authorize_owner(record, owner).is_err()
            || is_owner_terminal(&record.owner_state)
            || matches!(
                record.target_state,
                TargetState::Settling | TargetState::Closed
            )
        {
            return (None, Vec::new());
        }
        record.owner_state = OwnerState::Unavailable(ResponseFailure::DeadlineExceeded);
        let notification = match record.target_state {
            TargetState::CancellationRequested { .. }
            | TargetState::AcknowledgingCancellation(_) => None,
            _ => {
                let presented = record.target_state == TargetState::Presented;
                record.target_state = TargetState::CancellationRequested {
                    presented,
                    reason: CancellationReason::DeadlineExpired,
                };
                presented.then_some(RequestCancellationNotification {
                    target: record.target,
                    request,
                    label: record.label.clone(),
                    reason: CancellationReason::DeadlineExpired,
                    occurred_at_unix_ms: unix_time_ms(),
                    sequence: ActorEventSequence(0),
                    watermark: ActorEventSequence(0),
                })
            }
        };
        let notification = notification.map(|mut notification| {
            let sequence = next_event_sequence(&mut state, notification.target);
            notification.sequence = sequence;
            notification.watermark = sequence;
            notification
        });
        // Target custody stays live until cancellation or exit closes it;
        // releasing the owner's wait must not require target cooperation.
        (notification, reevaluate_watches(&mut state))
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
        // Closing this request would allow the same conversation to accept its
        // next assignment while old input can still arrive. Cancellation may be
        // requested immediately; acknowledgement waits for presentation custody.
        if record
            .updates
            .iter()
            .any(updates::UpdateRecord::fences_settlement)
        {
            return Err(ReplyError::UpdatePending);
        }
        record.target_state = TargetState::AcknowledgingCancellation(reason);
        record.progress = None;
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

    #[cfg(test)]
    pub(crate) fn register_watch_labeled(
        &self,
        owner: ActorRef,
        label: String,
        dependencies: Vec<(RequestId, bool)>,
    ) -> Result<(WatchId, Vec<WatchNotification>), ReplyError> {
        self.register_watch_requirements(
            owner,
            label,
            dependencies
                .into_iter()
                .map(|(request, allow_failure)| {
                    (request, WatchRequirement::Response { allow_failure })
                })
                .collect(),
        )
    }

    pub(crate) fn register_watch_requirements(
        &self,
        owner: ActorRef,
        label: String,
        dependencies: Vec<(RequestId, WatchRequirement)>,
    ) -> Result<(WatchId, Vec<WatchNotification>), ReplyError> {
        self.register_watch_with_route(owner, label, dependencies, None)
    }

    pub(crate) fn register_watch_with_route(
        &self,
        owner: ActorRef,
        label: String,
        dependencies: Vec<(RequestId, WatchRequirement)>,
        route: Option<routes::WatchRoute>,
    ) -> Result<(WatchId, Vec<WatchNotification>), ReplyError> {
        let mut state = self.state.lock();
        if state.cleaning.contains(&owner) {
            return Err(ReplyError::CancellationRequested);
        }
        let mut touched = std::collections::HashSet::from([owner]);
        for (request, _) in &dependencies {
            let record = state.requests.get(request).ok_or(ReplyError::Stale)?;
            authorize_owner(record, owner)?;
            if state.cleaning.contains(&record.target) {
                return Err(ReplyError::CancellationRequested);
            }
            touched.insert(record.target);
        }
        for actor in touched {
            *state.cleanup_revision.entry(actor).or_default() += 1;
        }
        state.next_watch = state.next_watch.saturating_add(1);
        let id = WatchId(state.next_watch);
        state.watches.insert(
            id,
            WatchRecord {
                route,
                owner,
                label,
                dependencies: dependencies
                    .into_iter()
                    .map(|(request, requirement)| WatchDependency {
                        request,
                        requirement,
                    })
                    .collect(),
                state: WatchState::Pending,
                progress: HashMap::new(),
            },
        );
        let notifications = reevaluate_watches(&mut state);
        Ok((id, notifications))
    }

    pub(crate) fn observe_watch_progress(
        &self,
        owner: ActorRef,
        watch: WatchId,
        request: RequestId,
        after: u64,
    ) -> Result<(Option<ProgressSnapshot>, bool), ReplyError> {
        let state = self.state.lock();
        let record = state.watches.get(&watch).ok_or(ReplyError::Stale)?;
        if record.owner != owner {
            return Err(identity_error(record.owner, owner));
        }
        if record.state != WatchState::Ready {
            return Err(ReplyError::Stale);
        }
        match record
            .progress
            .get(&(request, after))
            .ok_or(ReplyError::Unauthorized)?
        {
            ProgressCapture::Update(snapshot) => Ok((Some(snapshot.clone()), false)),
            ProgressCapture::Closed => Ok((None, true)),
        }
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
        if record.state == WatchState::Pending
            || record
                .route
                .as_ref()
                .is_some_and(routes::WatchRoute::is_active)
        {
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
            .filter(|(_, record)| {
                record.owner == actor
                    && (record.state == WatchState::Pending
                        || record
                            .route
                            .as_ref()
                            .is_some_and(routes::WatchRoute::is_active))
            })
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
        for watch in state
            .watches
            .values_mut()
            .filter(|watch| watch.owner == actor)
        {
            if let Some(route) = &mut watch.route {
                route.retire();
            }
        }
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

fn unix_time_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |duration| {
            u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
        })
}

fn next_event_sequence(state: &mut RequestStateTable, actor: ActorRef) -> ActorEventSequence {
    let next_event = state.next_event_by_actor.entry(actor).or_default();
    *next_event = next_event.saturating_add(1);
    ActorEventSequence(*next_event)
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

fn authorize_progress_publication(
    record: &RequestRecord,
    target: ActorRef,
) -> Result<(), ReplyError> {
    authorize_target(record, target)?;
    if record.owner_state != OwnerState::Observing {
        return Err(ReplyError::AlreadySettled);
    }
    match record.target_state {
        TargetState::Presented
        | TargetState::CancellationRequested {
            presented: true, ..
        } => Ok(()),
        TargetState::Closed | TargetState::AcknowledgingCancellation(_) => {
            Err(ReplyError::AlreadySettled)
        }
        _ => Err(ReplyError::Stale),
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
    for record in state.requests.values_mut() {
        if record.owner_state != OwnerState::Observing || record.target_state == TargetState::Closed
        {
            record.progress = None;
        }
    }
    let mut notifications = Vec::new();
    for (watch_id, watch) in &mut state.watches {
        if watch.state != WatchState::Pending {
            continue;
        }
        for dependency in &watch.dependencies {
            let WatchRequirement::ProgressAfter(after) = dependency.requirement else {
                continue;
            };
            let key = (dependency.request, after);
            if watch.progress.contains_key(&key) {
                continue;
            }
            let Some(record) = state.requests.get(&dependency.request) else {
                continue;
            };
            if let Some(snapshot) = record
                .progress
                .as_ref()
                .filter(|snapshot| snapshot.revision > after)
            {
                watch
                    .progress
                    .insert(key, ProgressCapture::Update(snapshot.clone()));
            } else if record.owner_state != OwnerState::Observing
                || record.target_state == TargetState::Closed
            {
                watch.progress.insert(key, ProgressCapture::Closed);
            }
        }
        let transition = watch.dependencies.iter().find_map(|dependency| {
            let record = state.requests.get(&dependency.request)?;
            match &record.owner_state {
                OwnerState::Unavailable(failure)
                    if dependency.requirement
                        == (WatchRequirement::Response {
                            allow_failure: false,
                        }) =>
                {
                    Some(WatchTransition::Unavailable {
                        request: dependency.request,
                        failure: failure.clone(),
                    })
                }
                OwnerState::Abandoned
                    if dependency.requirement
                        == (WatchRequirement::Response {
                            allow_failure: false,
                        }) =>
                {
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
                    if let WatchRequirement::ProgressAfter(after) = dependency.requirement {
                        return watch.progress.contains_key(&(dependency.request, after));
                    }
                    state
                        .requests
                        .get(&dependency.request)
                        .is_some_and(|record| match record.owner_state {
                            OwnerState::Ready => true,
                            OwnerState::Unavailable(_) | OwnerState::Abandoned => {
                                dependency.requirement
                                    == (WatchRequirement::Response {
                                        allow_failure: true,
                                    })
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
            WatchTransition::Unavailable { request, failure } => {
                watch.progress.clear();
                WatchState::Unavailable {
                    request: *request,
                    failure: failure.clone(),
                }
            }
        };
        if let Some(route) = &mut watch.route {
            route.schedule(*watch_id);
            continue;
        }
        let current = match &transition {
            WatchTransition::Ready => WatchStateProjection::Ready,
            WatchTransition::Unavailable { request, failure } => {
                WatchStateProjection::Unavailable {
                    request: *request,
                    failure: failure.clone(),
                }
            }
        };
        let label = watch.label.clone();
        let owner = watch.owner;
        let watch = *watch_id;
        let occurred_at_unix_ms = unix_time_ms();
        notifications.push(WatchNotification {
            owner,
            watch,
            label,
            previous: WatchStateProjection::Pending,
            current,
            transition,
            occurred_at_unix_ms,
            sequence: ActorEventSequence(0),
            watermark: ActorEventSequence(0),
        });
    }
    for notification in &mut notifications {
        let sequence = next_event_sequence(state, notification.owner);
        notification.sequence = sequence;
        notification.watermark = sequence;
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
    fn received_counts_measure_admission_once_not_reservation_or_presentation() {
        let registry = RequestRegistry::default();
        let owner = actor(1);
        let target = actor(2);
        let request = registry.reserve(owner, target);
        assert_eq!(registry.received_counts(target), (0, 0));
        registry.mark_queued(owner, target, request).unwrap();
        assert_eq!(registry.received_counts(target), (1, 0));
        assert!(registry.mark_queued(owner, target, request).is_err());
        registry.present(target, request).unwrap();
        assert_eq!(registry.received_counts(target), (1, 0));
        assert_eq!(registry.received_counts(owner), (0, 0));
    }

    #[test]
    fn progress_publication_requires_exact_presented_target() {
        let registry = RequestRegistry::default();
        let owner = actor(1);
        let target = actor(2);
        let request = registry.reserve(owner, target);
        let authorize = |publisher| {
            let state = registry.state.lock();
            authorize_progress_publication(&state.requests[&request], publisher)
        };
        assert_eq!(authorize(target), Err(ReplyError::Stale));
        registry.mark_queued(owner, target, request).unwrap();
        registry.present(target, request).unwrap();
        assert_eq!(authorize(target), Ok(()));
        assert_eq!(authorize(owner), Err(ReplyError::Unauthorized));
        assert_eq!(
            authorize(ActorRef {
                id: target.id,
                incarnation: Incarnation(2)
            }),
            Err(ReplyError::WrongIncarnation)
        );
        registry.abandon_response(owner, request).unwrap();
        assert_eq!(authorize(target), Err(ReplyError::AlreadySettled));
    }

    #[test]
    fn progress_registration_racing_settlement_cannot_miss_closure() {
        for _ in 0..32 {
            let registry = std::sync::Arc::new(RequestRegistry::default());
            let owner = actor(1);
            let target = actor(2);
            let request = registry.reserve(owner, target);
            registry.mark_queued(owner, target, request).unwrap();
            registry.present(target, request).unwrap();
            let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
            let registrar = {
                let registry = registry.clone();
                let barrier = barrier.clone();
                std::thread::spawn(move || {
                    barrier.wait();
                    registry
                        .register_watch_requirements(
                            owner,
                            "racing".into(),
                            vec![(request, WatchRequirement::ProgressAfter(0))],
                        )
                        .unwrap()
                })
            };
            barrier.wait();
            registry.begin_reply(target, request).unwrap();
            let notifications = registry.finish_reply(request);
            let (watch, initial) = registrar.join().unwrap();
            assert_eq!(notifications.len() + initial.len(), 1);
            assert!(matches!(
                registry.observe_watch_progress(owner, watch, request, 0),
                Ok((None, true))
            ));
        }
    }

    #[test]
    fn progress_watches_close_on_abandonment_cancellation_and_retirement() {
        for terminal_path in 0..3 {
            let registry = RequestRegistry::default();
            let owner = actor(1);
            let target = actor(2);
            let request = registry.reserve(owner, target);
            registry.mark_queued(owner, target, request).unwrap();
            registry.present(target, request).unwrap();
            let (watch, _) = registry
                .register_watch_requirements(
                    owner,
                    "progress".into(),
                    vec![(request, WatchRequirement::ProgressAfter(0))],
                )
                .unwrap();
            match terminal_path {
                0 => {
                    registry.abandon_response(owner, request).unwrap();
                }
                1 => {
                    registry
                        .cancel_request(owner, request, CancellationReason::RequesterCancelled)
                        .unwrap();
                    assert!(matches!(
                        registry.observe_watch(owner, watch),
                        Ok(WatchObservation::Pending)
                    ));
                    registry
                        .begin_cancellation_acknowledgement(target, request)
                        .unwrap();
                    registry.finish_cancellation_acknowledgement(request);
                }
                _ => {
                    registry.actor_stopped(
                        target,
                        &ActorTerminal {
                            kind: ActorExitKind::Cancelled,
                            summary: "retired".into(),
                        },
                    );
                }
            }
            assert!(matches!(
                registry.observe_progress(owner, request),
                Ok((None, true))
            ));
            assert!(matches!(
                registry.observe_watch_progress(owner, watch, request, 0),
                Ok((None, true))
            ));
        }
    }

    #[test]
    fn progress_watch_closure_is_stable_and_authorized() {
        let registry = RequestRegistry::default();
        let owner = actor(1);
        let target = actor(2);
        let request = registry.reserve(owner, target);
        registry.mark_queued(owner, target, request).unwrap();
        registry.present(target, request).unwrap();
        let (watch, initial) = registry
            .register_watch_requirements(
                owner,
                "progress".into(),
                vec![(request, WatchRequirement::ProgressAfter(0))],
            )
            .unwrap();
        assert!(initial.is_empty());
        assert!(matches!(
            registry.observe_watch(owner, watch),
            Ok(WatchObservation::Pending)
        ));
        assert!(matches!(
            registry.register_watch_requirements(
                target,
                "wrong-owner".into(),
                vec![(request, WatchRequirement::ProgressAfter(0))]
            ),
            Err(ReplyError::Unauthorized)
        ));
        registry.begin_reply(target, request).unwrap();
        let notifications = registry.finish_reply(request);
        assert_eq!(notifications.len(), 1);
        assert_eq!(notifications[0].watch, watch);
        for _ in 0..2 {
            assert!(matches!(
                registry.observe_watch_progress(owner, watch, request, 0),
                Ok((None, true))
            ));
        }
        assert!(matches!(
            registry.observe_watch_progress(target, watch, request, 0),
            Err(ReplyError::Unauthorized)
        ));
        assert!(registry.finish_reply(request).is_empty());
        let (late, notifications) = registry
            .register_watch_requirements(
                owner,
                "late".into(),
                vec![(request, WatchRequirement::ProgressAfter(10))],
            )
            .unwrap();
        assert_eq!(notifications.len(), 1);
        assert!(matches!(
            registry.observe_watch_progress(owner, late, request, 10),
            Ok((None, true))
        ));
    }

    #[test]
    fn cleanup_rejects_new_work_even_if_it_already_finished() {
        let registry = std::sync::Arc::new(RequestRegistry::default());
        let owner = actor(1);
        let target = actor(2);
        let inspected = [(target, registry.cleanup_revision(target))];
        let request = registry.reserve(owner, target);
        registry.mark_queued(owner, target, request).unwrap();
        registry.present(target, request).unwrap();
        registry.begin_reply(target, request).unwrap();
        registry.finish_reply(request);
        assert!(matches!(
            registry.begin_cleanup(owner, &inspected),
            Err(CleanupAdmissionError::Stale)
        ));
        assert_eq!(
            registry.observe_response(owner, request),
            Ok(ResponseObservation::Ready)
        );
    }

    #[test]
    fn cleanup_allows_observed_completion_and_freezes_new_admission_until_drop() {
        let registry = std::sync::Arc::new(RequestRegistry::default());
        let owner = actor(1);
        let target = actor(2);
        let request = registry.reserve(owner, target);
        registry.mark_queued(owner, target, request).unwrap();
        registry.present(target, request).unwrap();
        let inspected = [(target, registry.cleanup_revision(target))];
        assert!(matches!(
            registry.begin_cleanup(owner, &inspected),
            Err(CleanupAdmissionError::Pending)
        ));
        registry.begin_reply(target, request).unwrap();
        registry.finish_reply(request);
        // Unrelated root work and inspection do not invalidate this decision.
        let outside = registry.reserve(owner, actor(3));
        registry.mark_queued(owner, actor(3), outside).unwrap();
        registry.observe_response(owner, request).unwrap();
        let guard = registry.begin_cleanup(owner, &inspected).ok().unwrap();
        let next = registry.reserve(owner, target);
        assert_eq!(
            registry.mark_queued(owner, target, next),
            Err(ReplyError::CancellationRequested)
        );
        assert!(matches!(
            registry.register_watch(owner, vec![request]),
            Err(ReplyError::CancellationRequested)
        ));
        let outbound = registry.reserve(target, actor(3));
        assert_eq!(
            registry.mark_queued(target, actor(3), outbound),
            Err(ReplyError::CancellationRequested)
        );
        drop(guard);
        registry.mark_queued(owner, target, next).unwrap();
    }

    #[test]
    fn cleanup_tracks_new_watches_and_member_owned_outbound_work() {
        let registry = std::sync::Arc::new(RequestRegistry::default());
        let owner = actor(1);
        let member = actor(2);
        let outside = actor(3);
        let request = registry.reserve(member, outside);
        registry.mark_queued(member, outside, request).unwrap();
        registry.present(outside, request).unwrap();
        let inspected = [(member, registry.cleanup_revision(member))];
        assert!(matches!(
            registry.begin_cleanup(owner, &inspected),
            Err(CleanupAdmissionError::Pending)
        ));
        registry.begin_reply(outside, request).unwrap();
        registry.finish_reply(request);
        let (_, _) = registry.register_watch(member, vec![request]).unwrap();
        assert!(matches!(
            registry.begin_cleanup(owner, &inspected),
            Err(CleanupAdmissionError::Stale)
        ));
        let inspected = [(member, registry.cleanup_revision(member))];
        let _guard = registry.begin_cleanup(owner, &inspected).ok().unwrap();
        let released =
            registry.cleanup_campaign_metadata(member, &std::collections::HashSet::from([member]));
        assert_eq!(released.forgotten_responses, vec![request]);
        assert_eq!(released.forgotten_watches.len(), 1);
        assert!(registry.active_for_target(outside).is_empty());
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
        assert_eq!(notifications[0].label, "watch");
        assert_eq!(notifications[0].previous, WatchStateProjection::Pending);
        assert_eq!(notifications[0].current, WatchStateProjection::Ready);
        assert_eq!(notifications[0].sequence, ActorEventSequence(1));
        assert_eq!(notifications[0].watermark, ActorEventSequence(1));
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
    fn workbench_boundary_aborts_only_unsubmitted_reservations() {
        let registry = RequestRegistry::default();
        let owner = actor(1);
        let other_owner = actor(2);
        let target = actor(3);
        let leaked = registry.reserve(owner, target);
        let committed = registry.reserve(owner, target);
        let unrelated = registry.reserve(other_owner, target);
        registry.mark_queued(owner, target, committed).unwrap();

        assert_eq!(registry.abort_unsubmitted(owner), vec![leaked]);
        assert_eq!(
            registry.observe_response(owner, leaked),
            Err(ReplyError::Stale)
        );
        assert_eq!(
            registry.observe_response(owner, committed),
            Ok(ResponseObservation::Pending)
        );
        assert_eq!(registry.abort_unsubmitted(other_owner), vec![unrelated]);
        assert!(registry.abort_unsubmitted(owner).is_empty());
    }

    #[tokio::test]
    async fn status_preserves_authored_deadline_units_and_hides_terminal_deadlines() {
        let registry = RequestRegistry::default();
        let owner = actor(1);
        let target = actor(2);
        let request = registry.reserve(owner, target);
        let deadline = RequestDeadline::checked(600, DeadlineUnit::Seconds).expect("deadline");
        registry
            .mark_queued_with_deadline(
                owner,
                target,
                request,
                Some(ActiveRequestDeadline::start(deadline).expect("active deadline")),
            )
            .unwrap();

        let pending = registry.status_for(owner);
        assert_eq!(pending.deadlines.len(), 1);
        assert!(pending.deadlines[0].1.contains("after=600s"));
        assert!(pending.deadlines[0].1.contains("remaining="));
        assert!(pending.deadlines[0].1.contains("due_unix_ms="));

        registry.mark_target_unavailable(owner, request);
        assert!(registry.status_for(owner).deadlines.is_empty());
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
    fn cancellation_notifications_carry_target_local_sequence_and_label() {
        let registry = RequestRegistry::default();
        let first_owner = actor(1);
        let second_owner = actor(2);
        let first_target = actor(3);
        let second_target = actor(4);
        let first = registry.reserve_labeled(first_owner, first_target, "first".into());
        let second = registry.reserve_labeled(second_owner, second_target, "second".into());
        registry
            .mark_queued(first_owner, first_target, first)
            .unwrap();
        registry
            .mark_queued(second_owner, second_target, second)
            .unwrap();
        registry.present(first_target, first).unwrap();
        registry.present(second_target, second).unwrap();

        let first_notice = registry
            .cancel_request(first_owner, first, CancellationReason::RequesterCancelled)
            .unwrap()
            .1
            .unwrap();
        let second_notice = registry
            .cancel_request(second_owner, second, CancellationReason::RequesterCancelled)
            .unwrap()
            .1
            .unwrap();

        assert_eq!(first_notice.label, "first");
        assert_eq!(second_notice.label, "second");
        assert_eq!(first_notice.sequence, ActorEventSequence(1));
        assert_eq!(second_notice.sequence, ActorEventSequence(1));
        assert!(first_notice.occurred_at_unix_ms > 0);
    }

    #[test]
    fn response_deadline_wakes_owner_before_target_acknowledgement() {
        let registry = RequestRegistry::default();
        let owner = actor(1);
        let target = actor(2);
        let request = registry.reserve(owner, target);
        registry.mark_queued(owner, target, request).unwrap();
        let (watch, _) = registry.register_watch(owner, vec![request]).unwrap();

        let (cancellation, notifications) = registry.deadline_request(owner, request);
        assert_eq!(cancellation, None);
        assert_eq!(notifications.len(), 1);
        assert_eq!(notifications[0].watch, watch);
        let status = registry.status_for(owner);
        assert_eq!(
            status.unavailable_responses[0].2,
            ResponseFailure::DeadlineExceeded
        );
        assert_eq!(
            status.unavailable_watches[0].2,
            ResponseFailure::DeadlineExceeded
        );
        assert_eq!(
            registry.observe_response(owner, request),
            Ok(ResponseObservation::Unavailable(
                ResponseFailure::DeadlineExceeded
            ))
        );
        registry.present(target, request).unwrap();
        registry
            .begin_cancellation_acknowledgement(target, request)
            .unwrap();
        let notifications = registry.finish_cancellation_acknowledgement(request);
        assert!(notifications.is_empty());
        assert_eq!(
            registry.observe_response(owner, request),
            Ok(ResponseObservation::Unavailable(
                ResponseFailure::DeadlineExceeded
            ))
        );
        assert_eq!(
            registry.deadline_request(owner, request),
            (None, Vec::new())
        );
        assert_eq!(
            registry.cancel_request(owner, request, CancellationReason::RequesterCancelled),
            Ok((CancelRequestOutcome::AlreadyTerminal, None))
        );
    }

    #[test]
    fn deadline_rejects_late_reply_and_notifies_a_later_watch() {
        let registry = RequestRegistry::default();
        let owner = actor(1);
        let target = actor(2);
        let request = registry.reserve(owner, target);
        registry.mark_queued(owner, target, request).unwrap();
        registry.present(target, request).unwrap();
        let (cancellation, notifications) = registry.deadline_request(owner, request);
        assert_eq!(cancellation.unwrap().target, target);
        assert!(notifications.is_empty());
        assert_eq!(
            registry.begin_reply(target, request),
            Err(ReplyError::CancellationRequested)
        );
        let (watch, notifications) = registry.register_watch(owner, vec![request]).unwrap();
        assert_eq!(notifications.len(), 1);
        assert_eq!(notifications[0].watch, watch);
        assert_eq!(
            registry.deadline_request(owner, request),
            (None, Vec::new())
        );
        assert_eq!(registry.active_for_target(target).len(), 1);
        assert_eq!(
            registry.observe_reply(target, request),
            Ok(ReplyObservation::CancellationRequested(
                CancellationReason::DeadlineExpired
            ))
        );
    }

    #[test]
    fn accepted_reply_wins_deadline_even_when_settlement_fails() {
        for settlement_fails in [false, true] {
            let registry = RequestRegistry::default();
            let owner = actor(1);
            let target = actor(2);
            let request = registry.reserve(owner, target);
            registry.mark_queued(owner, target, request).unwrap();
            registry.present(target, request).unwrap();
            let (_, initial) = registry.register_watch(owner, vec![request]).unwrap();
            assert!(initial.is_empty());
            registry.begin_reply(target, request).unwrap();
            assert_eq!(
                registry.deadline_request(owner, request),
                (None, Vec::new())
            );
            let notifications = if settlement_fails {
                registry.fail_reply_settlement(request, "lost result")
            } else {
                registry.finish_reply(request)
            };
            assert_eq!(notifications.len(), 1);
            let expected = if settlement_fails {
                ResponseObservation::Unavailable(ResponseFailure::SettlementFailed(
                    "lost result".into(),
                ))
            } else {
                ResponseObservation::Ready
            };
            assert_eq!(registry.observe_response(owner, request), Ok(expected));
            assert_eq!(
                registry.deadline_request(owner, request),
                (None, Vec::new())
            );
        }
    }

    #[test]
    fn deadline_bounds_wait_during_requested_cancellation() {
        let registry = RequestRegistry::default();
        let owner = actor(1);
        let target = actor(2);
        let request = registry.reserve(owner, target);
        registry.mark_queued(owner, target, request).unwrap();
        registry.present(target, request).unwrap();
        registry
            .cancel_request(owner, request, CancellationReason::RequesterCancelled)
            .unwrap();
        registry
            .begin_cancellation_acknowledgement(target, request)
            .unwrap();
        let (_, initial) = registry.register_watch(owner, vec![request]).unwrap();
        assert!(initial.is_empty());
        let (cancellation, notifications) = registry.deadline_request(owner, request);
        assert!(cancellation.is_none());
        assert_eq!(notifications.len(), 1);
        assert!(registry
            .finish_cancellation_acknowledgement(request)
            .is_empty());
        assert_eq!(
            registry.observe_response(owner, request),
            Ok(ResponseObservation::Unavailable(
                ResponseFailure::DeadlineExceeded
            ))
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

    #[test]
    fn campaign_cleanup_forgets_only_terminal_scoped_metadata_and_reports_blockers() {
        let registry = RequestRegistry::default();
        let owner = actor(1);
        let ready_target = actor(2);
        let pending_target = actor(3);
        let outside_target = actor(4);

        let ready = registry.reserve_labeled(owner, ready_target, "ready".into());
        registry.mark_queued(owner, ready_target, ready).unwrap();
        registry.present(ready_target, ready).unwrap();
        let (ready_watch, _) = registry.register_watch(owner, vec![ready]).unwrap();
        registry.begin_reply(ready_target, ready).unwrap();
        registry.finish_reply(ready);

        let pending = registry.reserve_labeled(owner, pending_target, "pending".into());
        registry
            .mark_queued(owner, pending_target, pending)
            .unwrap();
        let outside = registry.reserve_labeled(owner, outside_target, "outside".into());
        registry
            .mark_queued(owner, outside_target, outside)
            .unwrap();

        let owners = [owner].into_iter().collect();
        let targets = [ready_target, pending_target].into_iter().collect();
        assert_eq!(
            registry.campaign_cleanup_blockers(&owners, &targets),
            (vec![pending], Vec::new())
        );
        let outcome = registry.cleanup_campaign_metadata(owner, &targets);
        assert_eq!(outcome.forgotten_responses, vec![ready]);
        assert_eq!(outcome.forgotten_watches, vec![ready_watch]);
        assert_eq!(outcome.pending_responses, vec![pending]);
        assert!(outcome.pending_watches.is_empty());
        assert_eq!(
            registry.observe_response(owner, outside),
            Ok(ResponseObservation::Pending),
            "another campaign remains untouched"
        );
    }
}
