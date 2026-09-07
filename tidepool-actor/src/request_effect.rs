use tidepool_bridge::{BridgeError, ToCore};
use tidepool_eval::Value;
use tidepool_repr::DataConTable;
use tidepool_runtime::session::{ResidentHole, RootCustody};

use crate::{
    ActorRef, CancellationReason, ReplyError, ReplyObservation, RequestId, ResponseFailure,
    ResponseObservation, WatchId, WatchObservation,
};

#[derive(Debug, Clone, Copy, tidepool_bridge_derive::FromCore)]
#[allow(
    clippy::enum_variant_names,
    reason = "variant names are the stable Haskell Duration constructors"
)]
pub(crate) enum RequestDuration {
    #[core(module = "Tidepool.Duration")]
    DurationMilliseconds(i64),
    #[core(module = "Tidepool.Duration")]
    DurationSeconds(i64),
    #[core(module = "Tidepool.Duration")]
    DurationMinutes(i64),
}

impl RequestDuration {
    pub(crate) fn checked(self) -> Result<crate::RequestDeadline, String> {
        let (value, unit) = match self {
            Self::DurationMilliseconds(value) => (value, crate::DeadlineUnit::Milliseconds),
            Self::DurationSeconds(value) => (value, crate::DeadlineUnit::Seconds),
            Self::DurationMinutes(value) => (value, crate::DeadlineUnit::Minutes),
        };
        crate::RequestDeadline::checked(value, unit)
    }
}

#[derive(tidepool_bridge_derive::FromCore)]
#[allow(dead_code, clippy::enum_variant_names)]
pub(crate) enum RepliesReq {
    #[core(module = "Tidepool.Agent.Reply.Internal")]
    ReserveRequestWith(String, (i64, i64)),
    #[core(module = "Tidepool.Agent.Reply.Internal")]
    // GHC erases the RequestDeadline newtype; its Core representation is Duration.
    SubmitRequestWith(i64, Value, (i64, i64), Option<RequestDuration>),
    #[core(module = "Tidepool.Agent.Reply.Internal")]
    AttemptReplyWith(i64, Value),
    #[core(module = "Tidepool.Agent.Reply.Internal")]
    ReplyWith(i64, Value),
    #[core(module = "Tidepool.Agent.Reply.Internal")]
    ObserveResponseWith(i64),
    #[core(module = "Tidepool.Agent.Reply.Internal")]
    CancelRequestWith(i64),
    AbandonResponseWith(i64),
    ForgetResponseWith(i64),
    ObserveReplyWith(i64),
    AttemptAcknowledgeCancellationWith(i64),
    AcknowledgeCancellationWith(i64),
    PublishProgressWith(i64, Value),
    ObserveProgressWith(i64),
    UpdateRequestWith(i64, String),
    ObserveRequestUpdateWith(i64, i64),
}

#[derive(tidepool_bridge_derive::FromCore)]
#[allow(
    clippy::enum_variant_names,
    reason = "variant names are the stable Haskell constructor names"
)]
pub(crate) enum WatchesReq {
    #[core(module = "Tidepool.Agent.Watch.Internal")]
    RegisterWatchWith(String, Vec<AwaitDependency>),
    RegisterRouteWith(String, tidepool_eval::Value, Vec<AwaitDependency>),
    ObserveRouteWith(i64),
    ListRoutesWith,
    #[core(module = "Tidepool.Agent.Watch.Internal")]
    ObserveWatchWith(i64),
    ForgetWatchWith(i64),
    ObserveWatchProgressWith(i64, i64, i64),
}

#[derive(tidepool_bridge_derive::FromCore)]
pub(crate) enum AwaitDependency {
    #[core(module = "Tidepool.Agent.Watch.Internal")]
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
    pub dependencies: Vec<(RequestId, crate::request::WatchRequirement)>,
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

pub(crate) fn reply_error_value(
    error: ReplyError,
    table: &DataConTable,
) -> Result<Value, BridgeError> {
    let name = match error {
        ReplyError::UpdatePending => "ReplyUpdatePending",
        ReplyError::Stale => "ReplyStale",
        ReplyError::AlreadySettled => "ReplyAlreadySettled",
        ReplyError::Unauthorized => "ReplyUnauthorized",
        ReplyError::WrongIncarnation => "ReplyWrongIncarnation",
        ReplyError::CancellationRequested => "ReplySettlementCancelled",
    };
    constructor(table, "Tidepool.Agent.Reply.Internal", name, Vec::new())
}

pub(crate) fn rejected_reply_value(
    error: ReplyError,
    table: &DataConTable,
) -> Result<Value, BridgeError> {
    let error = reply_error_value(error, table)?;
    let constructor = tidepool_bridge::get_resilient(table, "Left", 1)
        .ok_or_else(|| BridgeError::UnknownDataConName("Left".into()))?;
    Ok(Value::Con(constructor, vec![error]))
}

pub(crate) fn response_observation_value(
    observation: Result<ResponseObservation, ReplyError>,
    table: &DataConTable,
) -> Result<Value, BridgeError> {
    match observation {
        Ok(ResponseObservation::Pending) => constructor(
            table,
            "Tidepool.Agent.Reply.Internal",
            "RawResponsePending",
            Vec::new(),
        ),
        Ok(ResponseObservation::CancellationPending(reason)) => constructor(
            table,
            "Tidepool.Agent.Reply.Internal",
            "RawResponseCancellationPending",
            vec![cancellation_reason_value(reason, table)?],
        ),
        Ok(ResponseObservation::Ready) => constructor(
            table,
            "Tidepool.Agent.Reply.Internal",
            "RawResponseReady",
            Vec::new(),
        ),
        Ok(ResponseObservation::Unavailable(failure)) => constructor(
            table,
            "Tidepool.Agent.Reply.Internal",
            "RawResponseUnavailable",
            vec![response_failure_value(failure, table)?],
        ),
        Err(error) => constructor(
            table,
            "Tidepool.Agent.Reply.Internal",
            "RawResponseRejected",
            vec![reply_error_value(error, table)?],
        ),
    }
}

pub(crate) fn cancel_request_value(
    outcome: Result<crate::CancelRequestOutcome, ReplyError>,
    table: &DataConTable,
) -> Result<Value, BridgeError> {
    let (name, fields) = match outcome {
        Ok(crate::CancelRequestOutcome::Requested) => ("CancellationRequested", Vec::new()),
        Ok(crate::CancelRequestOutcome::AlreadyRequested) => {
            ("CancellationAlreadyRequested", Vec::new())
        }
        Ok(crate::CancelRequestOutcome::AlreadyTerminal) => {
            ("CancellationAlreadyTerminal", Vec::new())
        }
        Err(error) => (
            "CancellationRejected",
            vec![reply_error_value(error, table)?],
        ),
    };
    constructor(table, "Tidepool.Agent.Reply.Internal", name, fields)
}

pub(crate) fn abandon_response_value(
    outcome: Result<crate::AbandonResponseOutcome, ReplyError>,
    table: &DataConTable,
) -> Result<Value, BridgeError> {
    let (name, fields) = match outcome {
        Ok(crate::AbandonResponseOutcome::AbandonedNow) => ("ResponseAbandonedNow", Vec::new()),
        Ok(crate::AbandonResponseOutcome::AlreadyAbandoned) => {
            ("ResponseAlreadyAbandoned", Vec::new())
        }
        Ok(crate::AbandonResponseOutcome::AlreadyTerminal) => {
            ("ResponseAlreadyTerminal", Vec::new())
        }
        Err(error) => (
            "ResponseAbandonRejected",
            vec![reply_error_value(error, table)?],
        ),
    };
    constructor(table, "Tidepool.Agent.Reply.Internal", name, fields)
}

pub(crate) fn forget_response_value(
    outcome: Result<crate::ForgetResponseOutcome, ReplyError>,
    table: &DataConTable,
) -> Result<Value, BridgeError> {
    let (name, fields) = match outcome {
        Ok(crate::ForgetResponseOutcome::Forgotten) => ("ResponseForgotten", Vec::new()),
        Ok(crate::ForgetResponseOutcome::StillPending) => ("ResponseForgetPending", Vec::new()),
        Ok(crate::ForgetResponseOutcome::TargetStillActive) => {
            ("ResponseForgetTargetActive", Vec::new())
        }
        Ok(crate::ForgetResponseOutcome::RetainedByWatches(watches)) => (
            "ResponseRetainedByWatches",
            vec![watches
                .into_iter()
                .map(|watch| {
                    i64::try_from(watch.0)
                        .map_err(|_| BridgeError::UnsupportedType("watch id exceeds Int".into()))
                })
                .collect::<Result<Vec<_>, _>>()?
                .to_value(table)?],
        ),
        Err(error) => (
            "ResponseForgetRejected",
            vec![reply_error_value(error, table)?],
        ),
    };
    constructor(table, "Tidepool.Agent.Reply.Internal", name, fields)
}

pub(crate) fn forget_watch_value(
    outcome: Result<crate::ForgetWatchOutcome, ReplyError>,
    table: &DataConTable,
) -> Result<Value, BridgeError> {
    let (name, fields) = match outcome {
        Ok(crate::ForgetWatchOutcome::Forgotten) => ("WatchForgotten", Vec::new()),
        Ok(crate::ForgetWatchOutcome::StillPending) => ("WatchForgetPending", Vec::new()),
        Err(error) => (
            "WatchForgetRejected",
            vec![reply_error_value(error, table)?],
        ),
    };
    constructor(table, "Tidepool.Agent.Watch.Internal", name, fields)
}

pub(crate) fn reply_observation_value(
    observation: Result<ReplyObservation, ReplyError>,
    table: &DataConTable,
) -> Result<Value, BridgeError> {
    let (name, fields) = match observation {
        Ok(ReplyObservation::Open) => ("RawReplyOpen", Vec::new()),
        Ok(ReplyObservation::CancellationRequested(reason)) => (
            "RawReplyCancellationRequested",
            vec![cancellation_reason_value(reason, table)?],
        ),
        Ok(ReplyObservation::Closed) => ("RawReplyClosed", Vec::new()),
        Err(error) => ("RawReplyRejected", vec![reply_error_value(error, table)?]),
    };
    constructor(table, "Tidepool.Agent.Reply.Internal", name, fields)
}

fn cancellation_reason_value(
    reason: CancellationReason,
    table: &DataConTable,
) -> Result<Value, BridgeError> {
    let name = match reason {
        CancellationReason::RequesterCancelled => "CancelledByRequester",
        CancellationReason::DeadlineExpired => "DeadlineExpired",
    };
    constructor(table, "Tidepool.Agent.Reply.Internal", name, Vec::new())
}

pub(crate) fn watch_observation_value(
    observation: Result<WatchObservation, ReplyError>,
    table: &DataConTable,
) -> Result<Value, BridgeError> {
    match observation {
        Ok(WatchObservation::Pending) => constructor(
            table,
            "Tidepool.Agent.Watch.Internal",
            "RawWatchPending",
            Vec::new(),
        ),
        Ok(WatchObservation::Ready(failures)) => constructor(
            table,
            "Tidepool.Agent.Watch.Internal",
            "RawWatchReady",
            vec![failures
                .into_iter()
                .map(|(request, failure)| {
                    Ok((
                        i64::try_from(request.0).map_err(|_| {
                            BridgeError::UnsupportedType("request id exceeds Int".into())
                        })?,
                        response_failure_value(failure, table)?,
                    ))
                })
                .collect::<Result<Vec<_>, BridgeError>>()?
                .to_value(table)?],
        ),
        Ok(WatchObservation::Unavailable { request, failure }) => constructor(
            table,
            "Tidepool.Agent.Watch.Internal",
            "RawWatchUnavailable",
            vec![
                i64::try_from(request.0)
                    .map_err(|_| BridgeError::UnsupportedType("request id exceeds Int".into()))?
                    .to_value(table)?,
                response_failure_value(failure, table)?,
            ],
        ),
        Err(error) => constructor(
            table,
            "Tidepool.Agent.Watch.Internal",
            "RawWatchRejected",
            vec![reply_error_value(error, table)?],
        ),
    }
}

fn response_failure_value(
    failure: ResponseFailure,
    table: &DataConTable,
) -> Result<Value, BridgeError> {
    let (name, fields) = match failure {
        ResponseFailure::TargetUnavailable => ("ResponseTargetUnavailable", Vec::new()),
        ResponseFailure::TargetFailed(summary) => {
            ("ResponseTargetFailed", vec![summary.to_value(table)?])
        }
        ResponseFailure::TargetCancelled(summary) => {
            ("ResponseTargetCancelled", vec![summary.to_value(table)?])
        }
        ResponseFailure::RequesterStopped => ("ResponseRequesterStopped", Vec::new()),
        ResponseFailure::Abandoned => ("ResponseAbandoned", Vec::new()),
        ResponseFailure::Cancelled => ("ResponseCancelled", Vec::new()),
        ResponseFailure::DeadlineExceeded => ("ResponseDeadlineExceeded", Vec::new()),
        ResponseFailure::SettlementFailed(detail) => {
            ("ResponseSettlementFailed", vec![detail.to_value(table)?])
        }
    };
    constructor(table, "Tidepool.Agent.Reply.Internal", name, fields)
}

pub(crate) fn constructor(
    table: &DataConTable,
    module: &str,
    name: &str,
    fields: Vec<Value>,
) -> Result<Value, BridgeError> {
    let qualified = format!("{module}.{name}");
    let constructor = table
        .get_by_qualified_name(&qualified)
        .ok_or(BridgeError::UnknownDataConName(qualified))?;
    Ok(Value::Con(constructor, fields))
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
    }
}
