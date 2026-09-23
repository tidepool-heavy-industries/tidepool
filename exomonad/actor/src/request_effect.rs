use tidepool_bridge::HaskellValue;
use tidepool_bridge::{BridgeError, HaskellVisitor, ToHaskell};
use tidepool_repr::DataConTable;
use tidepool_runtime::session::{ResidentHole, RootCustody};

use crate::{
    ActorRef, CancellationReason, ReplyError, ReplyObservation, RequestId, ResponseFailure,
    ResponseObservation, WatchId, WatchObservation,
};

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
#[allow(dead_code, clippy::enum_variant_names)]
pub(crate) enum RepliesReq {
    #[haskell(module = "Tidepool.Agent.Reply.Internal")]
    ReserveRequestWith(String, (i64, i64), bool),
    #[haskell(module = "Tidepool.Agent.Reply.Internal")]
    // Duration reaches Core through its generated constructor representation.
    SubmitRequestWith(i64, HaskellValue, (i64, i64), Option<RequestDuration>),
    #[haskell(module = "Tidepool.Agent.Reply.Internal")]
    AttemptReplyWith(i64, HaskellValue),
    #[haskell(module = "Tidepool.Agent.Reply.Internal")]
    ReplyWith(i64, HaskellValue),
    #[haskell(module = "Tidepool.Agent.Reply.Internal")]
    ObserveResponseWith(i64),
    #[haskell(module = "Tidepool.Agent.Reply.Internal")]
    CancelRequestWith(i64),
    AbandonResponseWith(i64),
    ForgetResponseWith(i64),
    ObserveReplyWith(i64),
    AttemptAcknowledgeCancellationWith(i64),
    AcknowledgeCancellationWith(i64),
    PublishProgressWith(i64, HaskellValue),
    ObserveProgressWith(i64),
    UpdateRequestWith(i64, String),
    ObserveRequestUpdateWith(i64, i64),
}

#[derive(tidepool_bridge_derive::FromHaskell)]
#[allow(
    clippy::enum_variant_names,
    reason = "variant names are the stable Haskell constructor names"
)]
pub(crate) enum WatchesReq {
    #[haskell(module = "Tidepool.Agent.Watch.Internal")]
    RegisterWatchWith(String, Vec<AwaitDependency>),
    RegisterWatchGroupsWith(String, Vec<Vec<AwaitDependency>>),
    RegisterRouteWith(String, tidepool_bridge::HaskellValue, Vec<AwaitDependency>),
    RegisterRouteGroupsWith(
        String,
        tidepool_bridge::HaskellValue,
        Vec<Vec<AwaitDependency>>,
    ),
    ObserveRouteWith(i64),
    ListRoutesWith,
    #[haskell(module = "Tidepool.Agent.Watch.Internal")]
    ObserveWatchWith(i64),
    ForgetWatchWith(i64),
    ObserveWatchProgressWith(i64, i64, i64),
}

#[derive(tidepool_bridge_derive::FromHaskell)]
pub(crate) enum AwaitDependency {
    #[haskell(module = "Tidepool.Agent.Watch.Internal")]
    AwaitDependency(i64, bool),
    AwaitProgress(i64, i64),
}

impl AwaitDependency {
    pub(crate) fn checked(
        self,
    ) -> Result<(RequestId, crate::request::WatchRequirement), BridgeError> {
        Ok(match self {
            Self::AwaitDependency(request, allow_failure) => (
                request_id(request)?,
                crate::request::WatchRequirement::Response { allow_failure },
            ),
            Self::AwaitProgress(request, cursor) => (
                request_id(request)?,
                crate::request::WatchRequirement::ProgressAfter(u64::try_from(cursor).map_err(
                    |_| BridgeError::UnsupportedType("negative progress cursor".into()),
                )?),
            ),
        })
    }
}

pub(crate) struct RequestReservation {
    pub continuation: ResidentHole,
    pub target: ActorRef,
    pub label: String,
    pub notify_owner: bool,
}

pub(crate) struct RequestSubmission {
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
    pub continuation: ResidentHole,
    pub dependencies: Vec<Vec<(RequestId, crate::request::WatchRequirement)>>,
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
            Self::Response(Ok(ResponseObservation::Pending)) => emit!("RawResponsePending"),
            Self::Response(Ok(ResponseObservation::CancellationPending(reason))) => {
                emit!("RawResponseCancellationPending", reason)
            }
            Self::Response(Ok(ResponseObservation::Ready)) => emit!("RawResponseReady"),
            Self::Response(Ok(ResponseObservation::Unavailable(failure))) => {
                emit!("RawResponseUnavailable", failure)
            }
            Self::Response(Ok(ResponseObservation::Starting(detail))) => {
                emit!("RawResponseStarting", detail)
            }
            Self::Response(Err(error)) => emit!("RawResponseRejected", error),
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
            Self::Watch(Ok(WatchObservation::Pending)) => emit!("RawWatchPending"),
            Self::Watch(Ok(WatchObservation::Ready(failures))) => emit!("RawWatchReady", failures),
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
    use super::RequestDuration;

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
