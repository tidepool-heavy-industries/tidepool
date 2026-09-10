//! Private commands use the same owning-TUI socket as active input.
use crate::{AgentBackendError, NativeCommandOperation, NativeCommandReply, QueueReadyThread};
use serde::{Deserialize, Serialize};
use std::time::Duration;
use tidepool_bridge_effects::{
    CommandOutput, CommandPage, CommandPosition, CommandSpec, CommandStream,
};

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Request<'a> {
    thread_id: &'a str,
    id: &'a str,
    #[serde(flatten)]
    operation: Operation,
}
#[derive(Serialize)]
#[serde(tag = "operation", rename_all = "snake_case")]
enum Operation {
    Start {
        spec: CommandSpec,
    },
    Wait,
    Output {
        bytes: usize,
    },
    Read {
        stream: CommandStream,
        position: CommandPosition,
    },
    Input {
        text: String,
    },
    CloseInput,
    Resize {
        rows: u16,
        columns: u16,
    },
    Cancel,
}
#[derive(Deserialize)]
#[serde(tag = "result", content = "value", rename_all = "snake_case")]
enum Response {
    State(State),
    Output(CommandOutput),
    Page(CommandPage),
    Acknowledged,
}
#[derive(Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
enum State {
    Starting,
    Finished { exit_code: i32, cancelled: bool },
    Failed { detail: String },
}

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
        .post("http://localhost/v1/commands")
        .json(&Request {
            thread_id: &thread.id().0,
            id,
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
        if body.len() + chunk.len() > 800 * 1024 {
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
