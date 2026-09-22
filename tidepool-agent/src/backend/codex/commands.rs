//! Private commands use the same owning-TUI socket as active input.
use crate::{AgentBackendError, NativeCommandOperation, NativeCommandReply, QueueReadyThread};
use std::time::Duration;
use tidepool_bridge_effects::{
    CommandOutput, CommandPage, CommandPosition, CommandSpec, CommandStream,
};

type Request = codex_shoal_protocol::CommandRequest<CommandSpec, CommandStream, CommandPosition>;
type Operation =
    codex_shoal_protocol::CommandOperation<CommandSpec, CommandStream, CommandPosition>;
type Response = codex_shoal_protocol::CommandResponse<CommandOutput, CommandPage>;
use codex_shoal_protocol::CommandState as State;

pub(super) async fn request(
    thread: &QueueReadyThread,
    id: &str,
    operation: NativeCommandOperation,
) -> Result<NativeCommandReply, AgentBackendError> {
    let socket = thread
        .input_control_socket()
        .ok_or_else(|| unconfirmed("native command socket unavailable"))?;
    let client = reqwest::Client::builder()
        .unix_socket(socket)
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .retry(reqwest::retry::never())
        .connect_timeout(Duration::from_secs(10))
        .timeout(Duration::from_secs(35))
        .build()
        .map_err(unconfirmed)?;
    let operation = match operation {
        NativeCommandOperation::Start(spec) => Operation::Start { spec },
        NativeCommandOperation::Wait => Operation::Wait,
        NativeCommandOperation::Output(bytes) => Operation::Output { bytes },
        NativeCommandOperation::Read { stream, position } => Operation::Read { stream, position },
        NativeCommandOperation::Input(text) => Operation::Input { text },
        NativeCommandOperation::CloseInput => Operation::CloseInput,
        NativeCommandOperation::Resize { rows, columns } => Operation::Resize { rows, columns },
        NativeCommandOperation::Cancel => Operation::Cancel,
    };
    let mut response = client
        .post(format!(
            "http://localhost{}",
            codex_shoal_protocol::COMMAND_PATH
        ))
        .json(&Request {
            thread_id: thread.id().0.clone(),
            id: id.to_owned(),
            operation,
        })
        .send()
        .await
        .map_err(unconfirmed)?
        .error_for_status()
        .map_err(unconfirmed)?;
    // JSON escaping can expand a bounded byte read by up to six times.
    let mut body = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(unconfirmed)? {
        if body.len() + chunk.len() > codex_shoal_protocol::MAX_COMMAND_REPLY_BYTES {
            return Err(unconfirmed("native command reply exceeds limit"));
        }
        body.extend_from_slice(&chunk);
    }
    Ok(
        match serde_json::from_slice::<Response>(&body).map_err(unconfirmed)? {
            Response::State(State::Starting) => NativeCommandReply::Pending,
            Response::State(State::Finished {
                exit_code,
                cancelled,
            }) => NativeCommandReply::Finished {
                exit_code,
                cancelled,
            },
            Response::State(State::Failed { detail }) => NativeCommandReply::Unconfirmed(detail),
            Response::Output(output) => NativeCommandReply::Output(output),
            Response::Page(page) => NativeCommandReply::Page(page),
            Response::Acknowledged => NativeCommandReply::Acknowledged,
        },
    )
}

fn unconfirmed(error: impl std::fmt::Display) -> AgentBackendError {
    AgentBackendError::BackendUnavailable {
        detail: format!("native command operation is unconfirmed: {error}"),
    }
}
