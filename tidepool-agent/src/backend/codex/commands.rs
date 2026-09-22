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
    request_with_deadlines(
        thread,
        id,
        operation,
        Duration::from_secs(10),
        Duration::from_secs(35),
    )
    .await
}

async fn request_with_deadlines(
    thread: &QueueReadyThread,
    id: &str,
    operation: NativeCommandOperation,
    connect_timeout: Duration,
    operation_timeout: Duration,
) -> Result<NativeCommandReply, AgentBackendError> {
    let socket = thread
        .input_control_socket()
        .ok_or_else(|| unconfirmed("native command socket unavailable"))?;
    let client = reqwest::Client::builder()
        .unix_socket(socket)
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .retry(reqwest::retry::never())
        .connect_timeout(connect_timeout)
        .timeout(operation_timeout)
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    async fn serve_reply(socket: PathBuf, response: Option<Vec<u8>>) {
        let listener = tokio::net::UnixListener::bind(socket).unwrap();
        let (mut stream, _) = listener.accept().await.unwrap();
        let mut request = Vec::new();
        while !request.windows(4).any(|part| part == b"\r\n\r\n") {
            let mut chunk = [0; 1024];
            let count = stream.read(&mut chunk).await.unwrap();
            assert_ne!(count, 0);
            request.extend_from_slice(&chunk[..count]);
        }
        if let Some(body) = response {
            stream
                .write_all(
                    format!(
                        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                        body.len()
                    )
                    .as_bytes(),
                )
                .await
                .unwrap();
            stream.write_all(&body).await.unwrap();
        } else {
            tokio::time::sleep(Duration::from_secs(1)).await;
        }
    }

    async fn invoke(response: Option<Vec<u8>>, timeout: Duration) -> AgentBackendError {
        let directory = tempfile::tempdir().unwrap();
        let socket = directory.path().join("commands.sock");
        let server = tokio::spawn(serve_reply(socket.clone(), response));
        while !socket.exists() {
            tokio::task::yield_now().await;
        }
        let thread = QueueReadyThread::new(crate::BackendThreadId("thread".into()))
            .with_input_control(Some(socket));
        let error = request_with_deadlines(
            &thread,
            "command",
            NativeCommandOperation::Wait,
            timeout,
            timeout,
        )
        .await
        .expect_err("invalid peer reply must remain unconfirmed");
        server.abort();
        error
    }

    #[tokio::test]
    async fn malformed_and_oversized_replies_are_bounded_and_unconfirmed() {
        let malformed = invoke(Some(b"not-json".to_vec()), Duration::from_secs(1)).await;
        assert!(malformed.to_string().contains("unconfirmed"));
        let oversized = invoke(
            Some(vec![
                b'x';
                codex_shoal_protocol::MAX_COMMAND_REPLY_BYTES + 1
            ]),
            Duration::from_secs(2),
        )
        .await;
        assert!(oversized.to_string().contains("exceeds limit"));
    }

    #[tokio::test]
    async fn stalled_peer_expires_without_retrying_the_operation() {
        let error = invoke(None, Duration::from_millis(50)).await;
        assert!(error.to_string().contains("unconfirmed"));
    }
}
