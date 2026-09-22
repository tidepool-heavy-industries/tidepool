//! Sole transport owner for operations sent to one challenged native controller.

use std::time::Duration;

use http_body_util::BodyExt;
use serde::Serialize;

use crate::{InteractiveSessionBinding, QueueReadyThread};

const CONNECT_DEADLINE: Duration = Duration::from_secs(10);
const OPERATION_DEADLINE: Duration = Duration::from_secs(35);

#[derive(Debug, thiserror::Error)]
pub(super) enum Failure {
    #[error("operation was not submitted: {0}")]
    NotSubmitted(String),
    #[error("operation outcome is unconfirmed: {0}")]
    Unconfirmed(String),
}

pub(super) struct Reply {
    pub status: hyper::StatusCode,
    pub body: Vec<u8>,
    /// Credentials from the same connection that carried `body`.
    pub peer_pid: u32,
}

pub(super) fn binding(thread: &QueueReadyThread) -> Result<codex_shoal_protocol::Binding, Failure> {
    let InteractiveSessionBinding {
        launch_id,
        instance_id,
        generation,
        nonce,
    } = thread.session_binding().ok_or_else(|| {
        Failure::NotSubmitted("bound TUI has not completed the generation/nonce challenge".into())
    })?;
    Ok(codex_shoal_protocol::Binding {
        protocol_version: codex_shoal_protocol::INPUT_CONTROL_PROTOCOL_VERSION,
        launch_id: launch_id.clone(),
        instance_id: instance_id.clone(),
        generation: generation.get(),
        nonce: nonce.clone(),
    })
}

pub(super) async fn post<T: Serialize>(
    thread: &QueueReadyThread,
    path: &'static str,
    request: &T,
    response_limit: usize,
) -> Result<Reply, Failure> {
    post_with_deadlines(
        thread,
        path,
        request,
        response_limit,
        CONNECT_DEADLINE,
        OPERATION_DEADLINE,
    )
    .await
}

pub(super) async fn post_with_deadlines<T: Serialize>(
    thread: &QueueReadyThread,
    path: &'static str,
    request: &T,
    response_limit: usize,
    connect_deadline: Duration,
    operation_deadline: Duration,
) -> Result<Reply, Failure> {
    let body = serde_json::to_vec(request)
        .map_err(|error| Failure::NotSubmitted(format!("cannot encode request: {error}")))?;
    let socket = thread
        .input_control_socket()
        .ok_or_else(|| Failure::NotSubmitted("native controller socket unavailable".into()))?;
    let stream = tokio::time::timeout(connect_deadline, tokio::net::UnixStream::connect(socket))
        .await
        .map_err(|_| Failure::NotSubmitted(format!("connect exceeded {connect_deadline:?}")))?
        .map_err(|error| Failure::NotSubmitted(error.to_string()))?;
    let peer_pid = peer_pid(&stream)?;
    let (mut client, connection) = tokio::time::timeout(
        connect_deadline,
        hyper::client::conn::http1::handshake(hyper_util::rt::TokioIo::new(stream)),
    )
    .await
    .map_err(|_| Failure::NotSubmitted(format!("HTTP handshake exceeded {connect_deadline:?}")))?
    .map_err(|error| Failure::NotSubmitted(error.to_string()))?;
    let driver = tokio::spawn(connection);
    struct Driver(tokio::task::JoinHandle<Result<(), hyper::Error>>);
    impl Drop for Driver {
        fn drop(&mut self) {
            self.0.abort();
        }
    }
    let _driver = Driver(driver);
    let request = hyper::Request::post(path)
        .header("Host", "localhost")
        .header("Content-Type", "application/json")
        .body(http_body_util::Full::new(hyper::body::Bytes::from(body)))
        .map_err(|error| Failure::NotSubmitted(error.to_string()))?;
    tokio::time::timeout(operation_deadline, async {
        let response = client
            .send_request(request)
            .await
            .map_err(|error| Failure::Unconfirmed(error.to_string()))?;
        let status = response.status();
        let body = http_body_util::Limited::new(response.into_body(), response_limit)
            .collect()
            .await
            .map_err(|error| {
                Failure::Unconfirmed(format!(
                    "reply exceeds {response_limit} byte limit: {error}"
                ))
            })?
            .to_bytes()
            .to_vec();
        Ok(Reply {
            status,
            body,
            peer_pid,
        })
    })
    .await
    .map_err(|_| Failure::Unconfirmed(format!("operation exceeded {operation_deadline:?}")))?
}

fn peer_pid(stream: &tokio::net::UnixStream) -> Result<u32, Failure> {
    stream
        .peer_cred()
        .map_err(|error| Failure::NotSubmitted(error.to_string()))?
        .pid()
        .and_then(|pid| u32::try_from(pid).ok())
        .filter(|pid| *pid > 0)
        .ok_or_else(|| Failure::NotSubmitted("native controller peer PID unavailable".into()))
}

pub(super) fn validate_reply_binding(
    expected: &codex_shoal_protocol::Binding,
    actual: &codex_shoal_protocol::Binding,
) -> Result<(), Failure> {
    if actual == expected {
        Ok(())
    } else {
        Err(Failure::Unconfirmed(
            "native response binding does not match the challenged generation".into(),
        ))
    }
}
