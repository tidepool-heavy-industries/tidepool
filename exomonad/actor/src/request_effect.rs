use tidepool_bridge::HaskellValue;
use tidepool_bridge::{BridgeError, HaskellVisitor, ToHaskell};
use tidepool_repr::DataConTable;
use tidepool_runtime::session::{ResidentHole, RootCustody};

use crate::resident_workbench::to_haskell::{
    actor_haskell_int, visit_agent_roster_state, visit_named, visit_provider_health,
};
use crate::{
    ActorRef, CancellationReason, PendingProgress, ReplyError, ReplyObservation, RequestId,
    ResponseFailure, ResponseObservation, WatchId, WatchObservation,
};

/// The wire counterpart of `Tidepool.Agent.Reply.Internal.PendingProgress`:
/// the producing actor's lifecycle and provider health project through the
/// same `visit_agent_roster_state`/`visit_provider_health` helpers
/// `AgentRosterEntry` uses for `observeAgent`, so a still-pending response
/// or watch carries the identical evidence, not a second tracker.
impl tidepool_bridge::sealed::ToHaskellSealed for PendingProgress {}
impl ToHaskell for PendingProgress {
    fn visit(
        &self,
        table: &DataConTable,
        visitor: &mut dyn HaskellVisitor,
    ) -> Result<(), BridgeError> {
        visit_named(
            table,
            visitor,
            "Tidepool.Agent.Reply.Internal",
            "PendingProgress",
            |visitor| {
                visit_agent_roster_state(table, visitor, self.actor_terminal.as_ref())?;
                visit_provider_health(table, visitor, self.provider_turn.as_ref())?;
                self.last_activity_unix_ms
                    .map(|value| actor_haskell_int(value, "last activity"))
                    .transpose()?
                    .visit(table, visitor)?;
                self.progress_revision
                    .map(|value| actor_haskell_int(value, "progress revision"))
                    .transpose()?
                    .visit(table, visitor)?;
                self.watched.visit(table, visitor)
            },
        )
    }
}

#[derive(Debug, Clone, Copy, tidepool_bridge_derive::FromHaskell)]
#[allow(
    clippy::enum_variant_names,
    reason = "variant names are the stable Haskell Duration constructors"
)]
pub(crate) enum RequestDuration {
    #[haskell(module = "Tidepool.Duration")]
    DurationMilliseconds(i64),
    #[haskell(module = "Tidepool.Duration")]
    DurationSeconds(i64),
    #[haskell(module = "Tidepool.Duration")]
    DurationMinutes(i64),
}

impl RequestDuration {
    pub(crate) fn checked_milliseconds(self) -> Result<u64, String> {
        Ok(self.checked()?.duration().as_millis() as u64)
    }

    pub(crate) fn checked(self) -> Result<crate::RequestDeadline, String> {
        let (value, unit) = match self {
            Self::DurationMilliseconds(value) => (value, crate::DeadlineUnit::Milliseconds),
            Self::DurationSeconds(value) => (value, crate::DeadlineUnit::Seconds),
            Self::DurationMinutes(value) => (value, crate::DeadlineUnit::Minutes),
        };
        crate::RequestDeadline::checked(value, unit)
    }
}

#[derive(tidepool_bridge_derive::FromHaskell)]
#[allow(
    dead_code,
    clippy::enum_variant_names,
    reason = "decode-only shape mirrors the Haskell constructor names; fields \
              exist to make the FromHaskell arity match, not for Rust reads"
)]
pub(crate) enum RepliesReq {
    #[haskell(module = "Tidepool.Agent.Reply.Internal")]
    CurrentRequestWith(i64),
    #[haskell(module = "Tidepool.Agent.Reply.Internal")]
    ReserveRequestWith(Option<String>, (i64, i64), bool, crate::WorkerLifetime),
    #[haskell(module = "Tidepool.Agent.Reply.Internal")]
    // Duration reaches Core through its generated constructor representation.
    SubmitRequestWith(i64, i64, HaskellValue, (i64, i64), Option<RequestDuration>),
    // The `String` is the bounded reply preview `Tidepool.Agent.Reply.Internal.reply`/
    // `attemptReply` render on the Haskell side (`WorkbenchDisplay`), ahead
    // of the host's own line-boundary truncation in `stage_request_reply`.
    #[haskell(module = "Tidepool.Agent.Reply.Internal")]
    AttemptReplyWith(i64, HaskellValue, String),
    #[haskell(module = "Tidepool.Agent.Reply.Internal")]
    ReplyWith(i64, HaskellValue, String),
    #[haskell(module = "Tidepool.Agent.Reply.Internal")]
    ObserveResponseWith(i64, i64),
    #[haskell(module = "Tidepool.Agent.Reply.Internal")]
    CancelRequestWith(i64),
    RetainRequestWith(i64, crate::WorkerLifetime),
    AbandonResponseWith(i64),
    ForgetResponseWith(i64),
    ObserveReplyWith(i64),
    AttemptAcknowledgeCancellationWith(i64),
    AcknowledgeCancellationWith(i64),
    PublishProgressWith(i64, HaskellValue, i64),
    ObserveProgressWith(i64, i64),
    UpdateRequestWith(i64, String),
    ObserveRequestUpdateWith(i64, i64),
}

#[derive(Clone, Copy)]
pub(crate) enum RequestScopeRefusal {
    NoCurrentRequest,
    RequestTypeMismatch,
    RequestInputShadowed,
}

impl tidepool_bridge::sealed::ToHaskellSealed for RequestScopeRefusal {}
impl ToHaskell for RequestScopeRefusal {
    fn visit(
        &self,
        table: &DataConTable,
        visitor: &mut dyn HaskellVisitor,
    ) -> Result<(), BridgeError> {
        let reason = match self {
            Self::NoCurrentRequest => "NoCurrentRequest",
            Self::RequestTypeMismatch => "RequestTypeMismatch",
            Self::RequestInputShadowed => "RequestInputShadowed",
        };
        let reason = table
            .get_by_qualified_name(&format!("Tidepool.Agent.Reply.Internal.{reason}"))
            .ok_or_else(|| BridgeError::UnknownDataConName(reason.into()))?;
        let unavailable = table
            .get_by_qualified_name("Tidepool.Agent.Reply.Internal.RequestUnavailable")
            .ok_or_else(|| BridgeError::UnknownDataConName("RequestUnavailable".into()))?;
        visitor.begin_constructor(unavailable, 1)?;
        visitor.begin_constructor(reason, 0)?;
        visitor.end_constructor()?;
        visitor.end_constructor()
    }
}

#[derive(tidepool_bridge_derive::FromHaskell)]
#[allow(
    clippy::enum_variant_names,
    reason = "variant names are the stable Haskell constructor names"
)]
pub(crate) enum WatchesReq {
    #[haskell(module = "Tidepool.Agent.Watch.Internal")]
    RegisterWatchWith(String, AwaitPlan),
    RegisterAwaitWith(AwaitPlan),
    ReleaseAwaitWith(i64),
    RegisterRouteWith(String, tidepool_bridge::HaskellValue, AwaitPlan),
    ObserveRouteWith(i64),
    ListRoutesWith,
    #[haskell(module = "Tidepool.Agent.Watch.Internal")]
    ObserveWatchWith(i64),
    #[haskell(module = "Tidepool.Agent.Watch.Internal")]
    AwaitWatchWith(i64),
    ForgetWatchWith(i64),
    ObserveWatchProgressWith(i64, i64, Vec<i64>, i64, i64),
    ObserveWatchDecisionWith(i64, Vec<i64>),
    ObserveWatchResponseWith(i64, i64, Vec<i64>, i64),
    ObserveWatchCommandWith(i64, Vec<i64>, String),
}

#[derive(tidepool_bridge_derive::FromHaskell)]
#[allow(
    clippy::enum_variant_names,
    reason = "variant names are the stable Haskell constructor names"
)]
pub(crate) enum AwaitDependency {
    #[haskell(module = "Tidepool.Agent.Watch.Internal")]
    AwaitDependency(i64, bool),
    AwaitProgress(i64, i64),
    AwaitCommand(String),
    AwaitWatching(i64),
}

#[derive(tidepool_bridge_derive::FromHaskell)]
#[allow(clippy::enum_variant_names)]
pub(crate) enum AwaitPlan {
    #[haskell(module = "Tidepool.Agent.Watch.Internal")]
    AwaitPlan(Vec<AwaitNode>, i64),
}

#[derive(tidepool_bridge_derive::FromHaskell)]
pub(crate) enum AwaitNode {
    #[haskell(module = "Tidepool.Agent.Watch.Internal")]
    ReadyNode,
    LeafNode(AwaitDependency),
    AllNode(i64, i64),
    EitherNode(i64, i64),
}

pub(crate) fn projection_path(path: Vec<i64>) -> Result<Vec<usize>, BridgeError> {
    path.into_iter()
        .map(|node| {
            usize::try_from(node)
                .map_err(|_| BridgeError::UnsupportedType("negative observation path".into()))
        })
        .collect()
}

impl AwaitPlan {
    pub(crate) fn checked(
        self,
    ) -> Result<
        crate::request::readiness::Plan<(WatchSubject, crate::request::WatchRequirement)>,
        BridgeError,
    > {
        use crate::request::readiness::{Node, Plan};
        let Self::AwaitPlan(nodes, root) = self;
        let index = |value: i64| {
            usize::try_from(value).map_err(|_| {
                BridgeError::UnsupportedType("negative readiness node reference".into())
            })
        };
        let nodes = nodes
            .into_iter()
            .map(|node| {
                Ok(match node {
                    AwaitNode::ReadyNode => Node::Ready,
                    AwaitNode::LeafNode(dependency) => Node::Leaf(dependency.checked()?),
                    AwaitNode::AllNode(left, right) => Node::All(index(left)?, index(right)?),
                    AwaitNode::EitherNode(left, right) => Node::Either(index(left)?, index(right)?),
                })
            })
            .collect::<Result<Vec<_>, BridgeError>>()?;
        Plan::checked(nodes, index(root)?)
            .map_err(|_| BridgeError::UnsupportedType("invalid readiness graph".into()))
    }
}

/// What one watch dependency names before registration. A command job is
/// resolved to the request its completion settles by the actor that owns the
/// job table, at registration time.
pub(crate) enum WatchSubject {
    Request(RequestId),
    Command(String),
    Watch(WatchId),
}

impl AwaitDependency {
    pub(crate) fn checked(
        self,
    ) -> Result<(WatchSubject, crate::request::WatchRequirement), BridgeError> {
        Ok(match self {
            Self::AwaitDependency(request, allow_failure) => (
                WatchSubject::Request(request_id(request)?),
                crate::request::WatchRequirement::Response { allow_failure },
            ),
            Self::AwaitProgress(request, cursor) => (
                WatchSubject::Request(request_id(request)?),
                crate::request::WatchRequirement::ProgressAfter(u64::try_from(cursor).map_err(
                    |_| BridgeError::UnsupportedType("negative progress cursor".into()),
                )?),
            ),
            Self::AwaitWatching(watch) => (
                WatchSubject::Watch(watch_id(watch)?),
                crate::request::WatchRequirement::Response {
                    allow_failure: false,
                },
            ),
            Self::AwaitCommand(job) => (
                WatchSubject::Command(job),
                crate::request::WatchRequirement::Response {
                    allow_failure: false,
                },
            ),
        })
    }
}

#[derive(Debug, tidepool_bridge_derive::ToHaskell)]
#[haskell(module = "Tidepool.Agent.Reply.Internal")]
pub(crate) enum RequestError {
    RequestReservationRejected(ReplyError),
    RequestSubmissionRejected(ReplyError),
    RequestInvalidDeadline(String),
}

pub(crate) struct RequestReservation {
    pub continuation: ResidentHole,
    pub target: ActorRef,
    pub label: Option<String>,
    pub notify_owner: bool,
    pub lifetime: crate::WorkerLifetime,
}

pub(crate) struct RequestSubmission {
    pub destination: crate::owned_result::RequestResultDestination,
    pub continuation: ResidentHole,
    pub request: RequestId,
    pub target: ActorRef,
    pub message: crate::MailboxValue,
    pub deadline: Option<crate::RequestDeadline>,
}

pub(crate) struct ReplyAttempt {
    pub continuation: ResidentHole,
    pub request: RequestId,
    pub result: RootCustody,
    pub recoverable: bool,
    /// The reply preview Haskell rendered at the reply site (`WorkbenchDisplay`),
    /// ahead of the host's own truncation. `None` when the reply carries no
    /// preview (an older non-Replies reply path); `stage_request_reply` falls
    /// back to its own retained-heap walk in that case.
    pub preview: Option<String>,
}

pub(crate) struct ResponsePoll {
    pub continuation: ResidentHole,
    pub request: RequestId,
}

pub(crate) struct RequestCancellation {
    pub continuation: ResidentHole,
    pub request: RequestId,
}

pub(crate) struct ResponseAbandonment {
    pub continuation: ResidentHole,
    pub request: RequestId,
}

pub(crate) struct ResponseForget {
    pub continuation: ResidentHole,
    pub request: RequestId,
}

pub(crate) struct ReplyPoll {
    pub continuation: ResidentHole,
    pub request: RequestId,
}

pub(crate) struct CancellationAcknowledgement {
    pub continuation: ResidentHole,
    pub request: RequestId,
    pub recoverable: bool,
}

pub(crate) struct WatchRegistration {
    pub transient: bool,
    pub continuation: ResidentHole,
    pub dependencies:
        crate::request::readiness::Plan<(WatchSubject, crate::request::WatchRequirement)>,
    pub label: String,
}

pub(crate) struct WatchPoll {
    pub continuation: ResidentHole,
    pub watch: WatchId,
}

pub(crate) struct WatchForget {
    pub continuation: ResidentHole,
    pub watch: WatchId,
}

pub(crate) fn request_id(raw: i64) -> Result<RequestId, BridgeError> {
    u64::try_from(raw)
        .map(RequestId)
        .map_err(|_| BridgeError::UnsupportedType(format!("invalid request id {raw}")))
}

pub(crate) fn watch_id(raw: i64) -> Result<WatchId, BridgeError> {
    u64::try_from(raw)
        .map(WatchId)
        .map_err(|_| BridgeError::UnsupportedType(format!("invalid watch id {raw}")))
}

fn reply_error_name(error: ReplyError) -> &'static str {
    match error {
        ReplyError::UpdatePending => "ReplyUpdatePending",
        ReplyError::Stale => "ReplyStale",
        ReplyError::AlreadySettled => "ReplyAlreadySettled",
        ReplyError::Unauthorized => "ReplyUnauthorized",
        ReplyError::WrongIncarnation => "ReplyWrongIncarnation",
        ReplyError::InvalidReadiness => "ReplyInvalidReadiness",
        ReplyError::ProgressTypeMismatch => "ReplyProgressTypeMismatch",
        ReplyError::ReplyResultTypeMismatch => "ReplyResultTypeMismatch",
        ReplyError::ReplyResultUnavailable => "ReplyResultUnavailable",
        ReplyError::CancellationRequested => "ReplySettlementCancelled",
    }
}

/// An actor reply whose successful payload is emitted directly into the
/// resident managed builder.
pub(crate) struct ReplyResult<T>(pub Result<T, ReplyError>);

impl<T> tidepool_bridge::sealed::ToHaskellSealed for ReplyResult<T> {}

impl<T: ToHaskell> ToHaskell for ReplyResult<T> {
    fn visit(
        &self,
        table: &DataConTable,
        visitor: &mut dyn HaskellVisitor,
    ) -> Result<(), BridgeError> {
        match &self.0 {
            Ok(value) => {
                let right = tidepool_bridge::get_qualified(table, "Data.Either.Right", 1)
                    .ok_or_else(|| BridgeError::UnknownDataConName("Right".into()))?;
                visitor.begin_constructor(right, 1)?;
                value.visit(table, visitor)?;
                visitor.end_constructor()
            }
            Err(error) => {
                let left = tidepool_bridge::get_qualified(table, "Data.Either.Left", 1)
                    .ok_or_else(|| BridgeError::UnknownDataConName("Left".into()))?;
                visitor.begin_constructor(left, 1)?;
                error.visit(table, visitor)?;
                visitor.end_constructor()
            }
        }
    }
}

/// Owned reply metadata, visited only at the immediate resume boundary.
pub(crate) enum RequestAnswer {
    Response(Result<ResponseObservation, ReplyError>),
    Cancel(Result<crate::CancelRequestOutcome, ReplyError>),
    Abandon(Result<crate::AbandonResponseOutcome, ReplyError>),
    ForgetResponse(Result<crate::ForgetResponseOutcome, ReplyError>),
    Reply(Result<ReplyObservation, ReplyError>),
    Watch(Result<WatchObservation, ReplyError>),
    ForgetWatch(Result<crate::ForgetWatchOutcome, ReplyError>),
}

fn visit_constructor(
    table: &DataConTable,
    visitor: &mut dyn HaskellVisitor,
    module: &str,
    name: &str,
    fields: &[&dyn ToHaskell],
) -> Result<(), BridgeError> {
    let qualified = format!("{module}.{name}");
    let id = table
        .get_by_qualified_name(&qualified)
        .ok_or(BridgeError::UnknownDataConName(qualified))?;
    visitor.begin_constructor(id, fields.len())?;
    for field in fields {
        field.visit(table, visitor)?;
    }
    visitor.end_constructor()
}

impl tidepool_bridge::sealed::ToHaskellSealed for RequestAnswer {}
impl ToHaskell for RequestAnswer {
    fn visit(
        &self,
        table: &DataConTable,
        visitor: &mut dyn HaskellVisitor,
    ) -> Result<(), BridgeError> {
        let module = match self {
            Self::Watch(_) | Self::ForgetWatch(_) => "Tidepool.Agent.Watch.Internal",
            _ => "Tidepool.Agent.Reply.Internal",
        };
        macro_rules! emit {
            ($name:expr $(, $field:expr)* $(,)?) => {
                visit_constructor(table, visitor, module, $name, &[$($field),*])
            };
        }
        match self {
            Self::Response(Ok(ResponseObservation::Pending(progress))) => {
                emit!("ResponsePending", progress)
            }
            Self::Response(Ok(ResponseObservation::CancellationPending(reason))) => {
                emit!("ResponseCancellationPending", reason)
            }
            Self::Response(Ok(ResponseObservation::Ready)) => Err(BridgeError::UnsupportedType(
                "typed response readiness requires the owned live result".into(),
            )),
            Self::Response(Ok(ResponseObservation::Unavailable(failure))) => {
                emit!("ResponseUnavailable", failure)
            }
            Self::Response(Ok(ResponseObservation::Starting(detail))) => {
                emit!("ResponseStarting", detail)
            }
            Self::Response(Err(error)) => {
                let rejected = tidepool_bridge::get_qualified(
                    table,
                    "Tidepool.Agent.Reply.Internal.ResponseRejected",
                    1,
                )
                .ok_or_else(|| BridgeError::UnknownDataConName("ResponseRejected".into()))?;
                let unavailable = tidepool_bridge::get_qualified(
                    table,
                    "Tidepool.Agent.Reply.Internal.ResponseUnavailable",
                    1,
                )
                .ok_or_else(|| BridgeError::UnknownDataConName("ResponseUnavailable".into()))?;
                visitor.begin_constructor(unavailable, 1)?;
                visitor.begin_constructor(rejected, 1)?;
                error.visit(table, visitor)?;
                visitor.end_constructor()?;
                visitor.end_constructor()
            }
            Self::Cancel(Ok(crate::CancelRequestOutcome::Requested)) => {
                emit!("CancellationRequested")
            }
            Self::Cancel(Ok(crate::CancelRequestOutcome::AlreadyRequested)) => {
                emit!("CancellationAlreadyRequested")
            }
            Self::Cancel(Ok(crate::CancelRequestOutcome::AlreadyTerminal)) => {
                emit!("CancellationAlreadyTerminal")
            }
            Self::Cancel(Err(error)) => emit!("CancellationRejected", error),
            Self::Abandon(Ok(crate::AbandonResponseOutcome::AbandonedNow)) => {
                emit!("ResponseAbandonedNow")
            }
            Self::Abandon(Ok(crate::AbandonResponseOutcome::AlreadyAbandoned)) => {
                emit!("ResponseAlreadyAbandoned")
            }
            Self::Abandon(Ok(crate::AbandonResponseOutcome::AlreadyTerminal)) => {
                emit!("ResponseAlreadyTerminal")
            }
            Self::Abandon(Err(error)) => emit!("ResponseAbandonRejected", error),
            Self::ForgetResponse(Ok(crate::ForgetResponseOutcome::Forgotten)) => {
                emit!("ResponseForgotten")
            }
            Self::ForgetResponse(Ok(crate::ForgetResponseOutcome::StillPending)) => {
                emit!("ResponseForgetPending")
            }
            Self::ForgetResponse(Ok(crate::ForgetResponseOutcome::TargetStillActive)) => {
                emit!("ResponseForgetTargetActive")
            }
            Self::ForgetResponse(Err(error)) => emit!("ResponseForgetRejected", error),
            Self::Reply(Ok(ReplyObservation::Open)) => emit!("RawReplyOpen"),
            Self::Reply(Ok(ReplyObservation::CancellationRequested(reason))) => {
                emit!("RawReplyCancellationRequested", reason)
            }
            Self::Reply(Ok(ReplyObservation::Closed)) => emit!("RawReplyClosed"),
            Self::Reply(Err(error)) => emit!("RawReplyRejected", error),
            Self::Watch(Ok(WatchObservation::Pending(progress))) => {
                emit!("RawWatchPending", progress)
            }
            Self::Watch(Ok(WatchObservation::Rejected(error))) => emit!("RawWatchRejected", error),
            Self::Watch(Ok(WatchObservation::Ready(decision))) => emit!("RawWatchReady", decision),
            Self::Watch(Ok(WatchObservation::Unavailable { request, failure })) => {
                emit!("RawWatchUnavailable", request, failure)
            }
            Self::Watch(Err(error)) => emit!("RawWatchRejected", error),
            Self::ForgetWatch(Ok(crate::ForgetWatchOutcome::Forgotten)) => emit!("WatchForgotten"),
            Self::ForgetWatch(Ok(crate::ForgetWatchOutcome::StillPending)) => {
                emit!("WatchForgetPending")
            }
            Self::ForgetWatch(Err(error)) => emit!("WatchForgetRejected", error),
        }
    }
}

impl tidepool_bridge::sealed::ToHaskellSealed for ReplyError {}
impl ToHaskell for ReplyError {
    fn visit(
        &self,
        table: &DataConTable,
        visitor: &mut dyn HaskellVisitor,
    ) -> Result<(), BridgeError> {
        visit_constructor(
            table,
            visitor,
            "Tidepool.Agent.Reply.Internal",
            reply_error_name(*self),
            &[],
        )
    }
}

impl tidepool_bridge::sealed::ToHaskellSealed for CancellationReason {}
impl ToHaskell for CancellationReason {
    fn visit(
        &self,
        table: &DataConTable,
        visitor: &mut dyn HaskellVisitor,
    ) -> Result<(), BridgeError> {
        let name = match self {
            Self::RequesterCancelled => "CancelledByRequester",
            Self::DeadlineExpired => "DeadlineExpired",
        };
        visit_constructor(table, visitor, "Tidepool.Agent.Reply.Internal", name, &[])
    }
}

impl tidepool_bridge::sealed::ToHaskellSealed for ResponseFailure {}
impl ToHaskell for ResponseFailure {
    fn visit(
        &self,
        table: &DataConTable,
        visitor: &mut dyn HaskellVisitor,
    ) -> Result<(), BridgeError> {
        let (name, detail) = match self {
            Self::Released => ("ResponseReleased", None),
            Self::TargetUnavailable => ("ResponseTargetUnavailable", None),
            Self::TargetFailed(summary) => ("ResponseTargetFailed", Some(summary)),
            Self::TargetCancelled(summary) => ("ResponseTargetCancelled", Some(summary)),
            Self::RequesterStopped => ("ResponseRequesterStopped", None),
            Self::Abandoned => ("ResponseAbandoned", None),
            Self::Cancelled => ("ResponseCancelled", None),
            Self::DeadlineExceeded => ("ResponseDeadlineExceeded", None),
            Self::SettlementFailed(detail) => ("ResponseSettlementFailed", Some(detail)),
        };
        match detail {
            Some(detail) => visit_constructor(
                table,
                visitor,
                "Tidepool.Agent.Reply.Internal",
                name,
                &[detail],
            ),
            None => visit_constructor(table, visitor, "Tidepool.Agent.Reply.Internal", name, &[]),
        }
    }
}

impl tidepool_bridge::sealed::ToHaskellSealed for RequestId {}
impl ToHaskell for RequestId {
    fn visit(
        &self,
        table: &DataConTable,
        visitor: &mut dyn HaskellVisitor,
    ) -> Result<(), BridgeError> {
        i64::try_from(self.0)
            .map_err(|_| BridgeError::UnsupportedType("request id exceeds Int".into()))?
            .visit(table, visitor)
    }
}

impl tidepool_bridge::sealed::ToHaskellSealed for WatchId {}
impl ToHaskell for WatchId {
    fn visit(
        &self,
        table: &DataConTable,
        visitor: &mut dyn HaskellVisitor,
    ) -> Result<(), BridgeError> {
        i64::try_from(self.0)
            .map_err(|_| BridgeError::UnsupportedType("watch id exceeds Int".into()))?
            .visit(table, visitor)
    }
}

#[cfg(test)]
mod tests {
    use super::{RequestDuration, RequestError};
    use crate::{ReplyError, RequestId};
    use tidepool_bridge::{BridgeError, HaskellValue, ToHaskell};
    use tidepool_repr::{DataCon, DataConId, DataConTable, Literal};

    fn reservation_answer_table() -> DataConTable {
        let mut table = DataConTable::new();
        for (id, qualified_name, rep_arity) in [
            (1, "Data.Either.Right", 1),
            (2, "Data.Either.Left", 1),
            (3, "GHC.Types.I#", 1),
            (4, "GHC.Types.W#", 1),
            (
                5,
                "Tidepool.Agent.Reply.Internal.RequestReservationRejected",
                1,
            ),
            (6, "Tidepool.Agent.Reply.Internal.ReplyStale", 0),
        ] {
            table
                .insert_checked(DataCon {
                    identity: tidepool_repr::execution_schema::SymbolIdentity {
                        unit: "fixture".into(),
                        module: "Fixture".into(),
                        namespace: "constructor".into(),
                        occurrence: qualified_name.rsplit('.').next().unwrap().to_owned(),
                        record_parent: None,
                    },
                    id: DataConId(id),
                    name: qualified_name.rsplit('.').next().unwrap().into(),
                    tag: 1,
                    rep_arity,
                    field_bangs: Vec::new(),
                    qualified_name: Some(qualified_name.into()),
                    type_name: String::new(),
                })
                .expect("valid fixture metadata");
        }
        table
    }

    #[test]
    fn reservation_reply_emits_int_carrier_and_preserves_typed_refusal() {
        let table = reservation_answer_table();
        for value in [1_i64, i64::MAX] {
            let answer: Result<RequestId, RequestError> =
                Ok(RequestId(u64::try_from(value).unwrap()));
            let encoded = answer.to_value(&table).unwrap();
            let HaskellValue::Con(right, fields) = &encoded else {
                panic!("reservation reply must be Right")
            };
            assert_eq!(*right, DataConId(1));
            let [HaskellValue::Con(integer, payload)] = fields.as_slice() else {
                panic!("reservation reply must contain a boxed Int")
            };
            assert_eq!(*integer, DataConId(3));
            assert!(
                matches!(payload.as_slice(), [HaskellValue::Lit(Literal::LitInt(actual))] if *actual == value)
            );
        }

        let rejected: Result<RequestId, RequestError> =
            Err(RequestError::RequestReservationRejected(ReplyError::Stale));
        let encoded = rejected.to_value(&table).unwrap();
        let HaskellValue::Con(left, fields) = &encoded else {
            panic!("reservation refusal must be Left")
        };
        assert_eq!(*left, DataConId(2));
        let [HaskellValue::Con(rejection, fields)] = fields.as_slice() else {
            panic!("reservation refusal must retain RequestError")
        };
        assert_eq!(*rejection, DataConId(5));
        assert!(
            matches!(fields.as_slice(), [HaskellValue::Con(stale, fields)] if *stale == DataConId(6) && fields.is_empty())
        );
    }

    #[test]
    fn reservation_reply_refuses_request_id_above_haskell_int_max() {
        let answer: Result<RequestId, RequestError> =
            Ok(RequestId(u64::try_from(i64::MAX).unwrap() + 1));
        assert!(matches!(
            answer.to_value(&reservation_answer_table()),
            Err(BridgeError::UnsupportedType(_))
        ));
    }

    #[test]
    fn request_duration_preserves_authored_units_and_checks_conversion() {
        let seconds = RequestDuration::DurationSeconds(600)
            .checked()
            .expect("ten minute deadline");
        assert_eq!(seconds.duration().as_millis(), 600_000);

        let immediate = RequestDuration::DurationMilliseconds(0)
            .checked()
            .expect("explicit immediate deadline");
        assert_eq!(immediate.duration().as_millis(), 0);
        assert!(RequestDuration::DurationMinutes(-1).checked().is_err());
        assert!(RequestDuration::DurationMinutes(i64::MAX)
            .checked()
            .is_err());
        assert_eq!(
            RequestDuration::DurationMilliseconds(0)
                .checked_milliseconds()
                .unwrap(),
            0
        );
        assert_eq!(
            RequestDuration::DurationMinutes(15)
                .checked_milliseconds()
                .unwrap(),
            900_000
        );
    }
}

impl tidepool_bridge::sealed::ToHaskellSealed for crate::request::readiness::Decision {}
impl ToHaskell for crate::request::readiness::Decision {
    fn visit(
        &self,
        table: &DataConTable,
        visitor: &mut dyn HaskellVisitor,
    ) -> Result<(), BridgeError> {
        let leaves = self
            .leaves
            .iter()
            .map(|(node, failure)| (*node as i64, failure.clone()))
            .collect::<Vec<_>>();
        let choices = self
            .choices
            .iter()
            .map(|(node, left)| (*node as i64, *left))
            .collect::<Vec<_>>();
        visit_constructor(
            table,
            visitor,
            "Tidepool.Agent.Watch.Internal",
            "AwaitDecision",
            &[&leaves, &choices],
        )
    }
}
