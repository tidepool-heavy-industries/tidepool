//! Private commands use the same owning-TUI socket as active input.
use crate::{AgentBackendError, NativeCommandOperation, NativeCommandReply, QueueReadyThread};
use std::time::Duration;
use tidepool_bridge_effects::{
    CommandOutput, CommandPage, CommandPosition, CommandSpec, CommandStream,
};

use super::controller;

type Request = codex_shoal_protocol::CommandRequest<CommandSpec, CommandStream, CommandPosition>;
type Operation =
    codex_shoal_protocol::CommandOperation<CommandSpec, CommandStream, CommandPosition>;
type Response = codex_shoal_protocol::CommandResponse<CommandOutput, CommandPage>;
type BoundResponse = codex_shoal_protocol::BoundReply<Response>;
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
    let binding = match controller::binding(thread) {
        Ok(binding) => binding,
        Err(controller::Failure::NotSubmitted(detail)) => {
            return Ok(NativeCommandReply::NotSubmitted(detail));
        }
        Err(controller::Failure::Unconfirmed(detail)) => return Err(unconfirmed(detail)),
    };
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
    let response = controller::post_with_deadlines(
        thread,
        codex_shoal_protocol::COMMAND_PATH,
        &Request {
            binding: binding.clone(),
            thread_id: thread.id().0.clone(),
            id: id.to_owned(),
            operation,
        },
        codex_shoal_protocol::MAX_COMMAND_REPLY_BYTES,
        connect_timeout,
        operation_timeout,
    )
    .await;
    let response = match response {
        Ok(response) => response,
        Err(controller::Failure::NotSubmitted(detail)) => {
            return Ok(NativeCommandReply::NotSubmitted(detail));
        }
        Err(controller::Failure::Unconfirmed(detail)) => return Err(unconfirmed(detail)),
    };
    if !response.status.is_success() {
        return Err(unconfirmed(format!(
            "native command controller returned HTTP {}",
            response.status
        )));
    }
    let response: BoundResponse = serde_json::from_slice(&response.body).map_err(unconfirmed)?;
    controller::validate_reply_binding(&binding, &response.binding).map_err(unconfirmed)?;
    Ok(match response.payload {
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
    })
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

    fn bound_thread(socket: PathBuf) -> QueueReadyThread {
        QueueReadyThread::new(crate::BackendThreadId("thread".into()))
            .with_input_control(Some(socket))
            .with_challenged_session_binding(Some(crate::InteractiveSessionBinding {
                launch_id: "launch".into(),
                instance_id: "instance".into(),
                generation: std::num::NonZeroU64::new(1).unwrap(),
                nonce: "nonce".into(),
            }))
    }

    fn response_binding() -> codex_shoal_protocol::Binding {
        codex_shoal_protocol::Binding {
            protocol_version: codex_shoal_protocol::INPUT_CONTROL_PROTOCOL_VERSION,
            launch_id: "launch".into(),
            instance_id: "instance".into(),
            generation: 1,
            nonce: "nonce".into(),
        }
    }

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
        let thread = bound_thread(socket);
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
        assert!(oversized.to_string().contains("byte limit"));
    }

    #[tokio::test]
    async fn stalled_peer_expires_without_retrying_the_operation() {
        let error = invoke(None, Duration::from_millis(50)).await;
        assert!(error.to_string().contains("unconfirmed"));
    }

    #[tokio::test]
    async fn missing_binding_is_proven_not_submitted() {
        let thread = QueueReadyThread::new(crate::BackendThreadId("thread".into()));
        assert!(matches!(
            request(&thread, "command", NativeCommandOperation::Wait).await,
            Ok(NativeCommandReply::NotSubmitted(_))
        ));
    }

    #[tokio::test]
    async fn wrong_generation_reply_is_unconfirmed() {
        let mut reply = BoundResponse {
            binding: response_binding(),
            payload: Response::Acknowledged,
        };
        reply.binding.generation = 2;
        let error = invoke(
            Some(serde_json::to_vec(&reply).unwrap()),
            Duration::from_secs(1),
        )
        .await;
        assert!(error.to_string().contains("challenged generation"));
    }

    #[test]
    #[ignore = "measurement harness; run explicitly at integration boundaries"]
    fn command_protocol_roundtrip_measurement() {
        let value = BoundResponse {
            binding: response_binding(),
            payload: Response::State(State::Finished {
                exit_code: 0,
                cancelled: false,
            }),
        };
        let encoded = serde_json::to_vec(&value).unwrap();
        let iterations = 100_000_u128;
        let started = std::time::Instant::now();
        for _ in 0..iterations {
            let bytes = serde_json::to_vec(std::hint::black_box(&value)).unwrap();
            let decoded: BoundResponse =
                serde_json::from_slice(std::hint::black_box(&bytes)).unwrap();
            std::hint::black_box(decoded);
        }
        let elapsed = started.elapsed();
        eprintln!(
            "command_protocol bytes={} roundtrip_ns={}",
            encoded.len(),
            elapsed.as_nanos() / iterations,
        );
    }
}
