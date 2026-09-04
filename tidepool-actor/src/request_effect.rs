use tidepool_bridge::{BridgeError, ToCore};
use tidepool_eval::Value;
use tidepool_repr::DataConTable;
use tidepool_runtime::session::{ResidentHole, RootCustody};

use crate::{
    ActorRef, ReplyError, RequestId, ResponseFailure, ResponseObservation, WatchId,
    WatchObservation,
};

#[derive(tidepool_bridge_derive::FromCore)]
#[allow(dead_code, clippy::enum_variant_names)]
pub(crate) enum RepliesReq {
    #[core(module = "Tidepool.Agent.Reply.Internal")]
    ReserveRequestWith(String, (i64, i64)),
    #[core(module = "Tidepool.Agent.Reply.Internal")]
    SubmitRequestWith(i64, Value, (i64, i64)),
    #[core(module = "Tidepool.Agent.Reply.Internal")]
    AttemptReplyWith(i64, Value),
    #[core(module = "Tidepool.Agent.Reply.Internal")]
    ReplyWith(i64, Value),
    #[core(module = "Tidepool.Agent.Reply.Internal")]
    ObserveResponseWith(i64),
    #[core(module = "Tidepool.Agent.Reply.Internal")]
    CancelResponseWith(i64),
}

#[derive(tidepool_bridge_derive::FromCore)]
pub(crate) enum WatchesReq {
    #[core(module = "Tidepool.Agent.Watch.Internal")]
    RegisterWatchWith(String, Vec<(i64, bool)>),
    #[core(module = "Tidepool.Agent.Watch.Internal")]
    ObserveWatchWith(i64),
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

pub(crate) struct ResponseCancellation {
    pub continuation: ResidentHole,
    pub request: RequestId,
}

pub(crate) struct WatchRegistration {
    pub continuation: ResidentHole,
    pub dependencies: Vec<(RequestId, bool)>,
    pub label: String,
}

pub(crate) struct WatchPoll {
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
        ReplyError::Stale => "ReplyStale",
        ReplyError::AlreadySettled => "ReplyAlreadySettled",
        ReplyError::Unauthorized => "ReplyUnauthorized",
        ReplyError::WrongIncarnation => "ReplyWrongIncarnation",
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

pub(crate) fn cancellation_value(
    outcome: Result<crate::CancelResponseOutcome, ReplyError>,
    table: &DataConTable,
) -> Result<Value, BridgeError> {
    let (name, fields) = match outcome {
        Ok(crate::CancelResponseOutcome::CancelledNow) => ("ResponseCancelledNow", Vec::new()),
        Ok(crate::CancelResponseOutcome::AlreadyTerminal) => {
            ("ResponseAlreadyTerminal", Vec::new())
        }
        Err(error) => (
            "ResponseCancelRejected",
            vec![reply_error_value(error, table)?],
        ),
    };
    constructor(table, "Tidepool.Agent.Reply.Internal", name, fields)
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
        ResponseFailure::Cancelled => ("ResponseCancelled", Vec::new()),
        ResponseFailure::DeadlineExceeded => ("ResponseDeadlineExceeded", Vec::new()),
    };
    constructor(table, "Tidepool.Agent.Reply.Internal", name, fields)
}

fn constructor(
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
