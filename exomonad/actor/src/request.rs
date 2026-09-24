pub(crate) mod routes;
pub(crate) mod sources;
mod updates;
pub use updates::{
    LateUpdateEvidence, RequestUpdateCorrelation, RequestUpdateDelivery, RequestUpdateId,
    RequestUpdatePresentation, RequestUpdateReconciler, RequestUpdateState,
    UpdateReconciliationError,
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
    Released,
    TargetUnavailable,
    TargetFailed(String),
    TargetCancelled(String),
    RequesterStopped,
    Abandoned,
    Cancelled,
    DeadlineExceeded,
    SettlementFailed(String),
}

/// The producing actor's own progress as of one poll, carried on a
/// still-pending response or watch observation. Reuses
/// `ActorRuntimeObservation`, the same evidence `AgentInspection` already
/// reports for `observeAgent`; this is not a second tracker, only another
/// projection of it. A caller holding this has nothing a re-poll would add:
/// `watched` already says whether a registered watch will wake it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingProgress {
    pub actor_terminal: Option<ActorTerminal>,
    pub provider_turn: Option<exomonad_model::ProviderTurnObservation>,
    /// The producing actor's most recently observed provider activity
    /// (its latest recorded usage sample), if any has been observed yet.
    pub last_activity_unix_ms: Option<u64>,
    /// The current revision of this request's `Progress` channel, if the
    /// target has published to it at least once.
    pub progress_revision: Option<u64>,
    /// A registered watch depends on this exact request (or, for a watch
    /// observation, this observation itself is that registration) and will
    /// wake its owner when the dependency settles.
    pub watched: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResponseObservation {
    Pending(PendingProgress),
    CancellationPending(CancellationReason),
    Ready,
    Unavailable(ResponseFailure),
    /// The target is admitted for this request but has not yet started a
    /// provider turn for it (still queued, or presented but idle). Produced
    /// by refining a bare `Pending` against the target's runtime observation;
    /// see `ResidentActor::starting_observation`.
    Starting(String),
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
    RouteFailed {
        detail: String,
    },
    Unavailable {
        request: RequestId,
        failure: ResponseFailure,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum WatchStateProjection {
    Pending,
    Ready,
    RouteRunning,
    RouteFailed {
        detail: String,
    },
    Unavailable {
        request: RequestId,
        failure: ResponseFailure,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WatchObservation {
    Pending(PendingProgress),
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
pub enum SettlementTransition {
    Ready,
    Unavailable(ResponseFailure),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SettlementNotification {
    pub owner: ActorRef,
    pub request: RequestId,
    pub label: String,
    pub transition: SettlementTransition,
    /// A bounded, best-effort text rendering of the reply value, set only
    /// for a [`SettlementTransition::Ready`] whose value could be read
    /// cheaply (no Haskell compiled, no thunk forced) within its budget.
    /// `None` for `Unavailable`, and `None` for `Ready` when the value
    /// could not be read this way -- either way the owner falls back to
    /// `pollResponse` for the full value.
    pub reply_preview: Option<String>,
    /// The target's own `exomonad/<path>` actor-lineage path, when it has
    /// one (an unforked target has none).
    pub target_path: Option<String>,
    /// The exact commit the target was seeded from, when it was launched
    /// from a fork workspace.
    pub target_revision: Option<String>,
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
    sources: Vec<sources::RequestSourceConnection>,
    updates: Vec<updates::UpdateRecord>,
    owner: ActorRef,
    target: ActorRef,
    label: String,
    target_state: TargetState,
    owner_state: OwnerState,
    deadline: Option<ActiveRequestDeadline>,
    progress: Option<ProgressSnapshot>,
    notify_owner: bool,
    settlement_notified: bool,
    registered_at_unix_ms: u64,
    /// A bounded, best-effort text rendering of the reply value, attached by
    /// [`RequestRegistry::finish_reply`] and consumed the one time
    /// [`reevaluate_watches`] mints this request's `Ready` settlement
    /// notice. Never set for an `Unavailable` settlement.
    reply_preview: Option<String>,
    /// The target's own `exomonad/<path>` actor-lineage path, attached by
    /// [`RequestRegistry::record_target_identity`]. Only the target can read
    /// its own descriptor, so this rides in ahead of [`finish_reply`] rather
    /// than being looked up later from the request alone.
    target_path: Option<String>,
    /// The exact commit the target's checkout was seeded from, when it was
    /// launched from a fork workspace. `None` for a target with no fork
    /// workspace (an unforked `startActor`/`startAgent`, or the root actor).
    target_revision: Option<String>,
}

/// Snapshots share ownership, not a consumption cursor. Replacing the latest
/// publication drops only the registry's reference; an observer can retain it.
#[derive(Clone, Debug)]
pub(crate) struct ProgressSnapshot {
    pub revision: u64,
    pub value: std::sync::Arc<tidepool_runtime::session::RootCustody>,
    /// The resident session whose machine `value`'s handle actually lives
    /// on -- the publishing actor's own session at the moment it called
    /// `publish_progress`. An observer on a different session must export
    /// this root (borrowed, not consumed -- see
    /// `tidepool_runtime::session::ResidentSession::export_shared`) and
    /// import its own independent custody before it can read the value at
    /// all; same-session observation needs neither.
    pub session: tidepool_repr::SessionId,
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
    group_count: usize,
    state: WatchState,
    progress: HashMap<(RequestId, u64), ProgressCapture>,
    /// When the owner last observed this watch (via `observe_watch`) already
    /// settled Ready or Unavailable. Lets delivery acknowledge a queued
    /// `WatchChanged` notice without prompting when the owner polled the
    /// same settled state before the notice was delivered.
    observed_ready_at: Option<u64>,
    /// When this watch was registered, unix ms.
    registered_at_unix_ms: u64,
    /// When this watch last left `Pending`, unix ms. `None` while still
    /// pending.
    transitioned_at_unix_ms: Option<u64>,
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
    group: usize,
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
    settlement_notifications: Vec<SettlementNotification>,
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

/// One line's worth of native watch state, for the `status` tool's `watches`
/// view: everything a model is waiting on, without compiling a `pollWatch`
/// cell just to see whether anything settled.
pub(crate) struct WatchViewEntry {
    pub id: WatchId,
    pub label: String,
    /// `Debug`-rendered `WatchState` (`Pending`, `Ready`, or
    /// `Unavailable { .. }`); `WatchState` itself stays private to this
    /// module.
    pub state: String,
    pub registered_at_unix_ms: u64,
    /// `None` while still `Pending`.
    pub transitioned_at_unix_ms: Option<u64>,
}

pub(crate) struct PendingResponseAge {
    pub id: RequestId,
    pub label: String,
    pub registered_at_unix_ms: u64,
}

pub(crate) struct WatchesOverview {
    pub watches: Vec<WatchViewEntry>,
    pub pending_responses: Vec<PendingResponseAge>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct CleanupMetadataOutcome {
    pub forgotten_responses: Vec<RequestId>,
    pub forgotten_watches: Vec<WatchId>,
    pub pending_responses: Vec<RequestId>,
    pub pending_watches: Vec<WatchId>,
    pub watch_notifications: Vec<WatchNotification>,
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

/// Bound on how long a `RequestCleanupGuard` may hold its targets marked
/// `cleaning` before the guard's own watchdog force-releases them. Reuses
/// `local_actor::SHUTDOWN_BUDGET`: the guard is normally held across exactly
/// the retirements `local_actor`'s own shutdown path already bounds by that
/// budget (see `resident_actor::ResidentActorBoundary::CleanupExecute`), so
/// a stuck retirement is already supposed to give up within it. This is a
/// last-resort safety valve, not a routine operating point: a plan that
/// legitimately retires several actors, each close to the full budget,
/// sequentially, could still exceed it and trip the watchdog early. Widen
/// this constant (not a second timing system) if that turns out to matter.
const CLEANUP_GUARD_BUDGET: std::time::Duration = crate::local_actor::SHUTDOWN_BUDGET;

pub(crate) struct RequestCleanupGuard {
    registry: std::sync::Arc<RequestRegistry>,
    targets: std::collections::HashSet<ActorRef>,
    /// Set by whichever of {this guard's `Drop`, its watchdog task} settles
    /// the retained `cleaning` marks first; the other observes it already
    /// set and does nothing. Exactly one of the two ever calls
    /// `release_cleaning`, so a watchdog expiry followed by an eventual
    /// (late) `Drop` of the still-held guard is a no-op, not a double
    /// release of a possibly-since-reclaimed target.
    released: std::sync::Arc<std::sync::atomic::AtomicBool>,
    watchdog: Option<tokio::task::JoinHandle<()>>,
}

impl RequestCleanupGuard {
    fn release_cleaning(registry: &RequestRegistry, targets: &std::collections::HashSet<ActorRef>) {
        let mut state = registry.state.lock();
        for actor in targets {
            state.cleaning.remove(actor);
        }
    }
}

impl Drop for RequestCleanupGuard {
    fn drop(&mut self) {
        use std::sync::atomic::Ordering;
        if self
            .released
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
        {
            Self::release_cleaning(&self.registry, &self.targets);
        }
        // The watchdog is only needed while this guard is alive; an already
        // fired watchdog is a harmless no-op abort.
        if let Some(watchdog) = self.watchdog.take() {
            watchdog.abort();
        }
    }
}

impl RequestRegistry {
    /// Replacement moves request/watch ownership, not the immutable actor a
    /// request was originally submitted to. Existing handles keep their IDs.
    pub(crate) fn transfer_owner(&self, predecessor: ActorRef, successor: &crate::LocalActorRef) {
        let mut state = self.state.lock();
        for record in state
            .requests
            .values_mut()
            .filter(|record| record.owner == predecessor)
        {
            record.owner = successor.identity();
        }
        for (id, watch) in state
            .watches
            .iter_mut()
            .filter(|(_, watch)| watch.owner == predecessor)
        {
            watch.owner = successor.identity();
            if let Some(route) = &mut watch.route {
                route.transfer_owner(successor.clone(), *id);
            }
        }
    }
    pub(crate) fn publish_progress(
        &self,
        target: ActorRef,
        request: RequestId,
        value: tidepool_runtime::session::RootCustody,
        session: tidepool_repr::SessionId,
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
            session,
        });
        record.publish_source_progress();
        Ok((revision, reevaluate_watches(&mut state)))
    }

    pub(crate) fn observe_progress(
        &self,
        _owner: ActorRef,
        request: RequestId,
    ) -> Result<(Option<ProgressSnapshot>, bool), ReplyError> {
        let state = self.state.lock();
        let record = state.requests.get(&request).ok_or(ReplyError::Stale)?;
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
        drop(state);
        let released = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        // Only spawn the watchdog inside a running Tokio executor: unit
        // tests exercise this admission logic synchronously, with no
        // runtime, and are expected to release purely through `Drop`.
        let watchdog = tokio::runtime::Handle::try_current().ok().map(|handle| {
            let registry = std::sync::Arc::clone(self);
            let watchdog_targets = targets.clone();
            let released = std::sync::Arc::clone(&released);
            handle.spawn(async move {
                tokio::time::sleep(CLEANUP_GUARD_BUDGET).await;
                use std::sync::atomic::Ordering;
                if released
                    .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
                    .is_ok()
                {
                    tracing::warn!(
                        actors = ?watchdog_targets,
                        budget = ?CLEANUP_GUARD_BUDGET,
                        "cleanup guard watchdog force-released cleaning marks after budget expiry"
                    );
                    RequestCleanupGuard::release_cleaning(&registry, &watchdog_targets);
                }
            })
        });
        Ok(RequestCleanupGuard {
            registry: std::sync::Arc::clone(self),
            targets,
            released,
            watchdog,
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
            let Some(record) = state.requests.get(&request) else {
                continue;
            };
            if record.owner_state == OwnerState::Observing
                || record.target_state != TargetState::Closed
            {
                outcome.pending_responses.push(request);
            } else {
                outcome
                    .watch_notifications
                    .extend(release_request_record(&mut state, request));
                outcome.forgotten_responses.push(request);
            }
        }
        outcome.forgotten_responses.sort_unstable();
        outcome.forgotten_watches.sort_unstable();
        outcome.pending_responses.sort_unstable();
        outcome.pending_watches.sort_unstable();
        outcome
    }

    /// Whether the registry still holds this watch.
    ///
    /// Forgetting a watch (above) releases the state a notification describes,
    /// so a notification computed before the release must not be delivered
    /// afterwards: its owner would take the transition as an invitation to
    /// poll an id the registry no longer knows, and be answered
    /// `WatchUnavailable (WatchRejected ReplyStale)`. Publishers ask here
    /// rather than each keeping their own forgotten-id set.
    pub(crate) fn retains_watch(&self, owner: ActorRef, watch: WatchId) -> bool {
        self.state
            .lock()
            .watches
            .get(&watch)
            .is_some_and(|record| record.owner == owner)
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

    /// Everything `owner` is waiting on: every retained watch (with its
    /// registration and last-transition times) and every response it has
    /// not yet observed settle. Backs the `status` tool's `watches` view so
    /// a model can see what it is waiting on without compiling a
    /// `pollWatch`/`pollResponse` cell.
    pub(crate) fn watches_overview(&self, owner: ActorRef) -> WatchesOverview {
        let state = self.state.lock();
        let mut watches = state
            .watches
            .iter()
            .filter(|(_, record)| record.owner == owner)
            .map(|(id, record)| WatchViewEntry {
                id: *id,
                label: record.label.clone(),
                state: format!("{:?}", record.state),
                registered_at_unix_ms: record.registered_at_unix_ms,
                transitioned_at_unix_ms: record.transitioned_at_unix_ms,
            })
            .collect::<Vec<_>>();
        watches.sort_unstable_by_key(|entry| entry.id);
        let mut pending_responses = state
            .requests
            .iter()
            .filter(|(_, record)| {
                record.owner == owner && matches!(record.owner_state, OwnerState::Observing)
            })
            .map(|(id, record)| PendingResponseAge {
                id: *id,
                label: record.label.clone(),
                registered_at_unix_ms: record.registered_at_unix_ms,
            })
            .collect::<Vec<_>>();
        pending_responses.sort_unstable_by_key(|entry| entry.id);
        WatchesOverview {
            watches,
            pending_responses,
        }
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

    #[cfg(test)]
    pub(crate) fn reserve_labeled(
        &self,
        owner: ActorRef,
        target: ActorRef,
        label: String,
    ) -> RequestId {
        self.reserve_labeled_with_reporting(owner, target, label, true)
    }

    pub(crate) fn reserve_labeled_with_reporting(
        &self,
        owner: ActorRef,
        target: ActorRef,
        label: String,
        notify_owner: bool,
    ) -> RequestId {
        let mut state = self.state.lock();
        // `next_request` is a per-process u64 counter; wraparound needs
        // 2^64 reservations in one process lifetime and is not reachable.
        #[allow(clippy::expect_used)]
        {
            state.next_request = state
                .next_request
                .checked_add(1)
                .expect("request identity exhausted");
        }
        let id = RequestId(state.next_request);
        state.requests.insert(
            id,
            RequestRecord {
                sources: Vec::new(),
                updates: Vec::new(),
                owner,
                target,
                label,
                target_state: TargetState::Reserved,
                owner_state: OwnerState::Observing,
                deadline: None,
                progress: None,
                notify_owner,
                settlement_notified: false,
                registered_at_unix_ms: unix_time_ms(),
                reply_preview: None,
                target_path: None,
                target_revision: None,
            },
        );
        id
    }

    pub(crate) fn take_settlement_notifications(&self) -> Vec<SettlementNotification> {
        std::mem::take(&mut self.state.lock().settlement_notifications)
    }

    /// Remove request identities which never crossed the admission commit.
    ///
    /// The Haskell facade constructs a live request payload in two private
    /// effect steps because the payload itself contains the runtime-minted
    /// request id. The surrounding workbench invocation is the transaction:
    /// anything still `Reserved` when that invocation settles was never
    /// published and must not leak into observable request state.
    pub(crate) fn abort_unsubmitted(
        &self,
        owner: ActorRef,
    ) -> (Vec<RequestId>, Vec<WatchNotification>) {
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
        let mut notifications = Vec::new();
        for request in &aborted {
            notifications.extend(release_request_record(&mut state, *request));
        }
        (aborted, notifications)
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
                record.publish_source_closure();
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

    /// The target's own actor path and seeded source revision, attached by
    /// the target about itself ahead of [`Self::finish_reply`] — only it can
    /// read its own descriptor and prepared workspace. Rides along only as
    /// far as this request's settlement notice, same as `reply_preview`.
    pub(crate) fn record_target_identity(
        &self,
        request: RequestId,
        target_path: Option<String>,
        target_revision: Option<String>,
    ) {
        let mut state = self.state.lock();
        if let Some(record) = state.requests.get_mut(&request) {
            record.target_path = target_path;
            record.target_revision = target_revision;
        }
    }

    /// `reply_preview` is a bounded, best-effort rendering of the value the
    /// target just replied with (`None` when it could not be read cheaply).
    /// It rides along only as far as this request's own `Ready` settlement
    /// notice; nothing else on the request depends on it.
    pub(crate) fn finish_reply(
        &self,
        request: RequestId,
        reply_preview: Option<String>,
    ) -> Vec<WatchNotification> {
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
        record.reply_preview = reply_preview;
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

    /// The request's target actor, for callers that need to correlate a
    /// request with actor-level observation (e.g. runtime/provider status)
    /// outside this registry. Does not check ownership: it is a lookup, not
    /// an authorized observation.
    pub(crate) fn target_for(&self, request: RequestId) -> Option<ActorRef> {
        let state = self.state.lock();
        state.requests.get(&request).map(|record| record.target)
    }

    /// The target actor of one still-pending watch's first unsettled
    /// dependency, for callers correlating a pending watch observation with
    /// that actor's runtime progress. `None` for a watch that is not
    /// currently pending, or has no unsettled dependency (should not arise
    /// for a pending watch; see `first_pending_dependency`).
    pub(crate) fn watch_pending_target(&self, watch: WatchId) -> Option<ActorRef> {
        let state = self.state.lock();
        let record = state.watches.get(&watch)?;
        let request = first_pending_dependency(&state, record)?;
        state.requests.get(&request).map(|record| record.target)
    }

    pub(crate) fn observe_response(
        &self,
        _owner: ActorRef,
        request: RequestId,
    ) -> Result<ResponseObservation, ReplyError> {
        let state = self.state.lock();
        let record = state.requests.get(&request).ok_or(ReplyError::Stale)?;
        Ok(match &record.owner_state {
            OwnerState::Ready => ResponseObservation::Ready,
            OwnerState::Unavailable(failure) => ResponseObservation::Unavailable(failure.clone()),
            OwnerState::Abandoned => ResponseObservation::Unavailable(ResponseFailure::Abandoned),
            OwnerState::Observing => match record.target_state {
                TargetState::CancellationRequested { reason, .. } => {
                    ResponseObservation::CancellationPending(reason)
                }
                _ => ResponseObservation::Pending(base_pending_progress(&state, request)),
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
        // Target ownership stays live until cancellation or exit closes it;
        // releasing the owner's wait must not require target cooperation.
        (notification, reevaluate_watches(&mut state))
    }

    pub(crate) fn observe_reply(
        &self,
        _target: ActorRef,
        request: RequestId,
    ) -> Result<ReplyObservation, ReplyError> {
        let state = self.state.lock();
        let record = state.requests.get(&request).ok_or(ReplyError::Stale)?;
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
        // requested immediately; acknowledgement waits for presentation ownership.
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
    ) -> Result<(ForgetResponseOutcome, Vec<WatchNotification>), ReplyError> {
        let mut state = self.state.lock();
        let record = state.requests.get(&request).ok_or(ReplyError::Stale)?;
        authorize_owner(record, owner)?;
        if record.owner_state == OwnerState::Observing {
            return Ok((ForgetResponseOutcome::StillPending, Vec::new()));
        }
        if record.target_state != TargetState::Closed {
            return Ok((ForgetResponseOutcome::TargetStillActive, Vec::new()));
        }
        let notifications = release_request_record(&mut state, request);
        Ok((ForgetResponseOutcome::Forgotten, notifications))
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

    #[cfg(test)]
    pub(crate) fn register_watch_requirements(
        &self,
        owner: ActorRef,
        label: String,
        dependencies: Vec<(RequestId, WatchRequirement)>,
    ) -> Result<(WatchId, Vec<WatchNotification>), ReplyError> {
        self.register_watch_requirement_groups(
            owner,
            label,
            dependencies
                .into_iter()
                .map(|dependency| vec![dependency])
                .collect(),
        )
    }

    pub(crate) fn register_watch_requirement_groups(
        &self,
        owner: ActorRef,
        label: String,
        groups: Vec<Vec<(RequestId, WatchRequirement)>>,
    ) -> Result<(WatchId, Vec<WatchNotification>), ReplyError> {
        self.register_watch_groups_with_route(owner, label, groups, None)
    }

    pub(crate) fn register_watch_groups_with_route(
        &self,
        owner: ActorRef,
        label: String,
        groups: Vec<Vec<(RequestId, WatchRequirement)>>,
        route: Option<routes::WatchRoute>,
    ) -> Result<(WatchId, Vec<WatchNotification>), ReplyError> {
        let group_count = groups.len();
        let dependencies = groups
            .iter()
            .enumerate()
            .flat_map(|(group, dependencies)| {
                dependencies
                    .iter()
                    .copied()
                    .map(move |(request, requirement)| WatchDependency {
                        request,
                        requirement,
                        group,
                    })
            })
            .collect::<Vec<_>>();
        let mut state = self.state.lock();
        if state.cleaning.contains(&owner) {
            return Err(ReplyError::CancellationRequested);
        }
        let mut touched = std::collections::HashSet::from([owner]);
        for dependency in &dependencies {
            let record = state
                .requests
                .get(&dependency.request)
                .ok_or(ReplyError::Stale)?;
            if state.cleaning.contains(&record.target) {
                return Err(ReplyError::CancellationRequested);
            }
            touched.insert(record.target);
        }
        // An owner's response watch or route owns that owner's settlement
        // wake. A foreign listener and a progress-only watch do not.
        // A notice already emitted before registration cannot be retracted.
        for dependency in &dependencies {
            if owner == state.requests[&dependency.request].owner
                && matches!(dependency.requirement, WatchRequirement::Response { .. })
            {
                state
                    .requests
                    .get_mut(&dependency.request)
                    .ok_or(ReplyError::Stale)?
                    .notify_owner = false;
            }
        }
        for actor in touched {
            *state.cleanup_revision.entry(actor).or_default() += 1;
        }
        // `next_watch` is a per-process u64 counter; wraparound needs
        // 2^64 registrations in one process lifetime and is not reachable.
        #[allow(clippy::expect_used)]
        {
            state.next_watch = state
                .next_watch
                .checked_add(1)
                .expect("watch identity exhausted");
        }
        let id = WatchId(state.next_watch);
        state.watches.insert(
            id,
            WatchRecord {
                route,
                owner,
                label,
                dependencies,
                group_count,
                state: WatchState::Pending,
                progress: HashMap::new(),
                observed_ready_at: None,
                registered_at_unix_ms: unix_time_ms(),
                transitioned_at_unix_ms: None,
            },
        );
        let notifications = reevaluate_watches(&mut state);
        Ok((id, notifications))
    }

    pub(crate) fn observe_watch_progress(
        &self,
        _owner: ActorRef,
        watch: WatchId,
        request: RequestId,
        after: u64,
    ) -> Result<(Option<ProgressSnapshot>, bool), ReplyError> {
        let state = self.state.lock();
        let record = state.watches.get(&watch).ok_or(ReplyError::Stale)?;
        if record.state != WatchState::Ready {
            return Err(ReplyError::Stale);
        }
        let key = (request, after);
        if !record.dependencies.iter().any(|dependency| {
            dependency.request == request
                && dependency.requirement == WatchRequirement::ProgressAfter(after)
        }) {
            return Err(ReplyError::Unauthorized);
        }
        match record.progress.get(&key) {
            Some(ProgressCapture::Update(snapshot)) => Ok((Some(snapshot.clone()), false)),
            Some(ProgressCapture::Closed) => Ok((None, true)),
            None => Ok((None, false)),
        }
    }

    pub(crate) fn observe_watch(
        &self,
        _owner: ActorRef,
        watch: WatchId,
    ) -> Result<WatchObservation, ReplyError> {
        let mut state = self.state.lock();
        let record = state.watches.get(&watch).ok_or(ReplyError::Stale)?;
        let observation = match &record.state {
            WatchState::Pending => WatchObservation::Pending(PendingProgress {
                actor_terminal: None,
                provider_turn: None,
                last_activity_unix_ms: None,
                progress_revision: first_pending_dependency(&state, record)
                    .and_then(|request| state.requests.get(&request))
                    .and_then(|dependency_record| dependency_record.progress.as_ref())
                    .map(|snapshot| snapshot.revision),
                // Polling this observation is itself the registered watch;
                // its own settlement always wakes the caller.
                watched: true,
            }),
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
        };
        // A watch observed Ready or Unavailable has already delivered its
        // settled state to the owner through this call's return value.
        // Record when, so a queued `WatchChanged` notice describing a
        // transition the owner already observed can be acknowledged without
        // prompting instead of re-announcing state the owner already has.
        if matches!(
            observation,
            WatchObservation::Ready(_) | WatchObservation::Unavailable { .. }
        ) {
            if let Some(record) = state.watches.get_mut(&watch) {
                record.observed_ready_at = Some(unix_time_ms());
            }
        }
        Ok(observation)
    }

    /// Whether `owner` has observed `watch` (via `observe_watch`) already
    /// settled Ready or Unavailable at or after `occurred_at_unix_ms`. A
    /// queued `WatchChanged` notice whose `occurred_at_unix_ms` predates that
    /// observation describes a transition the owner has already picked up
    /// through `pollWatch`/`ObserveWatchWith`, and can be acknowledged
    /// without prompting.
    pub(crate) fn watch_observed_since(
        &self,
        owner: ActorRef,
        watch: WatchId,
        occurred_at_unix_ms: u64,
    ) -> bool {
        self.state.lock().watches.get(&watch).is_some_and(|record| {
            record.owner == owner
                && record
                    .observed_ready_at
                    .is_some_and(|observed_at| observed_at >= occurred_at_unix_ms)
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
    ) -> Result<Vec<WatchNotification>, (Vec<RequestId>, Vec<WatchId>)> {
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
        let released = state
            .requests
            .iter()
            .filter_map(|(request, record)| (record.owner == actor).then_some(*request))
            .collect::<Vec<_>>();
        let mut notifications = Vec::new();
        for request in released {
            notifications.extend(release_request_record(&mut state, request));
        }
        Ok(notifications)
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

/// The registry-owned half of one request's `PendingProgress`: whether a
/// still-pending registered watch depends on it, and its `Progress` channel's
/// current revision, if it has published at least once. The producing
/// actor's own lifecycle and last activity are filled in afterward from the
/// actor runtime observation, which this registry does not hold.
fn base_pending_progress(state: &RequestStateTable, request: RequestId) -> PendingProgress {
    PendingProgress {
        actor_terminal: None,
        provider_turn: None,
        last_activity_unix_ms: None,
        progress_revision: state
            .requests
            .get(&request)
            .and_then(|record| record.progress.as_ref())
            .map(|snapshot| snapshot.revision),
        watched: request_has_pending_watcher(state, request),
    }
}

/// The first dependency of one watch whose own request has not yet settled,
/// in dependency-list order. A still-pending watch always has at least one:
/// once every dependency settles the watch itself becomes `Ready`.
fn first_pending_dependency(state: &RequestStateTable, watch: &WatchRecord) -> Option<RequestId> {
    watch.dependencies.iter().find_map(|dependency| {
        let record = state.requests.get(&dependency.request)?;
        (record.owner_state == OwnerState::Observing).then_some(dependency.request)
    })
}

fn request_has_pending_watcher(state: &RequestStateTable, request: RequestId) -> bool {
    state.watches.values().any(|watch| {
        watch.state == WatchState::Pending
            && watch
                .dependencies
                .iter()
                .any(|dependency| dependency.request == request)
    })
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

/// Release removes the request under the same lock used to accept reads. A
/// watch that has not been polled must therefore stop advertising its former
/// readiness, even if its Ready notification is already queued.
fn release_request_record(
    state: &mut RequestStateTable,
    request: RequestId,
) -> Vec<WatchNotification> {
    let notifications = invalidate_response_watches(state, request);
    if let Some(mut record) = state.requests.remove(&request) {
        record.publish_source_release();
    }
    notifications
}

fn invalidate_response_watches(
    state: &mut RequestStateTable,
    request: RequestId,
) -> Vec<WatchNotification> {
    let mut notifications = Vec::new();
    for (watch_id, watch) in &mut state.watches {
        if !watch
            .dependencies
            .iter()
            .any(|dependency| dependency.request == request)
        {
            continue;
        }
        let previous = match &watch.state {
            WatchState::Pending => WatchStateProjection::Pending,
            WatchState::Ready => WatchStateProjection::Ready,
            WatchState::Unavailable { .. } => continue,
        };
        watch.state = WatchState::Unavailable {
            request,
            failure: ResponseFailure::Released,
        };
        watch.progress.clear();
        if let Some(route) = &mut watch.route {
            route.schedule(*watch_id);
            continue;
        }
        notifications.push(WatchNotification {
            owner: watch.owner,
            watch: *watch_id,
            label: watch.label.clone(),
            previous,
            current: WatchStateProjection::Unavailable {
                request,
                failure: ResponseFailure::Released,
            },
            transition: WatchTransition::Unavailable {
                request,
                failure: ResponseFailure::Released,
            },
            occurred_at_unix_ms: unix_time_ms(),
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

fn reevaluate_watches(state: &mut RequestStateTable) -> Vec<WatchNotification> {
    let mut settlements = Vec::new();
    for (request, record) in &mut state.requests {
        record.publish_source_closure();
        if record.owner_state != OwnerState::Observing || record.target_state == TargetState::Closed
        {
            record.progress = None;
        }
        if record.notify_owner && !record.settlement_notified {
            let transition = match &record.owner_state {
                OwnerState::Ready => Some(SettlementTransition::Ready),
                OwnerState::Unavailable(failure) => {
                    Some(SettlementTransition::Unavailable(failure.clone()))
                }
                OwnerState::Observing | OwnerState::Abandoned => None,
            };
            if let Some(transition) = transition {
                record.settlement_notified = true;
                // The preview describes a successful reply specifically; an
                // `Unavailable` settlement keeps today's guidance-only text.
                let reply_preview = match transition {
                    SettlementTransition::Ready => record.reply_preview.take(),
                    SettlementTransition::Unavailable(_) => None,
                };
                settlements.push((
                    record.owner,
                    *request,
                    record.label.clone(),
                    transition,
                    reply_preview,
                    record.target_path.clone(),
                    record.target_revision.clone(),
                ));
            }
        }
    }
    for (owner, request, label, transition, reply_preview, target_path, target_revision) in
        settlements
    {
        let sequence = next_event_sequence(state, owner);
        state.settlement_notifications.push(SettlementNotification {
            owner,
            request,
            label,
            transition,
            reply_preview,
            target_path,
            target_revision,
            occurred_at_unix_ms: unix_time_ms(),
            sequence,
            watermark: sequence,
        });
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
        let next_state = watch.dependencies.iter().find_map(|dependency| {
            let record = state.requests.get(&dependency.request)?;
            match &record.owner_state {
                OwnerState::Unavailable(failure)
                    if dependency.requirement
                        == (WatchRequirement::Response {
                            allow_failure: false,
                        }) =>
                {
                    Some(WatchState::Unavailable {
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
                    Some(WatchState::Unavailable {
                        request: dependency.request,
                        failure: ResponseFailure::Abandoned,
                    })
                }
                _ => None,
            }
        });
        let next_state = next_state.or_else(|| {
            (0..watch.group_count)
                .all(|group| {
                    watch.dependencies.iter().any(|dependency| {
                        if dependency.group != group {
                            return false;
                        }
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
                })
                .then_some(WatchState::Ready)
        });
        let Some(next_state) = next_state else {
            continue;
        };
        let (transition, current) = match &next_state {
            WatchState::Pending => continue,
            WatchState::Ready => (WatchTransition::Ready, WatchStateProjection::Ready),
            WatchState::Unavailable { request, failure } => {
                watch.progress.clear();
                (
                    WatchTransition::Unavailable {
                        request: *request,
                        failure: failure.clone(),
                    },
                    WatchStateProjection::Unavailable {
                        request: *request,
                        failure: failure.clone(),
                    },
                )
            }
        };
        watch.state = next_state;
        let occurred_at_unix_ms = unix_time_ms();
        watch.transitioned_at_unix_ms = Some(occurred_at_unix_ms);
        if let Some(route) = &mut watch.route {
            route.schedule(*watch_id);
            continue;
        }
        let label = watch.label.clone();
        let owner = watch.owner;
        let watch = *watch_id;
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
    fn settlement_reporting_is_terminal_exact_and_suppressible() {
        let registry = RequestRegistry::default();
        let owner = actor(1);
        let ready_target = actor(2);
        let silent_target = actor(3);
        let failed_target = actor(4);

        let ready =
            registry.reserve_labeled_with_reporting(owner, ready_target, "ready".into(), true);
        registry.mark_queued(owner, ready_target, ready).unwrap();
        registry.present(ready_target, ready).unwrap();
        registry.begin_reply(ready_target, ready).unwrap();
        registry.finish_reply(ready, None);

        let silent =
            registry.reserve_labeled_with_reporting(owner, silent_target, "silent".into(), false);
        registry.mark_queued(owner, silent_target, silent).unwrap();
        registry.present(silent_target, silent).unwrap();
        registry.begin_reply(silent_target, silent).unwrap();
        registry.finish_reply(silent, None);

        let failed =
            registry.reserve_labeled_with_reporting(owner, failed_target, "failed".into(), true);
        registry.mark_target_unavailable(owner, failed);

        let notices = registry.take_settlement_notifications();
        assert_eq!(notices.len(), 2);
        assert_eq!(notices[0].request, ready);
        assert_eq!(notices[0].label, "ready");
        assert_eq!(notices[0].transition, SettlementTransition::Ready);
        assert_eq!(notices[1].request, failed);
        assert_eq!(notices[1].label, "failed");
        assert!(matches!(
            notices[1].transition,
            SettlementTransition::Unavailable(ResponseFailure::TargetUnavailable)
        ));
        assert!(registry.take_settlement_notifications().is_empty());
    }

    #[test]
    fn response_watch_takes_over_settlement_notice_after_valid_registration() {
        let registry = RequestRegistry::default();
        let owner = actor(1);
        let target = actor(2);
        let watched = registry.reserve(owner, target);
        let progress_only = registry.reserve(owner, target);

        registry.register_watch(actor(3), vec![watched]).unwrap();
        registry.register_watch(owner, vec![watched]).unwrap();
        registry
            .register_watch_requirements(
                owner,
                "progress".into(),
                vec![(progress_only, WatchRequirement::ProgressAfter(0))],
            )
            .unwrap();

        registry.mark_target_unavailable(owner, watched);
        registry.mark_target_unavailable(owner, progress_only);
        let notices = registry.take_settlement_notifications();
        assert_eq!(notices.len(), 1);
        assert_eq!(notices[0].request, progress_only);
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
        assert!(matches!(
            authorize(ActorRef {
                id: target.id,
                incarnation: Incarnation(2)
            }),
            Err(ReplyError::WrongIncarnation)
        ));
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
            let notifications = registry.finish_reply(request, None);
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
                        Ok(WatchObservation::Pending(_))
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
    fn progress_watch_closure_is_stable_and_shareable() {
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
            Ok(WatchObservation::Pending(_))
        ));
        let (foreign_watch, _) = registry
            .register_watch_requirements(
                target,
                "foreign-listener".into(),
                vec![(request, WatchRequirement::ProgressAfter(0))],
            )
            .unwrap();
        registry.begin_reply(target, request).unwrap();
        let notifications = registry.finish_reply(request, None);
        assert_eq!(notifications.len(), 2);
        assert!(notifications.iter().any(|notice| notice.watch == watch));
        assert!(notifications
            .iter()
            .any(|notice| notice.watch == foreign_watch));
        for _ in 0..2 {
            assert!(matches!(
                registry.observe_watch_progress(owner, watch, request, 0),
                Ok((None, true))
            ));
        }
        assert!(matches!(
            registry.observe_watch_progress(target, watch, request, 0),
            Ok((None, true))
        ));
        assert!(registry.finish_reply(request, None).is_empty());
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
    fn progress_any_group_wakes_on_one_closure_and_freezes_other_as_pending() {
        let registry = RequestRegistry::default();
        let owner = actor(1);
        let first_target = actor(2);
        let second_target = actor(3);
        let unrelated_target = actor(4);
        let prepare = |target| {
            let request = registry.reserve(owner, target);
            registry.mark_queued(owner, target, request).unwrap();
            registry.present(target, request).unwrap();
            request
        };
        let first = prepare(first_target);
        let second = prepare(second_target);
        let unrelated = prepare(unrelated_target);
        let (watch, initial) = registry
            .register_watch_requirement_groups(
                owner,
                "any-progress".into(),
                vec![vec![
                    (first, WatchRequirement::ProgressAfter(0)),
                    (second, WatchRequirement::ProgressAfter(0)),
                ]],
            )
            .unwrap();
        assert!(initial.is_empty());

        registry.begin_reply(first_target, first).unwrap();
        let notifications = registry.finish_reply(first, None);
        assert_eq!(notifications.len(), 1);
        assert!(matches!(
            registry.observe_watch(owner, watch),
            Ok(WatchObservation::Ready(_))
        ));
        assert!(matches!(
            registry.observe_watch_progress(owner, watch, first, 0),
            Ok((None, true))
        ));
        assert!(matches!(
            registry.observe_watch_progress(owner, watch, second, 0),
            Ok((None, false))
        ));
        assert!(matches!(
            registry.observe_watch_progress(owner, watch, unrelated, 0),
            Err(ReplyError::Unauthorized)
        ));

        registry.begin_reply(second_target, second).unwrap();
        assert!(registry.finish_reply(second, None).is_empty());
        assert!(matches!(
            registry.observe_watch_progress(owner, watch, second, 0),
            Ok((None, false))
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
        registry.finish_reply(request, None);
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
        registry.finish_reply(request, None);
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
        registry.finish_reply(request, None);
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
        assert!(registry.finish_reply(left, None).is_empty());
        registry.begin_reply(right_target, right).unwrap();
        let notifications = registry.finish_reply(right, None);
        assert_eq!(notifications.len(), 1);
        assert_eq!(notifications[0].watch, watch);
        assert_eq!(notifications[0].label, "watch");
        assert_eq!(notifications[0].previous, WatchStateProjection::Pending);
        assert_eq!(notifications[0].current, WatchStateProjection::Ready);
        assert_eq!(notifications[0].sequence, ActorEventSequence(1));
        assert_eq!(notifications[0].watermark, ActorEventSequence(1));
        assert_eq!(notifications[0].transition, WatchTransition::Ready);
        assert_eq!(registry.finish_reply(right, None), Vec::new());
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

        assert_eq!(registry.abort_unsubmitted(owner).0, vec![leaked]);
        assert_eq!(
            registry.observe_response(owner, leaked),
            Err(ReplyError::Stale)
        );
        assert!(matches!(
            registry.observe_response(owner, committed),
            Ok(ResponseObservation::Pending(_))
        ));
        assert_eq!(registry.abort_unsubmitted(other_owner).0, vec![unrelated]);
        assert!(registry.abort_unsubmitted(owner).0.is_empty());
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
        assert!(registry.finish_reply(request, None).is_empty());

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
        assert!(matches!(
            registry.observe_response(owner, request),
            Ok(ResponseObservation::Pending(_))
        ));
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
        let notifications = registry.finish_reply(ready, None);
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
                registry.finish_reply(request, None)
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
        assert!(registry.finish_reply(request, None).is_empty());
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
    fn another_actor_can_observe_and_watch_but_cannot_control_a_response() {
        let registry = RequestRegistry::default();
        let owner = actor(1);
        let target = actor(2);
        let intruder = actor(3);
        let request = registry.reserve(owner, target);
        registry.mark_queued(owner, target, request).unwrap();

        assert!(matches!(
            registry.observe_response(intruder, request),
            Ok(ResponseObservation::Pending(_))
        ));
        let (watch, _) = registry.register_watch(intruder, vec![request]).unwrap();
        assert!(matches!(
            registry.observe_watch(intruder, watch),
            Ok(WatchObservation::Pending(_))
        ));
        assert!(matches!(
            registry.observe_reply(intruder, request),
            Ok(ReplyObservation::Open)
        ));
        assert_eq!(
            registry.begin_reply(intruder, request),
            Err(ReplyError::Unauthorized)
        );
        assert_eq!(
            registry.cancel_request(intruder, request, CancellationReason::RequesterCancelled),
            Err(ReplyError::Unauthorized)
        );
        assert_eq!(
            registry.forget_response(intruder, request),
            Err(ReplyError::Unauthorized)
        );
    }

    #[test]
    fn foreign_listener_preserves_owner_notice_and_release_invalidates_queued_ready() {
        let registry = RequestRegistry::default();
        let owner = actor(1);
        let target = actor(2);
        let child = actor(3);
        let request = registry.reserve_labeled_with_reporting(owner, target, "shared".into(), true);
        registry.mark_queued(owner, target, request).unwrap();
        registry.present(target, request).unwrap();
        let (owner_watch, _) = registry.register_watch(owner, vec![request]).unwrap();
        let (child_watch, _) = registry.register_watch(child, vec![request]).unwrap();

        registry.begin_reply(target, request).unwrap();
        let ready_notices = registry.finish_reply(request, None);
        assert_eq!(ready_notices.len(), 2);
        assert!(ready_notices
            .iter()
            .any(|notice| notice.watch == owner_watch));
        assert!(ready_notices
            .iter()
            .any(|notice| notice.watch == child_watch));
        // The owner's own response watch suppresses its ordinary settlement
        // notice, but the child's independent watch cannot change that choice.
        assert!(registry.take_settlement_notifications().is_empty());

        assert_eq!(
            registry.observe_response(child, request),
            Ok(ResponseObservation::Ready)
        );
        let (outcome, release_notices) = registry.forget_response(owner, request).unwrap();
        assert_eq!(outcome, ForgetResponseOutcome::Forgotten);
        assert_eq!(release_notices.len(), 2);
        assert_eq!(
            registry.observe_response(child, request),
            Err(ReplyError::Stale)
        );
        assert_eq!(
            registry.observe_watch(child, child_watch),
            Ok(WatchObservation::Unavailable {
                request,
                failure: ResponseFailure::Released
            })
        );
        assert_eq!(
            registry.observe_watch(owner, owner_watch),
            Ok(WatchObservation::Unavailable {
                request,
                failure: ResponseFailure::Released
            })
        );
    }

    #[test]
    fn foreign_watch_alone_does_not_suppress_owner_settlement() {
        let registry = RequestRegistry::default();
        let owner = actor(1);
        let target = actor(2);
        let child = actor(3);
        let request = registry.reserve_labeled_with_reporting(owner, target, "shared".into(), true);
        registry.mark_queued(owner, target, request).unwrap();
        registry.present(target, request).unwrap();
        let (watch, _) = registry.register_watch(child, vec![request]).unwrap();
        registry.begin_reply(target, request).unwrap();
        let notices = registry.finish_reply(request, None);
        assert_eq!(notices.len(), 1);
        assert_eq!(notices[0].watch, watch);
        let owner_notices = registry.take_settlement_notifications();
        assert_eq!(owner_notices.len(), 1);
        assert_eq!(owner_notices[0].owner, owner);
        assert_eq!(owner_notices[0].request, request);
    }

    #[test]
    fn release_wakes_a_pending_foreign_watch_without_releasing_active_work() {
        let registry = RequestRegistry::default();
        let owner = actor(1);
        let target = actor(2);
        let child = actor(3);
        let completed = registry.reserve(owner, target);
        let active = registry.reserve(owner, target);
        for request in [completed, active] {
            registry.mark_queued(owner, target, request).unwrap();
            registry.present(target, request).unwrap();
        }
        let (watch, _) = registry
            .register_watch(child, vec![completed, active])
            .unwrap();
        registry.begin_reply(target, completed).unwrap();
        assert!(registry.finish_reply(completed, None).is_empty());
        assert!(matches!(
            registry.observe_watch(child, watch),
            Ok(WatchObservation::Pending(_))
        ));

        let (_, notices) = registry.forget_response(owner, completed).unwrap();
        assert_eq!(notices.len(), 1);
        assert_eq!(notices[0].owner, child);
        assert_eq!(
            notices[0].transition,
            WatchTransition::Unavailable {
                request: completed,
                failure: ResponseFailure::Released
            }
        );
        assert_eq!(
            registry.observe_watch(child, watch),
            Ok(WatchObservation::Unavailable {
                request: completed,
                failure: ResponseFailure::Released
            })
        );
        assert_eq!(
            registry.forget_response(owner, active),
            Ok((ForgetResponseOutcome::StillPending, Vec::new()))
        );
    }

    #[test]
    fn mixed_watch_progress_read_after_release_reports_unavailable() {
        let registry = RequestRegistry::default();
        let owner = actor(1);
        let target = actor(2);
        let child = actor(3);
        let other = actor(4);
        let request = registry.reserve(owner, target);
        registry.mark_queued(owner, target, request).unwrap();
        registry.present(target, request).unwrap();
        let (watch, _) = registry
            .register_watch_requirements(
                child,
                "mixed".into(),
                vec![
                    (
                        request,
                        WatchRequirement::Response {
                            allow_failure: false,
                        },
                    ),
                    (request, WatchRequirement::ProgressAfter(0)),
                ],
            )
            .unwrap();
        registry.begin_reply(target, request).unwrap();
        registry.finish_reply(request, None);
        assert_eq!(
            registry.observe_watch(child, watch),
            Ok(WatchObservation::Ready(Vec::new()))
        );
        registry.forget_response(owner, request).unwrap();
        assert!(matches!(
            registry.observe_watch_progress(child, watch, request, 0),
            Err(ReplyError::Stale)
        ));
        assert!(matches!(
            registry.observe_watch_progress(other, watch, request, 0),
            Err(ReplyError::Stale)
        ));
        assert_eq!(
            registry.observe_watch(other, watch),
            Ok(WatchObservation::Unavailable {
                request,
                failure: ResponseFailure::Released
            })
        );
    }

    #[test]
    fn forgetting_terminal_owner_invalidates_foreign_watch() {
        let registry = RequestRegistry::default();
        let owner = actor(1);
        let target = actor(2);
        let observer = actor(3);
        let request = registry.reserve(owner, target);
        registry.mark_queued(owner, target, request).unwrap();
        registry.present(target, request).unwrap();
        let (watch, _) = registry.register_watch(observer, vec![request]).unwrap();
        registry.begin_reply(target, request).unwrap();
        registry.finish_reply(request, None);

        let notifications = registry.forget_terminal_actor_metadata(owner).unwrap();
        assert_eq!(notifications.len(), 1);
        assert_eq!(notifications[0].owner, observer);
        assert_eq!(
            notifications[0].transition,
            WatchTransition::Unavailable {
                request,
                failure: ResponseFailure::Released
            }
        );
        assert_eq!(
            registry.observe_watch(observer, watch),
            Ok(WatchObservation::Unavailable {
                request,
                failure: ResponseFailure::Released
            })
        );
    }

    #[test]
    fn explicit_cleanup_releases_terminal_response_and_invalidates_dependent_watch() {
        let registry = RequestRegistry::default();
        let owner = actor(1);
        let target = actor(2);
        let request = registry.reserve(owner, target);
        registry.mark_queued(owner, target, request).unwrap();
        registry.present(target, request).unwrap();
        let (watch, _) = registry.register_watch(owner, vec![request]).unwrap();

        assert_eq!(
            registry.forget_response(owner, request),
            Ok((ForgetResponseOutcome::StillPending, Vec::new()))
        );
        assert_eq!(
            registry.forget_watch(owner, watch),
            Ok(ForgetWatchOutcome::StillPending)
        );
        registry.begin_reply(target, request).unwrap();
        registry.finish_reply(request, None);
        let (outcome, notifications) = registry.forget_response(owner, request).unwrap();
        assert_eq!(outcome, ForgetResponseOutcome::Forgotten);
        assert_eq!(notifications.len(), 1);
        assert_eq!(
            notifications[0].transition,
            WatchTransition::Unavailable {
                request,
                failure: ResponseFailure::Released
            }
        );
        assert_eq!(
            registry.observe_watch(owner, watch),
            Ok(WatchObservation::Unavailable {
                request,
                failure: ResponseFailure::Released
            })
        );
        assert_eq!(
            registry.forget_watch(owner, watch),
            Ok(ForgetWatchOutcome::Forgotten)
        );
        assert_eq!(
            registry.forget_response(owner, request),
            Err(ReplyError::Stale)
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
        registry.finish_reply(ready, None);

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
        assert!(
            matches!(
                registry.observe_response(owner, outside),
                Ok(ResponseObservation::Pending(_))
            ),
            "another campaign remains untouched"
        );
    }

    /// A transition computed before cleanup can still be in a publisher's
    /// hand after it. The publisher's guard is this lookup, so a forgotten
    /// watch has no notice left to deliver while a live one still does.
    #[test]
    fn a_forgotten_watch_has_no_notice_left_to_publish() {
        let registry = RequestRegistry::default();
        let owner = actor(1);
        let target = actor(2);

        let request = registry.reserve_labeled(owner, target, "work".into());
        registry.mark_queued(owner, target, request).unwrap();
        registry.present(target, request).unwrap();
        let (watch, _) = registry.register_watch(owner, vec![request]).unwrap();
        registry.begin_reply(target, request).unwrap();

        // The settlement that makes the watch Ready is exactly the transition
        // a publisher would be holding when cleanup runs.
        let notifications = registry.finish_reply(request, None);
        assert!(
            notifications.iter().any(|notice| notice.watch == watch),
            "the settled request must produce the watch transition under test"
        );
        assert!(registry.retains_watch(owner, watch));

        let targets = [target].into_iter().collect();
        let outcome = registry.cleanup_campaign_metadata(owner, &targets);
        assert_eq!(outcome.forgotten_watches, vec![watch]);
        assert!(
            !registry.retains_watch(owner, watch),
            "a notice for a forgotten watch must not be delivered"
        );
    }

    #[test]
    fn watch_retention_is_scoped_to_its_exact_owner() {
        let registry = RequestRegistry::default();
        let owner = actor(1);
        let intruder = actor(3);
        let target = actor(2);

        let request = registry.reserve(owner, target);
        registry.mark_queued(owner, target, request).unwrap();
        let (watch, _) = registry.register_watch(owner, vec![request]).unwrap();

        assert!(registry.retains_watch(owner, watch));
        assert!(!registry.retains_watch(intruder, watch));
        assert!(!registry.retains_watch(
            ActorRef {
                id: owner.id,
                incarnation: crate::Incarnation(2),
            },
            watch
        ));
    }

    #[tokio::test(start_paused = true)]
    async fn cleanup_guard_watchdog_settles_once_and_a_late_drop_is_a_no_op() {
        let registry = std::sync::Arc::new(RequestRegistry::default());
        let owner = actor(1);
        let target = actor(2);
        let inspected = [(target, registry.cleanup_revision(target))];

        // Simulate a cleanup holder that never comes back (a stuck retire_by
        // RPC to `target`, or similar): the guard is admitted and then just
        // held, never dropped by its own logic.
        let stuck = registry.begin_cleanup(owner, &inspected).ok().unwrap();
        assert!(registry.state.lock().cleaning.contains(&target));

        // Give the freshly spawned watchdog task its first poll so its
        // `sleep` timer is actually registered before we advance past it.
        tokio::task::yield_now().await;
        tokio::time::advance(CLEANUP_GUARD_BUDGET + std::time::Duration::from_millis(1)).await;
        // Let the spawned watchdog task actually run past its sleep.
        tokio::task::yield_now().await;
        assert!(
            !registry.state.lock().cleaning.contains(&target),
            "the watchdog must force-release a guard held past its budget"
        );

        // A fresh cleanup can now be admitted for the same actor.
        let inspected = [(target, registry.cleanup_revision(target))];
        let fresh = registry.begin_cleanup(owner, &inspected).ok().unwrap();
        assert!(registry.state.lock().cleaning.contains(&target));

        // The original (stuck) guard's holder finally returns and drops it.
        // That late release must be a no-op: it must not clear the fresh
        // guard's still-active claim on the same actor.
        drop(stuck);
        assert!(
            registry.state.lock().cleaning.contains(&target),
            "a late release from an expired guard must not clear a fresh guard's claim"
        );

        drop(fresh);
        assert!(!registry.state.lock().cleaning.contains(&target));
    }
}
