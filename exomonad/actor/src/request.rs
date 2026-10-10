#[cfg(test)]
mod result_tests;
#[cfg(test)]
mod sequence_tests;
#[cfg(test)]
pub(crate) mod test_support;

pub(crate) mod readiness;

mod activation;
pub(crate) use activation::{ActivationPublicationRefusal, RequestActivationCompletion};

mod invocation;
pub(crate) mod routes;
pub(crate) mod sources;
mod updates;
pub(crate) use invocation::RequestCleanupState;
pub use updates::{
    LateUpdateEvidence, RequestUpdateCorrelation, RequestUpdateDelivery, RequestUpdateId,
    RequestUpdatePresentation, RequestUpdateReconciler, RequestUpdateState,
    UpdateReconciliationError,
};

use std::collections::{HashMap, VecDeque};

use parking_lot::Mutex;
use serde::{Deserialize, Serialize};

use crate::owned_result::{OwnedResultSnapshot, RequestResultDestination};
use crate::{ActorExitKind, ActorRef, ActorTerminal};
use std::sync::Arc;

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

/// The operation that owns the two-step construction of a request payload.
/// Actor identity authorizes the request; this identity limits rollback to
/// reservations created by the settling workbench or route callback.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum RequestReservationOwner {
    Workbench {
        execution: tidepool_runtime::session::WorkbenchExecutionId,
        attempt: WorkbenchReservationAttempt,
    },
    Route(WatchId),
    Scope(i64),
}

/// Cleanup lifetime is independent of actor authority and request construction.
/// Scope and run admission are checked by the existing resource owner before
/// entering the request registry; these identities do not grant authority.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ResourceCleanupOwner {
    Actor,
    Invocation(RequestReservationOwner),
    Scope(i64),
    Run,
}

/// Distinguishes a concrete workbench attempt from a replay identity. An
/// execution id can recur when a caller retries the same logical operation;
/// cleanup from that earlier attempt must never release the retry's requests.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct WorkbenchReservationAttempt([u8; 16]);

impl WorkbenchReservationAttempt {
    pub(crate) fn fresh() -> Self {
        Self(*uuid::Uuid::new_v4().as_bytes())
    }
}

/// Submission causality is telemetry, independent of reservation and cleanup custody.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RequestActivationOrigin {
    pub(crate) request: RequestId,
    pub(crate) parent_actor: ActorRef,
    pub(crate) target: ActorRef,
    pub(crate) execution: tidepool_runtime::session::WorkbenchExecutionId,
    pub(crate) attempt: WorkbenchReservationAttempt,
}

pub(crate) struct RequestPresentation {
    pub(crate) cancellation: Option<CancellationReason>,
    pub(crate) origin: Option<RequestActivationOrigin>,
}

impl std::fmt::Display for WorkbenchReservationAttempt {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        uuid::Uuid::from_bytes(self.0).fmt(formatter)
    }
}

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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ReplyError {
    UpdatePending,
    Stale,
    AlreadySettled,
    Unauthorized,
    WrongIncarnation,
    InvalidReadiness,
    ProgressTypeMismatch,
    ReplyResultTypeMismatch,
    ReplyResultUnavailable,
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
    Rejected(ReplyError),
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
    Rejected(ReplyError),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WatchObservation {
    Pending(PendingProgress),
    Ready(readiness::Decision),
    Unavailable {
        request: RequestId,
        failure: ResponseFailure,
    },
    Rejected(ReplyError),
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
    /// from a fork workspace. For a command settlement, the commit its
    /// working directory was at when the command started.
    pub target_revision: Option<String>,
    /// The command job this settlement reports, when the settled request is
    /// a command job's completion rather than an actor's reply.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command_job: Option<String>,
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
    Ready(RequestSuccess),
    Unavailable(ResponseFailure),
    Abandoned,
}

/// Typed success and command completion carry different, mandatory evidence.
#[derive(Debug, Clone)]
enum RequestSuccess {
    Typed(Arc<OwnedResultSnapshot>),
    Command(tidepool_bridge_effects::CommandReport),
}

impl PartialEq for RequestSuccess {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Typed(left), Self::Typed(right)) => Arc::ptr_eq(left, right),
            (Self::Command(left), Self::Command(right)) => left == right,
            _ => false,
        }
    }
}
impl Eq for RequestSuccess {}

/// One accepted reply attempt. Async incorporation cannot manufacture or copy
/// this winner; committing consumes it and rechecks the exact request state.
#[derive(Debug)]
pub(crate) struct RequestReplyClaim {
    request: RequestId,
    target: ActorRef,
    generation: u64,
    destination: Arc<RequestResultDestination>,
}

impl RequestReplyClaim {
    pub(crate) fn request(&self) -> RequestId {
        self.request
    }
    pub(crate) fn destination(&self) -> Arc<RequestResultDestination> {
        Arc::clone(&self.destination)
    }
}

impl PartialEq for RequestReplyClaim {
    fn eq(&self, other: &Self) -> bool {
        self.request == other.request
            && self.target == other.target
            && self.generation == other.generation
            && Arc::ptr_eq(&self.destination, &other.destination)
    }
}
impl Eq for RequestReplyClaim {}

enum ProgressTypeAdmission {
    Unadmitted,
    Absent,
    Typed(std::sync::Arc<tidepool_toolchain::checked_cell::CanonicalInputTypeWitness>),
}

impl From<Option<std::sync::Arc<tidepool_toolchain::checked_cell::CanonicalInputTypeWitness>>>
    for ProgressTypeAdmission
{
    fn from(
        witness: Option<
            std::sync::Arc<tidepool_toolchain::checked_cell::CanonicalInputTypeWitness>,
        >,
    ) -> Self {
        match witness {
            Some(witness) => Self::Typed(witness),
            None => Self::Absent,
        }
    }
}

struct RequestRecord {
    sources: Vec<sources::RequestSourceConnection>,
    updates: Vec<updates::UpdateRecord>,
    activation: Option<activation::ActivationRecord>,
    owner: ActorRef,
    reservation_owner: Option<RequestReservationOwner>,
    submission_origin: Option<RequestActivationOrigin>,
    cleanup_owner: ResourceCleanupOwner,
    target: ActorRef,
    label: String,
    target_state: TargetState,
    owner_state: OwnerState,
    result_destination: Option<Arc<RequestResultDestination>>,
    reply_claim: Option<u64>,
    deadline: Option<ActiveRequestDeadline>,
    progress: Option<ProgressSnapshot>,
    progress_type: ProgressTypeAdmission,
    /// Authored reporting intent; subscriptions temporarily own its wake.
    notify_owner: bool,
    /// The owner notice was emitted, a named subscription owns it, or a direct
    /// wait passed its cancellation gate and captured it.
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
    /// Set when this record is a command job's completion, owned and targeted
    /// by the actor that owns the job. Such a record never enters a mailbox:
    /// it is excluded from target-side work and settles only through
    /// [`RequestRegistry::settle_command`].
    command_job: Option<String>,
    /// A command record armed for a watch that has not registered yet. It is
    /// not released before that watch names it or is refused.
    held_for_watch: bool,
}

/// Snapshots share ownership, not a consumption cursor. Replacing the latest
/// publication drops only the registry's reference; an observer can retain it.
#[derive(Clone, Debug)]
pub(crate) struct ProgressSnapshot {
    pub revision: u64,
    pub type_witness: std::sync::Arc<tidepool_toolchain::checked_cell::CanonicalInputTypeWitness>,
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
    Rejected(ReplyError),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ReadinessDependency {
    Request(RequestId, WatchRequirement),
    Watch(WatchId),
}

/// Immutable projection custody. Nested watches retain the exact original
/// decision and selected snapshots, independently of the public source handle.
struct WatchSnapshot {
    decision: readiness::Decision,
    progress: HashMap<(RequestId, u64), ProgressCapture>,
    commands: HashMap<usize, (String, tidepool_bridge_effects::CommandReport)>,
    responses: HashMap<usize, Arc<OwnedResultSnapshot>>,
    sources: HashMap<usize, std::sync::Arc<WatchSnapshot>>,
}

struct WatchRecord {
    transient: bool,
    route: Option<routes::WatchRoute>,
    owner: ActorRef,
    label: String,
    dependencies: Vec<WatchDependency>,
    plan: readiness::Plan<ReadinessDependency>,
    evaluation: readiness::Evaluation,
    snapshot: Option<std::sync::Arc<WatchSnapshot>>,
    sources: HashMap<usize, std::sync::Arc<WatchSnapshot>>,
    state: WatchState,
    progress: HashMap<(RequestId, u64), ProgressCapture>,
    commands: HashMap<usize, (String, tidepool_bridge_effects::CommandReport)>,
    responses: HashMap<usize, Arc<OwnedResultSnapshot>>,
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
    waiters: HashMap<u64, tokio::sync::oneshot::Sender<()>>,
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
    node: usize,
}

#[derive(Default)]
struct RequestStateTable {
    next_request: u64,
    next_reply_claim: u64,
    received_requests: HashMap<ActorRef, u64>,
    next_watch: u64,
    next_watch_waiter: u64,
    next_event_by_actor: HashMap<ActorRef, u64>,
    cleanup_revision: HashMap<ActorRef, u64>,
    cleaning: std::collections::HashSet<ActorRef>,
    requests: HashMap<RequestId, RequestRecord>,
    watches: std::collections::BTreeMap<WatchId, WatchRecord>,
    settlement_notifications: VecDeque<SettlementNotification>,
}

enum RequestAdmission {
    Operation {
        owner: ActorRef,
        target: ActorRef,
        label: String,
        notify_owner: bool,
        reservation_owner: Option<RequestReservationOwner>,
        cleanup_owner: ResourceCleanupOwner,
    },
    CommandSettlement {
        owner: ActorRef,
        job: String,
        notify_owner: bool,
    },
}

enum RequestSettlement {
    CommandComplete {
        report: String,
        revision: Option<String>,
        value: tidepool_bridge_effects::CommandReport,
    },
    ReplyComplete {
        claim: RequestReplyClaim,
        value: Arc<OwnedResultSnapshot>,
        preview: Option<String>,
    },
    ReplyFailed(String),
    CancellationAcknowledged,
}

struct SettlementEffects {
    notifications: Vec<WatchNotification>,
}

impl RequestStateTable {
    fn admit(&mut self, admission: RequestAdmission) -> RequestId {
        // One process-local sequence issues every request identity. Keep the
        // distinct initial states in this constructor: command records are
        // already presented and may be held for a not-yet-registered watch.
        // A counter wrap would require 2^64 reservations in one process.
        #[allow(clippy::expect_used)]
        {
            self.next_request = self
                .next_request
                .checked_add(1)
                .expect("request identity exhausted");
        }
        let id = RequestId(self.next_request);
        let (
            owner,
            target,
            label,
            notify_owner,
            reservation_owner,
            cleanup_owner,
            target_state,
            command_job,
            held_for_watch,
        ) = match admission {
            RequestAdmission::Operation {
                owner,
                target,
                label,
                notify_owner,
                reservation_owner,
                cleanup_owner,
            } => (
                owner,
                target,
                label,
                notify_owner,
                reservation_owner,
                cleanup_owner,
                TargetState::Reserved,
                None,
                false,
            ),
            RequestAdmission::CommandSettlement {
                owner,
                job,
                notify_owner,
            } => (
                owner,
                owner,
                format!("job {job}"),
                notify_owner,
                None,
                ResourceCleanupOwner::Actor,
                TargetState::Presented,
                Some(job),
                !notify_owner,
            ),
        };
        self.requests.insert(
            id,
            RequestRecord {
                sources: Vec::new(),
                updates: Vec::new(),
                activation: None,
                owner,
                reservation_owner,
                submission_origin: None,
                cleanup_owner,
                target,
                label,
                target_state,
                owner_state: OwnerState::Observing,
                result_destination: None,
                reply_claim: None,
                deadline: None,
                progress: None,
                progress_type: ProgressTypeAdmission::Unadmitted,
                notify_owner,
                settlement_notified: false,
                registered_at_unix_ms: unix_time_ms(),
                reply_preview: None,
                target_path: None,
                target_revision: None,
                command_job,
                held_for_watch,
            },
        );
        id
    }

    fn settle(
        &mut self,
        request: RequestId,
        transition: RequestSettlement,
    ) -> Option<SettlementEffects> {
        let releases_command = match transition {
            RequestSettlement::CommandComplete {
                report,
                revision,
                value,
            } => {
                let record = self.requests.get_mut(&request)?;
                if record.command_job.is_none() || record.target_state == TargetState::Closed {
                    return None;
                }
                record.target_state = TargetState::Closed;
                if record.owner_state == OwnerState::Observing {
                    record.owner_state = OwnerState::Ready(RequestSuccess::Command(value));
                }
                record.reply_preview = Some(report);
                record.target_revision = revision;
                true
            }
            RequestSettlement::ReplyComplete {
                claim,
                value,
                preview,
            } => {
                let record = self.requests.get_mut(&request)?;
                if record.target_state != TargetState::Settling
                    || record.target != claim.target
                    || record.reply_claim != Some(claim.generation)
                {
                    return None;
                }
                record.target_state = TargetState::Closed;
                record.reply_claim = None;
                if record.owner_state == OwnerState::Observing {
                    if value.belongs_to(&claim.destination) {
                        record.owner_state = OwnerState::Ready(RequestSuccess::Typed(value));
                        record.reply_preview = preview;
                    } else {
                        record.owner_state =
                            OwnerState::Unavailable(ResponseFailure::SettlementFailed(
                                "reply result belongs to a different request admission".into(),
                            ));
                    }
                }
                false
            }
            RequestSettlement::ReplyFailed(detail) => {
                if let Some(record) = self.requests.get_mut(&request) {
                    if record.target_state == TargetState::Settling {
                        record.target_state = TargetState::Closed;
                        record.reply_claim = None;
                        if record.owner_state == OwnerState::Observing {
                            record.owner_state =
                                OwnerState::Unavailable(ResponseFailure::SettlementFailed(detail));
                        }
                    }
                }
                false
            }
            RequestSettlement::CancellationAcknowledged => {
                let record = self.requests.get_mut(&request)?;
                let TargetState::AcknowledgingCancellation(reason) = record.target_state else {
                    return None;
                };
                record.target_state = TargetState::Closed;
                if record.owner_state == OwnerState::Observing {
                    record.owner_state = OwnerState::Unavailable(match reason {
                        CancellationReason::RequesterCancelled => ResponseFailure::Cancelled,
                        CancellationReason::DeadlineExpired => ResponseFailure::DeadlineExceeded,
                    });
                }
                false
            }
        };

        let notifications = self.reevaluate_watches();
        if releases_command {
            // Watch evaluation first copies all notice data out of the record;
            // only then may a settled, unobserved command record be dropped.
            release_settled_commands(self);
        }
        Some(SettlementEffects { notifications })
    }
}

/// One process-local owner for request identity, terminal state, and watch
/// readiness. Typed successes retain the runtime-authenticated ROOT-owned
/// snapshot; watch and source captures share its immutable ownership.
#[derive(Default)]
pub(crate) struct RequestRegistry {
    state: Mutex<RequestStateTable>,
    settlement_publication: tokio::sync::Mutex<()>,
}

/// A cancellable subscription to one retained watch's readiness transition.
/// The watch and its dependencies outlive this value.
pub(crate) struct WatchWaitSubscription {
    registry: std::sync::Weak<RequestRegistry>,
    owner: ActorRef,
    watch: WatchId,
    waiter: u64,
    receiver: Option<tokio::sync::oneshot::Receiver<()>>,
}

impl WatchWaitSubscription {
    pub(crate) async fn wait(mut self) -> Result<WatchObservation, ReplyError> {
        if let Some(receiver) = self.receiver.take() {
            let _ = receiver.await;
        }
        let registry = self.registry.upgrade().ok_or(ReplyError::Stale)?;
        registry.observe_subscribed_watch(self.owner, self.watch)
    }
}

impl Drop for WatchWaitSubscription {
    fn drop(&mut self) {
        if let Some(registry) = self.registry.upgrade() {
            registry.unsubscribe_watch_waiter(self.watch, self.waiter);
        }
    }
}

pub(crate) struct ActorRequestStatus {
    pub pending_responses: Vec<(RequestId, String)>,
    pub ready_responses: Vec<(RequestId, String)>,
    pub unavailable_responses: Vec<(RequestId, String, ResponseFailure)>,
    pub pending_watches: Vec<(WatchId, String)>,
    pub ready_watches: Vec<(WatchId, String)>,
    pub unavailable_watches: Vec<(WatchId, String, ResponseFailure)>,
    pub rejected_watches: Vec<(WatchId, String, ReplyError)>,
    pub deadlines: Vec<(RequestId, String)>,
    /// Command jobs whose completion this actor still awaits, by job id.
    pub running_jobs: Vec<String>,
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
    /// Running command jobs whose completion settles to this actor; the
    /// label is the job id.
    pub running_jobs: Vec<PendingResponseAge>,
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
            // A running command does not hold its owner: retiring the owner
            // cancels the job, and its settlement then reports that.
            (record.command_job.is_none()
                && (targets.contains(&record.owner) || targets.contains(&record.target)))
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
    /// Wait for one retained watch to become terminal. A readiness wake is a
    /// prompt to inspect the state again; a pending observation subscribes
    /// afresh under the same exact-owner check.
    pub(crate) async fn await_watch(
        self: &std::sync::Arc<Self>,
        owner: ActorRef,
        watch: WatchId,
    ) -> Result<WatchObservation, ReplyError> {
        loop {
            let observation = self.subscribe_watch(owner, watch)?.wait().await?;
            if !matches!(observation, WatchObservation::Pending(_)) {
                return Ok(observation);
            }
        }
    }

    /// Atomically subscribe to one watch and inspect its current state. The
    /// returned lease owns only this waiter; dropping it never changes the
    /// request or the watch itself.
    pub(crate) fn subscribe_watch(
        self: &std::sync::Arc<Self>,
        owner: ActorRef,
        watch: WatchId,
    ) -> Result<WatchWaitSubscription, ReplyError> {
        let (sender, receiver) = tokio::sync::oneshot::channel();
        let mut state = self.state.lock();
        let record = state.watches.get(&watch).ok_or(ReplyError::Stale)?;
        if record.owner != owner {
            return Err(identity_error(record.owner, owner));
        }
        state.next_watch_waiter = state
            .next_watch_waiter
            .checked_add(1)
            .ok_or(ReplyError::Stale)?;
        let waiter = state.next_watch_waiter;
        let record = state.watches.get_mut(&watch).ok_or(ReplyError::Stale)?;
        let already_ready = record.state != WatchState::Pending;
        record.waiters.insert(waiter, sender);
        if already_ready {
            if let Some(sender) = record.waiters.remove(&waiter) {
                let _ = sender.send(());
            }
        }
        Ok(WatchWaitSubscription {
            registry: std::sync::Arc::downgrade(self),
            owner,
            watch,
            waiter,
            receiver: Some(receiver),
        })
    }

    fn unsubscribe_watch_waiter(&self, watch: WatchId, waiter: u64) {
        if let Some(record) = self.state.lock().watches.get_mut(&watch) {
            record.waiters.remove(&waiter);
        }
    }

    fn observe_subscribed_watch(
        &self,
        owner: ActorRef,
        watch: WatchId,
    ) -> Result<WatchObservation, ReplyError> {
        let mut state = self.state.lock();
        let record = state.watches.get(&watch).ok_or(ReplyError::Stale)?;
        if record.owner != owner {
            return Err(identity_error(record.owner, owner));
        }
        let observation = observe_watch_locked(&mut state, owner, watch)?;
        Ok(observation)
    }

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
            wake_watch_waiters(watch);
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
        value: tidepool_runtime::session::RuntimeProgressPublication,
        session: tidepool_repr::SessionId,
    ) -> Result<(u64, Vec<WatchNotification>), ReplyError> {
        let mut state = self.state.lock();
        let record = state.requests.get_mut(&request).ok_or(ReplyError::Stale)?;
        authorize_progress_publication(record, target)?;
        let (value, type_witness) = value.into_parts();
        match &record.progress_type {
            ProgressTypeAdmission::Typed(expected) if expected == &type_witness => {}
            _ => return Err(ReplyError::ProgressTypeMismatch),
        }
        let revision = record
            .progress
            .as_ref()
            .map_or(Some(1), |previous| previous.revision.checked_add(1))
            .filter(|revision| *revision <= i64::MAX as u64)
            .ok_or(ReplyError::Stale)?;
        record.progress = Some(ProgressSnapshot {
            revision,
            type_witness,
            value: std::sync::Arc::new(value),
            session,
        });
        record.publish_source_progress();
        Ok((revision, state.reevaluate_watches()))
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
                    && record.command_job.is_none()
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
                wake_watch_waiters(state.watches.get_mut(&watch).expect("watch checked"));
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
        release_settled_commands(&mut state);
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
                    && record.command_job.is_none()
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
            if record.target != target || record.command_job.is_some() {
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

    /// The lowest request presented to `target` whose owner still observes
    /// it and whose reply has not begun (`Settling` means a reply is in
    /// flight). `None` while `target` itself waits on a response or watch it
    /// owns: that settlement starts its next turn, so an idle turn is not
    /// yet a turn that ended without replying.
    pub(crate) fn open_without_reply(&self, target: ActorRef) -> Option<RequestId> {
        let state = self.state.lock();
        let waiting =
            // A running command job is not a wait that starts the next turn:
            // a background server may run for the whole session.
            state.requests.values().any(|record| {
                record.owner == target
                    && record.owner_state == OwnerState::Observing
                    && record.command_job.is_none()
            }) || state
                .watches
                .values()
                .any(|record| record.owner == target && record.state == WatchState::Pending);
        if waiting {
            return None;
        }
        state
            .requests
            .iter()
            .filter(|(_, record)| {
                record.target == target
                    && record.command_job.is_none()
                    && record.target_state == TargetState::Presented
                    && record.owner_state == OwnerState::Observing
            })
            .map(|(request, _)| *request)
            .min()
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
            rejected_watches: Vec::new(),
            deadlines: Vec::new(),
            running_jobs: Vec::new(),
        };
        for (request, record) in &state.requests {
            if record.owner != owner {
                continue;
            }
            if let Some(job) = &record.command_job {
                if record.owner_state == OwnerState::Observing {
                    status.running_jobs.push(job.clone());
                }
                continue;
            }
            match record.owner_state {
                OwnerState::Ready(_) => status
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
                WatchState::Rejected(error) => {
                    status
                        .rejected_watches
                        .push((*watch, record.label.clone(), error))
                }
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
            .rejected_watches
            .sort_unstable_by_key(|entry| entry.0);
        status
            .unavailable_watches
            .sort_unstable_by_key(|entry| entry.0);
        status.deadlines.sort_unstable();
        status.running_jobs.sort_unstable();
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
        let (mut running_jobs, mut pending_responses): (Vec<_>, Vec<_>) = state
            .requests
            .iter()
            .filter(|(_, record)| {
                record.owner == owner && matches!(record.owner_state, OwnerState::Observing)
            })
            .map(|(id, record)| {
                (
                    record.command_job.is_some(),
                    PendingResponseAge {
                        id: *id,
                        label: record.command_job.clone().unwrap_or(record.label.clone()),
                        registered_at_unix_ms: record.registered_at_unix_ms,
                    },
                )
            })
            .partition(|(job, _)| *job);
        running_jobs.sort_unstable_by_key(|(_, entry)| entry.id);
        pending_responses.sort_unstable_by_key(|(_, entry)| entry.id);
        WatchesOverview {
            watches,
            pending_responses: pending_responses
                .into_iter()
                .map(|(_, entry)| entry)
                .collect(),
            running_jobs: running_jobs.into_iter().map(|(_, entry)| entry).collect(),
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

    #[cfg(test)]
    pub(crate) fn reserve_labeled_with_reporting(
        &self,
        owner: ActorRef,
        target: ActorRef,
        label: String,
        notify_owner: bool,
    ) -> RequestId {
        let request = self.reserve_for_operation(owner, target, label, notify_owner, None);
        test_support::admit_destination(self, owner, request);
        request
    }

    pub(crate) fn reserve_for_operation(
        &self,
        owner: ActorRef,
        target: ActorRef,
        label: String,
        notify_owner: bool,
        reservation_owner: Option<RequestReservationOwner>,
    ) -> RequestId {
        let cleanup_owner = match &reservation_owner {
            Some(reservation @ RequestReservationOwner::Workbench { .. }) => {
                ResourceCleanupOwner::Invocation(reservation.clone())
            }
            Some(RequestReservationOwner::Scope(scope)) => ResourceCleanupOwner::Scope(*scope),
            _ => ResourceCleanupOwner::Actor,
        };
        self.reserve_for_cleanup_owner(
            owner,
            target,
            label,
            notify_owner,
            reservation_owner,
            cleanup_owner,
        )
    }

    /// The resource owner's admission gate must remain held until this
    /// reservation is registered. Construction provenance remains unchanged
    /// when cleanup lifetime later transfers to another owner.
    pub(crate) fn reserve_for_cleanup_owner(
        &self,
        owner: ActorRef,
        target: ActorRef,
        label: String,
        notify_owner: bool,
        reservation_owner: Option<RequestReservationOwner>,
        cleanup_owner: ResourceCleanupOwner,
    ) -> RequestId {
        self.state.lock().admit(RequestAdmission::Operation {
            owner,
            target,
            label,
            notify_owner,
            reservation_owner,
            cleanup_owner,
        })
    }

    /// Reserve the settlement of one command job. The record is owned and
    /// targeted by `owner`, is already running, and settles only through
    /// [`Self::settle_command`], so watches, routes and the settlement notice
    /// treat the job's completion exactly like a reply. A record reserved
    /// without an owner notice is being armed for a watch and is held until
    /// that watch registers or [`Self::release_command_holds`] refuses it.
    pub(crate) fn reserve_command_settlement(
        &self,
        owner: ActorRef,
        job: String,
        notify_owner: bool,
    ) -> RequestId {
        self.state
            .lock()
            .admit(RequestAdmission::CommandSettlement {
                owner,
                job,
                notify_owner,
            })
    }

    /// Settle a command job's completion with its rendered report and the
    /// commit it started at. A record already released or no longer
    /// observed by its owner (the owner stopped) is left as it is.
    pub(crate) fn settle_command(
        &self,
        request: RequestId,
        report: String,
        revision: Option<String>,
        value: tidepool_bridge_effects::CommandReport,
    ) -> Vec<WatchNotification> {
        let mut state = self.state.lock();
        state
            .settle(
                request,
                RequestSettlement::CommandComplete {
                    report,
                    revision,
                    value,
                },
            )
            .map_or_else(Vec::new, |effects| effects.notifications)
    }

    /// Serialize publishers across channel reservation without holding the
    /// request-state lock. Cancellation leaves unclaimed notices in the queue.
    pub(crate) async fn lock_settlement_publication(&self) -> tokio::sync::MutexGuard<'_, ()> {
        self.settlement_publication.lock().await
    }

    pub(crate) fn has_settlement_notifications(&self) -> bool {
        !self.state.lock().settlement_notifications.is_empty()
    }

    /// Called by the serialized publisher only after reserving channel capacity.
    /// Claim and permit delivery must have no intervening suspension point.
    pub(crate) fn take_next_settlement_notification(&self) -> Option<SettlementNotification> {
        self.state.lock().settlement_notifications.pop_front()
    }

    #[cfg(test)]
    pub(crate) fn take_settlement_notifications(&self) -> Vec<SettlementNotification> {
        self.state
            .lock()
            .settlement_notifications
            .drain(..)
            .collect()
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
        operation: &RequestReservationOwner,
    ) -> (Vec<RequestId>, Vec<WatchNotification>) {
        let mut state = self.state.lock();
        let mut aborted = state
            .requests
            .iter()
            .filter_map(|(request, record)| {
                (record.owner == owner
                    && record.reservation_owner.as_ref() == Some(operation)
                    && record.target_state == TargetState::Reserved)
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
        self.mark_queued_with_deadline(owner, target, request, None, None)
    }

    pub(crate) fn mark_queued_with_deadline(
        &self,
        owner: ActorRef,
        target: ActorRef,
        request: RequestId,
        deadline: Option<ActiveRequestDeadline>,
        submission_owner: Option<RequestReservationOwner>,
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
                if let Some(RequestReservationOwner::Workbench { execution, attempt }) =
                    submission_owner
                {
                    tracing::info!(target: "exomonad_actor::request",
                        request = request.0, parent_actor = %owner, activation_actor = %target,
                        parent_execution = %execution, parent_attempt = %attempt,
                        "request activation origin issued");
                    record.submission_origin = Some(RequestActivationOrigin {
                        request,
                        parent_actor: owner,
                        target,
                        execution,
                        attempt,
                    });
                }
                let received = state.received_requests.entry(target).or_default();
                *received = received.saturating_add(1);
                *state.cleanup_revision.entry(target).or_default() += 1;
                *state.cleanup_revision.entry(owner).or_default() += 1;
                Ok(())
            }
            _ => Err(ReplyError::AlreadySettled),
        }
    }

    #[cfg(test)]
    pub(crate) fn present(
        &self,
        target: ActorRef,
        request: RequestId,
    ) -> Result<Option<CancellationReason>, ReplyError> {
        self.present_with_progress_type(target, request, None)
            .map(|presentation| presentation.cancellation)
    }

    pub(crate) fn present_with_progress_type(
        &self,
        target: ActorRef,
        request: RequestId,
        progress_type: Option<
            std::sync::Arc<tidepool_toolchain::checked_cell::CanonicalInputTypeWitness>,
        >,
    ) -> Result<RequestPresentation, ReplyError> {
        let mut state = self.state.lock();
        let record = state.requests.get_mut(&request).ok_or(ReplyError::Stale)?;
        authorize_target(record, target)?;
        match record.target_state {
            TargetState::Queued => {
                record.progress_type = progress_type.into();
                record.target_state = TargetState::Presented;
                Ok(RequestPresentation {
                    cancellation: None,
                    origin: record.submission_origin.clone(),
                })
            }
            TargetState::CancellationRequested {
                presented: false,
                reason,
            } => {
                record.progress_type = progress_type.into();
                record.target_state = TargetState::CancellationRequested {
                    presented: true,
                    reason,
                };
                Ok(RequestPresentation {
                    cancellation: Some(reason),
                    origin: record.submission_origin.clone(),
                })
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

    #[cfg(test)]
    /// Check the immediate presentation gate without admitting native work.
    /// Publish staged activation work under the same arbitration as requester
    /// cancellation and deadline expiry. The callback must not block or await.
    pub(crate) fn publish_presented_request<R>(
        &self,
        target: ActorRef,
        request: RequestId,
        publish: impl FnOnce() -> R,
    ) -> Result<R, ReplyError> {
        let state = self.state.lock();
        let record = state.requests.get(&request).ok_or(ReplyError::Stale)?;
        authorize_target(record, target)?;
        match record.target_state {
            TargetState::Presented => Ok(publish()),
            TargetState::CancellationRequested { .. }
            | TargetState::AcknowledgingCancellation(_) => Err(ReplyError::CancellationRequested),
            TargetState::Closed => Err(ReplyError::AlreadySettled),
            TargetState::Reserved | TargetState::Queued | TargetState::Settling => {
                Err(ReplyError::Stale)
            }
        }
    }

    /// Admission binds result ownership before the payload can enter a mailbox.
    /// Cleanup handoff changes `owner`, never this original issuer destination.
    pub(crate) fn admit_result_destination(
        &self,
        owner: ActorRef,
        request: RequestId,
        destination: RequestResultDestination,
    ) -> Result<(), ReplyError> {
        let mut state = self.state.lock();
        let record = state.requests.get_mut(&request).ok_or(ReplyError::Stale)?;
        authorize_owner(record, owner)?;
        if destination.issuer() != owner {
            return Err(identity_error(owner, destination.issuer()));
        }
        if record.target_state != TargetState::Reserved
            || record.result_destination.is_some()
            || record.command_job.is_some()
        {
            return Err(ReplyError::Stale);
        }
        record.result_destination = Some(Arc::new(destination));
        Ok(())
    }

    pub(crate) fn begin_reply(
        &self,
        target: ActorRef,
        request: RequestId,
    ) -> Result<RequestReplyClaim, ReplyError> {
        let mut state = self.state.lock();
        let generation = state
            .next_reply_claim
            .checked_add(1)
            .ok_or(ReplyError::Stale)?;
        state.next_reply_claim = generation;
        let record = state.requests.get_mut(&request).ok_or(ReplyError::Stale)?;
        authorize_target(record, target)?;
        match record.target_state {
            TargetState::Presented => {
                if record
                    .activation
                    .as_mut()
                    .is_some_and(activation::ActivationRecord::fences_settlement)
                    || record
                        .updates
                        .iter()
                        .any(updates::UpdateRecord::fences_settlement)
                {
                    return Err(ReplyError::UpdatePending);
                }
                let destination = record
                    .result_destination
                    .clone()
                    .ok_or(ReplyError::ReplyResultUnavailable)?;
                record.target_state = TargetState::Settling;
                record.reply_claim = Some(generation);
                record.publish_source_closure();
                record.progress = None;
                Ok(RequestReplyClaim {
                    request,
                    target,
                    generation,
                    destination,
                })
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
        claim: RequestReplyClaim,
        value: Arc<OwnedResultSnapshot>,
        reply_preview: Option<String>,
    ) -> Vec<WatchNotification> {
        let mut state = self.state.lock();
        state
            .settle(
                claim.request,
                RequestSettlement::ReplyComplete {
                    claim,
                    value,
                    preview: reply_preview,
                },
            )
            .map_or_else(Vec::new, |effects| effects.notifications)
    }

    pub(crate) fn fail_reply_settlement(
        &self,
        request: RequestId,
        detail: impl Into<String>,
    ) -> Vec<WatchNotification> {
        let mut state = self.state.lock();
        state
            .settle(request, RequestSettlement::ReplyFailed(detail.into()))
            .map_or_else(Vec::new, |effects| effects.notifications)
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
            OwnerState::Ready(_) => ResponseObservation::Ready,
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

    /// Borrowed observation returns the same canonical root on every read.
    /// A released handle grants no new observation, even if a watch retains it.
    pub(crate) fn observe_response_result(
        &self,
        _owner: ActorRef,
        request: RequestId,
    ) -> Result<Arc<OwnedResultSnapshot>, ReplyError> {
        let state = self.state.lock();
        let record = state.requests.get(&request).ok_or(ReplyError::Stale)?;
        match &record.owner_state {
            OwnerState::Ready(RequestSuccess::Typed(snapshot)) => Ok(Arc::clone(snapshot)),
            _ => Err(ReplyError::ReplyResultUnavailable),
        }
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
        // Releasing the requester's wait does not settle the target. An
        // abandoned response still needs cancellation and a deadline may
        // already have requested it without receiving acknowledgment.
        if record.target_state == TargetState::Closed {
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
        (notification, state.reevaluate_watches())
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
            .activation
            .as_mut()
            .is_some_and(activation::ActivationRecord::fences_settlement)
            || record
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
        state
            .settle(request, RequestSettlement::CancellationAcknowledged)
            .map_or_else(Vec::new, |effects| effects.notifications)
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
            OwnerState::Ready(_) | OwnerState::Unavailable(_) => {
                AbandonResponseOutcome::AlreadyTerminal
            }
        };
        let notifications = state.reevaluate_watches();
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

    #[cfg(test)]
    pub(crate) fn register_watch_requirement_groups(
        &self,
        owner: ActorRef,
        label: String,
        groups: Vec<Vec<(RequestId, WatchRequirement)>>,
    ) -> Result<(WatchId, Vec<WatchNotification>), ReplyError> {
        self.register_watch_plan(owner, label, test_readiness_groups(groups))
    }

    pub(crate) fn register_watch_plan(
        &self,
        owner: ActorRef,
        label: String,
        plan: readiness::Plan<ReadinessDependency>,
    ) -> Result<(WatchId, Vec<WatchNotification>), ReplyError> {
        self.register_watch_plan_with_route(owner, label, plan, None)
    }

    pub(crate) fn register_watch_plan_with_route(
        &self,
        owner: ActorRef,
        label: String,
        plan: readiness::Plan<ReadinessDependency>,
        route: Option<routes::WatchRoute>,
    ) -> Result<(WatchId, Vec<WatchNotification>), ReplyError> {
        self.register_watch_plan_owned(owner, label, plan, route, false)
    }

    pub(crate) fn register_transient_watch(
        &self,
        owner: ActorRef,
        plan: readiness::Plan<ReadinessDependency>,
    ) -> Result<WatchId, ReplyError> {
        self.register_watch_plan_owned(owner, "await".into(), plan, None, true)
            .map(|(watch, _)| watch)
    }

    fn register_watch_plan_owned(
        &self,
        owner: ActorRef,
        label: String,
        plan: readiness::Plan<ReadinessDependency>,
        route: Option<routes::WatchRoute>,
        transient: bool,
    ) -> Result<(WatchId, Vec<WatchNotification>), ReplyError> {
        let plan = readiness::Plan::checked(plan.nodes, plan.root)?;
        let dependencies = plan
            .leaves()
            .filter_map(|(node, dependency)| match dependency {
                ReadinessDependency::Request(request, requirement) => Some(WatchDependency {
                    request: *request,
                    requirement: *requirement,
                    node,
                }),
                ReadinessDependency::Watch(_) => None,
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
        for (_, dependency) in plan.leaves() {
            if let ReadinessDependency::Watch(source) = dependency {
                let record = state.watches.get(source).ok_or(ReplyError::Stale)?;
                if state.cleaning.contains(&record.owner) {
                    return Err(ReplyError::CancellationRequested);
                }
                touched.insert(record.owner);
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
        let named = dependencies
            .iter()
            .map(|dependency| dependency.request)
            .collect::<Vec<_>>();
        state.watches.insert(
            id,
            WatchRecord {
                transient,
                route,
                owner,
                label,
                dependencies,
                evaluation: readiness::Evaluation::new(&plan),
                plan,
                snapshot: None,
                sources: HashMap::new(),
                state: WatchState::Pending,
                progress: HashMap::new(),
                commands: HashMap::new(),
                responses: HashMap::new(),
                observed_ready_at: None,
                registered_at_unix_ms: unix_time_ms(),
                transitioned_at_unix_ms: None,
                waiters: HashMap::new(),
            },
        );
        for request in named {
            if let Some(record) = state.requests.get_mut(&request) {
                record.held_for_watch = false;
            }
        }
        let notifications = state.reevaluate_watches();
        Ok((id, notifications))
    }

    #[cfg(test)]
    pub(crate) fn observe_watch_progress(
        &self,
        owner: ActorRef,
        watch: WatchId,
        request: RequestId,
        after: u64,
    ) -> Result<(Option<ProgressSnapshot>, bool), ReplyError> {
        self.observe_watch_snapshot_progress(owner, watch, &[], request, after)
    }

    pub(crate) fn observe_watch_snapshot_progress(
        &self,
        _owner: ActorRef,
        watch: WatchId,
        path: &[usize],
        request: RequestId,
        after: u64,
    ) -> Result<(Option<ProgressSnapshot>, bool), ReplyError> {
        let state = self.state.lock();
        match snapshot_at(&state, watch, path)?
            .progress
            .get(&(request, after))
        {
            Some(ProgressCapture::Update(snapshot)) => Ok((Some(snapshot.clone()), false)),
            Some(ProgressCapture::Closed) => Ok((None, true)),
            None => Err(ReplyError::Unauthorized),
        }
    }

    #[cfg(test)]
    pub(crate) fn observe_watch_command(
        &self,
        watch: WatchId,
        job: &str,
    ) -> Result<Option<tidepool_bridge_effects::CommandReport>, ReplyError> {
        self.observe_watch_snapshot_command(watch, &[], job)
    }

    pub(crate) fn observe_watch_snapshot_command(
        &self,
        watch: WatchId,
        path: &[usize],
        job: &str,
    ) -> Result<Option<tidepool_bridge_effects::CommandReport>, ReplyError> {
        let state = self.state.lock();
        Ok(snapshot_at(&state, watch, path)?
            .commands
            .values()
            .find_map(|(captured_job, report)| (captured_job == job).then(|| report.clone())))
    }

    pub(crate) fn observe_watch_snapshot_response(
        &self,
        watch: WatchId,
        path: &[usize],
        node: usize,
    ) -> Result<Arc<OwnedResultSnapshot>, ReplyError> {
        let state = self.state.lock();
        snapshot_at(&state, watch, path)?
            .responses
            .get(&node)
            .cloned()
            .ok_or(ReplyError::ReplyResultUnavailable)
    }

    pub(crate) fn observe_watch_snapshot_decision(
        &self,
        watch: WatchId,
        path: &[usize],
    ) -> Result<readiness::Decision, ReplyError> {
        let state = self.state.lock();
        Ok(snapshot_at(&state, watch, path)?.decision.clone())
    }

    pub(crate) fn observe_watch(
        &self,
        owner: ActorRef,
        watch: WatchId,
    ) -> Result<WatchObservation, ReplyError> {
        let mut state = self.state.lock();
        state.watches.get(&watch).ok_or(ReplyError::Stale)?;
        observe_watch_locked(&mut state, owner, watch)
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
        wake_watch_waiters(state.watches.get_mut(&watch).expect("watch checked"));
        state.watches.remove(&watch);
        release_settled_commands(&mut state);
        Ok(ForgetWatchOutcome::Forgotten)
    }

    pub(crate) fn is_transient_watch(&self, owner: ActorRef, watch: WatchId) -> bool {
        self.state
            .lock()
            .watches
            .get(&watch)
            .is_some_and(|record| record.owner == owner && record.transient)
    }

    /// Claim the owner's readiness wake only after cancellation and retirement
    /// chose this direct wait's successful resume. A cancelled waiter must leave
    /// the original reporting policy available to the producer.
    pub(crate) fn claim_transient_watch_wake(
        &self,
        owner: ActorRef,
        watch: WatchId,
    ) -> Result<(), ReplyError> {
        let mut state = self.state.lock();
        let record = state.watches.get(&watch).ok_or(ReplyError::Stale)?;
        if record.owner != owner {
            return Err(identity_error(record.owner, owner));
        }
        if !record.transient || record.state == WatchState::Pending {
            return Err(ReplyError::Stale);
        }
        let requests = record
            .dependencies
            .iter()
            .filter_map(|dependency| {
                matches!(dependency.requirement, WatchRequirement::Response { .. })
                    .then_some(dependency.request)
            })
            .collect::<Vec<_>>();
        for request in requests {
            if let Some(record) = state
                .requests
                .get_mut(&request)
                .filter(|record| record.owner == owner)
            {
                record.settlement_notified = true;
            }
        }
        Ok(())
    }

    /// Drop only the invocation's subscription, including a pending one.
    /// The producing request and its cancellation/cleanup state are untouched.
    pub(crate) fn release_transient_watch(
        &self,
        owner: ActorRef,
        watch: WatchId,
    ) -> Result<Vec<WatchNotification>, ReplyError> {
        let mut state = self.state.lock();
        let record = state.watches.get(&watch).ok_or(ReplyError::Stale)?;
        if record.owner != owner {
            return Err(identity_error(record.owner, owner));
        }
        if !record.transient {
            return Err(ReplyError::Unauthorized);
        }
        wake_watch_waiters(state.watches.get_mut(&watch).expect("watch checked"));
        state.watches.remove(&watch);
        let notifications = state.reevaluate_watches();
        release_settled_commands(&mut state);
        Ok(notifications)
    }

    /// Hold an existing command record for a watch about to register, so
    /// it is not released in between. False when the record was already
    /// released: its job then re-arms a fresh, already settled record.
    pub(crate) fn hold_command(&self, request: RequestId) -> bool {
        let mut state = self.state.lock();
        match state.requests.get_mut(&request) {
            Some(record) if record.command_job.is_some() => {
                record.held_for_watch = true;
                true
            }
            _ => false,
        }
    }

    /// Arm the owner's completion notice for an existing command settlement.
    /// An owner response watch keeps its wake instead. `None` means the record
    /// was already released and the job must rearm from its retained report.
    pub(crate) fn notify_command_owner(
        &self,
        request: RequestId,
    ) -> Option<Vec<WatchNotification>> {
        let mut state = self.state.lock();
        let record = state.requests.get(&request)?;
        if record.command_job.is_none() {
            return None;
        }
        if let Some(record) = state.requests.get_mut(&request) {
            record.notify_owner = true;
        }
        let notifications = state.reevaluate_watches();
        release_settled_commands(&mut state);
        Some(notifications)
    }

    /// A watch registration that armed these command records was refused:
    /// drop their holds so they are released once settled and unwatched.
    pub(crate) fn release_command_holds(&self, requests: &[RequestId]) {
        let mut state = self.state.lock();
        for request in requests {
            if let Some(record) = state.requests.get_mut(request) {
                record.held_for_watch = false;
            }
        }
        release_settled_commands(&mut state);
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
                record.command_job.is_none()
                    && (record.target == actor
                        || (record.owner == actor
                            && record.cleanup_owner != ResourceCleanupOwner::Run
                            && record.target_state != TargetState::Closed))
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
        state.watches.retain(|_, record| {
            if record.owner == actor {
                wake_watch_waiters(record);
                false
            } else {
                true
            }
        });
        let released = state
            .requests
            .iter()
            .filter_map(|(request, record)| {
                (record.owner == actor && record.cleanup_owner != ResourceCleanupOwner::Run)
                    .then_some(*request)
            })
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
            if record.command_job.is_some() {
                if record.owner == actor && record.owner_state == OwnerState::Observing {
                    record.owner_state = OwnerState::Unavailable(ResponseFailure::RequesterStopped);
                }
            } else if record.target == actor {
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
        state.reevaluate_watches()
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
        state.reevaluate_watches()
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

#[cfg(test)]
pub(crate) fn test_readiness_groups(
    groups: Vec<Vec<(RequestId, WatchRequirement)>>,
) -> readiness::Plan<ReadinessDependency> {
    use readiness::{Node, Plan};
    let mut nodes = Vec::new();
    let mut root = None;
    for group in groups {
        let mut group_root = None;
        for dependency in group {
            let leaf = nodes.len();
            nodes.push(Node::Leaf(ReadinessDependency::Request(
                dependency.0,
                dependency.1,
            )));
            group_root = Some(match group_root {
                None => leaf,
                Some(left) => {
                    let index = nodes.len();
                    nodes.push(Node::Either(left, leaf));
                    index
                }
            });
        }
        let group_root = group_root.unwrap_or_else(|| {
            let index = nodes.len();
            nodes.push(Node::Ready);
            index
        });
        root = Some(match root {
            None => group_root,
            Some(left) => {
                let index = nodes.len();
                nodes.push(Node::All(left, group_root));
                index
            }
        });
    }
    let root = root.unwrap_or_else(|| {
        nodes.push(Node::Ready);
        0
    });
    Plan::checked(nodes, root).expect("test readiness expression is valid")
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
        OwnerState::Ready(_) | OwnerState::Unavailable(_) | OwnerState::Abandoned
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

/// Remove closed command records that no watch names. Their settlement
/// notice, if any, is already queued with its full text; the job's report
/// stays with the job.
fn release_settled_commands(state: &mut RequestStateTable) {
    let released = state
        .requests
        .iter()
        .filter(|(request, record)| {
            record.command_job.is_some()
                && !record.held_for_watch
                && record.target_state == TargetState::Closed
                && !state.watches.values().any(|watch| {
                    watch
                        .dependencies
                        .iter()
                        .any(|dependency| dependency.request == **request)
                })
        })
        .map(|(request, _)| *request)
        .collect::<Vec<_>>();
    for request in released {
        state.requests.remove(&request);
    }
}

fn release_request_record(
    state: &mut RequestStateTable,
    request: RequestId,
) -> Vec<WatchNotification> {
    let notifications = invalidate_response_watches(state, request);
    if let Some(mut record) = state.requests.remove(&request) {
        record.publish_source_release();
    }
    let mut notifications = notifications;
    notifications.extend(state.reevaluate_watches());
    notifications
}

fn invalidate_response_watches(
    state: &mut RequestStateTable,
    request: RequestId,
) -> Vec<WatchNotification> {
    // Successful leaf facts and terminal decisions are already retained by
    // their watch. Release only changes leaves not yet captured; evaluation
    // handles that failure within its own expression branch.
    for watch in state.watches.values_mut() {
        if watch.state != WatchState::Pending {
            continue;
        }
        for dependency in &watch.dependencies {
            if dependency.request == request {
                // Progress captures remain immutable; an uncaptured closed
                // stream has a terminal observation even after source release.
                if let WatchRequirement::ProgressAfter(after) = dependency.requirement {
                    watch
                        .progress
                        .entry((request, after))
                        .or_insert(ProgressCapture::Closed);
                }
            }
        }
    }
    Vec::new()
}

fn snapshot_at<'a>(
    state: &'a RequestStateTable,
    watch: WatchId,
    path: &[usize],
) -> Result<&'a WatchSnapshot, ReplyError> {
    let record = state.watches.get(&watch).ok_or(ReplyError::Stale)?;
    let mut snapshot = record.snapshot.as_deref().ok_or(ReplyError::Stale)?;
    for node in path {
        snapshot = snapshot
            .sources
            .get(node)
            .ok_or(ReplyError::Unauthorized)?
            .as_ref();
    }
    Ok(snapshot)
}

fn observe_watch_locked(
    state: &mut RequestStateTable,
    owner: ActorRef,
    watch: WatchId,
) -> Result<WatchObservation, ReplyError> {
    let record = state.watches.get(&watch).ok_or(ReplyError::Stale)?;
    let observation = match &record.state {
        WatchState::Pending => WatchObservation::Pending(PendingProgress {
            actor_terminal: None,
            provider_turn: None,
            last_activity_unix_ms: None,
            progress_revision: first_pending_dependency(state, record)
                .and_then(|request| state.requests.get(&request))
                .and_then(|dependency_record| dependency_record.progress.as_ref())
                .map(|snapshot| snapshot.revision),
            watched: true,
        }),
        WatchState::Rejected(error) => WatchObservation::Rejected(*error),
        WatchState::Ready => WatchObservation::Ready(
            record
                .snapshot
                .as_ref()
                .expect("ready watch retains its snapshot")
                .decision
                .clone(),
        ),
        WatchState::Unavailable { request, failure } => WatchObservation::Unavailable {
            request: *request,
            failure: failure.clone(),
        },
    };
    if matches!(
        observation,
        WatchObservation::Ready(_)
            | WatchObservation::Unavailable { .. }
            | WatchObservation::Rejected(_)
    ) {
        if let Some(record) = state
            .watches
            .get_mut(&watch)
            .filter(|record| record.owner == owner)
        {
            record.observed_ready_at = Some(unix_time_ms());
        }
    }
    Ok(observation)
}

fn wake_watch_waiters(watch: &mut WatchRecord) {
    for (_, waiter) in std::mem::take(&mut watch.waiters) {
        let _ = waiter.send(());
    }
}

fn queue_settlement_notifications(state: &mut RequestStateTable) {
    let mut settlements = Vec::new();
    let watches = &state.watches;
    for (request, record) in &mut state.requests {
        record.publish_source_closure();
        if record.owner_state != OwnerState::Observing || record.target_state == TargetState::Closed
        {
            record.progress = None;
        }
        if record.notify_owner && !record.settlement_notified {
            let transition = match &record.owner_state {
                OwnerState::Ready(_) => Some(SettlementTransition::Ready),
                OwnerState::Unavailable(failure) => {
                    Some(SettlementTransition::Unavailable(failure.clone()))
                }
                OwnerState::Observing | OwnerState::Abandoned => None,
            };
            if let Some(transition) = transition {
                let (subscribed, named) = watches
                    .values()
                    .filter(|watch| {
                        watch.owner == record.owner
                            && watch.dependencies.iter().any(|dependency| {
                                dependency.request == *request
                                    && matches!(
                                        dependency.requirement,
                                        WatchRequirement::Response { .. }
                                    )
                            })
                    })
                    .fold((false, false), |(_, named), watch| {
                        (true, named || !watch.transient)
                    });
                if subscribed {
                    // A named watch emits its retained wake below. A direct
                    // wait claims only after its cancellation gate chooses resume.
                    if named {
                        record.settlement_notified = true;
                    }
                    continue;
                }
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
                    record.command_job.clone(),
                ));
            }
        }
    }
    for (
        owner,
        request,
        label,
        transition,
        reply_preview,
        target_path,
        target_revision,
        command_job,
    ) in settlements
    {
        let sequence = next_event_sequence(state, owner);
        state
            .settlement_notifications
            .push_back(SettlementNotification {
                owner,
                request,
                label,
                transition,
                reply_preview,
                target_path,
                target_revision,
                command_job,
                occurred_at_unix_ms: unix_time_ms(),
                sequence,
                watermark: sequence,
            });
    }
}

impl RequestStateTable {
    fn reevaluate_watches(&mut self) -> Vec<WatchNotification> {
        for record in self.requests.values_mut() {
            // A pending target still owes native settlement after its owner
            // stops. An accepted reply's affine claim holds the admission pin
            // through incorporation; terminal roots retain only their custody.
            if record.target_state == TargetState::Closed
                || (record.reply_claim.is_some()
                    && matches!(record.owner_state, OwnerState::Unavailable(_)))
            {
                record.result_destination = None;
            }
        }
        queue_settlement_notifications(self);
        let mut notifications = Vec::new();
        // A source must already exist at admission, so its monotonic ID is
        // earlier than its dependent. Evaluate that DAG in publication order
        // under the same lock, then retain one coherent immutable snapshot.
        let watch_ids = self.watches.keys().copied().collect::<Vec<_>>();
        for watch_id in watch_ids {
            let mut watch = self.watches.remove(&watch_id).expect("listed watch exists");
            if watch.state == WatchState::Pending {
                if let Some(notification) = self.evaluate_watch(watch_id, &mut watch) {
                    notifications.push(notification);
                }
            }
            self.watches.insert(watch_id, watch);
        }
        for notification in &mut notifications {
            let sequence = next_event_sequence(self, notification.owner);
            notification.sequence = sequence;
            notification.watermark = sequence;
        }
        notifications
    }

    fn evaluate_watch(
        &self,
        watch_id: WatchId,
        watch: &mut WatchRecord,
    ) -> Option<WatchNotification> {
        let state = self;
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
        for dependency in &watch.dependencies {
            if let Some(record) = state.requests.get(&dependency.request) {
                match &record.owner_state {
                    OwnerState::Ready(RequestSuccess::Command(report)) => {
                        if let Some(job) = &record.command_job {
                            watch
                                .commands
                                .entry(dependency.node)
                                .or_insert_with(|| (job.clone(), report.clone()));
                        }
                    }
                    OwnerState::Ready(RequestSuccess::Typed(snapshot)) => {
                        if matches!(dependency.requirement, WatchRequirement::Response { .. }) {
                            watch
                                .responses
                                .entry(dependency.node)
                                .or_insert_with(|| Arc::clone(snapshot));
                        }
                    }
                    _ => {}
                }
            }
        }
        let outcome = watch.evaluation.advance(&watch.plan, |node, dependency| {
            use readiness::LeafState;
            let (request, requirement) = match *dependency {
                ReadinessDependency::Request(request, requirement) => (request, requirement),
                ReadinessDependency::Watch(source) => {
                    if watch.sources.contains_key(&node) {
                        return LeafState::Ready;
                    }
                    return match state.watches.get(&source) {
                        Some(source) => match &source.state {
                            WatchState::Pending => LeafState::Pending,
                            WatchState::Ready => {
                                watch.sources.insert(
                                    node,
                                    source
                                        .snapshot
                                        .clone()
                                        .expect("ready source retains snapshot"),
                                );
                                LeafState::Ready
                            }
                            WatchState::Unavailable { request, failure } => {
                                LeafState::Failed(*request, failure.clone())
                            }
                            WatchState::Rejected(error) => LeafState::Rejected(*error),
                        },
                        None => LeafState::Rejected(ReplyError::Stale),
                    };
                }
            };
            if let WatchRequirement::ProgressAfter(after) = requirement {
                return if watch.progress.contains_key(&(request, after)) {
                    LeafState::Ready
                } else {
                    LeafState::Pending
                };
            }
            let failure = match state
                .requests
                .get(&request)
                .map(|record| &record.owner_state)
            {
                Some(OwnerState::Ready(_)) => return LeafState::Ready,
                Some(OwnerState::Observing) => return LeafState::Pending,
                Some(OwnerState::Unavailable(failure)) => failure.clone(),
                Some(OwnerState::Abandoned) => ResponseFailure::Abandoned,
                None => ResponseFailure::Released,
            };
            if requirement
                == (WatchRequirement::Response {
                    allow_failure: true,
                })
            {
                LeafState::SettledFailure(failure)
            } else {
                LeafState::Failed(request, failure)
            }
        });
        let next_state = outcome.map(|outcome| match outcome {
            readiness::Outcome::Ready(decision) => {
                let selected = decision
                    .leaves
                    .iter()
                    .map(|(node, _)| *node)
                    .collect::<std::collections::HashSet<_>>();
                watch.progress.retain(|&(request, after), _| {
                    watch.dependencies.iter().any(|dependency| {
                        selected.contains(&dependency.node)
                            && dependency.request == request
                            && dependency.requirement == WatchRequirement::ProgressAfter(after)
                    })
                });
                watch.commands.retain(|node, _| selected.contains(node));
                watch.responses.retain(|node, _| selected.contains(node));
                watch.sources.retain(|node, _| selected.contains(node));
                watch.snapshot = Some(std::sync::Arc::new(WatchSnapshot {
                    decision,
                    progress: std::mem::take(&mut watch.progress),
                    commands: std::mem::take(&mut watch.commands),
                    responses: std::mem::take(&mut watch.responses),
                    sources: std::mem::take(&mut watch.sources),
                }));
                WatchState::Ready
            }
            readiness::Outcome::Rejected(error) => {
                watch.progress.clear();
                watch.commands.clear();
                watch.responses.clear();
                watch.sources.clear();
                WatchState::Rejected(error)
            }
            readiness::Outcome::Failed(request, failure) => {
                watch.progress.clear();
                watch.commands.clear();
                watch.responses.clear();
                watch.sources.clear();
                WatchState::Unavailable { request, failure }
            }
        });
        let Some(next_state) = next_state else {
            return None;
        };
        let (transition, current) = match &next_state {
            WatchState::Pending => return None,
            WatchState::Ready => (WatchTransition::Ready, WatchStateProjection::Ready),
            WatchState::Rejected(error) => (
                WatchTransition::Rejected(*error),
                WatchStateProjection::Rejected(*error),
            ),
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
        wake_watch_waiters(watch);
        let occurred_at_unix_ms = unix_time_ms();
        watch.transitioned_at_unix_ms = Some(occurred_at_unix_ms);
        if watch.transient {
            return None;
        }
        if let Some(route) = &mut watch.route {
            route.schedule(watch_id);
            return None;
        }
        let label = watch.label.clone();
        let owner = watch.owner;
        let watch = watch_id;
        Some(WatchNotification {
            owner,
            watch,
            label,
            previous: WatchStateProjection::Pending,
            current,
            transition,
            occurred_at_unix_ms,
            sequence: ActorEventSequence(0),
            watermark: ActorEventSequence(0),
        })
    }
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
    fn open_without_reply_is_the_presented_request_until_a_reply_begins() {
        let registry = RequestRegistry::default();
        let owner = actor(1);
        let target = actor(2);
        let request = registry.reserve(owner, target);
        assert_eq!(registry.open_without_reply(target), None);
        registry.mark_queued(owner, target, request).unwrap();
        assert_eq!(registry.open_without_reply(target), None);
        registry.present(target, request).unwrap();
        assert_eq!(registry.open_without_reply(target), Some(request));
        assert_eq!(registry.open_without_reply(owner), None);

        let grandchild = registry.reserve(target, actor(3));
        assert_eq!(registry.open_without_reply(target), None);
        registry.mark_target_unavailable(target, grandchild);
        assert_eq!(registry.open_without_reply(target), Some(request));

        let mut reply_claim_request = Some(registry.begin_reply(target, request).unwrap());
        assert_eq!(registry.open_without_reply(target), None);
        crate::request::test_support::complete_optional_reply(
            &registry,
            &mut reply_claim_request,
            None,
        );
        assert_eq!(registry.open_without_reply(target), None);
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
        let mut reply_claim_ready = Some(registry.begin_reply(ready_target, ready).unwrap());
        crate::request::test_support::complete_optional_reply(
            &registry,
            &mut reply_claim_ready,
            None,
        );

        let silent =
            registry.reserve_labeled_with_reporting(owner, silent_target, "silent".into(), false);
        registry.mark_queued(owner, silent_target, silent).unwrap();
        registry.present(silent_target, silent).unwrap();
        let mut reply_claim_silent = Some(registry.begin_reply(silent_target, silent).unwrap());
        crate::request::test_support::complete_optional_reply(
            &registry,
            &mut reply_claim_silent,
            None,
        );

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
    fn command_settlement_notifies_its_owner_and_is_never_target_work() {
        let registry = RequestRegistry::default();
        let owner = actor(1);
        let notified = registry.reserve_command_settlement(owner, "job-a".into(), true);
        let watched = registry.reserve_command_settlement(owner, "job-b".into(), false);
        let stopped = registry.reserve_command_settlement(owner, "job-c".into(), true);

        // A running job is not work presented to its owner, and does not hold
        // the owner's retirement.
        assert!(registry.active_for_target(owner).is_empty());
        assert_eq!(registry.work_for_target(owner), (Vec::new(), Vec::new()));
        let owners = std::collections::HashSet::from([owner]);
        assert_eq!(
            registry.campaign_cleanup_blockers(&owners, &owners),
            (Vec::new(), Vec::new())
        );

        let (watch, _) = registry
            .register_watch_requirement_groups(
                owner,
                "job-b-done".into(),
                vec![vec![(
                    watched,
                    WatchRequirement::Response {
                        allow_failure: false,
                    },
                )]],
            )
            .unwrap();
        let ready = registry.settle_command(
            watched,
            "job-b report".into(),
            None,
            crate::request::test_support::command_report(),
        );
        assert_eq!(ready.len(), 1);
        assert_eq!(ready[0].watch, watch);
        assert_eq!(ready[0].transition, WatchTransition::Ready);

        registry.settle_command(
            notified,
            "job-a report".into(),
            Some("abc123".into()),
            crate::request::test_support::command_report(),
        );
        // Settling twice keeps the first report.
        assert!(registry
            .settle_command(
                notified,
                "later".into(),
                None,
                crate::request::test_support::command_report()
            )
            .is_empty());
        let notices = registry.take_settlement_notifications();
        assert_eq!(
            notices.len(),
            1,
            "the watched job's wake belongs to its watch"
        );
        assert_eq!(notices[0].request, notified);
        assert_eq!(notices[0].command_job.as_deref(), Some("job-a"));
        assert_eq!(notices[0].reply_preview.as_deref(), Some("job-a report"));
        assert_eq!(notices[0].target_revision.as_deref(), Some("abc123"));

        // Settled and unwatched: released once its notice exists. The watched
        // record stays until its watch is forgotten.
        assert_eq!(
            registry.observe_response(owner, notified),
            Err(ReplyError::Stale)
        );
        assert_eq!(
            registry.observe_response(owner, watched),
            Ok(ResponseObservation::Ready)
        );
        registry.observe_watch(owner, watch).unwrap();
        assert_eq!(
            registry.forget_watch(owner, watch),
            Ok(ForgetWatchOutcome::Forgotten)
        );
        assert_eq!(
            registry.observe_response(owner, watched),
            Err(ReplyError::Stale)
        );

        // The owner retiring mid-job ends observation as the requester, not a
        // target; the job's later completion settles nothing and panics nowhere.
        registry.actor_stopped(
            owner,
            &ActorTerminal {
                kind: ActorExitKind::Completed,
                summary: String::new(),
                diagnostic: None,
            },
        );
        assert_eq!(
            registry.observe_response(owner, stopped),
            Ok(ResponseObservation::Unavailable(
                ResponseFailure::RequesterStopped
            ))
        );
        assert!(registry
            .settle_command(
                stopped,
                "after stop".into(),
                None,
                crate::request::test_support::command_report()
            )
            .is_empty());
        let notices = registry.take_settlement_notifications();
        assert!(notices.iter().all(|notice| notice.transition
            == SettlementTransition::Unavailable(ResponseFailure::RequesterStopped)));
        assert_eq!(
            registry.observe_response(owner, stopped),
            Err(ReplyError::Stale)
        );
    }

    #[test]
    fn command_handoff_upgrades_an_unwatched_settlement_without_duplicating_notice() {
        let registry = RequestRegistry::default();
        let owner = actor(1);
        let request = registry.reserve_command_settlement(owner, "job".into(), false);
        registry.release_command_holds(&[request]);
        assert!(registry.notify_command_owner(request).is_some());
        registry.settle_command(
            request,
            "finished".into(),
            None,
            crate::request::test_support::command_report(),
        );
        assert_eq!(registry.take_settlement_notifications().len(), 1);
        // A later handoff can rearm from the retained job report; it cannot
        // emit a second notice from this released request.
        assert!(registry.notify_command_owner(request).is_none());

        let raced = registry.reserve_command_settlement(owner, "raced".into(), false);
        registry.settle_command(
            raced,
            "already finished".into(),
            None,
            crate::request::test_support::command_report(),
        );
        assert!(registry.notify_command_owner(raced).is_some());
        assert!(registry.notify_command_owner(raced).is_some());
        let notices = registry.take_settlement_notifications();
        assert_eq!(notices.len(), 1);
        assert_eq!(
            notices[0].reply_preview.as_deref(),
            Some("already finished")
        );
    }

    #[test]
    fn running_commands_and_foreign_command_watches_do_not_mute_the_reply_reminder() {
        let registry = RequestRegistry::default();
        let parent = actor(1);
        let owner = actor(2);
        let foreign = actor(3);
        let request = registry.reserve(parent, owner);
        registry.mark_queued(parent, owner, request).unwrap();
        registry.present(owner, request).unwrap();
        assert_eq!(registry.open_without_reply(owner), Some(request));

        // A background job (say a dev server) is not a wait.
        let server = registry.reserve_command_settlement(owner, "server".into(), true);
        assert_eq!(registry.open_without_reply(owner), Some(request));
        // Another actor watching the owner's job arms a record the owner owns;
        // it changes nothing for the owner either.
        let armed = registry.reserve_command_settlement(owner, "check".into(), false);
        registry
            .register_watch_requirement_groups(
                foreign,
                "check-done".into(),
                vec![vec![
                    (
                        armed,
                        WatchRequirement::Response {
                            allow_failure: false,
                        },
                    ),
                    (
                        server,
                        WatchRequirement::Response {
                            allow_failure: false,
                        },
                    ),
                ]],
            )
            .unwrap();
        assert_eq!(registry.open_without_reply(owner), Some(request));
        assert_eq!(registry.open_without_reply(foreign), None);
        assert!(registry.status_for(owner).pending_responses.is_empty());
        assert_eq!(
            registry.status_for(owner).running_jobs,
            vec!["check".to_owned(), "server".to_owned()]
        );
        let overview = registry.watches_overview(owner);
        assert!(overview.pending_responses.is_empty());
        assert_eq!(overview.running_jobs.len(), 2);
        // Campaign cleanup reports no job as a forgotten response.
        let owners = std::collections::HashSet::from([owner]);
        let outcome = registry.cleanup_campaign_metadata(owner, &owners);
        assert!(!outcome.forgotten_responses.contains(&armed));
        assert!(!outcome.pending_responses.contains(&armed));
    }

    #[test]
    fn refused_watch_releases_its_command_hold_and_a_released_record_can_be_held_again() {
        let registry = RequestRegistry::default();
        let owner = actor(1);
        let armed = registry.reserve_command_settlement(owner, "job".into(), false);
        // Held for its watch: settling does not release it.
        registry.settle_command(
            armed,
            "report".into(),
            None,
            crate::request::test_support::command_report(),
        );
        assert!(registry.hold_command(armed));
        registry.release_command_holds(&[armed]);
        assert_eq!(
            registry.observe_response(owner, armed),
            Err(ReplyError::Stale)
        );
        assert!(!registry.hold_command(armed));
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
    fn observed_nested_choice_keeps_the_source_decision_after_source_forget() {
        use readiness::{Node, Plan};
        let registry = RequestRegistry::default();
        let owner = actor(1);
        let left = registry.reserve(owner, actor(2));
        let right = registry.reserve(owner, actor(3));
        let tail = registry.reserve(owner, actor(4));
        for (request, target) in [(left, actor(2)), (right, actor(3)), (tail, actor(4))] {
            registry.mark_queued(owner, target, request).unwrap();
            registry.present(target, request).unwrap();
        }
        let response = |request| {
            ReadinessDependency::Request(
                request,
                WatchRequirement::Response {
                    allow_failure: false,
                },
            )
        };
        let source = registry
            .register_watch_plan(
                owner,
                "source".into(),
                Plan::checked(
                    vec![
                        Node::Leaf(response(left)),
                        Node::Leaf(response(right)),
                        Node::Either(0, 1),
                    ],
                    2,
                )
                .unwrap(),
            )
            .unwrap()
            .0;
        let first = registry
            .register_watch_plan(
                owner,
                "first".into(),
                Plan::checked(vec![Node::Leaf(ReadinessDependency::Watch(source))], 0).unwrap(),
            )
            .unwrap()
            .0;
        let nested = registry
            .register_watch_plan(
                owner,
                "nested".into(),
                Plan::checked(
                    vec![
                        Node::Leaf(ReadinessDependency::Watch(first)),
                        Node::Leaf(response(tail)),
                        Node::All(0, 1),
                    ],
                    2,
                )
                .unwrap(),
            )
            .unwrap()
            .0;
        let mut reply_claim_right = Some(registry.begin_reply(actor(3), right).unwrap());
        crate::request::test_support::complete_optional_reply(
            &registry,
            &mut reply_claim_right,
            None,
        );
        let original = registry
            .observe_watch_snapshot_decision(source, &[])
            .unwrap();
        assert_eq!(original.choices, vec![(2, false)]);
        registry.forget_watch(owner, source).unwrap();
        registry.forget_watch(owner, first).unwrap();
        registry.mark_target_unavailable(owner, left);
        let mut reply_claim_tail = Some(registry.begin_reply(actor(4), tail).unwrap());
        crate::request::test_support::complete_optional_reply(
            &registry,
            &mut reply_claim_tail,
            None,
        );
        assert!(matches!(
            registry.observe_watch(owner, nested),
            Ok(WatchObservation::Ready(_))
        ));
        assert_eq!(
            registry.observe_watch_snapshot_decision(nested, &[0, 0]),
            Ok(original)
        );
        assert_eq!(
            registry.observe_watch_snapshot_decision(source, &[]),
            Err(ReplyError::Stale)
        );
        assert_eq!(
            registry.observe_watch_snapshot_decision(nested, &[1]),
            Err(ReplyError::Unauthorized)
        );
    }

    #[test]
    fn cancelling_a_pending_source_watch_rejects_dependents_without_cancelling_requests() {
        use readiness::{Node, Plan};
        let registry = RequestRegistry::default();
        let owner = actor(1);
        let target = actor(2);
        let request = registry.reserve(owner, target);
        registry.mark_queued(owner, target, request).unwrap();
        registry.present(target, request).unwrap();
        let source = registry
            .register_transient_watch(
                owner,
                test_readiness_groups(vec![vec![(
                    request,
                    WatchRequirement::Response {
                        allow_failure: false,
                    },
                )]]),
            )
            .unwrap();
        let dependent = registry
            .register_watch_plan(
                owner,
                "dependent".into(),
                Plan::checked(vec![Node::Leaf(ReadinessDependency::Watch(source))], 0).unwrap(),
            )
            .unwrap()
            .0;
        let notices = registry.release_transient_watch(owner, source).unwrap();
        assert_eq!(notices.len(), 1);
        assert_eq!(notices[0].watch, dependent);
        assert_eq!(
            notices[0].transition,
            WatchTransition::Rejected(ReplyError::Stale)
        );
        assert_eq!(
            registry.observe_watch(owner, dependent),
            Ok(WatchObservation::Rejected(ReplyError::Stale))
        );
        assert!(matches!(
            registry.observe_response(owner, request),
            Ok(ResponseObservation::Pending(_))
        ));
    }

    #[test]
    fn observed_admission_racing_source_forget_either_refuses_or_retains_the_original_snapshot() {
        use readiness::{Node, Plan};
        for _ in 0..32 {
            let registry = std::sync::Arc::new(RequestRegistry::default());
            let owner = actor(1);
            let source = registry
                .register_watch_plan(
                    owner,
                    "source".into(),
                    Plan::checked(vec![Node::Ready, Node::Ready, Node::Either(0, 1)], 2).unwrap(),
                )
                .unwrap()
                .0;
            let original = registry
                .observe_watch_snapshot_decision(source, &[])
                .unwrap();
            let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
            let registrar = {
                let registry = registry.clone();
                let barrier = barrier.clone();
                std::thread::spawn(move || {
                    barrier.wait();
                    registry.register_watch_plan(
                        owner,
                        "dependent".into(),
                        Plan::checked(vec![Node::Leaf(ReadinessDependency::Watch(source))], 0)
                            .unwrap(),
                    )
                })
            };
            barrier.wait();
            registry.forget_watch(owner, source).unwrap();
            match registrar.join().unwrap() {
                Ok((dependent, _)) => assert_eq!(
                    registry.observe_watch_snapshot_decision(dependent, &[0]),
                    Ok(original)
                ),
                Err(error) => assert_eq!(error, ReplyError::Stale),
            }
        }
    }

    #[test]
    fn private_read_subscription_retains_projection_after_public_root_forget() {
        use readiness::{Node, Plan};
        use tidepool_bridge_effects::{
            CommandCleanup, CommandOutcome, CommandReport, CommandResult,
        };
        let registry = std::sync::Arc::new(RequestRegistry::default());
        let owner = actor(1);
        let request = registry.reserve_command_settlement(owner, "read-job".into(), false);
        let source = registry.register_watch(owner, vec![request]).unwrap().0;
        let report = CommandReport {
            command: vec!["true".into()],
            source: None,
            result: CommandResult {
                outcome: CommandOutcome::CommandExited(0),
                cleanup: CommandCleanup::CommandClean,
            },
            output_complete: true,
            tail: "stable projection".into(),
        };
        registry.settle_command(request, "exit 0".into(), None, report.clone());
        let read = registry
            .register_transient_watch(
                owner,
                Plan::checked(vec![Node::Leaf(ReadinessDependency::Watch(source))], 0).unwrap(),
            )
            .unwrap();
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
        let reader = {
            let registry = registry.clone();
            let barrier = barrier.clone();
            std::thread::spawn(move || {
                let readiness = registry.observe_watch(owner, read).unwrap();
                barrier.wait();
                barrier.wait();
                (
                    readiness,
                    registry
                        .observe_watch_snapshot_command(read, &[0], "read-job")
                        .unwrap(),
                )
            })
        };
        barrier.wait();
        registry.forget_watch(owner, source).unwrap();
        registry.forget_response(owner, request).unwrap();
        assert_eq!(
            registry.observe_watch(owner, source),
            Err(ReplyError::Stale)
        );
        barrier.wait();
        let (readiness, projected) = reader.join().unwrap();
        assert!(matches!(readiness, WatchObservation::Ready(_)));
        assert_eq!(projected, Some(report));
        registry.release_transient_watch(owner, read).unwrap();
        assert_eq!(
            registry.observe_watch_snapshot_command(read, &[0], "read-job"),
            Err(ReplyError::Stale)
        );
    }

    #[test]
    fn releasing_a_pending_private_read_does_not_cancel_its_public_source() {
        use readiness::{Node, Plan};
        let registry = RequestRegistry::default();
        let owner = actor(1);
        let target = actor(2);
        let request = registry.reserve(owner, target);
        registry.mark_queued(owner, target, request).unwrap();
        registry.present(target, request).unwrap();
        let source = registry.register_watch(owner, vec![request]).unwrap().0;
        let read = registry
            .register_transient_watch(
                owner,
                Plan::checked(vec![Node::Leaf(ReadinessDependency::Watch(source))], 0).unwrap(),
            )
            .unwrap();
        assert!(registry
            .release_transient_watch(owner, read)
            .unwrap()
            .is_empty());
        assert!(matches!(
            registry.observe_watch(owner, source),
            Ok(WatchObservation::Pending(_))
        ));
        assert!(matches!(
            registry.observe_response(owner, request),
            Ok(ResponseObservation::Pending(_))
        ));
    }

    #[test]
    fn command_projection_racing_source_release_keeps_the_selected_report() {
        use tidepool_bridge_effects::{
            CommandCleanup, CommandOutcome, CommandReport, CommandResult,
        };
        for _ in 0..32 {
            let registry = std::sync::Arc::new(RequestRegistry::default());
            let owner = actor(1);
            let request = registry.reserve_command_settlement(owner, "retained-job".into(), false);
            let (watch, _) = registry.register_watch(owner, vec![request]).unwrap();
            let report = CommandReport {
                command: vec!["true".into()],
                source: None,
                result: CommandResult {
                    outcome: CommandOutcome::CommandExited(0),
                    cleanup: CommandCleanup::CommandClean,
                },
                output_complete: true,
                tail: "captured before release".into(),
            };
            registry.settle_command(request, "exit 0".into(), None, report.clone());
            let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
            let observer = {
                let registry = registry.clone();
                let barrier = barrier.clone();
                std::thread::spawn(move || {
                    barrier.wait();
                    let decision = registry.observe_watch(owner, watch).unwrap();
                    let projected = registry
                        .observe_watch_command(watch, "retained-job")
                        .unwrap();
                    (decision, projected)
                })
            };
            barrier.wait();
            registry.forget_response(owner, request).unwrap();
            let (decision, projected) = observer.join().unwrap();
            assert_eq!(
                decision,
                WatchObservation::Ready(readiness::Decision {
                    leaves: vec![(0, None)],
                    choices: vec![]
                })
            );
            assert_eq!(projected, Some(report));
            registry.forget_watch(owner, watch).unwrap();
            assert_eq!(
                registry.observe_watch_command(watch, "retained-job"),
                Err(ReplyError::Stale)
            );
        }
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
            let mut reply_claim_request = Some(registry.begin_reply(target, request).unwrap());
            let notifications = crate::request::test_support::complete_optional_reply(
                &registry,
                &mut reply_claim_request,
                None,
            );
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
                            diagnostic: None,
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
        let mut reply_claim_request = Some(registry.begin_reply(target, request).unwrap());
        let notifications = crate::request::test_support::complete_optional_reply(
            &registry,
            &mut reply_claim_request,
            None,
        );
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
        assert!(crate::request::test_support::complete_optional_reply(
            &registry,
            &mut reply_claim_request,
            None
        )
        .is_empty());
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
    fn progress_choice_retains_only_the_selected_snapshot() {
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

        let mut reply_claim_first = Some(registry.begin_reply(first_target, first).unwrap());
        let notifications = crate::request::test_support::complete_optional_reply(
            &registry,
            &mut reply_claim_first,
            None,
        );
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
            Err(ReplyError::Unauthorized)
        ));
        assert!(matches!(
            registry.observe_watch_progress(owner, watch, unrelated, 0),
            Err(ReplyError::Unauthorized)
        ));

        let mut reply_claim_second = Some(registry.begin_reply(second_target, second).unwrap());
        assert!(crate::request::test_support::complete_optional_reply(
            &registry,
            &mut reply_claim_second,
            None
        )
        .is_empty());
        assert!(matches!(
            registry.observe_watch_progress(owner, watch, second, 0),
            Err(ReplyError::Unauthorized)
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
        let mut reply_claim_request = Some(registry.begin_reply(target, request).unwrap());
        crate::request::test_support::complete_optional_reply(
            &registry,
            &mut reply_claim_request,
            None,
        );
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
        let mut reply_claim_request = Some(registry.begin_reply(target, request).unwrap());
        crate::request::test_support::complete_optional_reply(
            &registry,
            &mut reply_claim_request,
            None,
        );
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
        let mut reply_claim_request = Some(registry.begin_reply(outside, request).unwrap());
        crate::request::test_support::complete_optional_reply(
            &registry,
            &mut reply_claim_request,
            None,
        );
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

        let mut reply_claim_left = Some(registry.begin_reply(left_target, left).unwrap());
        assert!(crate::request::test_support::complete_optional_reply(
            &registry,
            &mut reply_claim_left,
            None
        )
        .is_empty());
        let mut reply_claim_right = Some(registry.begin_reply(right_target, right).unwrap());
        let notifications = crate::request::test_support::complete_optional_reply(
            &registry,
            &mut reply_claim_right,
            None,
        );
        assert_eq!(notifications.len(), 1);
        assert_eq!(notifications[0].watch, watch);
        assert_eq!(notifications[0].label, "watch");
        assert_eq!(notifications[0].previous, WatchStateProjection::Pending);
        assert_eq!(notifications[0].current, WatchStateProjection::Ready);
        assert_eq!(notifications[0].sequence, ActorEventSequence(1));
        assert_eq!(notifications[0].watermark, ActorEventSequence(1));
        assert_eq!(notifications[0].transition, WatchTransition::Ready);
        assert_eq!(
            crate::request::test_support::complete_optional_reply(
                &registry,
                &mut reply_claim_right,
                None
            ),
            Vec::new()
        );
        assert_eq!(
            registry.begin_reply(right_target, right),
            Err(ReplyError::AlreadySettled)
        );
        assert!(matches!(
            registry.observe_watch(owner, watch),
            Ok(WatchObservation::Ready(_))
        ));
    }

    #[test]
    fn workbench_boundary_aborts_only_unsubmitted_reservations() {
        let registry = RequestRegistry::default();
        let owner = actor(1);
        let other_owner = actor(2);
        let target = actor(3);
        let operation = RequestReservationOwner::Workbench {
            execution: tidepool_runtime::session::WorkbenchExecutionId::from_digest([1; 16]),
            attempt: WorkbenchReservationAttempt::fresh(),
        };
        let reserve = |actor, operation: &RequestReservationOwner| {
            registry.reserve_for_operation(
                actor,
                target,
                "request".into(),
                true,
                Some(operation.clone()),
            )
        };
        let leaked = reserve(owner, &operation);
        let committed = reserve(owner, &operation);
        let unrelated = reserve(other_owner, &operation);
        let sibling_operation = RequestReservationOwner::Workbench {
            execution: tidepool_runtime::session::WorkbenchExecutionId::from_digest([2; 16]),
            attempt: WorkbenchReservationAttempt::fresh(),
        };
        let sibling = reserve(owner, &sibling_operation);
        let route_operation = RequestReservationOwner::Route(WatchId(1));
        let route = reserve(owner, &route_operation);
        registry.mark_queued(owner, target, committed).unwrap();

        assert_eq!(
            registry.abort_unsubmitted(owner, &operation).0,
            vec![leaked]
        );
        assert_eq!(
            registry.observe_response(owner, leaked),
            Err(ReplyError::Stale)
        );
        assert!(matches!(
            registry.observe_response(owner, committed),
            Ok(ResponseObservation::Pending(_))
        ));
        assert!(matches!(
            registry.observe_response(owner, sibling),
            Ok(ResponseObservation::Pending(_))
        ));
        assert!(matches!(
            registry.observe_response(owner, route),
            Ok(ResponseObservation::Pending(_))
        ));
        // The surviving operation can still commit its request after its
        // sibling's rollback; cleanup is not merely hidden from observation.
        registry.mark_queued(owner, target, sibling).unwrap();
        assert!(registry
            .abort_unsubmitted(owner, &sibling_operation)
            .0
            .is_empty());
        assert_eq!(
            registry.abort_unsubmitted(owner, &route_operation).0,
            vec![route]
        );
        assert_eq!(
            registry.abort_unsubmitted(other_owner, &operation).0,
            vec![unrelated]
        );
        assert!(registry.abort_unsubmitted(owner, &operation).0.is_empty());
    }

    #[test]
    fn stale_workbench_attempt_cleanup_cannot_abort_same_execution_retry() {
        let registry = RequestRegistry::default();
        let owner = actor(1);
        let target = actor(2);
        let execution = tidepool_runtime::session::WorkbenchExecutionId::from_digest([7; 16]);
        let attempt_a = RequestReservationOwner::Workbench {
            execution: execution.clone(),
            attempt: WorkbenchReservationAttempt::fresh(),
        };
        let request_a = registry.reserve_for_operation(
            owner,
            target,
            "attempt A".into(),
            true,
            Some(attempt_a.clone()),
        );

        assert_eq!(
            registry.abort_unsubmitted(owner, &attempt_a).0,
            vec![request_a]
        );
        assert_eq!(
            registry.observe_response(owner, request_a),
            Err(ReplyError::Stale)
        );

        let attempt_b = RequestReservationOwner::Workbench {
            execution,
            attempt: WorkbenchReservationAttempt::fresh(),
        };
        let request_b = registry.reserve_for_operation(
            owner,
            target,
            "attempt B".into(),
            true,
            Some(attempt_b.clone()),
        );

        // A delayed cleanup from the prior attempt is harmless even though
        // this retry has the same logical execution id and actor incarnation.
        assert!(registry.abort_unsubmitted(owner, &attempt_a).0.is_empty());
        assert!(matches!(
            registry.observe_response(owner, request_b),
            Ok(ResponseObservation::Pending(_))
        ));
        assert_eq!(
            registry.abort_unsubmitted(owner, &attempt_b).0,
            vec![request_b]
        );
    }

    #[test]
    fn submission_origin_preserves_reservation_custody_and_first_successful_submission() {
        let registry = RequestRegistry::default();
        let owner = actor(1);
        let target = actor(2);
        let reservation = RequestReservationOwner::Workbench {
            execution: tidepool_runtime::session::WorkbenchExecutionId::from_digest([1; 16]),
            attempt: WorkbenchReservationAttempt::fresh(),
        };
        let submission = RequestReservationOwner::Workbench {
            execution: tidepool_runtime::session::WorkbenchExecutionId::from_digest([2; 16]),
            attempt: WorkbenchReservationAttempt::fresh(),
        };
        let request = registry.reserve_for_operation(
            owner,
            target,
            "input".into(),
            true,
            Some(reservation.clone()),
        );
        let wrong_target = ActorRef {
            incarnation: Incarnation(2),
            ..target
        };
        assert!(registry
            .mark_queued_with_deadline(owner, wrong_target, request, None, Some(submission.clone()))
            .is_err());
        assert!(registry
            .mark_queued_with_deadline(actor(3), target, request, None, Some(submission.clone()))
            .is_err());
        assert!(registry.state.lock().requests[&request]
            .submission_origin
            .is_none());
        registry
            .mark_queued_with_deadline(owner, target, request, None, Some(submission.clone()))
            .unwrap();
        let expected = registry.state.lock().requests[&request]
            .submission_origin
            .clone()
            .unwrap();
        assert_eq!(
            registry.state.lock().requests[&request].reservation_owner,
            Some(reservation.clone())
        );
        assert!(registry
            .mark_queued_with_deadline(owner, target, request, None, Some(reservation))
            .is_err());
        assert_eq!(
            registry.state.lock().requests[&request].submission_origin,
            Some(expected.clone())
        );
        assert!(registry
            .present_with_progress_type(wrong_target, request, None)
            .is_err());
        let presentation = registry
            .present_with_progress_type(target, request, None)
            .unwrap();
        assert_eq!(presentation.origin, Some(expected.clone()));
        assert_eq!(presentation.cancellation, None);
        assert_eq!(expected.request, request);
        assert_eq!(expected.target, target);
        assert_eq!(expected.parent_actor, owner);
        if let RequestReservationOwner::Workbench { execution, attempt } = submission {
            assert_eq!(expected.execution, execution);
            assert_eq!(expected.attempt, attempt);
        }
        assert!(registry
            .present_with_progress_type(target, request, None)
            .is_err());
    }

    proptest::proptest! {
        #[test]
        fn submission_origin_operation_histories(
            original in proptest::array::uniform16(proptest::num::u8::ANY),
            submitted in proptest::array::uniform16(proptest::num::u8::ANY),
            retries in 0usize..12,
            cancel in proptest::bool::ANY,
            workbench in proptest::bool::ANY,
        ) {
            let registry = RequestRegistry::default();
            let owner = actor(1);
            let target = actor(2);
            let original = RequestReservationOwner::Workbench {
                execution: tidepool_runtime::session::WorkbenchExecutionId::from_digest(original),
                attempt: WorkbenchReservationAttempt::fresh(),
            };
            let submitted = workbench.then(|| RequestReservationOwner::Workbench {
                execution: tidepool_runtime::session::WorkbenchExecutionId::from_digest(submitted),
                attempt: WorkbenchReservationAttempt::fresh(),
            });
            let request = registry.reserve_for_operation(owner, target, "generated".into(), true,
                                                         Some(original.clone()));
            registry.mark_queued_with_deadline(owner, target, request, None, submitted.clone()).unwrap();
            let expected = registry.state.lock().requests[&request].submission_origin.clone();
            for _ in 0..retries {
                proptest::prop_assert!(registry.mark_queued_with_deadline(owner, target, request, None,
                                                                         Some(original.clone())).is_err());
                proptest::prop_assert_eq!(&registry.state.lock().requests[&request].submission_origin, &expected);
            }
            if cancel {
                registry.cancel_request(owner, request, CancellationReason::RequesterCancelled).unwrap();
            }
            let presentation = registry.present_with_progress_type(target, request, None).unwrap();
            proptest::prop_assert_eq!(presentation.cancellation, cancel.then_some(CancellationReason::RequesterCancelled));
            proptest::prop_assert_eq!(presentation.origin.as_ref(), expected.as_ref());
            proptest::prop_assert_eq!(&registry.state.lock().requests[&request].reservation_owner, &Some(original));
            match submitted {
                Some(RequestReservationOwner::Workbench { execution, attempt }) => {
                    let origin = presentation.origin.unwrap();
                    proptest::prop_assert_eq!((origin.request, origin.parent_actor, origin.target), (request, owner, target));
                    proptest::prop_assert_eq!((origin.execution, origin.attempt), (execution, attempt));
                }
                None => proptest::prop_assert!(presentation.origin.is_none()),
                _ => unreachable!(),
            }
        }
    }

    #[test]
    fn non_workbench_submissions_do_not_invent_activation_origin() {
        for submission in [
            None,
            Some(RequestReservationOwner::Route(WatchId(7))),
            Some(RequestReservationOwner::Scope(8)),
        ] {
            let registry = RequestRegistry::default();
            let owner = actor(1);
            let target = actor(2);
            let request = registry.reserve(owner, target);
            registry
                .mark_queued_with_deadline(owner, target, request, None, submission)
                .unwrap();
            assert!(registry
                .present_with_progress_type(target, request, None)
                .unwrap()
                .origin
                .is_none());
        }
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
                None,
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
        let mut reply_claim_request = Some(registry.begin_reply(target, request).unwrap());
        assert!(crate::request::test_support::complete_optional_reply(
            &registry,
            &mut reply_claim_request,
            None
        )
        .is_empty());

        let (watch, notifications) = registry.register_watch(owner, vec![request]).unwrap();
        assert_eq!(notifications.len(), 1);
        assert_eq!(notifications[0].watch, watch);
        assert_eq!(notifications[0].transition, WatchTransition::Ready);
    }

    #[tokio::test]
    async fn watch_wait_subscription_observes_settlement_before_and_after_subscribe() {
        let registry = std::sync::Arc::new(RequestRegistry::default());
        let owner = actor(1);
        let target = actor(2);
        let request = registry.reserve(owner, target);
        registry.mark_queued(owner, target, request).unwrap();
        registry.present(target, request).unwrap();
        let (watch, _) = registry.register_watch(owner, vec![request]).unwrap();

        let before_settlement = registry.subscribe_watch(owner, watch).unwrap();
        let mut reply_claim_request = Some(registry.begin_reply(target, request).unwrap());
        crate::request::test_support::complete_optional_reply(
            &registry,
            &mut reply_claim_request,
            None,
        );
        assert!(matches!(
            before_settlement.wait().await,
            Ok(WatchObservation::Ready(_))
        ));

        // The state check and subscription share the registry lock, so a
        // subscription made after the transition is immediately signalled.
        assert!(matches!(
            registry.subscribe_watch(owner, watch).unwrap().wait().await,
            Ok(WatchObservation::Ready(_))
        ));
    }

    #[tokio::test]
    async fn watch_wait_subscription_preserves_unavailable_state_and_checks_owner() {
        let registry = std::sync::Arc::new(RequestRegistry::default());
        let owner = actor(1);
        let target = actor(2);
        let intruder = actor(3);
        let replacement = ActorRef {
            id: owner.id,
            incarnation: Incarnation(owner.incarnation.0 + 1),
        };
        let request = registry.reserve(owner, target);
        registry.mark_queued(owner, target, request).unwrap();
        registry.present(target, request).unwrap();
        let (watch, _) = registry.register_watch(owner, vec![request]).unwrap();

        assert_eq!(
            registry.subscribe_watch(intruder, watch).err(),
            Some(ReplyError::Unauthorized)
        );
        assert_eq!(
            registry.subscribe_watch(replacement, watch).err(),
            Some(ReplyError::WrongIncarnation)
        );
        let waiting = registry.subscribe_watch(owner, watch).unwrap();
        registry.abandon_response(owner, request).unwrap();
        assert_eq!(
            waiting.wait().await,
            Ok(WatchObservation::Unavailable {
                request,
                failure: ResponseFailure::Abandoned,
            })
        );
    }

    #[tokio::test]
    async fn dropping_watch_wait_subscription_leaves_watch_registered() {
        let registry = std::sync::Arc::new(RequestRegistry::default());
        let owner = actor(1);
        let target = actor(2);
        let request = registry.reserve(owner, target);
        registry.mark_queued(owner, target, request).unwrap();
        registry.present(target, request).unwrap();
        let (watch, _) = registry.register_watch(owner, vec![request]).unwrap();

        let waiting = registry.subscribe_watch(owner, watch).unwrap();
        drop(waiting);
        assert!(registry.retains_watch(owner, watch));
        assert!(registry
            .state
            .lock()
            .watches
            .get(&watch)
            .is_some_and(|record| record.waiters.is_empty()));

        let mut reply_claim_request = Some(registry.begin_reply(target, request).unwrap());
        crate::request::test_support::complete_optional_reply(
            &registry,
            &mut reply_claim_request,
            None,
        );
        assert!(matches!(
            registry.observe_watch(owner, watch),
            Ok(WatchObservation::Ready(_))
        ));
    }

    #[tokio::test]
    async fn forgotten_watch_refuses_a_waiter_without_retaining_it() {
        let registry = std::sync::Arc::new(RequestRegistry::default());
        let owner = actor(1);
        let target = actor(2);
        let request = registry.reserve(owner, target);
        registry.mark_queued(owner, target, request).unwrap();
        registry.present(target, request).unwrap();
        let (watch, _) = registry.register_watch(owner, vec![request]).unwrap();
        let mut reply_claim_request = Some(registry.begin_reply(target, request).unwrap());
        crate::request::test_support::complete_optional_reply(
            &registry,
            &mut reply_claim_request,
            None,
        );

        let waiting = registry.subscribe_watch(owner, watch).unwrap();
        assert_eq!(
            registry.forget_watch(owner, watch),
            Ok(ForgetWatchOutcome::Forgotten)
        );
        assert_eq!(waiting.wait().await, Err(ReplyError::Stale));
    }

    #[tokio::test]
    async fn retired_owner_wakes_and_refuses_a_settled_watch_waiter() {
        let registry = std::sync::Arc::new(RequestRegistry::default());
        let owner = actor(1);
        let target = actor(2);
        let request = registry.reserve(owner, target);
        registry.mark_queued(owner, target, request).unwrap();
        registry.present(target, request).unwrap();
        let (watch, _) = registry.register_watch(owner, vec![request]).unwrap();
        let mut reply_claim_request = Some(registry.begin_reply(target, request).unwrap());
        crate::request::test_support::complete_optional_reply(
            &registry,
            &mut reply_claim_request,
            None,
        );

        let waiting = registry.subscribe_watch(owner, watch).unwrap();
        registry.forget_terminal_actor_metadata(owner).unwrap();
        assert!(!registry.retains_watch(owner, watch));
        assert_eq!(waiting.wait().await, Err(ReplyError::Stale));
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

        let _claim = registry.begin_reply(target, request).unwrap();
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
            diagnostic: None,
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
            diagnostic: None,
        };
        assert!(registry.actor_stopped(failed_target, &terminal).is_empty());
        let mut reply_claim_ready = Some(registry.begin_reply(ready_target, ready).unwrap());
        let notifications = crate::request::test_support::complete_optional_reply(
            &registry,
            &mut reply_claim_ready,
            None,
        );
        assert_eq!(notifications.len(), 1);
        assert_eq!(notifications[0].watch, watch);
        assert_eq!(
            registry.observe_watch(owner, watch),
            Ok(WatchObservation::Ready(readiness::Decision {
                leaves: vec![
                    (0, Some(ResponseFailure::TargetFailed("boom".into()))),
                    (1, None)
                ],
                choices: vec![],
            }))
        );
    }

    #[test]
    fn activation_publication_rechecks_cancellation_deadline_and_exact_target() {
        let registry = RequestRegistry::default();
        let owner = actor(1);
        let target = actor(2);
        let request = registry.reserve(owner, target);
        let publish = || panic!("ineligible activation must not publish");
        assert_eq!(
            registry.publish_presented_request(target, request, publish),
            Err(ReplyError::Stale)
        );
        registry.mark_queued(owner, target, request).unwrap();
        registry.present(target, request).unwrap();
        assert_eq!(
            registry.publish_presented_request(actor(3), request, publish),
            Err(ReplyError::Unauthorized)
        );
        assert_eq!(
            registry.publish_presented_request(target, request, || 42),
            Ok(42)
        );
        registry
            .cancel_request(owner, request, CancellationReason::RequesterCancelled)
            .unwrap();
        assert_eq!(
            registry.publish_presented_request(target, request, publish),
            Err(ReplyError::CancellationRequested)
        );
        let deadline = registry.reserve(owner, target);
        registry.mark_queued(owner, target, deadline).unwrap();
        registry.present(target, deadline).unwrap();
        registry.deadline_request(owner, deadline);
        assert_eq!(
            registry.publish_presented_request(target, deadline, publish),
            Err(ReplyError::CancellationRequested)
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
            let mut reply_claim_request = Some(registry.begin_reply(target, request).unwrap());
            assert_eq!(
                registry.deadline_request(owner, request),
                (None, Vec::new())
            );
            let notifications = if settlement_fails {
                registry.fail_reply_settlement(request, "lost result")
            } else {
                crate::request::test_support::complete_optional_reply(
                    &registry,
                    &mut reply_claim_request,
                    None,
                )
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
        let mut reply_claim_request = Some(registry.begin_reply(target, request).unwrap());
        assert!(crate::request::test_support::complete_optional_reply(
            &registry,
            &mut reply_claim_request,
            None
        )
        .is_empty());
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
            diagnostic: None,
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
    fn foreign_observation_does_not_acknowledge_owner_watch_transition() {
        let registry = RequestRegistry::default();
        let owner = actor(1);
        let target = actor(2);
        let foreign = actor(3);
        let other_incarnation = ActorRef {
            id: owner.id,
            incarnation: Incarnation(2),
        };
        let request = registry.reserve(owner, target);
        registry.mark_queued(owner, target, request).unwrap();
        registry.present(target, request).unwrap();
        let (watch, _) = registry.register_watch(owner, vec![request]).unwrap();

        let mut reply_claim_request = Some(registry.begin_reply(target, request).unwrap());
        let notifications = crate::request::test_support::complete_optional_reply(
            &registry,
            &mut reply_claim_request,
            None,
        );
        let transition_time = notifications
            .iter()
            .find(|notification| notification.watch == watch)
            .expect("owner watch transition")
            .occurred_at_unix_ms;

        assert!(matches!(
            registry.observe_watch(foreign, watch),
            Ok(WatchObservation::Ready(_))
        ));
        assert!(matches!(
            registry.observe_watch(other_incarnation, watch),
            Ok(WatchObservation::Ready(_))
        ));
        assert!(!registry.watch_observed_since(owner, watch, transition_time));

        let _ = registry.status_for(owner);
        let _ = registry.watches_overview(owner);
        assert!(!registry.watch_observed_since(owner, watch, transition_time));

        registry.observe_watch(owner, watch).unwrap();
        assert!(registry.watch_observed_since(owner, watch, transition_time));
        // Repeated foreign inspection cannot clear the owner's acknowledgment.
        registry.observe_watch(foreign, watch).unwrap();
        assert!(registry.watch_observed_since(owner, watch, transition_time));

        let unavailable_registry = RequestRegistry::default();
        let unavailable_request = unavailable_registry.reserve(owner, target);
        unavailable_registry
            .mark_queued(owner, target, unavailable_request)
            .unwrap();
        unavailable_registry
            .present(target, unavailable_request)
            .unwrap();
        let (unavailable_watch, _) = unavailable_registry
            .register_watch(owner, vec![unavailable_request])
            .unwrap();
        let notifications =
            unavailable_registry.mark_target_unavailable(owner, unavailable_request);
        let unavailable_transition_time = notifications
            .iter()
            .find(|notification| notification.watch == unavailable_watch)
            .expect("owner watch failure transition")
            .occurred_at_unix_ms;
        assert!(matches!(
            unavailable_registry.observe_watch(foreign, unavailable_watch),
            Ok(WatchObservation::Unavailable { .. })
        ));
        assert!(!unavailable_registry.watch_observed_since(
            owner,
            unavailable_watch,
            unavailable_transition_time
        ));
        unavailable_registry
            .observe_watch(owner, unavailable_watch)
            .unwrap();
        assert!(unavailable_registry.watch_observed_since(
            owner,
            unavailable_watch,
            unavailable_transition_time
        ));
    }

    #[test]
    fn foreign_listener_preserves_owner_notice_and_release_preserves_queued_decision() {
        let registry = RequestRegistry::default();
        let owner = actor(1);
        let target = actor(2);
        let child = actor(3);
        let request = registry.reserve_labeled_with_reporting(owner, target, "shared".into(), true);
        registry.mark_queued(owner, target, request).unwrap();
        registry.present(target, request).unwrap();
        let (owner_watch, _) = registry.register_watch(owner, vec![request]).unwrap();
        let (child_watch, _) = registry.register_watch(child, vec![request]).unwrap();

        let mut reply_claim_request = Some(registry.begin_reply(target, request).unwrap());
        let ready_notices = crate::request::test_support::complete_optional_reply(
            &registry,
            &mut reply_claim_request,
            None,
        );
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
        assert!(release_notices.is_empty());
        assert_eq!(
            registry.observe_response(child, request),
            Err(ReplyError::Stale)
        );
        assert_eq!(
            registry.observe_watch(child, child_watch),
            Ok(WatchObservation::Ready(readiness::Decision {
                leaves: vec![(0, None)],
                choices: vec![]
            }))
        );
        assert_eq!(
            registry.observe_watch(owner, owner_watch),
            Ok(WatchObservation::Ready(readiness::Decision {
                leaves: vec![(0, None)],
                choices: vec![]
            }))
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
        let mut reply_claim_request = Some(registry.begin_reply(target, request).unwrap());
        let notices = crate::request::test_support::complete_optional_reply(
            &registry,
            &mut reply_claim_request,
            None,
        );
        assert_eq!(notices.len(), 1);
        assert_eq!(notices[0].watch, watch);
        let owner_notices = registry.take_settlement_notifications();
        assert_eq!(owner_notices.len(), 1);
        assert_eq!(owner_notices[0].owner, owner);
        assert_eq!(owner_notices[0].request, request);
    }

    #[test]
    fn captured_leaf_survives_release_while_another_leaf_remains_pending() {
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
        let mut reply_claim_completed = Some(registry.begin_reply(target, completed).unwrap());
        assert!(crate::request::test_support::complete_optional_reply(
            &registry,
            &mut reply_claim_completed,
            None
        )
        .is_empty());
        assert!(matches!(
            registry.observe_watch(child, watch),
            Ok(WatchObservation::Pending(_))
        ));

        let (_, notices) = registry.forget_response(owner, completed).unwrap();
        assert!(notices.is_empty());
        assert!(matches!(
            registry.observe_watch(child, watch),
            Ok(WatchObservation::Pending(_))
        ));
        assert_eq!(
            registry.forget_response(owner, active),
            Ok((ForgetResponseOutcome::StillPending, Vec::new()))
        );
        let mut reply_claim_active = Some(registry.begin_reply(target, active).unwrap());
        assert_eq!(
            crate::request::test_support::complete_optional_reply(
                &registry,
                &mut reply_claim_active,
                None
            )
            .len(),
            1
        );
        assert!(matches!(
            registry.observe_watch(child, watch),
            Ok(WatchObservation::Ready(_))
        ));
    }

    #[test]
    fn mixed_watch_keeps_closed_progress_snapshot_after_response_release() {
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
        let mut reply_claim_request = Some(registry.begin_reply(target, request).unwrap());
        crate::request::test_support::complete_optional_reply(
            &registry,
            &mut reply_claim_request,
            None,
        );
        assert!(matches!(
            registry.observe_watch(child, watch),
            Ok(WatchObservation::Ready(_))
        ));
        registry.forget_response(owner, request).unwrap();
        assert!(matches!(
            registry.observe_watch_progress(child, watch, request, 0),
            Ok((None, true))
        ));
        assert!(matches!(
            registry.observe_watch_progress(other, watch, request, 0),
            Ok((None, true))
        ));
        assert_eq!(
            registry.observe_watch(other, watch),
            Ok(WatchObservation::Ready(readiness::Decision {
                leaves: vec![(0, None), (1, None)],
                choices: vec![]
            }))
        );
    }

    #[test]
    fn forgetting_terminal_owner_preserves_foreign_watch_decision() {
        let registry = RequestRegistry::default();
        let owner = actor(1);
        let target = actor(2);
        let observer = actor(3);
        let request = registry.reserve(owner, target);
        registry.mark_queued(owner, target, request).unwrap();
        registry.present(target, request).unwrap();
        let (watch, _) = registry.register_watch(observer, vec![request]).unwrap();
        let mut reply_claim_request = Some(registry.begin_reply(target, request).unwrap());
        crate::request::test_support::complete_optional_reply(
            &registry,
            &mut reply_claim_request,
            None,
        );

        let notifications = registry.forget_terminal_actor_metadata(owner).unwrap();
        assert!(notifications.is_empty());
        assert_eq!(
            registry.observe_watch(observer, watch),
            Ok(WatchObservation::Ready(readiness::Decision {
                leaves: vec![(0, None)],
                choices: vec![]
            }))
        );
    }

    #[test]
    fn explicit_cleanup_releases_terminal_response_and_preserves_dependent_watch() {
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
        let mut reply_claim_request = Some(registry.begin_reply(target, request).unwrap());
        crate::request::test_support::complete_optional_reply(
            &registry,
            &mut reply_claim_request,
            None,
        );
        let (outcome, notifications) = registry.forget_response(owner, request).unwrap();
        assert_eq!(outcome, ForgetResponseOutcome::Forgotten);
        assert!(notifications.is_empty());
        assert_eq!(
            registry.observe_watch(owner, watch),
            Ok(WatchObservation::Ready(readiness::Decision {
                leaves: vec![(0, None)],
                choices: vec![]
            }))
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
        let mut reply_claim_ready = Some(registry.begin_reply(ready_target, ready).unwrap());
        crate::request::test_support::complete_optional_reply(
            &registry,
            &mut reply_claim_ready,
            None,
        );

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
        let mut reply_claim_request = Some(registry.begin_reply(target, request).unwrap());

        // The settlement that makes the watch Ready is exactly the transition
        // a publisher would be holding when cleanup runs.
        let notifications = crate::request::test_support::complete_optional_reply(
            &registry,
            &mut reply_claim_request,
            None,
        );
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
